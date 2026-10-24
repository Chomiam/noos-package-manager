use std::process::Stdio;

use tokio::process::Command;

use crate::backend::elevation::Elevation;
use crate::backend::models::ServiceRestartFailure;
use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, Default)]
pub struct NeedrestartReport {
    pub services: Vec<String>,
    pub kernel_status: Option<u32>,
}

#[derive(Debug, Clone, Default)]
pub struct ServiceRestartReport {
    pub restarted: Vec<String>,
    pub failures: Vec<ServiceRestartFailure>,
}

/// Détecte les services nécessitant un redémarrage après une mise à jour.
pub async fn detect(elevation: &Elevation) -> AppResult<NeedrestartReport> {
    let output = elevation
        .build_command("needrestart")
        .arg("-b")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|err| {
            AppError::NeedrestartError(format!(
                "impossible d'exécuter needrestart (paquet 'needrestart' installé ?) : {err}"
            ))
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let report = parse_batch_output(&stdout);

    if !output.status.success() && report.services.is_empty() && report.kernel_status.is_none() {
        let detail = stderr.trim();
        return Err(AppError::NeedrestartError(if detail.is_empty() {
            format!(
                "needrestart a échoué (code {})",
                output.status.code().unwrap_or(1)
            )
        } else {
            detail.to_string()
        }));
    }

    Ok(report)
}

/// Redémarre les services listés puis vérifie leur état.
pub async fn restart_services(services: &[String]) -> AppResult<ServiceRestartReport> {
    let services: Vec<String> = services
        .iter()
        .map(|service| service.trim().to_string())
        .filter(|service| {
            !service.is_empty() && service != "systemd-manager" && service != "systemd-user"
        })
        .collect();

    if services.is_empty() {
        return Ok(ServiceRestartReport::default());
    }

    let elevation = Elevation::detect();
    let mut restart_cmd = elevation.build_command("systemctl");
    restart_cmd
        .arg("restart")
        .args(&services)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let restart_output = restart_cmd.output().await?;
    if !restart_output.status.success() && elevation.authentication_was_cancelled() {
        return Err(AppError::NeedrestartError(String::from(
            "Authentification administrateur annulée.",
        )));
    }

    let mut report = ServiceRestartReport::default();
    for service in &services {
        let active_output = Command::new("systemctl")
            .args(["is-active", service])
            .stdin(Stdio::null())
            .output()
            .await;

        let active = active_output
            .as_ref()
            .map(|output| String::from_utf8_lossy(&output.stdout).trim() == "active")
            .unwrap_or(false);

        if active {
            report.restarted.push(service.clone());
            continue;
        }

        let status = systemctl_status(service).await;
        report.failures.push(ServiceRestartFailure {
            name: service.clone(),
            status,
        });
    }

    Ok(report)
}

async fn systemctl_status(service: &str) -> String {
    let output = Command::new("systemctl")
        .args(["status", "--no-pager", "--lines=0", service])
        .stdin(Stdio::null())
        .output()
        .await;

    let Ok(output) = output else {
        return String::from("état indisponible");
    };

    let mut text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stderr).trim().to_string();
    }
    if text.is_empty() {
        return format!("état inconnu (code {})", output.status.code().unwrap_or(1));
    }

    const MAX_CHARS: usize = 1200;
    if text.chars().count() > MAX_CHARS {
        let truncated: String = text.chars().take(MAX_CHARS).collect();
        format!("{truncated}…")
    } else {
        text
    }
}

/// Parse la sortie de `needrestart -b`.
pub(crate) fn parse_batch_output(stdout: &str) -> NeedrestartReport {
    let mut report = NeedrestartReport::default();
    for line in stdout.lines() {
        if let Some(service) = line.strip_prefix("NEEDRESTART-SVC: ") {
            let service = service.trim();
            if !service.is_empty()
                && service != "systemd-manager"
                && service != "systemd-user"
                && !report.services.iter().any(|existing| existing == service)
            {
                report.services.push(service.to_string());
            }
        } else if let Some(status) = line.strip_prefix("NEEDRESTART-KSTA: ") {
            report.kernel_status = status.trim().parse::<u32>().ok();
        }
    }
    report
        .services
        .sort_by_key(|service| service.to_lowercase());
    report
}

/// Indique si un redémarrage complet est conseillé (nouveau noyau).
pub fn reboot_recommended(kernel_status: Option<u32>) -> bool {
    kernel_status.is_some_and(|status| status >= 2)
}
