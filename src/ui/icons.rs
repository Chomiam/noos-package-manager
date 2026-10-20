use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use gtk4::gdk;

use crate::backend::models::PackageItem;

struct IconResolverIndex {
    /// Table `nom_icone_minuscule -> chemin_fichier_svg_ou_png`
    file_index: HashMap<String, PathBuf>,
    /// Table `nom_paquet_ou_binaire -> nom_icone_du_desktop`
    desktop_icon_map: HashMap<String, String>,
}

static ICON_INDEX: OnceLock<IconResolverIndex> = OnceLock::new();

const APP_LOGO_SVG: &str = include_str!("../../assets/noos-package-manager.svg");

fn ensure_app_logo_and_desktop_installed() -> Option<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME")?);
    let icon_dir = home
        .join(".local")
        .join("share")
        .join("icons")
        .join("hicolor")
        .join("scalable")
        .join("apps");
    let _ = fs::create_dir_all(&icon_dir);
    let icon_path = icon_dir.join("noos-package-manager.svg");
    let _ = fs::write(&icon_path, APP_LOGO_SVG);

    let apps_dir = home.join(".local").join("share").join("applications");
    let _ = fs::create_dir_all(&apps_dir);
    let desktop_path = apps_dir.join("noos-package-manager.desktop");
    if !Path::new("/usr/share/applications/noos-package-manager.desktop").exists() {
        let exec_cmd = env::current_exe()
            .ok()
            .and_then(|p| p.to_str().map(|s| format!("\"{s}\"")))
            .unwrap_or_else(|| String::from("noos-package-manager"));
        let desktop_content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Version=1.0\n\
             Name=Gestionnaire de paquets\n\
             Name[fr]=Gestionnaire de paquets\n\
             GenericName=Gestionnaire de paquets Arch Linux\n\
             Comment=Gestionnaire de paquets graphique moderne (Pacman, AUR, Flatpak/Flathub et AppImages) pour Arch Linux\n\
             Exec={exec_cmd}\n\
             Icon={}\n\
             Terminal=false\n\
             Categories=System;PackageManager;GTK;GNOME;\n\
             Keywords=pacman;aur;yay;paru;flatpak;flathub;appimage;arch;package;manager;noos;gestionnaire;paquets;\n\
             StartupNotify=true\n\
             StartupWMClass=org.noos.PackageManager\n",
            icon_path.display()
        );
        let _ = fs::write(desktop_path, desktop_content);
    }

    Some(icon_path)
}

pub fn init_icon_search_paths() {
    let _ = ensure_app_logo_and_desktop_installed();

    let Some(display) = gdk::Display::default() else {
        return;
    };
    let icon_theme = gtk4::IconTheme::for_display(&display);

    for dir in candidate_icon_directories() {
        if dir.is_dir() {
            icon_theme.add_search_path(&dir);
        }
    }

    gtk4::Window::set_default_icon_name("noos-package-manager");

    let _ = get_icon_index();
}

fn candidate_icon_directories() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    if let Some(home_os) = env::var_os("HOME") {
        let home = PathBuf::from(home_os);
        let user_icons = home.join(".local").join("share").join("icons");
        dirs.push(user_icons.join("WhiteSur").join("apps").join("scalable"));
        dirs.push(
            user_icons
                .join("WhiteSur-dark")
                .join("apps")
                .join("scalable"),
        );
        dirs.push(
            user_icons
                .join("WhiteSur-light")
                .join("apps")
                .join("scalable"),
        );
        dirs.push(user_icons.join("hicolor").join("scalable").join("apps"));
        dirs.push(user_icons.join("hicolor").join("128x128").join("apps"));
        dirs.push(user_icons.join("hicolor").join("48x48").join("apps"));

        let user_flatpak_icons = home
            .join(".local")
            .join("share")
            .join("flatpak")
            .join("exports")
            .join("share")
            .join("icons")
            .join("hicolor");
        dirs.push(user_flatpak_icons.join("scalable").join("apps"));
        dirs.push(user_flatpak_icons.join("128x128").join("apps"));
    }

    for sys_path in [
        "/usr/share/icons/WhiteSur/apps/scalable",
        "/usr/share/icons/WhiteSur-dark/apps/scalable",
        "/var/lib/flatpak/exports/share/icons/hicolor/scalable/apps",
        "/var/lib/flatpak/exports/share/icons/hicolor/128x128/apps",
        "/var/lib/flatpak/appstream/flathub/x86_64/active/icons/128x128",
        "/var/lib/flatpak/appstream/flathub/x86_64/active/icons/64x64",
        "/usr/share/icons/hicolor/scalable/apps",
        "/usr/share/icons/hicolor/128x128/apps",
        "/usr/share/icons/hicolor/64x64/apps",
        "/usr/share/icons/hicolor/48x48/apps",
        "/usr/share/icons/breeze/apps/48",
        "/usr/share/icons/breeze-dark/apps/48",
        "/usr/share/pixmaps",
    ] {
        dirs.push(PathBuf::from(sys_path));
    }

    dirs
}

fn get_icon_index() -> &'static IconResolverIndex {
    ICON_INDEX.get_or_init(|| {
        let mut file_index = HashMap::with_capacity(7000);
        let mut desktop_icon_map = HashMap::with_capacity(500);

        if let Some(logo_path) = ensure_app_logo_and_desktop_installed() {
            file_index.insert(String::from("noos-package-manager"), logo_path.clone());
            file_index.insert(String::from("org.noos.packagemanager"), logo_path);
        }

        for dir in candidate_icon_directories() {
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry_res in entries {
                let Ok(entry) = entry_res else {
                    continue;
                };
                let path = entry.path();
                let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
                    continue;
                };
                if !ext.eq_ignore_ascii_case("svg") && !ext.eq_ignore_ascii_case("png") {
                    continue;
                }
                let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                let key = stem.to_lowercase();
                file_index.entry(key.clone()).or_insert_with(|| path.clone());
                if let Some((_, last_seg)) = key.rsplit_once('.') {
                    if last_seg.len() >= 3 {
                        file_index.entry(last_seg.to_string()).or_insert(path);
                    }
                }
            }
        }

        // Indexation des fichiers .desktop pour relier les noms de binaires/paquets à leur champ Icon=
        let mut desktop_dirs = vec![PathBuf::from("/usr/share/applications")];
        if let Some(home_os) = env::var_os("HOME") {
            desktop_dirs.push(
                PathBuf::from(home_os)
                    .join(".local")
                    .join("share")
                    .join("applications"),
            );
        }

        for d_dir in desktop_dirs {
            let Ok(entries) = fs::read_dir(&d_dir) else {
                continue;
            };
            for entry_res in entries {
                let Ok(entry) = entry_res else {
                    continue;
                };
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                    continue;
                }
                let Ok(content) = fs::read_to_string(&path) else {
                    continue;
                };
                let mut icon_val = None;
                let mut exec_bin = None;
                for line in content.lines() {
                    let trimmed = line.trim();
                    if let Some(rest) = trimmed.strip_prefix("Icon=") {
                        if icon_val.is_none() && !rest.trim().is_empty() {
                            icon_val = Some(rest.trim().to_string());
                        }
                    } else if let Some(rest) = trimmed.strip_prefix("Exec=") {
                        if exec_bin.is_none() {
                            if let Some(first_tok) = rest.split_whitespace().next() {
                                let bin_name = Path::new(first_tok.trim_matches('"'))
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(first_tok);
                                exec_bin = Some(bin_name.to_lowercase());
                            }
                        }
                    }
                }

                if let Some(icon_str) = icon_val {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        let stem_low = stem.to_lowercase();
                        desktop_icon_map
                            .entry(stem_low.clone())
                            .or_insert_with(|| icon_str.clone());
                        if let Some((_, last_seg)) = stem_low.rsplit_once('.') {
                            desktop_icon_map
                                .entry(last_seg.to_string())
                                .or_insert_with(|| icon_str.clone());
                        }
                    }
                    if let Some(bin) = exec_bin {
                        desktop_icon_map.entry(bin).or_insert(icon_str);
                    }
                }
            }
        }

        IconResolverIndex {
            file_index,
            desktop_icon_map,
        }
    })
}

fn alias_candidates(base_name: &str) -> &'static [&'static str] {
    match base_name {
        "noos-package-manager" => &["noos-package-manager", "org.noos.packagemanager"],
        "code" | "code-oss" | "vscodium" => &["visual-studio-code", "vscode", "com.visualstudio.code"],
        "obs-studio" => &["com.obsproject.studio", "obs", "obs-studio"],
        "zed" => &["zed", "dev.zed.zed", "accessories-text-editor"],
        "btop" | "htop" => &["btop", "htop", "utilities-system-monitor"],
        "fastfetch" | "neofetch" => &["utilities-terminal", "terminal"],
        "lazygit" | "git" => &["git", "gitg", "git-gui"],
        "gamemode" => &["input-gaming", "applications-games"],
        "fwupd" => &["system-software-update", "software-update-available"],
        "libreoffice-fresh" | "libreoffice-still" => &["libreoffice-main", "libreoffice-startcenter", "libreoffice"],
        "telegram-desktop" => &["telegram", "org.telegram.desktop"],
        "signal-desktop" => &["signal-desktop", "org.signal.signal", "signal"],
        "nextcloud-client" => &["nextcloud", "com.nextcloud.desktopclient.nextcloud"],
        "prismlauncher" => &["org.prismlauncher.prismlauncher", "prismlauncher"],
        "lutris" => &["net.lutris.lutris", "lutris"],
        "thunderbird" => &["org.mozilla.thunderbird", "thunderbird"],
        "baobab" => &["org.gnome.baobab", "baobab"],
        "evince" => &["org.gnome.evince", "evince"],
        "okular" => &["org.kde.okular", "okular"],
        "ark" => &["org.kde.ark", "ark", "accessories-archiver"],
        "kdenlive" => &["org.kde.kdenlive", "kdenlive"],
        "krita" => &["org.kde.krita", "krita"],
        "gparted" => &["gparted", "partitionmanager"],
        "virt-manager" => &["virt-manager", "qemu"],
        "flatpak" => &["flatpak", "system-software-install"],
        "rustup" | "rust" => &["rust", "text-x-rust"],
        "nodejs" => &["nodejs", "application-javascript"],
        "go" => &["go", "text-x-go"],
        _ => &[],
    }
}

fn generate_candidate_keys(pkg_name: &str) -> Vec<String> {
    let lower = pkg_name.trim().to_lowercase().replace(' ', "-");
    let mut keys = Vec::with_capacity(12);
    keys.push(lower.clone());

    for suffix in [
        "-bin",
        "-git",
        "-fresh",
        "-still",
        "-desktop",
        "-client",
        "-qt",
        "-qt5",
        "-qt6",
        "-gtk",
        "-gtk3",
        "-gtk4",
        "-wayland",
        "-x11",
    ] {
        if let Some(stripped) = lower.strip_suffix(suffix) {
            if !stripped.is_empty() {
                keys.push(stripped.to_string());
            }
        }
    }

    for &alias in alias_candidates(&lower) {
        keys.push(alias.to_string());
    }

    let index = get_icon_index();
    let current_len = keys.len();
    for i in 0..current_len {
        if let Some(mapped) = index.desktop_icon_map.get(&keys[i]) {
            keys.push(mapped.to_lowercase());
        }
    }

    keys
}

/// Construit un widget `gtk4::Image` affichant le logo réel du paquet s'il existe dans les thèmes
/// d'icônes du système (WhiteSur, hicolor, breeze, pixmaps, .desktop), ou l'icône de source en repli.
pub fn build_package_icon_widget(item: &PackageItem) -> gtk4::Image {
    let index = get_icon_index();
    let mut candidates = Vec::new();
    if let Some(app_id) = item.flatpak_app_id() {
        let low_id = app_id.to_lowercase();
        candidates.push(low_id.clone());
        if let Some((_, last_seg)) = low_id.rsplit_once('.') {
            candidates.push(last_seg.to_string());
        }
    }
    candidates.extend(generate_candidate_keys(&item.name));

    // 1. Recherche directe dans l'index des fichiers SVG/PNG (WhiteSur, hicolor, breeze, pixmaps)
    for cand in &candidates {
        let cand_path = Path::new(cand);
        if cand_path.is_absolute() && cand_path.is_file() {
            let img = gtk4::Image::from_file(cand_path);
            img.set_pixel_size(36);
            return img;
        }
        if let Some(found_path) = index.file_index.get(cand) {
            let img = gtk4::Image::from_file(found_path);
            img.set_pixel_size(36);
            return img;
        }
    }

    // 2. Recherche via gtk4::IconTheme courant
    if let Some(display) = gdk::Display::default() {
        let icon_theme = gtk4::IconTheme::for_display(&display);
        for cand in &candidates {
            if icon_theme.has_icon(cand) {
                let img = gtk4::Image::from_icon_name(cand);
                img.set_pixel_size(36);
                return img;
            }
        }
    }

    // 3. Repli : icône symbolique de la source du paquet
    let fallback = gtk4::Image::from_icon_name(item.source.icon_name());
    fallback.set_pixel_size(32);
    fallback
}
