use std::collections::{HashMap, HashSet};

use crate::backend::aur;
use crate::backend::appimage;
use crate::backend::cache;
use crate::backend::flatpak;
use crate::backend::logging::log_warn;
use crate::backend::pacman;
use crate::backend::models::{
    AurHelper, PackageItem, PackageSource, RecommendationCategory,
};
use crate::error::AppResult;

pub const SELF_PACKAGE_NAME: &str = "noos-package-manager";
pub const SELF_PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// État complet du catalogue retourné par le chargement initial ou l'actualisation.
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

    // Initialise le client HTTP AUR partagé (une seule fois pour toute la durée du processus).
    if let Err(e) = crate::backend::aur::init_shared_aur_client() {
        log_warn("aur", format!("Impossible d'initialiser le client AUR partagé : {e}"));
    }

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
        if let Some(aur_client) = aur::shared_aur_client() {
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
                    let mut seen_aur_names = already_in_catalog.clone();
                    for (_, extra_aur_pkg) in enriched_map {
                        if seen_aur_names.insert(extra_aur_pkg.name.clone()) {
                            installed_packages.push(extra_aur_pkg);
                        }
                    }

                    let discovery_pool = aur_client
                        .fetch_initial_discovery_pool(&foreign_installed_versions)
                        .await;
                    for mut disc_pkg in discovery_pool {
                        if !all_installed_versions.contains_key(&disc_pkg.name)
                            && seen_aur_names.insert(disc_pkg.name.clone())
                        {
                            disc_pkg.is_installed = false;
                            disc_pkg.update_available = None;
                            installed_packages.push(disc_pkg);
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
        if let Some(aur_client) = aur::shared_aur_client() {
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
    let non_installed_aur_count = snapshot
        .packages
        .iter()
        .filter(|pkg| matches!(pkg.source, PackageSource::Aur) && !pkg.is_installed)
        .count();

    if let Some(aur_client) = aur::shared_aur_client() {
        if !aur_query_names.is_empty() {
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

        if non_installed_aur_count < 250 {
            let mut known_names: HashSet<String> =
                snapshot.packages.iter().map(|p| p.name.clone()).collect();
            let discovery_pool = aur_client
                .fetch_initial_discovery_pool(&foreign_installed_versions)
                .await;
            for mut disc_pkg in discovery_pool {
                if !all_installed_versions.contains_key(&disc_pkg.name)
                    && known_names.insert(disc_pkg.name.clone())
                {
                    disc_pkg.is_installed = false;
                    disc_pkg.update_available = None;
                    snapshot.packages.push(disc_pkg);
                }
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
