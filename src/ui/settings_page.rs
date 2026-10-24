use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend::models::ReflectorMirror;
use crate::backend::reflector::{format_rate, mirror_display_name};
use crate::ui::messages::BackendCommand;

#[derive(Clone)]
pub struct SettingsView {
    root: adw::PreferencesPage,
    mirror_table_rows: gtk4::Box,
    mirror_rows: Rc<RefCell<Vec<gtk4::Box>>>,
    mirror_status: gtk4::Label,
    mirror_spinner: gtk4::Spinner,
    mirror_loading: Rc<std::cell::Cell<bool>>,
    refresh_source: Rc<RefCell<Option<gtk4::glib::SourceId>>>,
    refresh_deadline: Rc<std::cell::Cell<Instant>>,
    apply_best_button: gtk4::Button,
    apply_selection_button: gtk4::Button,
    mirrors: Rc<RefCell<Vec<ReflectorMirror>>>,
    selected_mirrors: Rc<RefCell<HashSet<String>>>,
    cache_size_row: adw::ActionRow,
    cache_spinner: gtk4::Spinner,
    clean_button: gtk4::Button,
    check_services_button: gtk4::Button,
    cmd_tx: Sender<BackendCommand>,
}

impl SettingsView {
    pub fn new(cmd_tx: &Sender<BackendCommand>) -> Self {
        let root = adw::PreferencesPage::new();
        root.set_hexpand(true);

        let mirror_group = adw::PreferencesGroup::builder()
            .title("Miroirs Pacman (Reflector)")
            .description(
                "Analyse les miroirs Arch et actualise leur débit toutes les 4 secondes \
                 pendant 1 minute à l'ouverture de cette page.",
            )
            .build();

        let mirror_table = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        mirror_table.add_css_class("mirror-table");
        mirror_table.set_margin_top(6);
        mirror_table.set_margin_bottom(6);
        mirror_table.append(&build_mirror_table_header());

        let mirror_table_rows = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        mirror_table.append(&mirror_table_rows);

        let mirror_table_scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::Automatic)
            .propagate_natural_height(true)
            .max_content_height(430)
            .kinetic_scrolling(true)
            .child(&mirror_table)
            .build();
        mirror_table_scroll.add_css_class("mirror-table-scroll");
        mirror_group.add(&mirror_table_scroll);

        let mirror_rows: Rc<RefCell<Vec<gtk4::Box>>> = Rc::new(RefCell::new(Vec::new()));
        let mirror_status = gtk4::Label::builder()
            .label("Les miroirs sont analysés automatiquement à l'ouverture des paramètres.")
            .xalign(0.0)
            .wrap(true)
            .css_classes(["dim-label"])
            .build();
        let mirror_spinner = gtk4::Spinner::builder()
            .spinning(false)
            .visible(false)
            .valign(gtk4::Align::Center)
            .build();

        let mirror_status_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        mirror_status_box.set_margin_top(6);
        mirror_status_box.set_margin_bottom(6);
        mirror_status_box.set_margin_start(4);
        mirror_status_box.set_margin_end(4);
        mirror_status_box.append(&mirror_spinner);
        mirror_status_box.append(&mirror_status);
        mirror_group.add(&mirror_status_box);

        let mirror_buttons = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        mirror_buttons.set_margin_top(6);
        mirror_buttons.set_margin_bottom(6);
        mirror_buttons.set_margin_start(4);
        mirror_buttons.set_margin_end(4);
        mirror_buttons.set_halign(gtk4::Align::End);

        let apply_best_button = gtk4::Button::builder()
            .label("Appliquer les 5 meilleurs")
            .tooltip_text("Écrit les 5 miroirs les plus rapides dans /etc/pacman.d/mirrorlist")
            .build();

        let apply_selection_button = gtk4::Button::builder()
            .label("Appliquer la sélection")
            .tooltip_text("Écrit uniquement les miroirs cochés dans /etc/pacman.d/mirrorlist")
            .build();

        mirror_buttons.append(&apply_selection_button);
        mirror_buttons.append(&apply_best_button);
        mirror_group.add(&mirror_buttons);

        root.add(&mirror_group);

        // --- Cache Pacman ---
        let cache_group = adw::PreferencesGroup::builder()
            .title("Cache Pacman")
            .description(
                "Espace disque utilisé par les paquets téléchargés dans /var/cache/pacman/pkg.",
            )
            .build();

        let cache_size_row = adw::ActionRow::builder()
            .title("Taille du cache")
            .subtitle("Calcul en cours…")
            .build();
        let cache_spinner = gtk4::Spinner::builder()
            .spinning(false)
            .visible(false)
            .valign(gtk4::Align::Center)
            .build();
        cache_size_row.add_suffix(&cache_spinner);

        let clean_button = gtk4::Button::builder()
            .label("Nettoyer")
            .valign(gtk4::Align::Center)
            .tooltip_text("Lance paccache -r : conserve les 3 dernières versions de chaque paquet")
            .build();
        clean_button.add_css_class("destructive-action");

        let clean_row = adw::ActionRow::builder()
            .title("Nettoyer le cache")
            .subtitle("paccache -r · conserve les 3 dernières versions de chaque paquet")
            .build();
        clean_row.add_suffix(&clean_button);

        cache_group.add(&cache_size_row);
        cache_group.add(&clean_row);
        root.add(&cache_group);

        let services_group = adw::PreferencesGroup::builder()
            .title("Services système")
            .description(
                "Détecte les services utilisant encore d'anciennes bibliothèques après une mise à jour.",
            )
            .build();

        let check_services_button = gtk4::Button::builder()
            .label("Vérifier")
            .valign(gtk4::Align::Center)
            .tooltip_text("Lance needrestart -b et propose de redémarrer les services concernés")
            .build();

        let check_services_row = adw::ActionRow::builder()
            .title("Vérifier les services à redémarrer")
            .subtitle("needrestart -b · analyse les bibliothèques obsolètes")
            .build();
        check_services_row.add_suffix(&check_services_button);
        services_group.add(&check_services_row);
        root.add(&services_group);

        let view = Self {
            root,
            mirror_table_rows,
            mirror_rows,
            mirror_status,
            mirror_spinner,
            mirror_loading: Rc::new(std::cell::Cell::new(false)),
            refresh_source: Rc::new(RefCell::new(None)),
            refresh_deadline: Rc::new(std::cell::Cell::new(Instant::now())),
            apply_best_button,
            apply_selection_button,
            mirrors: Rc::new(RefCell::new(Vec::new())),
            selected_mirrors: Rc::new(RefCell::new(HashSet::new())),
            cache_size_row,
            cache_spinner,
            clean_button,
            check_services_button,
            cmd_tx: cmd_tx.clone(),
        };

        view.connect_handlers();
        view.refresh_cache_size();
        view
    }

    fn connect_handlers(&self) {
        {
            let cmd_tx = self.cmd_tx.clone();
            let view = self.clone();
            self.apply_best_button.connect_clicked(move |_| {
                let servers = view.best_servers(5);
                if servers.is_empty() {
                    view.set_mirror_status(
                        "Les miroirs sont en cours d'analyse. Réessayez dans quelques secondes.",
                    );
                    return;
                }
                view.set_mirror_status("Application des meilleurs miroirs…");
                let _ = cmd_tx.send(BackendCommand::ApplyMirrorlist { servers });
            });
        }

        {
            let cmd_tx = self.cmd_tx.clone();
            let view = self.clone();
            self.apply_selection_button.connect_clicked(move |_| {
                let servers = view.selected_servers();
                if servers.is_empty() {
                    view.set_mirror_status("Cochez au moins un miroir à appliquer.");
                    return;
                }
                view.set_mirror_status("Application de la sélection…");
                let _ = cmd_tx.send(BackendCommand::ApplyMirrorlist { servers });
            });
        }

        {
            let cmd_tx = self.cmd_tx.clone();
            let view = self.clone();
            self.clean_button.connect_clicked(move |button| {
                button.set_sensitive(false);
                view.set_cache_loading();
                let _ = cmd_tx.send(BackendCommand::CleanPacmanCache);
            });
        }

        {
            let cmd_tx = self.cmd_tx.clone();
            self.check_services_button.connect_clicked(move |button| {
                button.set_sensitive(false);
                let _ = cmd_tx.send(BackendCommand::CheckServices);
            });
        }
    }

    pub fn root(&self) -> &adw::PreferencesPage {
        &self.root
    }

    pub fn set_mirror_loading(&self) {
        self.mirror_spinner.set_visible(true);
        self.mirror_spinner.set_spinning(true);
        self.set_mirror_status("Actualisation des miroirs et mesure des débits en cours…");
    }

    /// Démarre l'actualisation automatique (toutes les 4 s) pendant au plus 1 minute,
    /// et uniquement tant que la page Paramètres est visible.
    pub fn start_auto_refresh(&self) {
        self.refresh_deadline
            .set(Instant::now() + Duration::from_secs(60));
        self.refresh_mirrors_if_idle();
        if self.refresh_source.borrow().is_some() {
            return;
        }

        let view = self.clone();
        let source_holder = Rc::clone(&self.refresh_source);
        let source = glib::timeout_add_local(Duration::from_secs(4), move || {
            if !view.root.is_mapped() || Instant::now() >= view.refresh_deadline.get() {
                *source_holder.borrow_mut() = None;
                if view.root.is_mapped() {
                    view.set_mirror_status(
                        "Analyse terminée (1 minute). Rouvrez les paramètres pour actualiser à nouveau.",
                    );
                }
                return glib::ControlFlow::Break;
            }

            view.refresh_mirrors_if_idle();
            glib::ControlFlow::Continue
        });
        *self.refresh_source.borrow_mut() = Some(source);
    }

    fn refresh_mirrors_if_idle(&self) {
        if self.mirror_loading.replace(true) {
            return;
        }
        self.set_mirror_loading();
        let _ = self.cmd_tx.send(BackendCommand::ListReflectorMirrors);
    }

    pub fn on_mirror_list_loaded(&self, mirrors: Vec<ReflectorMirror>) {
        self.mirror_spinner.set_spinning(false);
        self.mirror_spinner.set_visible(false);
        self.mirror_loading.set(false);

        for row in self.mirror_rows.borrow_mut().drain(..) {
            self.mirror_table_rows.remove(&row);
        }

        let first_load = self.mirrors.borrow().is_empty();
        {
            let mut selected = self.selected_mirrors.borrow_mut();
            if first_load {
                selected.clear();
                for mirror in mirrors.iter().take(5) {
                    selected.insert(mirror.url.clone());
                }
            } else {
                selected.retain(|url| mirrors.iter().any(|mirror| &mirror.url == url));
            }
        }

        for (index, mirror) in mirrors.iter().enumerate() {
            let checked = self.selected_mirrors.borrow().contains(&mirror.url);
            let row = build_mirror_table_row(
                mirror,
                index,
                checked,
                Rc::clone(&self.selected_mirrors),
            );
            self.mirror_table_rows.append(&row);
            self.mirror_rows.borrow_mut().push(row);
        }

        self.mirrors.borrow_mut().clone_from(&mirrors);
        if mirrors.is_empty() {
            self.set_mirror_status("Aucun miroir retourné par Reflector.");
        } else {
            self.set_mirror_status(&format!(
                "{} miroir(s) analysé(s) · actualisation automatique (1 min max).",
                mirrors.len()
            ));
        }
    }

    pub fn on_mirror_error(&self, detail: &str) {
        self.mirror_spinner.set_spinning(false);
        self.mirror_spinner.set_visible(false);
        self.mirror_loading.set(false);
        self.set_mirror_status(&format!("Erreur Reflector : {detail}"));
    }

    pub fn on_mirrorlist_applied(&self, message: &str) {
        self.set_mirror_status(message);
    }

    pub fn set_services_checking(&self, checking: bool) {
        self.check_services_button.set_sensitive(!checking);
    }

    pub fn refresh_cache_size(&self) {
        self.set_cache_loading();
        let _ = self.cmd_tx.send(BackendCommand::GetPacmanCacheSize);
    }

    pub fn set_cache_loading(&self) {
        self.cache_spinner.set_visible(true);
        self.cache_spinner.set_spinning(true);
        self.cache_size_row.set_subtitle("Calcul en cours…");
    }

    pub fn on_cache_size(&self, bytes: u64) {
        self.cache_spinner.set_spinning(false);
        self.cache_spinner.set_visible(false);
        self.cache_size_row.set_subtitle(&format_cache_size(bytes));
        self.clean_button.set_sensitive(true);
    }

    pub fn on_cache_error(&self, detail: &str) {
        self.cache_spinner.set_spinning(false);
        self.cache_spinner.set_visible(false);
        self.cache_size_row
            .set_subtitle(&format!("Erreur : {detail}"));
        self.clean_button.set_sensitive(true);
    }

    fn set_mirror_status(&self, message: &str) {
        self.mirror_status.set_label(message);
    }

    fn selected_servers(&self) -> Vec<String> {
        let selected = self.selected_mirrors.borrow();
        self.mirrors
            .borrow()
            .iter()
            .filter(|mirror| selected.contains(&mirror.url))
            .map(|mirror| mirror.url.clone())
            .collect()
    }

    fn best_servers(&self, count: usize) -> Vec<String> {
        self.mirrors
            .borrow()
            .iter()
            .take(count)
            .map(|mirror| mirror.url.clone())
            .collect()
    }
}

fn build_mirror_table_header() -> gtk4::Box {
    let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    header.add_css_class("mirror-table-header");
    header.set_valign(gtk4::Align::Center);

    let check_spacer = gtk4::Box::builder().width_request(30).build();
    header.append(&check_spacer);

    let mirror_label = gtk4::Label::builder()
        .label("MIROIR")
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["mirror-table-header-label"])
        .build();
    header.append(&mirror_label);

    let location_label = gtk4::Label::builder()
        .label("LOCALISATION")
        .xalign(0.0)
        .width_request(210)
        .css_classes(["mirror-table-header-label"])
        .build();
    header.append(&location_label);

    let speed_label = gtk4::Label::builder()
        .label("VITESSE")
        .xalign(1.0)
        .width_request(125)
        .css_classes(["mirror-table-header-label"])
        .build();
    header.append(&speed_label);

    header
}

fn build_mirror_table_row(
    mirror: &ReflectorMirror,
    index: usize,
    checked: bool,
    selected_mirrors: Rc<RefCell<HashSet<String>>>,
) -> gtk4::Box {
    let row = gtk4::Box::new(gtk4::Orientation::Horizontal, 12);
    row.add_css_class("mirror-table-row");
    if index % 2 == 1 {
        row.add_css_class("mirror-table-row-alt");
    }
    row.set_valign(gtk4::Align::Center);
    row.set_tooltip_text(Some(&mirror.url));

    let check_slot = gtk4::Box::builder()
        .width_request(30)
        .halign(gtk4::Align::Center)
        .valign(gtk4::Align::Center)
        .build();
    let check = gtk4::CheckButton::builder()
        .active(checked)
        .valign(gtk4::Align::Center)
        .tooltip_text("Cocher pour inclure ce miroir dans la sélection manuelle")
        .build();
    let selected = Rc::clone(&selected_mirrors);
    let url = mirror.url.clone();
    check.connect_toggled(move |button| {
        if button.is_active() {
            selected.borrow_mut().insert(url.clone());
        } else {
            selected.borrow_mut().remove(&url);
        }
    });
    check_slot.append(&check);
    row.append(&check_slot);

    let mirror_cell = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    mirror_cell.set_hexpand(true);
    mirror_cell.set_valign(gtk4::Align::Center);

    let host = gtk4::Label::builder()
        .label(mirror_display_name(&mirror.url))
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .css_classes(["mirror-host"])
        .build();
    mirror_cell.append(&host);

    let protocol = gtk4::Label::builder()
        .label(mirror.protocol.to_uppercase())
        .valign(gtk4::Align::Center)
        .css_classes(["mirror-protocol-badge"])
        .build();
    mirror_cell.append(&protocol);
    row.append(&mirror_cell);

    let location_cell = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    location_cell.set_width_request(210);
    location_cell.set_valign(gtk4::Align::Center);

    let flag = gtk4::Label::builder()
        .label(country_flag(&mirror.country_code))
        .valign(gtk4::Align::Center)
        .css_classes(["mirror-flag"])
        .build();
    location_cell.append(&flag);

    let country_text = if mirror.country.is_empty() {
        String::from("Pays inconnu")
    } else if mirror.country_code.is_empty() {
        mirror.country.clone()
    } else {
        format!("{} ({})", mirror.country, mirror.country_code)
    };
    let country = gtk4::Label::builder()
        .label(&country_text)
        .xalign(0.0)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .css_classes(["mirror-country"])
        .build();
    location_cell.append(&country);
    row.append(&location_cell);

    let speed_cell = gtk4::Box::builder()
        .width_request(125)
        .halign(gtk4::Align::End)
        .valign(gtk4::Align::Center)
        .build();
    let speed = gtk4::Label::builder()
        .label(format_rate(mirror.rate_kib_s))
        .halign(gtk4::Align::End)
        .css_classes(["mirror-speed-pill", speed_class(mirror.rate_kib_s)])
        .build();
    speed_cell.append(&speed);
    row.append(&speed_cell);

    row
}

fn speed_class(rate_kib_s: f64) -> &'static str {
    if rate_kib_s >= 20.0 * 1024.0 {
        "mirror-speed-fast"
    } else if rate_kib_s >= 5.0 * 1024.0 {
        "mirror-speed-medium"
    } else {
        "mirror-speed-slow"
    }
}

fn country_flag(code: &str) -> String {
    let code = code.trim().to_ascii_uppercase();
    let mut chars = code.chars();
    let (Some(first), Some(second), None) = (chars.next(), chars.next(), chars.next()) else {
        return String::from("🌐");
    };
    if !first.is_ascii_alphabetic() || !second.is_ascii_alphabetic() {
        return String::from("🌐");
    }

    let flag_char = |value: char| {
        char::from_u32(0x1F1E6 + (value as u32 - 'A' as u32)).unwrap_or('🌐')
    };
    format!("{}{}", flag_char(first), flag_char(second))
}

fn format_cache_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["o", "Kio", "Mio", "Gio", "Tio"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} o")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
