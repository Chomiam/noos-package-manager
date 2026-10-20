use std::sync::mpsc::Sender;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend::appimage::format_unix_timestamp;
use crate::backend::models::{PackageItem, PackageSource};
use crate::ui::icons::build_package_icon_widget;
use crate::ui::messages::BackendCommand;

const COL_VOTES_WIDTH: i32 = 72;
const COL_POPULARITY_WIDTH: i32 = 68;
const COL_MAINTAINER_WIDTH: i32 = 110;
const COL_UPDATED_WIDTH: i32 = 96;
const COL_VERSION_WIDTH: i32 = 146;
const COL_ACTIONS_WIDTH: i32 = 90;

/// Construit la ligne d'en-tête des colonnes pour l'onglet AUR afin d'aligner visuellement
/// `Paquet & Description | Votes | Popularité | Mainteneur | Dernière màj | Version | Actions`.
pub fn build_aur_columns_header_row() -> adw::ActionRow {
    let header_row = adw::ActionRow::builder()
        .title("PAQUET &amp; DESCRIPTION")
        .css_classes(["aur-table-header-row"])
        .activatable(false)
        .build();

    let suffix_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    suffix_box.set_valign(gtk4::Align::Center);

    let cols: [(&str, i32, f32); 6] = [
        ("VOTES", COL_VOTES_WIDTH, 0.5),
        ("POP.", COL_POPULARITY_WIDTH, 0.5),
        ("MAINTENEUR", COL_MAINTAINER_WIDTH, 0.5),
        ("MISE À JOUR", COL_UPDATED_WIDTH, 0.5),
        ("VERSION", COL_VERSION_WIDTH, 0.5),
        ("ACTIONS", COL_ACTIONS_WIDTH, 0.5),
    ];

    for (title, width, xalign) in cols {
        let lbl = gtk4::Label::builder()
            .label(title)
            .width_request(width)
            .xalign(xalign)
            .css_classes(["aur-col-header-label"])
            .build();
        suffix_box.append(&lbl);
    }

    header_row.add_suffix(&suffix_box);
    header_row
}

pub fn build_package_action_row<F, I>(
    item: &PackageItem,
    is_checked: bool,
    show_source_badge: bool,
    cmd_tx: &Sender<BackendCommand>,
    on_check_changed: F,
    on_info_clicked: I,
) -> adw::ActionRow
where
    F: Fn(bool) + 'static,
    I: Fn() + 'static,
{
    let base_desc = item.truncated_description(100);

    let escaped_title = gtk4::glib::markup_escape_text(&item.name);
    let escaped_subtitle = gtk4::glib::markup_escape_text(&base_desc);

    let row = adw::ActionRow::builder()
        .title(escaped_title.as_str())
        .subtitle(escaped_subtitle.as_str())
        .title_lines(1)
        .subtitle_lines(2)
        .build();

    // Préfixe : case à cocher + logo réel + badge source (uniquement dans Installés)
    let prefix_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    prefix_box.set_valign(gtk4::Align::Center);

    let can_select_for_install = matches!(
        item.source,
        PackageSource::Pacman | PackageSource::Aur | PackageSource::Flatpak
    ) && (!item.is_installed || item.update_available.is_some());

    if can_select_for_install {
        let check_btn = gtk4::CheckButton::builder()
            .active(is_checked)
            .valign(gtk4::Align::Center)
            .tooltip_text("Sélectionner ce paquet pour une installation groupée")
            .build();

        check_btn.connect_toggled(move |btn| {
            on_check_changed(btn.is_active());
        });

        row.set_activatable_widget(Some(&check_btn));
        prefix_box.append(&check_btn);
    }

    let icon = build_package_icon_widget(item);
    icon.set_valign(gtk4::Align::Center);
    prefix_box.append(&icon);

    if show_source_badge {
        let source_badge = gtk4::Label::new(Some(item.source.label()));
        source_badge.add_css_class("source-badge");
        source_badge.add_css_class(item.source.css_badge_class());
        source_badge.set_valign(gtk4::Align::Center);
        prefix_box.append(&source_badge);
    }

    row.add_prefix(&prefix_box);

    // Suffixe : colonnes structurées pour AUR, ou affichage standard pour Pacman / Flatpak / AppImage
    let suffix_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    suffix_box.set_valign(gtk4::Align::Center);

    if let Some(ref meta) = item.aur_meta {
        // Colonne 1 : Votes
        let votes_lbl = gtk4::Label::builder()
            .label(format!("★ {}", meta.votes))
            .width_request(COL_VOTES_WIDTH)
            .xalign(0.5)
            .valign(gtk4::Align::Center)
            .tooltip_text(format!("Votes AUR : {}", meta.votes))
            .css_classes(["aur-col-cell", "aur-col-votes"])
            .build();
        suffix_box.append(&votes_lbl);

        // Colonne 2 : Popularité
        let pop_lbl = gtk4::Label::builder()
            .label(format!("{:.2}", meta.popularity))
            .width_request(COL_POPULARITY_WIDTH)
            .xalign(0.5)
            .valign(gtk4::Align::Center)
            .tooltip_text(format!("Indice de popularité AUR : {:.2}", meta.popularity))
            .css_classes(["aur-col-cell"])
            .build();
        suffix_box.append(&pop_lbl);

        // Colonne 3 : Mainteneur
        let is_orphan = meta.maintainer.is_none();
        let maint_str = meta.maintainer.as_deref().unwrap_or("Orphelin");
        let maint_lbl = gtk4::Label::builder()
            .label(maint_str)
            .width_request(COL_MAINTAINER_WIDTH)
            .max_width_chars(12)
            .xalign(0.5)
            .valign(gtk4::Align::Center)
            .ellipsize(gtk4::pango::EllipsizeMode::End)
            .tooltip_text(format!("Mainteneur AUR : {maint_str}"))
            .css_classes(["aur-col-cell"])
            .build();
        if is_orphan {
            maint_lbl.add_css_class("aur-col-orphan");
        }
        suffix_box.append(&maint_lbl);

        // Colonne 4 : Dernière mise à jour
        let (short_date, full_date) = match meta.last_modified {
            Some(ts) => {
                let full = format_unix_timestamp(ts);
                let short = full
                    .split_whitespace()
                    .next()
                    .unwrap_or(&full)
                    .to_string();
                (short, full)
            }
            None => (String::from("—"), String::from("Date inconnue")),
        };
        let date_lbl = gtk4::Label::builder()
            .label(&short_date)
            .width_request(COL_UPDATED_WIDTH)
            .max_width_chars(10)
            .xalign(0.5)
            .valign(gtk4::Align::Center)
            .tooltip_text(format!("Dernière mise à jour AUR : {full_date}"))
            .css_classes(["aur-col-cell"])
            .build();
        suffix_box.append(&date_lbl);

        // Colonne 5 : Version (+ Panneau Obsolète empilé verticalement s'il est out-of-date)
        let ver_col_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(3)
            .width_request(COL_VERSION_WIDTH)
            .valign(gtk4::Align::Center)
            .build();

        if let Some(ref new_ver) = item.update_available {
            let ver_text = format!("{} → {}", item.version, new_ver);
            let ver_label = gtk4::Label::builder()
                .label(&ver_text)
                .width_request(COL_VERSION_WIDTH)
                .max_width_chars(15)
                .xalign(0.5)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .tooltip_text(format!("Mise à jour disponible : {ver_text}"))
                .css_classes(["version-update-pill"])
                .build();
            ver_col_box.append(&ver_label);
        } else {
            let ver_label = gtk4::Label::builder()
                .label(&item.version)
                .width_request(COL_VERSION_WIDTH)
                .max_width_chars(15)
                .xalign(0.5)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .tooltip_text(format!("Version : {}", item.version))
                .css_classes(["version-pill"])
                .build();
            ver_col_box.append(&ver_label);
        }

        if let Some(out_ts) = meta.out_of_date {
            let out_full = format_unix_timestamp(out_ts);
            let out_short = out_full
                .split_whitespace()
                .next()
                .unwrap_or(&out_full)
                .to_string();
            let warn_panel = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
            warn_panel.add_css_class("aur-obsolete-panel");
            warn_panel.set_halign(gtk4::Align::Center);
            warn_panel.set_tooltip_text(Some(&format!(
                "Attention : ce paquet AUR est marqué comme obsolète (Out-of-Date) par le mainteneur depuis le {out_full}"
            )));

            let warn_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
            warn_icon.set_pixel_size(12);
            warn_panel.append(&warn_icon);

            let warn_lbl = gtk4::Label::builder()
                .label(format!("Obsolète · {out_short}"))
                .max_width_chars(15)
                .ellipsize(gtk4::pango::EllipsizeMode::End)
                .build();
            warn_panel.append(&warn_lbl);

            ver_col_box.append(&warn_panel);
        }

        suffix_box.append(&ver_col_box);
    } else {
        if let Some(ref new_ver) = item.update_available {
            let ver_text = format!("{} → {}", item.version, new_ver);
            let ver_label = gtk4::Label::new(Some(&ver_text));
            ver_label.add_css_class("version-update-pill");
            ver_label.set_valign(gtk4::Align::Center);
            ver_label.set_tooltip_text(Some("Une mise à jour est disponible pour ce paquet"));
            suffix_box.append(&ver_label);
        } else {
            let ver_label = gtk4::Label::new(Some(&item.version));
            ver_label.add_css_class("version-pill");
            ver_label.set_valign(gtk4::Align::Center);
            suffix_box.append(&ver_label);
        }
    }

    // Colonne Actions (Info + Installer / Mettre à jour / Désinstaller / Exécuter)
    let actions_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk4::Align::End)
        .valign(gtk4::Align::Center)
        .build();
    if item.aur_meta.is_some() {
        actions_box.set_width_request(COL_ACTIONS_WIDTH);
    }

    let info_btn = gtk4::Button::builder()
        .icon_name("dialog-information-symbolic")
        .valign(gtk4::Align::Center)
        .tooltip_text("Afficher la fiche détaillée, le dépôt GitHub et le README du paquet")
        .css_classes(["flat", "circular"])
        .build();
    info_btn.connect_clicked(move |_| {
        on_info_clicked();
    });
    actions_box.append(&info_btn);

    match &item.source {
        PackageSource::AppImage(path) => {
            if item.update_available.is_some() {
                let upd_btn = gtk4::Button::builder()
                    .label("Mettre à jour")
                    .icon_name("software-update-available-symbolic")
                    .valign(gtk4::Align::Center)
                    .tooltip_text("Télécharger et installer la nouvelle version de cette AppImage dans ~/Applications")
                    .build();
                upd_btn.add_css_class("suggested-action");

                let tx_upd = cmd_tx.clone();
                let upd_item = item.clone();
                upd_btn.connect_clicked(move |btn| {
                    btn.set_sensitive(false);
                    let _ = tx_upd.send(BackendCommand::UpdateAppImage {
                        item: upd_item.clone(),
                    });
                });
                actions_box.append(&upd_btn);
            }

            let run_btn = gtk4::Button::builder()
                .label("Exécuter")
                .icon_name("media-playback-start-symbolic")
                .valign(gtk4::Align::Center)
                .tooltip_text("Rendre exécutable si besoin et lancer cette AppImage")
                .build();
            run_btn.add_css_class("suggested-action");

            let tx_run = cmd_tx.clone();
            let pkg_name = item.name.clone();
            let pkg_path = path.clone();
            run_btn.connect_clicked(move |btn| {
                btn.set_sensitive(false);
                let _ = tx_run.send(BackendCommand::ExecuteAppImage {
                    name: pkg_name.clone(),
                    path: pkg_path.clone(),
                });
                btn.set_sensitive(true);
            });
            actions_box.append(&run_btn);

            let del_btn = gtk4::Button::builder()
                .label("Supprimer")
                .icon_name("user-trash-symbolic")
                .valign(gtk4::Align::Center)
                .tooltip_text("Supprimer ce fichier AppImage de ~/Applications ainsi que son raccourci du menu démarrer")
                .build();
            del_btn.add_css_class("destructive-action");

            let tx_del = cmd_tx.clone();
            let del_name = item.name.clone();
            let del_source = item.source.clone();
            del_btn.connect_clicked(move |btn| {
                btn.set_sensitive(false);
                let _ = tx_del.send(BackendCommand::RemovePackage {
                    name: del_name.clone(),
                    source: del_source.clone(),
                });
            });
            actions_box.append(&del_btn);
        }
        PackageSource::Pacman | PackageSource::Aur | PackageSource::Flatpak => {
            if item.update_available.is_some() {
                let update_btn = gtk4::Button::builder()
                    .label("Mettre à jour")
                    .icon_name("software-update-available-symbolic")
                    .valign(gtk4::Align::Center)
                    .tooltip_text("Installer la nouvelle version de ce paquet avec le gestionnaire d'installation")
                    .build();
                update_btn.add_css_class("suggested-action");

                let tx_upd = cmd_tx.clone();
                let upd_item = item.clone();
                update_btn.connect_clicked(move |btn| {
                    btn.set_sensitive(false);
                    let _ = tx_upd.send(BackendCommand::InstallBatch {
                        items: vec![upd_item.clone()],
                    });
                });
                actions_box.append(&update_btn);
            }

            if item.is_installed {
                if let Some(app_id) = item.flatpak_app_id() {
                    let run_btn = gtk4::Button::builder()
                        .label("Lancer")
                        .icon_name("media-playback-start-symbolic")
                        .valign(gtk4::Align::Center)
                        .tooltip_text("Lancer cette application Flatpak")
                        .build();

                    let tx_run = cmd_tx.clone();
                    let fp_id = app_id.to_string();
                    let fp_name = item.name.clone();
                    run_btn.connect_clicked(move |_| {
                        let _ = tx_run.send(BackendCommand::LaunchFlatpak {
                            app_id: fp_id.clone(),
                            name: fp_name.clone(),
                        });
                    });
                    actions_box.append(&run_btn);
                }

                let remove_btn = gtk4::Button::builder()
                    .label("Désinstaller")
                    .icon_name("user-trash-symbolic")
                    .valign(gtk4::Align::Center)
                    .tooltip_text("Désinstaller ce paquet du système")
                    .build();
                remove_btn.add_css_class("destructive-action");

                let tx_rem = cmd_tx.clone();
                let rem_name = item
                    .flatpak_app_id()
                    .unwrap_or(&item.name)
                    .to_string();
                let rem_source = item.source.clone();
                remove_btn.connect_clicked(move |btn| {
                    btn.set_sensitive(false);
                    let _ = tx_rem.send(BackendCommand::RemovePackage {
                        name: rem_name.clone(),
                        source: rem_source.clone(),
                    });
                });
                actions_box.append(&remove_btn);
            } else {
                let install_btn = gtk4::Button::builder()
                    .label("Installer")
                    .icon_name("list-add-symbolic")
                    .valign(gtk4::Align::Center)
                    .tooltip_text("Installer ce paquet avec le gestionnaire d'installation")
                    .build();
                install_btn.add_css_class("suggested-action");

                let tx_inst = cmd_tx.clone();
                let inst_item = item.clone();
                install_btn.connect_clicked(move |btn| {
                    btn.set_sensitive(false);
                    let _ = tx_inst.send(BackendCommand::InstallBatch {
                        items: vec![inst_item.clone()],
                    });
                });
                actions_box.append(&install_btn);
            }
        }
    }

    suffix_box.append(&actions_box);
    row.add_suffix(&suffix_box);
    row
}
