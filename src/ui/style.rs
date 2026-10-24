use gtk4::gdk;

pub const APP_CSS: &str = r#"
.source-badge {
    padding: 2px 8px;
    border-radius: 999px;
    font-size: 0.78em;
    font-weight: 700;
}

.source-badge-pacman {
    background-color: alpha(@accent_bg_color, 0.18);
    color: @accent_color;
}

.source-badge-aur {
    background-color: alpha(@warning_bg_color, 0.22);
    color: @warning_color;
}

.source-badge-flatpak {
    background-color: alpha(@accent_bg_color, 0.24);
    color: @accent_color;
}

.source-badge-appimage {
    background-color: alpha(@success_bg_color, 0.20);
    color: @success_color;
}

.update-badge {
    padding: 1px 7px;
    border-radius: 999px;
    font-size: 0.76em;
    font-weight: 700;
    background-color: @accent_bg_color;
    color: @accent_fg_color;
}

.sidebar-count-badge {
    padding: 1px 6px;
    border-radius: 999px;
    font-size: 0.75em;
    font-weight: 600;
    background-color: alpha(@window_fg_color, 0.10);
}

.version-pill {
    padding: 2px 7px;
    border-radius: 6px;
    font-family: monospace;
    font-size: 0.82em;
    background-color: alpha(@window_fg_color, 0.08);
}

.version-update-pill {
    padding: 2px 7px;
    border-radius: 6px;
    font-family: monospace;
    font-size: 0.82em;
    font-weight: 700;
    background-color: alpha(@accent_bg_color, 0.22);
    color: @accent_color;
}

.aur-obsolete-panel {
    padding: 2px 7px;
    border-radius: 6px;
    font-size: 0.76em;
    font-weight: 700;
    background-color: alpha(@warning_bg_color, 0.25);
    color: @warning_color;
    border: 1px solid alpha(@warning_color, 0.45);
}

.aur-meta-pill {
    padding: 2px 7px;
    border-radius: 6px;
    font-size: 0.78em;
    background-color: alpha(@window_fg_color, 0.06);
}

row.aur-table-header-row {
    min-height: 24px;
    padding-top: 2px;
    padding-bottom: 2px;
    background-color: alpha(@window_fg_color, 0.04);
}

.aur-col-header-label {
    font-size: 0.74em;
    font-weight: 800;
    letter-spacing: 0.04em;
    opacity: 0.75;
    padding: 1px 0px;
}

row.aur-pagination-row {
    min-height: 36px;
    padding-top: 4px;
    padding-bottom: 4px;
    background-color: alpha(@window_fg_color, 0.025);
}

.aur-col-cell {
    padding: 4px 0px;
    border-radius: 6px;
    font-size: 0.85em;
    font-variant-numeric: tabular-nums;
    background-color: alpha(@window_fg_color, 0.045);
}

.aur-col-votes {
    font-weight: 700;
    color: @accent_color;
}

.aur-col-orphan {
    font-weight: 700;
    color: @warning_color;
    background-color: alpha(@warning_bg_color, 0.16);
}

.appimage-dropzone {
    padding: 22px 18px;
    border-radius: 14px;
    border: 2px dashed alpha(@accent_color, 0.45);
    background-color: alpha(@accent_bg_color, 0.07);
    transition: all 150ms ease-in-out;
}

.appimage-dropzone-active {
    border-color: @accent_color;
    background-color: alpha(@accent_bg_color, 0.20);
}

/* --- Tableau des miroirs Arch (Paramètres) --- */
.mirror-table {
    border: 1px solid alpha(@window_fg_color, 0.10);
    border-radius: 10px;
    background-color: alpha(@window_fg_color, 0.015);
}

.mirror-table-scroll {
    border-radius: 10px;
    background-color: transparent;
}

.mirror-table-header {
    padding: 7px 12px;
    background-color: alpha(@window_fg_color, 0.055);
    border-bottom: 1px solid alpha(@window_fg_color, 0.10);
}

.mirror-table-header-label {
    font-size: 0.73em;
    font-weight: 800;
    letter-spacing: 0.05em;
    opacity: 0.70;
}

.mirror-table-row {
    padding: 8px 12px;
    border-bottom: 1px solid alpha(@window_fg_color, 0.06);
}

.mirror-table-row:hover {
    background-color: alpha(@accent_bg_color, 0.06);
}

.mirror-table-row-alt {
    background-color: alpha(@window_fg_color, 0.015);
}

.mirror-host {
    font-weight: 600;
}

.mirror-protocol-badge {
    padding: 1px 6px;
    border-radius: 5px;
    font-size: 0.68em;
    font-weight: 800;
    background-color: alpha(@window_fg_color, 0.08);
    opacity: 0.75;
}

.mirror-flag {
    font-size: 1.25em;
}

.mirror-country {
    font-size: 0.9em;
}

.mirror-speed-pill {
    padding: 3px 9px;
    border-radius: 999px;
    font-size: 0.78em;
    font-weight: 800;
    font-variant-numeric: tabular-nums;
    background-color: alpha(@accent_bg_color, 0.16);
    color: @accent_color;
}

.mirror-speed-fast {
    background-color: alpha(@success_bg_color, 0.18);
    color: @success_color;
}

.mirror-speed-medium {
    background-color: alpha(@warning_bg_color, 0.18);
    color: @warning_color;
}

.mirror-speed-slow {
    background-color: alpha(@error_bg_color, 0.16);
    color: @error_color;
}
"#;

pub fn load_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(APP_CSS);

    if let Some(display) = gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}
