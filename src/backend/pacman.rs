use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::backend::models::{
    DependencyStatus, DependencyStepItem, InstallCancelHandle, PackageItem, PackageSource,
    RecommendationCategory,
};
use crate::error::{AppError, AppResult};
use crate::ui::messages::UiEvent;

/// Parse la sortie brute de `pacman -Qi` (en locale C) en une liste de `PackageItem`.
pub fn parse_pacman_qi_output(
    raw: &str,
    foreign_packages: &HashSet<String>,
    updates_map: &HashMap<String, String>,
) -> Vec<PackageItem> {
    let mut items = Vec::new();

    for block in raw.split("\n\n") {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut name = String::new();
        let mut version = String::new();
        let mut description = String::new();

        for line in trimmed.lines() {
            if let Some((key, val)) = line.split_once(':') {
                let k = key.trim();
                let v = val.trim();
                match k {
                    "Name" => name = v.to_string(),
                    "Version" => version = v.to_string(),
                    "Description" => description = v.to_string(),
                    _ => {}
                }
            }
        }

        if !name.is_empty() {
            let is_foreign = foreign_packages.contains(&name);
            let source = if is_foreign {
                PackageSource::Aur
            } else {
                PackageSource::Pacman
            };
            let id_prefix = if is_foreign { "aur" } else { "pacman" };
            let update_available = updates_map.get(&name).cloned();

            items.push(PackageItem {
                id: format!("{id_prefix}:{name}"),
                name,
                version,
                description: if description.is_empty() {
                    String::from("Paquet système installé")
                } else {
                    description
                },
                source,
                is_installed: true,
                update_available,
                aur_meta: None,
            });
        }
    }

    items
}

/// Parse la sortie brute de `pacman -Si` (en locale C) en une table `nom -> PackageItem`.
pub fn parse_pacman_si_output(
    raw: &str,
    installed_versions: &HashMap<String, String>,
    updates_map: &HashMap<String, String>,
) -> HashMap<String, PackageItem> {
    let mut map = HashMap::new();

    for block in raw.split("\n\n") {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut name = String::new();
        let mut version = String::new();
        let mut description = String::new();

        for line in trimmed.lines() {
            if let Some((key, val)) = line.split_once(':') {
                let k = key.trim();
                let v = val.trim();
                match k {
                    "Name" => name = v.to_string(),
                    "Version" => version = v.to_string(),
                    "Description" => description = v.to_string(),
                    _ => {}
                }
            }
        }

        if !name.is_empty() {
            let installed_ver = installed_versions.get(&name);
            let is_installed = installed_ver.is_some();
            let display_version = installed_ver.cloned().unwrap_or(version);
            let update_available = updates_map.get(&name).cloned();

            let item = PackageItem {
                id: format!("pacman:{name}"),
                name: name.clone(),
                version: display_version,
                description,
                source: PackageSource::Pacman,
                is_installed,
                update_available,
                aur_meta: None,
            };

            map.insert(name, item);
        }
    }

    map
}

/// Parse une spécification de dépendance optionnelle (`Optional Deps` ou `OptDepends`),
/// par exemple `"rocm-smi-lib: AMD GPU support"` ou `"python>=3.10: scripting"`,
/// en renvoyant `(nom_du_paquet, description_optionnelle)`.
pub fn parse_optdepend_spec(raw: &str) -> Option<(String, String)> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "None" {
        return None;
    }

    let (pkg_part, reason_part) = match trimmed.split_once(':') {
        Some((left, right)) => (left.trim(), right.trim()),
        None => (trimmed, ""),
    };

    let clean_name = pkg_part
        .split(['>', '<', '='])
        .next()
        .unwrap_or(pkg_part)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .trim();

    if clean_name.is_empty() {
        return None;
    }

    let clean_reason = reason_part
        .strip_suffix("[installed]")
        .or_else(|| reason_part.strip_suffix("[pending]"))
        .unwrap_or(reason_part)
        .trim()
        .to_string();

    Some((clean_name.to_string(), clean_reason))
}

/// Parse la sortie de `pacman -Si` (en locale C) pour extraire la liste des dépendances optionnelles
/// (`Optional Deps`) de chaque paquet : `nom_du_paquet -> Vec<(nom_opt_dep, raison)>`.
pub fn parse_pacman_si_optdepends(raw: &str) -> HashMap<String, Vec<(String, String)>> {
    let mut map = HashMap::new();

    for block in raw.split("\n\n") {
        let trimmed = block.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut pkg_name = String::new();
        let mut opt_deps = Vec::new();
        let mut in_opt_deps = false;

        for line in trimmed.lines() {
            if (line.starts_with(' ') || line.starts_with('\t')) && in_opt_deps {
                if let Some(parsed) = parse_optdepend_spec(line) {
                    opt_deps.push(parsed);
                }
                continue;
            }

            if let Some((key, val)) = line.split_once(':') {
                let k = key.trim();
                let v = val.trim();
                in_opt_deps = false;

                match k {
                    "Name" => pkg_name = v.to_string(),
                    "Optional Deps" if !v.is_empty() && v != "None" => {
                        if let Some(parsed) = parse_optdepend_spec(v) {
                            opt_deps.push(parsed);
                        }
                        in_opt_deps = true;
                    }
                    _ => {}
                }
            }
        }

        if !pkg_name.is_empty() && !opt_deps.is_empty() {
            map.insert(pkg_name, opt_deps);
        }
    }

    map
}

/// Récupère via `pacman -Si` les dépendances optionnelles déclarées pour un lot de paquets Pacman.
pub async fn fetch_pacman_optdepends(
    targets: &[String],
) -> AppResult<HashMap<String, Vec<(String, String)>>> {
    if targets.is_empty() {
        return Ok(HashMap::new());
    }

    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Si")
        .args(targets)
        .output()
        .await?;

    let stdout = String::from_utf8(output.stdout)?;
    Ok(parse_pacman_si_optdepends(&stdout))
}

/// Récupère les fiches `PackageItem` d'un lot de noms de paquets dans les dépôts officiels via `pacman -Si`.
pub async fn fetch_pacman_si_map(
    names: &[String],
    installed_versions: &HashMap<String, String>,
) -> AppResult<HashMap<String, PackageItem>> {
    if names.is_empty() {
        return Ok(HashMap::new());
    }

    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Si")
        .args(names)
        .output()
        .await?;

    let stdout = String::from_utf8(output.stdout)?;
    Ok(parse_pacman_si_output(
        &stdout,
        installed_versions,
        &HashMap::new(),
    ))
}

/// Vérifie via `pacman -T` quelles dépendances parmi `names` ne sont pas encore satisfaites sur le système
/// (gère aussi bien les noms de paquets exacts que les provisions virtuelles comme `org.freedesktop.secrets`).
pub async fn check_unsatisfied_dependencies(names: &[String]) -> HashSet<String> {
    if names.is_empty() {
        return HashSet::new();
    }

    let Ok(output) = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-T")
        .args(names)
        .output()
        .await
    else {
        return names.iter().cloned().collect();
    };

    if output.status.success() {
        return HashSet::new();
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|line| {
            line.split(['>', '<', '='])
                .next()
                .map(|s| s.trim().to_string())
        })
        .collect()
}

/// Vérifie si une dépendance ou provision virtuelle est résoluble par Pacman (`pacman -Sp`).
pub async fn can_resolve_in_pacman(pkg_or_provision: &str) -> bool {
    Command::new("pacman")
        .env("LC_ALL", "C")
        .args(["-Sp", "--print-format", "%n", pkg_or_provision])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Charge les paquets Pacman recommandés organisés par catégories thématiques via `pacman -Si`.
/// Au sein de chaque catégorie, les paquets non installés sont placés en premier afin de ne pas
/// mettre les paquets déjà installés en tête de liste.
pub async fn load_recommended_by_category(
    installed_versions: &HashMap<String, String>,
    updates_map: &HashMap<String, String>,
) -> AppResult<Vec<(RecommendationCategory, Vec<PackageItem>)>> {
    let mut all_names = Vec::new();
    let mut seen = HashSet::new();

    for cat in RecommendationCategory::ALL {
        for &pkg_name in cat.package_names() {
            if seen.insert(pkg_name) {
                all_names.push(pkg_name);
            }
        }
    }

    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Si")
        .args(&all_names)
        .output()
        .await?;

    let stdout = String::from_utf8(output.stdout)?;
    let parsed_map = parse_pacman_si_output(&stdout, installed_versions, updates_map);

    let mut categorized = Vec::with_capacity(RecommendationCategory::ALL.len());
    for cat in RecommendationCategory::ALL {
        let mut cat_items: Vec<PackageItem> = cat
            .package_names()
            .iter()
            .filter_map(|&name| parsed_map.get(name).cloned())
            .collect();

        // Les paquets non installés apparaissent en premier dans les recommandations
        cat_items.sort_by_key(|item| item.is_installed);

        if !cat_items.is_empty() {
            categorized.push((cat, cat_items));
        }
    }

    Ok(categorized)
}

/// Parse la sortie de `checkupdates` ou `pacman -Qu` (`nom ancienne_version -> nouvelle_version`).
pub fn parse_checkupdates_output(raw: &str) -> HashMap<String, (String, String)> {
    let mut updates = HashMap::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 4 && parts[2] == "->" {
            let pkg_name = parts[0].to_string();
            let old_ver = parts[1].to_string();
            let new_ver = parts[3].to_string();
            updates.insert(pkg_name, (old_ver, new_ver));
        } else if parts.len() == 2 {
            let pkg_name = parts[0].to_string();
            let new_ver = parts[1].to_string();
            updates.insert(pkg_name, (String::new(), new_ver));
        }
    }

    updates
}

/// Parse la sortie de `pacman -Ss <terme>` (en locale C).
pub fn parse_pacman_ss_output(
    raw: &str,
    updates_map: &HashMap<String, String>,
) -> Vec<PackageItem> {
    let mut items = Vec::new();
    let mut lines = raw.lines().peekable();

    while let Some(header_line) = lines.next() {
        if header_line.starts_with(' ')
            || header_line.starts_with('\t')
            || header_line.trim().is_empty()
        {
            continue;
        }

        let description = match lines.peek() {
            Some(next_line) if next_line.starts_with(' ') || next_line.starts_with('\t') => {
                let desc = next_line.trim().to_string();
                let _ = lines.next();
                desc
            }
            _ => String::from("Aucune description disponible"),
        };

        let mut tokens = header_line.split_whitespace();
        let Some(repo_and_name) = tokens.next() else {
            continue;
        };
        let Some(version_token) = tokens.next() else {
            continue;
        };

        let name = match repo_and_name.split_once('/') {
            Some((_, n)) => n.trim().to_string(),
            None => repo_and_name.trim().to_string(),
        };

        let is_installed = header_line.contains("[installed");
        let mut installed_version = version_token.to_string();
        let mut update_available = updates_map.get(&name).cloned();

        if let Some(inst_idx) = header_line.find("[installed:") {
            let after_prefix = &header_line[inst_idx + "[installed:".len()..];
            if let Some(end_bracket) = after_prefix.find(']') {
                let actual_inst_ver = after_prefix[..end_bracket].trim();
                if !actual_inst_ver.is_empty() {
                    installed_version = actual_inst_ver.to_string();
                    if actual_inst_ver != version_token && update_available.is_none() {
                        update_available = Some(version_token.to_string());
                    }
                }
            }
        }

        items.push(PackageItem {
            id: format!("pacman:{name}"),
            name,
            version: if is_installed {
                installed_version
            } else {
                version_token.to_string()
            },
            description,
            source: PackageSource::Pacman,
            is_installed,
            update_available,
            aur_meta: None,
        });
    }

    items
}

/// Récupère l'ensemble des noms de paquets étrangers (AUR / locaux) via `pacman -Qqm`.
pub async fn list_foreign_package_names() -> AppResult<HashSet<String>> {
    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Qqm")
        .output()
        .await?;

    if !output.status.success() && output.stdout.is_empty() {
        return Ok(HashSet::new());
    }

    let stdout = String::from_utf8(output.stdout)?;
    let set = stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    Ok(set)
}

/// Récupère une table de tous les paquets installés (`nom -> version`) via `pacman -Q`.
pub async fn list_all_installed_versions() -> AppResult<HashMap<String, String>> {
    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Q")
        .output()
        .await?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::PacmanError(format!(
            "Échec de `pacman -Q` : {stderr}"
        )));
    }

    let stdout = String::from_utf8(output.stdout)?;
    let mut map = HashMap::new();
    for line in stdout.lines() {
        let mut parts = line.split_whitespace();
        if let (Some(name), Some(version)) = (parts.next(), parts.next()) {
            map.insert(name.to_string(), version.to_string());
        }
    }

    Ok(map)
}

const PACMAN_UNLOCK_AND_EXEC_SCRIPT: &str = "if [ -f /var/lib/pacman/db.lck ] && ! pgrep -x pacman >/dev/null 2>&1; then rm -f /var/lib/pacman/db.lck; fi; \
rm -rf /var/lib/pacman/sync/download-* 2>/dev/null || true; \
SYNCED_FROM_CHECKUP=0; \
for d in /tmp/checkup-db-*/sync; do \
    if [ -d \"$d\" ] && ls \"$d\"/*.db >/dev/null 2>&1; then \
        cp -f \"$d\"/*.db /var/lib/pacman/sync/ 2>/dev/null && SYNCED_FROM_CHECKUP=1; \
    fi; \
done; \
if [ \"$SYNCED_FROM_CHECKUP\" = \"1\" ] && [ \"$1\" = \"-Syu\" ]; then \
    shift; exec pacman -Su \"$@\"; \
elif [ \"$SYNCED_FROM_CHECKUP\" = \"1\" ] && [ \"$1\" = \"-Sy\" ]; then \
    shift; exec pacman -S \"$@\"; \
else \
    exec pacman \"$@\"; \
fi";

/// Vérifie si un processus `pacman` est actuellement en cours d'exécution.
pub async fn is_pacman_process_running() -> bool {
    Command::new("pgrep")
        .args(["-x", "pacman"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Si une autre instance de `pacman` tourne en arrière-plan, attend sa fin en informant l'interface.
pub async fn wait_for_active_pacman(ui_tx: &async_channel::Sender<UiEvent>) {
    for attempt in 0..120 {
        if !is_pacman_process_running().await {
            return;
        }
        if attempt == 0 {
            let _ = ui_tx
                .send(UiEvent::InstallProgress {
                    phase: String::from(
                        "Une autre opération Pacman est en cours, attente de la libération de la base…",
                    ),
                    fraction: 0.03,
                    active_package: None,
                    log_line: Some(String::from(
                        "[Attente] Une autre instance de `pacman` est active ; attente de la libération de /var/lib/pacman/db.lck…",
                    )),
                })
                .await;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
}

/// Supprime un éventuel verrou `/var/lib/pacman/db.lck` orphelin avant d'exécuter un assistant externe (`yay`/`paru`).
pub async fn ensure_stale_db_lock_removed(ui_tx: &async_channel::Sender<UiEvent>) {
    wait_for_active_pacman(ui_tx).await;
    if std::path::Path::new("/var/lib/pacman/db.lck").exists()
        && !is_pacman_process_running().await
    {
        let _ = ui_tx
            .send(UiEvent::InstallProgress {
                phase: String::from("Suppression du verrou Pacman orphelin (/var/lib/pacman/db.lck)…"),
                fraction: 0.04,
                active_package: None,
                log_line: Some(String::from(
                    "$ pkexec rm -f /var/lib/pacman/db.lck",
                )),
            })
            .await;
        let _ = Command::new("pkexec")
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .args(["rm", "-f", "/var/lib/pacman/db.lck"])
            .output()
            .await;
    }
}

async fn clean_stale_checkupdates_lock() {
    if let Some(db_dir) = find_checkupdates_db_dir() {
        let lock_file = db_dir.join("db.lck");
        if lock_file.exists() {
            let checkup_running = Command::new("pgrep")
                .args(["-x", "checkupdates"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await
                .map(|s| s.success())
                .unwrap_or(false);
            if !checkup_running {
                let _ = std::fs::remove_file(lock_file);
            }
        }
    }
}

/// Détecte les mises à jour disponibles via `checkupdates` (avec repli sur `pacman -Qu`).
/// Renvoie une table `nom_du_paquet -> (ancienne_version, nouvelle_version)`.
pub async fn check_updates_map() -> AppResult<HashMap<String, (String, String)>> {
    clean_stale_checkupdates_lock().await;

    let checkupdates_res = Command::new("checkupdates")
        .env("LC_ALL", "C")
        .output()
        .await;

    match checkupdates_res {
        Ok(output) => {
            let code = output.status.code().unwrap_or(1);
            if output.status.success() || code == 2 {
                let stdout = String::from_utf8(output.stdout)?;
                return Ok(parse_checkupdates_output(&stdout));
            }
            fallback_pacman_qu().await
        }
        Err(_) => fallback_pacman_qu().await,
    }
}

async fn fallback_pacman_qu() -> AppResult<HashMap<String, (String, String)>> {
    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Qu")
        .output()
        .await?;

    let code = output.status.code().unwrap_or(1);
    if output.status.success() || code == 1 {
        let stdout = String::from_utf8(output.stdout)?;
        Ok(parse_checkupdates_output(&stdout))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(AppError::PacmanError(format!(
            "Impossible de vérifier les mises à jour (`pacman -Qu`) : {stderr}"
        )))
    }
}

/// Liste les paquets explicitement installés (`pacman -Qqe`) avec leurs métadonnées (`pacman -Qi`).
pub async fn list_installed_packages(
    updates_full: &HashMap<String, (String, String)>,
) -> AppResult<Vec<PackageItem>> {
    let qqe_output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Qqe")
        .output()
        .await?;

    if !qqe_output.status.success() {
        let stderr = String::from_utf8_lossy(&qqe_output.stderr).trim().to_string();
        return Err(AppError::PacmanError(format!(
            "Échec de `pacman -Qqe` : {stderr}"
        )));
    }

    let qqe_stdout = String::from_utf8(qqe_output.stdout)?;
    let mut target_names: Vec<String> = qqe_stdout
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect();

    let existing_set: HashSet<String> = target_names.iter().cloned().collect();
    for upd_pkg in updates_full.keys() {
        if !existing_set.contains(upd_pkg) {
            target_names.push(upd_pkg.clone());
        }
    }

    if target_names.is_empty() {
        return Ok(Vec::new());
    }

    let foreign_set = list_foreign_package_names().await.unwrap_or_default();
    let updates_simple: HashMap<String, String> = updates_full
        .iter()
        .map(|(k, (_, new_v))| (k.clone(), new_v.clone()))
        .collect();

    let qi_output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Qi")
        .args(&target_names)
        .output()
        .await?;

    let qi_stdout = String::from_utf8(qi_output.stdout)?;
    let mut items = parse_pacman_qi_output(&qi_stdout, &foreign_set, &updates_simple);

    items.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(items)
}

/// Recherche des paquets dans les dépôts synchronisés via `pacman -Ss`.
pub async fn search_packages(
    query: &str,
    updates_map: &HashMap<String, String>,
) -> AppResult<Vec<PackageItem>> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let output = Command::new("pacman")
        .env("LC_ALL", "C")
        .arg("-Ss")
        .arg(trimmed)
        .output()
        .await?;

    let code = output.status.code().unwrap_or(1);
    if !output.status.success() {
        if code == 1 && output.stderr.is_empty() {
            return Ok(Vec::new());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::PacmanError(format!(
            "Échec de la recherche `pacman -Ss {trimmed}` : {stderr}"
        )));
    }

    let stdout = String::from_utf8(output.stdout)?;
    Ok(parse_pacman_ss_output(&stdout, updates_map))
}

/// Installe un paquet officiel via `pkexec pacman -Sy --noconfirm --needed`.
pub async fn install_package(pkg_name: &str) -> AppResult<String> {
    let output = Command::new("pkexec")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .args([
            "sh",
            "-c",
            PACMAN_UNLOCK_AND_EXEC_SCRIPT,
            "--",
            "-Sy",
            "--noconfirm",
            "--needed",
            pkg_name,
        ])
        .output()
        .await?;

    if output.status.success() {
        Ok(format!("Le paquet « {pkg_name} » a été installé avec succès."))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(AppError::PacmanError(format!(
            "Échec de l'installation de « {pkg_name} » : {detail}"
        )))
    }
}

/// Met à jour un paquet officiel via `pkexec pacman -Sy --noconfirm`.
pub async fn update_package(pkg_name: &str) -> AppResult<String> {
    let output = Command::new("pkexec")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .args([
            "sh",
            "-c",
            PACMAN_UNLOCK_AND_EXEC_SCRIPT,
            "--",
            "-Sy",
            "--noconfirm",
            pkg_name,
        ])
        .output()
        .await?;

    if output.status.success() {
        Ok(format!("Le paquet « {pkg_name} » a été mis à jour avec succès."))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(AppError::PacmanError(format!(
            "Échec de la mise à jour de « {pkg_name} » : {detail}"
        )))
    }
}

/// Met à jour l'ensemble des paquets système via `pkexec pacman -Syu --noconfirm`.
pub async fn update_all_system_packages() -> AppResult<String> {
    let output = Command::new("pkexec")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .args([
            "sh",
            "-c",
            PACMAN_UNLOCK_AND_EXEC_SCRIPT,
            "--",
            "-Syu",
            "--noconfirm",
        ])
        .output()
        .await?;

    if output.status.success() {
        Ok(String::from(
            "Mise à jour complète du système terminée avec succès.",
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(AppError::PacmanError(format!(
            "Échec de la mise à jour système (`pacman -Syu`) : {detail}"
        )))
    }
}

/// Désinstalle un paquet via `pkexec pacman -Rns --noconfirm`.
pub async fn remove_package(pkg_name: &str) -> AppResult<String> {
    let output = Command::new("pkexec")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .args([
            "sh",
            "-c",
            PACMAN_UNLOCK_AND_EXEC_SCRIPT,
            "--",
            "-Rns",
            "--noconfirm",
            pkg_name,
        ])
        .output()
        .await?;

    if output.status.success() {
        Ok(format!(
            "Le paquet « {pkg_name} » a été désinstallé avec succès."
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(AppError::PacmanError(format!(
            "Échec de la désinstallation de « {pkg_name} » : {detail}"
        )))
    }
}

/// Parse la sortie de `pacman -Sp --print-format "%n|%v|%s"` pour construire la liste ordonnée
/// des dépendances et cibles à télécharger/installer.
pub fn parse_pacman_sp_output(
    raw: &str,
    explicit_targets: &HashSet<String>,
) -> Vec<DependencyStepItem> {
    let mut deps = Vec::new();
    let mut seen = HashSet::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let parts: Vec<&str> = trimmed.split('|').collect();
        if parts.len() >= 3 {
            let name = parts[0].trim().to_string();
            let version = parts[1].trim().to_string();
            let size_bytes = parts[2].trim().parse::<u64>().unwrap_or(0);
            if !name.is_empty() && seen.insert(name.clone()) {
                let is_explicit_target = explicit_targets.contains(&name);
                deps.push(DependencyStepItem {
                    name,
                    version,
                    size_bytes,
                    is_explicit_target,
                    source: PackageSource::Pacman,
                    status: DependencyStatus::Pending,
                });
            }
        }
    }

    deps
}

fn find_checkupdates_db_dir() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var("CHECKUPDATES_DB") {
        let p = PathBuf::from(custom);
        if p.join("sync").is_dir() {
            return Some(p);
        }
    }
    if let Ok(status_str) = std::fs::read_to_string("/proc/self/status") {
        for line in status_str.lines() {
            if let Some(rest) = line.strip_prefix("Uid:") {
                if let Some(uid_tok) = rest.split_whitespace().next() {
                    let cand = PathBuf::from(format!("/tmp/checkup-db-{uid_tok}"));
                    if cand.join("sync").is_dir() {
                        return Some(cand);
                    }
                }
            }
        }
    }
    None
}

/// Résout sans privilèges root la liste complète des dépendances et tailles de téléchargement
/// d'un lot de paquets Pacman via `pacman -Sp --print-format "%n|%v|%s"`.
pub async fn resolve_pacman_dependencies(targets: &[String]) -> AppResult<Vec<DependencyStepItem>> {
    if targets.is_empty() {
        return Ok(Vec::new());
    }

    let explicit_set: HashSet<String> = targets.iter().cloned().collect();
    let mut resolved = Vec::new();

    // Si checkupdates a synchronisé une base temporaire récente (/tmp/checkup-db-<uid>), l'utiliser en priorité
    // afin de détecter les tailles et versions des mises à jour avant le `pacman -Sy` root.
    if let Some(checkup_db) = find_checkupdates_db_dir() {
        if let Some(db_str) = checkup_db.to_str() {
            if let Ok(output) = Command::new("pacman")
                .env("LC_ALL", "C")
                .args([
                    "-Sp",
                    "--needed",
                    "--dbpath",
                    db_str,
                    "--print-format",
                    "%n|%v|%s",
                ])
                .args(targets)
                .output()
                .await
            {
                if let Ok(stdout) = String::from_utf8(output.stdout) {
                    resolved = parse_pacman_sp_output(&stdout, &explicit_set);
                }
            }
        }
    }

    if resolved.is_empty() {
        let output = Command::new("pacman")
            .env("LC_ALL", "C")
            .args(["-Sp", "--needed", "--print-format", "%n|%v|%s"])
            .args(targets)
            .output()
            .await?;

        let stdout = String::from_utf8(output.stdout)?;
        resolved = parse_pacman_sp_output(&stdout, &explicit_set);
    }

    if resolved.is_empty() {
        for t in targets {
            resolved.push(DependencyStepItem {
                name: t.clone(),
                version: String::from("dernière"),
                size_bytes: 0,
                is_explicit_target: true,
                source: PackageSource::Pacman,
                status: DependencyStatus::Pending,
            });
        }
    }

    Ok(resolved)
}

/// Analyse une ligne de sortie de `pacman -S` pour en extraire l'étape courante et le paquet actif.
pub fn parse_pacman_progress_line(
    line: &str,
    known_deps: &[String],
) -> Option<(String, Option<(String, DependencyStatus)>)> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.contains("Synchronizing package databases") {
        return Some((
            String::from("Synchronisation des bases de données de paquets (`pacman -Sy`)…"),
            None,
        ));
    }
    if trimmed.contains("Starting full system upgrade") {
        return Some((
            String::from("Calcul de la mise à jour complète du système…"),
            None,
        ));
    }
    if trimmed.contains("resolving dependencies") {
        return Some((String::from("Résolution des dépendances…"), None));
    }
    if trimmed.contains("looking for conflicting packages") {
        return Some((String::from("Vérification des conflits de paquets…"), None));
    }
    if trimmed.contains("Retrieving packages") {
        return Some((
            String::from("Téléchargement des paquets et dépendances…"),
            None,
        ));
    }
    if let Some(before_dl) = trimmed.strip_suffix("downloading...") {
        let token = before_dl.trim();
        let matched_pkg = match_package_from_filename_token(token, known_deps);
        return Some((
            format!("Téléchargement de « {matched_pkg} »…"),
            Some((matched_pkg, DependencyStatus::Downloading)),
        ));
    }
    if trimmed.contains("checking package integrity")
        || trimmed.contains("checking keyring")
        || trimmed.contains("loading package files")
        || trimmed.contains("checking for file conflicts")
    {
        return Some((
            String::from("Vérification de l'intégrité et des fichiers…"),
            None,
        ));
    }
    if trimmed.contains("Processing package changes") {
        return Some((
            String::from("Installation des paquets et dépendances…"),
            None,
        ));
    }

    for action_kw in ["installing ", "upgrading ", "reinstalling "] {
        if let Some(idx) = trimmed.find(action_kw) {
            let after = &trimmed[idx + action_kw.len()..];
            let pkg_name = after.trim_end_matches('.').trim().to_string();
            if !pkg_name.is_empty() {
                return Some((
                    format!("Installation de « {pkg_name} »…"),
                    Some((pkg_name, DependencyStatus::Installing)),
                ));
            }
        }
    }

    if trimmed.contains("Running post-transaction hooks") {
        return Some((
            String::from("Exécution des hooks système post-installation…"),
            None,
        ));
    }

    None
}

fn match_package_from_filename_token(token: &str, known_deps: &[String]) -> String {
    let mut best_match: Option<&String> = None;
    for dep in known_deps {
        let prefix = format!("{dep}-");
        if token.starts_with(&prefix) {
            match best_match {
                Some(prev) if dep.len() > prev.len() => best_match = Some(dep),
                None => best_match = Some(dep),
                _ => {}
            }
        }
    }
    if let Some(found) = best_match {
        return found.clone();
    }

    // Repli : suppression des 3 derniers segments `-pkgver-pkgrel-arch`
    let parts: Vec<&str> = token.split('-').collect();
    if parts.len() > 3 {
        parts[..parts.len() - 3].join("-")
    } else {
        token.to_string()
    }
}

/// Installe ou met à jour un lot de paquets Pacman via `pkexec pacman -Sy --noconfirm` en diffusant
/// en temps réel la progression de téléchargement et d'installation des dépendances.
pub async fn install_packages_streaming(
    targets: &[String],
    known_deps: &[String],
    has_updates: bool,
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> AppResult<String> {
    if targets.is_empty() {
        return Ok(String::from("Aucun paquet Pacman à installer."));
    }
    if cancel_handle.is_cancelled() {
        return Err(AppError::PacmanError(String::from(
            "Installation annulée par l'utilisateur.",
        )));
    }

    wait_for_active_pacman(ui_tx).await;

    let base_args: &[&str] = if has_updates {
        &["-Sy", "--noconfirm"]
    } else {
        &["-Sy", "--noconfirm", "--needed"]
    };

    let cmd_preview = format!(
        "$ pkexec pacman {} {}",
        base_args.join(" "),
        targets.join(" ")
    );

    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: String::from(
                "Élévation de privilèges administrateur (Polkit) et synchronisation Pacman…",
            ),
            fraction: 0.04,
            active_package: None,
            log_line: Some(cmd_preview),
        })
        .await;

    let mut child = Command::new("pkexec")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .args(["sh", "-c", PACMAN_UNLOCK_AND_EXEC_SCRIPT, "--"])
        .args(base_args)
        .args(targets)
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
                            phase: String::from("Installation annulée par l'utilisateur."),
                            fraction: -1.0,
                            active_package: None,
                            log_line: Some(String::from("[Annulé] Transaction interrompue à la demande de l'utilisateur.")),
                        })
                        .await;
                    return Err(AppError::PacmanError(String::from(
                        "Installation annulée par l'utilisateur.",
                    )));
                }
                res = reader.next_line() => res.ok().flatten(),
            };

            let Some(line) = next_line_opt else {
                break;
            };

            if let Some((phase, active_pkg)) = parse_pacman_progress_line(&line, known_deps) {
                if active_pkg.is_some() {
                    completed_steps = (completed_steps + 1.0).min(total_steps);
                }
                let raw_frac = 0.08 + 0.86 * (completed_steps / total_steps);
                let fraction = if phase.contains("hooks") {
                    0.95
                } else {
                    raw_frac.clamp(0.08, 0.94)
                };

                let _ = ui_tx
                    .send(UiEvent::InstallProgress {
                        phase,
                        fraction,
                        active_package: active_pkg,
                        log_line: Some(line),
                    })
                    .await;
            } else {
                let _ = ui_tx
                    .send(UiEvent::InstallProgress {
                        phase: String::new(),
                        fraction: -1.0,
                        active_package: None,
                        log_line: Some(line),
                    })
                    .await;
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
            return Err(AppError::PacmanError(String::from(
                "Installation annulée par l'utilisateur.",
            )));
        }
        st = child.wait() => st?,
    };

    if status.success() {
        Ok(format!(
            "{} paquet(s) Pacman et leurs dépendances installés/mis à jour avec succès.",
            targets.len()
        ))
    } else {
        let exit_code = status.code().unwrap_or(1);
        let detail = if exit_code == 126 || exit_code == 127 || stderr_buf.contains("dismissed") || stderr_buf.contains("Not authorized") {
            format!(
                "Élévation de privilèges (Polkit) refusée ou annulée (code {exit_code}) : {}",
                stderr_buf.trim()
            )
        } else if stderr_buf.trim().is_empty() {
            format!("Code de sortie {exit_code}")
        } else {
            stderr_buf.trim().to_string()
        };
        Err(AppError::PacmanError(detail))
    }
}

/// Met à jour tous les paquets des dépôts officiels via `pkexec pacman -Syu --noconfirm`
/// avec diffusion temps réel de la progression et prise en charge de l'annulation.
pub async fn update_pacman_all_streaming(
    known_deps: &[String],
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> AppResult<String> {
    if cancel_handle.is_cancelled() {
        return Err(AppError::PacmanError(String::from(
            "Mise à jour annulée par l'utilisateur.",
        )));
    }

    wait_for_active_pacman(ui_tx).await;

    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: String::from(
                "Élévation de privilèges (Polkit) et mise à jour des dépôts officiels (`pkexec pacman -Syu`)…",
            ),
            fraction: 0.05,
            active_package: None,
            log_line: Some(String::from("$ pkexec pacman -Syu --noconfirm")),
        })
        .await;

    let mut child = Command::new("pkexec")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .args([
            "sh",
            "-c",
            PACMAN_UNLOCK_AND_EXEC_SCRIPT,
            "--",
            "-Syu",
            "--noconfirm",
        ])
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
                            phase: String::from("Mise à jour Pacman annulée par l'utilisateur."),
                            fraction: -1.0,
                            active_package: None,
                            log_line: Some(String::from("[Annulé] Mise à jour Pacman interrompue à la demande de l'utilisateur.")),
                        })
                        .await;
                    return Err(AppError::PacmanError(String::from(
                        "Mise à jour Pacman annulée par l'utilisateur.",
                    )));
                }
                res = reader.next_line() => res.ok().flatten(),
            };

            let Some(line) = next_line_opt else {
                break;
            };

            if let Some((phase, active_pkg)) = parse_pacman_progress_line(&line, known_deps) {
                if active_pkg.is_some() {
                    completed_steps = (completed_steps + 1.0).min(total_steps);
                }
                let frac = (0.08 + 0.45 * (completed_steps / total_steps)).clamp(0.08, 0.52);
                let _ = ui_tx
                    .send(UiEvent::InstallProgress {
                        phase,
                        fraction: frac,
                        active_package: active_pkg,
                        log_line: Some(line),
                    })
                    .await;
            } else {
                let _ = ui_tx
                    .send(UiEvent::InstallProgress {
                        phase: String::new(),
                        fraction: -1.0,
                        active_package: None,
                        log_line: Some(line),
                    })
                    .await;
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
            return Err(AppError::PacmanError(String::from(
                "Mise à jour Pacman annulée par l'utilisateur.",
            )));
        }
        st = child.wait() => st?,
    };

    if status.success() {
        Ok(String::from(
            "Mises à jour Pacman terminées avec succès.",
        ))
    } else {
        let exit_code = status.code().unwrap_or(1);
        let detail = if exit_code == 126
            || exit_code == 127
            || stderr_buf.contains("dismissed")
            || stderr_buf.contains("Not authorized")
        {
            format!(
                "Élévation de privilèges (Polkit) refusée ou annulée (code {exit_code}) : {}",
                stderr_buf.trim()
            )
        } else if stderr_buf.trim().is_empty() {
            format!("Code de sortie {exit_code}")
        } else {
            stderr_buf.trim().to_string()
        };
        Err(AppError::PacmanError(detail))
    }
}
