use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use tempfile::TempDir;
use tokio::process::Command;

use crate::backend::models::InstallCancelHandle;
use crate::error::{AppError, AppResult};
use crate::ui::messages::UiEvent;

/// Demande adressée à l'interface graphique lorsqu'un programme interactif
/// (Yay, makepkg ou un PKGBUILD) attend une réponse.
#[derive(Debug, Clone)]
pub enum YayPromptKind {
    /// Menu numéroté de Yay (choix d'un fournisseur de dépendance).
    Choice {
        title: String,
        /// Libellés affichés dans la liste déroulante.
        options: Vec<String>,
        /// Réponse à envoyer pour chaque libellé (le numéro du menu Yay).
        answers: Vec<String>,
        /// Index sélectionné par défaut.
        default: usize,
    },
    /// Question fermée de type `[Y/n]`.
    Confirm { title: String, default: bool },
    /// Question libre (PKGBUILD utilisant `read`).
    Text { title: String, default: String },
}

#[derive(Debug, Clone)]
pub struct YayPromptAnswer {
    pub request_id: u64,
    pub cancelled: bool,
    pub value: Option<String>,
}

struct PromptChannel {
    tx: async_channel::Sender<YayPromptAnswer>,
    rx: async_channel::Receiver<YayPromptAnswer>,
}

static PROMPT_CHANNEL: OnceLock<PromptChannel> = OnceLock::new();
static NEXT_PROMPT_ID: AtomicU64 = AtomicU64::new(1);

const TAIL_LIMIT: usize = 24;
/// Délai sans nouvelle sortie après lequel une ligne non terminée est traitée
/// comme un prompt (beaucoup de programmes n'ajoutent pas de saut de ligne).
const IDLE_PROMPT_DELAY: Duration = Duration::from_millis(350);

fn prompt_channel() -> &'static PromptChannel {
    PROMPT_CHANNEL.get_or_init(|| {
        let (tx, rx) = async_channel::unbounded();
        PromptChannel { tx, rx }
    })
}

/// Réponse envoyée par l'interface graphique au backend bloqué sur un prompt Yay.
pub fn submit_prompt_answer(answer: YayPromptAnswer) {
    let _ = prompt_channel().tx.try_send(answer);
}

async fn request_prompt(
    ui_tx: &async_channel::Sender<UiEvent>,
    cancel_handle: &InstallCancelHandle,
    prompt: YayPromptKind,
) -> AppResult<String> {
    let request_id = NEXT_PROMPT_ID.fetch_add(1, Ordering::Relaxed);
    let _ = ui_tx
        .send(UiEvent::YayPromptRequired { request_id, prompt })
        .await;

    loop {
        tokio::select! {
            _ = cancel_handle.notified() => {
                return Err(AppError::AurHelperError(String::from(
                    "Opération annulée pendant l'attente d'un choix Yay.",
                )));
            }
            answer = prompt_channel().rx.recv() => {
                match answer {
                    Ok(answer) if answer.request_id == request_id => {
                        if answer.cancelled {
                            return Err(AppError::AurHelperError(String::from(
                                "Choix Yay annulé par l'utilisateur.",
                            )));
                        }
                        return Ok(answer.value.unwrap_or_default());
                    }
                    Ok(_) => continue,
                    Err(_) => {
                        return Err(AppError::AurHelperError(String::from(
                            "Canal de réponse Yay fermé.",
                        )));
                    }
                }
            }
        }
    }
}

/// Wrapper Pacman qui ajoute `--noconfirm` pour éviter les confirmations système
/// pendant que Yay reste interactif pour les choix de build.
pub struct PacmanNoconfirmWrapper {
    _dir: TempDir,
    path: PathBuf,
}

impl PacmanNoconfirmWrapper {
    pub fn new() -> AppResult<Self> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("pacman");
        fs::write(
            &path,
            "#!/bin/sh\nexec /usr/bin/pacman --noconfirm \"$@\"\n",
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(Self { _dir: dir, path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Spécification d'une commande Yay à lancer dans un PTY.
pub struct PtyCommandSpec {
    program: OsString,
    args: Vec<OsString>,
    envs: Vec<(OsString, Option<OsString>)>,
}

impl PtyCommandSpec {
    pub fn from_command(command: &Command) -> Self {
        let std_command = command.as_std();
        Self {
            program: std_command.get_program().to_os_string(),
            args: std_command
                .get_args()
                .map(|arg| arg.to_os_string())
                .collect(),
            envs: std_command
                .get_envs()
                .map(|(key, value)| (key.to_os_string(), value.map(|value| value.to_os_string())))
                .collect(),
        }
    }
}

/// Menu numéroté détecté dans la sortie de Yay.
struct NumberedMenu {
    title: String,
    repository: Option<String>,
    labels: Vec<String>,
    answers: Vec<String>,
    default_number: Option<u32>,
}

impl NumberedMenu {
    fn new(title: &str, default_number: Option<u32>) -> Self {
        Self {
            title: title.to_string(),
            repository: None,
            labels: Vec::new(),
            answers: Vec::new(),
            default_number,
        }
    }

    fn push_options(&mut self, line: &str) {
        for (number, name) in parse_numbered_options(line) {
            let label = match &self.repository {
                Some(repository) => format!("{name} ({repository})"),
                None => name,
            };
            self.labels.push(label);
            self.answers.push(number.to_string());
        }
    }

    fn default_index(&self) -> usize {
        let Some(default_number) = self.default_number else {
            return 0;
        };
        self.answers
            .iter()
            .position(|answer| *answer == default_number.to_string())
            .unwrap_or(0)
    }
}

struct InteractionState {
    menu: Option<NumberedMenu>,
    tail: VecDeque<String>,
    total_steps: f64,
    completed_steps: f64,
}

/// Lance Yay dans un pseudo-terminal et affiche ses prompts de build dans l'UI.
pub async fn run_interactive_yay(
    spec: PtyCommandSpec,
    success_message: String,
    known_deps: &[String],
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) -> AppResult<String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 50,
            cols: 220,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|err| AppError::AurHelperError(format!("PTY Yay : {err}")))?;

    let mut builder = CommandBuilder::new(&spec.program);
    builder.args(spec.args);
    for (key, value) in spec.envs {
        match value {
            Some(value) => {
                builder.env(key, value);
            }
            None => builder.env_remove(key),
        }
    }
    builder.env("LC_ALL", "C");
    builder.env("NO_COLOR", "1");
    builder.env("TERM", "xterm-256color");

    let mut child = pair
        .slave
        .spawn_command(builder)
        .map_err(|err| AppError::AurHelperError(format!("Lancement Yay PTY : {err}")))?;
    drop(pair.slave);

    let mut reader = pair
        .master
        .try_clone_reader()
        .map_err(|err| AppError::AurHelperError(format!("Lecture PTY Yay : {err}")))?;
    let mut writer = pair
        .master
        .take_writer()
        .map_err(|err| AppError::AurHelperError(format!("Écriture PTY Yay : {err}")))?;

    let (chunk_tx, chunk_rx) = async_channel::unbounded::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 4096];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    if chunk_tx.send_blocking(buffer[..count].to_vec()).is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    let mut state = InteractionState {
        menu: None,
        tail: VecDeque::with_capacity(TAIL_LIMIT),
        total_steps: (known_deps.len().max(1) * 2) as f64,
        completed_steps: 0.0,
    };
    let mut line_buffer: Vec<u8> = Vec::new();

    let run_result: AppResult<()> = 'run: loop {
        let mut idle = Box::pin(tokio::time::sleep(IDLE_PROMPT_DELAY));

        tokio::select! {
            _ = cancel_handle.notified() => {
                break Err(AppError::AurHelperError(String::from(
                    "Opération annulée par l'utilisateur.",
                )));
            }
            chunk = chunk_rx.recv() => {
                let Ok(bytes) = chunk else { break Ok(()) };
                for byte in bytes {
                    if byte == b'\n' || byte == b'\r' {
                        if !line_buffer.is_empty() {
                            let line = String::from_utf8_lossy(&line_buffer).into_owned();
                            line_buffer.clear();
                            if let Err(err) = process_line(
                                &line,
                                &mut state,
                                writer.as_mut(),
                                ui_tx,
                                cancel_handle,
                                known_deps,
                            )
                            .await
                            {
                                break 'run Err(err);
                            }
                        }
                    } else {
                        line_buffer.push(byte);
                    }
                }
            }
            _ = &mut idle, if !line_buffer.is_empty() => {
                let line = String::from_utf8_lossy(&line_buffer).into_owned();
                line_buffer.clear();
                if let Err(err) = process_line(
                    &line,
                    &mut state,
                    writer.as_mut(),
                    ui_tx,
                    cancel_handle,
                    known_deps,
                )
                .await
                {
                    break 'run Err(err);
                }
            }
        }
    };

    if let Err(err) = run_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(err);
    }

    let status = child
        .wait()
        .map_err(|err| AppError::AurHelperError(format!("Attente Yay : {err}")))?;

    if status.success() {
        Ok(success_message)
    } else {
        let detail = if state.tail.is_empty() {
            format!("code de sortie {}", status.exit_code())
        } else {
            state.tail.into_iter().collect::<Vec<_>>().join("\n")
        };
        Err(AppError::AurHelperError(detail))
    }
}

async fn process_line(
    raw_line: &str,
    state: &mut InteractionState,
    writer: &mut (dyn Write + Send),
    ui_tx: &async_channel::Sender<UiEvent>,
    cancel_handle: &InstallCancelHandle,
    known_deps: &[String],
) -> AppResult<()> {
    let line = strip_ansi(raw_line).trim().to_string();
    if line.is_empty() {
        return Ok(());
    }

    // Pendant un menu numéroté, on collecte les options sans les traiter comme
    // des prompts génériques.
    if state.menu.is_some() {
        send_log(ui_tx, &line).await;

        if let Some(repository) = parse_repository_line(&line) {
            if let Some(menu) = state.menu.as_mut() {
                menu.repository = Some(repository);
            }
            return Ok(());
        }

        if is_number_prompt(&line) {
            if let Some(mut menu) = state.menu.take() {
                if let Some(default_number) = parse_default_number(&line) {
                    menu.default_number = Some(default_number);
                }
                if !menu.labels.is_empty() {
                    let default = menu.default_index();
                    ask_and_answer(
                        YayPromptKind::Choice {
                            title: provider_menu_title(&menu.title),
                            options: menu.labels,
                            answers: menu.answers,
                            default,
                        },
                        writer,
                        ui_tx,
                        cancel_handle,
                    )
                    .await?;
                }
            }
            return Ok(());
        }

        if let Some(menu) = state.menu.as_mut() {
            menu.push_options(&line);
        }
        return Ok(());
    }

    push_tail(state, &line);

    if is_menu_header(&line) {
        send_log(ui_tx, &line).await;
        let default_number = parse_default_number(&line);
        state.menu = Some(NumberedMenu::new(&line, default_number));
        return Ok(());
    }

    let mut logged = false;
    if let Some((phase, active_package)) =
        crate::backend::pacman::parse_pacman_progress_line(&line, known_deps)
    {
        logged = true;
        if active_package.is_some() {
            state.completed_steps = (state.completed_steps + 1.0).min(state.total_steps);
        }
        let fraction =
            (0.12 + 0.82 * (state.completed_steps / state.total_steps)).clamp(0.12, 0.94);
        let _ = ui_tx
            .send(UiEvent::InstallProgress {
                phase,
                fraction,
                active_package,
                log_line: Some(line.clone()),
            })
            .await;
    }

    if !logged {
        send_log(ui_tx, &line).await;
    }

    if let Some(prompt) = detect_prompt(&line) {
        ask_and_answer(prompt, writer, ui_tx, cancel_handle).await?;
    }

    Ok(())
}

fn push_tail(state: &mut InteractionState, line: &str) {
    if state.tail.len() >= TAIL_LIMIT {
        state.tail.pop_front();
    }
    state.tail.push_back(line.to_string());
}

async fn ask_and_answer(
    prompt: YayPromptKind,
    writer: &mut (dyn Write + Send),
    ui_tx: &async_channel::Sender<UiEvent>,
    cancel_handle: &InstallCancelHandle,
) -> AppResult<()> {
    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: String::from("En attente de votre choix pour la compilation AUR…"),
            fraction: -1.0,
            active_package: None,
            log_line: None,
        })
        .await;

    let answer = request_prompt(ui_tx, cancel_handle, prompt).await?;
    writer
        .write_all(answer.as_bytes())
        .and_then(|_| writer.write_all(b"\n"))
        .and_then(|_| writer.flush())
        .map_err(|err| AppError::AurHelperError(format!("Réponse PTY Yay : {err}")))?;
    Ok(())
}

async fn send_log(ui_tx: &async_channel::Sender<UiEvent>, line: &str) {
    let _ = ui_tx
        .send(UiEvent::InstallProgress {
            phase: String::new(),
            fraction: -1.0,
            active_package: None,
            log_line: Some(line.to_string()),
        })
        .await;
}

/// Traduit l'en-tête anglais de Yay (`There are N providers available for X:`)
/// en une phrase lisible pour la fenêtre de choix.
fn provider_menu_title(raw: &str) -> String {
    let marker = "providers available for";
    if let Some(position) = raw.find(marker) {
        let dependency = raw[position + marker.len()..]
            .trim()
            .trim_end_matches(':')
            .trim();
        if !dependency.is_empty() {
            return format!("Plusieurs paquets fournissent « {dependency} ».");
        }
    }
    raw.trim().to_string()
}

fn is_menu_header(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("providers available for") || lower.contains("packages in group")
}

fn is_number_prompt(line: &str) -> bool {
    line.to_ascii_lowercase().contains("enter a number")
}

fn parse_repository_line(line: &str) -> Option<String> {
    let trimmed = line.trim_start_matches(|ch: char| !ch.is_alphanumeric());
    let repository = trimmed.strip_prefix("Repository ")?.trim();
    if repository.is_empty() {
        None
    } else {
        Some(repository.to_string())
    }
}

fn parse_default_number(line: &str) -> Option<u32> {
    let lower = line.to_ascii_lowercase();
    let start = lower.find("default=")? + "default=".len();
    let digits: String = lower[start..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn detect_prompt(line: &str) -> Option<YayPromptKind> {
    let lower = line.to_ascii_lowercase();
    if lower.contains("[y/n]") || lower.contains("(y/n)") || lower.contains("yes/no") {
        return Some(YayPromptKind::Confirm {
            title: line.to_string(),
            default: !line.contains("[y/N]"),
        });
    }

    let trimmed = line.trim_end();
    if trimmed.ends_with('?') && trimmed.chars().count() < 240 {
        return Some(YayPromptKind::Text {
            title: trimmed.to_string(),
            default: String::new(),
        });
    }

    None
}

fn parse_numbered_options(line: &str) -> Vec<(u32, String)> {
    let mut options = Vec::new();
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let mut index = 0;
    while index < tokens.len() {
        let token = tokens[index];
        if let Some(number) = token.strip_suffix(')') {
            if let Ok(number) = number.parse::<u32>() {
                if let Some(name) = tokens.get(index + 1) {
                    let name = name.trim_matches(|ch: char| ch == ',' || ch == ';');
                    if !name.is_empty() {
                        options.push((number, name.to_string()));
                    }
                    index += 1;
                }
            }
        }
        index += 1;
    }
    options
}

fn strip_ansi(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\u{1b}' {
            output.push(ch);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' && chars.peek() == Some(&'\\') {
                        chars.next();
                        break;
                    }
                }
            }
            Some('(') | Some(')') | Some('#') | Some('%') => {
                // Séquences de deux caractères (jeux de caractères, etc.).
                chars.next();
                chars.next();
            }
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_options() {
        let options = parse_numbered_options("1) libgtk3 2) libgtk4 3) libadwaita");
        assert_eq!(
            options,
            vec![
                (1, String::from("libgtk3")),
                (2, String::from("libgtk4")),
                (3, String::from("libadwaita")),
            ]
        );
    }

    #[test]
    fn strips_ansi_sequences() {
        assert_eq!(
            strip_ansi("\u{1b}[1;32mThere are 2 providers\u{1b}[0m"),
            "There are 2 providers"
        );
        assert_eq!(
            strip_ansi("\u{1b}(B\u{1b}[1m==>\u{1b}(B\u{1b}[m Making package"),
            "==> Making package"
        );
    }

    #[test]
    fn detects_confirm_prompt() {
        let prompt = detect_prompt("Build with GTK4? [Y/n]");
        assert!(matches!(
            prompt,
            Some(YayPromptKind::Confirm { default: true, .. })
        ));
    }

    #[test]
    fn detects_default_no_prompt() {
        let prompt = detect_prompt("Build with GTK4? [y/N]");
        assert!(matches!(
            prompt,
            Some(YayPromptKind::Confirm { default: false, .. })
        ));
    }

    #[test]
    fn localizes_provider_menu_title() {
        assert_eq!(
            provider_menu_title("There are 2 providers available for libfoo:"),
            "Plusieurs paquets fournissent « libfoo »."
        );
    }

    #[test]
    fn parses_repository_and_default() {
        assert_eq!(
            parse_repository_line(":: Repository extra"),
            Some(String::from("extra"))
        );
        assert_eq!(parse_default_number("Enter a number (default=2):"), Some(2));
    }

    #[tokio::test]
    async fn detects_real_yay_provider_menu_output() {
        let (ui_tx, ui_rx) = async_channel::unbounded();
        let cancel = InstallCancelHandle::new();
        let mut state = InteractionState {
            menu: None,
            tail: VecDeque::new(),
            total_steps: 1.0,
            completed_steps: 0.0,
        };
        let mut writer: Vec<u8> = Vec::new();

        for line in [
            ":: \u{1b}[1mThere are 2 providers available for otf-font:",
            ":: \u{1b}[1mRepository AUR",
            "    \u{1b}[0m1) otf-clarity-city 2) otf-impallari-miltonian \u{1b}[0m",
        ] {
            process_line(line, &mut state, &mut writer, &ui_tx, &cancel, &[])
                .await
                .unwrap();
        }

        let prompt_line = "Enter a number (default=1): ";
        let mut answered = false;
        {
            let prompt = process_line(prompt_line, &mut state, &mut writer, &ui_tx, &cancel, &[]);
            tokio::pin!(prompt);

            loop {
                tokio::select! {
                    result = &mut prompt => {
                        result.unwrap();
                        break;
                    }
                    event = ui_rx.recv() => {
                        if let Ok(UiEvent::YayPromptRequired {
                            request_id,
                            prompt:
                                YayPromptKind::Choice {
                                    title,
                                    options,
                                    answers,
                                    default,
                                },
                        }) = event
                        {
                            assert_eq!(title, "Plusieurs paquets fournissent « otf-font ».");
                            assert_eq!(
                                options,
                                vec!["otf-clarity-city (AUR)", "otf-impallari-miltonian (AUR)"]
                            );
                            assert_eq!(answers, vec!["1", "2"]);
                            assert_eq!(default, 0);
                            submit_prompt_answer(YayPromptAnswer {
                                request_id,
                                cancelled: false,
                                value: Some(String::from("2")),
                            });
                            answered = true;
                        }
                    }
                }
            }
        }

        assert!(answered, "le menu fournisseur réel n'a pas été détecté");
        assert_eq!(writer, b"2\n");
    }

    #[tokio::test]
    async fn interactive_provider_prompt_roundtrip() {
        let (ui_tx, ui_rx) = async_channel::unbounded();
        let cancel = std::sync::Arc::new(InstallCancelHandle::new());

        let mut command = Command::new("sh");
        command.arg("-c").arg(
            "printf 'There are 2 providers available for libfoo:\\n:: Repository AUR\\n    1) libgtk3 2) libgtk4\\nEnter a number (default=1):\\n'; read answer; echo \"CHOSEN:$answer\"",
        );
        let spec = PtyCommandSpec::from_command(&command);

        let task = {
            let cancel = std::sync::Arc::clone(&cancel);
            tokio::spawn(async move {
                run_interactive_yay(spec, String::from("ok"), &[], &cancel, &ui_tx).await
            })
        };

        let mut answered = false;
        while let Ok(event) = ui_rx.recv().await {
            if let UiEvent::YayPromptRequired { request_id, prompt } = event {
                match prompt {
                    YayPromptKind::Choice {
                        options,
                        answers,
                        default,
                        ..
                    } => {
                        assert_eq!(options, vec!["libgtk3 (AUR)", "libgtk4 (AUR)"]);
                        assert_eq!(answers, vec!["1", "2"]);
                        assert_eq!(default, 0);
                        submit_prompt_answer(YayPromptAnswer {
                            request_id,
                            cancelled: false,
                            value: Some(String::from("2")),
                        });
                        answered = true;
                        break;
                    }
                    _ => panic!("prompt Choice attendu"),
                }
            }
        }

        assert!(answered, "aucun prompt Choice reçu");
        let result = task.await.expect("tâche PTY");
        assert!(result.is_ok(), "échec PTY : {result:?}");
    }

    #[tokio::test]
    async fn interactive_pkgbuild_confirm_prompt() {
        let (ui_tx, ui_rx) = async_channel::unbounded();
        let cancel = std::sync::Arc::new(InstallCancelHandle::new());

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg("printf 'Build with GTK4? [Y/n] '; read answer; echo \"CHOSEN:$answer\"");
        let spec = PtyCommandSpec::from_command(&command);

        let task = {
            let cancel = std::sync::Arc::clone(&cancel);
            tokio::spawn(async move {
                run_interactive_yay(spec, String::from("ok"), &[], &cancel, &ui_tx).await
            })
        };

        let mut answered = false;
        let receive = async {
            while let Ok(event) = ui_rx.recv().await {
                if let UiEvent::YayPromptRequired { request_id, prompt } = event {
                    if matches!(prompt, YayPromptKind::Confirm { .. }) {
                        submit_prompt_answer(YayPromptAnswer {
                            request_id,
                            cancelled: false,
                            value: Some(String::from("n")),
                        });
                        answered = true;
                        break;
                    }
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(5), receive)
            .await
            .expect("prompt Confirm non détecté");

        assert!(answered);
        let result = task.await.expect("tâche PTY");
        assert!(result.is_ok(), "échec PTY : {result:?}");
    }
}
