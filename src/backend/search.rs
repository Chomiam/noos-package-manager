use std::collections::HashMap;

use crate::backend::aur::shared_aur_client;
use crate::backend::models::PackageItem;
use crate::error::{AppError, AppResult};

/// Effectue une recherche combinée asynchrone sur Pacman (`pacman -Ss`), AUR RPC v5 (`search`)
/// et les fichiers AppImage locaux.
///
/// Retourne les résultats triés par pertinence (nom exact > préfixe > mot commençant par la requête
/// > nom contient > id contient > description contient) ainsi que la liste des avertissements non fatals.
pub async fn search_all_sources(query: &str) -> AppResult<(Vec<PackageItem>, Vec<String>)> {
    let trimmed = query.trim().to_string();
    if trimmed.is_empty() {
        let snapshot = crate::backend::catalog::load_system_catalog().await?;
        return Ok((snapshot.packages, snapshot.non_fatal_warnings));
    }

    let mut warnings = Vec::new();

    let (pacman_upd_res, aur_upd_res) =
        tokio::join!(crate::backend::pacman::check_updates_map(), crate::backend::aur::check_aur_updates_via_helper());
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

    let installed_versions = crate::backend::pacman::list_all_installed_versions()
        .await
        .unwrap_or_default();
    let foreign_set = crate::backend::pacman::list_foreign_package_names()
        .await
        .unwrap_or_default();
    let mut foreign_installed_versions: HashMap<String, String> = installed_versions
        .iter()
        .filter(|(name, _)| foreign_set.contains(*name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let self_effective_ver = installed_versions
        .get(crate::backend::catalog::SELF_PACKAGE_NAME)
        .cloned()
        .unwrap_or_else(|| format!("{}-1", crate::backend::catalog::SELF_PACKAGE_VERSION));
    foreign_installed_versions.insert(
        crate::backend::catalog::SELF_PACKAGE_NAME.to_string(),
        self_effective_ver,
    );

    let pacman_fut = crate::backend::pacman::search_packages(&trimmed, &updates_simple);
    let aur_fut = async {
        let client = shared_aur_client()
            .ok_or_else(|| {
                AppError::AurHelperError(String::from("Client AUR partagé non initialisé"))
            })?;
        client.search(&trimmed, &foreign_installed_versions).await
    };
    let flatpak_fut = async {
        let fp_updates = crate::backend::flatpak::check_flatpak_updates_map()
            .await
            .unwrap_or_default();
        let mut fp_installed_map = HashMap::new();
        if let Ok(fp_list) = crate::backend::flatpak::list_installed_flatpaks(&fp_updates).await {
            for item in fp_list {
                if let Some(app_id) = item.flatpak_app_id() {
                    fp_installed_map.insert(app_id.to_string(), item.version);
                }
            }
        }
        crate::backend::flatpak::search_flatpak(&trimmed, &fp_installed_map, &fp_updates).await
    };
    let query_lower = trimmed.to_lowercase();
    let appimage_fut = tokio::task::spawn_blocking(move || -> AppResult<Vec<PackageItem>> {
        let all_appimages = crate::backend::appimage::scan_default_directories()?;
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

    // Tri par pertinence multi-critères (nom exact > préfixe du nom > mot commençant par la requête
    // > nom contient > id contient > description contient)
    let q_low = trimmed.to_lowercase();
    results.sort_by(|a, b| a.compare_search_relevance(b, &q_low));

    Ok((results, warnings))
}
