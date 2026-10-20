use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::Duration;

use reqwest::Client;
use serde::Deserialize;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::backend::models::{
    DependencyStatus, DependencyStepItem, InstallCancelHandle, PackageItem, PackageSource,
    RecommendationCategory,
};
use crate::error::{AppError, AppResult};
use crate::ui::messages::UiEvent;

const FLATHUB_API_BASE: &str = "https://flathub.org/api/v2";

#[derive(Debug, Clone)]
pub struct CachedFlathubEntry {
    pub app_id: String,
    pub name: String,
    pub version: String,
    pub branch: String,
    pub description: String,
}

#[derive(Debug, Clone, Default)]
pub struct FlatpakRemoteInfoMeta {
    pub app_id: String,
    pub version: String,
    pub license: String,
    pub download_size_bytes: u64,
    pub installed_size_bytes: u64,
    pub runtime: Option<String>,
    pub sdk: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FlathubCollectionResponse {
    #[serde(default)]
    hits: Vec<FlathubCollectionHit>,
}

#[derive(Debug, Deserialize)]
struct FlathubCollectionHit {
    #[serde(default)]
    app_id: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    developer_name: Option<String>,
}

/// Analyse la sortie tabulée de `flatpak list --app --columns=application,name,version,branch,origin,description`.
pub fn parse_flatpak_list_output(
    raw: &str,
    updates_map: &HashMap<String, String>,
) -> Vec<PackageItem> {
    let mut items = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cols: Vec<&str> = line.split('\t').collect();
        let Some(app_id_raw) = cols.first() else {
            continue;
        };
        let app_id = app_id_raw.trim();
        if app_id.is_empty() || app_id.eq_ignore_ascii_case("Application ID") {
            continue;
        }

        let name = cols
            .get(1)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(app_id);
        let version_col = cols.get(2).map(|s| s.trim()).unwrap_or("");
        let branch_col = cols.get(3).map(|s| s.trim()).unwrap_or("stable");
        let origin_col = cols.get(4).map(|s| s.trim()).unwrap_or("flathub");
        let desc_col = cols.get(5).map(|s| s.trim()).unwrap_or("");

        let version = if !version_col.is_empty() {
            version_col.to_string()
        } else if !branch_col.is_empty() {
            branch_col.to_string()
        } else {
            String::from("stable")
        };

        let description = if !desc_col.is_empty() {
            format!("{desc_col} ({app_id} • {origin_col})")
        } else {
            format!("Application Flatpak ({app_id} • {origin_col})")
        };

        let update_available = updates_map.get(app_id).cloned();

        items.push(PackageItem {
            id: format!("flatpak:{app_id}"),
            name: name.to_string(),
            version,
            description,
            source: PackageSource::Flatpak,
            is_installed: true,
            update_available,
            aur_meta: None,
        });
    }

    items.sort_by_key(|a| a.name.to_lowercase());
    items
}

/// Analyse la sortie tabulée de `flatpak remote-ls --updates --app --columns=application,version,branch`.
pub fn parse_flatpak_updates_output(raw: &str) -> HashMap<String, String> {
    let mut updates = HashMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cols: Vec<&str> = line.split('\t').collect();
        let Some(app_id_raw) = cols.first() else {
            continue;
        };
        let app_id = app_id_raw.trim();
        if app_id.is_empty() || app_id.eq_ignore_ascii_case("Application ID") {
            continue;
        }

        let version_col = cols.get(1).map(|s| s.trim()).unwrap_or("");
        let branch_col = cols.get(2).map(|s| s.trim()).unwrap_or("mise à jour");

        let new_ver = if !version_col.is_empty() {
            version_col.to_string()
        } else if !branch_col.is_empty() {
            branch_col.to_string()
        } else {
            String::from("mise à jour")
        };

        updates.insert(app_id.to_string(), new_ver);
    }

    updates
}

/// Analyse la sortie tabulée de `flatpak remote-ls flathub --app --cached --columns=application,name,version,branch,description`.
pub fn parse_flatpak_remote_ls_output(raw: &str) -> HashMap<String, CachedFlathubEntry> {
    let mut map = HashMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let cols: Vec<&str> = line.split('\t').collect();
        let Some(app_id_raw) = cols.first() else {
            continue;
        };
        let app_id = app_id_raw.trim();
        if app_id.is_empty() || !app_id.contains('.') {
            continue;
        }

        let name = cols
            .get(1)
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .unwrap_or(app_id)
            .to_string();
        let version_col = cols.get(2).map(|s| s.trim()).unwrap_or("");
        let branch_col = cols.get(3).map(|s| s.trim()).unwrap_or("stable");
        let desc_col = cols.get(4).map(|s| s.trim()).unwrap_or("");

        let version = if !version_col.is_empty() {
            version_col.to_string()
        } else if !branch_col.is_empty() {
            branch_col.to_string()
        } else {
            String::from("stable")
        };

        map.insert(
            app_id.to_string(),
            CachedFlathubEntry {
                app_id: app_id.to_string(),
                name,
                version,
                branch: branch_col.to_string(),
                description: desc_col.to_string(),
            },
        );
    }

    map
}

/// Analyse la sortie tabulée de `flatpak search --columns=name,description,application,version,branch,remotes`.
pub fn parse_flatpak_search_output(
    raw: &str,
    installed_map: &HashMap<String, String>,
    updates_map: &HashMap<String, String>,
) -> Vec<PackageItem> {
    let mut items = Vec::new();
    let mut seen = HashSet::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed.eq_ignore_ascii_case("No matches found")
            || trimmed.contains("Aucune correspondance")
        {
            continue;
        }

        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }

        let name = cols.first().map(|s| s.trim()).unwrap_or("");
        let desc = cols.get(1).map(|s| s.trim()).unwrap_or("");
        let app_id = cols.get(2).map(|s| s.trim()).unwrap_or("");
        if app_id.is_empty() || !app_id.contains('.') || !seen.insert(app_id.to_string()) {
            continue;
        }

        let version_col = cols.get(3).map(|s| s.trim()).unwrap_or("");
        let branch_col = cols.get(4).map(|s| s.trim()).unwrap_or("stable");
        let remotes_col = cols.get(5).map(|s| s.trim()).unwrap_or("flathub");

        let is_installed = installed_map.contains_key(app_id);
        let version = if let Some(inst_ver) = installed_map.get(app_id) {
            inst_ver.clone()
        } else if !version_col.is_empty() {
            version_col.to_string()
        } else if !branch_col.is_empty() {
            branch_col.to_string()
        } else {
            String::from("stable")
        };

        let display_name = if !name.is_empty() {
            name.to_string()
        } else {
            app_id.to_string()
        };

        let description = if !desc.is_empty() {
            format!("{desc} ({app_id} • {remotes_col})")
        } else {
            format!("Application Flatpak ({app_id} • {remotes_col})")
        };

        let update_available = if is_installed {
            updates_map.get(app_id).cloned()
        } else {
            None
        };

        items.push(PackageItem {
            id: format!("flatpak:{app_id}"),
            name: display_name,
            version,
            description,
            source: PackageSource::Flatpak,
            is_installed,
            update_available,
            aur_meta: None,
        });
    }

    items
}

/// Convertit une chaîne de taille Flatpak (ex: `"129.7 MB"`, `"129,7\u{a0}Mo"`, `"129.7?MB"`, `"450.2 kB"`) en octets.
pub fn parse_flatpak_size_to_bytes(raw_size: &str) -> u64 {
    let cleaned: String = raw_size
        .chars()
        .map(|c| {
            if c == ',' {
                '.'
            } else if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                ' '
            }
        })
        .collect();

    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    let Some(num_str) = parts.first() else {
        return 0;
    };
    let Ok(val) = num_str.parse::<f64>() else {
        return 0;
    };
    let unit = parts
        .get(1)
        .map(|u| u.to_uppercase())
        .unwrap_or_else(|| String::from("B"));

    let multiplier: f64 = if unit.starts_with('G') {
        1024.0 * 1024.0 * 1024.0
    } else if unit.starts_with('M') {
        1024.0 * 1024.0
    } else if unit.starts_with('K') {
        1024.0
    } else {
        1.0
    };

    (val * multiplier) as u64
}

/// Analyse la sortie de `LC_ALL=C flatpak remote-info flathub <app_id>`.
pub fn parse_flatpak_remote_info(app_id: &str, raw: &str) -> FlatpakRemoteInfoMeta {
    let mut meta = FlatpakRemoteInfoMeta {
        app_id: app_id.to_string(),
        version: String::from("stable"),
        license: String::from("Non spécifiée"),
        download_size_bytes: 0,
        installed_size_bytes: 0,
        runtime: None,
        sdk: None,
    };

    for line in raw.lines() {
        let Some((k, v)) = line.split_once(':') else {
            continue;
        };
        let key = k.trim();
        let val = v.trim();
        match key {
            "ID" if !val.is_empty() => meta.app_id = val.to_string(),
            "Version" if !val.is_empty() => meta.version = val.to_string(),
            "License" if !val.is_empty() => meta.license = val.to_string(),
            "Download Size" => meta.download_size_bytes = parse_flatpak_size_to_bytes(val),
            "Installed Size" => meta.installed_size_bytes = parse_flatpak_size_to_bytes(val),
            "Runtime" if !val.is_empty() => meta.runtime = Some(val.to_string()),
            "Sdk" if !val.is_empty() => meta.sdk = Some(val.to_string()),
            _ => {}
        }
    }

    meta
}

/// Liste toutes les applications Flatpak installées sur le système.
pub async fn list_installed_flatpaks(
    updates_map: &HashMap<String, String>,
) -> AppResult<Vec<PackageItem>> {
    let output = Command::new("flatpak")
        .args([
            "list",
            "--app",
            "--columns=application,name,version,branch,origin,description",
        ])
        .output()
        .await;

    let Ok(output) = output else {
        return Ok(Vec::new());
    };

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(parse_flatpak_list_output(&stdout, updates_map))
}

/// Liste les runtimes Flatpak déjà installés afin d'éviter de les compter comme manquants.
pub async fn list_installed_flatpak_runtimes() -> HashSet<String> {
    let mut set = HashSet::new();
    let Ok(output) = Command::new("flatpak")
        .args(["list", "--runtime", "--columns=application,arch,branch,ref"])
        .output()
        .await
    else {
        return set;
    };

    if output.status.success() {
        if let Ok(stdout) = String::from_utf8(output.stdout) {
            for line in stdout.lines() {
                let cols: Vec<&str> = line.split('\t').map(str::trim).collect();
                if let Some(app) = cols.first() {
                    if !app.is_empty() {
                        set.insert((*app).to_string());
                    }
                    if let (Some(arch), Some(branch)) = (cols.get(1), cols.get(2)) {
                        set.insert(format!("{app}/{arch}/{branch}"));
                        set.insert(format!("{app}/{branch}"));
                    }
                }
                if let Some(full_ref) = cols.get(3) {
                    if !full_ref.is_empty() {
                        set.insert((*full_ref).to_string());
                    }
                }
            }
        }
    }

    set
}

/// Détecte les mises à jour disponibles pour les applications Flatpak installées (`flatpak remote-ls --updates`).
pub async fn check_flatpak_updates_map() -> AppResult<HashMap<String, String>> {
    let output = Command::new("flatpak")
        .args([
            "remote-ls",
            "--updates",
            "--app",
            "--columns=application,version,branch",
        ])
        .output()
        .await;

    let Ok(output) = output else {
        return Ok(HashMap::new());
    };

    if !output.status.success() {
        return Ok(HashMap::new());
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(parse_flatpak_updates_output(&stdout))
}

/// Charge le catalogue Flathub local mis en cache (`flatpak remote-ls flathub --app --cached`).
pub async fn load_cached_flathub_catalog() -> HashMap<String, CachedFlathubEntry> {
    let output = Command::new("flatpak")
        .args([
            "remote-ls",
            "flathub",
            "--app",
            "--cached",
            "--columns=application,name,version,branch,description",
        ])
        .output()
        .await;

    let Ok(output) = output else {
        return HashMap::new();
    };

    if !output.status.success() {
        return HashMap::new();
    }

    let Ok(stdout) = String::from_utf8(output.stdout) else {
        return HashMap::new();
    };

    parse_flatpak_remote_ls_output(&stdout)
}

/// Charge les recommandations Flathub groupées par `RecommendationCategory`, en combinant
/// le catalogue local Flathub et l'API officielle Flathub v2 (`https://flathub.org/api/v2/collection/category/...`).
pub async fn load_flathub_recommendations_by_category(
    installed_map: &HashMap<String, String>,
    updates_map: &HashMap<String, String>,
) -> AppResult<Vec<(RecommendationCategory, Vec<PackageItem>)>> {
    let cached_catalog = load_cached_flathub_catalog().await;

    let http_client = Client::builder()
        .user_agent("noos-package-manager/0.1.0 (Arch Linux; GTK4/Libadwaita)")
        .timeout(Duration::from_secs(5))
        .connect_timeout(Duration::from_secs(3))
        .build()
        .ok();

    let mut groups = Vec::with_capacity(RecommendationCategory::ALL.len());

    for category in RecommendationCategory::ALL {
        let mut category_items = Vec::new();
        let mut seen_ids = HashSet::new();

        // 1. Récupération optionnelle depuis l'API Flathub v2 pour enrichir la catégorie
        let mut api_hits: HashMap<String, FlathubCollectionHit> = HashMap::new();
        let mut api_ordered_ids = Vec::new();
        if let Some(ref client) = http_client {
            let url = format!(
                "{FLATHUB_API_BASE}/collection/category/{}?page=1&per_page=12",
                category.flathub_api_category()
            );
            if let Ok(resp) = client.get(&url).send().await {
                if let Ok(parsed) = resp.json::<FlathubCollectionResponse>().await {
                    for hit in parsed.hits {
                        if !hit.app_id.is_empty() {
                            api_ordered_ids.push(hit.app_id.clone());
                            api_hits.insert(hit.app_id.clone(), hit);
                        }
                    }
                }
            }
        }

        // 2. Construction de la liste ordonnée : d'abord notre sélection phare Flathub, puis le top API Flathub
        let mut candidate_app_ids: Vec<String> = category
            .flathub_curated_app_ids()
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        for api_id in api_ordered_ids {
            if candidate_app_ids.len() >= 14 {
                break;
            }
            if !candidate_app_ids.contains(&api_id) {
                candidate_app_ids.push(api_id);
            }
        }

        for app_id in candidate_app_ids {
            if !seen_ids.insert(app_id.clone()) {
                continue;
            }

            let cached = cached_catalog.get(&app_id);
            let api_hit = api_hits.get(&app_id);

            if cached.is_none() && api_hit.is_none() && !installed_map.contains_key(&app_id) {
                continue;
            }

            let name = api_hit
                .map(|h| h.name.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| cached.map(|c| c.name.as_str()))
                .unwrap_or(&app_id)
                .to_string();

            let is_installed = installed_map.contains_key(&app_id);
            let version = if let Some(inst_ver) = installed_map.get(&app_id) {
                inst_ver.clone()
            } else if let Some(c) = cached {
                c.version.clone()
            } else {
                String::from("Flathub")
            };

            let summary = api_hit
                .map(|h| h.summary.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| cached.map(|c| c.description.as_str()))
                .unwrap_or("Application Flatpak distribuée sur Flathub");

            let dev_suffix = api_hit
                .and_then(|h| h.developer_name.as_deref())
                .filter(|d| !d.is_empty())
                .map(|d| format!("par {d} • {app_id}"))
                .unwrap_or_else(|| format!("{app_id} • Flathub"));

            let description = format!("{summary} ({dev_suffix})");
            let update_available = if is_installed {
                updates_map.get(&app_id).cloned()
            } else {
                None
            };

            category_items.push(PackageItem {
                id: format!("flatpak:{app_id}"),
                name,
                version,
                description,
                source: PackageSource::Flatpak,
                is_installed,
                update_available,
                aur_meta: None,
            });
        }

        // Afficher les paquets non installés en premier dans chaque catégorie de recommandation
        category_items.sort_by_key(|item| item.is_installed);

        if !category_items.is_empty() {
            groups.push((category, category_items));
        }
    }

    Ok(groups)
}

/// Recherche des applications Flatpak via `flatpak search`.
pub async fn search_flatpak(
    query: &str,
    installed_map: &HashMap<String, String>,
    updates_map: &HashMap<String, String>,
) -> AppResult<Vec<PackageItem>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let output = Command::new("flatpak")
        .args([
            "search",
            "--columns=name,description,application,version,branch,remotes",
            trimmed,
        ])
        .output()
        .await?;

    if !output.status.success() {
        return Ok(Vec::new());
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(parse_flatpak_search_output(
        &stdout,
        installed_map,
        updates_map,
    ))
}

/// Résout les moteurs d'exécution (Runtimes) et tailles de téléchargement pour un lot d'applications Flatpak.
pub async fn resolve_flatpak_dependencies(
    app_ids: &[String],
) -> AppResult<Vec<DependencyStepItem>> {
    if app_ids.is_empty() {
        return Ok(Vec::new());
    }

    let installed_runtimes = list_installed_flatpak_runtimes().await;
    let mut steps = Vec::new();
    let mut seen_runtimes = HashSet::new();

    for app_id in app_ids {
        let output = Command::new("flatpak")
            .env("LC_ALL", "C")
            .args(["remote-info", "flathub", app_id])
            .output()
            .await;

        if let Ok(out) = output {
            if out.status.success() {
                if let Ok(stdout) = String::from_utf8(out.stdout) {
                    let info = parse_flatpak_remote_info(app_id, &stdout);

                    if let Some(ref runtime_ref) = info.runtime {
                        let runtime_name = runtime_ref
                            .split('/')
                            .next()
                            .unwrap_or(runtime_ref)
                            .trim()
                            .to_string();
                        let runtime_branch = runtime_ref
                            .split('/')
                            .nth(2)
                            .unwrap_or("stable")
                            .trim()
                            .to_string();

                        if !installed_runtimes.contains(runtime_ref)
                            && !installed_runtimes.contains(&runtime_name)
                            && seen_runtimes.insert(runtime_ref.clone())
                        {
                            steps.push(DependencyStepItem {
                                name: runtime_name,
                                version: runtime_branch,
                                size_bytes: 0,
                                is_explicit_target: false,
                                source: PackageSource::Flatpak,
                                status: DependencyStatus::Pending,
                            });
                        }
                    }

                    steps.push(DependencyStepItem {
                        name: app_id.clone(),
                        version: info.version,
                        size_bytes: info.download_size_bytes,
                        is_explicit_target: true,
                        source: PackageSource::Flatpak,
                        status: DependencyStatus::Pending,
                    });
                    continue;
                }
            }
        }

        steps.push(DependencyStepItem {
            name: app_id.clone(),
            version: String::from("stable"),
            size_bytes: 0,
            is_explicit_target: true,
            source: PackageSource::Flatpak,
            status: DependencyStatus::Pending,
        });
    }

    Ok(steps)
}

/// Analyse une ligne de sortie de `flatpak install` ou `flatpak update` pour extraire l'étape active.
pub fn parse_flatpak_progress_line(
    line: &str,
    known_deps: &[String],
) -> Option<(String, Option<(String, DependencyStatus)>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lower = trimmed.to_lowercase();

    if lower.starts_with("installing ") || lower.starts_with("updating ") {
        for dep in known_deps {
            if trimmed.contains(dep.as_str()) {
                return Some((
                    format!("Installation Flatpak de « {dep} »…"),
                    Some((dep.clone(), DependencyStatus::Installing)),
                ));
            }
        }
        return Some((format!("Flatpak : {trimmed}"), None));
    }

    for dep in known_deps {
        if trimmed.contains(dep.as_str()) {
            let status = if lower.contains("installing") || lower.contains("deploying") {
                DependencyStatus::Installing
            } else {
                DependencyStatus::Downloading
            };
            let phase = match status {
                DependencyStatus::Installing => format!("Déploiement Flatpak de « {dep} »…"),
                _ => format!("Téléchargement Flatpak de « {dep} »…"),
            };
            return Some((phase, Some((dep.clone(), status))));
        }
    }

    if lower.contains("installation complete") || lower.contains("updates complete") {
        return Some((String::from("Opération Flatpak terminée."), None));
    }

    None
}

/// Installe un lot d'applications Flatpak depuis Flathub avec suivi temps réel et support d'annulation.
pub async fn install_flatpak_packages_streaming(
    app_ids: &[String],
    known_deps: &[String],
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> AppResult<String> {
    if app_ids.is_empty() {
        return Ok(String::from("Aucune application Flatpak à installer."));
    }
    if cancel_handle.is_cancelled() {
        return Err(AppError::FlatpakError(String::from(
            "Installation Flatpak annulée par l'utilisateur.",
        )));
    }

    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: format!(
                "Téléchargement et installation de {} application(s) Flatpak depuis Flathub…",
                app_ids.len()
            ),
            fraction: 0.10,
            active_package: app_ids
                .first()
                .map(|n| (n.clone(), DependencyStatus::Downloading)),
            log_line: Some(format!(
                "$ flatpak install --noninteractive -y --or-update flathub {}",
                app_ids.join(" ")
            )),
        })
        .await;

    let mut child = Command::new("flatpak")
        .env("LC_ALL", "C")
        .args(["install", "--noninteractive", "-y", "--or-update", "flathub"])
        .args(app_ids)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let total_steps = (known_deps.len().max(1) * 2) as f64;
    let mut completed_steps = 0.0_f64;

    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout).lines();
        loop {
            let next_line_opt = tokio::select! {
                _ = cancel_handle.notified() => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    let _ = ui_tx
                        .send(UiEvent::InstallProgress {
                            phase: String::from("Installation Flatpak annulée par l'utilisateur."),
                            fraction: -1.0,
                            active_package: None,
                            log_line: Some(String::from("[Annulé] Processus Flatpak interrompu à la demande de l'utilisateur.")),
                        })
                        .await;
                    return Err(AppError::FlatpakError(String::from(
                        "Installation annulée par l'utilisateur.",
                    )));
                }
                res = reader.next_line() => res.ok().flatten(),
            };

            let Some(line) = next_line_opt else {
                break;
            };

            for segment in line.split('\r') {
                let clean = segment.trim();
                if clean.is_empty() {
                    continue;
                }
                if let Some((phase, active_pkg)) = parse_flatpak_progress_line(clean, known_deps) {
                    if active_pkg.is_some() {
                        completed_steps = (completed_steps + 1.0).min(total_steps);
                    }
                    let frac = (0.12 + 0.82 * (completed_steps / total_steps)).clamp(0.12, 0.95);
                    let _ = ui_tx
                        .send(UiEvent::InstallProgress {
                            phase,
                            fraction: frac,
                            active_package: active_pkg,
                            log_line: Some(clean.to_string()),
                        })
                        .await;
                } else {
                    let _ = ui_tx
                        .send(UiEvent::InstallProgress {
                            phase: String::new(),
                            fraction: -1.0,
                            active_package: None,
                            log_line: Some(clean.to_string()),
                        })
                        .await;
                }
            }
        }
    }

    let mut stderr_buf = String::new();
    if let Some(stderr) = child.stderr.take() {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(err_line)) = reader.next_line().await {
            if !stderr_buf.is_empty() {
                stderr_buf.push('\n');
            }
            stderr_buf.push_str(&err_line);
            let _ = ui_tx
                .send(UiEvent::InstallProgress {
                    phase: String::new(),
                    fraction: -1.0,
                    active_package: None,
                    log_line: Some(format!("[stderr] {err_line}")),
                })
                .await;
        }
    }

    let status = tokio::select! {
        _ = cancel_handle.notified() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(AppError::FlatpakError(String::from(
                "Installation annulée par l'utilisateur.",
            )));
        }
        st = child.wait() => st?,
    };

    if status.success() {
        Ok(format!(
            "{} application(s) Flatpak installée(s) avec succès depuis Flathub.",
            app_ids.len()
        ))
    } else {
        let detail = if stderr_buf.trim().is_empty() {
            format!("Code de sortie {}", status.code().unwrap_or(1))
        } else {
            stderr_buf.trim().to_string()
        };
        Err(AppError::FlatpakError(detail))
    }
}

/// Met à jour toutes les applications et runtimes Flatpak via `flatpak update --noninteractive -y`.
pub async fn update_flatpak_all_streaming(
    known_deps: &[String],
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> AppResult<String> {
    if cancel_handle.is_cancelled() {
        return Err(AppError::FlatpakError(String::from(
            "Mise à jour Flatpak annulée par l'utilisateur.",
        )));
    }

    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: String::from("Mise à jour des applications Flatpak (`flatpak update`)…"),
            fraction: 0.78,
            active_package: known_deps
                .first()
                .map(|n| (n.clone(), DependencyStatus::Downloading)),
            log_line: Some(String::from("$ flatpak update --noninteractive -y")),
        })
        .await;

    let mut child = Command::new("flatpak")
        .env("LC_ALL", "C")
        .args(["update", "--noninteractive", "-y"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let total_steps = (known_deps.len().max(1) * 2) as f64;
    let mut completed_steps = 0.0_f64;

    if let Some(stdout) = child.stdout.take() {
        let mut reader = BufReader::new(stdout).lines();
        loop {
            let next_line_opt = tokio::select! {
                _ = cancel_handle.notified() => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(AppError::FlatpakError(String::from(
                        "Mise à jour Flatpak annulée par l'utilisateur.",
                    )));
                }
                res = reader.next_line() => res.ok().flatten(),
            };

            let Some(line) = next_line_opt else {
                break;
            };

            for segment in line.split('\r') {
                let clean = segment.trim();
                if clean.is_empty() {
                    continue;
                }
                if let Some((phase, active_pkg)) = parse_flatpak_progress_line(clean, known_deps) {
                    if active_pkg.is_some() {
                        completed_steps = (completed_steps + 1.0).min(total_steps);
                    }
                    let frac = (0.80 + 0.18 * (completed_steps / total_steps)).clamp(0.80, 0.98);
                    let _ = ui_tx
                        .send(UiEvent::InstallProgress {
                            phase,
                            fraction: frac,
                            active_package: active_pkg,
                            log_line: Some(clean.to_string()),
                        })
                        .await;
                } else {
                    let _ = ui_tx
                        .send(UiEvent::InstallProgress {
                            phase: String::new(),
                            fraction: -1.0,
                            active_package: None,
                            log_line: Some(clean.to_string()),
                        })
                        .await;
                }
            }
        }
    }

    let mut stderr_buf = String::new();
    if let Some(stderr) = child.stderr.take() {
        let mut reader = BufReader::new(stderr).lines();
        while let Ok(Some(err_line)) = reader.next_line().await {
            if !stderr_buf.is_empty() {
                stderr_buf.push('\n');
            }
            stderr_buf.push_str(&err_line);
            let _ = ui_tx
                .send(UiEvent::InstallProgress {
                    phase: String::new(),
                    fraction: -1.0,
                    active_package: None,
                    log_line: Some(format!("[stderr] {err_line}")),
                })
                .await;
        }
    }

    let status = tokio::select! {
        _ = cancel_handle.notified() => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            return Err(AppError::FlatpakError(String::from(
                "Mise à jour Flatpak annulée par l'utilisateur.",
            )));
        }
        st = child.wait() => st?,
    };

    if status.success() {
        Ok(String::from("Mises à jour Flatpak terminées avec succès."))
    } else {
        let detail = if stderr_buf.trim().is_empty() {
            format!("Code de sortie {}", status.code().unwrap_or(1))
        } else {
            stderr_buf.trim().to_string()
        };
        Err(AppError::FlatpakError(detail))
    }
}

/// Désinstalle une application Flatpak (`flatpak uninstall --noninteractive -y <app_id>`).
pub async fn remove_flatpak_package(app_id: &str) -> AppResult<String> {
    let output = Command::new("flatpak")
        .env("LC_ALL", "C")
        .args(["uninstall", "--noninteractive", "-y", app_id])
        .output()
        .await?;

    if output.status.success() {
        Ok(format!(
            "Application Flatpak « {app_id} » désinstallée avec succès."
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(AppError::FlatpakError(format!(
            "Échec de la désinstallation de « {app_id} » : {detail}"
        )))
    }
}

/// Lance une application Flatpak installée en arrière-plan (`flatpak run <app_id>`).
pub fn launch_flatpak_app(app_id: &str) -> AppResult<String> {
    std::process::Command::new("flatpak")
        .arg("run")
        .arg(app_id)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| {
            AppError::FlatpakError(format!("Impossible de lancer « {app_id} » : {e}"))
        })?;

    Ok(format!("Application Flatpak « {app_id} » lancée."))
}
