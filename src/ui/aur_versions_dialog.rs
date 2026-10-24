use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend::appimage::format_unix_timestamp;
use crate::backend::models::{AurVersionInfo, PackageItem};
use crate::ui::messages::BackendCommand;

#[derive(Clone)]
pub struct AurVersionsDialog {
    window: adw::Window,
    title_widget: adw::WindowTitle,
    stack: gtk4::Stack,
    spinner: gtk4::Spinner,
    page: adw::PreferencesPage,
    active_groups: Rc<RefCell<Vec<adw::PreferencesGroup>>>,
    current_package: Rc<RefCell<Option<PackageItem>>>,
}

impl AurVersionsDialog {
    pub fn new(parent: &adw::ApplicationWindow) -> Self {
        let toolbar_view = adw::ToolbarView::new();
        let header_bar = adw::HeaderBar::new();
        let title_widget = adw::WindowTitle::new(
            "Historique des versions",
            "Versions antérieures disponibles sur l'AUR",
        );
        header_bar.set_title_widget(Some(&title_widget));
        toolbar_view.add_top_bar(&header_bar);

        let stack = gtk4::Stack::builder()
            .transition_type(gtk4::StackTransitionType::Crossfade)
            .transition_duration(150)
            .vexpand(true)
            .hexpand(true)
            .build();

        let loading_box = gtk4::Box::builder()
            .orientation(gtk4::Orientation::Vertical)
            .spacing(14)
            .valign(gtk4::Align::Center)
            .halign(gtk4::Align::Center)
            .build();
        let spinner = gtk4::Spinner::builder()
            .spinning(false)
            .width_request(36)
            .height_request(36)
            .build();
        loading_box.append(&spinner);
        let loading_label = gtk4::Label::builder()
            .label("Lecture de l'historique git du paquet AUR…")
            .css_classes(["dim-label"])
            .build();
        loading_box.append(&loading_label);
        stack.add_named(&loading_box, Some("loading"));

        let page = adw::PreferencesPage::new();
        stack.add_named(&page, Some("content"));

        toolbar_view.set_content(Some(&stack));

        let window = adw::Window::builder()
            .transient_for(parent)
            .modal(true)
            .hide_on_close(true)
            .title("Historique des versions")
            .default_width(600)
            .default_height(560)
            .content(&toolbar_view)
            .build();

        Self {
            window,
            title_widget,
            stack,
            spinner,
            page,
            active_groups: Rc::new(RefCell::new(Vec::new())),
            current_package: Rc::new(RefCell::new(None)),
        }
    }

    pub fn show_loading(&self, item: &PackageItem) {
        *self.current_package.borrow_mut() = Some(item.clone());
        self.title_widget
            .set_title(&format!("{} — versions", item.name));
        self.title_widget
            .set_subtitle(&format!("Version actuelle : {}", item.version));

        for group in self.active_groups.borrow_mut().drain(..) {
            self.page.remove(&group);
        }

        self.spinner.set_spinning(true);
        self.stack.set_visible_child_name("loading");
        self.window.set_visible(true);
        self.window.present();
    }

    pub fn show_versions(
        &self,
        item: &PackageItem,
        package_base: &str,
        versions: &[AurVersionInfo],
        cmd_tx: &Sender<BackendCommand>,
    ) {
        let current = self.current_package.borrow().clone();
        if current.as_ref().map(|pkg| pkg.name.as_str()) != Some(item.name.as_str()) {
            return;
        }

        self.spinner.set_spinning(false);
        for group in self.active_groups.borrow_mut().drain(..) {
            self.page.remove(&group);
        }

        let group = adw::PreferencesGroup::builder()
            .title("Versions disponibles dans l'historique AUR")
            .description(
                "Installer une version antérieure compile son PKGBUILD d'origine. \
                 Les dépendances actuelles doivent rester compatibles.",
            )
            .build();

        if versions.is_empty() {
            group.add(
                &adw::ActionRow::builder()
                    .title("Aucune version antérieure trouvée")
                    .subtitle("L'historique du PKGBUILD ne contient pas de version exploitable.")
                    .build(),
            );
        } else {
            for version in versions {
                let subtitle = match version.timestamp {
                    Some(timestamp) => format!(
                        "{} · commit {}",
                        format_unix_timestamp(timestamp.max(0) as u64),
                        &version.commit[..version.commit.len().min(10)]
                    ),
                    None => format!("commit {}", &version.commit[..version.commit.len().min(10)]),
                };

                let row = adw::ActionRow::builder()
                    .title(&version.version)
                    .subtitle(&subtitle)
                    .build();

                if version.version == item.version {
                    let installed = gtk4::Label::builder()
                        .label("Installée")
                        .css_classes(["dim-label"])
                        .valign(gtk4::Align::Center)
                        .build();
                    row.add_suffix(&installed);
                } else {
                    let install_btn = gtk4::Button::builder()
                        .label("Installer")
                        .icon_name("software-install-symbolic")
                        .valign(gtk4::Align::Center)
                        .tooltip_text("Compiler et installer cette version historique via yay -Bi")
                        .build();
                    install_btn.add_css_class("suggested-action");

                    let tx = cmd_tx.clone();
                    let package = item.name.clone();
                    let package_base = package_base.to_string();
                    let version_number = version.version.clone();
                    let commit = version.commit.clone();
                    let window = self.window.clone();
                    install_btn.connect_clicked(move |btn| {
                        btn.set_sensitive(false);
                        let _ = tx.send(BackendCommand::InstallAurVersion {
                            package: package.clone(),
                            package_base: package_base.clone(),
                            version: version_number.clone(),
                            commit: commit.clone(),
                        });
                        window.close();
                    });
                    row.add_suffix(&install_btn);
                }

                group.add(&row);
            }
        }

        self.page.add(&group);
        self.active_groups.borrow_mut().push(group);
        self.stack.set_visible_child_name("content");
    }

    pub fn show_versions_for_current(
        &self,
        package_base: &str,
        versions: &[AurVersionInfo],
        cmd_tx: &Sender<BackendCommand>,
    ) {
        let Some(item) = self.current_package.borrow().clone() else {
            return;
        };
        self.show_versions(&item, package_base, versions, cmd_tx);
    }

    pub fn show_error(&self, detail: &str) {
        self.spinner.set_spinning(false);
        for group in self.active_groups.borrow_mut().drain(..) {
            self.page.remove(&group);
        }

        let group = adw::PreferencesGroup::builder()
            .title("Historique indisponible")
            .description(detail)
            .build();
        group.add(
            &adw::ActionRow::builder()
                .title("Impossible de charger les versions")
                .subtitle("Vérifiez la connexion à l'AUR et que git est installé.")
                .build(),
        );
        self.page.add(&group);
        self.active_groups.borrow_mut().push(group);
        self.stack.set_visible_child_name("content");
    }
}
