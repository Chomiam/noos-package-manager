use std::collections::HashMap;
use std::env;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, UNIX_EPOCH};

use reqwest::Client;
use serde::Deserialize;

use crate::backend::details::resolve_github_owner_repo;
use crate::backend::models::{AppImageMetadata, PackageItem, PackageSource};
use crate::error::{AppError, AppResult};

/// Version du format des raccourcis `.desktop` générés par Noos.
/// Permet de régénérer automatiquement les anciens raccourcis (icône manquante, métadonnées incomplètes).
const SHORTCUT_VERSION: &str = "2";

/// Retourne le chemin du dossier principal `~/Applications` et le crée automatiquement s'il n'existe pas.
pub fn ensure_applications_directory() -> AppResult<PathBuf> {
    let Some(home_os) = env::var_os("HOME") else {
        return Err(AppError::AppImageError(String::from(
            "Variable d'environnement HOME introuvable",
        )));
    };
    let apps_dir = PathBuf::from(home_os).join("Applications");
    if !apps_dir.exists() {
        fs::create_dir_all(&apps_dir)?;
    }
    Ok(apps_dir)
}

/// Retourne le dossier `~/.local/share/icons/hicolor/{subdir}/apps` sans le créer.
fn user_hicolor_apps_dir_path(subdir: &str) -> Option<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME")?);
    Some(
        home.join(".local")
            .join("share")
            .join("icons")
            .join("hicolor")
            .join(subdir)
            .join("apps"),
    )
}

/// Retourne le dossier `~/.local/share/icons/hicolor/{subdir}/apps` et le crée si nécessaire.
fn user_hicolor_apps_dir(subdir: &str) -> Option<PathBuf> {
    let dir = user_hicolor_apps_dir_path(subdir)?;
    let _ = fs::create_dir_all(&dir);
    Some(dir)
}

/// Retourne le chemin cible d'une icône extraite pour un slug donné (`256x256` pour PNG, `scalable` pour SVG).
pub fn user_extracted_icon_path(slug: &str, ext: &str) -> Option<PathBuf> {
    let subdir = if ext.eq_ignore_ascii_case("svg") {
        "scalable"
    } else {
        "256x256"
    };
    Some(user_hicolor_apps_dir(subdir)?.join(format!("noos-appimage-{slug}.{ext}")))
}

/// Liste les chemins où l'icône extraite d'une AppImage peut se trouver (formats et tailles courants).
pub fn expected_extracted_icon_paths(appimage_path: &Path) -> Vec<PathBuf> {
    let Some(file_name) = appimage_path.file_name().and_then(|n| n.to_str()) else {
        return Vec::new();
    };
    let (deduced_name, _) = deduce_name_and_version(file_name);
    let slug = slugify_appimage_name(&deduced_name);

    let mut paths = Vec::with_capacity(6);
    for subdir in ["256x256", "128x128", "scalable"] {
        let Some(base) = user_hicolor_apps_dir_path(subdir) else {
            return paths;
        };
        for ext in ["png", "svg"] {
            paths.push(base.join(format!("noos-appimage-{slug}.{ext}")));
        }
    }
    paths
}

/// Recherche le raccourci `.desktop` géré par Noos pour une AppImage donnée.
fn find_managed_desktop_shortcut(appimage_path: &Path) -> Option<PathBuf> {
    let menu_dir = user_applications_menu_dir()?;
    let abs_path = fs::canonicalize(appimage_path).unwrap_or_else(|_| appimage_path.to_path_buf());
    let abs_str = abs_path.to_string_lossy();
    let raw_str = appimage_path.to_string_lossy();

    for entry_res in fs::read_dir(menu_dir).ok()?.flatten() {
        let p = entry_res.path();
        if p.extension().and_then(|e| e.to_str()) != Some("desktop") {
            continue;
        }
        let Ok(content) = fs::read_to_string(&p) else {
            continue;
        };
        if content.contains(abs_str.as_ref()) || content.contains(raw_str.as_ref()) {
            return Some(p);
        }
    }
    None
}

/// Retourne la valeur du champ `Icon=` du raccourci `.desktop` géré par Noos pour cette AppImage.
pub fn appimage_desktop_icon_reference(appimage_path: &Path) -> Option<String> {
    let shortcut = find_managed_desktop_shortcut(appimage_path)?;
    let content = fs::read_to_string(shortcut).ok()?;
    parse_desktop_entry_content(&content).2
}

/// Retourne le dossier `~/.local/share/applications` pour les raccourcis du menu démarrer.
pub fn user_applications_menu_dir() -> Option<PathBuf> {
    let home = PathBuf::from(env::var_os("HOME")?);
    let dir = home.join(".local").join("share").join("applications");
    let _ = fs::create_dir_all(&dir);
    Some(dir)
}

/// Retourne la liste des répertoires standards à scanner :
/// `~/Applications`, `~/.local/bin`, et `~/Desktop`.
pub fn default_appimage_directories() -> Vec<PathBuf> {
    let _ = ensure_applications_directory();
    let Some(home_os) = env::var_os("HOME") else {
        return Vec::new();
    };
    let home = PathBuf::from(home_os);
    vec![
        home.join("Applications"),
        home.join(".local").join("bin"),
        home.join("Desktop"),
    ]
}

/// Vérifie si un chemin correspond à un fichier AppImage valide (par extension `.AppImage` / `.appimage`
/// ou par signature binaire ELF + magic AppImage `AI\x01` / `AI\x02` à l'offset 8).
pub fn is_appimage_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }

    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        if ext.eq_ignore_ascii_case("appimage") {
            return true;
        }
    }

    if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
        let lower = file_name.to_ascii_lowercase();
        if lower.ends_with(".appimage") {
            return true;
        }
    }

    // Inspection des octets magiques ELF + AppImage (offset 0: \x7fELF, offset 8: 'A' 'I' 0x01|0x02)
    if metadata.len() >= 12 {
        if let Ok(mut file) = File::open(path) {
            let mut header = [0u8; 12];
            if file.read_exact(&mut header).is_ok() {
                let is_elf = &header[0..4] == b"\x7fELF";
                let is_ai_magic =
                    header[8] == b'A' && header[9] == b'I' && (header[10] == 1 || header[10] == 2);
                if is_elf && is_ai_magic {
                    return true;
                }
            }
        }
    }

    false
}

/// Déduit le nom de l'application et sa version éventuelle à partir du nom de fichier AppImage.
pub fn deduce_name_and_version(file_name: &str) -> (String, String) {
    let mut stem = file_name.trim();
    for suffix in [".AppImage", ".appimage", ".APPIMAGE"] {
        if let Some(stripped) = stem.strip_suffix(suffix) {
            stem = stripped;
            break;
        }
    }
    if stem.to_ascii_lowercase().ends_with(".appimage") && stem.len() > 9 {
        stem = &stem[..stem.len() - 9];
    }

    // Nettoyage des suffixes d'architecture courants
    let arch_suffixes = [
        "-x86_64",
        "_x86_64",
        ".x86_64",
        "-amd64",
        "_amd64",
        "-aarch64",
        "_aarch64",
        "-linux-x86_64",
        "-linux",
    ];
    let mut cleaned = stem.to_string();
    for arch in arch_suffixes {
        if cleaned.to_ascii_lowercase().ends_with(arch) && cleaned.len() > arch.len() {
            let new_len = cleaned.len() - arch.len();
            cleaned.truncate(new_len);
        }
    }

    // Recherche d'un segment de version (commençant par un chiffre ou 'v' suivi d'un chiffre)
    let parts: Vec<&str> = cleaned.split(['-', '_']).filter(|s| !s.is_empty()).collect();
    if parts.len() >= 2 {
        for (idx, part) in parts.iter().enumerate().skip(1) {
            let first_char = part.chars().next().unwrap_or(' ');
            let is_ver = first_char.is_ascii_digit()
                || ((first_char == 'v' || first_char == 'V')
                    && part.chars().nth(1).is_some_and(|c| c.is_ascii_digit()));
            if is_ver {
                let name_part = parts[..idx].join(" ");
                let ver_part = part.trim_start_matches(['v', 'V']).to_string();
                if !name_part.is_empty() && !ver_part.is_empty() {
                    return (name_part, ver_part);
                }
            }
        }
    }

    let friendly_name = cleaned.replace(['_', '-'], " ").trim().to_string();
    let final_name = if friendly_name.is_empty() {
        String::from("AppImage")
    } else {
        friendly_name
    };

    (final_name, String::from("Locale"))
}

/// Génère un identifiant court (slug) sûr pour nommer le fichier `.desktop` et l'icône d'une AppImage.
pub fn slugify_appimage_name(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
        } else if (ch == ' ' || ch == '-' || ch == '_' || ch == '.') && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let trimmed = slug.trim_matches('-').to_string();
    if trimmed.is_empty() {
        String::from("appimage")
    } else {
        trimmed
    }
}

/// Formate une taille en octets de manière lisible (o, Kio, Mio, Gio).
pub fn format_file_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

    let b = bytes as f64;
    if b >= GIB {
        format!("{:.2} Gio", b / GIB)
    } else if b >= MIB {
        format!("{:.1} Mio", b / MIB)
    } else if b >= KIB {
        format!("{:.1} Kio", b / KIB)
    } else {
        format!("{bytes} o")
    }
}

/// Formate un timestamp Unix (secondes) en date `YYYY-MM-DD HH:MM` sans dépendance externe lourde.
pub fn format_unix_timestamp(unix_secs: u64) -> String {
    let days_since_epoch = (unix_secs / 86_400) as i64;
    let secs_of_day = unix_secs % 86_400;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;

    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };

    format!("{year:04}-{m:02}-{d:02} {hour:02}:{minute:02}")
}

/// Parse le contenu texte d'un fichier `.desktop` pour en extraire `(Name, Comment, Icon, X-AppImage-Version)`.
pub fn parse_desktop_entry_content(
    content: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let mut name = None;
    let mut comment = None;
    let mut icon = None;
    let mut version = None;
    let mut in_desktop_entry = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_desktop_entry = trimmed.eq_ignore_ascii_case("[Desktop Entry]");
            continue;
        }
        if !in_desktop_entry && content.contains("[Desktop Entry]") {
            continue;
        }

        if let Some((key, val)) = trimmed.split_once('=') {
            let k = key.trim();
            let v = val.trim();
            if v.is_empty() {
                continue;
            }
            match k {
                "Name" if name.is_none() => name = Some(v.to_string()),
                "Comment" | "Comment[fr]" => comment = Some(v.to_string()),
                "Icon" if icon.is_none() => icon = Some(v.to_string()),
                "X-AppImage-Version" => version = Some(v.to_string()),
                _ => {}
            }
        }
    }

    (name, comment, icon, version)
}

/// Recherche un fichier `.desktop` adjacent ou dans `~/.local/share/applications` pour enrichir les métadonnées.
fn extract_desktop_metadata(
    appimage_path: &Path,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    // 1. Vérifier un fichier `.desktop` adjacent portant le même préfixe
    let mut sidecar = appimage_path.to_path_buf();
    sidecar.set_extension("desktop");
    if let Ok(content) = fs::read_to_string(&sidecar) {
        return parse_desktop_entry_content(&content);
    }

    // 2. Vérifier s'il existe un `.desktop` dans le même dossier qui mentionne le nom du binaire
    if let (Some(parent), Some(file_name)) = (
        appimage_path.parent(),
        appimage_path.file_name().and_then(|n| n.to_str()),
    ) {
        let stem = file_name
            .trim_end_matches(".AppImage")
            .trim_end_matches(".appimage");
        let candidate = parent.join(format!("{stem}.desktop"));
        if let Ok(content) = fs::read_to_string(&candidate) {
            return parse_desktop_entry_content(&content);
        }
    }

    // 3. Vérifier dans `~/.local/share/applications` si un raccourci géré par Noos existe
    if let Some(shortcut) = find_managed_desktop_shortcut(appimage_path) {
        if let Ok(content) = fs::read_to_string(&shortcut) {
            return parse_desktop_entry_content(&content);
        }
    }

    (None, None, None, None)
}

/// Vérifie si un raccourci `.desktop` existe dans `~/.local/share/applications` pour cette AppImage.
pub fn has_desktop_shortcut(appimage_path: &Path) -> bool {
    find_managed_desktop_shortcut(appimage_path).is_some()
}

/// Indique si le raccourci `.desktop` d'une AppImage doit être régénéré :
/// absent, généré par une ancienne version, ou pointant vers une icône supprimée.
pub fn desktop_shortcut_needs_refresh(appimage_path: &Path) -> bool {
    let Some(shortcut) = find_managed_desktop_shortcut(appimage_path) else {
        return true;
    };
    let Ok(content) = fs::read_to_string(shortcut) else {
        return true;
    };

    let has_current_version = content.lines().any(|line| {
        line.trim()
            .strip_prefix("X-Noos-Shortcut-Version=")
            .is_some_and(|value| value.trim() == SHORTCUT_VERSION)
    });
    if !has_current_version {
        return true;
    }

    match parse_desktop_entry_content(&content).2 {
        Some(icon) => {
            let icon_path = Path::new(&icon);
            icon_path.is_absolute() && !icon_path.is_file()
        }
        None => true,
    }
}

/// Crée ou met à jour le raccourci `.desktop` dans le menu démarrer (`~/.local/share/applications`)
/// et tente d'extraire l'icône embarquée de l'AppImage.
pub fn create_or_update_desktop_shortcut(appimage_path: &Path) -> AppResult<PathBuf> {
    let _ = ensure_executable_permission(appimage_path);

    let file_name = appimage_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("AppImage");
    let (deduced_name, deduced_ver) = deduce_name_and_version(file_name);
    let slug = slugify_appimage_name(&deduced_name);

    // Tentative rapide d'extraction de l'icône et du .desktop intégrés via `--appimage-extract`
    let (extracted_name, extracted_comment, extracted_icon_path, extracted_ver) =
        try_extract_embedded_appimage_assets(appimage_path, &slug);

    let (adj_name, adj_comment, adj_icon, adj_ver) = extract_desktop_metadata(appimage_path);

    let final_name = extracted_name.or(adj_name).unwrap_or(deduced_name);
    let final_version = extracted_ver.or(adj_ver).unwrap_or(deduced_ver);
    let final_comment = extracted_comment
        .or(adj_comment)
        .unwrap_or_else(|| format!("Application portable AppImage ({final_name})"));
    let final_icon = extracted_icon_path
        .or_else(|| {
            expected_extracted_icon_paths(appimage_path)
                .into_iter()
                .find(|path| path.is_file())
                .map(|path| path.to_string_lossy().to_string())
        })
        .or(adj_icon)
        .unwrap_or_else(|| slug.clone());

    let menu_dir = user_applications_menu_dir().ok_or_else(|| {
        AppError::AppImageError(String::from(
            "Impossible de déterminer le dossier ~/.local/share/applications",
        ))
    })?;

    let desktop_file_path = menu_dir.join(format!("noos-appimage-{slug}.desktop"));
    let abs_path = fs::canonicalize(appimage_path).unwrap_or_else(|_| appimage_path.to_path_buf());
    let abs_str = abs_path.to_string_lossy();

    let desktop_content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={final_name}\n\
         Comment={final_comment}\n\
         Exec=\"{abs_str}\" %U\n\
         TryExec={abs_str}\n\
         Icon={final_icon}\n\
         Terminal=false\n\
         Categories=Utility;Application;\n\
         StartupNotify=true\n\
         X-AppImage-Version={final_version}\n\
         X-Noos-Shortcut-Version={SHORTCUT_VERSION}\n\
         X-Noos-Managed-AppImage={abs_str}\n"
    );

    let mut f = File::create(&desktop_file_path)?;
    f.write_all(desktop_content.as_bytes())?;

    let _ = std::process::Command::new("update-desktop-database")
        .arg(&menu_dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();

    Ok(desktop_file_path)
}

/// Lance `--appimage-extract` sur un motif donné dans le dossier de travail.
fn run_appimage_extract(appimage_path: &Path, workdir: &Path, pattern: &str) {
    let _ = std::process::Command::new(appimage_path)
        .arg("--appimage-extract")
        .arg(pattern)
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Normalise lexicalement un chemin (supprime `.` et résout `..`).
fn normalize_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Suit les liens symboliques d'une entrée extraite du SquashFS et extrait sa cible si nécessaire.
/// Les AppImages embarquent souvent `.DirIcon` et le `.desktop` racine sous forme de liens
/// symboliques : `--appimage-extract <lien>` ne crée que le lien, pas le fichier cible.
fn materialize_extracted_entry(
    appimage_path: &Path,
    workdir: &Path,
    squash_root: &Path,
    entry: &Path,
) -> Option<PathBuf> {
    let mut current = normalize_path(entry);

    for _ in 0..8 {
        let metadata = fs::symlink_metadata(&current).ok()?;
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&current).ok()?;
            let next = if target.is_absolute() {
                squash_root.join(target.strip_prefix("/").ok()?)
            } else {
                current.parent().unwrap_or(squash_root).join(target)
            };
            let next = normalize_path(&next);
            if !next.starts_with(squash_root) {
                return None;
            }
            if fs::symlink_metadata(&next).is_err() {
                let relative = next.strip_prefix(squash_root).ok()?;
                run_appimage_extract(appimage_path, workdir, relative.to_str()?);
            }
            if fs::symlink_metadata(&next).is_err() {
                return None;
            }
            current = next;
        } else if metadata.is_file() {
            return Some(current);
        } else {
            return None;
        }
    }

    None
}

/// Ajoute à `out` les fichiers `.desktop` présents directement dans `dir`.
fn collect_desktop_entries(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
            out.push(path);
        }
    }
}

/// Recherche l'icône désignée par le champ `Icon=` du `.desktop` embarqué, lorsque `.DirIcon` est absent.
fn find_embedded_icon_by_name(
    appimage_path: &Path,
    workdir: &Path,
    squash_root: &Path,
    icon_name: &str,
) -> Option<PathBuf> {
    let icon_name = icon_name.trim();
    if icon_name.is_empty() || icon_name.contains("..") {
        return None;
    }

    let has_extension = Path::new(icon_name).extension().is_some();
    let mut direct_names = Vec::with_capacity(2);
    if has_extension {
        direct_names.push(icon_name.to_string());
    } else {
        direct_names.push(format!("{icon_name}.png"));
        direct_names.push(format!("{icon_name}.svg"));
    }

    for name in &direct_names {
        run_appimage_extract(appimage_path, workdir, name);
        let entry = squash_root.join(name);
        if let Some(real_path) =
            materialize_extracted_entry(appimage_path, workdir, squash_root, &entry)
        {
            if real_path.is_file() {
                return Some(real_path);
            }
        }
    }

    if !icon_name.contains('/') && !has_extension {
        for subdir in ["scalable", "256x256", "128x128", "64x64", "48x48"] {
            for ext in ["png", "svg"] {
                let relative = format!("usr/share/icons/hicolor/{subdir}/apps/{icon_name}.{ext}");
                run_appimage_extract(appimage_path, workdir, &relative);
                let entry = squash_root.join(&relative);
                if let Some(real_path) =
                    materialize_extracted_entry(appimage_path, workdir, squash_root, &entry)
                {
                    if real_path.is_file() {
                        return Some(real_path);
                    }
                }
            }
        }
    }

    None
}

/// Enregistre les octets d'une icône extraite dans `~/.local/share/icons/hicolor/.../apps`.
fn save_extracted_icon(source: &Path, slug: &str) -> Option<String> {
    let bytes = fs::read(source).ok()?;
    if bytes.is_empty() {
        return None;
    }

    let source_ext = source
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let ext = if source_ext.eq_ignore_ascii_case("svg")
        || bytes.starts_with(b"<svg")
        || bytes.starts_with(b"<?xml")
    {
        "svg"
    } else if source_ext.eq_ignore_ascii_case("png") || bytes.starts_with(b"\x89PNG") {
        "png"
    } else {
        return None;
    };

    for stale_ext in ["png", "svg"] {
        if let Some(stale) = user_extracted_icon_path(slug, stale_ext) {
            let _ = fs::remove_file(stale);
        }
    }

    let target_icon = user_extracted_icon_path(slug, ext)?;
    fs::write(&target_icon, &bytes).ok()?;
    Some(target_icon.to_string_lossy().to_string())
}

/// Tente d'extraire `.DirIcon` et `*.desktop` d'une AppImage Type 2 dans un dossier temporaire.
fn try_extract_embedded_appimage_assets(
    appimage_path: &Path,
    slug: &str,
) -> (
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
) {
    let Ok(temp_dir) = tempfile::TempDir::new() else {
        return (None, None, None, None);
    };
    let workdir = temp_dir.path();

    // Extraction du fichier .desktop embarqué à la racine du SquashFS et de l'icône du dossier
    run_appimage_extract(appimage_path, workdir, "*.desktop");
    run_appimage_extract(appimage_path, workdir, ".DirIcon");

    let squash_root = workdir.join("squashfs-root");
    if !squash_root.is_dir() {
        return (None, None, None, None);
    }

    let mut desktop_entries = Vec::new();
    collect_desktop_entries(&squash_root, &mut desktop_entries);
    if desktop_entries.is_empty() {
        run_appimage_extract(appimage_path, workdir, "usr/share/applications/*.desktop");
        collect_desktop_entries(
            &squash_root.join("usr/share/applications"),
            &mut desktop_entries,
        );
    }

    let mut parsed_name = None;
    let mut parsed_comment = None;
    let mut parsed_icon_name = None;
    let mut parsed_ver = None;

    for entry in desktop_entries {
        let Some(real_path) =
            materialize_extracted_entry(appimage_path, workdir, &squash_root, &entry)
        else {
            continue;
        };
        let Ok(content) = fs::read_to_string(&real_path) else {
            continue;
        };
        let (name, comment, icon, version) = parse_desktop_entry_content(&content);
        if name.is_none() && comment.is_none() && icon.is_none() && version.is_none() {
            continue;
        }
        parsed_name = name;
        parsed_comment = comment;
        parsed_icon_name = icon;
        parsed_ver = version;
        if parsed_name.is_some() {
            break;
        }
    }

    let dir_icon_entry = squash_root.join(".DirIcon");
    let icon_source =
        materialize_extracted_entry(appimage_path, workdir, &squash_root, &dir_icon_entry).or_else(
            || {
                parsed_icon_name.as_deref().and_then(|icon_name| {
                    find_embedded_icon_by_name(appimage_path, workdir, &squash_root, icon_name)
                })
            },
        );

    let saved_icon_path = icon_source.and_then(|source| save_extracted_icon(&source, slug));

    (parsed_name, parsed_comment, saved_icon_path, parsed_ver)
}

/// Déplace une AppImage déposée (Drag & Drop ou sélecteur de fichier) vers `~/Applications`,
/// lui accorde les permissions d'exécution (`+x`) et crée son raccourci dans le menu démarrer.
pub fn integrate_dropped_appimage(source_path: &Path) -> AppResult<PackageItem> {
    if !source_path.exists() {
        return Err(AppError::AppImageError(format!(
            "Le fichier « {} » est introuvable.",
            source_path.display()
        )));
    }

    if !is_appimage_file(source_path) {
        return Err(AppError::AppImageError(format!(
            "Le fichier « {} » n'est pas une AppImage valide (.AppImage ou binaire ELF AppImage).",
            source_path.display()
        )));
    }

    let apps_dir = ensure_applications_directory()?;
    let raw_file_name = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("Application.AppImage");

    let final_file_name = if raw_file_name.to_ascii_lowercase().ends_with(".appimage") {
        raw_file_name.to_string()
    } else {
        format!("{raw_file_name}.AppImage")
    };

    let dest_path = apps_dir.join(&final_file_name);

    let src_canon = fs::canonicalize(source_path).unwrap_or_else(|_| source_path.to_path_buf());
    let dest_canon = fs::canonicalize(&dest_path).unwrap_or_else(|_| dest_path.clone());

    if src_canon != dest_canon && fs::rename(source_path, &dest_path).is_err() {
        fs::copy(source_path, &dest_path)?;
        let _ = fs::remove_file(source_path);
    }

    let _ = ensure_executable_permission(&dest_path)?;
    let _ = create_or_update_desktop_shortcut(&dest_path)?;

    let meta = inspect_appimage(&dest_path)?;
    Ok(meta.into_package_item())
}

/// Inspecte un fichier AppImage et extrait ses métadonnées complètes (permissions, taille, date, `.desktop`).
pub fn inspect_appimage(path: &Path) -> AppResult<AppImageMetadata> {
    let metadata = fs::metadata(path)?;
    if !metadata.is_file() {
        return Err(AppError::AppImageError(format!(
            "Le chemin « {} » n'est pas un fichier régulier",
            path.display()
        )));
    }

    let is_executable = (metadata.permissions().mode() & 0o111) != 0;
    let size_bytes = metadata.len();
    let modified_unix_secs = metadata
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("AppImage");
    let (deduced_name, deduced_version) = deduce_name_and_version(file_name);
    let (desktop_name, desktop_comment, icon_name, desktop_ver) = extract_desktop_metadata(path);

    let final_name = desktop_name.unwrap_or(deduced_name);
    let final_version = desktop_ver.unwrap_or(deduced_version);

    Ok(AppImageMetadata {
        path: path.to_path_buf(),
        name: final_name,
        version: final_version,
        size_bytes,
        modified_unix_secs,
        is_executable,
        desktop_comment,
        icon_name,
    })
}

impl AppImageMetadata {
    pub fn into_package_item(self) -> PackageItem {
        let size_str = format_file_size(self.size_bytes);
        let date_str = format_unix_timestamp(self.modified_unix_secs);
        let perm_str = if self.is_executable {
            "Exécutable (+x)"
        } else {
            "Non exécutable (chmod +x requis)"
        };
        let menu_str = if has_desktop_shortcut(&self.path) {
            " · Raccourci menu démarrer actif"
        } else {
            ""
        };

        let description = match &self.desktop_comment {
            Some(comment) if !comment.is_empty() => {
                format!("{comment} — {size_str} · Modifié le {date_str} · {perm_str}{menu_str}")
            }
            _ => format!(
                "{} — {size_str} · Modifié le {date_str} · {perm_str}{menu_str}",
                self.path.display()
            ),
        };

        PackageItem {
            id: format!("appimage:{}", self.path.to_string_lossy()),
            name: self.name,
            version: self.version,
            description,
            source: PackageSource::AppImage(self.path),
            is_installed: true,
            update_available: None,
            aur_meta: None,
        }
    }
}

/// Scanne un répertoire unique à la recherche de fichiers AppImage.
pub fn scan_directory(dir: &Path) -> AppResult<Vec<PackageItem>> {
    if !dir.exists() || !dir.is_dir() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(dir)?;
    let mut items = Vec::new();

    for entry_res in entries {
        let Ok(entry) = entry_res else {
            continue;
        };
        let path = entry.path();
        if is_appimage_file(&path) {
            if let Ok(meta) = inspect_appimage(&path) {
                items.push(meta.into_package_item());
            }
        }
    }

    items.sort_by_key(|a| a.name.to_lowercase());
    Ok(items)
}

/// Scanne plusieurs répertoires et déduplique les AppImages trouvées par chemin canonique.
pub fn scan_directories(dirs: &[PathBuf]) -> AppResult<Vec<PackageItem>> {
    let mut all_items = Vec::new();
    let mut seen_paths = std::collections::HashSet::new();

    for dir in dirs {
        if let Ok(dir_items) = scan_directory(dir) {
            for item in dir_items {
                if let PackageSource::AppImage(ref path) = item.source {
                    let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.clone());
                    if seen_paths.insert(canonical) {
                        all_items.push(item);
                    }
                }
            }
        }
    }

    all_items.sort_by_key(|a| a.name.to_lowercase());
    Ok(all_items)
}

/// Scanne automatiquement `~/Applications`, `~/.local/bin` et `~/Desktop`,
/// en s'assurant que `~/Applications` existe et que chaque AppImage de `~/Applications` possède son raccourci menu démarrer.
pub fn scan_default_directories() -> AppResult<Vec<PackageItem>> {
    let apps_dir = ensure_applications_directory().ok();
    let dirs = default_appimage_directories();
    let mut items = scan_directories(&dirs)?;

    if let Some(ref main_apps_dir) = apps_dir {
        for item in &mut items {
            if let PackageSource::AppImage(ref path) = item.source {
                if path.starts_with(main_apps_dir)
                    && desktop_shortcut_needs_refresh(path)
                    && create_or_update_desktop_shortcut(path).is_ok()
                {
                    if let Ok(refreshed) = inspect_appimage(path) {
                        *item = refreshed.into_package_item();
                    }
                }
            }
        }
    }

    Ok(items)
}

/// Vérifie et applique la permission d'exécution (`chmod +x`) sur une AppImage si nécessaire.
pub fn ensure_executable_permission(path: &Path) -> AppResult<bool> {
    let metadata = fs::metadata(path)?;
    let mut perms = metadata.permissions();
    let mode = perms.mode();
    if (mode & 0o111) == 0 {
        perms.set_mode(mode | 0o755);
        fs::set_permissions(path, perms)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Lance l'exécution d'une AppImage en tâche de fond après avoir garanti sa permission d'exécution (`PermissionsExt`).
pub fn execute_appimage(path: &Path) -> AppResult<String> {
    if !path.exists() {
        return Err(AppError::AppImageError(format!(
            "Le fichier AppImage « {} » est introuvable.",
            path.display()
        )));
    }

    let chmod_applied = ensure_executable_permission(path)?;

    std::process::Command::new(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| {
            AppError::AppImageError(format!(
                "Impossible de lancer l'AppImage « {} » : {e}",
                path.display()
            ))
        })?;

    let file_label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("AppImage");

    if chmod_applied {
        Ok(format!(
            "Permission d'exécution accordée (+x) et « {file_label} » lancée."
        ))
    } else {
        Ok(format!("Application « {file_label} » lancée avec succès."))
    }
}

/// Supprime les icônes extraites associées à une AppImage (uniquement dans `~/.local/share/icons`).
fn remove_extracted_icon(appimage_path: &Path) {
    let user_icons_root = env::var_os("HOME").map(|home| {
        PathBuf::from(home)
            .join(".local")
            .join("share")
            .join("icons")
    });

    if let Some(icon) = appimage_desktop_icon_reference(appimage_path) {
        let icon_path = Path::new(&icon);
        if let Some(ref root) = user_icons_root {
            if icon_path.is_absolute() && icon_path.starts_with(root) {
                let _ = fs::remove_file(icon_path);
            }
        }
    }

    for icon_path in expected_extracted_icon_paths(appimage_path) {
        let _ = fs::remove_file(icon_path);
    }
}

/// Supprime un fichier AppImage du disque ainsi que son raccourci `.desktop` dans le menu démarrer.
pub fn remove_appimage(path: &Path) -> AppResult<String> {
    if !path.exists() {
        return Err(AppError::AppImageError(format!(
            "Le fichier AppImage « {} » n'existe déjà plus.",
            path.display()
        )));
    }

    let file_label = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("AppImage")
        .to_string();

    let abs_str = fs::canonicalize(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string();
    let raw_str = path.to_string_lossy().to_string();

    remove_extracted_icon(path);
    fs::remove_file(path)?;

    // Suppression des raccourcis .desktop associés dans ~/.local/share/applications
    if let Some(menu_dir) = user_applications_menu_dir() {
        if let Ok(entries) = fs::read_dir(menu_dir) {
            for entry_res in entries.flatten() {
                let p = entry_res.path();
                if p.extension().and_then(|e| e.to_str()) == Some("desktop") {
                    if let Ok(content) = fs::read_to_string(&p) {
                        if content.contains(&abs_str) || content.contains(&raw_str) {
                            let _ = fs::remove_file(&p);
                        }
                    }
                }
            }
        }
    }

    Ok(format!(
        "L'AppImage « {file_label} » et son raccourci du menu démarrer ont été supprimés."
    ))
}

#[derive(Debug, Deserialize)]
struct GithubReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Deserialize)]
struct GithubLatestRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GithubReleaseAsset>,
}

/// Résout le dépôt GitHub `(owner, repo)` associé à une AppImage connue.
pub fn resolve_appimage_github_repo(app_name: &str) -> Option<(String, String)> {
    let clean = app_name.trim().to_lowercase();
    let known: Option<(&str, &str)> = match clean.as_str() {
        s if s.contains("obsidian") => Some(("obsidianmd", "obsidian-releases")),
        s if s.contains("freecad") => Some(("FreeCAD", "FreeCAD")),
        s if s.contains("krita") => Some(("KDE", "krita")),
        s if s.contains("kdenlive") => Some(("KDE", "kdenlive")),
        s if s.contains("balena") || s.contains("etcher") => Some(("balena-io", "etcher")),
        s if s.contains("cura") => Some(("Ultimaker", "Cura")),
        s if s.contains("prusaslicer") => Some(("prusa3d", "PrusaSlicer")),
        s if s.contains("bitwarden") => Some(("bitwarden", "clients")),
        s if s.contains("joplin") => Some(("laurent22", "joplin")),
        s if s.contains("logseq") => Some(("logseq", "logseq")),
        s if s.contains("localsend") => Some(("localsend", "localsend")),
        s if s.contains("bruno") => Some(("usebruno", "bruno")),
        s if s.contains("heroic") => Some(("Heroic-Games-Launcher", "HeroicGamesLauncher")),
        s if s.contains("rpcs3") => Some(("RPCS3", "rpcs3")),
        s if s.contains("jan") => Some(("janhq", "jan")),
        _ => None,
    };

    if let Some((o, r)) = known {
        return Some((o.to_string(), r.to_string()));
    }

    resolve_github_owner_repo(app_name, None)
}

/// Vérifie auprès de l'API GitHub Releases si des mises à jour existent pour les AppImages installées.
/// Renvoie une table `item.id -> nouvelle_version`.
pub async fn check_appimage_updates(items: &[PackageItem]) -> HashMap<String, String> {
    let mut updates = HashMap::new();
    let appimages: Vec<&PackageItem> = items
        .iter()
        .filter(|i| matches!(i.source, PackageSource::AppImage(_)))
        .collect();

    if appimages.is_empty() {
        return updates;
    }

    let Ok(http) = Client::builder()
        .user_agent("noos-package-manager/0.1.0 (Arch Linux; GTK4/Libadwaita)")
        .timeout(Duration::from_secs(6))
        .connect_timeout(Duration::from_secs(4))
        .build()
    else {
        return updates;
    };

    for item in appimages {
        let Some((owner, repo)) = resolve_appimage_github_repo(&item.name) else {
            continue;
        };
        let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
        let Ok(resp) = http.get(&url).send().await else {
            continue;
        };
        if !resp.status().is_success() {
            continue;
        }
        let Ok(release) = resp.json::<GithubLatestRelease>().await else {
            continue;
        };

        let candidate_ver = release
            .tag_name
            .trim()
            .trim_start_matches(['v', 'V'])
            .to_string();
        if candidate_ver.is_empty() {
            continue;
        }

        if item.version != candidate_ver
            && (item.version == "Locale"
                || is_appimage_version_newer(&item.version, &candidate_ver).await)
        {
            updates.insert(item.id.clone(), candidate_ver);
        }
    }

    updates
}

async fn is_appimage_version_newer(current: &str, candidate: &str) -> bool {
    if current == candidate {
        return false;
    }
    if let Ok(out) = tokio::process::Command::new("vercmp")
        .arg(candidate)
        .arg(current)
        .output()
        .await
    {
        if out.status.success() {
            if let Ok(s) = String::from_utf8(out.stdout) {
                if let Ok(v) = s.trim().parse::<i32>() {
                    return v > 0;
                }
            }
        }
    }
    candidate != current
}

/// Télécharge la dernière version `.AppImage` depuis GitHub Releases pour une AppImage installée,
/// la place dans `~/Applications`, met à jour son raccourci menu démarrer et supprime l'ancien binaire.
pub async fn update_appimage_from_github(item: &PackageItem) -> AppResult<String> {
    let PackageSource::AppImage(ref old_path) = item.source else {
        return Err(AppError::AppImageError(String::from(
            "Ce paquet n'est pas une AppImage.",
        )));
    };

    let (owner, repo) = resolve_appimage_github_repo(&item.name).ok_or_else(|| {
        AppError::AppImageError(format!(
            "Aucun dépôt GitHub de publication directe trouvé pour « {} ».",
            item.name
        ))
    })?;

    let http = Client::builder()
        .user_agent("noos-package-manager/0.1.0 (Arch Linux; GTK4/Libadwaita)")
        .timeout(Duration::from_secs(120))
        .connect_timeout(Duration::from_secs(10))
        .build()?;

    let url = format!("https://api.github.com/repos/{owner}/{repo}/releases/latest");
    let release: GithubLatestRelease = http
        .get(&url)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    let asset = release
        .assets
        .iter()
        .find(|a| {
            let low = a.name.to_ascii_lowercase();
            low.ends_with(".appimage")
                && !low.contains("arm64")
                && !low.contains("aarch64")
                && !low.contains("armv7")
        })
        .ok_or_else(|| {
            AppError::AppImageError(format!(
                "Aucun fichier .AppImage x86_64 trouvé dans la release {} de {owner}/{repo}.",
                release.tag_name
            ))
        })?;

    let apps_dir = ensure_applications_directory()?;
    let dest_path = apps_dir.join(&asset.name);

    let bytes = http
        .get(&asset.browser_download_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;

    {
        let mut f = File::create(&dest_path)?;
        f.write_all(&bytes)?;
    }

    let _ = ensure_executable_permission(&dest_path)?;
    let _ = create_or_update_desktop_shortcut(&dest_path)?;

    let old_canon = fs::canonicalize(old_path).unwrap_or_else(|_| old_path.clone());
    let new_canon = fs::canonicalize(&dest_path).unwrap_or_else(|_| dest_path.clone());
    if old_canon != new_canon && old_path.exists() {
        let _ = fs::remove_file(old_path);
    }

    Ok(format!(
        "AppImage « {} » mise à jour vers {} dans ~/Applications.",
        item.name, release.tag_name
    ))
}
