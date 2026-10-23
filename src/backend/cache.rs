use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::backend::models::{PackageItem, PackageSource, RecommendationCategory};
use crate::backend::{aur, CatalogSnapshot, SELF_PACKAGE_NAME, SELF_PACKAGE_VERSION};

pub const CACHE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalCatalogCache {
    pub version: u32,
    pub saved_at_unix: u64,
    pub packages: Vec<PackageItem>,
    pub recommendations: Vec<(RecommendationCategory, Vec<PackageItem>)>,
    pub flatpak_recommendations: Vec<(RecommendationCategory, Vec<PackageItem>)>,
}

/// Résultat du calcul de différentiel incrémental entre le cache local et l'état actuel de Pacman/AUR.
#[derive(Debug, Clone, Default)]
pub struct PacmanAurDiffPlan {
    pub retained_installed: Vec<PackageItem>,
    pub added_installed_names: Vec<String>,
    pub retained_showcase: Vec<PackageItem>,
    pub missing_showcase_names: Vec<String>,
}

/// Retourne le chemin du fichier de cache local (`$XDG_CACHE_HOME/noos-package-manager/catalog_cache_v1.json`
/// ou `~/.cache/noos-package-manager/catalog_cache_v1.json`).
pub fn cache_file_path() -> Option<PathBuf> {
    let base_dir = std::env::var_os("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;

    Some(
        base_dir
            .join("noos-package-manager")
            .join("catalog_cache_v1.json"),
    )
}

/// Charge le catalogue mis en cache sur le disque s'il existe et est valide.
pub fn load_cached_snapshot() -> Option<CatalogSnapshot> {
    let path = cache_file_path()?;
    let bytes = std::fs::read(&path).ok()?;
    let mut cache: LocalCatalogCache = serde_json::from_slice(&bytes).ok()?;

    if cache.version != CACHE_SCHEMA_VERSION || cache.packages.is_empty() {
        return None;
    }

    let self_running_ver = format!("{SELF_PACKAGE_VERSION}-1");
    for pkg in &mut cache.packages {
        if pkg.name == SELF_PACKAGE_NAME {
            pkg.is_installed = true;
            if pkg.update_available.as_deref() == Some(self_running_ver.as_str()) {
                pkg.version = self_running_ver.clone();
                pkg.update_available = None;
            }
        }
    }

    let updates_count = cache
        .packages
        .iter()
        .filter(|pkg| pkg.update_available.is_some())
        .count();

    Some(CatalogSnapshot {
        packages: cache.packages,
        recommendations: cache.recommendations,
        flatpak_recommendations: cache.flatpak_recommendations,
        updates_count,
        aur_helper: aur::detect_aur_helper(),
        non_fatal_warnings: Vec::new(),
    })
}

/// Sauvegarde de manière atomique le catalogue complet dans le cache local.
pub fn save_cached_snapshot(snapshot: &CatalogSnapshot) {
    let Some(path) = cache_file_path() else {
        return;
    };
    let Some(parent) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }

    let saved_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let payload = LocalCatalogCache {
        version: CACHE_SCHEMA_VERSION,
        saved_at_unix,
        packages: snapshot.packages.clone(),
        recommendations: snapshot.recommendations.clone(),
        flatpak_recommendations: snapshot.flatpak_recommendations.clone(),
    };

    let Ok(bytes) = serde_json::to_vec(&payload) else {
        return;
    };

    let tmp_path = parent.join(".catalog_cache_v1.json.tmp");
    if std::fs::write(&tmp_path, bytes).is_ok() {
        let _ = std::fs::rename(&tmp_path, &path);
    }
}

/// Calcule le différentiel incrémental entre les paquets Pacman/AUR présents dans le cache
/// et l'état réel du système (`pacman -Q`, `pacman -Qqe`, `pacman -Qqm`).
/// Seuls les paquets nouvellement installés (`added_installed_names`) nécessitent un appel à `pacman -Qi`.
pub fn compute_pacman_aur_diff(
    cached_packages: &[PackageItem],
    all_installed_versions: &HashMap<String, String>,
    explicit_set: &HashSet<String>,
    foreign_set: &HashSet<String>,
    showcase_names: &[&str],
) -> PacmanAurDiffPlan {
    let mut cached_by_name: HashMap<String, PackageItem> = HashMap::new();
    for pkg in cached_packages {
        if matches!(pkg.source, PackageSource::Pacman | PackageSource::Aur) {
            cached_by_name.insert(pkg.name.clone(), pkg.clone());
        }
    }

    // Ensemble cible des paquets installés à afficher :
    // 1. Tous les paquets explicitement installés (`pacman -Qqe`)
    // 2. Les éventuels paquets dépendances en cache ayant une mise à jour en attente et toujours installés
    // 3. Notre propre paquet `noos-package-manager`
    let mut target_installed: HashSet<String> = explicit_set.clone();
    for (name, pkg) in &cached_by_name {
        if pkg.is_installed
            && pkg.update_available.is_some()
            && all_installed_versions.contains_key(name)
        {
            target_installed.insert(name.clone());
        }
    }
    target_installed.insert(SELF_PACKAGE_NAME.to_string());

    let mut retained_installed = Vec::with_capacity(target_installed.len());
    let mut added_installed_names = Vec::new();

    let self_running_ver = format!("{SELF_PACKAGE_VERSION}-1");

    let mut sorted_targets: Vec<String> = target_installed.into_iter().collect();
    sorted_targets.sort();

    for name in sorted_targets {
        let inst_ver_opt = all_installed_versions.get(&name).cloned().or_else(|| {
            if name == SELF_PACKAGE_NAME {
                Some(self_running_ver.clone())
            } else {
                None
            }
        });

        let Some(inst_ver) = inst_ver_opt else {
            continue;
        };

        let is_foreign = if name == SELF_PACKAGE_NAME {
            !all_installed_versions.contains_key(SELF_PACKAGE_NAME)
                || foreign_set.contains(SELF_PACKAGE_NAME)
        } else {
            foreign_set.contains(&name)
        };

        if let Some(mut cached_pkg) = cached_by_name.remove(&name) {
            cached_pkg.source = if is_foreign {
                PackageSource::Aur
            } else {
                PackageSource::Pacman
            };
            let prefix = if is_foreign { "aur" } else { "pacman" };
            cached_pkg.id = format!("{prefix}:{name}");
            cached_pkg.is_installed = true;
            cached_pkg.version = inst_ver;

            if cached_pkg.update_available.as_deref() == Some(cached_pkg.version.as_str()) {
                cached_pkg.update_available = None;
            }

            retained_installed.push(cached_pkg);
        } else if name == SELF_PACKAGE_NAME {
            retained_installed.push(PackageItem {
                id: format!("aur:{SELF_PACKAGE_NAME}"),
                name: SELF_PACKAGE_NAME.to_string(),
                version: inst_ver,
                description: String::from(
                    "Gestionnaire de paquets GTK4/Libadwaita (Pacman, AUR, Flatpak et AppImages)",
                ),
                source: PackageSource::Aur,
                is_installed: true,
                update_available: None,
                aur_meta: None,
            });
        } else {
            added_installed_names.push(name);
        }
    }

    // Gestion de la sélection AUR populaire non installée
    let mut retained_showcase = Vec::new();
    let mut missing_showcase_names = Vec::new();

    for &showcase_name in showcase_names {
        if all_installed_versions.contains_key(showcase_name)
            || showcase_name == SELF_PACKAGE_NAME
        {
            continue;
        }

        if let Some(mut cached_pkg) = cached_by_name.remove(showcase_name) {
            cached_pkg.id = format!("aur:{showcase_name}");
            cached_pkg.source = PackageSource::Aur;
            cached_pkg.is_installed = false;
            cached_pkg.update_available = None;
            retained_showcase.push(cached_pkg);
        } else {
            missing_showcase_names.push(showcase_name.to_string());
        }
    }

    // Conserver également tous les paquets AUR supplémentaires découverts et mis en cache
    for (_, mut extra_pkg) in cached_by_name {
        if matches!(extra_pkg.source, PackageSource::Aur)
            && !all_installed_versions.contains_key(&extra_pkg.name)
            && extra_pkg.name != SELF_PACKAGE_NAME
        {
            extra_pkg.id = format!("aur:{}", extra_pkg.name);
            extra_pkg.is_installed = false;
            extra_pkg.update_available = None;
            retained_showcase.push(extra_pkg);
        }
    }

    PacmanAurDiffPlan {
        retained_installed,
        added_installed_names,
        retained_showcase,
        missing_showcase_names,
    }
}

/// Met à jour en mémoire l'état d'installation, la version et les mises à jour disponibles
/// des recommandations Pacman à partir du différentiel système.
pub fn reconcile_pacman_recommendations(
    recommendations: &mut [(RecommendationCategory, Vec<PackageItem>)],
    all_installed_versions: &HashMap<String, String>,
    updates_simple: Option<&HashMap<String, String>>,
) {
    for (_, items) in recommendations.iter_mut() {
        for item in items.iter_mut() {
            if let Some(inst_ver) = all_installed_versions.get(&item.name) {
                item.is_installed = true;
                item.version = inst_ver.clone();
                if let Some(upd_map) = updates_simple {
                    item.update_available = upd_map
                        .get(&item.name)
                        .filter(|new_v| *new_v != inst_ver)
                        .cloned();
                } else if item.update_available.as_deref() == Some(inst_ver.as_str()) {
                    item.update_available = None;
                }
            } else {
                item.is_installed = false;
                item.update_available = None;
            }
        }
        items.sort_by_key(|item| item.is_installed);
    }
}

/// Met à jour en mémoire l'état d'installation, la version et les mises à jour disponibles
/// des recommandations Flathub à partir du différentiel Flatpak.
pub fn reconcile_flatpak_recommendations(
    flatpak_recommendations: &mut [(RecommendationCategory, Vec<PackageItem>)],
    installed_flatpak_map: &HashMap<String, String>,
    flatpak_updates: Option<&HashMap<String, String>>,
) {
    for (_, items) in flatpak_recommendations.iter_mut() {
        for item in items.iter_mut() {
            let app_id_opt = item.flatpak_app_id().map(ToString::to_string);
            if let Some(app_id) = app_id_opt {
                if let Some(inst_ver) = installed_flatpak_map.get(&app_id) {
                    item.is_installed = true;
                    item.version = inst_ver.clone();
                    if let Some(upd_map) = flatpak_updates {
                        item.update_available = upd_map
                            .get(&app_id)
                            .filter(|new_v| *new_v != inst_ver)
                            .cloned();
                    } else if item.update_available.as_deref() == Some(inst_ver.as_str()) {
                        item.update_available = None;
                    }
                } else {
                    item.is_installed = false;
                    item.update_available = None;
                }
            }
        }
        items.sort_by_key(|item| item.is_installed);
    }
}
