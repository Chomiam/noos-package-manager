use std::env;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use tokio::process::Command;

use crate::backend::logging::log_warn;
use crate::backend::pacman::{build_pkexec_command, has_no_new_privileges};
use crate::error::{AppError, AppResult};

/// Variable d'environnement qui active le mode « assistant askpass » du binaire principal.
pub const ASKPASS_MODE_ENV: &str = "NOOS_ASKPASS_MODE";
/// Répertoire de session transmis à l'assistant askpass (aucun secret n'y est stocké).
pub const ASKPASS_SESSION_ENV: &str = "NOOS_ASKPASS_SESSION_DIR";

const ASKPASS_DIR_PREFIX: &str = "noos-askpass-";

/// Mécanisme d'élévation de privilèges utilisé pour une opération.
///
/// `Sudo` regroupe toute l'opération derrière une seule authentification : `yay`
/// est lancé avec `--sudoloop`, ce qui valide sudo une fois puis rafraîchit le ticket
/// pendant toute la transaction. `Pkexec` reste le repli lorsque `sudo` est absent.
pub enum Elevation {
    Root,
    Sudo(SudoSession),
    Pkexec,
}

impl Elevation {
    pub fn detect() -> Self {
        if is_running_as_root() {
            return Self::Root;
        }

        if let Some(sudo_exe) = find_command("sudo") {
            if sudo_is_usable(&sudo_exe) {
                match SudoSession::new(sudo_exe) {
                    Ok(session) => return Self::Sudo(session),
                    Err(e) => {
                        log_warn(
                            "elevation",
                            format!("Session sudo indisponible, repli sur pkexec : {e}"),
                        );
                    }
                }
            } else {
                log_warn(
                    "elevation",
                    "sudo n'est pas autorisé pour cet utilisateur, repli sur pkexec",
                );
            }
        }

        Self::Pkexec
    }

    /// Construit une commande exécutée avec les privilèges administrateur.
    pub fn build_command(&self, program: &str) -> Command {
        match self {
            Self::Root => Command::new(program),
            Self::Sudo(session) => session.build_sudo_command(&[program]),
            Self::Pkexec => {
                let mut cmd = build_pkexec_command();
                cmd.arg(program);
                cmd
            }
        }
    }

    /// Construit une commande utilisateur (par exemple `yay`) qui doit pouvoir
    /// déclencher l'élévation pour ses appels internes à Pacman.
    pub fn user_command(&self, program: &str, args: &[&str]) -> Command {
        match self {
            Self::Root => {
                let mut cmd = Command::new(program);
                cmd.args(args);
                cmd
            }
            Self::Sudo(session) => session.build_user_command(program, args),
            Self::Pkexec => build_pkexec_user_command(program, args),
        }
    }

    /// Arguments à ajouter aux commandes `yay` pour qu'elles réutilisent la session sudo.
    pub fn yay_elevation_args(&self) -> Vec<String> {
        match self {
            Self::Sudo(session) => vec![
                String::from("--sudo"),
                session.wrapper_path.to_string_lossy().into_owned(),
                String::from("--sudoflags"),
                String::new(),
                String::from("--sudoloop"),
            ],
            _ => Vec::new(),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Root => "",
            Self::Sudo(_) => "sudo",
            Self::Pkexec => "pkexec",
        }
    }

    /// Indique si l'utilisateur a annulé la fenêtre d'authentification.
    pub fn authentication_was_cancelled(&self) -> bool {
        match self {
            Self::Sudo(session) => authentication_is_cancelled(session._dir.path()),
            _ => false,
        }
    }
}

/// Session d'élévation sudo partagée par une opération complète.
///
/// Contient uniquement un script wrapper (sans secret) qui configure `sudo -A`
/// pour utiliser l'assistant askpass graphique du binaire principal. Le mot de
/// passe n'est jamais stocké : sudo le garde uniquement dans son ticket temporaire.
pub struct SudoSession {
    _dir: TempDir,
    askpass_exe: PathBuf,
    sudo_exe: PathBuf,
    wrapper_path: PathBuf,
}

impl SudoSession {
    fn new(sudo_exe: PathBuf) -> AppResult<Self> {
        let base_dir = runtime_base_dir().ok_or_else(|| {
            AppError::PacmanError(String::from(
                "Impossible de localiser un répertoire temporaire pour l'authentification.",
            ))
        })?;

        cleanup_stale_askpass_dirs(&base_dir);

        let dir = tempfile::Builder::new()
            .prefix(ASKPASS_DIR_PREFIX)
            .tempdir_in(base_dir)?;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;

        let askpass_exe = env::current_exe()?;
        let wrapper_path = dir.path().join("sudo-askpass.sh");

        let script = format!(
            "#!/bin/sh\n\
             export {mode}=1\n\
             export {session}={session_dir}\n\
             export SUDO_ASKPASS={askpass}\n\
             export SUDO_ASKPASS_REQUIRE=force\n\
             if [ -f \"${{{session}}}/cancelled\" ]; then\n\
                 kill -TERM \"$PPID\" 2>/dev/null || true\n\
                 exit 1\n\
             fi\n\
             exec {sudo} -A \"$@\"\n",
            mode = ASKPASS_MODE_ENV,
            session = ASKPASS_SESSION_ENV,
            session_dir = shell_quote(dir.path()),
            askpass = shell_quote(&askpass_exe),
            sudo = shell_quote(&sudo_exe),
        );

        fs::write(&wrapper_path, script)?;
        fs::set_permissions(&wrapper_path, fs::Permissions::from_mode(0o700))?;

        Ok(Self {
            _dir: dir,
            askpass_exe,
            sudo_exe,
            wrapper_path,
        })
    }

    fn build_sudo_command(&self, args: &[&str]) -> Command {
        if should_wrap_with_systemd_run() {
            let mut cmd = systemd_run_base(&self.direct_env_pairs());
            cmd.arg(&self.sudo_exe).arg("-A").args(args);
            cmd
        } else {
            let mut cmd = Command::new(&self.sudo_exe);
            cmd.arg("-A").args(args);
            self.apply_direct_env(&mut cmd);
            cmd.env("LC_ALL", "C");
            cmd
        }
    }

    fn build_user_command(&self, program: &str, args: &[&str]) -> Command {
        if should_wrap_with_systemd_run() {
            let mut cmd = systemd_run_base(&[(String::from("PACMAN_AUTH"), String::new())]);
            cmd.arg(program).args(args);
            cmd
        } else {
            let mut cmd = Command::new(program);
            cmd.args(args)
                .env("LC_ALL", "C")
                .env_remove("PACMAN_AUTH")
                .env_remove("SUDO_ASKPASS")
                .env_remove("SUDO_ASKPASS_REQUIRE")
                .env_remove(ASKPASS_MODE_ENV)
                .env_remove(ASKPASS_SESSION_ENV);
            cmd
        }
    }

    fn apply_direct_env(&self, cmd: &mut Command) {
        cmd.env(ASKPASS_MODE_ENV, "1");
        cmd.env(ASKPASS_SESSION_ENV, self._dir.path());
        cmd.env("SUDO_ASKPASS", &self.askpass_exe);
        cmd.env("SUDO_ASKPASS_REQUIRE", "force");
        cmd.env_remove("PACMAN_AUTH");
    }

    fn direct_env_pairs(&self) -> Vec<(String, String)> {
        vec![
            (String::from(ASKPASS_MODE_ENV), String::from("1")),
            (
                String::from(ASKPASS_SESSION_ENV),
                self._dir.path().to_string_lossy().into_owned(),
            ),
            (
                String::from("SUDO_ASKPASS"),
                self.askpass_exe.to_string_lossy().into_owned(),
            ),
            (String::from("SUDO_ASKPASS_REQUIRE"), String::from("force")),
            (String::from("LC_ALL"), String::from("C")),
        ]
    }
}

fn build_pkexec_user_command(program: &str, args: &[&str]) -> Command {
    if should_wrap_with_systemd_run() {
        let mut cmd = systemd_run_base(&[(String::from("PACMAN_AUTH"), String::from("pkexec"))]);
        cmd.arg(program).args(args);
        cmd
    } else {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .env("LC_ALL", "C")
            .env("PACMAN_AUTH", "pkexec");
        cmd
    }
}

fn should_wrap_with_systemd_run() -> bool {
    has_no_new_privileges() && Path::new("/usr/bin/systemd-run").exists()
}

fn systemd_run_base(extra_env: &[(String, String)]) -> Command {
    let mut cmd = Command::new("systemd-run");
    cmd.args([
        "--user",
        "--pipe",
        "--wait",
        "--quiet",
        "--collect",
        "--setenv=LC_ALL=C",
    ]);

    for (key, value) in extra_env {
        if key == "LC_ALL" {
            continue;
        }
        cmd.arg(format!("--setenv={key}={value}"));
    }

    for var in [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "XAUTHORITY",
        "XDG_RUNTIME_DIR",
        "DBUS_SESSION_BUS_ADDRESS",
        "HOME",
        "USER",
        "PATH",
    ] {
        if let Ok(value) = env::var(var) {
            if !value.is_empty() {
                cmd.arg(format!("--setenv={var}={value}"));
            }
        }
    }

    cmd
}

fn runtime_base_dir() -> Option<PathBuf> {
    env::var_os("XDG_RUNTIME_DIR")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| Some(env::temp_dir()))
}

fn cleanup_stale_askpass_dirs(base_dir: &Path) {
    let Ok(entries) = fs::read_dir(base_dir) else {
        return;
    };
    let now = SystemTime::now();

    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.starts_with(ASKPASS_DIR_PREFIX) {
            continue;
        }
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Ok(modified) = metadata.modified() else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or_default();
        if age > Duration::from_secs(3600) {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

fn is_running_as_root() -> bool {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status
                .lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(2))
                .and_then(|value| value.parse::<u32>().ok())
        })
        == Some(0)
}

fn find_command(binary: &str) -> Option<PathBuf> {
    let mut search_dirs: Vec<PathBuf> = env::var_os("PATH")
        .map(|paths| env::split_paths(&paths).collect())
        .unwrap_or_default();

    for standard in ["/usr/bin", "/usr/local/bin", "/bin"] {
        let path = PathBuf::from(standard);
        if !search_dirs.contains(&path) {
            search_dirs.push(path);
        }
    }

    for dir in search_dirs {
        let candidate = dir.join(binary);
        if let Ok(metadata) = fs::metadata(&candidate) {
            if metadata.is_file() && metadata.permissions().mode() & 0o111 != 0 {
                return Some(candidate);
            }
        }
    }

    None
}

/// Vérifie, sans afficher de fenêtre, que sudo est utilisable par l'utilisateur courant.
///
/// `sudo -n -l` échoue avec un message contenant « password », « tty » ou « askpass »
/// lorsqu'une authentification est simplement requise (cas normal). Un message
/// « not in the sudoers file » indique en revanche que sudo ne pourra jamais aboutir :
/// on préfère alors le repli pkexec.
fn sudo_is_usable(sudo_exe: &Path) -> bool {
    let output = std::process::Command::new(sudo_exe)
        .env("LC_ALL", "C")
        .args(["-n", "-l"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).to_lowercase();
            stderr.contains("password")
                || stderr.contains("tty")
                || stderr.contains("askpass")
                || stderr.contains("terminal")
        }
        Err(_) => false,
    }
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Vérifie que le répertoire de session askpass fourni par le wrapper sudo est
/// bien un répertoire privé créé par l'application.
pub fn askpass_session_is_safe(session_dir: &Path) -> bool {
    let Some(base) = runtime_base_dir() else {
        return false;
    };
    let Some(name) = session_dir.file_name().and_then(|name| name.to_str()) else {
        return false;
    };

    session_dir.parent() == Some(base.as_path())
        && name.starts_with(ASKPASS_DIR_PREFIX)
        && fs::metadata(session_dir)
            .map(|meta| meta.is_dir() && meta.permissions().mode() & 0o077 == 0)
            .unwrap_or(false)
}

/// Indique que l'utilisateur a annulé la demande de mot de passe.
pub fn authentication_is_cancelled(session_dir: &Path) -> bool {
    session_dir.join("cancelled").exists()
}

/// Marque l'authentification comme annulée pour éviter que sudo ne réaffiche
/// plusieurs fenêtres après un clic sur « Annuler ».
pub fn mark_authentication_cancelled(session_dir: &Path) {
    let path = session_dir.join("cancelled");
    let _ = fs::write(&path, b"1");
    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
}
