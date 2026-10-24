use std::collections::HashMap;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

use crate::backend::elevation::Elevation;
use crate::backend::models::ReflectorMirror;
use crate::error::{AppError, AppResult};

const REFLECTOR_TIMEOUT: Duration = Duration::from_secs(180);
const MIRROR_COUNT: usize = 15;

/// Analyse les miroirs Arch avec Reflector et retourne leur débit mesuré.
///
/// Reflector écrit la liste des miroirs (avec pays/protocole) sur stdout et les
/// mesures de débit sur stderr en mode `--verbose --info`.
pub async fn list_mirrors() -> AppResult<Vec<ReflectorMirror>> {
    let args = [
        "--latest",
        &MIRROR_COUNT.to_string(),
        "--sort",
        "rate",
        "--verbose",
        "--info",
        "--connection-timeout",
        "3",
        "--download-timeout",
        "5",
    ];

    let output = tokio::time::timeout(
        REFLECTOR_TIMEOUT,
        Command::new("reflector")
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output(),
    )
    .await
    .map_err(|_| AppError::ReflectorError(String::from("délai d'analyse des miroirs dépassé")))?
    .map_err(|err| {
        AppError::ReflectorError(format!(
            "impossible d'exécuter reflector (paquet 'reflector' installé ?) : {err}"
        ))
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::ReflectorError(if stderr.is_empty() {
            format!(
                "reflector a échoué (code {})",
                output.status.code().unwrap_or(1)
            )
        } else {
            stderr
        }));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    Ok(parse_reflector_output(&stdout, &stderr))
}

/// Écrit `/etc/pacman.d/mirrorlist` avec les miroirs fournis.
pub async fn apply_mirrorlist(elevation: &Elevation, servers: &[String]) -> AppResult<String> {
    let mut unique = Vec::new();
    for server in servers {
        let trimmed = server.trim().trim_end_matches('/');
        if trimmed.is_empty() {
            continue;
        }
        if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
            return Err(AppError::ReflectorError(format!(
                "URL de miroir invalide : {trimmed}"
            )));
        }
        if trimmed.chars().any(char::is_whitespace) {
            return Err(AppError::ReflectorError(format!(
                "URL de miroir invalide : {trimmed}"
            )));
        }
        if !unique.iter().any(|existing| existing == trimmed) {
            unique.push(trimmed.to_string());
        }
    }

    if unique.is_empty() {
        return Err(AppError::ReflectorError(String::from(
            "aucun miroir sélectionné",
        )));
    }

    let mut content = String::from(
        "# /etc/pacman.d/mirrorlist\n\
         # Généré par noos-package-manager via Reflector\n\n",
    );
    for server in &unique {
        content.push_str(&format!("Server = {server}$repo/os/$arch\n"));
    }

    let temp_dir = tempfile::tempdir()?;
    let temp_path = temp_dir.path().join("mirrorlist");
    std::fs::write(&temp_path, &content)?;

    let output = elevation
        .build_command("install")
        .args(["-m", "644"])
        .arg(&temp_path)
        .arg("/etc/pacman.d/mirrorlist")
        .stdin(Stdio::null())
        .output()
        .await?;

    if output.status.success() {
        Ok(format!(
            "Mirrorlist mise à jour avec {} miroir(s).",
            unique.len()
        ))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(AppError::ReflectorError(format!(
            "échec de l'écriture de /etc/pacman.d/mirrorlist : {detail}"
        )))
    }
}

/// Extrait les miroirs (stdout `--info`) et les débits (stderr `--verbose`).
pub(crate) fn parse_reflector_output(stdout: &str, stderr: &str) -> Vec<ReflectorMirror> {
    let rates = parse_rates(stderr);
    let mut mirrors = Vec::new();

    for block in stdout.split("\n\n") {
        let mut lines = block.lines().filter(|line| !line.trim().is_empty());
        let Some(first) = lines.next() else {
            continue;
        };
        let url = normalize_url(first.trim());
        if url.is_empty() || !url.starts_with("http") {
            continue;
        }

        let mut country = String::new();
        let mut protocol = String::new();
        let mut delay_hours = 0.0;

        for line in lines {
            let Some((key, value)) = line.split_once(':') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "country" => country = value.to_string(),
                "protocol" => protocol = value.to_string(),
                "delay" => delay_hours = value.parse::<f64>().unwrap_or(0.0),
                _ => {}
            }
        }

        let rate_kib_s = rates.get(&url).copied().unwrap_or(0.0);
        mirrors.push(ReflectorMirror {
            url,
            country,
            protocol,
            rate_kib_s,
            delay_hours,
        });
    }

    mirrors.sort_by(|a, b| {
        b.rate_kib_s
            .total_cmp(&a.rate_kib_s)
            .then_with(|| a.url.cmp(&b.url))
    });
    mirrors
}

fn parse_rates(stderr: &str) -> HashMap<String, f64> {
    let mut rates = HashMap::new();
    for line in stderr.lines() {
        let Some((_, rest)) = line.split_once("INFO: ") else {
            continue;
        };
        let tokens: Vec<&str> = rest.split_whitespace().collect();
        if tokens.len() < 3 || tokens[2] != "KiB/s" {
            continue;
        }
        let url = normalize_url(tokens[0]);
        if let Ok(rate) = tokens[1].parse::<f64>() {
            rates.insert(url, rate);
        }
    }
    rates
}

fn normalize_url(url: &str) -> String {
    url.trim()
        .trim_end_matches('/')
        .trim_end_matches("$repo/os/$arch")
        .trim_end_matches('/')
        .to_string()
}

/// Formate un débit en unité lisible.
pub fn format_rate(rate_kib_s: f64) -> String {
    if rate_kib_s >= 1024.0 {
        format!("{:.1} MiB/s", rate_kib_s / 1024.0)
    } else if rate_kib_s > 0.0 {
        format!("{rate_kib_s:.1} KiB/s")
    } else {
        String::from("débit inconnu")
    }
}

/// Utilitaire d'affichage : extrait le nom d'hôte d'une URL de miroir.
pub fn mirror_display_name(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(url)
        .to_string()
}
