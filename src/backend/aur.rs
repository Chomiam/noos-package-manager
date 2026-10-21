use std::collections::{HashMap, HashSet};
use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::backend::models::{
    AurHelper, AurPackageMeta, AurSortMode, DependencyStatus, DependencyStepItem,
    InstallCancelHandle, PackageItem, PackageSource,
};
use crate::error::{AppError, AppResult};
use crate::ui::messages::UiEvent;

pub const AUR_RPC_BASE_URL: &str = "https://aur.archlinux.org/rpc/v5";

pub const POPULAR_AUR_SHOWCASE: &[&str] = &[
    "noos-package-manager",
    "paru",
    "yay",
    "brave-bin",
    "visual-studio-code-bin",
    "google-chrome",
    "zen-browser-bin",
    "vesktop-bin",
    "spotify",
    "heroic-games-launcher-bin",
    "protonup-qt",
    "bottles",
    "onlyoffice-bin",
    "vscodium-bin",
    "floorp-bin",
    "slack-desktop",
    "zoom",
    "anydesk-bin",
    "timeshift-autosnap",
    "ventoy-bin",
    "sublime-text-4",
    "jetbrains-toolbox",
    "postman-bin",
    "insomnia-bin",
    "dbeaver-ce-bin",
    "github-desktop-bin",
    "gitkraken",
    "cursor-bin",
    "zed-preview-bin",
    "waterfox-bin",
    "tor-browser-bin",
    "mullvad-browser-bin",
    "ferdium-bin",
    "teams-for-linux",
    "viber",
    "jellyfin-media-player",
    "stremio",
    "nuclear-player-bin",
    "freetube-bin",
    "yt-dlg",
    "upscayl-bin",
    "xnviewmp",
    "aseprite",
    "blockbench-bin",
    "itch-setup-bin",
    "lunar-client",
    "r2modman-bin",
    "game-devices-udev",
    "mangohud-git",
    "auto-cpufreq",
    "coolercontrol-bin",
    "bauh",
    "octopi",
    "pamac-aur",
    "pacseek",
    "downgrade",
    "pikaur",
    "trizen",
    "aurutils",
    "debtap",
    "appimagelauncher",
    "localsend-bin",
    "rustdesk-bin",
    "nomachine",
    "teamviewer",
    "1password",
    "bitwarden-bin",
    "keepassxc-git",
    "veracrypt-git",
    "obsidian-bin",
    "notion-app-enhanced",
    "logseq-desktop-bin",
    "joplin-appimage",
    "standardnotes-bin",
    "ticktick",
    "todoist-appimage",
    "wps-office",
    "freeoffice",
    "masterpdfeditor-free",
    "xmind",
    "drawio-desktop-bin",
];

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AurRpcResponse {
    pub version: u32,
    #[serde(rename = "type")]
    pub response_type: String,
    pub resultcount: usize,
    #[serde(default)]
    pub results: Vec<AurRpcPackage>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AurRpcPackage {
    #[serde(rename = "ID")]
    pub id: u64,
    #[serde(rename = "Name")]
    pub name: String,
    #[serde(rename = "PackageBaseID")]
    pub package_base_id: Option<u64>,
    #[serde(rename = "PackageBase")]
    pub package_base: Option<String>,
    #[serde(rename = "Version")]
    pub version: String,
    #[serde(rename = "Description")]
    pub description: Option<String>,
    #[serde(rename = "URL")]
    pub url: Option<String>,
    #[serde(rename = "NumVotes")]
    pub num_votes: Option<u32>,
    #[serde(rename = "Popularity")]
    pub popularity: Option<f64>,
    #[serde(rename = "OutOfDate")]
    pub out_of_date: Option<u64>,
    #[serde(rename = "Maintainer")]
    pub maintainer: Option<String>,
    #[serde(rename = "FirstSubmitted")]
    pub first_submitted: Option<u64>,
    #[serde(rename = "LastModified")]
    pub last_modified: Option<u64>,
    #[serde(rename = "URLPath")]
    pub url_path: Option<String>,
    #[serde(rename = "Depends", default)]
    pub depends: Vec<String>,
    #[serde(rename = "MakeDepends", default)]
    pub make_depends: Vec<String>,
    #[serde(rename = "OptDepends", default)]
    pub opt_depends: Vec<String>,
    #[serde(rename = "License", default)]
    pub license: Vec<String>,
    #[serde(rename = "Keywords", default)]
    pub keywords: Vec<String>,
}

impl AurRpcPackage {
    pub fn into_package_item(
        self,
        installed_versions: &HashMap<String, String>,
    ) -> PackageItem {
        let installed_ver = installed_versions.get(&self.name);
        let is_installed = installed_ver.is_some();

        let (display_version, update_available) = match installed_ver {
            Some(inst_v) => {
                let update = if inst_v != &self.version {
                    Some(self.version.clone())
                } else {
                    None
                };
                (inst_v.clone(), update)
            }
            None => (self.version, None),
        };

        let base_desc = self
            .description
            .unwrap_or_else(|| String::from("Paquet communautaire AUR"));

        let aur_meta = AurPackageMeta {
            votes: self.num_votes.unwrap_or(0),
            popularity: self.popularity.unwrap_or(0.0),
            maintainer: self.maintainer,
            last_modified: self.last_modified,
            out_of_date: self.out_of_date,
        };

        PackageItem {
            id: format!("aur:{}", self.name),
            name: self.name,
            version: display_version,
            description: base_desc,
            source: PackageSource::Aur,
            is_installed,
            update_available,
            aur_meta: Some(aur_meta),
        }
    }
}

/// Parse une réponse JSON de l'API AUR RPC v5 et la convertit en `Vec<PackageItem>`
/// triée par popularité décroissante par défaut.
pub fn parse_aur_rpc_json(
    json_payload: &str,
    installed_versions: &HashMap<String, String>,
) -> AppResult<Vec<PackageItem>> {
    let parsed: AurRpcResponse = serde_json::from_str(json_payload)?;

    if parsed.response_type == "error" {
        let msg = parsed
            .error
            .unwrap_or_else(|| String::from("Erreur inconnue renvoyée par l'API AUR RPC v5"));
        return Err(AppError::AurApiError(msg));
    }

    let mut items: Vec<PackageItem> = parsed
        .results
        .into_iter()
        .map(|pkg| pkg.into_package_item(installed_versions))
        .collect();

    items.sort_by(|a, b| AurSortMode::Popularity.compare_items(a, b));
    Ok(items)
}

/// Construit l'URL de recherche AUR RPC v5 : `https://aur.archlinux.org/rpc/v5/search/{terme}`
pub fn build_search_url(term: &str) -> String {
    let encoded = urlencoding::encode(term.trim());
    format!("{AUR_RPC_BASE_URL}/search/{encoded}")
}

/// Construit l'URL d'informations détaillées AUR RPC v5 : `https://aur.archlinux.org/rpc/v5/info?arg[]={pkg}`
pub fn build_info_url(packages: &[&str]) -> String {
    let mut query_parts = Vec::with_capacity(packages.len());
    for pkg in packages {
        let encoded = urlencoding::encode(pkg.trim());
        query_parts.push(format!("arg[]={encoded}"));
    }
    format!("{AUR_RPC_BASE_URL}/info?{}", query_parts.join("&"))
}

/// Vérifie si un fichier binaire existe et est exécutable dans l'un des répertoires donnés.
pub fn find_executable_in_dirs(binary_name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    for dir in dirs {
        let candidate = dir.join(binary_name);
        if let Ok(meta) = std::fs::metadata(&candidate) {
            if meta.is_file() && (meta.permissions().mode() & 0o111 != 0) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Détecte automatiquement l'assistant AUR dans une liste de dossiers (`paru` en priorité, sinon `yay`).
pub fn detect_aur_helper_in_dirs(dirs: &[PathBuf]) -> Option<AurHelper> {
    if find_executable_in_dirs(AurHelper::Paru.binary_name(), dirs).is_some() {
        return Some(AurHelper::Paru);
    }
    if find_executable_in_dirs(AurHelper::Yay.binary_name(), dirs).is_some() {
        return Some(AurHelper::Yay);
    }
    None
}

/// Détecte les mises à jour AUR disponibles via `yay -Qua` (ou `paru -Qua`).
/// Renvoie une table `nom_du_paquet -> (ancienne_version, nouvelle_version)`.
pub async fn check_aur_updates_via_helper() -> AppResult<HashMap<String, (String, String)>> {
    let Some(helper) = detect_aur_helper() else {
        return Ok(HashMap::new());
    };
    let bin = helper.binary_name();

    let output = Command::new(bin)
        .env("LC_ALL", "C")
        .arg("-Qua")
        .output()
        .await?;

    let code = output.status.code().unwrap_or(1);
    // `yay -Qua` renvoie 0 quand des mises à jour AUR existent, et 1 (sortie vide) quand il n'y en a aucune.
    if output.status.success() || code == 1 {
        let stdout = String::from_utf8(output.stdout)?;
        Ok(crate::backend::pacman::parse_checkupdates_output(&stdout))
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(AppError::AurHelperError(format!(
            "Échec de `{bin} -Qua` : {stderr}"
        )))
    }
}

/// Détecte automatiquement l'assistant AUR disponible sur le système (`paru`, puis fallback `yay`).
pub fn detect_aur_helper() -> Option<AurHelper> {
    let mut search_dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).collect())
        .unwrap_or_default();

    for standard in ["/usr/bin", "/usr/local/bin", "/bin"] {
        let p = PathBuf::from(standard);
        if !search_dirs.contains(&p) {
            search_dirs.push(p);
        }
    }

    detect_aur_helper_in_dirs(&search_dirs)
}

#[derive(Debug, Clone)]
pub struct AurClient {
    http: Client,
}

impl AurClient {
    pub fn new() -> AppResult<Self> {
        let http = Client::builder()
            .user_agent("noos-package-manager/0.1.0 (Arch Linux; GTK4/Libadwaita)")
            .timeout(Duration::from_secs(15))
            .connect_timeout(Duration::from_secs(8))
            .build()?;
        Ok(Self { http })
    }

    /// Recherche de paquets sur l'API AUR RPC v5 : `https://aur.archlinux.org/rpc/v5/search/{terme}`
    pub async fn search(
        &self,
        term: &str,
        installed_versions: &HashMap<String, String>,
    ) -> AppResult<Vec<PackageItem>> {
        let trimmed = term.trim();
        if trimmed.len() < 2 {
            return Ok(Vec::new());
        }

        let url = build_search_url(trimmed);
        let response = self.http.get(&url).send().await?.error_for_status()?;
        let body = response.text().await?;
        parse_aur_rpc_json(&body, installed_versions)
    }

    /// Récupération des détails d'un ou plusieurs paquets via `https://aur.archlinux.org/rpc/v5/info?arg[]={pkg}`
    pub async fn info(
        &self,
        packages: &[&str],
        installed_versions: &HashMap<String, String>,
    ) -> AppResult<Vec<PackageItem>> {
        if packages.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_items = Vec::new();
        for chunk in packages.chunks(100) {
            let url = build_info_url(chunk);
            let response = self.http.get(&url).send().await?.error_for_status()?;
            let body = response.text().await?;
            let mut chunk_items = parse_aur_rpc_json(&body, installed_versions)?;
            all_items.append(&mut chunk_items);
        }

        all_items.sort_by(|a, b| AurSortMode::Popularity.compare_items(a, b));
        Ok(all_items)
    }

    /// Récupère les métadonnées AUR complètes et les mises à jour pour une liste de paquets.
    pub async fn fetch_enriched_aur_items(
        &self,
        package_names: &[&str],
        installed_versions: &HashMap<String, String>,
    ) -> AppResult<Vec<PackageItem>> {
        let mut items = self.info(package_names, installed_versions).await?;
        for item in &mut items {
            if let Some(ref candidate_ver) = item.update_available {
                if !is_version_newer(&item.version, candidate_ver).await {
                    item.update_available = None;
                }
            }
        }
        Ok(items)
    }

    /// Vérifie les mises à jour disponibles sur AUR pour les paquets étrangers installés.
    pub async fn check_foreign_updates(
        &self,
        foreign_packages: &[String],
        installed_versions: &HashMap<String, String>,
    ) -> AppResult<HashMap<String, String>> {
        if foreign_packages.is_empty() {
            return Ok(HashMap::new());
        }

        let refs: Vec<&str> = foreign_packages.iter().map(String::as_str).collect();
        let remote_items = self.info(&refs, installed_versions).await?;

        let mut updates = HashMap::new();
        for item in remote_items {
            if let Some(new_ver) = item.update_available {
                if is_version_newer(&item.version, &new_ver).await {
                    updates.insert(item.name, new_ver);
                }
            }
        }

        Ok(updates)
    }

    /// Résout les dépendances déclarées sur l'API AUR RPC v5 (`Depends` + `MakeDepends`) pour un lot de paquets AUR.
    pub async fn resolve_aur_dependencies(
        &self,
        targets: &[String],
        installed_versions: &HashMap<String, String>,
    ) -> AppResult<Vec<DependencyStepItem>> {
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        let refs: Vec<&str> = targets.iter().map(String::as_str).collect();
        let url = build_info_url(&refs);
        let response = self.http.get(&url).send().await?.error_for_status()?;
        let body = response.text().await?;
        let parsed: AurRpcResponse = serde_json::from_str(&body)?;

        let mut steps = Vec::new();
        let mut seen = HashSet::new();

        for pkg in &parsed.results {
            for dep_raw in pkg.depends.iter().chain(pkg.make_depends.iter()) {
                let dep_clean = dep_raw
                    .split(['>', '<', '=', ':'])
                    .next()
                    .unwrap_or(dep_raw)
                    .trim();
                if !dep_clean.is_empty()
                    && !installed_versions.contains_key(dep_clean)
                    && seen.insert(dep_clean.to_string())
                {
                    steps.push(DependencyStepItem {
                        name: dep_clean.to_string(),
                        version: String::from("dépendance"),
                        size_bytes: 0,
                        is_explicit_target: false,
                        source: PackageSource::Aur,
                        status: DependencyStatus::Pending,
                    });
                }
            }

            if seen.insert(pkg.name.clone()) {
                steps.push(DependencyStepItem {
                    name: pkg.name.clone(),
                    version: pkg.version.clone(),
                    size_bytes: 0,
                    is_explicit_target: true,
                    source: PackageSource::Aur,
                    status: DependencyStatus::Pending,
                });
            }
        }

        Ok(steps)
    }
}

/// Compare deux versions de paquets Arch Linux via `vercmp` (ou fallback lexicographique).
async fn is_version_newer(installed: &str, candidate: &str) -> bool {
    if installed == candidate {
        return false;
    }

    if let Ok(output) = Command::new("vercmp")
        .arg(candidate)
        .arg(installed)
        .output()
        .await
    {
        if output.status.success() {
            if let Ok(stdout) = String::from_utf8(output.stdout) {
                if let Ok(cmp_val) = stdout.trim().parse::<i32>() {
                    return cmp_val > 0;
                }
            }
        }
    }

    candidate != installed
}

/// Installe ou met à jour un paquet AUR via l'assistant détecté (`paru` ou `yay`).
pub async fn install_or_update_aur_package(pkg_name: &str) -> AppResult<String> {
    let helper = detect_aur_helper().ok_or(AppError::AurHelperNotFound)?;
    let bin = helper.binary_name();

    let mut cmd = Command::new(bin);
    cmd.env("LC_ALL", "C")
        .env("PACMAN_AUTH", "pkexec")
        .stdin(Stdio::null())
        .args(["-S", "--aur", "--noconfirm", "--needed", "--sudo", "pkexec"]);
    if matches!(helper, AurHelper::Yay) {
        cmd.args([
            "--answerclean",
            "None",
            "--answerdiff",
            "None",
            "--answeredit",
            "None",
            "--removemake",
            "no",
        ]);
    }
    cmd.arg(pkg_name);

    let output = cmd.output().await?;

    if output.status.success() {
        Ok(format!(
            "Paquet AUR « {pkg_name} » installé/mis à jour via {}.",
            helper.display_name()
        ))
    } else {
        // Repli sur lancement dans un terminal interactif si pkexec/noconfirm échoue sur une interaction GPG/PKGBUILD
        if try_spawn_terminal_aur_command(bin, pkg_name).is_ok() {
            return Ok(format!(
                "Installation de « {pkg_name} » ouverte dans le terminal avec {}.",
                helper.display_name()
            ));
        }

        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        Err(AppError::AurHelperError(format!(
            "Échec de {} pour « {pkg_name} » : {detail}",
            helper.display_name()
        )))
    }
}

fn try_spawn_terminal_aur_command(helper_bin: &str, pkg_name: &str) -> AppResult<()> {
    let cmd_line = format!(
        "{helper_bin} -S {pkg_name}; echo ''; echo 'Appuyez sur Entrée pour fermer...'; read _"
    );

    let terminals: [(&str, &[&str]); 7] = [
        ("kgx", &["-e", "sh", "-c"]),
        ("gnome-terminal", &["--", "sh", "-c"]),
        ("konsole", &["-e", "sh", "-c"]),
        ("xfce4-terminal", &["-e", "sh", "-c"]),
        ("alacritty", &["-e", "sh", "-c"]),
        ("kitty", &["sh", "-c"]),
        ("xterm", &["-e", "sh", "-c"]),
    ];

    let path_dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|p| env::split_paths(&p).collect())
        .unwrap_or_default();

    for (term_bin, args_prefix) in terminals {
        if find_executable_in_dirs(term_bin, &path_dirs).is_some()
            || Path::new(&format!("/usr/bin/{term_bin}")).exists()
        {
            let mut cmd = std::process::Command::new(term_bin);
            for arg in args_prefix {
                cmd.arg(arg);
            }
            cmd.arg(&cmd_line);
            if cmd.spawn().is_ok() {
                return Ok(());
            }
        }
    }

    Err(AppError::AurHelperError(String::from(
        "Aucun terminal graphique disponible pour l'installation interactive",
    )))
}

/// Installe un lot de paquets AUR via `paru` ou `yay` en diffusant les journaux et étapes de progression.
pub async fn install_aur_packages_streaming(
    targets: &[String],
    known_deps: &[String],
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> AppResult<String> {
    if targets.is_empty() {
        return Ok(String::from("Aucun paquet AUR à installer."));
    }
    if cancel_handle.is_cancelled() {
        return Err(AppError::AurHelperError(String::from(
            "Installation annulée par l'utilisateur.",
        )));
    }

    let helper = detect_aur_helper().ok_or(AppError::AurHelperNotFound)?;
    let bin = helper.binary_name();

    crate::backend::pacman::ensure_stale_db_lock_removed(ui_tx).await;

    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: format!(
                "Démarrage de {} pour installer {} paquet(s) AUR…",
                helper.display_name(),
                targets.len()
            ),
            fraction: 0.10,
            active_package: targets
                .first()
                .map(|n| (n.clone(), DependencyStatus::Downloading)),
            log_line: Some(format!(
                "$ {bin} -S --aur --noconfirm --sudo pkexec {}",
                targets.join(" ")
            )),
        })
        .await;

    let mut cmd = Command::new(bin);
    cmd.env("LC_ALL", "C")
        .env("PACMAN_AUTH", "pkexec")
        .stdin(Stdio::null())
        .args(["-S", "--aur", "--noconfirm", "--sudo", "pkexec"]);
    if matches!(helper, AurHelper::Yay) {
        cmd.args([
            "--answerclean",
            "None",
            "--answerdiff",
            "None",
            "--answeredit",
            "None",
            "--removemake",
            "no",
        ]);
    }
    cmd.args(targets)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = cmd.spawn()?;

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
                            phase: String::from("Installation AUR annulée par l'utilisateur."),
                            fraction: -1.0,
                            active_package: None,
                            log_line: Some(String::from("[Annulé] Transaction AUR interrompue à la demande de l'utilisateur.")),
                        })
                        .await;
                    return Err(AppError::AurHelperError(String::from(
                        "Installation annulée par l'utilisateur.",
                    )));
                }
                res = reader.next_line() => res.ok().flatten(),
            };

            let Some(line) = next_line_opt else {
                break;
            };

            if let Some((phase, active_pkg)) =
                crate::backend::pacman::parse_pacman_progress_line(&line, known_deps)
            {
                if active_pkg.is_some() {
                    completed_steps = (completed_steps + 1.0).min(total_steps);
                }
                let frac = (0.12 + 0.82 * (completed_steps / total_steps)).clamp(0.12, 0.94);
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
            return Err(AppError::AurHelperError(String::from(
                "Installation annulée par l'utilisateur.",
            )));
        }
        st = child.wait() => st?,
    };

    if status.success() {
        Ok(format!(
            "{} paquet(s) AUR installés/mis à jour avec succès via {}.",
            targets.len(),
            helper.display_name()
        ))
    } else {
        let detail = if stderr_buf.trim().is_empty() {
            format!("Code de sortie {}", status.code().unwrap_or(1))
        } else {
            stderr_buf.trim().to_string()
        };
        Err(AppError::AurHelperError(detail))
    }
}
