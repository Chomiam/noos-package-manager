use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;

use tempfile::TempDir;

use crate::backend::appimage::{
    deduce_name_and_version, ensure_executable_permission, inspect_appimage, is_appimage_file,
    resolve_appimage_github_repo, scan_directory, slugify_appimage_name,
};
use crate::backend::aur::{
    build_info_url, build_search_url, detect_aur_helper_in_dirs, parse_aur_rpc_json,
};
use crate::backend::details::{
    extract_github_from_url, markdown_to_pango_markup, parse_pacman_detail_fields,
    resolve_github_owner_repo,
};
use crate::backend::flatpak::{
    parse_flatpak_list_output, parse_flatpak_progress_line, parse_flatpak_remote_info,
    parse_flatpak_search_output, parse_flatpak_size_to_bytes,
};
use crate::backend::models::{
    AurHelper, AurSortMode, DependencyStatus, PackageSource, RecommendationCategory,
};
use crate::backend::pacman::{
    parse_checkupdates_output, parse_optdepend_spec, parse_pacman_progress_line,
    parse_pacman_qi_output, parse_pacman_si_optdepends, parse_pacman_si_output,
    parse_pacman_sp_output, parse_pacman_ss_output,
};
use crate::error::AppError;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn test_aur_rpc_v5_search_json_parsing() -> TestResult {
    let sample_json = r#"{
        "version": 5,
        "type": "search",
        "resultcount": 2,
        "results": [
            {
                "ID": 1298412,
                "Name": "yay",
                "PackageBaseID": 159044,
                "PackageBase": "yay",
                "Version": "12.4.2-1",
                "Description": "Yet another yogurt AUR helper",
                "URL": "https://github.com/Jguer/yay",
                "NumVotes": 982,
                "Popularity": 18.45,
                "OutOfDate": null,
                "Maintainer": "Jguer",
                "FirstSubmitted": 1603895664,
                "LastModified": 1720000000,
                "URLPath": "/cgit/aur.git/snapshot/yay.tar.gz"
            },
            {
                "ID": 1300100,
                "Name": "yay-bin",
                "PackageBaseID": 159045,
                "PackageBase": "yay-bin",
                "Version": "12.4.2-1",
                "Description": "Yet another yogurt AUR helper (precompiled binary)",
                "URL": "https://github.com/Jguer/yay",
                "NumVotes": 640,
                "Popularity": 12.30,
                "OutOfDate": null,
                "Maintainer": "Jguer",
                "FirstSubmitted": 1603895700,
                "LastModified": 1720000100,
                "URLPath": "/cgit/aur.git/snapshot/yay-bin.tar.gz"
            }
        ]
    }"#;

    let mut installed = HashMap::new();
    installed.insert(String::from("yay"), String::from("12.4.1-1"));

    let mut items = parse_aur_rpc_json(sample_json, &installed)?;
    assert_eq!(items.len(), 2);

    let yay = &items[0];
    assert_eq!(yay.name, "yay");
    assert_eq!(yay.id, "aur:yay");
    assert_eq!(yay.version, "12.4.1-1");
    assert!(yay.is_installed);
    assert_eq!(yay.update_available.as_deref(), Some("12.4.2-1"));
    assert_eq!(yay.source, PackageSource::Aur);
    assert!(yay.description.contains("Yet another yogurt AUR helper"));
    let meta = yay.aur_meta.as_ref().ok_or("aur_meta absent")?;
    assert_eq!(meta.votes, 982);
    assert_eq!(meta.maintainer.as_deref(), Some("Jguer"));
    assert_eq!(meta.last_modified, Some(1720000000));
    assert!(meta.out_of_date.is_none());

    let yay_bin = &items[1];
    assert_eq!(yay_bin.name, "yay-bin");
    assert_eq!(yay_bin.version, "12.4.2-1");
    assert!(!yay_bin.is_installed);
    assert!(yay_bin.update_available.is_none());
    assert_eq!(yay_bin.source, PackageSource::Aur);

    // Test du tri par "Mis à jour récemment" (yay-bin a LastModified 1720000100 > yay 1720000000)
    items.sort_by(|a, b| AurSortMode::RecentlyUpdated.compare_items(a, b));
    assert_eq!(items[0].name, "yay-bin");

    // Test du tri par Popularité (par défaut : yay 18.45 > yay-bin 12.30)
    items.sort_by(|a, b| AurSortMode::Popularity.compare_items(a, b));
    assert_eq!(items[0].name, "yay");

    Ok(())
}

#[test]
fn test_aur_rpc_v5_info_and_error_json_parsing() -> TestResult {
    let info_json = r#"{
        "version": 5,
        "type": "multiinfo",
        "resultcount": 1,
        "results": [
            {
                "ID": 999001,
                "Name": "yay",
                "PackageBaseID": 12345,
                "PackageBase": "yay",
                "Version": "12.4.2-1",
                "Description": "Yet another yogurt. Pacman wrapper and AUR helper written in go.",
                "URL": "https://github.com/Jguer/yay",
                "NumVotes": 2310,
                "Popularity": 25.12,
                "Depends": ["pacman>6.1", "git"],
                "MakeDepends": ["go>=1.21"],
                "License": ["GPL-3.0-or-later"]
            }
        ]
    }"#;

    let mut installed = HashMap::new();
    installed.insert(String::from("yay"), String::from("12.4.2-1"));

    let items = parse_aur_rpc_json(info_json, &installed)?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].name, "yay");
    assert!(items[0].is_installed);
    assert!(items[0].update_available.is_none());

    let error_json = r#"{
        "version": 5,
        "type": "error",
        "resultcount": 0,
        "results": [],
        "error": "Query arg too small."
    }"#;

    let err_res = parse_aur_rpc_json(error_json, &installed);
    assert!(err_res.is_err());
    if let Err(AppError::AurApiError(msg)) = err_res {
        assert_eq!(msg, "Query arg too small.");
    } else {
        return Err("Expected AppError::AurApiError".into());
    }

    assert_eq!(
        build_search_url("gtk4 rust"),
        "https://aur.archlinux.org/rpc/v5/search/gtk4%20rust"
    );
    assert_eq!(
        build_info_url(&["yay", "yay-bin"]),
        "https://aur.archlinux.org/rpc/v5/info?arg[]=yay&arg[]=yay-bin"
    );

    Ok(())
}

#[test]
fn test_appimage_detection_and_filtering_in_tempdir() -> TestResult {
    let temp_dir = TempDir::new()?;
    let dir_path = temp_dir.path();

    // 1. Fichier .AppImage valide avec permission d'exécution (+x) et fichier .desktop adjacent
    let obsidian_path = dir_path.join("Obsidian-1.7.7-x86_64.AppImage");
    {
        let mut f = File::create(&obsidian_path)?;
        f.write_all(b"\x7fELF\x02\x01\x01\x00AI\x02\x00dummy_payload_bytes")?;
        let mut perms = fs::metadata(&obsidian_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&obsidian_path, perms)?;
    }

    let obsidian_desktop = dir_path.join("Obsidian-1.7.7-x86_64.desktop");
    {
        let mut f = File::create(&obsidian_desktop)?;
        f.write_all(
            b"[Desktop Entry]\nName=Obsidian Notes\nComment=Base de connaissances Markdown\nX-AppImage-Version=1.7.7\n",
        )?;
    }

    // 2. Fichier .appimage (minuscule) sans permission d'exécution
    let freecad_path = dir_path.join("FreeCAD_1.0.0.appimage");
    {
        let mut f = File::create(&freecad_path)?;
        f.write_all(b"appimage_content")?;
        let mut perms = fs::metadata(&freecad_path)?.permissions();
        perms.set_mode(0o644);
        fs::set_permissions(&freecad_path, perms)?;
    }

    // 3. Binaire sans extension mais possédant les octets magiques ELF + AppImage (AI\x02)
    let magic_bin_path = dir_path.join("neovim-nightly");
    {
        let mut f = File::create(&magic_bin_path)?;
        f.write_all(b"\x7fELF\x02\x01\x01\x00AI\x02\x00elf_appimage_binary")?;
        let mut perms = fs::metadata(&magic_bin_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&magic_bin_path, perms)?;
    }

    // 4. Fichiers parasites qui DOIVENT être filtrés (texte, script shell, dossier)
    let txt_path = dir_path.join("notes.txt");
    File::create(&txt_path)?.write_all(b"Simple fichier texte")?;

    let sh_path = dir_path.join("launcher.sh");
    File::create(&sh_path)?.write_all(b"#!/bin/sh\necho hello\n")?;

    let sub_dir = dir_path.join("FakeDir.AppImage");
    fs::create_dir(&sub_dir)?;

    assert!(is_appimage_file(&obsidian_path));
    assert!(is_appimage_file(&freecad_path));
    assert!(is_appimage_file(&magic_bin_path));
    assert!(!is_appimage_file(&txt_path));
    assert!(!is_appimage_file(&sh_path));
    assert!(!is_appimage_file(&sub_dir));

    let scanned = scan_directory(dir_path)?;
    assert_eq!(scanned.len(), 3);

    // Vérification du tri et des métadonnées extraites
    let freecad_item = scanned
        .iter()
        .find(|i| i.name == "FreeCAD")
        .ok_or("FreeCAD AppImage non trouvé")?;
    assert_eq!(freecad_item.version, "1.0.0");
    assert!(freecad_item
        .description
        .contains("Non exécutable (chmod +x requis)"));
    assert_eq!(
        freecad_item.source,
        PackageSource::AppImage(freecad_path.clone())
    );

    let obsidian_item = scanned
        .iter()
        .find(|i| i.name == "Obsidian Notes")
        .ok_or("Obsidian Notes AppImage non trouvé")?;
    assert_eq!(obsidian_item.version, "1.7.7");
    assert!(obsidian_item
        .description
        .contains("Base de connaissances Markdown"));
    assert!(obsidian_item.description.contains("Exécutable (+x)"));

    // Vérification de l'activation de la permission d'exécution
    let changed = ensure_executable_permission(&freecad_path)?;
    assert!(changed);
    let updated_meta = inspect_appimage(&freecad_path)?;
    assert!(updated_meta.is_executable);

    let (deduced_name, deduced_ver) = deduce_name_and_version("Krita-v5.2.6-x86_64.AppImage");
    assert_eq!(deduced_name, "Krita");
    assert_eq!(deduced_ver, "5.2.6");
    assert_eq!(slugify_appimage_name("Obsidian Notes"), "obsidian-notes");
    assert_eq!(
        resolve_appimage_github_repo("Obsidian Notes"),
        Some((
            String::from("obsidianmd"),
            String::from("obsidian-releases")
        ))
    );

    Ok(())
}

#[test]
fn test_pacman_parsers_and_aur_helper_detection() -> TestResult {
    let raw_checkupdates = "\
linux 6.11.3.arch1-1 -> 6.11.5.arch1-1
gtk4 1:4.16.2-1 -> 1:4.16.3-1
";
    let updates_map = parse_checkupdates_output(raw_checkupdates);
    assert_eq!(updates_map.len(), 2);
    assert_eq!(
        updates_map.get("linux"),
        Some(&(
            String::from("6.11.3.arch1-1"),
            String::from("6.11.5.arch1-1")
        ))
    );

    let raw_qi = "\
Name            : gtk4
Version         : 1:4.16.2-1
Description     : GObject-based multi-platform GUI toolkit

Name            : yay
Version         : 12.4.2-1
Description     : Yet another yogurt AUR helper
";
    let mut foreign = HashSet::new();
    foreign.insert(String::from("yay"));
    let mut simple_updates = HashMap::new();
    simple_updates.insert(String::from("gtk4"), String::from("1:4.16.3-1"));

    let qi_items = parse_pacman_qi_output(raw_qi, &foreign, &simple_updates);
    assert_eq!(qi_items.len(), 2);
    assert_eq!(qi_items[0].name, "gtk4");
    assert_eq!(qi_items[0].source, PackageSource::Pacman);
    assert_eq!(
        qi_items[0].update_available.as_deref(),
        Some("1:4.16.3-1")
    );
    assert_eq!(qi_items[1].name, "yay");
    assert_eq!(qi_items[1].source, PackageSource::Aur);

    let raw_ss = "\
extra/libadwaita 1:1.6.1-1 [installed]
    Building blocks for modern adaptive GNOME applications
core/linux 6.11.5.arch1-1 [installed: 6.11.3.arch1-1]
    The Linux kernel and modules
";
    let ss_items = parse_pacman_ss_output(raw_ss, &HashMap::new());
    assert_eq!(ss_items.len(), 2);
    assert!(ss_items[0].is_installed);
    assert!(ss_items[0].update_available.is_none());
    assert!(ss_items[1].is_installed);
    assert_eq!(ss_items[1].version, "6.11.3.arch1-1");
    assert_eq!(
        ss_items[1].update_available.as_deref(),
        Some("6.11.5.arch1-1")
    );

    // Test de détection de helper AUR dans un dossier temporaire
    let bin_dir = TempDir::new()?;
    let yay_bin = bin_dir.path().join("yay");
    File::create(&yay_bin)?.write_all(b"#!/bin/sh\n")?;
    let mut perms = fs::metadata(&yay_bin)?.permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&yay_bin, perms)?;

    let dirs = vec![bin_dir.path().to_path_buf()];
    assert_eq!(detect_aur_helper_in_dirs(&dirs), Some(AurHelper::Yay));

    let raw_si = "\
Repository      : extra
Name            : firefox
Version         : 131.0.3-1
Description     : Fast, Private & Safe Web Browser

Repository      : extra
Name            : vlc
Version         : 3.0.21-1
Description     : Multi-platform MPEG, VCD/DVD, and DivX player
";
    let mut inst_map = HashMap::new();
    inst_map.insert(String::from("vlc"), String::from("3.0.21-1"));
    let si_parsed = parse_pacman_si_output(raw_si, &inst_map, &HashMap::new());
    assert_eq!(si_parsed.len(), 2);
    let ff = si_parsed.get("firefox").ok_or("firefox absent")?;
    assert!(!ff.is_installed);
    assert_eq!(ff.description, "Fast, Private & Safe Web Browser");
    let vlc = si_parsed.get("vlc").ok_or("vlc absent")?;
    assert!(vlc.is_installed);
    assert_eq!(RecommendationCategory::ALL.len(), 6);

    let raw_sp = "\
js140|140.17.0-1|15181129
gjs|2:1.90.1-1|721384
webkitgtk-6.0|2.54.1-1|40344561
foliate|3.3.0-3|3209478
";
    let mut explicit = HashSet::new();
    explicit.insert(String::from("foliate"));
    let deps = parse_pacman_sp_output(raw_sp, &explicit);
    assert_eq!(deps.len(), 4);
    assert!(!deps[0].is_explicit_target);
    assert_eq!(deps[0].name, "js140");
    assert_eq!(deps[0].size_bytes, 15_181_129);
    assert!(deps[3].is_explicit_target);
    assert_eq!(deps[3].name, "foliate");

    let known_names: Vec<String> = deps.iter().map(|d| d.name.clone()).collect();
    let dl_parsed = parse_pacman_progress_line(
        " webkitgtk-6.0-2.54.1-1-x86_64 downloading...",
        &known_names,
    );
    assert_eq!(
        dl_parsed,
        Some((
            String::from("Téléchargement de « webkitgtk-6.0 »…"),
            Some((String::from("webkitgtk-6.0"), DependencyStatus::Downloading))
        ))
    );

    let inst_parsed =
        parse_pacman_progress_line("(4/4) installing foliate...", &known_names);
    assert_eq!(
        inst_parsed,
        Some((
            String::from("Installation de « foliate »…"),
            Some((String::from("foliate"), DependencyStatus::Installing))
        ))
    );

    let sync_parsed =
        parse_pacman_progress_line(":: Synchronizing package databases...", &known_names);
    assert_eq!(
        sync_parsed,
        Some((
            String::from("Synchronisation des bases de données de paquets (`pacman -Sy`)…"),
            None
        ))
    );

    assert_eq!(
        extract_github_from_url("https://github.com/Jguer/yay/"),
        Some((String::from("Jguer"), String::from("yay")))
    );
    assert_eq!(
        resolve_github_owner_repo("neovim", Some("https://neovim.io")),
        Some((String::from("neovim"), String::from("neovim")))
    );

    let detail_raw = "\
Repository      : extra
Name            : btop
Version         : 1.4.0-1
URL             : https://github.com/aristocratos/btop
Licenses        : Apache-2.0
Depends On      : gcc-libs  glibc
Optional Deps   : rocm-smi-lib: AMD GPU support
";
    let detail_info = parse_pacman_detail_fields(ff, detail_raw);
    assert_eq!(
        detail_info.upstream_url.as_deref(),
        Some("https://github.com/aristocratos/btop")
    );
    assert_eq!(detail_info.licenses, "Apache-2.0");
    assert_eq!(detail_info.dependencies, vec!["gcc-libs", "glibc"]);
    assert_eq!(detail_info.optional_deps.len(), 1);

    let sample_md = "# Titre Principal\n- Premier point & détail\n```rust\nlet x = 1;\n```";
    let pango = markdown_to_pango_markup(sample_md);
    assert!(pango.contains("Titre Principal"));
    assert!(pango.contains("Premier point &amp; détail"));
    assert!(pango.contains("<tt>let x = 1;</tt>"));

    let raw_yay_qua = "\
antigravity 1.22.2-1 -> 1.23.2-1
google-chrome 130.0.6723.58-1 -> 130.0.6723.69-1
";
    let aur_updates = parse_checkupdates_output(raw_yay_qua);
    assert_eq!(aur_updates.len(), 2);
    assert_eq!(
        aur_updates.get("antigravity"),
        Some(&(String::from("1.22.2-1"), String::from("1.23.2-1")))
    );
    assert_eq!(
        aur_updates.get("google-chrome"),
        Some(&(
            String::from("130.0.6723.58-1"),
            String::from("130.0.6723.69-1")
        ))
    );

    let fp_list_raw = "org.mozilla.firefox\tFirefox\t158.0.0\tstable\tflathub\tFast, Private & Safe Web Browser\n";
    let mut fp_upd = HashMap::new();
    fp_upd.insert(String::from("org.mozilla.firefox"), String::from("158.0.1"));
    let fp_installed = parse_flatpak_list_output(fp_list_raw, &fp_upd);
    assert_eq!(fp_installed.len(), 1);
    assert_eq!(fp_installed[0].name, "Firefox");
    assert_eq!(fp_installed[0].flatpak_app_id(), Some("org.mozilla.firefox"));
    assert_eq!(
        fp_installed[0].update_available.as_deref(),
        Some("158.0.1")
    );

    let fp_search_raw = "Firefox\tFast, Private & Safe Web Browser\torg.mozilla.firefox\t158.0.1\tstable\tflathub\nObsidian\tMarkdown knowledge base\tmd.obsidian.Obsidian\t1.7.7\tstable\tflathub\n";
    let mut fp_inst_map = HashMap::new();
    fp_inst_map.insert(String::from("org.mozilla.firefox"), String::from("158.0.0"));
    let fp_searched = parse_flatpak_search_output(fp_search_raw, &fp_inst_map, &fp_upd);
    assert_eq!(fp_searched.len(), 2);
    assert!(fp_searched[0].is_installed);
    assert!(!fp_searched[1].is_installed);

    let fp_remote_raw = "\
            ID: org.mozilla.firefox
       Version: 158.0.1
       License: MPL-2.0
 Download Size: 129.7?MB
Installed Size: 345.0?MB
       Runtime: org.freedesktop.Platform/x86_64/25.08
";
    let fp_meta = parse_flatpak_remote_info("org.mozilla.firefox", fp_remote_raw);
    assert_eq!(fp_meta.version, "158.0.1");
    assert_eq!(fp_meta.license, "MPL-2.0");
    assert!(fp_meta.download_size_bytes > 100_000_000);
    assert_eq!(
        fp_meta.runtime.as_deref(),
        Some("org.freedesktop.Platform/x86_64/25.08")
    );
    assert_eq!(parse_flatpak_size_to_bytes("10 MB"), 10 * 1024 * 1024);

    let fp_prog = parse_flatpak_progress_line(
        "Installing for system: app/org.mozilla.firefox/x86_64/stable from flathub",
        &[String::from("org.mozilla.firefox")],
    );
    assert!(fp_prog.is_some());

    assert_eq!(
        parse_optdepend_spec("rocm-smi-lib: AMD GPU support [installed]"),
        Some((
            String::from("rocm-smi-lib"),
            String::from("AMD GPU support")
        ))
    );
    assert_eq!(
        parse_optdepend_spec("python>=3.10: scripting support (see :help python)"),
        Some((
            String::from("python"),
            String::from("scripting support (see :help python)")
        ))
    );
    assert_eq!(
        parse_optdepend_spec("cups"),
        Some((String::from("cups"), String::new()))
    );
    assert_eq!(parse_optdepend_spec("None"), None);

    let raw_si_opt = "\
Repository      : extra
Name            : foliate
Version         : 3.3.0-3
Optional Deps   : speech-dispatcher: Text-to-speech support
                  localsearch: Track the locations of files
Conflicts With  : None

Repository      : extra
Name            : htop
Version         : 3.3.0-1
Optional Deps   : None
Conflicts With  : None
";
    let opt_map = parse_pacman_si_optdepends(raw_si_opt);
    assert_eq!(opt_map.len(), 1);
    let foliate_opts = opt_map.get("foliate").ok_or("foliate opts absents")?;
    assert_eq!(foliate_opts.len(), 2);
    assert_eq!(foliate_opts[0].0, "speech-dispatcher");
    assert_eq!(foliate_opts[0].1, "Text-to-speech support");
    assert_eq!(foliate_opts[1].0, "localsearch");

    assert!(crate::backend::pacman::is_pacman_lock_error_text(
        "error: failed to init transaction (unable to lock database)\n  if you're sure a package manager is not already running, you can remove /var/lib/pacman/db.lck"
    ));
    assert!(crate::backend::pacman::is_pacman_lock_error_text(
        "erreur : impossible de verrouiller la base de données"
    ));
    assert!(!crate::backend::pacman::is_pacman_lock_error_text(
        "error: target not found: nonexistent-package"
    ));

    // Test du différentiel incrémental du cache (`compute_pacman_aur_diff`)
    let cached_pkgs = vec![
        crate::backend::models::PackageItem {
            id: String::from("pacman:firefox"),
            name: String::from("firefox"),
            version: String::from("135.0-1"),
            description: String::from("Standalone web browser"),
            source: PackageSource::Pacman,
            is_installed: true,
            update_available: Some(String::from("136.0-1")),
            aur_meta: None,
        },
        crate::backend::models::PackageItem {
            id: String::from("pacman:old-removed-pkg"),
            name: String::from("old-removed-pkg"),
            version: String::from("1.0-1"),
            description: String::from("To be removed"),
            source: PackageSource::Pacman,
            is_installed: true,
            update_available: None,
            aur_meta: None,
        },
    ];
    let mut current_installed = HashMap::new();
    current_installed.insert(String::from("firefox"), String::from("136.0-1"));
    current_installed.insert(String::from("newly-added-pkg"), String::from("2.0-1"));

    let mut current_explicit = HashSet::new();
    current_explicit.insert(String::from("firefox"));
    current_explicit.insert(String::from("newly-added-pkg"));

    let current_foreign = HashSet::new();
    let diff = crate::backend::cache::compute_pacman_aur_diff(
        &cached_pkgs,
        &current_installed,
        &current_explicit,
        &current_foreign,
        &["google-chrome"],
    );

    assert_eq!(diff.added_installed_names, vec![String::from("newly-added-pkg")]);
    assert_eq!(diff.missing_showcase_names, vec![String::from("google-chrome")]);
    let retained_ff = diff
        .retained_installed
        .iter()
        .find(|p| p.name == "firefox")
        .ok_or("firefox absent du cache retenu")?;
    assert_eq!(retained_ff.version, "136.0-1");
    assert_eq!(retained_ff.update_available, None);
    assert!(diff
        .retained_installed
        .iter()
        .all(|p| p.name != "old-removed-pkg"));

    // Test de filtrage et tri par pertinence de la recherche Flatpak
    let fp_noisy_raw = "\
Boxflat\tConfigure Moza Racing hardware\tio.github.lawstorant.boxflat\tv1.37.1\tstable\tflathub
ElectronMail\tUnofficial ProtonMail desktop app\tcom.github.vladimiry.ElectronMail\t5.3.10\tstable\tflathub
PortProton\tRun Windows games on Linux easily\tru.linux_gaming.PortProton\t1.7.5\tstable\tflathub
Proton Pass\tEncrypted password manager\tme.proton.Pass\t1.42.0\tstable\tflathub
Proton VPN\tSecures your internet\tcom.protonvpn.www\t4.18.7\tstable\tflathub
";
    let fp_parsed = parse_flatpak_search_output(fp_noisy_raw, &HashMap::new(), &HashMap::new());
    let mut fp_filtered: Vec<_> = fp_parsed
        .into_iter()
        .filter(|item| item.matches_search_query("proton"))
        .collect();
    fp_filtered.sort_by(|a, b| a.compare_search_relevance(b, "proton"));
    assert_eq!(fp_filtered.len(), 4);
    assert_eq!(fp_filtered[0].name, "Proton Pass");
    assert_eq!(fp_filtered[1].name, "Proton VPN");
    assert_eq!(fp_filtered[2].name, "PortProton");
    assert_eq!(fp_filtered[3].name, "ElectronMail");

    Ok(())
}
