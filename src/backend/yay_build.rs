use std::env;
use std::path::PathBuf;

use tempfile::TempDir;

use crate::backend::settings::YaySettings;
use crate::error::{AppError, AppResult};

/// Environnement d'exécution Yay/makepkg pour une transaction AUR.
///
/// Toutes les options sont transmises par variables d'environnement :
/// - `MAKEFLAGS` pour le multi-cœur ;
/// - `PKGEXT` pour désactiver la compression ;
/// - `BUILDDIR` pour compiler dans un tmpfs.
pub struct YayBuildEnv {
    multicore: bool,
    disable_compression: bool,
    ram_dir: Option<TempDir>,
    interactive_build_options: bool,
}

impl YayBuildEnv {
    pub fn prepare(settings: &YaySettings) -> AppResult<Self> {
        let ram_dir = if settings.compile_in_ram {
            let base = ram_base_dir().ok_or_else(|| {
                AppError::AurHelperError(String::from(
                    "Impossible de trouver un répertoire tmpfs pour compiler en RAM.",
                ))
            })?;
            Some(
                tempfile::Builder::new()
                    .prefix("noos-yay-build-")
                    .tempdir_in(base)?,
            )
        } else {
            None
        };

        Ok(Self {
            multicore: settings.multicore,
            disable_compression: settings.disable_compression,
            ram_dir,
            interactive_build_options: settings.interactive_build_options,
        })
    }

    pub fn interactive_build_options(&self) -> bool {
        self.interactive_build_options
    }

    pub fn env_pairs(&self) -> Vec<(String, String)> {
        let mut pairs = Vec::new();

        let jobs = if self.multicore {
            std::thread::available_parallelism()
                .map(|count| count.get())
                .unwrap_or(1)
        } else {
            1
        };
        pairs.push((String::from("MAKEFLAGS"), format!("-j{jobs}")));

        if self.disable_compression {
            pairs.push((String::from("PKGEXT"), String::from(".pkg.tar")));
        }

        if let Some(dir) = &self.ram_dir {
            pairs.push((
                String::from("BUILDDIR"),
                dir.path().to_string_lossy().into_owned(),
            ));
        }

        pairs
    }
}

fn ram_base_dir() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
        .or_else(|| {
            let dev_shm = PathBuf::from("/dev/shm");
            dev_shm.is_dir().then_some(dev_shm)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(multicore: bool, compression: bool) -> YaySettings {
        YaySettings {
            multicore,
            disable_compression: compression,
            compile_in_ram: false,
            interactive_build_options: true,
        }
    }

    #[test]
    fn builds_makeflags_for_all_cores() {
        let env = YayBuildEnv::prepare(&settings(true, false)).unwrap();
        let pairs = env.env_pairs();
        let makeflags = pairs
            .iter()
            .find(|(key, _)| key == "MAKEFLAGS")
            .map(|(_, value)| value.clone())
            .unwrap();
        assert!(makeflags.starts_with("-j"), "{makeflags}");
        assert_ne!(makeflags, "-j0");
        assert!(!pairs.iter().any(|(key, _)| key == "PKGEXT"));
    }

    #[test]
    fn disables_compression_and_forces_single_job() {
        let env = YayBuildEnv::prepare(&settings(false, true)).unwrap();
        let pairs = env.env_pairs();
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "MAKEFLAGS" && value == "-j1"));
        assert!(pairs
            .iter()
            .any(|(key, value)| key == "PKGEXT" && value == ".pkg.tar"));
    }
}
