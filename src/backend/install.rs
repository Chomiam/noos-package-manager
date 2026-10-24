use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::backend::appimage;
use crate::backend::aur::shared_aur_client;
use crate::backend::elevation::Elevation;
use crate::backend::flatpak;
use crate::backend::models::{
    DependencyStatus, DependencyStepItem, InstallCancelHandle, OptionalDependencyItem,
    PackageItem, PackageSource,
};
use crate::error::{AppError, AppResult};
use crate::ui::messages::UiEvent;

use super::catalog::SELF_PACKAGE_NAME;

/// Exécute une action d'installation pour un paquet donné selon sa source.
pub async fn install_item(name: &str, source: &PackageSource) -> AppResult<String> {
    match source {
        PackageSource::Pacman => crate::backend::pacman::install_package(name).await,
        PackageSource::Aur => crate::backend::aur::install_or_update_aur_package(name).await,
        PackageSource::Flatpak => flatpak::remove_flatpak_package(name).await,
        PackageSource::AppImage(path) => {
            let p: PathBuf = path.clone();
            tokio::task::spawn_blocking(move || appimage::execute_appimage(&p))
                .await
                .map_err(|e| AppError::AppImageError(e.to_string()))?
        }
    }
}

/// Exécute une action de mise à jour pour un paquet donné selon sa source.
pub async fn update_item(name: &str, source: &PackageSource) -> AppResult<String> {
    match source {
        PackageSource::Pacman => crate::backend::pacman::update_package(name).await,
        PackageSource::Aur => crate::backend::aur::install_or_update_aur_package(name).await,
        PackageSource::Flatpak => flatpak::remove_flatpak_package(name).await,
        PackageSource::AppImage(_) => Err(AppError::AppImageError(String::from(
            "Les fichiers AppImage locaux se mettent à jour en remplaçant le binaire.",
        ))),
    }
}

/// Exécute une action de suppression/désinstallation pour un paquet donné selon sa source.
pub async fn remove_item(name: &str, source: &PackageSource) -> AppResult<String> {
    match source {
        PackageSource::Pacman | PackageSource::Aur => crate::backend::pacman::remove_package(name).await,
        PackageSource::Flatpak => flatpak::remove_flatpak_package(name).await,
        PackageSource::AppImage(path) => {
            let p: PathBuf = path.clone();
            tokio::task::spawn_blocking(move || appimage::remove_appimage(&p))
                .await
                .map_err(|e| AppError::AppImageError(e.to_string()))?
        }
    }
}

/// Résout les dépendances optionnelles (`Optional Deps` Pacman et `OptDepends` AUR) pour un lot
/// de paquets sélectionnés en vue d'une installation.
/// Renvoie une liste vide si aucune dépendance optionnelle non installée n'est disponible.
pub async fn resolve_optional_dependencies(
    items: &[PackageItem],
) -> AppResult<Vec<OptionalDependencyItem>> {
    let pacman_targets: Vec<String> = items
        .iter()
        .filter(|i| matches!(i.source, PackageSource::Pacman) && !i.is_installed)
        .map(|i| i.name.clone())
        .collect();

    let aur_targets: Vec<String> = items
        .iter()
        .filter(|i| matches!(i.source, PackageSource::Aur) && !i.is_installed)
        .map(|i| i.name.clone())
        .collect();

    if pacman_targets.is_empty() && aur_targets.is_empty() {
        return Ok(Vec::new());
    }

    let explicit_names: HashSet<String> = items.iter().map(|i| i.name.clone()).collect();

    let mut ordered_dep_names: Vec<String> = Vec::new();
    let mut dep_meta_map: HashMap<String, (String, Vec<String>)> = HashMap::new();

    if !pacman_targets.is_empty() {
        if let Ok(pac_opt_map) = crate::backend::pacman::fetch_pacman_optdepends(&pacman_targets).await {
            for parent_name in &pacman_targets {
                if let Some(opt_list) = pac_opt_map.get(parent_name) {
                    for (dep_name, reason) in opt_list {
                        if explicit_names.contains(dep_name) {
                            continue;
                        }
                        let entry = dep_meta_map.entry(dep_name.clone()).or_insert_with(|| {
                            ordered_dep_names.push(dep_name.clone());
                            (reason.clone(), Vec::new())
                        });
                        if entry.0.is_empty() && !reason.is_empty() {
                            entry.0 = reason.clone();
                        }
                        if !entry.1.contains(parent_name) {
                            entry.1.push(parent_name.clone());
                        }
                    }
                }
            }
        }
    }

    let aur_client = shared_aur_client();

    if !aur_targets.is_empty() {
        if let Some(ref client) = aur_client {
            let refs: Vec<&str> = aur_targets.iter().map(String::as_str).collect();
            if let Ok(raw_pkgs) = client.fetch_raw_packages(&refs).await {
                for pkg in raw_pkgs {
                    for opt_raw in &pkg.opt_depends {
                        if let Some((dep_name, reason)) =
                            crate::backend::pacman::parse_optdepend_spec(opt_raw)
                        {
                            if explicit_names.contains(&dep_name) {
                                continue;
                            }
                            let entry = dep_meta_map.entry(dep_name.clone()).or_insert_with(|| {
                                ordered_dep_names.push(dep_name.clone());
                                (reason.clone(), Vec::new())
                            });
                            if entry.0.is_empty() && !reason.is_empty() {
                                entry.0 = reason;
                            }
                            if !entry.1.contains(&pkg.name) {
                                entry.1.push(pkg.name.clone());
                            }
                        }
                    }
                }
            }
        }
    }

    if ordered_dep_names.is_empty() {
        return Ok(Vec::new());
    }

    let (installed_res, unsatisfied_set) = tokio::join!(
        crate::backend::pacman::list_all_installed_versions(),
        crate::backend::pacman::check_unsatisfied_dependencies(&ordered_dep_names)
    );
    let installed_versions = installed_res.unwrap_or_default();

    // Si toutes les dépendances optionnelles sont déjà satisfaites sur le système, inutile d'afficher la fenêtre
    let has_any_uninstalled = ordered_dep_names
        .iter()
        .any(|name| !installed_versions.contains_key(name) && unsatisfied_set.contains(name));
    if !has_any_uninstalled {
        return Ok(Vec::new());
    }

    let pacman_si_map = crate::backend::pacman::fetch_pacman_si_map(&ordered_dep_names, &installed_versions)
        .await
        .unwrap_or_default();

    let unknown_names: Vec<String> = ordered_dep_names
        .iter()
        .filter(|name| !pacman_si_map.contains_key(*name))
        .cloned()
        .collect();

    let mut aur_info_map: HashMap<String, crate::backend::aur::AurRpcPackage> = HashMap::new();
    if !unknown_names.is_empty() {
        if let Some(ref client) = aur_client {
            let refs: Vec<&str> = unknown_names.iter().map(String::as_str).collect();
            if let Ok(aur_pkgs) = client.fetch_raw_packages(&refs).await {
                for p in aur_pkgs {
                    aur_info_map.insert(p.name.clone(), p);
                }
            }
        }
    }

    let mut results = Vec::new();
    for dep_name in ordered_dep_names {
        let Some((reason, required_by)) = dep_meta_map.remove(&dep_name) else {
            continue;
        };

        let is_installed =
            installed_versions.contains_key(&dep_name) || !unsatisfied_set.contains(&dep_name);

        if let Some(pac_item) = pacman_si_map.get(&dep_name) {
            let desc = if !reason.is_empty() {
                reason
            } else if !pac_item.description.is_empty() {
                pac_item.description.clone()
            } else {
                String::from("Dépendance optionnelle (dépôt officiel Pacman)")
            };
            results.push(OptionalDependencyItem {
                name: dep_name,
                version: pac_item.version.clone(),
                description: desc,
                required_by,
                source: PackageSource::Pacman,
                is_installed,
            });
        } else if let Some(aur_pkg) = aur_info_map.get(&dep_name) {
            let desc = if !reason.is_empty() {
                reason
            } else {
                aur_pkg
                    .description
                    .clone()
                    .unwrap_or_else(|| String::from("Dépendance optionnelle (AUR)"))
            };
            let ver = installed_versions
                .get(&dep_name)
                .cloned()
                .unwrap_or_else(|| aur_pkg.version.clone());
            results.push(OptionalDependencyItem {
                name: dep_name,
                version: ver,
                description: desc,
                required_by,
                source: PackageSource::Aur,
                is_installed,
            });
        } else if is_installed || crate::backend::pacman::can_resolve_in_pacman(&dep_name).await {
            let desc = if !reason.is_empty() {
                reason
            } else {
                String::from("Fonctionnalité ou dépendance optionnelle")
            };
            let ver = installed_versions
                .get(&dep_name)
                .cloned()
                .unwrap_or_else(|| String::from("fourni par dépôt"));
            results.push(OptionalDependencyItem {
                name: dep_name,
                version: ver,
                description: desc,
                required_by,
                source: PackageSource::Pacman,
                is_installed,
            });
        }
    }

    // Ne conserver que si au moins une dépendance optionnelle résolue n'est pas encore installée
    if !results.iter().any(|d| !d.is_installed) {
        return Ok(Vec::new());
    }

    // Placer les dépendances optionnelles non installées en tête de liste
    results.sort_by_key(|d| d.is_installed);

    Ok(results)
}


/// Vérifie avec needrestart les services à redémarrer après une transaction.
///
/// Le résultat est envoyé à l'interface seulement s'il y a quelque chose à signaler.
async fn prepare_service_restart_check(
    elevation: &Elevation,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> Option<crate::backend::needrestart::NeedrestartReport> {
    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: String::from("Vérification des services à redémarrer (needrestart)…"),
            fraction: 0.99,
            active_package: None,
            log_line: Some(String::from(
                "Analyse des bibliothèques obsolètes avec `needrestart -b`…",
            )),
        })
        .await;

    match crate::backend::needrestart::detect(elevation).await {
        Ok(report) => {
            if report.services.is_empty()
                && !crate::backend::needrestart::reboot_recommended(report.kernel_status)
            {
                None
            } else {
                Some(report)
            }
        }
        Err(err) => {
            crate::backend::logging::log_warn(
                "needrestart",
                format!("Vérification des services ignorée : {err}"),
            );
            None
        }
    }
}

/// Exécute l'installation d'un lot de paquets (Pacman, AUR et/ou Flatpak) avec résolution préalable
/// des dépendances et diffusion temps réel de la progression au Gestionnaire d'installation.
pub async fn install_batch_with_progress(
    items: &[PackageItem],
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) {
    cancel_handle.reset();

    let target_names: Vec<String> = items.iter().map(|i| i.name.clone()).collect();
    let installed_ids: Vec<String> = items.iter().map(|i| i.id.clone()).collect();

    let _ = ui_tx
        .send(UiEvent::InstallStarted {
            target_names: target_names.clone(),
        })
        .await;

    let pacman_targets: Vec<String> = items
        .iter()
        .filter(|i| matches!(i.source, PackageSource::Pacman))
        .map(|i| i.name.clone())
        .collect();

    let aur_targets: Vec<String> = items
        .iter()
        .filter(|i| matches!(i.source, PackageSource::Aur))
        .map(|i| i.name.clone())
        .collect();

    let flatpak_targets: Vec<String> = items
        .iter()
        .filter(|i| matches!(i.source, PackageSource::Flatpak))
        .map(|i| i.flatpak_app_id().unwrap_or(&i.name).to_string())
        .collect();

    // Une seule commande yay couvre Pacman + AUR ; elle déclenche au plus une
    // authentification sudo grâce à --sudoloop.
    let elevation = Elevation::detect();

    let mut all_deps: Vec<DependencyStepItem> = Vec::new();

    if !pacman_targets.is_empty() {
        match crate::backend::pacman::resolve_pacman_dependencies(&pacman_targets).await {
            Ok(mut deps) => all_deps.append(&mut deps),
            Err(e) => {
                let _ = ui_tx
                    .send(UiEvent::InstallProgress {
                        phase: format!("Avertissement résolution dépendances Pacman : {e}"),
                        fraction: 0.02,
                        active_package: None,
                        log_line: Some(format!("[warn] {e}")),
                    })
                    .await;
            }
        }
    }

    if !aur_targets.is_empty() {
        let installed_versions = crate::backend::pacman::list_all_installed_versions()
            .await
            .unwrap_or_default();
        if let Some(client) = shared_aur_client() {
            if let Ok(mut aur_deps) = client
                .resolve_aur_dependencies(&aur_targets, &installed_versions)
                .await
            {
                all_deps.append(&mut aur_deps);
            }
        }
    }

    if !flatpak_targets.is_empty() {
        if let Ok(mut fp_deps) = flatpak::resolve_flatpak_dependencies(&flatpak_targets).await {
            all_deps.append(&mut fp_deps);
        }
    }

    let total_download_bytes: u64 = all_deps.iter().map(|d| d.size_bytes).sum();
    let known_dep_names: Vec<String> = all_deps.iter().map(|d| d.name.clone()).collect();

    let _ = ui_tx
        .send(UiEvent::InstallDependenciesResolved {
            dependencies: all_deps,
            total_download_bytes,
        })
        .await;

    let mut summary_messages = Vec::new();

    let pacman_has_updates = items.iter().any(|i| {
        matches!(i.source, PackageSource::Pacman) && (i.update_available.is_some() || i.is_installed)
    });

    if !pacman_targets.is_empty() && !aur_targets.is_empty() {
        // Une seule transaction yay pour Pacman + AUR : une seule authentification sudo.
        let mut combined_targets = pacman_targets.clone();
        combined_targets.extend(aur_targets.iter().cloned());

        match crate::backend::aur::install_mixed_packages_streaming(
            &combined_targets,
            &known_dep_names,
            &elevation,
            cancel_handle,
            ui_tx,
        )
        .await
        {
            Ok(msg) => summary_messages.push(msg),
            Err(err) => {
                let _ = ui_tx
                    .send(UiEvent::InstallFinished {
                        success: false,
                        message: format!("Échec de l'installation Pacman + AUR : {err}"),
                        installed_ids: Vec::new(),
                    })
                    .await;
                return;
            }
        }
    } else if !pacman_targets.is_empty() {
        match crate::backend::pacman::install_packages_streaming(
            &pacman_targets,
            &known_dep_names,
            pacman_has_updates,
            &elevation,
            cancel_handle,
            ui_tx,
        )
        .await
        {
            Ok(msg) => summary_messages.push(msg),
            Err(err) => {
                let _ = ui_tx
                    .send(UiEvent::InstallFinished {
                        success: false,
                        message: format!("Échec de l'installation Pacman : {err}"),
                        installed_ids: Vec::new(),
                    })
                    .await;
                return;
            }
        }
    } else if !aur_targets.is_empty() {
        match crate::backend::aur::install_aur_packages_streaming(
            &aur_targets,
            &known_dep_names,
            &elevation,
            cancel_handle,
            ui_tx,
        )
        .await
        {
            Ok(msg) => summary_messages.push(msg),
            Err(err) => {
                let _ = ui_tx
                    .send(UiEvent::InstallFinished {
                        success: false,
                        message: format!("Échec de l'installation AUR : {err}"),
                        installed_ids: Vec::new(),
                    })
                    .await;
                return;
            }
        }
    }

    if !flatpak_targets.is_empty() {
        match flatpak::install_flatpak_packages_streaming(
            &flatpak_targets,
            &known_dep_names,
            cancel_handle,
            ui_tx,
        )
        .await
        {
            Ok(msg) => summary_messages.push(msg),
            Err(err) => {
                let _ = ui_tx
                    .send(UiEvent::InstallFinished {
                        success: false,
                        message: format!("Échec de l'installation Flatpak : {err}"),
                        installed_ids: Vec::new(),
                    })
                    .await;
                return;
            }
        }
    }

    // Vérification post-installation / post-mise à jour via `pacman -Q`
    if !pacman_targets.is_empty() || !aur_targets.is_empty() {
        let post_versions = crate::backend::pacman::list_all_installed_versions()
            .await
            .unwrap_or_default();

        for item in items {
            if matches!(item.source, PackageSource::Pacman | PackageSource::Aur) {
                match post_versions.get(&item.name) {
                    Some(inst_ver) => {
                        if let Some(ref expected_ver) = item.update_available {
                            if !item.version.is_empty()
                                && inst_ver == &item.version
                                && inst_ver != expected_ver
                            {
                                let _ = ui_tx
                                    .send(UiEvent::InstallFinished {
                                        success: false,
                                        message: format!(
                                            "La mise à jour de « {} » n'a pas été appliquée (version installée toujours {}). Vérifiez l'authentification administrateur.",
                                            item.name, inst_ver
                                        ),
                                        installed_ids: Vec::new(),
                                    })
                                    .await;
                                return;
                            }
                            let _ = ui_tx
                                .send(UiEvent::InstallProgress {
                                    phase: format!(
                                        "Vérification : « {} » mis à jour en version {}",
                                        item.name, inst_ver
                                    ),
                                    fraction: 0.98,
                                    active_package: Some((
                                        item.name.clone(),
                                        DependencyStatus::Completed,
                                    )),
                                    log_line: Some(format!(
                                        "[Vérifié] « {} » est maintenant installé en version {} (précédemment {}).",
                                        item.name, inst_ver, item.version
                                    )),
                                })
                                .await;
                        } else {
                            let _ = ui_tx
                                .send(UiEvent::InstallProgress {
                                    phase: format!(
                                        "Vérification : « {} » installé en version {}",
                                        item.name, inst_ver
                                    ),
                                    fraction: 0.98,
                                    active_package: Some((
                                        item.name.clone(),
                                        DependencyStatus::Completed,
                                    )),
                                    log_line: Some(format!(
                                        "[Vérifié] « {} » est installé en version {}.",
                                        item.name, inst_ver
                                    )),
                                })
                                .await;
                        }
                    }
                    None => {
                        let unsatisfied =
                            crate::backend::pacman::check_unsatisfied_dependencies(std::slice::from_ref(&item.name))
                                .await;
                        if unsatisfied.is_empty() {
                            let _ = ui_tx
                                .send(UiEvent::InstallProgress {
                                    phase: format!(
                                        "Vérification : dépendance « {} » satisfaite",
                                        item.name
                                    ),
                                    fraction: 0.98,
                                    active_package: Some((
                                        item.name.clone(),
                                        DependencyStatus::Completed,
                                    )),
                                    log_line: Some(format!(
                                        "[Vérifié] La dépendance ou provision « {} » est installée.",
                                        item.name
                                    )),
                                })
                                .await;
                        } else {
                            let _ = ui_tx
                                .send(UiEvent::InstallFinished {
                                    success: false,
                                    message: format!(
                                        "Vérification échouée : le paquet « {} » n'apparaît pas comme installé après la transaction.",
                                        item.name
                                    ),
                                    installed_ids: Vec::new(),
                                })
                                .await;
                            return;
                        }
                    }
                }
            }
        }
    }

    if target_names.iter().any(|n| n == SELF_PACKAGE_NAME) {
        sync_self_installed_binaries();
    }

    let service_report = if !pacman_targets.is_empty() || !aur_targets.is_empty() {
        prepare_service_restart_check(&elevation, ui_tx).await
    } else {
        None
    };

    let final_msg = if summary_messages.is_empty() {
        String::from("Installation terminée et vérifiée avec succès.")
    } else {
        summary_messages.join(" · ")
    };

    let _ = ui_tx
        .send(UiEvent::InstallFinished {
            success: true,
            message: final_msg,
            installed_ids,
        })
        .await;

    if let Some(report) = service_report {
        let _ = ui_tx
            .send(UiEvent::ServicesNeedRestart {
                services: report.services,
                kernel_status: report.kernel_status,
            })
            .await;
    }
}

/// Synchronise de façon atomique les binaires `/usr/bin/noos-package-manager` et `/usr/bin/noos-up`
/// avec `~/.cargo/bin/` et `~/.local/bin/` afin que le redémarrage automatique et le CLI lancent
/// toujours la nouvelle version fraîchement installée.
pub fn sync_self_installed_binaries() {
    use std::os::unix::fs::PermissionsExt;

    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };

    for bin_name in [SELF_PACKAGE_NAME, "noos-up"] {
        let usr_bin = PathBuf::from(format!("/usr/bin/{bin_name}"));
        if !usr_bin.is_file() {
            continue;
        }

        for rel_dir in [".cargo/bin", ".local/bin"] {
            let dir = home.join(rel_dir);
            let dest = dir.join(bin_name);
            if dest.exists() {
                let tmp_dest = dir.join(format!(".{bin_name}.new"));
                if std::fs::copy(&usr_bin, &tmp_dest).is_ok() {
                    if let Ok(meta) = std::fs::metadata(&tmp_dest) {
                        let mut perms = meta.permissions();
                        perms.set_mode(0o755);
                        let _ = std::fs::set_permissions(&tmp_dest, perms);
                    }
                    let _ = std::fs::rename(&tmp_dest, &dest);
                }
            }
        }
    }

    let desktop_path = home.join(".local/share/applications/noos-package-manager.desktop");
    if let Ok(content) = std::fs::read_to_string(&desktop_path) {
        let updated: String = content
            .lines()
            .map(|line| {
                if line.starts_with("Exec=") {
                    "Exec=noos-package-manager"
                } else {
                    line
                }
            })
            .collect::<Vec<&str>>()
            .join("\n");
        let _ = std::fs::write(&desktop_path, format!("{updated}\n"));
    }
}

/// Charge une vague supplémentaire de paquets AUR pour prolonger l'exploration paginée sans limite.
pub async fn load_more_aur_discovery_wave(wave_index: usize) -> Vec<PackageItem> {
    let Some(aur_client) = shared_aur_client() else {
        return Vec::new();
    };
    let all_installed_versions = crate::backend::pacman::list_all_installed_versions()
        .await
        .unwrap_or_default();
    let raw_wave = aur_client
        .fetch_discovery_wave(wave_index, &HashMap::new())
        .await;
    raw_wave
        .into_iter()
        .filter(|pkg| {
            !all_installed_versions.contains_key(&pkg.name) && pkg.name != SELF_PACKAGE_NAME
        })
        .map(|mut pkg| {
            pkg.is_installed = false;
            pkg.update_available = None;
            pkg
        })
        .collect()
}

/// Exécute la mise à jour globale de tous les paquets système (Pacman via `pacman -Syu`),
/// AUR (via `yay`) et Flatpak (`flatpak update`) avec suivi dans le Gestionnaire d'installation.
pub async fn update_all_system_and_aur_with_progress(
    catalog_update_items: &[PackageItem],
    ignored_updates: &[String],
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) {
    cancel_handle.reset();

    let _ = ui_tx
        .send(UiEvent::InstallStarted {
            target_names: vec![String::from("Mise à jour système (Pacman, AUR & Flatpak)")],
        })
        .await;

    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: String::from(
                "Analyse des mises à jour disponibles (Pacman, AUR, Flatpak)…",
            ),
            fraction: 0.02,
            active_package: None,
            log_line: Some(String::from(
                "Vérification des dépôts officiels, AUR et Flathub avant la transaction…",
            )),
        })
        .await;

    let (pacman_upd_res, aur_upd_res, flatpak_upd_res) = tokio::join!(
        crate::backend::pacman::check_updates_map(),
        crate::backend::aur::check_aur_updates_via_helper(),
        flatpak::check_flatpak_updates_map()
    );

    let mut pacman_updates = pacman_upd_res.unwrap_or_default();
    let mut aur_updates = aur_upd_res.unwrap_or_default();
    let mut flatpak_updates = flatpak_upd_res.unwrap_or_default();
    let mut appimage_updates = Vec::new();

    for item in catalog_update_items {
        let Some(ref new_ver) = item.update_available else {
            continue;
        };
        match &item.source {
            PackageSource::Pacman => {
                pacman_updates
                    .entry(item.name.clone())
                    .or_insert_with(|| (item.version.clone(), new_ver.clone()));
            }
            PackageSource::Aur => {
                aur_updates
                    .entry(item.name.clone())
                    .or_insert_with(|| (item.version.clone(), new_ver.clone()));
            }
            PackageSource::Flatpak => {
                let app_id = item.flatpak_app_id().unwrap_or(&item.name).to_string();
                flatpak_updates
                    .entry(app_id)
                    .or_insert_with(|| new_ver.clone());
            }
            PackageSource::AppImage(_) => {
                appimage_updates.push(item.clone());
            }
        }
    }

    let ignored: HashSet<&str> = ignored_updates.iter().map(String::as_str).collect();
    pacman_updates.retain(|name, _| {
        let pacman_id = format!("pacman:{name}");
        let aur_id = format!("aur:{name}");
        !ignored.contains(pacman_id.as_str()) && !ignored.contains(aur_id.as_str())
    });
    aur_updates.retain(|name, _| {
        let aur_id = format!("aur:{name}");
        !ignored.contains(aur_id.as_str())
    });
    flatpak_updates.retain(|app_id, _| {
        let flatpak_id = format!("flatpak:{app_id}");
        !ignored.contains(flatpak_id.as_str())
    });

    let mut updated_ids = Vec::new();
    let mut dep_steps = Vec::new();
    for (name, (_, new_ver)) in &pacman_updates {
        updated_ids.push(format!("pacman:{name}"));
        dep_steps.push(DependencyStepItem {
            name: name.clone(),
            version: new_ver.clone(),
            size_bytes: 0,
            is_explicit_target: true,
            source: PackageSource::Pacman,
            status: DependencyStatus::Pending,
        });
    }
    let mut aur_target_names = Vec::new();
    for (name, (_, new_ver)) in &aur_updates {
        updated_ids.push(format!("aur:{name}"));
        aur_target_names.push(name.clone());
        dep_steps.push(DependencyStepItem {
            name: name.clone(),
            version: new_ver.clone(),
            size_bytes: 0,
            is_explicit_target: true,
            source: PackageSource::Aur,
            status: DependencyStatus::Pending,
        });
    }
    for (app_id, new_ver) in &flatpak_updates {
        updated_ids.push(format!("flatpak:{app_id}"));
        dep_steps.push(DependencyStepItem {
            name: app_id.clone(),
            version: new_ver.clone(),
            size_bytes: 0,
            is_explicit_target: true,
            source: PackageSource::Flatpak,
            status: DependencyStatus::Pending,
        });
    }
    for app_item in &appimage_updates {
        updated_ids.push(app_item.id.clone());
    }

    let known_names: Vec<String> = dep_steps.iter().map(|d| d.name.clone()).collect();

    let _ = ui_tx
        .send(UiEvent::InstallDependenciesResolved {
            dependencies: dep_steps,
            total_download_bytes: 0,
        })
        .await;

    // Une seule commande yay (-Syu) couvre Pacman + AUR : une seule authentification sudo.
    let elevation = Elevation::detect();
    let has_pacman_updates = !pacman_updates.is_empty();
    let has_aur_updates = !aur_target_names.is_empty();

    let mut summaries = Vec::new();

    if has_pacman_updates && has_aur_updates {
        match crate::backend::aur::update_all_via_yay_streaming(
            &known_names,
            &elevation,
            cancel_handle,
            ui_tx,
        )
        .await
        {
            Ok(msg) => summaries.push(msg),
            Err(err) => {
                let _ = ui_tx
                    .send(UiEvent::InstallFinished {
                        success: false,
                        message: format!("Échec de la mise à jour Pacman + AUR : {err}"),
                        installed_ids: Vec::new(),
                    })
                    .await;
                return;
            }
        }
    } else {
        let will_run_pacman = has_pacman_updates
            || (aur_updates.is_empty() && flatpak_updates.is_empty() && appimage_updates.is_empty());

        if will_run_pacman {
            match crate::backend::pacman::update_pacman_all_streaming(
                &known_names,
                &elevation,
                cancel_handle,
                ui_tx,
            )
            .await
            {
                Ok(msg) => summaries.push(msg),
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::InstallFinished {
                            success: false,
                            message: format!("Échec de la mise à jour Pacman : {err}"),
                            installed_ids: Vec::new(),
                        })
                        .await;
                    return;
                }
            }
        }

        if has_aur_updates {
            match crate::backend::aur::install_aur_packages_streaming(
                &aur_target_names,
                &known_names,
                &elevation,
                cancel_handle,
                ui_tx,
            )
            .await
            {
                Ok(msg) => summaries.push(msg),
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::InstallFinished {
                            success: false,
                            message: format!("Échec de la mise à jour AUR : {err}"),
                            installed_ids: Vec::new(),
                        })
                        .await;
                    return;
                }
            }
        }
    }

    if !flatpak_updates.is_empty() {
        match flatpak::update_flatpak_all_streaming(&known_names, cancel_handle, ui_tx).await {
            Ok(msg) => summaries.push(msg),
            Err(err) => {
                let _ = ui_tx
                    .send(UiEvent::InstallFinished {
                        success: false,
                        message: format!("Échec de la mise à jour Flatpak : {err}"),
                        installed_ids: Vec::new(),
                    })
                    .await;
                return;
            }
        }
    }

    for app_item in &appimage_updates {
        match appimage::update_appimage_from_github(app_item).await {
            Ok(msg) => summaries.push(msg),
            Err(err) => {
                let _ = ui_tx
                    .send(UiEvent::InstallFinished {
                        success: false,
                        message: format!(
                            "Échec de la mise à jour AppImage « {} » : {err}",
                            app_item.name
                        ),
                        installed_ids: Vec::new(),
                    })
                    .await;
                return;
            }
        }
    }

    // Vérification post-mise à jour des versions installées via `pacman -Q`
    if !pacman_updates.is_empty() || !aur_updates.is_empty() {
        let post_versions = crate::backend::pacman::list_all_installed_versions()
            .await
            .unwrap_or_default();

        for (pkg_name, (old_ver, new_ver)) in pacman_updates.iter().chain(aur_updates.iter()) {
            if let Some(inst_ver) = post_versions.get(pkg_name) {
                if !old_ver.is_empty() && inst_ver == old_ver && inst_ver != new_ver {
                    let _ = ui_tx
                        .send(UiEvent::InstallFinished {
                            success: false,
                            message: format!(
                                "La mise à jour de « {pkg_name} » n'a pas été appliquée (toujours en version {inst_ver}). Vérifiez l'authentification administrateur."
                            ),
                            installed_ids: Vec::new(),
                        })
                        .await;
                    return;
                }
                let _ = ui_tx
                    .send(UiEvent::InstallProgress {
                        phase: format!("Vérification : « {pkg_name} » mis à jour en {inst_ver}"),
                        fraction: 0.98,
                        active_package: Some((pkg_name.clone(), DependencyStatus::Completed)),
                        log_line: Some(format!(
                            "[Vérifié] « {pkg_name} » mis à jour ({old_ver} → {inst_ver})."
                        )),
                    })
                    .await;
            }
        }
    }

    if updated_ids
        .iter()
        .any(|id| id == "aur:noos-package-manager" || id == "pacman:noos-package-manager")
    {
        sync_self_installed_binaries();
    }

    let service_report = if has_pacman_updates || has_aur_updates {
        prepare_service_restart_check(&elevation, ui_tx).await
    } else {
        None
    };

    let final_msg = if summaries.is_empty() {
        String::from("Système Pacman, AUR et Flatpak entièrement mis à jour.")
    } else {
        summaries.join(" · ")
    };

    let _ = ui_tx
        .send(UiEvent::InstallFinished {
            success: true,
            message: final_msg,
            installed_ids: updated_ids,
        })
        .await;

    if let Some(report) = service_report {
        let _ = ui_tx
            .send(UiEvent::ServicesNeedRestart {
                services: report.services,
                kernel_status: report.kernel_status,
            })
            .await;
    }
}
