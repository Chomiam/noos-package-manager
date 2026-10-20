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
    padding: 2px 9px;
    border-radius: 999px;
    font-size: 0.80em;
    font-weight: 700;
    background-color: @accent_bg_color;
    color: @accent_fg_color;
}

.sidebar-count-badge {
    padding: 1px 8px;
    border-radius: 999px;
    font-size: 0.78em;
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

.aur-table-header-row {
    min-height: 34px;
    background-color: alpha(@window_fg_color, 0.04);
}

.aur-col-header-label {
    font-size: 0.75em;
    font-weight: 800;
    letter-spacing: 0.04em;
    opacity: 0.72;
    padding: 4px 0px;
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
