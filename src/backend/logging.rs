use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// Niveau de sévérité d'une entrée de journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Une entrée du journal interne de l'application.
#[derive(Debug, Clone)]
pub struct LogEntry {
    pub timestamp_secs: u64,
    pub level: LogLevel,
    pub target: String,
    pub message: String,
}

/// Capacité maximale du journal en mémoire (les entrées les plus anciennes sont évincées).
const MAX_LOG_ENTRIES: usize = 500;

/// Chemin du fichier de journal persistant (optionnel, dans le cache utilisateur).
fn log_file_path() -> Option<PathBuf> {
    let base_dir = std::env::var_os("XDG_CACHE_HOME")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .filter(|s| !s.is_empty())
                .map(|h| PathBuf::from(h).join(".cache"))
        })?;

    Some(base_dir.join("noos-package-manager").join("noos.log"))
}

/// Journal en mémoire partagé entre tous les modules backend.
/// Utilise un `OnceLock` car `Mutex::new` n'est pas `const` dans l'édition 2021.
static LOGS: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();

/// Retourne une référence au `Mutex` du journal, l'initialisant paresseusement.
fn logs_mutex() -> &'static Mutex<VecDeque<LogEntry>> {
    LOGS.get_or_init(|| Mutex::new(VecDeque::with_capacity(MAX_LOG_ENTRIES)))
}

/// Ajoute une entrée au journal en mémoire et l'append dans le fichier de log si possible.
fn push_log(level: LogLevel, target: &str, message: impl Into<String>) {
    let entry = LogEntry {
        timestamp_secs: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        level,
        target: target.to_string(),
        message: message.into(),
    };

    // Écriture dans le fichier de log (meilleur effort, ne fait pas échouer l'appli si impossible).
    if let Some(path) = log_file_path() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let line = format!(
            "[{}] [{:?}] [{}] {}\n",
            entry.timestamp_secs, entry.level, entry.target, entry.message
        );
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
    }

    // Ajout en mémoire avec éviction des entrées les plus anciennes.
    let mut logs = logs_mutex().lock().expect("le journal interne est verrouillé (empoisonné)");
    if logs.len() >= MAX_LOG_ENTRIES {
        logs.pop_front();
    }
    logs.push_back(entry);
}

/// Enregistre un message d'erreur (niveau `Error`).
pub fn log_error(target: &str, message: impl Into<String>) {
    push_log(LogLevel::Error, target, message);
}

/// Enregistre un avertissement (niveau `Warn`).
pub fn log_warn(target: &str, message: impl Into<String>) {
    push_log(LogLevel::Warn, target, message);
}

/// Enregistre une information (niveau `Info`).
pub fn log_info(target: &str, message: impl Into<String>) {
    push_log(LogLevel::Info, target, message);
}

/// Retourne les entrées de journal récentes (tous niveaux confondus).
pub fn recent_logs(limit: usize) -> Vec<LogEntry> {
    let logs = logs_mutex().lock().expect("le journal interne est verrouillé (empoisonné)");
    logs.iter().rev().take(limit).cloned().collect()
}

/// Retourne uniquement les erreurs récentes.
pub fn recent_errors(limit: usize) -> Vec<LogEntry> {
    let logs = logs_mutex().lock().expect("le journal interne est verrouillé (empoisonné)");
    logs.iter()
        .rev()
        .filter(|e| e.level == LogLevel::Error)
        .take(limit)
        .cloned()
        .collect()
}

/// Retourne uniquement les avertissements récents.
pub fn recent_warnings(limit: usize) -> Vec<LogEntry> {
    let logs = logs_mutex().lock().expect("le journal interne est verrouillé (empoisonné)");
    logs.iter()
        .rev()
        .filter(|e| e.level == LogLevel::Warn)
        .take(limit)
        .cloned()
        .collect()
}

/// Vide le journal en mémoire.
pub fn clear_logs() {
    let mut logs = logs_mutex().lock().expect("le journal interne est verrouillé (empoisonné)");
    logs.clear();
}
