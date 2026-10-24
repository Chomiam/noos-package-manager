use std::fs;
use std::process::Stdio;

use tokio::process::Command;

use crate::backend::elevation::Elevation;
use crate::error::{AppError, AppResult};

const PACMAN_CONF: &str = "/etc/pacman.conf";

/// Lit la valeur effective de `ParallelDownloads` (1 à 20).
pub async fn read_parallel_downloads() -> AppResult<u8> {
    if let Ok(output) = Command::new("pacman-conf")
        .arg("ParallelDownloads")
        .stdin(Stdio::null())
        .output()
        .await
    {
        if output.status.success() {
            if let Ok(value) = String::from_utf8_lossy(&output.stdout).trim().parse::<u8>() {
                return Ok(value.clamp(1, 20));
            }
        }
    }

    let content = fs::read_to_string(PACMAN_CONF)?;
    Ok(parse_parallel_downloads(&content).unwrap_or(5))
}

/// Applique `ParallelDownloads` dans `/etc/pacman.conf` via l'élévation privilégiée.
pub async fn apply_parallel_downloads(elevation: &Elevation, value: u8) -> AppResult<String> {
    if !(1..=20).contains(&value) {
        return Err(AppError::PacmanError(format!(
            "valeur ParallelDownloads invalide : {value} (attendu : 1 à 20)"
        )));
    }

    let content = fs::read_to_string(PACMAN_CONF)?;
    let updated = rewrite_parallel_downloads(&content, value)?;
    let mut temp = tempfile::NamedTempFile::new()?;
    use std::io::Write;
    temp.write_all(updated.as_bytes())?;
    temp.flush()?;

    let script = "if [ ! -e /etc/pacman.conf.noos-backup ]; then \
                      cp -a /etc/pacman.conf /etc/pacman.conf.noos-backup; \
                  fi; \
                  install -m 644 \"$1\" /etc/pacman.conf.noos-new && \
                  mv -f /etc/pacman.conf.noos-new /etc/pacman.conf";
    let output = elevation
        .build_command("sh")
        .args(["-c", script, "--"])
        .arg(temp.path())
        .stdin(Stdio::null())
        .output()
        .await?;

    if !output.status.success() {
        if elevation.authentication_was_cancelled() {
            return Err(AppError::PacmanError(String::from(
                "Authentification administrateur annulée.",
            )));
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::PacmanError(format!(
            "échec de l'écriture de /etc/pacman.conf : {stderr}"
        )));
    }

    let effective = read_parallel_downloads().await?;
    if effective != value {
        return Err(AppError::PacmanError(format!(
            "ParallelDownloads a été écrit mais la valeur effective est {effective} au lieu de {value}."
        )));
    }

    Ok(format!(
        "Téléchargements parallèles Pacman réglés sur {value}."
    ))
}

/// Extrait la dernière valeur active de `ParallelDownloads`.
pub(crate) fn parse_parallel_downloads(content: &str) -> Option<u8> {
    let mut value = None;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') || trimmed.is_empty() {
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("ParallelDownloads") {
            if let Some((_, raw_value)) = rest.split_once('=') {
                if let Ok(parsed) = raw_value.trim().parse::<u8>() {
                    value = Some(parsed);
                }
            }
        }
    }
    value
}

/// Réécrit uniquement la ligne `ParallelDownloads` en conservant le reste du fichier.
pub(crate) fn rewrite_parallel_downloads(content: &str, value: u8) -> AppResult<String> {
    if !(1..=20).contains(&value) {
        return Err(AppError::PacmanError(format!(
            "valeur ParallelDownloads invalide : {value} (attendu : 1 à 20)"
        )));
    }

    let had_trailing_newline = content.ends_with('\n');
    let mut lines: Vec<String> = content.lines().map(str::to_string).collect();

    let Some(options_index) = lines.iter().position(|line| line.trim() == "[options]") else {
        lines.insert(0, String::from("[options]"));
        lines.insert(1, format!("ParallelDownloads = {value}"));
        let mut result = lines.join("\n");
        if had_trailing_newline {
            result.push('\n');
        }
        return Ok(result);
    };

    let options_end = lines
        .iter()
        .enumerate()
        .skip(options_index + 1)
        .find(|(_, line)| line.trim_start().starts_with('['))
        .map(|(index, _)| index)
        .unwrap_or(lines.len());

    let existing_index = (options_index + 1..options_end).rev().find(|&index| {
        let trimmed = lines[index].trim_start();
        !trimmed.starts_with('#') && trimmed.starts_with("ParallelDownloads")
    });

    if let Some(index) = existing_index {
        lines[index] = format!("ParallelDownloads = {value}");
    } else {
        lines.insert(options_index + 1, format!("ParallelDownloads = {value}"));
    }

    let mut result = lines.join("\n");
    if had_trailing_newline {
        result.push('\n');
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_last_active_value() {
        let content = "[options]\n#ParallelDownloads = 3\nParallelDownloads = 5\n";
        assert_eq!(parse_parallel_downloads(content), Some(5));
    }

    #[test]
    fn rewrites_existing_value_and_keeps_comments() {
        let content =
            "[options]\n#ParallelDownloads = 3\nParallelDownloads = 5\nColor\n\n[extra]\n";
        let updated = rewrite_parallel_downloads(content, 12).unwrap();
        assert_eq!(
            updated,
            "[options]\n#ParallelDownloads = 3\nParallelDownloads = 12\nColor\n\n[extra]\n"
        );
    }

    #[test]
    fn inserts_value_inside_options_section() {
        let content = "[options]\nColor\n\n[extra]\n";
        let updated = rewrite_parallel_downloads(content, 8).unwrap();
        assert_eq!(
            updated,
            "[options]\nParallelDownloads = 8\nColor\n\n[extra]\n"
        );
    }

    #[test]
    fn creates_options_section_when_missing() {
        let content = "[custom]\nSigLevel = Never\n";
        let updated = rewrite_parallel_downloads(content, 4).unwrap();
        assert_eq!(
            updated,
            "[options]\nParallelDownloads = 4\n[custom]\nSigLevel = Never\n"
        );
    }

    #[test]
    fn rejects_out_of_range_values() {
        assert!(rewrite_parallel_downloads("[options]\n", 0).is_err());
        assert!(rewrite_parallel_downloads("[options]\n", 21).is_err());
    }
}
