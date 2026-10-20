use std::cell::RefCell;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend::appimage::format_unix_timestamp;
use crate::backend::details::{markdown_to_pango_markup, PackageDetailedInfo};
use crate::backend::models::PackageItem;
use crate::ui::icons::build_package_icon_widget;

#[derive(Clone)]
pub struct PackageDetailsDialog {
    window: adw::Window,
    title_widget: adw::WindowTitle,
    obsolete_banner: adw::Banner,
    stack: gtk4::Stack,
    loading_spinner: gtk4::Spinner,
    details_page: adw::PreferencesPage,
    active_groups: Rc<RefCell<Vec<adw::PreferencesGroup>>>,
    current_pkg_id: Rc<RefCell<String>>,
}

impl PackageDetailsDialog {
    pub fn new(parent: &adw::ApplicationWindow) -> Self {
        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        let title_widget = adw::WindowTitle::new("Informations du paquet", "Fiche technique & README");
        header_bar.set_title_widget(Some(&title_widget));
        toolbar_view.add_top_bar(&header_bar);

        let obsolete_banner = adw::Banner::builder()
            .revealed(false)
            .build();
        toolbar_view.add_top_bar(&obsolete_banner);

        let stack = gtk4::Stack::builder()
            .transition_type(gtk4::StackTransitionType::Crossfade)
            .transition_duration(150)
            .vexpand(true)
            .hexpand(true)
            .build();

        // Page de chargement
        let loading_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(14)
            .valign(gtk4::Align::Center)
            .halign(gtk4::Align::Center)
            .build();

        let loading_spinner = gtk4::Spinner::builder()
            .spinning(false)
            .width_request(36)
            .height_request(36)
            .build();
        loading_box.append(&loading_spinner);

        let loading_label = gtk4::Label::builder()
            .label("Récupération de la fiche du paquet et du README (GitHub / Amont)…")
            .css_classes(["dim-label"])
            .build();
        loading_box.append(&loading_label);

        stack.add_named(&loading_box, Some("loading"));

        // Page de contenu détaillé
        let details_page = adw::PreferencesPage::new();
        stack.add_named(&details_page, Some("content"));

        toolbar_view.set_content(Some(&stack));

        let window = adw::Window::builder()
            .transient_for(parent)
            .modal(true)
            .hide_on_close(true)
            .title("Informations du paquet")
            .default_width(800)
            .default_height(660)
            .content(&toolbar_view)
            .build();

        Self {
            window,
            title_widget,
            obsolete_banner,
            stack,
            loading_spinner,
            details_page,
            active_groups: Rc::new(RefCell::new(Vec::new())),
            current_pkg_id: Rc::new(RefCell::new(String::new())),
        }
    }

    pub fn show_loading(&self, item: &PackageItem) {
        *self.current_pkg_id.borrow_mut() = item.id.clone();
        self.title_widget
            .set_title(&format!("{} ({})", item.name, item.version));
        self.title_widget.set_subtitle(item.source.label());
        self.obsolete_banner.set_revealed(false);

        for old_group in self.active_groups.borrow_mut().drain(..) {
            self.details_page.remove(&old_group);
        }

        self.loading_spinner.set_spinning(true);
        self.stack.set_visible_child_name("loading");
        self.window.set_visible(true);
        self.window.present();
    }

    pub fn show_details(&self, info: &PackageDetailedInfo) {
        if *self.current_pkg_id.borrow() != info.item.id {
            return;
        }

        self.loading_spinner.set_spinning(false);

        if let Some(out_ts) = info.item.aur_meta.as_ref().and_then(|m| m.out_of_date) {
            let out_date = format_unix_timestamp(out_ts);
            self.obsolete_banner.set_title(&format!(
                "⚠️ Attention : Ce paquet est marqué comme obsolète (Out-of-Date) depuis le {out_date}."
            ));
            self.obsolete_banner.set_revealed(true);
        } else {
            self.obsolete_banner.set_revealed(false);
        }

        for old_group in self.active_groups.borrow_mut().drain(..) {
            self.details_page.remove(&old_group);
        }

        let mut groups = Vec::new();

        // --- 1. En-tête & Liens externes (GitHub / Site officiel) ---
        let header_group = adw::PreferencesGroup::new();

        let hero_row = adw::ActionRow::builder()
            .title(glib::markup_escape_text(&info.item.name).as_str())
            .subtitle(glib::markup_escape_text(&info.item.description).as_str())
            .title_lines(1)
            .subtitle_lines(4)
            .build();

        let icon = build_package_icon_widget(&info.item);
        icon.set_pixel_size(52);
        icon.set_valign(gtk4::Align::Center);
        hero_row.add_prefix(&icon);

        let links_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        links_box.set_valign(gtk4::Align::Center);

        if let Some(ref gh_url) = info.github_url {
            let gh_btn = gtk4::Button::builder()
                .label("GitHub")
                .icon_name("web-browser-symbolic")
                .valign(gtk4::Align::Center)
                .tooltip_text(gh_url)
                .build();
            gh_btn.add_css_class("suggested-action");
            let url_clone = gh_url.clone();
            let win_clone = self.window.clone();
            gh_btn.connect_clicked(move |_| {
                let launcher = gtk4::UriLauncher::new(&url_clone);
                launcher.launch(Some(&win_clone), gtk4::gio::Cancellable::NONE, |_| {});
            });
            links_box.append(&gh_btn);
        }

        if let Some(ref up_url) = info.upstream_url {
            if info.github_url.as_ref() != Some(up_url) {
                let site_btn = gtk4::Button::builder()
                    .label("Site officiel")
                    .icon_name("globe-symbolic")
                    .valign(gtk4::Align::Center)
                    .tooltip_text(up_url)
                    .build();
                let url_clone = up_url.clone();
                let win_clone = self.window.clone();
                site_btn.connect_clicked(move |_| {
                    let launcher = gtk4::UriLauncher::new(&url_clone);
                    launcher.launch(Some(&win_clone), gtk4::gio::Cancellable::NONE, |_| {});
                });
                links_box.append(&site_btn);
            }
        }

        hero_row.add_suffix(&links_box);
        header_group.add(&hero_row);
        self.details_page.add(&header_group);
        groups.push(header_group);

        // --- 2. README / Documentation intégrée (GitHub / GitLab / Locale) ---
        if let Some(ref readme_md) = info.readme_markdown {
            let source_title = info
                .readme_source_label
                .as_deref()
                .unwrap_or("README & Documentation");
            let esc_source_title = glib::markup_escape_text(source_title);

            let readme_group = adw::PreferencesGroup::builder()
                .title(esc_source_title.as_str())
                .description("Documentation extraite automatiquement du dépôt du projet")
                .build();

            let pango_markup = markdown_to_pango_markup(readme_md);
            let readme_label = gtk4::Label::builder()
                .use_markup(true)
                .label(&pango_markup)
                .wrap(true)
                .wrap_mode(gtk4::pango::WrapMode::WordChar)
                .xalign(0.0)
                .yalign(0.0)
                .selectable(true)
                .margin_top(14)
                .margin_bottom(14)
                .margin_start(16)
                .margin_end(16)
                .build();
            // Si le balisage Pango échoue sur un caractère inhabituel, repli sur texte brut
            if readme_label.text().is_empty() && !readme_md.is_empty() {
                readme_label.set_use_markup(false);
                readme_label.set_label(readme_md);
            }

            let readme_frame = gtk4::Frame::builder()
                .child(&readme_label)
                .build();

            readme_group.add(&readme_frame);
            self.details_page.add(&readme_group);
            groups.push(readme_group);
        }

        // --- 3. Fiche technique & Métadonnées ---
        let meta_group = adw::PreferencesGroup::builder()
            .title("Fiche technique")
            .build();

        add_meta_row(&meta_group, "Version", &info.item.version);
        add_meta_row(&meta_group, "Source", &info.repository);
        add_meta_row(&meta_group, "Licence(s)", &info.licenses);

        if let Some(ref aur_m) = info.item.aur_meta {
            add_meta_row(&meta_group, "Votes AUR", &format!("★ {}", aur_m.votes));
            add_meta_row(
                &meta_group,
                "Popularité AUR",
                &format!("{:.2}", aur_m.popularity),
            );
            if let Some(out_ts) = aur_m.out_of_date {
                add_meta_row(
                    &meta_group,
                    "Statut AUR",
                    &format!("⚠️ Marqué comme obsolète depuis le {}", format_unix_timestamp(out_ts)),
                );
            }
        }

        if let Some(ref dl_sz) = info.download_size {
            add_meta_row(&meta_group, "Taille de téléchargement", dl_sz);
        }
        if let Some(ref inst_sz) = info.installed_size {
            add_meta_row(&meta_group, "Taille sur le disque", inst_sz);
        }
        if let Some(ref maint) = info.maintainer_or_packager {
            add_meta_row(&meta_group, "Mainteneur / Packager", maint);
        }
        if let Some(ref bdate) = info.build_or_modified_date {
            add_meta_row(&meta_group, "Date de build / modification", bdate);
        }
        if let Some(ref url) = info.upstream_url {
            add_meta_row(&meta_group, "URL du projet", url);
        }

        self.details_page.add(&meta_group);
        groups.push(meta_group);

        // --- 4. Dépendances directes & optionnelles ---
        if !info.dependencies.is_empty() || !info.optional_deps.is_empty() {
            let deps_group = adw::PreferencesGroup::builder()
                .title("Dépendances du paquet")
                .build();

            if !info.dependencies.is_empty() {
                let joined_deps = info.dependencies.join(", ");
                add_meta_row(
                    &deps_group,
                    &format!("Dépendances requises ({})", info.dependencies.len()),
                    &joined_deps,
                );
            }

            if !info.optional_deps.is_empty() {
                let joined_opts = info.optional_deps.join("\n");
                add_meta_row(
                    &deps_group,
                    &format!("Dépendances optionnelles ({})", info.optional_deps.len()),
                    &joined_opts,
                );
            }

            self.details_page.add(&deps_group);
            groups.push(deps_group);
        }

        *self.active_groups.borrow_mut() = groups;
        self.stack.set_visible_child_name("content");
    }
}

fn add_meta_row(group: &adw::PreferencesGroup, title: &str, value: &str) {
    let esc_title = glib::markup_escape_text(title);
    let esc_val = glib::markup_escape_text(value);
    let row = adw::ActionRow::builder()
        .title(esc_title.as_str())
        .subtitle(esc_val.as_str())
        .subtitle_selectable(true)
        .build();
    group.add(&row);
}
