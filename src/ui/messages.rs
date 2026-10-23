use crate::backend::details::PackageDetailedInfo;
use crate::backend::models::{
    DependencyStatus, DependencyStepItem, OptionalDependencyItem, PackageItem, PackageSource,
};
use crate::backend::pacman::PacmanLockInfo;
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
    },
    LoadMoreAurPackages {
        wave_index: usize,
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
}
