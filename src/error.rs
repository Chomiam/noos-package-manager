use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("Erreur Pacman : {0}")]
    PacmanError(String),

    #[error("Erreur réseau AUR : {0}")]
    AurNetworkError(#[from] reqwest::Error),

    #[error("Erreur API AUR : {0}")]
    AurApiError(String),

    #[error("Aucun assistant AUR détecté (installez 'yay')")]
    AurHelperNotFound,

    #[error("Erreur de l'assistant AUR : {0}")]
    AurHelperError(String),

    #[error("Erreur Flatpak / Flathub : {0}")]
    FlatpakError(String),

    #[error("Erreur Reflector : {0}")]
    ReflectorError(String),

    #[error("Erreur needrestart : {0}")]
    NeedrestartError(String),

    #[error("Erreur AppImage : {0}")]
    AppImageError(String),

    #[error("Erreur d'entrée/sortie : {0}")]
    IoError(#[from] std::io::Error),

    #[error("Erreur de désérialisation JSON : {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Erreur d'encodage UTF-8 : {0}")]
    Utf8Error(#[from] std::string::FromUtf8Error),

    #[error("Erreur de communication interne : {0}")]
    ChannelError(String),
}

pub type AppResult<T> = Result<T, AppError>;
