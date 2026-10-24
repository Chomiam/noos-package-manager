use std::collections::{HashMap, HashSet};
use std::process::Stdio;
use std::time::Duration;

use tempfile::TempDir;
use tokio::process::Command;

use crate::backend::aur::{run_yay_transaction_streaming, shared_aur_client, YayTransaction};
use crate::backend::elevation::Elevation;
use crate::backend::models::{AurVersionInfo, InstallCancelHandle};
use crate::error::{AppError, AppResult};
use crate::ui::messages::UiEvent;

const GIT_TIMEOUT: Duration = Duration::from_secs(180);
const MAX_COMMITS: usize = 220;
const MAX_VERSIONS: usize = 30;

/// Récupère l'historique des versions d'un paquet AUR depuis son dépôt git.
pub async fn list_versions(package: &str) -> AppResult<(String, Vec<AurVersionInfo>)> {
    let client = shared_aur_client().ok_or_else(|| {
        AppError::AurHelperError(String::from("Client AUR partagé non initialisé"))
    })?;

    let raw = client.fetch_raw_packages(&[package]).await?;
    let package_base = raw
        .first()
        .and_then(|pkg| pkg.package_base.clone())
        .unwrap_or_else(|| package.to_string());

    let repo = clone_aur_repo(&package_base).await?;
    let versions = read_version_history(&repo).await?;
    Ok((package_base, versions))
}

/// Installe une version historique d'un paquet AUR en construisant le PKGBUILD
/// correspondant au commit fourni via `yay -Bi`.
pub async fn install_version(
    package: &str,
    package_base: &str,
    version: &str,
    commit: &str,
    elevation: &Elevation,
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> AppResult<String> {
    let repo = clone_aur_repo(package_base).await?;

    let checkout = tokio::time::timeout(
        GIT_TIMEOUT,
        Command::new("git")
            .current_dir(repo.path())
            .args(["checkout", "--quiet", commit])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| AppError::AurHelperError(String::from("délai de préparation git dépassé")))?
    .map_err(|err| AppError::AurHelperError(format!("git checkout : {err}")))?;

    if !checkout.status.success() {
        let stderr = String::from_utf8_lossy(&checkout.stderr).trim().to_string();
        return Err(AppError::AurHelperError(format!(
            "impossible de préparer le commit {commit} : {stderr}"
        )));
    }

    let build_dir = repo.path().to_string_lossy().into_owned();
    let yay_args = ["-B", "-i", build_dir.as_str(), "--noconfirm"];

    run_yay_transaction_streaming(
        YayTransaction {
            yay_args: &yay_args,
            targets: &[],
            phase: format!(
                "Installation de {package} {version} depuis l'historique AUR (une seule authentification)…"
            ),
            success_message: format!("{package} {version} installé depuis l'historique AUR."),
            interactive: true,
        },
        &[],
        elevation,
        cancel_handle,
        ui_tx,
    )
    .await
}

async fn clone_aur_repo(package_base: &str) -> AppResult<TempDir> {
    let dir = tempfile::tempdir()?;
    let url = format!("https://aur.archlinux.org/{package_base}.git");

    let output = tokio::time::timeout(
        GIT_TIMEOUT,
        Command::new("git")
            .args(["clone", "--quiet", "--no-checkout", &url])
            .arg(dir.path())
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| AppError::AurHelperError(String::from("délai de clonage du dépôt AUR dépassé")))?
    .map_err(|err| AppError::AurHelperError(format!("git clone : {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::AurHelperError(format!(
            "impossible de récupérer le dépôt AUR de « {package_base} » : {stderr}"
        )));
    }

    Ok(dir)
}

async fn read_version_history(repo: &TempDir) -> AppResult<Vec<AurVersionInfo>> {
    let log = tokio::time::timeout(
        GIT_TIMEOUT,
        Command::new("git")
            .current_dir(repo.path())
            .args(["log", "--format=%H%x09%ct", "--", "PKGBUILD"])
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| {
        AppError::AurHelperError(String::from("délai de lecture de l'historique git dépassé"))
    })?
    .map_err(|err| AppError::AurHelperError(format!("git log : {err}")))?;

    if !log.status.success() {
        let stderr = String::from_utf8_lossy(&log.stderr).trim().to_string();
        return Err(AppError::AurHelperError(format!(
            "impossible de lire l'historique du PKGBUILD : {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&log.stdout);
    let mut versions = Vec::new();
    let mut seen = HashSet::<String>::new();

    for line in stdout.lines().take(MAX_COMMITS) {
        let mut parts = line.splitn(2, '\t');
        let Some(commit) = parts.next() else {
            continue;
        };
        let timestamp = parts.next().and_then(|value| value.parse::<i64>().ok());

        let show = Command::new("git")
            .current_dir(repo.path())
            .args(["show", &format!("{commit}:PKGBUILD")])
            .stdin(Stdio::null())
            .output()
            .await;

        let Ok(show) = show else {
            continue;
        };
        if !show.status.success() {
            continue;
        }

        let pkgbuild = String::from_utf8_lossy(&show.stdout);
        let Some(version) = parse_pkgbuild_version(&pkgbuild) else {
            continue;
        };

        if seen.insert(version.clone()) {
            versions.push(AurVersionInfo {
                version,
                commit: commit.to_string(),
                timestamp,
            });
            if versions.len() >= MAX_VERSIONS {
                break;
            }
        }
    }

    Ok(versions)
}

pub(crate) fn parse_pkgbuild_version(pkgbuild: &str) -> Option<String> {
    let mut variables = HashMap::<String, String>::new();
    let mut pkgver = None;
    let mut pkgrel = None;
    let mut epoch = None;

    for raw_line in pkgbuild.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .to_string();

        if key.starts_with('_') {
            variables.insert(key.to_string(), value.clone());
        }
        match key {
            "pkgver" => pkgver = Some(value),
            "pkgrel" => pkgrel = Some(value),
            "epoch" => epoch = Some(value),
            _ => {}
        }
    }

    let expand = |value: &str| -> String {
        let mut expanded = value.to_string();
        for (name, replacement) in &variables {
            expanded = expanded.replace(&format!("${{{name}}}"), replacement);
            expanded = expanded.replace(&format!("${name}"), replacement);
        }
        expanded
    };

    let pkgver = expand(&pkgver?);
    if pkgver.is_empty() {
        return None;
    }

    let mut result = String::new();
    if let Some(epoch) = epoch {
        let epoch = expand(&epoch);
        if !epoch.is_empty() && epoch != "0" {
            result.push_str(&epoch);
            result.push(':');
        }
    }
    result.push_str(&pkgver);

    if let Some(pkgrel) = pkgrel {
        let pkgrel = expand(&pkgrel);
        if !pkgrel.is_empty() {
            result.push('-');
            result.push_str(&pkgrel);
        }
    }

    Some(result)
}
