use std::cmp::Ordering;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageSource {
    Pacman,
    Aur,
    Flatpak,
    AppImage(std::path::PathBuf),
}

impl PackageSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Pacman => "Pacman",
            Self::Aur => "AUR",
            Self::Flatpak => "Flatpak",
            Self::AppImage(_) => "AppImage",
        }
    }

    pub fn icon_name(&self) -> &'static str {
        match self {
            Self::Pacman => "package-x-generic-symbolic",
            Self::Aur => "system-software-install-symbolic",
            Self::Flatpak => "package-x-generic-symbolic",
            Self::AppImage(_) => "application-x-executable-symbolic",
        }
    }

    pub fn css_badge_class(&self) -> &'static str {
        match self {
            Self::Pacman => "source-badge-pacman",
            Self::Aur => "source-badge-aur",
            Self::Flatpak => "source-badge-flatpak",
            Self::AppImage(_) => "source-badge-appimage",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AurPackageMeta {
    pub votes: u32,
    pub popularity: f64,
    pub maintainer: Option<String>,
    pub last_modified: Option<u64>,
    pub out_of_date: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AurSortMode {
    #[default]
    Popularity,
    Votes,
    RecentlyUpdated,
    Name,
}

impl AurSortMode {
    pub const ALL: [AurSortMode; 4] = [
        AurSortMode::Popularity,
        AurSortMode::Votes,
        AurSortMode::RecentlyUpdated,
        AurSortMode::Name,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Popularity => "Popularité (défaut)",
            Self::Votes => "Votes",
            Self::RecentlyUpdated => "Mis à jour récemment",
            Self::Name => "Nom (A → Z)",
        }
    }

    pub fn from_index(idx: u32) -> Self {
        match idx {
            0 => Self::Popularity,
            1 => Self::Votes,
            2 => Self::RecentlyUpdated,
            3 => Self::Name,
            _ => Self::Popularity,
        }
    }

    pub fn compare_items(self, a: &PackageItem, b: &PackageItem) -> Ordering {
        let ma = a.aur_meta.as_ref();
        let mb = b.aur_meta.as_ref();

        match self {
            Self::Popularity => {
                let pop_a = ma.map(|m| m.popularity).unwrap_or(0.0);
                let pop_b = mb.map(|m| m.popularity).unwrap_or(0.0);
                pop_b
                    .partial_cmp(&pop_a)
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| {
                        let va = ma.map(|m| m.votes).unwrap_or(0);
                        let vb = mb.map(|m| m.votes).unwrap_or(0);
                        vb.cmp(&va)
                    })
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }
            Self::Votes => {
                let va = ma.map(|m| m.votes).unwrap_or(0);
                let vb = mb.map(|m| m.votes).unwrap_or(0);
                vb.cmp(&va)
                    .then_with(|| {
                        let pop_a = ma.map(|m| m.popularity).unwrap_or(0.0);
                        let pop_b = mb.map(|m| m.popularity).unwrap_or(0.0);
                        pop_b.partial_cmp(&pop_a).unwrap_or(Ordering::Equal)
                    })
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }
            Self::RecentlyUpdated => {
                let la = ma.and_then(|m| m.last_modified).unwrap_or(0);
                let lb = mb.and_then(|m| m.last_modified).unwrap_or(0);
                lb.cmp(&la)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }
            Self::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PackageItem {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub source: PackageSource,
    pub is_installed: bool,
    pub update_available: Option<String>,
    pub aur_meta: Option<AurPackageMeta>,
}

impl PackageItem {
    pub fn has_update(&self) -> bool {
        self.update_available.is_some()
    }

    pub fn flatpak_app_id(&self) -> Option<&str> {
        self.id.strip_prefix("flatpak:")
    }

    pub fn is_aur_out_of_date(&self) -> bool {
        self.aur_meta
            .as_ref()
            .and_then(|m| m.out_of_date)
            .is_some()
    }

    pub fn truncated_description(&self, max_chars: usize) -> String {
        let trimmed = self.description.trim();
        if trimmed.is_empty() {
            return String::from("Aucune description disponible");
        }
        let char_count = trimmed.chars().count();
        if char_count <= max_chars {
            trimmed.to_string()
        } else {
            let take_len = max_chars.saturating_sub(1);
            let mut result: String = trimmed.chars().take(take_len).collect();
            result.push('…');
            result
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecommendationCategory {
    BrowsersInternet,
    MultimediaCreation,
    OfficeProductivity,
    DevelopmentTools,
    GamingEntertainment,
    SystemUtilities,
}

impl RecommendationCategory {
    pub const ALL: [RecommendationCategory; 6] = [
        RecommendationCategory::BrowsersInternet,
        RecommendationCategory::MultimediaCreation,
        RecommendationCategory::OfficeProductivity,
        RecommendationCategory::DevelopmentTools,
        RecommendationCategory::GamingEntertainment,
        RecommendationCategory::SystemUtilities,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::BrowsersInternet => "Recommandations · Navigateurs & Internet",
            Self::MultimediaCreation => "Recommandations · Multimédia, Audio & Création",
            Self::OfficeProductivity => "Recommandations · Bureautique & Productivité",
            Self::DevelopmentTools => "Recommandations · Développement & Programmation",
            Self::GamingEntertainment => "Recommandations · Jeux & Gaming",
            Self::SystemUtilities => "Recommandations · Système & Utilitaires",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::BrowsersInternet => {
                "Navigateurs web, messageries instantanées, clients mail et transferts"
            }
            Self::MultimediaCreation => {
                "Lecteurs vidéo/audio, retouche photo, montage vidéo, modélisation 3D et streaming"
            }
            Self::OfficeProductivity => {
                "Suites bureautiques, prise de notes, lecteurs PDF/ePub et gestionnaires de mots de passe"
            }
            Self::DevelopmentTools => {
                "Éditeurs de code, émulateurs de terminal, conteneurs, compilateurs et outils Git"
            }
            Self::GamingEntertainment => {
                "Lanceurs de jeux, compatibilité Proton/Wine, émulateurs et optimisation des performances"
            }
            Self::SystemUtilities => {
                "Moniteurs de ressources, gestion de disques, sauvegardes, captures d'écran et virtualisation"
            }
        }
    }

    pub fn package_names(self) -> &'static [&'static str] {
        match self {
            Self::BrowsersInternet => &[
                "firefox",
                "chromium",
                "thunderbird",
                "telegram-desktop",
                "signal-desktop",
                "qbittorrent",
                "filezilla",
                "remmina",
                "nextcloud-client",
            ],
            Self::MultimediaCreation => &[
                "vlc",
                "mpv",
                "gimp",
                "inkscape",
                "krita",
                "blender",
                "obs-studio",
                "kdenlive",
                "audacity",
                "darktable",
                "handbrake",
            ],
            Self::OfficeProductivity => &[
                "libreoffice-fresh",
                "obsidian",
                "okular",
                "evince",
                "foliate",
                "calibre",
                "keepassxc",
                "xournalpp",
            ],
            Self::DevelopmentTools => &[
                "code",
                "zed",
                "neovim",
                "alacritty",
                "kitty",
                "git",
                "lazygit",
                "docker",
                "dbeaver",
                "meld",
                "rustup",
                "cmake",
                "nodejs",
                "go",
            ],
            Self::GamingEntertainment => &[
                "steam",
                "lutris",
                "prismlauncher",
                "mangohud",
                "gamemode",
                "wine",
                "winetricks",
                "retroarch",
                "supertuxkart",
                "0ad",
            ],
            Self::SystemUtilities => &[
                "btop",
                "htop",
                "fastfetch",
                "gparted",
                "baobab",
                "timeshift",
                "flameshot",
                "ark",
                "virt-manager",
                "flatpak",
                "fwupd",
            ],
        }
    }

    pub fn flathub_title(self) -> &'static str {
        match self {
            Self::BrowsersInternet => "Recommandations Flathub · Navigateurs & Internet",
            Self::MultimediaCreation => "Recommandations Flathub · Multimédia, Audio & Création",
            Self::OfficeProductivity => "Recommandations Flathub · Bureautique & Productivité",
            Self::DevelopmentTools => "Recommandations Flathub · Développement & Programmation",
            Self::GamingEntertainment => "Recommandations Flathub · Jeux & Gaming",
            Self::SystemUtilities => "Recommandations Flathub · Système & Utilitaires",
        }
    }

    pub fn flathub_api_category(self) -> &'static str {
        match self {
            Self::BrowsersInternet => "network",
            Self::MultimediaCreation => "audiovideo",
            Self::OfficeProductivity => "office",
            Self::DevelopmentTools => "development",
            Self::GamingEntertainment => "game",
            Self::SystemUtilities => "utility",
        }
    }

    pub fn flathub_curated_app_ids(self) -> &'static [&'static str] {
        match self {
            Self::BrowsersInternet => &[
                "org.mozilla.firefox",
                "com.brave.Browser",
                "io.github.zen_browser.zen",
                "com.discordapp.Discord",
                "org.telegram.desktop",
                "org.signal.Signal",
                "org.mozilla.Thunderbird",
                "org.qbittorrent.qBittorrent",
                "de.haeckerfelix.Fragments",
            ],
            Self::MultimediaCreation => &[
                "com.spotify.Client",
                "com.obsproject.Studio",
                "org.videolan.VLC",
                "org.kde.kdenlive",
                "org.gimp.GIMP",
                "org.inkscape.Inkscape",
                "org.kde.krita",
                "org.blender.Blender",
                "org.audacityteam.Audacity",
                "fr.handbrake.ghb",
            ],
            Self::OfficeProductivity => &[
                "md.obsidian.Obsidian",
                "org.libreoffice.LibreOffice",
                "com.bitwarden.desktop",
                "org.keepassxc.KeePassXC",
                "com.github.flxzt.rnote",
                "io.github.alainm23.planify",
                "org.onlyoffice.desktopeditors",
                "com.logseq.Logseq",
                "com.github.johnfactotum.Foliate",
            ],
            Self::DevelopmentTools => &[
                "com.visualstudio.code",
                "com.vscodium.codium",
                "dev.zed.Zed",
                "io.podman_desktop.PodmanDesktop",
                "com.usebruno.Bruno",
                "rest.insomnia.Insomnia",
                "io.dbeaver.DBeaverCommunity",
                "org.gnome.Builder",
                "com.github.marhkb.Pods",
            ],
            Self::GamingEntertainment => &[
                "com.valvesoftware.Steam",
                "com.heroicgameslauncher.hgl",
                "net.lutris.Lutris",
                "com.usebottles.bottles",
                "org.prismlauncher.PrismLauncher",
                "net.davidotek.pupgui2",
                "com.github.Matoking.protontricks",
                "org.libretro.RetroArch",
                "com.moonlight_stream.Moonlight",
            ],
            Self::SystemUtilities => &[
                "com.github.tchx84.Flatseal",
                "io.missioncenter.MissionCenter",
                "io.github.flattool.Warehouse",
                "com.mattjakeman.ExtensionManager",
                "org.localsend.localsend_app",
                "org.gnome.Boxes",
                "org.gnome.baobab",
                "org.flameshot.Flameshot",
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AurHelper {
    Paru,
    Yay,
}

impl AurHelper {
    pub fn binary_name(self) -> &'static str {
        match self {
            Self::Paru => "paru",
            Self::Yay => "yay",
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            Self::Paru => "Paru",
            Self::Yay => "Yay",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SidebarCategory {
    #[default]
    All,
    Installed,
    Updates,
    Pacman,
    Aur,
    Flatpak,
    AppImages,
}

impl SidebarCategory {
    pub const ALL: [SidebarCategory; 7] = [
        SidebarCategory::All,
        SidebarCategory::Installed,
        SidebarCategory::Updates,
        SidebarCategory::Pacman,
        SidebarCategory::Aur,
        SidebarCategory::Flatpak,
        SidebarCategory::AppImages,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::All => "Tous",
            Self::Installed => "Installés",
            Self::Updates => "Mises à jour",
            Self::Pacman => "Pacman",
            Self::Aur => "AUR",
            Self::Flatpak => "Flatpak",
            Self::AppImages => "AppImages",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Self::All => "Sélection de paquets recommandés et applications du système",
            Self::Installed => "Paquets actuellement présents sur le système",
            Self::Updates => "Paquets disposant d'une nouvelle version",
            Self::Pacman => "Dépôts officiels Arch Linux — Recommandations par catégories",
            Self::Aur => "Arch User Repository (RPC v5)",
            Self::Flatpak => "Applications sandboxées Flathub — Recommandations par catégories",
            Self::AppImages => "Applications portables locales",
        }
    }

    pub fn icon_name(self) -> &'static str {
        match self {
            Self::All => "view-grid-symbolic",
            Self::Installed => "emblem-default-symbolic",
            Self::Updates => "software-update-available-symbolic",
            Self::Pacman => "package-x-generic-symbolic",
            Self::Aur => "system-software-install-symbolic",
            Self::Flatpak => "package-x-generic-symbolic",
            Self::AppImages => "application-x-executable-symbolic",
        }
    }

    pub fn matches(self, item: &PackageItem) -> bool {
        match self {
            Self::All => true,
            Self::Installed => item.is_installed,
            Self::Updates => item.update_available.is_some(),
            Self::Pacman => matches!(item.source, PackageSource::Pacman),
            Self::Aur => matches!(item.source, PackageSource::Aur) && !item.is_installed,
            Self::Flatpak => matches!(item.source, PackageSource::Flatpak),
            Self::AppImages => matches!(item.source, PackageSource::AppImage(_)),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppImageMetadata {
    pub path: PathBuf,
    pub name: String,
    pub version: String,
    pub size_bytes: u64,
    pub modified_unix_secs: u64,
    pub is_executable: bool,
    pub desktop_comment: Option<String>,
    pub icon_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyStatus {
    Pending,
    Downloading,
    Installing,
    Completed,
    Failed,
}

impl DependencyStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Pending => "En attente",
            Self::Downloading => "Téléchargement…",
            Self::Installing => "Installation…",
            Self::Completed => "Installé",
            Self::Failed => "Échec",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DependencyStepItem {
    pub name: String,
    pub version: String,
    pub size_bytes: u64,
    pub is_explicit_target: bool,
    pub source: PackageSource,
    pub status: DependencyStatus,
}

#[derive(Debug, Default)]
pub struct InstallCancelHandle {
    cancelled: std::sync::atomic::AtomicBool,
    notify: tokio::sync::Notify,
}

impl InstallCancelHandle {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&self) {
        self.cancelled
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub async fn notified(&self) {
        if self.is_cancelled() {
            return;
        }
        self.notify.notified().await;
    }
}

