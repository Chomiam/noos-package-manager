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
