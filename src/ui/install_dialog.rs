use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend::appimage::format_file_size;
use crate::backend::models::{
    DependencyStatus, DependencyStepItem, InstallCancelHandle, PackageItem,
};
use crate::ui::icons::build_package_icon_widget;

struct DepRowWidgets {
    status_label: gtk4::Label,
    status_spinner: gtk4::Spinner,
    status_icon: gtk4::Image,
    current_status: DependencyStatus,
}

#[derive(Clone)]
pub struct InstallManagerUi {
    window: adw::Window,
    phase_label: gtk4::Label,
    size_label: gtk4::Label,
    progress_bar: gtk4::ProgressBar,
    deps_page: adw::PreferencesPage,
    deps_group_slot: Rc<RefCell<Option<adw::PreferencesGroup>>>,
    dep_rows: Rc<RefCell<HashMap<String, DepRowWidgets>>>,
    dep_order: Rc<RefCell<Vec<String>>>,
    log_buffer: gtk4::TextBuffer,
    log_view: gtk4::TextView,
    cancel_btn: gtk4::Button,
    close_btn: gtk4::Button,
    is_installing: Rc<Cell<bool>>,
    cancel_handle: Arc<InstallCancelHandle>,
}

impl InstallManagerUi {
    pub fn new(
        parent: &adw::ApplicationWindow,
        cancel_handle: Arc<InstallCancelHandle>,
    ) -> Self {
        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        let title_widget = adw::WindowTitle::new(
            "Gestionnaire d'installation",
            "Téléchargement et installation des paquets et dépendances",
        );
        header_bar.set_title_widget(Some(&title_widget));
        toolbar_view.add_top_bar(&header_bar);

        let root_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(12)
            .margin_top(14)
            .margin_bottom(14)
            .margin_start(18)
            .margin_end(18)
            .vexpand(true)
            .hexpand(true)
            .build();

        let status_header_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .spacing(12)
            .build();

        let phase_label = gtk4::Label::builder()
            .label("En attente d'une tâche d'installation…")
            .xalign(0.0)
            .hexpand(true)
            .css_classes(["title-4"])
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .build();
        status_header_box.append(&phase_label);

        let size_label = gtk4::Label::builder()
            .label("")
            .xalign(1.0)
            .css_classes(["dim-label"])
            .build();
        status_header_box.append(&size_label);

        root_box.append(&status_header_box);

        let progress_bar = gtk4::ProgressBar::builder()
            .show_text(true)
            .text("0 %")
            .fraction(0.0)
            .build();
        root_box.append(&progress_bar);

        let deps_page = adw::PreferencesPage::builder()
            .vexpand(true)
            .hexpand(true)
            .build();
        root_box.append(&deps_page);

        let log_buffer = gtk4::TextBuffer::new(None);
        let log_view = gtk4::TextView::builder()
            .buffer(&log_buffer)
            .editable(false)
            .cursor_visible(false)
            .monospace(true)
            .wrap_mode(gtk4::WrapMode::WordChar)
            .top_margin(8)
            .bottom_margin(8)
            .left_margin(10)
            .right_margin(10)
            .build();

        let log_scroll = gtk4::ScrolledWindow::builder()
            .min_content_height(130)
            .max_content_height(180)
            .child(&log_view)
            .build();

        let log_expander = gtk4::Expander::builder()
            .label("Journal détaillé de téléchargement et d'installation (Pacman / AUR)")
            .expanded(false)
            .child(&log_scroll)
            .build();
        root_box.append(&log_expander);

        // Barre inférieure avec bouton "Annuler l'installation" et bouton "Fermer / Masquer"
        let bottom_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Horizontal)
            .halign(gtk4::Align::End)
            .spacing(10)
            .build();

        let cancel_btn = gtk4::Button::builder()
            .label("Annuler l'installation")
            .icon_name("process-stop-symbolic")
            .visible(false)
            .css_classes(["destructive-action", "pill"])
            .tooltip_text("Interrompre et annuler l'installation en cours")
            .build();
        bottom_box.append(&cancel_btn);

        let close_btn = gtk4::Button::builder()
            .label("Fermer")
            .css_classes(["pill"])
            .build();
        bottom_box.append(&close_btn);
        root_box.append(&bottom_box);

        toolbar_view.set_content(Some(&root_box));

        let window = adw::Window::builder()
            .transient_for(parent)
            .modal(false)
            .hide_on_close(true)
            .title("Gestionnaire d'installation")
            .default_width(760)
            .default_height(600)
            .content(&toolbar_view)
            .build();

        let is_installing = Rc::new(Cell::new(false));

        // Alerte de confirmation lors du clic sur "Annuler l'installation"
        {
            let win_for_alert = window.clone();
            let cancel_handle_btn = Arc::clone(&cancel_handle);
            let is_inst_btn = Rc::clone(&is_installing);

            cancel_btn.connect_clicked(move |btn| {
                if !is_inst_btn.get() {
                    return;
                }

                let dialog = adw::MessageDialog::builder()
                    .transient_for(&win_for_alert)
                    .modal(true)
                    .heading("Annuler l'installation en cours ?")
                    .body("Voulez-vous vraiment interrompre et annuler le téléchargement et l'installation des paquets en cours ?")
                    .build();

                dialog.add_response("continue", "Continuer l'installation");
                dialog.add_response("confirm_cancel", "Oui, annuler l'installation");
                dialog.set_default_response(Some("continue"));
                dialog.set_close_response("continue");
                dialog.set_response_appearance(
                    "confirm_cancel",
                    adw::ResponseAppearance::Destructive,
                );

                let handle_clone = Arc::clone(&cancel_handle_btn);
                let btn_clone = btn.clone();
                dialog.connect_response(None, move |dlg, response| {
                    if response == "confirm_cancel" {
                        btn_clone.set_sensitive(false);
                        handle_clone.cancel();
                    }
                    dlg.close();
                });

                dialog.present();
            });
        }

        {
            let win_ref = window.clone();
            close_btn.connect_clicked(move |_| {
                win_ref.set_visible(false);
            });
        }

        Self {
            window,
            phase_label,
            size_label,
            progress_bar,
            deps_page,
            deps_group_slot: Rc::new(RefCell::new(None)),
            dep_rows: Rc::new(RefCell::new(HashMap::new())),
            dep_order: Rc::new(RefCell::new(Vec::new())),
            log_buffer,
            log_view,
            cancel_btn,
            close_btn,
            is_installing,
            cancel_handle,
        }
    }

    pub fn is_installing(&self) -> bool {
        self.is_installing.get()
    }

    pub fn cancel_installation(&self) {
        self.cancel_handle.cancel();
        self.is_installing.set(false);
    }

    pub fn present(&self) {
        self.window.set_visible(true);
        self.window.present();
    }

    pub fn on_install_started(&self, target_names: &[String]) {
        self.is_installing.set(true);
        self.cancel_btn.set_visible(true);
        self.cancel_btn.set_sensitive(true);

        self.log_buffer.set_text("");
        self.phase_label.set_label(&format!(
            "Calcul et résolution des dépendances pour {} paquet(s)…",
            target_names.len()
        ));
        self.size_label.set_label("Analyse…");
        self.progress_bar.set_fraction(0.02);
        self.progress_bar.set_text(Some("2 % — Résolution des dépendances"));
        self.close_btn.set_label("Masquer");

        if let Some(old_group) = self.deps_group_slot.borrow_mut().take() {
            self.deps_page.remove(&old_group);
        }
        self.dep_rows.borrow_mut().clear();
        self.dep_order.borrow_mut().clear();

        self.present();
    }

    pub fn on_dependencies_resolved(
        &self,
        dependencies: &[DependencyStepItem],
        total_download_bytes: u64,
    ) {
        if let Some(old_group) = self.deps_group_slot.borrow_mut().take() {
            self.deps_page.remove(&old_group);
        }
        self.dep_rows.borrow_mut().clear();
        self.dep_order.borrow_mut().clear();

        let explicit_count = dependencies
            .iter()
            .filter(|d| d.is_explicit_target)
            .count();
        let extra_deps_count = dependencies.len().saturating_sub(explicit_count);

        let total_size_str = if total_download_bytes > 0 {
            format!(
                "Téléchargement total : {}",
                format_file_size(total_download_bytes)
            )
        } else {
            String::from("Paquets en cache ou compilation locale")
        };
        self.size_label.set_label(&total_size_str);

        let desc_text = format!(
            "{} paquet(s) au total ({} sélectionné(s) + {} dépendance(s) requise(s))",
            dependencies.len(),
            explicit_count,
            extra_deps_count
        );
        let escaped_desc = glib::markup_escape_text(&desc_text);

        let group = adw::PreferencesGroup::builder()
            .title("Paquets et dépendances de la transaction")
            .description(escaped_desc.as_str())
            .build();

        let mut rows_map = self.dep_rows.borrow_mut();
        let mut order_vec = self.dep_order.borrow_mut();

        for dep in dependencies {
            let role_str = if dep.is_explicit_target {
                "Paquet sélectionné"
            } else {
                "Dépendance requise"
            };
            let size_info = if dep.size_bytes > 0 {
                format!(" · {}", format_file_size(dep.size_bytes))
            } else {
                String::new()
            };
            let sub = format!("Version {} · {}{}", dep.version, role_str, size_info);

            let escaped_title = glib::markup_escape_text(&dep.name);
            let escaped_sub = glib::markup_escape_text(&sub);

            let row = adw::ActionRow::builder()
                .title(escaped_title.as_str())
                .subtitle(escaped_sub.as_str())
                .title_lines(1)
                .subtitle_lines(1)
                .build();

            let temp_pkg = PackageItem {
                id: dep.name.clone(),
                name: dep.name.clone(),
                version: dep.version.clone(),
                description: String::new(),
                source: dep.source.clone(),
                is_installed: false,
                update_available: None,
                aur_meta: None,
            };
            let pkg_icon = build_package_icon_widget(&temp_pkg);
            row.add_prefix(&pkg_icon);

            let suffix_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
            suffix_box.set_valign(gtk4::Align::Center);

            let status_spinner = gtk4::Spinner::builder()
                .spinning(false)
                .visible(false)
                .valign(gtk4::Align::Center)
                .build();
            suffix_box.append(&status_spinner);

            let status_icon = gtk4::Image::from_icon_name("content-loading-symbolic");
            status_icon.set_valign(gtk4::Align::Center);
            suffix_box.append(&status_icon);

            let status_label = gtk4::Label::new(Some(dep.status.label()));
            status_label.add_css_class("version-pill");
            status_label.set_valign(gtk4::Align::Center);
            suffix_box.append(&status_label);

            row.add_suffix(&suffix_box);
            group.add(&row);

            order_vec.push(dep.name.clone());
            rows_map.insert(
                dep.name.clone(),
                DepRowWidgets {
                    status_label,
                    status_spinner,
                    status_icon,
                    current_status: DependencyStatus::Pending,
                },
            );
        }

        self.deps_page.add(&group);
        *self.deps_group_slot.borrow_mut() = Some(group);
    }

    pub fn on_progress(
        &self,
        phase: &str,
        fraction: f64,
        active_package: Option<(String, DependencyStatus)>,
        log_line: Option<String>,
    ) {
        if !phase.is_empty() {
            self.phase_label.set_label(phase);
        }

        if fraction >= 0.0 {
            let clamped = fraction.clamp(0.0, 1.0);
            self.progress_bar.set_fraction(clamped);
            let pct = (clamped * 100.0).round() as u32;
            let text = if !phase.is_empty() {
                format!("{pct} % — {phase}")
            } else {
                format!("{pct} %")
            };
            self.progress_bar.set_text(Some(&text));
        }

        if let Some((pkg_name, new_status)) = active_package {
            let mut rows_map = self.dep_rows.borrow_mut();
            let order = self.dep_order.borrow();

            for name in order.iter() {
                if name == &pkg_name {
                    break;
                }
                if let Some(w) = rows_map.get_mut(name) {
                    if matches!(new_status, DependencyStatus::Installing)
                        && matches!(
                            w.current_status,
                            DependencyStatus::Installing | DependencyStatus::Downloading
                        )
                    {
                        set_row_status(w, DependencyStatus::Completed);
                    }
                }
            }

            if let Some(target_w) = rows_map.get_mut(&pkg_name) {
                set_row_status(target_w, new_status);
            }
        }

        if let Some(line) = log_line {
            let mut end_iter = self.log_buffer.end_iter();
            self.log_buffer.insert(&mut end_iter, &line);
            self.log_buffer.insert(&mut end_iter, "\n");
            let mark = self.log_buffer.create_mark(None, &end_iter, false);
            self.log_view.scroll_to_mark(&mark, 0.0, false, 0.0, 1.0);
        }
    }

    pub fn on_install_finished(&self, success: bool, message: &str) {
        self.is_installing.set(false);
        self.cancel_btn.set_visible(false);
        self.phase_label.set_label(message);
        self.close_btn.set_label("Fermer");

        let mut rows_map = self.dep_rows.borrow_mut();
        if success {
            self.progress_bar.set_fraction(1.0);
            self.progress_bar
                .set_text(Some("100 % — Installation terminée avec succès"));
            for w in rows_map.values_mut() {
                set_row_status(w, DependencyStatus::Completed);
            }
        } else {
            self.progress_bar.set_text(Some(message));
            for w in rows_map.values_mut() {
                if matches!(
                    w.current_status,
                    DependencyStatus::Downloading | DependencyStatus::Installing
                ) {
                    set_row_status(w, DependencyStatus::Failed);
                }
            }
        }
    }
}

fn set_row_status(w: &mut DepRowWidgets, status: DependencyStatus) {
    w.current_status = status;
    w.status_label.set_label(status.label());

    match status {
        DependencyStatus::Pending => {
            w.status_spinner.set_spinning(false);
            w.status_spinner.set_visible(false);
            w.status_icon.set_visible(true);
            w.status_icon.set_icon_name(Some("content-loading-symbolic"));
        }
        DependencyStatus::Downloading | DependencyStatus::Installing => {
            w.status_icon.set_visible(false);
            w.status_spinner.set_visible(true);
            w.status_spinner.set_spinning(true);
        }
        DependencyStatus::Completed => {
            w.status_spinner.set_spinning(false);
            w.status_spinner.set_visible(false);
            w.status_icon.set_visible(true);
            w.status_icon.set_icon_name(Some("emblem-ok-symbolic"));
        }
        DependencyStatus::Failed => {
            w.status_spinner.set_spinning(false);
            w.status_spinner.set_visible(false);
            w.status_icon.set_visible(true);
            w.status_icon.set_icon_name(Some("dialog-error-symbolic"));
        }
    }
}
