use crate::backend::details::PackageDetailedInfo;
use crate::backend::models::{
    AurVersionInfo, DependencyStatus, DependencyStepItem, OptionalDependencyItem, PackageItem,
    PackageSource, ReflectorMirror, ServiceRestartFailure,
};
use crate::backend::pacman::PacmanLockInfo;
use crate::backend::yay_interaction::YayPromptKind;
use crate::backend::CatalogSnapshot;

#[derive(Debug, Clone)]
pub enum BackendCommand {
    RefreshCatalog,
    CheckAllUpdates,
    CheckPacmanLock,
    RepairPacmanLock,
    Search {
        query: String,
    },
    FetchPackageDetails {
        item: PackageItem,
    },
    PrepareInstallBatch {
        items: Vec<PackageItem>,
    },
    InstallBatch {
        items: Vec<PackageItem>,
    },
    InstallPackage {
        name: String,
        source: PackageSource,
    },
    UpdatePackage {
        name: String,
        source: PackageSource,
    },
    RemovePackage {
        name: String,
        source: PackageSource,
    },
    ExecuteAppImage {
        name: String,
        path: std::path::PathBuf,
    },
    ImportAppImages {
        paths: Vec<std::path::PathBuf>,
    },
    CheckAppImageUpdates,
    UpdateAppImage {
        item: PackageItem,
    },
    LaunchFlatpak {
        app_id: String,
        name: String,
    },
    UpdateAllSystem {
        items: Vec<PackageItem>,
        ignored: Vec<String>,
    },
    LoadMoreAurPackages {
        wave_index: usize,
    },
    FetchAurVersions {
        package: String,
    },
    InstallAurVersion {
        package: String,
        package_base: String,
        version: String,
        commit: String,
    },
    CheckServices,
    GetPacmanParallelDownloads,
    SetPacmanParallelDownloads {
        value: u8,
    },
    ListReflectorMirrors,
    ApplyMirrorlist {
        servers: Vec<String>,
    },
    GetPacmanCacheSize,
    CleanPacmanCache,
    RestartServices {
        services: Vec<String>,
        kernel_status: Option<u32>,
    },
}

#[derive(Debug, Clone)]
pub enum UiEvent {
    LoadingStarted {
        message: String,
    },
    CatalogLoaded(CatalogSnapshot),
    AurMorePackagesLoaded {
        wave_index: usize,
        items: Vec<PackageItem>,
    },
    UpdatesChecked {
        snapshot: CatalogSnapshot,
        summary_toast: String,
    },
    SearchCompleted {
        query: String,
        results: Vec<PackageItem>,
        warnings: Vec<String>,
    },
    PackageDetailsLoaded(Box<PackageDetailedInfo>),
    OptionalDependenciesPrompt {
        items: Vec<PackageItem>,
        optional_deps: Vec<OptionalDependencyItem>,
    },
    InstallStarted {
        target_names: Vec<String>,
    },
    InstallDependenciesResolved {
        dependencies: Vec<DependencyStepItem>,
        total_download_bytes: u64,
    },
    InstallProgress {
        phase: String,
        fraction: f64,
        active_package: Option<(String, DependencyStatus)>,
        log_line: Option<String>,
    },
    InstallFinished {
        success: bool,
        message: String,
        installed_ids: Vec<String>,
    },
    OperationSuccess {
        message: String,
        reload_catalog: bool,
    },
    OperationError {
        title: String,
        detail: String,
    },
    PacmanLockDetected(PacmanLockInfo),
    PacmanLockCleared,
    PacmanLockRepaired {
        message: String,
    },
    CatalogReloadRequested,
    AurVersionsLoaded {
        package: String,
        package_base: String,
        versions: Vec<AurVersionInfo>,
    },
    AurVersionsError(String),
    MirrorListLoaded(Vec<ReflectorMirror>),
    MirrorListApplied {
        message: String,
    },
    ReflectorError(String),
    PacmanCacheSize {
        bytes: u64,
    },
    PacmanCacheError(String),
    PacmanParallelDownloadsLoaded {
        value: u8,
    },
    ServicesNeedRestart {
        services: Vec<String>,
        kernel_status: Option<u32>,
    },
    ServicesRestarted {
        restarted: Vec<String>,
        failures: Vec<ServiceRestartFailure>,
        kernel_status: Option<u32>,
    },
    YayPromptRequired {
        request_id: u64,
        prompt: YayPromptKind,
    },
}
