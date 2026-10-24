use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend::models::ReflectorMirror;
use crate::backend::reflector::{format_rate, mirror_display_name};
use crate::ui::messages::BackendCommand;

#[derive(Clone)]
pub struct SettingsView {
    root: adw::PreferencesPage,
    mirror_group: adw::PreferencesGroup,
    mirror_rows: Rc<RefCell<Vec<adw::ActionRow>>>,
    mirror_status: gtk4::Label,
    mirror_spinner: gtk4::Spinner,
    analyze_button: gtk4::Button,
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
                "Analyse les miroirs Arch, mesure leur débit et permet d'appliquer \
                 les meilleurs ou une sélection manuelle.",
            )
            .build();

        let mirror_rows = Rc::new(RefCell::new(Vec::new()));
        let mirror_status = gtk4::Label::builder()
            .label("Cliquez sur « Analyser les miroirs » pour commencer.")
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

        let analyze_button = gtk4::Button::builder()
            .label("Analyser les miroirs")
            .tooltip_text("Interroge Reflector et mesure le débit des miroirs récents")
            .build();
        analyze_button.add_css_class("suggested-action");

        let apply_best_button = gtk4::Button::builder()
            .label("Appliquer les 5 meilleurs")
            .tooltip_text("Écrit les 5 miroirs les plus rapides dans /etc/pacman.d/mirrorlist")
            .build();

        let apply_selection_button = gtk4::Button::builder()
            .label("Appliquer la sélection")
            .tooltip_text("Écrit uniquement les miroirs cochés dans /etc/pacman.d/mirrorlist")
            .build();

        mirror_buttons.append(&analyze_button);
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
            mirror_group,
            mirror_rows,
            mirror_status,
            mirror_spinner,
            analyze_button,
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
            self.analyze_button.connect_clicked(move |_| {
                view.set_mirror_loading();
                let _ = cmd_tx.send(BackendCommand::ListReflectorMirrors);
            });
        }

        {
            let cmd_tx = self.cmd_tx.clone();
            let view = self.clone();
            self.apply_best_button.connect_clicked(move |_| {
                let servers = view.best_servers(5);
                if servers.is_empty() {
                    view.set_mirror_status(
                        "Analysez d'abord les miroirs avant d'appliquer une liste.",
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
        self.analyze_button.set_sensitive(false);
        self.apply_best_button.set_sensitive(false);
        self.apply_selection_button.set_sensitive(false);
        self.set_mirror_status("Analyse des miroirs et mesure des débits en cours…");
    }

    pub fn on_mirror_list_loaded(&self, mirrors: Vec<ReflectorMirror>) {
        self.mirror_spinner.set_spinning(false);
        self.mirror_spinner.set_visible(false);
        self.analyze_button.set_sensitive(true);
        self.apply_best_button.set_sensitive(true);
        self.apply_selection_button.set_sensitive(true);

        for row in self.mirror_rows.borrow_mut().drain(..) {
            self.mirror_group.remove(&row);
        }

        {
            let mut selected = self.selected_mirrors.borrow_mut();
            selected.clear();
            for (index, mirror) in mirrors.iter().enumerate() {
                if index < 5 {
                    selected.insert(mirror.url.clone());
                }
            }
        }

        for (index, mirror) in mirrors.iter().enumerate() {
            let country = if mirror.country.is_empty() {
                String::from("Pays inconnu")
            } else {
                mirror.country.clone()
            };
            let subtitle = format!(
                "{} · {} · {}",
                country,
                format_rate(mirror.rate_kib_s),
                mirror.protocol.to_uppercase()
            );

            let row = adw::ActionRow::builder()
                .title(mirror_display_name(&mirror.url))
                .subtitle(subtitle)
                .build();
            row.set_tooltip_text(Some(&mirror.url));

            let check = gtk4::CheckButton::builder()
                .active(index < 5)
                .valign(gtk4::Align::Center)
                .tooltip_text("Cocher pour inclure ce miroir dans la sélection manuelle")
                .build();
            let selected = Rc::clone(&self.selected_mirrors);
            let url = mirror.url.clone();
            check.connect_toggled(move |button| {
                if button.is_active() {
                    selected.borrow_mut().insert(url.clone());
                } else {
                    selected.borrow_mut().remove(&url);
                }
            });
            row.add_prefix(&check);
            row.set_activatable_widget(Some(&check));

            self.mirror_group.add(&row);
            self.mirror_rows.borrow_mut().push(row);
        }

        self.mirrors.borrow_mut().clone_from(&mirrors);
        if mirrors.is_empty() {
            self.set_mirror_status("Aucun miroir retourné par Reflector.");
        } else {
            self.set_mirror_status(&format!(
                "{} miroir(s) analysé(s). Les 5 meilleurs sont présélectionnés.",
                mirrors.len()
            ));
        }
    }

    pub fn on_mirror_error(&self, detail: &str) {
        self.mirror_spinner.set_spinning(false);
        self.mirror_spinner.set_visible(false);
        self.analyze_button.set_sensitive(true);
        self.apply_best_button.set_sensitive(true);
        self.apply_selection_button.set_sensitive(true);
        self.set_mirror_status(&format!("Erreur Reflector : {detail}"));
    }

    pub fn on_mirrorlist_applied(&self, message: &str) {
        self.apply_best_button.set_sensitive(true);
        self.apply_selection_button.set_sensitive(true);
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
