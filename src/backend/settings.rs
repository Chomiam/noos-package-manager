use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::backend::logging::log_warn;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AppSettings {
    /// Identifiants des paquets dont les mises à jour sont volontairement ignorées
    /// (par exemple `aur:paquet` ou `flatpak:org.example.App`).
    #[serde(default)]
    pub ignored_updates: BTreeSet<String>,
    /// Préférences de compilation AUR/Yay.
    #[serde(default)]
    pub yay: YaySettings,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct YaySettings {
    /// Compilation multi-cœur via `MAKEFLAGS`.
    #[serde(default = "default_true")]
    pub multicore: bool,
    /// Désactive la compression des paquets (`PKGEXT=.pkg.tar`).
    #[serde(default)]
    pub disable_compression: bool,
    /// Utilise un répertoire de build dans un tmpfs (`BUILDDIR`).
    #[serde(default)]
    pub compile_in_ram: bool,
}

impl Default for YaySettings {
    fn default() -> Self {
        Self {
            multicore: true,
            disable_compression: false,
            compile_in_ram: false,
        }
    }
}

fn default_true() -> bool {
    true
}

/// Chemin du fichier de configuration (`$XDG_CONFIG_HOME/noos-package-manager/settings.json`).
pub fn settings_file_path() -> Option<PathBuf> {
    let base_dir = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|value| !value.is_empty())
                .map(|home| PathBuf::from(home).join(".config"))
        })?;

    Some(base_dir.join("noos-package-manager").join("settings.json"))
}

pub fn load_settings() -> AppSettings {
    let Some(path) = settings_file_path() else {
        return AppSettings::default();
    };
    let Ok(bytes) = fs::read(&path) else {
        return AppSettings::default();
    };

    match serde_json::from_slice(&bytes) {
        Ok(settings) => settings,
        Err(err) => {
            log_warn(
                "settings",
                format!(
                    "Configuration illisible ({}), valeurs par défaut : {err}",
                    path.display()
                ),
            );
            AppSettings::default()
        }
    }
}

pub fn save_settings(settings: &AppSettings) -> std::io::Result<()> {
    let Some(path) = settings_file_path() else {
        return Ok(());
    };
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)?;

    let payload = serde_json::to_vec_pretty(settings)
        .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))?;
    let tmp_path = parent.join(".settings.json.tmp");
    fs::write(&tmp_path, payload)?;
    fs::rename(tmp_path, &path)?;
    Ok(())
}

/// Ajoute ou retire un paquet de la liste des mises à jour ignorées.
pub fn set_update_ignored(package: &str, ignored: bool) {
    let mut settings = load_settings();
    let changed = if ignored {
        settings.ignored_updates.insert(package.to_string())
    } else {
        settings.ignored_updates.remove(package)
    };

    if changed {
        if let Err(err) = save_settings(&settings) {
            log_warn(
                "settings",
                format!("Impossible d'enregistrer les mises à jour ignorées : {err}"),
            );
        }
    }
}

/// Met à jour les préférences Yay et les sauvegarde.
pub fn update_yay_settings(update: impl FnOnce(&mut YaySettings)) -> std::io::Result<()> {
    let mut settings = load_settings();
    update(&mut settings.yay);
    save_settings(&settings)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_get_yay_defaults() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"ignored_updates":["aur:x"]}"#).unwrap();
        assert!(settings.yay.multicore);
        assert!(!settings.yay.disable_compression);
        assert!(!settings.yay.compile_in_ram);
    }

    #[test]
    fn obsolete_interactive_setting_is_ignored() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"yay":{"interactive_build_options":false}}"#).unwrap();
        assert!(settings.yay.multicore);
    }

    #[test]
    fn partial_yay_settings_get_defaults() {
        let settings: AppSettings =
            serde_json::from_str(r#"{"yay":{"disable_compression":true}}"#).unwrap();
        assert!(settings.yay.multicore);
        assert!(settings.yay.disable_compression);
    }
}
