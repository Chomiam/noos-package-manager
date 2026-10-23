pub mod appimage;
pub mod aur;
pub mod cache;
pub mod catalog;
pub mod details;
pub mod flatpak;
pub mod install;
pub mod logging;
pub mod models;
pub mod pacman;
pub mod search;

#[cfg(test)]
mod tests;

// Réexport des symboles publics afin de préserver les chemins d'import utilisés par l'UI
// (crate::backend::CatalogSnapshot, crate::backend::load_system_catalog, etc.)
pub use crate::backend::catalog::{
    apply_incremental_catalog_diff, load_system_catalog, load_system_catalog_with_options,
    refresh_catalog_updates,
};
pub use crate::backend::install::{
    install_batch_with_progress, install_item, load_more_aur_discovery_wave,
    remove_item, resolve_optional_dependencies, sync_self_installed_binaries,
    update_all_system_and_aur_with_progress, update_item,
};
pub use crate::backend::search::search_all_sources;

pub use crate::backend::catalog::{CatalogSnapshot, SELF_PACKAGE_NAME, SELF_PACKAGE_VERSION};
