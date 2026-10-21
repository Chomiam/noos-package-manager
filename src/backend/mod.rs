pub mod appimage;
pub mod aur;
pub mod details;
pub mod flatpak;
pub mod models;
pub mod pacman;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use crate::backend::aur::AurClient;
use crate::backend::models::{
    AurHelper, DependencyStatus, DependencyStepItem, InstallCancelHandle, PackageItem,
    PackageSource, RecommendationCategory,
};
use crate::error::AppResult;
use crate::ui::messages::UiEvent;

#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    pub packages: Vec<PackageItem>,
    pub recommendations: Vec<(RecommendationCategory, Vec<PackageItem>)>,
    pub flatpak_recommendations: Vec<(RecommendationCategory, Vec<PackageItem>)>,
    pub updates_count: usize,
    pub aur_helper: Option<AurHelper>,
    pub non_fatal_warnings: Vec<String>,
}

/// Charge l'état complet des paquets installés (Pacman + AUR), les recommandations Pacman
/// triées par catégories, les mises à jour disponibles (`checkupdates` + AUR RPC v5 `info`)
/// ainsi que les AppImages locales.
pub async fn load_system_catalog() -> AppResult<CatalogSnapshot> {
    let mut warnings = Vec::new();
    let aur_helper = aur::detect_aur_helper();

    let (pacman_upd_res, aur_upd_res) =
        tokio::join!(pacman::check_updates_map(), aur::check_aur_updates_via_helper());

    let mut updates_full = match pacman_upd_res {
        Ok(map) => map,
        Err(e) => {
            warnings.push(format!("Vérification des mises à jour Pacman : {e}"));
            HashMap::new()
        }
    };

    match aur_upd_res {
        Ok(aur_map) => {
            for (pkg_name, versions) in aur_map {
                updates_full.entry(pkg_name).or_insert(versions);
            }
        }
        Err(e) => {
            warnings.push(format!("Vérification des mises à jour AUR (yay/paru) : {e}"));
        }
    }

    let all_installed_versions = pacman::list_all_installed_versions()
        .await
        .unwrap_or_default();

    // Ne conserver que les mises à jour dont la version cible diffère réellement de la version installée
    updates_full.retain(|pkg_name, (_, new_v)| {
        all_installed_versions
            .get(pkg_name)
            .is_none_or(|inst_v| inst_v != new_v)
    });

    let updates_simple: HashMap<String, String> = updates_full
        .iter()
        .map(|(k, (_, new_v))| (k.clone(), new_v.clone()))
        .collect();

    let recommendations =
        match pacman::load_recommended_by_category(&all_installed_versions, &updates_simple).await {
            Ok(recs) => recs,
            Err(e) => {
                warnings.push(format!("Recommandations Pacman : {e}"));
                Vec::new()
            }
        };

    let mut installed_packages = match pacman::list_installed_packages(&updates_full).await {
        Ok(list) => list,
        Err(e) => {
            warnings.push(format!("Lecture des paquets Pacman : {e}"));
            Vec::new()
        }
    };

    // Seuls les paquets véritablement étrangers (`pacman -Qqm`) sont considérés comme installés depuis l'AUR.
    // Cela évite qu'un paquet installé depuis un dépôt Pacman tiers (ex: chaotic-aur) soit dupliqué en tant que paquet AUR.
    let foreign_set = pacman::list_foreign_package_names()
        .await
        .unwrap_or_default();
    let foreign_installed_versions: HashMap<String, String> = all_installed_versions
        .iter()
        .filter(|(name, _)| foreign_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let already_in_catalog: HashSet<String> = installed_packages
        .iter()
        .map(|pkg| pkg.name.clone())
        .collect();

    // Enrichissement des paquets AUR installés et chargement de la sélection AUR populaire via RPC v5 `info`
    let mut aur_query_names: Vec<String> = installed_packages
        .iter()
        .filter(|pkg| matches!(pkg.source, PackageSource::Aur))
        .map(|pkg| pkg.name.clone())
        .collect();

    for &showcase_pkg in aur::POPULAR_AUR_SHOWCASE {
        if !already_in_catalog.contains(showcase_pkg)
            && !aur_query_names.iter().any(|n| n == showcase_pkg)
        {
            aur_query_names.push(showcase_pkg.to_string());
        }
    }

    if !aur_query_names.is_empty() {
        if let Ok(aur_client) = AurClient::new() {
            let refs: Vec<&str> = aur_query_names.iter().map(String::as_str).collect();
            match aur_client
                .fetch_enriched_aur_items(&refs, &foreign_installed_versions)
                .await
            {
                Ok(enriched_aur) => {
                    let mut enriched_map: HashMap<String, PackageItem> = enriched_aur
                        .into_iter()
                        .map(|p| (p.name.clone(), p))
                        .collect();

                    for pkg in &mut installed_packages {
                        if matches!(pkg.source, PackageSource::Aur) {
                            if let Some(rich) = enriched_map.remove(&pkg.name) {
                                if pkg.update_available.is_none() {
                                    pkg.update_available = rich.update_available;
                                }
                                pkg.aur_meta = rich.aur_meta;
                                pkg.description = rich.description;
                            }
                        }
                    }

                    // Ajout des paquets AUR populaires non encore installés (ni via Pacman ni via AUR) pour le catalogue AUR
                    for (_, extra_aur_pkg) in enriched_map {
                        if !already_in_catalog.contains(&extra_aur_pkg.name) {
                            installed_packages.push(extra_aur_pkg);
                        }
                    }
                }
                Err(e) => {
                    warnings.push(format!("Vérification AUR RPC v5 : {e}"));
                }
            }
        }
    }

    // Chargement des applications Flatpak installées, des mises à jour Flatpak et des recommandations Flathub
    let flatpak_updates = flatpak::check_flatpak_updates_map()
        .await
        .unwrap_or_default();
    let mut installed_flatpak_map = HashMap::new();
    if let Ok(mut installed_fp) = flatpak::list_installed_flatpaks(&flatpak_updates).await {
        for fp in &installed_fp {
            if let Some(app_id) = fp.flatpak_app_id() {
                installed_flatpak_map.insert(app_id.to_string(), fp.version.clone());
            }
        }
        installed_packages.append(&mut installed_fp);
    }

    let flatpak_recommendations = match flatpak::load_flathub_recommendations_by_category(
        &installed_flatpak_map,
        &flatpak_updates,
    )
    .await
    {
        Ok(recs) => recs,
        Err(e) => {
            warnings.push(format!("Recommandations Flathub : {e}"));
            Vec::new()
        }
    };

    // Scan des AppImages locales (crée ~/Applications si besoin et vérifie les raccourcis menu démarrer)
    match tokio::task::spawn_blocking(appimage::scan_default_directories).await {
        Ok(Ok(mut appimages)) => {
            if !appimages.is_empty() {
                let appimage_updates = appimage::check_appimage_updates(&appimages).await;
                for app in &mut appimages {
                    if let Some(new_ver) = appimage_updates.get(&app.id) {
                        app.update_available = Some(new_ver.clone());
                    }
                }
            }
            installed_packages.append(&mut appimages);
        }
        Ok(Err(e)) => {
            warnings.push(format!("Scan AppImages : {e}"));
        }
        Err(e) => {
            warnings.push(format!("Tâche AppImage interrompue : {e}"));
        }
    }

    installed_packages.sort_by_key(|a| a.name.to_lowercase());

    let updates_count = installed_packages
        .iter()
        .filter(|pkg| pkg.update_available.is_some())
        .count();

    Ok(CatalogSnapshot {
        packages: installed_packages,
        recommendations,
        flatpak_recommendations,
        updates_count,
        aur_helper,
        non_fatal_warnings: warnings,
    })
}

/// Effectue une recherche combinée asynchrone sur Pacman (`pacman -Ss`), AUR RPC v5 (`search`)
/// et les fichiers AppImage locaux.
pub async fn search_all_sources(query: &str) -> AppResult<(Vec<PackageItem>, Vec<String>)> {
    let trimmed = query.trim().to_string();
    if trimmed.is_empty() {
        let snapshot = load_system_catalog().await?;
        return Ok((snapshot.packages, snapshot.non_fatal_warnings));
    }

    let mut warnings = Vec::new();

    let (pacman_upd_res, aur_upd_res) =
        tokio::join!(pacman::check_updates_map(), aur::check_aur_updates_via_helper());
    let mut updates_full = pacman_upd_res.unwrap_or_default();
    if let Ok(aur_map) = aur_upd_res {
        for (pkg_name, versions) in aur_map {
            updates_full.entry(pkg_name).or_insert(versions);
        }
    }
    let updates_simple: HashMap<String, String> = updates_full
        .into_iter()
        .map(|(k, (_, new_v))| (k, new_v))
        .collect();

    let installed_versions = pacman::list_all_installed_versions()
        .await
        .unwrap_or_default();
    let foreign_set = pacman::list_foreign_package_names()
        .await
        .unwrap_or_default();
    let foreign_installed_versions: HashMap<String, String> = installed_versions
        .iter()
        .filter(|(name, _)| foreign_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    let pacman_fut = pacman::search_packages(&trimmed, &updates_simple);
    let aur_fut = async {
        let client = AurClient::new()?;
        client.search(&trimmed, &foreign_installed_versions).await
    };
    let flatpak_fut = async {
        let fp_updates = flatpak::check_flatpak_updates_map()
            .await
            .unwrap_or_default();
        let mut fp_installed_map = HashMap::new();
        if let Ok(fp_list) = flatpak::list_installed_flatpaks(&fp_updates).await {
            for item in fp_list {
                if let Some(app_id) = item.flatpak_app_id() {
                    fp_installed_map.insert(app_id.to_string(), item.version);
                }
            }
        }
        flatpak::search_flatpak(&trimmed, &fp_installed_map, &fp_updates).await
    };
    let query_lower = trimmed.to_lowercase();
    let appimage_fut = tokio::task::spawn_blocking(move || -> AppResult<Vec<PackageItem>> {
        let all_appimages = appimage::scan_default_directories()?;
        let filtered = all_appimages
            .into_iter()
            .filter(|item| {
                item.name.to_lowercase().contains(&query_lower)
                    || item.description.to_lowercase().contains(&query_lower)
            })
            .collect();
        Ok(filtered)
    });

    let (pacman_res, aur_res, flatpak_res, appimage_res) =
        tokio::join!(pacman_fut, aur_fut, flatpak_fut, appimage_fut);

    let mut results = Vec::new();

    match pacman_res {
        Ok(mut pkgs) => results.append(&mut pkgs),
        Err(e) => warnings.push(format!("Recherche Pacman : {e}")),
    }

    match aur_res {
        Ok(mut pkgs) => results.append(&mut pkgs),
        Err(e) => warnings.push(format!("Recherche AUR : {e}")),
    }

    match flatpak_res {
        Ok(mut pkgs) => results.append(&mut pkgs),
        Err(e) => warnings.push(format!("Recherche Flatpak : {e}")),
    }

    match appimage_res {
        Ok(Ok(mut pkgs)) => results.append(&mut pkgs),
        Ok(Err(e)) => warnings.push(format!("Recherche AppImage : {e}")),
        Err(e) => warnings.push(format!("Tâche AppImage : {e}")),
    }

    // Tri par pertinence sans mettre les paquets installés en tête :
    // correspondance exacte, puis préfixe, puis non-installés avant installés, puis alphabétique.
    let q_low = trimmed.to_lowercase();
    results.sort_by(|a, b| {
        let a_low = a.name.to_lowercase();
        let b_low = b.name.to_lowercase();
        let a_exact = a_low == q_low;
        let b_exact = b_low == q_low;
        let a_prefix = a_low.starts_with(&q_low);
        let b_prefix = b_low.starts_with(&q_low);

        b_exact
            .cmp(&a_exact)
            .then_with(|| b_prefix.cmp(&a_prefix))
            .then_with(|| a.is_installed.cmp(&b.is_installed))
            .then_with(|| a_low.cmp(&b_low))
    });

    Ok((results, warnings))
}

/// Exécute une action d'installation pour un paquet donné selon sa source.
pub async fn install_item(name: &str, source: &PackageSource) -> AppResult<String> {
    match source {
        PackageSource::Pacman => pacman::install_package(name).await,
        PackageSource::Aur => aur::install_or_update_aur_package(name).await,
        PackageSource::Flatpak => flatpak::remove_flatpak_package(name).await,
        PackageSource::AppImage(path) => {
            let p: PathBuf = path.clone();
            tokio::task::spawn_blocking(move || appimage::execute_appimage(&p))
                .await
                .map_err(|e| crate::error::AppError::AppImageError(e.to_string()))?
        }
    }
}

/// Exécute une action de mise à jour pour un paquet donné selon sa source.
pub async fn update_item(name: &str, source: &PackageSource) -> AppResult<String> {
    match source {
        PackageSource::Pacman => pacman::update_package(name).await,
        PackageSource::Aur => aur::install_or_update_aur_package(name).await,
        PackageSource::Flatpak => flatpak::remove_flatpak_package(name).await,
        PackageSource::AppImage(_) => Err(crate::error::AppError::AppImageError(String::from(
            "Les fichiers AppImage locaux se mettent à jour en remplaçant le binaire.",
        ))),
    }
}

/// Exécute une action de suppression/désinstallation pour un paquet donné selon sa source.
pub async fn remove_item(name: &str, source: &PackageSource) -> AppResult<String> {
    match source {
        PackageSource::Pacman | PackageSource::Aur => pacman::remove_package(name).await,
        PackageSource::Flatpak => flatpak::remove_flatpak_package(name).await,
        PackageSource::AppImage(path) => {
            let p: PathBuf = path.clone();
            tokio::task::spawn_blocking(move || appimage::remove_appimage(&p))
                .await
                .map_err(|e| crate::error::AppError::AppImageError(e.to_string()))?
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

    let mut all_deps: Vec<DependencyStepItem> = Vec::new();

    if !pacman_targets.is_empty() {
        match pacman::resolve_pacman_dependencies(&pacman_targets).await {
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
        let installed_versions = pacman::list_all_installed_versions()
            .await
            .unwrap_or_default();
        if let Ok(client) = AurClient::new() {
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

    if !pacman_targets.is_empty() {
        match pacman::install_packages_streaming(
            &pacman_targets,
            &known_dep_names,
            pacman_has_updates,
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
    }

    if !aur_targets.is_empty() {
        match aur::install_aur_packages_streaming(
            &aur_targets,
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
        let post_versions = pacman::list_all_installed_versions()
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
                                            "La mise à jour de « {} » n'a pas été appliquée (version installée toujours {}). Vérifiez l'élévation de privilèges Polkit.",
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
}

/// Exécute la mise à jour globale de tous les paquets système (Pacman via `pkexec pacman -Syu`),
/// AUR (via `yay` / `paru`) et Flatpak (`flatpak update`) avec suivi dans le Gestionnaire d'installation.
pub async fn update_all_system_and_aur_with_progress(
    catalog_update_items: &[PackageItem],
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
        pacman::check_updates_map(),
        aur::check_aur_updates_via_helper(),
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

    let mut summaries = Vec::new();

    if !pacman_updates.is_empty()
        || (aur_updates.is_empty() && flatpak_updates.is_empty() && appimage_updates.is_empty())
    {
        match pacman::update_pacman_all_streaming(&known_names, cancel_handle, ui_tx).await {
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

    if !aur_target_names.is_empty() {
        match aur::install_aur_packages_streaming(
            &aur_target_names,
            &known_names,
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
        let post_versions = pacman::list_all_installed_versions()
            .await
            .unwrap_or_default();

        for (pkg_name, (old_ver, new_ver)) in pacman_updates.iter().chain(aur_updates.iter()) {
            if let Some(inst_ver) = post_versions.get(pkg_name) {
                if !old_ver.is_empty() && inst_ver == old_ver && inst_ver != new_ver {
                    let _ = ui_tx
                        .send(UiEvent::InstallFinished {
                            success: false,
                            message: format!(
                                "La mise à jour de « {pkg_name} » n'a pas été appliquée (toujours en version {inst_ver}). Vérifiez l'authentification Polkit."
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
}
