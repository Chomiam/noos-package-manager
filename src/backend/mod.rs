pub mod appimage;
pub mod aur;
pub mod cache;
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
    AurHelper, DependencyStatus, DependencyStepItem, InstallCancelHandle, OptionalDependencyItem,
    PackageItem, PackageSource, RecommendationCategory,
};
use crate::error::AppResult;
use crate::ui::messages::UiEvent;

pub const SELF_PACKAGE_NAME: &str = "noos-package-manager";
pub const SELF_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    load_system_catalog_with_options(false).await
}

/// Charge l'état complet du catalogue avec option de synchronisation forcée des bases distantes (`force_sync`).
pub async fn load_system_catalog_with_options(force_sync: bool) -> AppResult<CatalogSnapshot> {
    let mut warnings = Vec::new();
    let aur_helper = aur::detect_aur_helper();

    let (pacman_upd_res, aur_upd_res) = tokio::join!(
        pacman::check_updates_map_with_mode(force_sync),
        aur::check_aur_updates_via_helper()
    );

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
            warnings.push(format!("Vérification des mises à jour AUR (yay) : {e}"));
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
    let mut foreign_installed_versions: HashMap<String, String> = all_installed_versions
        .iter()
        .filter(|(name, _)| foreign_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Garantit que notre propre programme (`noos-package-manager`) est toujours reconnu comme installé
    // avec sa version courante d'exécution afin que toute mise à jour disponible sur l'AUR soit détectée.
    let self_running_ver = format!("{SELF_PACKAGE_VERSION}-1");
    let self_effective_ver = all_installed_versions
        .get(SELF_PACKAGE_NAME)
        .cloned()
        .unwrap_or(self_running_ver);
    foreign_installed_versions.insert(SELF_PACKAGE_NAME.to_string(), self_effective_ver.clone());

    if !installed_packages.iter().any(|p| p.name == SELF_PACKAGE_NAME) {
        installed_packages.push(PackageItem {
            id: format!("aur:{SELF_PACKAGE_NAME}"),
            name: SELF_PACKAGE_NAME.to_string(),
            version: self_effective_ver,
            description: String::from(
                "Gestionnaire de paquets GTK4/Libadwaita (Pacman, AUR, Flatpak et AppImages)",
            ),
            source: PackageSource::Aur,
            is_installed: true,
            update_available: None,
            aur_meta: None,
        });
    }

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

    let snapshot = CatalogSnapshot {
        packages: installed_packages,
        recommendations,
        flatpak_recommendations,
        updates_count,
        aur_helper,
        non_fatal_warnings: warnings,
    };

    cache::save_cached_snapshot(&snapshot);

    Ok(snapshot)
}

/// Applique un différentiel incrémental rapide sur un `CatalogSnapshot` issu du cache local :
/// ne requête `pacman -Qi` et l'AUR RPC v5 que pour les paquets nouvellement installés ou retirés
/// depuis la dernière sauvegarde du cache. Renvoie `(snapshot_reconcilie, a_change)`.
pub async fn apply_incremental_catalog_diff(
    cached: &CatalogSnapshot,
) -> AppResult<(CatalogSnapshot, bool)> {
    let mut warnings = Vec::new();
    let aur_helper = aur::detect_aur_helper();

    let mut cached_flatpak_updates: HashMap<String, String> = HashMap::new();
    let mut cached_appimage_updates: HashMap<String, String> = HashMap::new();

    for pkg in &cached.packages {
        if let Some(ref upd) = pkg.update_available {
            match pkg.source {
                PackageSource::Flatpak => {
                    if let Some(app_id) = pkg.flatpak_app_id() {
                        cached_flatpak_updates.insert(app_id.to_string(), upd.clone());
                    }
                }
                PackageSource::AppImage(_) => {
                    cached_appimage_updates.insert(pkg.id.clone(), upd.clone());
                }
                _ => {}
            }
        }
    }

    let (all_ver_res, explicit_res, foreign_res, fp_res, appimage_res) = tokio::join!(
        pacman::list_all_installed_versions(),
        pacman::list_explicit_package_names(),
        pacman::list_foreign_package_names(),
        flatpak::list_installed_flatpaks(&cached_flatpak_updates),
        tokio::task::spawn_blocking(appimage::scan_default_directories),
    );

    let all_installed_versions = all_ver_res.unwrap_or_default();
    let explicit_set = explicit_res.unwrap_or_default();
    let foreign_set = foreign_res.unwrap_or_default();

    let diff_plan = cache::compute_pacman_aur_diff(
        &cached.packages,
        &all_installed_versions,
        &explicit_set,
        &foreign_set,
        aur::POPULAR_AUR_SHOWCASE,
    );

    let mut packages = diff_plan.retained_installed;

    // Interroge `pacman -Qi` uniquement pour le différentiel de paquets nouvellement installés
    if !diff_plan.added_installed_names.is_empty() {
        match pacman::fetch_installed_packages_by_names(
            &diff_plan.added_installed_names,
            &foreign_set,
            &HashMap::new(),
        )
        .await
        {
            Ok(mut added_items) => packages.append(&mut added_items),
            Err(e) => warnings.push(format!("Lecture différentielle Pacman : {e}")),
        }
    }

    let mut foreign_installed_versions: HashMap<String, String> = all_installed_versions
        .iter()
        .filter(|(name, _)| foreign_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let self_effective_ver = all_installed_versions
        .get(SELF_PACKAGE_NAME)
        .cloned()
        .unwrap_or_else(|| format!("{SELF_PACKAGE_VERSION}-1"));
    foreign_installed_versions.insert(SELF_PACKAGE_NAME.to_string(), self_effective_ver);

    let mut aur_missing_names: Vec<String> = diff_plan
        .added_installed_names
        .iter()
        .filter(|n| foreign_set.contains(*n))
        .cloned()
        .collect();

    for showcase_missing in &diff_plan.missing_showcase_names {
        if !aur_missing_names.contains(showcase_missing) {
            aur_missing_names.push(showcase_missing.clone());
        }
    }

    packages.extend(diff_plan.retained_showcase);

    if !aur_missing_names.is_empty() {
        if let Ok(aur_client) = AurClient::new() {
            let refs: Vec<&str> = aur_missing_names.iter().map(String::as_str).collect();
            if let Ok(enriched_aur) = aur_client
                .fetch_enriched_aur_items(&refs, &foreign_installed_versions)
                .await
            {
                let mut enriched_map: HashMap<String, PackageItem> = enriched_aur
                    .into_iter()
                    .map(|p| (p.name.clone(), p))
                    .collect();

                for pkg in &mut packages {
                    if matches!(pkg.source, PackageSource::Aur) {
                        if let Some(rich) = enriched_map.remove(&pkg.name) {
                            pkg.aur_meta = rich.aur_meta;
                            pkg.description = rich.description;
                            if pkg.update_available.is_none() {
                                pkg.update_available = rich.update_available;
                            }
                        }
                    }
                }

                let existing_names: HashSet<String> =
                    packages.iter().map(|p| p.name.clone()).collect();
                for (_, extra_aur_pkg) in enriched_map {
                    if !existing_names.contains(&extra_aur_pkg.name) {
                        packages.push(extra_aur_pkg);
                    }
                }
            }
        }
    }

    let mut installed_flatpak_map = HashMap::new();
    if let Ok(mut installed_fp) = fp_res {
        for fp in &mut installed_fp {
            if fp.update_available.as_deref() == Some(fp.version.as_str()) {
                fp.update_available = None;
            }
            if let Some(app_id) = fp.flatpak_app_id() {
                installed_flatpak_map.insert(app_id.to_string(), fp.version.clone());
            }
        }
        packages.append(&mut installed_fp);
    }

    match appimage_res {
        Ok(Ok(mut appimages)) => {
            for app in &mut appimages {
                if let Some(upd_ver) = cached_appimage_updates.get(&app.id) {
                    if &app.version != upd_ver {
                        app.update_available = Some(upd_ver.clone());
                    }
                }
            }
            packages.append(&mut appimages);
        }
        Ok(Err(e)) => warnings.push(format!("Scan AppImages : {e}")),
        Err(e) => warnings.push(format!("Tâche AppImage interrompue : {e}")),
    }

    packages.sort_by_key(|a| a.name.to_lowercase());

    let mut recommendations = if cached.recommendations.is_empty() {
        pacman::load_recommended_by_category(&all_installed_versions, &HashMap::new())
            .await
            .unwrap_or_default()
    } else {
        cached.recommendations.clone()
    };
    cache::reconcile_pacman_recommendations(&mut recommendations, &all_installed_versions, None);

    let mut flatpak_recommendations = if cached.flatpak_recommendations.is_empty() {
        flatpak::load_flathub_recommendations_by_category(
            &installed_flatpak_map,
            &cached_flatpak_updates,
        )
        .await
        .unwrap_or_default()
    } else {
        cached.flatpak_recommendations.clone()
    };
    cache::reconcile_flatpak_recommendations(
        &mut flatpak_recommendations,
        &installed_flatpak_map,
        None,
    );

    let updates_count = packages
        .iter()
        .filter(|pkg| pkg.update_available.is_some())
        .count();

    let has_diff = packages != cached.packages
        || recommendations != cached.recommendations
        || flatpak_recommendations != cached.flatpak_recommendations;

    let snapshot = CatalogSnapshot {
        packages,
        recommendations,
        flatpak_recommendations,
        updates_count,
        aur_helper,
        non_fatal_warnings: warnings,
    };

    if has_diff {
        cache::save_cached_snapshot(&snapshot);
    }

    Ok((snapshot, has_diff))
}

/// Actualise en arrière-plan toutes les mises à jour disponibles (Pacman, AUR, Flatpak, AppImages)
/// sur un `CatalogSnapshot` existant et met à jour le cache local.
pub async fn refresh_catalog_updates(
    mut snapshot: CatalogSnapshot,
    force_sync: bool,
) -> AppResult<CatalogSnapshot> {
    let mut warnings = Vec::new();
    snapshot.aur_helper = aur::detect_aur_helper();

    let appimage_items: Vec<PackageItem> = snapshot
        .packages
        .iter()
        .filter(|p| matches!(p.source, PackageSource::AppImage(_)))
        .cloned()
        .collect();

    let (
        pacman_upd_res,
        aur_upd_res,
        all_ver_res,
        explicit_res,
        foreign_res,
        fp_upd_res,
        appimage_updates,
    ) = tokio::join!(
        pacman::check_updates_map_with_mode(force_sync),
        aur::check_aur_updates_via_helper(),
        pacman::list_all_installed_versions(),
        pacman::list_explicit_package_names(),
        pacman::list_foreign_package_names(),
        flatpak::check_flatpak_updates_map(),
        async {
            if appimage_items.is_empty() {
                HashMap::new()
            } else {
                appimage::check_appimage_updates(&appimage_items).await
            }
        }
    );

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
            warnings.push(format!("Vérification des mises à jour AUR (yay) : {e}"));
        }
    }

    let all_installed_versions = all_ver_res.unwrap_or_default();
    let explicit_set = explicit_res.unwrap_or_default();
    let foreign_set = foreign_res.unwrap_or_default();
    let flatpak_updates = fp_upd_res.unwrap_or_default();

    updates_full.retain(|pkg_name, (_, new_v)| {
        all_installed_versions
            .get(pkg_name)
            .is_none_or(|inst_v| inst_v != new_v)
    });

    let updates_simple: HashMap<String, String> = updates_full
        .iter()
        .map(|(k, (_, new_v))| (k.clone(), new_v.clone()))
        .collect();

    let mut foreign_installed_versions: HashMap<String, String> = all_installed_versions
        .iter()
        .filter(|(name, _)| foreign_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let self_effective_ver = all_installed_versions
        .get(SELF_PACKAGE_NAME)
        .cloned()
        .unwrap_or_else(|| format!("{SELF_PACKAGE_VERSION}-1"));
    foreign_installed_versions.insert(SELF_PACKAGE_NAME.to_string(), self_effective_ver);

    // Interroge l'API AUR RPC v5 pour les paquets AUR installés (ou l'ensemble si force_sync)
    let aur_query_names: Vec<String> = snapshot
        .packages
        .iter()
        .filter(|pkg| {
            matches!(pkg.source, PackageSource::Aur) && (force_sync || pkg.is_installed)
        })
        .map(|pkg| pkg.name.clone())
        .collect();

    let mut enriched_aur_map: HashMap<String, PackageItem> = HashMap::new();
    if !aur_query_names.is_empty() {
        if let Ok(aur_client) = AurClient::new() {
            let refs: Vec<&str> = aur_query_names.iter().map(String::as_str).collect();
            match aur_client
                .fetch_enriched_aur_items(&refs, &foreign_installed_versions)
                .await
            {
                Ok(list) => {
                    for item in list {
                        enriched_aur_map.insert(item.name.clone(), item);
                    }
                }
                Err(e) => warnings.push(format!("Vérification AUR RPC v5 : {e}")),
            }
        }
    }

    // Si `checkupdates` signale une mise à jour sur une dépendance non présente dans `snapshot.packages`,
    // on ne charge via `pacman -Qi` que ce différentiel de paquets.
    let existing_names: HashSet<String> = snapshot
        .packages
        .iter()
        .filter(|p| matches!(p.source, PackageSource::Pacman | PackageSource::Aur))
        .map(|p| p.name.clone())
        .collect();

    let missing_upd_names: Vec<String> = updates_full
        .keys()
        .filter(|name| !existing_names.contains(*name))
        .cloned()
        .collect();

    if !missing_upd_names.is_empty() {
        if let Ok(mut delta_upd_items) = pacman::fetch_installed_packages_by_names(
            &missing_upd_names,
            &foreign_set,
            &updates_simple,
        )
        .await
        {
            snapshot.packages.append(&mut delta_upd_items);
        }
    }

    // Retire les éventuels paquets dépendances qui n'ont plus de mise à jour en attente
    snapshot.packages.retain(|pkg| {
        if matches!(pkg.source, PackageSource::Pacman | PackageSource::Aur) && pkg.is_installed {
            explicit_set.contains(&pkg.name)
                || updates_simple.contains_key(&pkg.name)
                || pkg.name == SELF_PACKAGE_NAME
        } else {
            true
        }
    });

    let mut installed_flatpak_map = HashMap::new();

    for pkg in &mut snapshot.packages {
        match pkg.source {
            PackageSource::Pacman => {
                if let Some(inst_v) = all_installed_versions.get(&pkg.name) {
                    pkg.version = inst_v.clone();
                }
                pkg.update_available = updates_simple.get(&pkg.name).cloned();
            }
            PackageSource::Aur => {
                if pkg.is_installed {
                    if let Some(inst_v) = all_installed_versions.get(&pkg.name) {
                        pkg.version = inst_v.clone();
                    }
                }
                if let Some(rich) = enriched_aur_map.remove(&pkg.name) {
                    pkg.aur_meta = rich.aur_meta;
                    pkg.description = rich.description;
                    if !pkg.is_installed {
                        pkg.version = rich.version;
                        pkg.update_available = None;
                    } else {
                        pkg.update_available = updates_simple
                            .get(&pkg.name)
                            .cloned()
                            .or(rich.update_available);
                    }
                } else if pkg.is_installed {
                    pkg.update_available = updates_simple.get(&pkg.name).cloned();
                }
            }
            PackageSource::Flatpak => {
                let app_id_opt = pkg.flatpak_app_id().map(ToString::to_string);
                if let Some(app_id) = app_id_opt {
                    if pkg.is_installed {
                        installed_flatpak_map.insert(app_id.clone(), pkg.version.clone());
                        pkg.update_available = flatpak_updates
                            .get(&app_id)
                            .filter(|new_v| *new_v != &pkg.version)
                            .cloned();
                    } else {
                        pkg.update_available = None;
                    }
                }
            }
            PackageSource::AppImage(_) => {
                pkg.update_available = appimage_updates.get(&pkg.id).cloned();
            }
        }
    }

    snapshot
        .packages
        .sort_by_key(|a| a.name.to_lowercase());

    cache::reconcile_pacman_recommendations(
        &mut snapshot.recommendations,
        &all_installed_versions,
        Some(&updates_simple),
    );
    cache::reconcile_flatpak_recommendations(
        &mut snapshot.flatpak_recommendations,
        &installed_flatpak_map,
        Some(&flatpak_updates),
    );

    snapshot.updates_count = snapshot
        .packages
        .iter()
        .filter(|pkg| pkg.update_available.is_some())
        .count();
    snapshot.non_fatal_warnings = warnings;

    cache::save_cached_snapshot(&snapshot);

    Ok(snapshot)
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
    let mut foreign_installed_versions: HashMap<String, String> = installed_versions
        .iter()
        .filter(|(name, _)| foreign_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let self_effective_ver = installed_versions
        .get(SELF_PACKAGE_NAME)
        .cloned()
        .unwrap_or_else(|| format!("{SELF_PACKAGE_VERSION}-1"));
    foreign_installed_versions.insert(SELF_PACKAGE_NAME.to_string(), self_effective_ver);

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
        if let Ok(pac_opt_map) = pacman::fetch_pacman_optdepends(&pacman_targets).await {
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

    let aur_client = AurClient::new().ok();

    if !aur_targets.is_empty() {
        if let Some(ref client) = aur_client {
            let refs: Vec<&str> = aur_targets.iter().map(String::as_str).collect();
            if let Ok(raw_pkgs) = client.fetch_raw_packages(&refs).await {
                for pkg in raw_pkgs {
                    for opt_raw in &pkg.opt_depends {
                        if let Some((dep_name, reason)) = pacman::parse_optdepend_spec(opt_raw) {
                            if explicit_names.contains(&dep_name) {
                                continue;
                            }
                            let entry =
                                dep_meta_map.entry(dep_name.clone()).or_insert_with(|| {
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
        pacman::list_all_installed_versions(),
        pacman::check_unsatisfied_dependencies(&ordered_dep_names)
    );
    let installed_versions = installed_res.unwrap_or_default();

    // Si toutes les dépendances optionnelles sont déjà satisfaites sur le système, inutile d'afficher la fenêtre
    let has_any_uninstalled = ordered_dep_names
        .iter()
        .any(|name| !installed_versions.contains_key(name) && unsatisfied_set.contains(name));
    if !has_any_uninstalled {
        return Ok(Vec::new());
    }

    let pacman_si_map = pacman::fetch_pacman_si_map(&ordered_dep_names, &installed_versions)
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
        } else if is_installed || pacman::can_resolve_in_pacman(&dep_name).await {
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
                        let unsatisfied =
                            pacman::check_unsatisfied_dependencies(std::slice::from_ref(&item.name))
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

/// Exécute la mise à jour globale de tous les paquets système (Pacman via `pkexec pacman -Syu`),
/// AUR (via `yay`) et Flatpak (`flatpak update`) avec suivi dans le Gestionnaire d'installation.
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

    if updated_ids
        .iter()
        .any(|id| id == "aur:noos-package-manager" || id == "pacman:noos-package-manager")
    {
        sync_self_installed_binaries();
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
