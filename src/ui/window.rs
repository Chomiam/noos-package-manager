use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend;
use crate::backend::elevation::Elevation;
use crate::backend::models::{
    AurHelper, AurSortMode, InstallCancelHandle, PackageItem, PackageSource,
    RecommendationCategory, SidebarCategory,
};
use crate::ui::aur_versions_dialog::AurVersionsDialog;
use crate::ui::details_dialog::PackageDetailsDialog;
use crate::ui::install_dialog::InstallManagerUi;
use crate::ui::messages::{BackendCommand, UiEvent};
use crate::ui::row::{build_aur_columns_header_row, build_package_action_row};
use crate::ui::settings_page::SettingsView;

const MAX_RENDERED_ROWS: usize = 250;
const AUR_PAGE_SIZE: usize = 8;
const SIDEBAR_EXPANDED_MIN_WIDTH: f64 = 182.0;
const SIDEBAR_EXPANDED_MAX_WIDTH: f64 = 200.0;
const SIDEBAR_EXPANDED_FRACTION: f64 = 0.165;
const SIDEBAR_COLLAPSED_WIDTH: f64 = 58.0;
const SIDEBAR_COLLAPSED_FRACTION: f64 = 0.05;

#[derive(Default)]
struct AppUiState {
    catalog_packages: Vec<PackageItem>,
    recommendations: Vec<(RecommendationCategory, Vec<PackageItem>)>,
    flatpak_recommendations: Vec<(RecommendationCategory, Vec<PackageItem>)>,
    search_results: Option<Vec<PackageItem>>,
    selected_packages: BTreeMap<String, PackageItem>,
    active_query: String,
    selected_category: SidebarCategory,
    aur_sort_mode: AurSortMode,
    aur_current_page: usize,
    aur_next_wave_index: usize,
    aur_loading_more: bool,
    aur_advance_page_on_load: bool,
    updates_count: usize,
    aur_helper: Option<AurHelper>,
    active_groups: Vec<adw::PreferencesGroup>,
    debounce_source: Option<glib::SourceId>,
    lock_dialog_open: bool,
    lock_dismissed_while_present: bool,
    sidebar_collapsed: bool,
    /// Historique des notifications utilisateur (erreurs, fin de mises à jour, alertes).
    /// Affiché via la cloche dans la barre supérieure.
    notifications: Vec<String>,
    /// Nombre de notifications non lues (affiché sur la cloche).
    notification_unread: usize,
    /// Paquets dont les mises à jour sont ignorées par l'utilisateur.
    ignored_updates: BTreeSet<String>,
    /// Statut noyau retourné par needrestart lors de la dernière mise à jour.
    pending_kernel_status: Option<u32>,
}

struct SidebarRowWidgets {
    row: gtk4::ListBoxRow,
    hbox: gtk4::Box,
    icon: gtk4::Image,
    title_label: gtk4::Label,
    category: SidebarCategory,
}

struct CategoryBadgeWidgets {
    all_label: gtk4::Label,
    installed_label: gtk4::Label,
    updates_label: gtk4::Label,
    pacman_label: gtk4::Label,
    aur_label: gtk4::Label,
    flatpak_label: gtk4::Label,
    appimages_label: gtk4::Label,
    sidebar_rows: Vec<SidebarRowWidgets>,
}

#[derive(Clone)]
struct HeaderActionButtons {
    refresh_btn: gtk4::Button,
    batch_install_btn: gtk4::Button,
    clear_selection_btn: gtk4::Button,
    update_all_btn: gtk4::Button,
    aur_sort_box: gtk4::Box,
    loading_status_page: adw::StatusPage,
    empty_check_updates_btn: gtk4::Button,
    details_dialog: PackageDetailsDialog,
    aur_versions_dialog: AurVersionsDialog,
    settings_view: SettingsView,
}

pub fn build_main_window(app: &adw::Application) -> adw::ApplicationWindow {
    let (cmd_tx, cmd_rx) = mpsc::channel::<BackendCommand>();
    let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
    let cancel_handle = Arc::new(InstallCancelHandle::new());

    spawn_backend_worker(cmd_rx, ui_tx, Arc::clone(&cancel_handle));

    let saved_settings = backend::settings::load_settings();
    let state = Rc::new(RefCell::new(AppUiState {
        aur_next_wave_index: backend::aur::INITIAL_AUR_DISCOVERY_WAVES_COUNT,
        ignored_updates: saved_settings.ignored_updates,
        ..AppUiState::default()
    }));

    let toast_overlay = adw::ToastOverlay::new();

    let split_view = adw::NavigationSplitView::builder()
        .min_sidebar_width(SIDEBAR_EXPANDED_MIN_WIDTH)
        .max_sidebar_width(SIDEBAR_EXPANDED_MAX_WIDTH)
        .sidebar_width_fraction(SIDEBAR_EXPANDED_FRACTION)
        .build();

    // --- Construction de la Sidebar ---
    let sidebar_toolbar = adw::ToolbarView::new();
    let sidebar_header = adw::HeaderBar::builder()
        .show_start_title_buttons(true)
        .show_end_title_buttons(false)
        .build();

    let sidebar_header_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .hexpand(true)
        .halign(gtk4::Align::End)
        .build();

    let sidebar_toggle_btn = gtk4::Button::builder()
        .icon_name(resolve_sidebar_toggle_icon(false))
        .tooltip_text("Réduire le volet latéral (afficher uniquement les icônes)")
        .css_classes(["flat"])
        .build();
    sidebar_header_box.append(&sidebar_toggle_btn);
    sidebar_header.set_title_widget(Some(&sidebar_header_box));
    sidebar_toolbar.add_top_bar(&sidebar_header);

    let sidebar_list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(4)
        .margin_end(4)
        .build();

    let all_badge = create_sidebar_badge(false);
    all_badge.set_visible(false);
    let installed_badge = create_sidebar_badge(false);
    let updates_badge = create_sidebar_badge(true);
    let pacman_badge = create_sidebar_badge(false);
    pacman_badge.set_visible(false);
    let aur_badge = create_sidebar_badge(false);
    aur_badge.set_visible(false);
    let flatpak_badge = create_sidebar_badge(false);
    flatpak_badge.set_visible(false);
    let appimages_badge = create_sidebar_badge(false);

    let mut sidebar_rows = Vec::with_capacity(SidebarCategory::ALL.len());
    for category in SidebarCategory::ALL {
        let badge_ref = match category {
            SidebarCategory::All => &all_badge,
            SidebarCategory::Installed => &installed_badge,
            SidebarCategory::Updates => &updates_badge,
            SidebarCategory::Pacman => &pacman_badge,
            SidebarCategory::Aur => &aur_badge,
            SidebarCategory::Flatpak => &flatpak_badge,
            SidebarCategory::AppImages => &appimages_badge,
        };
        let row_widgets = create_sidebar_row(category, badge_ref);
        sidebar_list.append(&row_widgets.row);
        sidebar_rows.push(row_widgets);
    }

    if let Some(first_row) = sidebar_list.row_at_index(0) {
        sidebar_list.select_row(Some(&first_row));
    }

    let sidebar_scroll = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&sidebar_list)
        .build();
    sidebar_toolbar.set_content(Some(&sidebar_scroll));

    // Version de l'application, affichée en bas à gauche de la fenêtre.
    let version_label = gtk4::Label::builder()
        .label(format!("v{}", env!("CARGO_PKG_VERSION")))
        .halign(gtk4::Align::Start)
        .margin_start(10)
        .margin_end(8)
        .margin_top(6)
        .margin_bottom(6)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .max_width_chars(12)
        .css_classes(["dim-label", "caption"])
        .tooltip_text(format!(
            "noos-package-manager v{}",
            env!("CARGO_PKG_VERSION")
        ))
        .build();

    let settings_button = gtk4::Button::builder()
        .css_classes(["flat"])
        .halign(gtk4::Align::Fill)
        .tooltip_text("Ouvrir les paramètres de l'application")
        .build();

    let settings_content = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(10)
        .margin_start(6)
        .margin_end(6)
        .build();
    let settings_icon = gtk4::Image::from_icon_name("emblem-system-symbolic");
    let settings_label = gtk4::Label::builder()
        .label("Paramètres")
        .xalign(0.0)
        .hexpand(true)
        .build();
    settings_content.append(&settings_icon);
    settings_content.append(&settings_label);
    settings_button.set_child(Some(&settings_content));

    let sidebar_footer = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(2)
        .build();
    sidebar_footer.append(&settings_button);
    sidebar_footer.append(&version_label);
    sidebar_toolbar.add_bottom_bar(&sidebar_footer);

    let sidebar_page = adw::NavigationPage::builder()
        .title("Gestionnaire de paquets")
        .tag("sidebar")
        .child(&sidebar_toolbar)
        .build();

    // --- Construction de la Vue de Contenu ---
    let content_toolbar = adw::ToolbarView::new();
    let content_header = adw::HeaderBar::new();

    let collapsed_window_controls = gtk4::WindowControls::builder()
        .side(gtk4::PackType::Start)
        .visible(false)
        .build();
    content_header.pack_start(&collapsed_window_controls);

    let app_header_title = gtk4::Label::builder()
        .label("Gestionnaire de paquets")
        .valign(gtk4::Align::Center)
        .margin_start(4)
        .margin_end(6)
        .css_classes(["heading"])
        .build();
    content_header.pack_start(&app_header_title);

    let search_entry = gtk4::SearchEntry::builder()
        .placeholder_text("Rechercher un paquet (Pacman, AUR, Flatpak, AppImage)…")
        .hexpand(true)
        .width_chars(30)
        .build();
    content_header.set_title_widget(Some(&search_entry));

    let spinner = gtk4::Spinner::builder()
        .spinning(false)
        .visible(false)
        .valign(gtk4::Align::Center)
        .margin_start(6)
        .margin_end(6)
        .tooltip_text("Opération en cours…")
        .build();
    content_header.pack_start(&spinner);

    // Cloche de notifications (erreurs, fin de mises à jour, alertes système)
    let notification_btn = gtk4::Button::builder()
        .icon_name("notification-symbolic")
        .tooltip_text("Notifications (erreurs, fin de mises à jour, alertes)")
        .css_classes(["flat"])
        .build();
    let notification_badge = gtk4::Label::builder()
        .label("0")
        .visible(false)
        .valign(gtk4::Align::Center)
        .css_classes(["update-badge"])
        .build();
    content_header.pack_start(&notification_btn);
    content_header.pack_start(&notification_badge);

    // Bouton d'ouverture du Gestionnaire d'installation
    let install_mgr_btn = gtk4::Button::builder()
        .icon_name("folder-download-symbolic")
        .tooltip_text("Ouvrir le gestionnaire d'installation et de progression des dépendances")
        .build();
    content_header.pack_start(&install_mgr_btn);

    let refresh_btn = gtk4::Button::builder()
        .icon_name("view-refresh-symbolic")
        .tooltip_text("Rafraîchir la liste des paquets et vérifier les mises à jour")
        .build();
    content_header.pack_end(&refresh_btn);

    // Bouton global "Installer (N)" pour les paquets cochés
    let batch_install_btn = gtk4::Button::builder()
        .label("Installer (0)")
        .sensitive(false)
        .tooltip_text("Installer tous les paquets cochés et leurs dépendances")
        .build();
    batch_install_btn.add_css_class("suggested-action");
    content_header.pack_end(&batch_install_btn);

    // Bouton pour tout décocher
    let clear_selection_btn = gtk4::Button::builder()
        .icon_name("edit-clear-all-symbolic")
        .visible(false)
        .tooltip_text("Tout décocher")
        .build();
    content_header.pack_end(&clear_selection_btn);

    let update_all_btn = gtk4::Button::builder()
        .label("Tout mettre à jour")
        .icon_name("software-update-available-symbolic")
        .visible(false)
        .tooltip_text("Mettre à jour tous les paquets système Pacman et AUR (pacman -Syu + yay -Sua)")
        .build();
    content_header.pack_end(&update_all_btn);

    // Sélecteur de tri AUR (Popularité par défaut, Votes, Mis à jour récemment, Nom)
    let aur_sort_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk4::Align::Center)
        .visible(false)
        .build();
    let aur_sort_label = gtk4::Label::builder()
        .label("Tri AUR :")
        .css_classes(["dim-label"])
        .build();
    aur_sort_box.append(&aur_sort_label);

    let sort_strings: [&str; 4] = [
        AurSortMode::Popularity.label(),
        AurSortMode::Votes.label(),
        AurSortMode::RecentlyUpdated.label(),
        AurSortMode::Name.label(),
    ];
    let aur_sort_dropdown = gtk4::DropDown::from_strings(&sort_strings);
    aur_sort_dropdown.set_selected(0);
    aur_sort_dropdown.set_tooltip_text(Some(
        "Choisir le critère de tri des paquets AUR (par défaut : Popularité)",
    ));
    aur_sort_box.append(&aur_sort_dropdown);
    content_header.pack_end(&aur_sort_box);

    content_toolbar.add_top_bar(&content_header);

    let error_banner = adw::Banner::builder()
        .revealed(false)
        .button_label("Fermer")
        .build();
    error_banner.connect_button_clicked(|banner| {
        banner.set_revealed(false);
    });
    content_toolbar.add_top_bar(&error_banner);

    let content_stack = gtk4::Stack::builder()
        .transition_type(gtk4::StackTransitionType::Crossfade)
        .transition_duration(150)
        .vexpand(true)
        .hexpand(true)
        .build();

    let preferences_page = adw::PreferencesPage::new();
    widen_preferences_page_clamp(preferences_page.upcast_ref::<gtk4::Widget>());
    content_stack.add_named(&preferences_page, Some("list"));

    let empty_check_updates_btn = gtk4::Button::builder()
        .label("Vérifier les mises à jour")
        .icon_name("view-refresh-symbolic")
        .halign(gtk4::Align::Center)
        .visible(false)
        .css_classes(["suggested-action", "pill"])
        .tooltip_text("Synchroniser les dépôts Pacman, l'API AUR, Flathub et GitHub Releases")
        .build();

    let empty_status_page = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title("Aucun paquet à afficher")
        .description("Modifiez votre recherche ou sélectionnez une autre catégorie.")
        .child(&empty_check_updates_btn)
        .vexpand(true)
        .build();
    content_stack.add_named(&empty_status_page, Some("empty"));

    let loading_status_page = adw::StatusPage::builder()
        .icon_name("emblem-synchronizing-symbolic")
        .title("Synchronisation des paquets…")
        .description("Analyse des dépôts Pacman, de l'API AUR RPC v5 et des AppImages locales.")
        .vexpand(true)
        .build();
    content_stack.add_named(&loading_status_page, Some("loading"));
    content_stack.set_visible_child_name("loading");

    content_toolbar.set_content(Some(&content_stack));

    let content_page = adw::NavigationPage::builder()
        .title("Tous les paquets")
        .tag("content")
        .child(&content_toolbar)
        .build();

    split_view.set_sidebar(Some(&sidebar_page));
    split_view.set_content(Some(&content_page));



    toast_overlay.set_child(Some(&split_view));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Gestionnaire de paquets")
        .icon_name("noos-package-manager")
        .default_width(1180)
        .default_height(760)
        .width_request(760)
        .height_request(500)
        .content(&toast_overlay)
        .build();

    // Fenêtre du Gestionnaire d'installation et Fenêtre d'informations détaillées
    let install_manager = InstallManagerUi::new(&window, Arc::clone(&cancel_handle));
    let details_dialog = PackageDetailsDialog::new(&window);
    let aur_versions_dialog = AurVersionsDialog::new(&window);
    let settings_view = SettingsView::new(&cmd_tx);
    widen_preferences_page_clamp(settings_view.root().upcast_ref::<gtk4::Widget>());
    content_stack.add_named(
        settings_view.root().upcast_ref::<gtk4::Widget>(),
        Some("settings"),
    );

    {
        let stack = content_stack.clone();
        let nav = content_page.clone();
        let list = sidebar_list.clone();
        let header_title = app_header_title.clone();
        let settings_view_ref = settings_view.clone();
        settings_button.connect_clicked(move |_| {
            list.unselect_all();
            nav.set_title("Paramètres");
            header_title.set_label("Paramètres");
            stack.set_visible_child_name("settings");
            settings_view_ref.refresh_cache_size();
            settings_view_ref.start_auto_refresh();
        });
    }



    // Alerte à la fermeture de l'application si une installation est en cours
    {
        let mgr_for_close = install_manager.clone();
        window.connect_close_request(move |win| {
            if !mgr_for_close.is_installing() {
                return glib::Propagation::Proceed;
            }

            let dialog = adw::MessageDialog::builder()
                .transient_for(win)
                .modal(true)
                .heading("Installation en cours")
                .body("Une installation de paquets est actuellement en cours. Voulez-vous vraiment fermer l'application et annuler l'installation en cours ?")
                .build();

            dialog.add_response("stay", "Rester et continuer");
            dialog.add_response("quit_and_cancel", "Fermer et annuler l'installation");
            dialog.set_default_response(Some("stay"));
            dialog.set_close_response("stay");
            dialog.set_response_appearance(
                "quit_and_cancel",
                adw::ResponseAppearance::Destructive,
            );

            let win_clone = win.clone();
            let mgr_clone = mgr_for_close.clone();
            dialog.connect_response(None, move |dlg, response| {
                dlg.close();
                if response == "quit_and_cancel" {
                    mgr_clone.cancel_installation();
                    win_clone.destroy();
                }
            });

            dialog.present();
            glib::Propagation::Stop
        });
    }

    {
        let mgr_ref = install_manager.clone();
        install_mgr_btn.connect_clicked(move |_| {
            mgr_ref.present();
        });
    }

    let badge_widgets = Rc::new(CategoryBadgeWidgets {
        all_label: all_badge,
        installed_label: installed_badge,
        updates_label: updates_badge,
        pacman_label: pacman_badge,
        aur_label: aur_badge,
        flatpak_label: flatpak_badge,
        appimages_label: appimages_badge,
        sidebar_rows,
    });

    // --- Connexion du bouton de réduction / dépliage du volet latéral gauche ---
    {
        let state_toggle = Rc::clone(&state);
        let split_ref = split_view.clone();
        let list_ref = sidebar_list.clone();
        let sidebar_header_ref = sidebar_header.clone();
        let sidebar_hbox_ref = sidebar_header_box.clone();
        let collapsed_wc_ref = collapsed_window_controls;
        let badges_ref = Rc::clone(&badge_widgets);
        let version_label_ref = version_label.clone();
        let settings_label_ref = settings_label.clone();
        let settings_content_ref = settings_content.clone();

        sidebar_toggle_btn.connect_clicked(move |btn| {
            let (is_collapsed, updates_count) = {
                let mut st = state_toggle.borrow_mut();
                st.sidebar_collapsed = !st.sidebar_collapsed;
                let upd = st
                    .catalog_packages
                    .iter()
                    .filter(|p| {
                        p.update_available.is_some() && !st.ignored_updates.contains(&p.id)
                    })
                    .count();
                (st.sidebar_collapsed, upd)
            };

            apply_sidebar_collapsed_state(
                &split_ref,
                &list_ref,
                &sidebar_header_ref,
                &sidebar_hbox_ref,
                &collapsed_wc_ref,
                btn,
                &badges_ref,
                &version_label_ref,
                &settings_label_ref,
                &settings_content_ref,
                is_collapsed,
                updates_count,
            );
        });
    }

    let header_actions = HeaderActionButtons {
        refresh_btn: refresh_btn.clone(),
        batch_install_btn: batch_install_btn.clone(),
        clear_selection_btn: clear_selection_btn.clone(),
        update_all_btn: update_all_btn.clone(),
        aur_sort_box: aur_sort_box.clone(),
        loading_status_page: loading_status_page.clone(),
        empty_check_updates_btn: empty_check_updates_btn.clone(),
        details_dialog,
        aur_versions_dialog,
        settings_view,
    };

    // --- Connexion du sélecteur de tri AUR ---
    {
        let state_sort = Rc::clone(&state);
        let prefs_page = preferences_page.clone();
        let stack = content_stack.clone();
        let empty_page = empty_status_page.clone();
        let badges = Rc::clone(&badge_widgets);
        let actions = header_actions.clone();
        let tx = cmd_tx.clone();

        aur_sort_dropdown.connect_selected_notify(move |dd| {
            let mode = AurSortMode::from_index(dd.selected());
            {
                let mut st = state_sort.borrow_mut();
                st.aur_sort_mode = mode;
                st.aur_current_page = 0;
            }
            rebuild_package_list_ui(
                &state_sort,
                &prefs_page,
                &stack,
                &empty_page,
                &badges,
                &actions,
                &tx,
            );
        });
    }

    // --- Connexion du bouton global "Installer (N)" ---
    {
        let state_batch = Rc::clone(&state);
        let tx_batch = cmd_tx.clone();
        batch_install_btn.connect_clicked(move |btn| {
            let items_to_install: Vec<PackageItem> = {
                let st = state_batch.borrow();
                st.selected_packages.values().cloned().collect()
            };
            if items_to_install.is_empty() {
                return;
            }
            btn.set_sensitive(false);
            let _ = tx_batch.send(BackendCommand::PrepareInstallBatch {
                items: items_to_install,
            });
        });
    }

    // --- Connexion du bouton "Tout décocher" ---
    {
        let state_clear = Rc::clone(&state);
        let prefs_page = preferences_page.clone();
        let stack = content_stack.clone();
        let empty_page = empty_status_page.clone();
        let badges = Rc::clone(&badge_widgets);
        let actions = header_actions.clone();
        let tx = cmd_tx.clone();

        clear_selection_btn.connect_clicked(move |_| {
            {
                let mut st = state_clear.borrow_mut();
                st.selected_packages.clear();
            }
            update_selection_buttons(&state_clear, &actions);
            rebuild_package_list_ui(
                &state_clear,
                &prefs_page,
                &stack,
                &empty_page,
                &badges,
                &actions,
                &tx,
            );
        });
    }

    // --- Connexion de la sélection de catégorie dans la Sidebar ---
    {
        let state_cat = Rc::clone(&state);
        let prefs_page = preferences_page.clone();
        let stack = content_stack.clone();
        let empty_page = empty_status_page.clone();
        let nav_content = content_page.clone();
        let tx = cmd_tx.clone();
        let badges = Rc::clone(&badge_widgets);
        let actions = header_actions.clone();
        let header_title = app_header_title.clone();

        sidebar_list.connect_row_selected(move |_, opt_row| {
            let Some(row) = opt_row else {
                return;
            };
            let idx = row.index();
            if idx < 0 {
                return;
            }
            let category = SidebarCategory::ALL
                .get(idx as usize)
                .copied()
                .unwrap_or(SidebarCategory::All);

            {
                let mut st = state_cat.borrow_mut();
                st.selected_category = category;
                st.aur_current_page = 0;
            }
            nav_content.set_title(category.title());
            header_title.set_label(category.title());

            rebuild_package_list_ui(
                &state_cat,
                &prefs_page,
                &stack,
                &empty_page,
                &badges,
                &actions,
                &tx,
            );
        });
    }

    // --- Connexion de la barre de recherche avec debounce non-bloquant ---
    {
        let state_search = Rc::clone(&state);
        let tx_search = cmd_tx.clone();
        let prefs_page = preferences_page.clone();
        let stack = content_stack.clone();
        let empty_page = empty_status_page.clone();
        let badges = Rc::clone(&badge_widgets);
        let actions = header_actions.clone();

        search_entry.connect_search_changed(move |entry| {
            let text = entry.text().trim().to_string();

            let prev_source = {
                let mut st = state_search.borrow_mut();
                st.active_query = text.clone();
                st.aur_current_page = 0;
                // Invalide les résultats de la requête précédente : tant que la nouvelle
                // recherche distante n'a pas répondu, on filtre le catalogue local avec le
                // texte courant plutôt que d'afficher des résultats devenus obsolètes.
                st.search_results = None;
                st.debounce_source.take()
            };

            if let Some(source_id) = prev_source {
                source_id.remove();
            }

            if text.is_empty() {
                rebuild_package_list_ui(
                    &state_search,
                    &prefs_page,
                    &stack,
                    &empty_page,
                    &badges,
                    &actions,
                    &tx_search,
                );
                return;
            }

            rebuild_package_list_ui(
                &state_search,
                &prefs_page,
                &stack,
                &empty_page,
                &badges,
                &actions,
                &tx_search,
            );

            let state_timer = Rc::clone(&state_search);
            let tx_timer = tx_search.clone();
            let query_for_timer = text;

            let source_id =
                glib::timeout_add_local_once(Duration::from_millis(350), move || {
                    {
                        let mut st = state_timer.borrow_mut();
                        st.debounce_source = None;
                    }
                    let _ = tx_timer.send(BackendCommand::Search {
                        query: query_for_timer,
                    });
                });

            state_search.borrow_mut().debounce_source = Some(source_id);
        });
    }

    // --- Connexion du bouton de rafraîchissement / vérification des mises à jour ---
    {
        let tx_refresh = cmd_tx.clone();
        let search_ref = search_entry.clone();
        let stack_ref = content_stack.clone();
        let loading_ref = loading_status_page.clone();
        refresh_btn.connect_clicked(move |btn| {
            let current_query = search_ref.text().trim().to_string();
            if current_query.is_empty() {
                btn.set_sensitive(false);
                loading_ref.set_title("Vérification des mises à jour en cours…");
                loading_ref.set_description(Some(
                    "Synchronisation forcée des bases Pacman (checkupdates), de l'API AUR RPC v5 (yay), de Flathub et des AppImages…",
                ));
                stack_ref.set_visible_child_name("loading");
                let _ = tx_refresh.send(BackendCommand::CheckAllUpdates);
            } else {
                let _ = tx_refresh.send(BackendCommand::Search {
                    query: current_query,
                });
            }
        });
    }

    // --- Connexion du bouton "Vérifier les mises à jour" sur la page "Système entièrement à jour" ---
    {
        let tx_chk_empty = cmd_tx.clone();
        let stack_ref = content_stack.clone();
        let loading_ref = loading_status_page.clone();
        let refresh_ref = refresh_btn.clone();
        empty_check_updates_btn.connect_clicked(move |_| {
            refresh_ref.set_sensitive(false);
            loading_ref.set_title("Vérification des mises à jour en cours…");
            loading_ref.set_description(Some(
                "Synchronisation forcée des bases Pacman (checkupdates), de l'API AUR RPC v5 (yay), de Flathub et des AppImages…",
            ));
            stack_ref.set_visible_child_name("loading");
            let _ = tx_chk_empty.send(BackendCommand::CheckAllUpdates);
        });
    }

    // --- Connexion du bouton "Tout mettre à jour" ---
    {
        let state_upd_all = Rc::clone(&state);
        let tx_upd_all = cmd_tx.clone();
        update_all_btn.connect_clicked(move |btn| {
            let (items, ignored): (Vec<PackageItem>, Vec<String>) = {
                let st = state_upd_all.borrow();
                (
                    st.catalog_packages
                        .iter()
                        .filter(|p| {
                            p.update_available.is_some() && !st.ignored_updates.contains(&p.id)
                        })
                        .cloned()
                        .collect(),
                    st.ignored_updates.iter().cloned().collect(),
                )
            };
            btn.set_sensitive(false);
            let _ = tx_upd_all.send(BackendCommand::UpdateAllSystem { items, ignored });
        });
    }

    let notification_badge_clone = notification_badge.clone();

    // --- Connexion de la cloche de notifications ---
    {
        let state_notif = Rc::clone(&state);
        let win_notif = window.clone();
        notification_btn.connect_clicked(move |_| {
            let (notifs, _unread) = {
                let st = state_notif.borrow();
                (st.notifications.clone(), st.notification_unread)
            };
            if notifs.is_empty() {
                let dialog = adw::MessageDialog::builder()
                    .transient_for(&win_notif)
                    .modal(true)
                    .heading("Aucune notification")
                    .body("Aucune notification pour le moment.")
                    .build();
                dialog.add_response("ok", "OK");
                dialog.set_default_response(Some("ok"));
                dialog.set_close_response("ok");
                dialog.present();
                return;
            }

            let body = notifs
                .iter()
                .enumerate()
                .map(|(i, n)| format!("{}. {}", i + 1, n))
                .collect::<Vec<_>>()
                .join("\n");

            let dialog = adw::MessageDialog::builder()
                .transient_for(&win_notif)
                .modal(true)
                .heading(format!("Notifications ({})", notifs.len()))
                .body(&body)
                .build();
            dialog.add_response("ok", "OK");
            dialog.add_response("clear", "Tout effacer");
            dialog.set_default_response(Some("ok"));
            dialog.set_close_response("ok");
            dialog.set_response_appearance("clear", adw::ResponseAppearance::Destructive);

            let state_clr = Rc::clone(&state_notif);
            dialog.connect_response(None, move |dlg, response| {
                dlg.close();
                if response == "clear" {
                    let mut st = state_clr.borrow_mut();
                    st.notifications.clear();
                    st.notification_unread = 0;
                }
            });
            dialog.present();

            // Marque les notifications comme lues à l'ouverture.
            let mut st = state_notif.borrow_mut();
            st.notification_unread = 0;
            update_notification_badge(&notification_badge_clone, 0);
        });
    }

    let notification_badge_loop = notification_badge.clone();

    // --- Boucle de réception asynchrone GLib (UI Thread) ---
    {
        let state_ui = Rc::clone(&state);
        let prefs_page = preferences_page;
        let stack = content_stack;
        let empty_page = empty_status_page;
        let spinner_ui = spinner;
        let toast_ui = toast_overlay;
        let banner_ui = error_banner;
        let badges_ui = Rc::clone(&badge_widgets);
        let actions_ui = header_actions;
        let install_mgr_ui = install_manager.clone();
        let tx_ui = cmd_tx.clone();
        let win_for_shots = window.clone();
        let sidebar_for_shots = sidebar_list.clone();

        glib::MainContext::default().spawn_local(async move {
            while let Ok(event) = ui_rx.recv().await {
                match event {
                    UiEvent::LoadingStarted { message } => {
                        spinner_ui.set_visible(true);
                        spinner_ui.set_spinning(true);
                        spinner_ui.set_tooltip_text(Some(&message));
                    }
                    UiEvent::CatalogLoaded(snapshot) => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.refresh_btn.set_sensitive(true);
                        actions_ui.update_all_btn.set_sensitive(true);

                        if !snapshot.non_fatal_warnings.is_empty() {
                            let joined = snapshot.non_fatal_warnings.join(" · ");
                            banner_ui.set_title(&joined);
                            banner_ui.set_revealed(true);
                            if backend::pacman::is_pacman_lock_error_text(&joined) {
                                state_ui.borrow_mut().lock_dismissed_while_present = false;
                                let _ = tx_ui.send(BackendCommand::CheckPacmanLock);
                            }
                        } else {
                            banner_ui.set_revealed(false);
                        }

                        let data_changed = {
                            let mut st = state_ui.borrow_mut();
                            let mut merged_packages = snapshot.packages;
                            merge_preserved_aur_catalog(&mut merged_packages, &st.catalog_packages);
                            let changed = st.catalog_packages != merged_packages
                                || st.recommendations != snapshot.recommendations
                                || st.flatpak_recommendations != snapshot.flatpak_recommendations
                                || st.aur_helper != snapshot.aur_helper;
                            st.catalog_packages = merged_packages;
                            st.recommendations = snapshot.recommendations;
                            st.flatpak_recommendations = snapshot.flatpak_recommendations;
                            st.updates_count = snapshot.updates_count;
                            st.aur_helper = snapshot.aur_helper;
                            changed
                        };

                        let is_loading_view =
                            stack.visible_child_name().as_deref() == Some("loading");
                        if data_changed || is_loading_view {
                            update_selection_buttons(&state_ui, &actions_ui);
                            rebuild_package_list_ui(
                                &state_ui,
                                &prefs_page,
                                &stack,
                                &empty_page,
                                &badges_ui,
                                &actions_ui,
                                &tx_ui,
                            );
                        }

                        if is_loading_view {
                            maybe_schedule_automated_screenshots(
                                &win_for_shots,
                                &sidebar_for_shots,
                            );
                        }
                    }
                    UiEvent::UpdatesChecked {
                        snapshot,
                        summary_toast: _,
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.refresh_btn.set_sensitive(true);
                        actions_ui.update_all_btn.set_sensitive(true);

                        let visible_updates = {
                            let st = state_ui.borrow();
                            snapshot
                                .packages
                                .iter()
                                .filter(|pkg| {
                                    pkg.update_available.is_some()
                                        && !st.ignored_updates.contains(&pkg.id)
                                })
                                .count()
                        };
                        let summary_toast = if visible_updates > 0 {
                            format!(
                                "Vérification terminée : {visible_updates} mise(s) à jour disponible(s)."
                            )
                        } else {
                            String::from(
                                "Vérification terminée : tous vos paquets (Pacman, AUR, Flatpak, AppImages) sont à jour.",
                            )
                        };

                        if !snapshot.non_fatal_warnings.is_empty() {
                            let joined = snapshot.non_fatal_warnings.join(" · ");
                            banner_ui.set_title(&joined);
                            banner_ui.set_revealed(true);
                            if backend::pacman::is_pacman_lock_error_text(&joined) {
                                state_ui.borrow_mut().lock_dismissed_while_present = false;
                                let _ = tx_ui.send(BackendCommand::CheckPacmanLock);
                            }
                        } else {
                            banner_ui.set_revealed(false);
                        }

                        {
                            let mut st = state_ui.borrow_mut();
                            let mut merged_packages = snapshot.packages;
                            merge_preserved_aur_catalog(&mut merged_packages, &st.catalog_packages);
                            st.catalog_packages = merged_packages;
                            st.recommendations = snapshot.recommendations;
                            st.flatpak_recommendations = snapshot.flatpak_recommendations;
                            st.updates_count = snapshot.updates_count;
                            st.aur_helper = snapshot.aur_helper;
                        }

                        update_selection_buttons(&state_ui, &actions_ui);
                        rebuild_package_list_ui(
                            &state_ui,
                            &prefs_page,
                            &stack,
                            &empty_page,
                            &badges_ui,
                            &actions_ui,
                            &tx_ui,
                        );

                        let toast = adw::Toast::builder()
                            .title(&summary_toast)
                            .timeout(4)
                            .build();
                        toast_ui.add_toast(toast);
                    }
                    UiEvent::SearchCompleted {
                        query,
                        results,
                        warnings,
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);

                        let should_apply = {
                            let st = state_ui.borrow();
                            st.active_query == query
                        };

                        if should_apply {
                            if !warnings.is_empty() {
                                banner_ui.set_title(&warnings.join(" · "));
                                banner_ui.set_revealed(true);
                            }

                            {
                                let mut st = state_ui.borrow_mut();
                                st.search_results = Some(results);
                            }

                            rebuild_package_list_ui(
                                &state_ui,
                                &prefs_page,
                                &stack,
                                &empty_page,
                                &badges_ui,
                                &actions_ui,
                                &tx_ui,
                            );
                        }
                    }
                    UiEvent::AurMorePackagesLoaded { wave_index, items } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);

                        let mut retry_next_wave: Option<usize> = None;
                        {
                            let mut st = state_ui.borrow_mut();
                            let existing_names: std::collections::HashSet<String> = st
                                .catalog_packages
                                .iter()
                                .filter(|p| p.source == PackageSource::Aur)
                                .map(|p| p.name.clone())
                                .collect();
                            let mut added_count = 0usize;
                            for pkg in items {
                                if !existing_names.contains(&pkg.name) {
                                    st.catalog_packages.push(pkg);
                                    added_count += 1;
                                }
                            }
                            let next_idx = wave_index + 1;
                            st.aur_next_wave_index = st.aur_next_wave_index.max(next_idx);
                            if added_count == 0
                                && st.aur_next_wave_index < backend::aur::AUR_DISCOVERY_WAVES.len()
                            {
                                retry_next_wave = Some(st.aur_next_wave_index);
                                st.aur_loading_more = true;
                            } else {
                                st.aur_loading_more = false;
                                if st.aur_advance_page_on_load && added_count > 0 {
                                    st.aur_current_page += 1;
                                }
                                st.aur_advance_page_on_load = false;
                            }
                        }

                        if let Some(next_wave) = retry_next_wave {
                            let _ = tx_ui.send(BackendCommand::LoadMoreAurPackages {
                                wave_index: next_wave,
                            });
                        } else {
                            rebuild_package_list_ui(
                                &state_ui,
                                &prefs_page,
                                &stack,
                                &empty_page,
                                &badges_ui,
                                &actions_ui,
                                &tx_ui,
                            );
                        }
                    }
                    UiEvent::PackageDetailsLoaded(info) => {
                        actions_ui.details_dialog.show_details(&info);
                    }
                    UiEvent::CatalogReloadRequested => {
                        let _ = tx_ui.send(BackendCommand::RefreshCatalog);
                    }
                    UiEvent::AurVersionsLoaded {
                        package_base,
                        versions,
                        ..
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui
                            .aur_versions_dialog
                            .show_versions_for_current(&package_base, &versions, &tx_ui);
                    }
                    UiEvent::AurVersionsError(detail) => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.aur_versions_dialog.show_error(&detail);
                    }
                    UiEvent::MirrorListLoaded(mirrors) => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.settings_view.on_mirror_list_loaded(mirrors);
                    }
                    UiEvent::MirrorListApplied { message } => {
                        actions_ui.settings_view.on_mirrorlist_applied(&message);
                    }
                    UiEvent::ReflectorError(detail) => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.settings_view.on_mirror_error(&detail);
                    }
                    UiEvent::PacmanCacheSize { bytes } => {
                        actions_ui.settings_view.on_cache_size(bytes);
                    }
                    UiEvent::PacmanCacheError(detail) => {
                        actions_ui.settings_view.on_cache_error(&detail);
                    }
                    UiEvent::ServicesNeedRestart {
                        services,
                        kernel_status,
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.settings_view.set_services_checking(false);
                        if services.is_empty() {
                            if backend::needrestart::reboot_recommended(kernel_status) {
                                let toast = adw::Toast::builder()
                                    .title("Un redémarrage du système est conseillé : un nouveau noyau a été installé.")
                                    .timeout(7)
                                    .build();
                                toast_ui.add_toast(toast);
                            }
                        } else {
                            state_ui.borrow_mut().pending_kernel_status = kernel_status;
                            show_services_restart_prompt(
                                &win_for_shots,
                                &services,
                                kernel_status,
                                &tx_ui,
                            );
                        }
                    }
                    UiEvent::ServicesRestarted {
                        restarted,
                        failures,
                        kernel_status,
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.settings_view.set_services_checking(false);
                        let kernel_status = state_ui
                            .borrow_mut()
                            .pending_kernel_status
                            .take()
                            .or(kernel_status);
                        show_services_restart_result(
                            &win_for_shots,
                            &restarted,
                            &failures,
                            kernel_status,
                        );
                    }
                    UiEvent::OptionalDependenciesPrompt {
                        items,
                        optional_deps,
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        update_selection_buttons(&state_ui, &actions_ui);
                        install_mgr_ui.prompt_optional_dependencies(
                            &win_for_shots,
                            items,
                            optional_deps,
                            tx_ui.clone(),
                        );
                    }
                    UiEvent::InstallStarted { target_names } => {
                        spinner_ui.set_visible(true);
                        spinner_ui.set_spinning(true);
                        install_mgr_ui.on_install_started(&target_names);
                    }
                    UiEvent::InstallDependenciesResolved {
                        dependencies,
                        total_download_bytes,
                    } => {
                        install_mgr_ui
                            .on_dependencies_resolved(&dependencies, total_download_bytes);
                    }
                    UiEvent::InstallProgress {
                        phase,
                        fraction,
                        active_package,
                        log_line,
                    } => {
                        install_mgr_ui.on_progress(&phase, fraction, active_package, log_line);
                    }
                    UiEvent::InstallFinished {
                        success,
                        message,
                        installed_ids,
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.update_all_btn.set_sensitive(true);

                        install_mgr_ui.on_install_finished(success, &message);

                        if success {
                            add_notification(
                                &state_ui,
                                &notification_badge_loop,
                                format!("Installation terminée : {message}"),
                            );
                        } else {
                            add_notification(
                                &state_ui,
                                &notification_badge_loop,
                                format!("Échec d'installation : {message}"),
                            );
                        }

                        if success {
                            {
                                let mut st = state_ui.borrow_mut();
                                for id in &installed_ids {
                                    st.selected_packages.remove(id);
                                }
                                for pkg in &mut st.catalog_packages {
                                    if installed_ids.contains(&pkg.id) {
                                        pkg.is_installed = true;
                                        if let Some(new_ver) = pkg.update_available.take() {
                                            pkg.version = new_ver;
                                        }
                                    }
                                }
                                if let Some(ref mut results) = st.search_results {
                                    for pkg in results.iter_mut() {
                                        if installed_ids.contains(&pkg.id) {
                                            pkg.is_installed = true;
                                            if let Some(new_ver) = pkg.update_available.take() {
                                                pkg.version = new_ver;
                                            }
                                        }
                                    }
                                }
                                for (_, list) in &mut st.recommendations {
                                    for pkg in list.iter_mut() {
                                        if installed_ids.contains(&pkg.id) {
                                            pkg.is_installed = true;
                                            if let Some(new_ver) = pkg.update_available.take() {
                                                pkg.version = new_ver;
                                            }
                                        }
                                    }
                                }
                                for (_, list) in &mut st.flatpak_recommendations {
                                    for pkg in list.iter_mut() {
                                        if installed_ids.contains(&pkg.id) {
                                            pkg.is_installed = true;
                                            if let Some(new_ver) = pkg.update_available.take() {
                                                pkg.version = new_ver;
                                            }
                                        }
                                    }
                                }
                                st.updates_count = st
                                    .catalog_packages
                                    .iter()
                                    .filter(|p| p.update_available.is_some())
                                    .count();

                                backend::cache::save_cached_snapshot(&backend::CatalogSnapshot {
                                    packages: st.catalog_packages.clone(),
                                    recommendations: st.recommendations.clone(),
                                    flatpak_recommendations: st.flatpak_recommendations.clone(),
                                    updates_count: st.updates_count,
                                    aur_helper: st.aur_helper,
                                    non_fatal_warnings: Vec::new(),
                                });
                            }
                            update_selection_buttons(&state_ui, &actions_ui);
                            rebuild_package_list_ui(
                                &state_ui,
                                &prefs_page,
                                &stack,
                                &empty_page,
                                &badges_ui,
                                &actions_ui,
                                &tx_ui,
                            );

                            let self_was_updated = installed_ids.iter().any(|id| {
                                id == "aur:noos-package-manager"
                                    || id == "pacman:noos-package-manager"
                                    || id == backend::SELF_PACKAGE_NAME
                            });

                            if self_was_updated {
                                let restart_msg = format!(
                                    "{message} — Redémarrage automatique avec la nouvelle version…"
                                );
                                install_mgr_ui.on_install_finished(true, &restart_msg);
                                let toast = adw::Toast::builder()
                                    .title(&restart_msg)
                                    .timeout(4)
                                    .build();
                                toast_ui.add_toast(toast);

                                let win_restart = win_for_shots.clone();
                                glib::timeout_add_local_once(
                                    Duration::from_millis(1200),
                                    move || {
                                        relaunch_self_application(&win_restart);
                                    },
                                );
                            } else {
                                let toast = adw::Toast::builder()
                                    .title(&message)
                                    .timeout(4)
                                    .build();
                                toast_ui.add_toast(toast);

                                let _ = tx_ui.send(BackendCommand::RefreshCatalog);
                            }
                        } else {
                            update_selection_buttons(&state_ui, &actions_ui);
                            banner_ui.set_title(&message);
                            banner_ui.set_revealed(true);
                            if backend::pacman::is_pacman_lock_error_text(&message) {
                                state_ui.borrow_mut().lock_dismissed_while_present = false;
                                show_pacman_lock_repair_dialog(
                                    &win_for_shots,
                                    &state_ui,
                                    &tx_ui,
                                    backend::pacman::PacmanLockInfo {
                                        lock_path: String::from(
                                            backend::pacman::PACMAN_DB_LOCK_PATH,
                                        ),
                                        running_processes: Vec::new(),
                                        is_stale: true,
                                    },
                                );
                            }
                            rebuild_package_list_ui(
                                &state_ui,
                                &prefs_page,
                                &stack,
                                &empty_page,
                                &badges_ui,
                                &actions_ui,
                                &tx_ui,
                            );
                        }
                    }
                    UiEvent::OperationSuccess {
                        message,
                        reload_catalog,
                    } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        actions_ui.refresh_btn.set_sensitive(true);
                        actions_ui.update_all_btn.set_sensitive(true);
                        actions_ui.settings_view.set_services_checking(false);

                        add_notification(
                            &state_ui,
                            &notification_badge_loop,
                            format!("Succès : {message}"),
                        );

                        let toast = adw::Toast::builder()
                            .title(&message)
                            .timeout(4)
                            .build();
                        toast_ui.add_toast(toast);

                        if reload_catalog {
                            let _ = tx_ui.send(BackendCommand::RefreshCatalog);
                        }
                    }
                    UiEvent::OperationError { title, detail } => {
                        spinner_ui.set_spinning(false);
                        actions_ui.settings_view.set_services_checking(false);
                        spinner_ui.set_visible(false);
                        actions_ui.refresh_btn.set_sensitive(true);
                        actions_ui.update_all_btn.set_sensitive(true);
                        update_selection_buttons(&state_ui, &actions_ui);

                        add_notification(
                            &state_ui,
                            &notification_badge_loop,
                            format!("Erreur : {title} — {detail}"),
                        );

                        let toast = adw::Toast::builder()
                            .title(&title)
                            .timeout(5)
                            .build();
                        toast_ui.add_toast(toast);

                        let full_err = format!("{title} — {detail}");
                        banner_ui.set_title(&full_err);
                        banner_ui.set_revealed(true);

                        if backend::pacman::is_pacman_lock_error_text(&full_err) {
                            state_ui.borrow_mut().lock_dismissed_while_present = false;
                            show_pacman_lock_repair_dialog(
                                &win_for_shots,
                                &state_ui,
                                &tx_ui,
                                backend::pacman::PacmanLockInfo {
                                    lock_path: String::from(backend::pacman::PACMAN_DB_LOCK_PATH),
                                    running_processes: Vec::new(),
                                    is_stale: true,
                                },
                            );
                        }

                        rebuild_package_list_ui(
                            &state_ui,
                            &prefs_page,
                            &stack,
                            &empty_page,
                            &badges_ui,
                            &actions_ui,
                            &tx_ui,
                        );
                    }
                    UiEvent::PacmanLockDetected(lock_info) => {
                        if !install_mgr_ui.is_installing() {
                            let should_open = {
                                let st = state_ui.borrow();
                                !st.lock_dialog_open && !st.lock_dismissed_while_present
                            };
                            if should_open {
                                show_pacman_lock_repair_dialog(
                                    &win_for_shots,
                                    &state_ui,
                                    &tx_ui,
                                    lock_info,
                                );
                            }
                        }
                    }
                    UiEvent::PacmanLockCleared => {
                        state_ui.borrow_mut().lock_dismissed_while_present = false;
                    }
                    UiEvent::PacmanLockRepaired { message } => {
                        spinner_ui.set_spinning(false);
                        spinner_ui.set_visible(false);
                        banner_ui.set_revealed(false);
                        state_ui.borrow_mut().lock_dismissed_while_present = false;

                        let toast = adw::Toast::builder()
                            .title(&message)
                            .timeout(5)
                            .build();
                        toast_ui.add_toast(toast);

                        let _ = tx_ui.send(BackendCommand::RefreshCatalog);
                    }
                }
            }
        });
    }

    // --- Surveillance automatique périodique du verrou Pacman (/var/lib/pacman/db.lck) ---
    {
        let state_poll = Rc::clone(&state);
        let install_mgr_poll = install_manager;
        let tx_poll = cmd_tx.clone();
        glib::timeout_add_local(Duration::from_secs(3), move || {
            let can_poll = !install_mgr_poll.is_installing()
                && !state_poll.borrow().lock_dialog_open;
            if can_poll {
                let _ = tx_poll.send(BackendCommand::CheckPacmanLock);
            }
            glib::ControlFlow::Continue
        });
    }

    let _ = cmd_tx.send(BackendCommand::RefreshCatalog);
    let _ = cmd_tx.send(BackendCommand::CheckPacmanLock);

    window
}

fn show_services_restart_prompt(
    parent: &adw::ApplicationWindow,
    services: &[String],
    kernel_status: Option<u32>,
    cmd_tx: &Sender<BackendCommand>,
) {
    let mut body = format!(
        "{} service(s) utilisent encore d'anciennes bibliothèques et doivent être redémarrés :",
        services.len()
    );
    if backend::needrestart::reboot_recommended(kernel_status) {
        body.push_str(
            "\n\nUn nouveau noyau est installé : un redémarrage complet restera nécessaire.",
        );
    }

    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading("Services à redémarrer")
        .body(&body)
        .build();

    let list = services
        .iter()
        .map(|service| format!("• {service}"))
        .collect::<Vec<_>>()
        .join("\n");
    let list_label = gtk4::Label::builder()
        .label(&list)
        .xalign(0.0)
        .selectable(true)
        .wrap(true)
        .build();
    let scroll = gtk4::ScrolledWindow::builder()
        .min_content_height(120)
        .max_content_height(280)
        .child(&list_label)
        .build();
    dialog.set_extra_child(Some(&scroll));

    dialog.add_response("later", "Plus tard");
    dialog.add_response("restart", "Redémarrer maintenant");
    dialog.set_default_response(Some("restart"));
    dialog.set_close_response("later");
    dialog.set_response_appearance("restart", adw::ResponseAppearance::Suggested);

    let services = services.to_vec();
    let tx = cmd_tx.clone();
    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response == "restart" {
            let _ = tx.send(BackendCommand::RestartServices {
                services: services.clone(),
                kernel_status,
            });
        }
    });
    dialog.present();
}

fn show_services_restart_result(
    parent: &adw::ApplicationWindow,
    restarted: &[String],
    failures: &[crate::backend::models::ServiceRestartFailure],
    kernel_status: Option<u32>,
) {
    let needs_reboot =
        backend::needrestart::reboot_recommended(kernel_status) || !failures.is_empty();

    let (heading, mut body) = if failures.is_empty() {
        (
            "Services redémarrés",
            format!(
                "Les {} service(s) détecté(s) ont redémarré avec succès.",
                restarted.len()
            ),
        )
    } else {
        (
            "Certains services n'ont pas redémarré",
            format!(
                "{} service(s) redémarré(s), {} échec(s).",
                restarted.len(),
                failures.len()
            ),
        )
    };

    if needs_reboot {
        body.push_str(
            "\n\nUn redémarrage du système est conseillé pour finaliser la mise à jour.",
        );
    }

    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading(heading)
        .body(&body)
        .build();

    if !failures.is_empty() {
        let details = failures
            .iter()
            .map(|failure| format!("• {}\n{}", failure.name, failure.status))
            .collect::<Vec<_>>()
            .join("\n\n");
        let details_label = gtk4::Label::builder()
            .label(&details)
            .xalign(0.0)
            .selectable(true)
            .wrap(true)
            .build();
        let scroll = gtk4::ScrolledWindow::builder()
            .min_content_height(160)
            .max_content_height(340)
            .child(&details_label)
            .build();
        dialog.set_extra_child(Some(&scroll));
    }

    dialog.add_response("ok", "OK");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("ok");
    dialog.present();
}

fn show_pacman_lock_repair_dialog(
    parent: &adw::ApplicationWindow,
    state: &Rc<RefCell<AppUiState>>,
    cmd_tx: &Sender<BackendCommand>,
    lock_info: backend::pacman::PacmanLockInfo,
) {
    {
        let mut st = state.borrow_mut();
        if st.lock_dialog_open {
            return;
        }
        st.lock_dialog_open = true;
    }

    let body_text = if lock_info.is_stale {
        format!(
            "Le fichier de verrouillage « {} » bloque actuellement Pacman alors qu'aucun processus pacman n'est en cours d'exécution (verrou orphelin suite à une opération interrompue).\n\nVoulez-vous supprimer ce verrou et réparer la base de données Pacman maintenant ?",
            lock_info.lock_path
        )
    } else {
        let procs = lock_info
            .running_processes
            .iter()
            .map(|p| format!("• {p}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "Le fichier de verrouillage « {} » est actif et un processus Pacman est en cours ou bloqué :\n{}\n\nSi cette opération est bloquée, vous pouvez forcer l'arrêt du processus et supprimer le verrou pour réparer Pacman.",
            lock_info.lock_path, procs
        )
    };

    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading("Base de données Pacman verrouillée")
        .body(&body_text)
        .build();

    dialog.add_response("ignore", "Ignorer");
    dialog.add_response("repair", "Déverrouiller et réparer Pacman");
    dialog.set_default_response(Some("repair"));
    dialog.set_close_response("ignore");
    dialog.set_response_appearance("repair", adw::ResponseAppearance::Suggested);

    let state_resp = Rc::clone(state);
    let tx_resp = cmd_tx.clone();
    dialog.connect_response(None, move |dlg, response| {
        {
            let mut st = state_resp.borrow_mut();
            st.lock_dialog_open = false;
            st.lock_dismissed_while_present = response != "repair";
        }
        if response == "repair" {
            let _ = tx_resp.send(BackendCommand::RepairPacmanLock);
        }
        dlg.close();
    });

    dialog.present();
}

fn save_window_screenshot(window: &adw::ApplicationWindow, path: &std::path::Path) {
    let paintable = gtk4::WidgetPaintable::new(Some(window));
    let width = paintable.intrinsic_width().max(1180);
    let height = paintable.intrinsic_height().max(760);
    let snapshot = gtk4::Snapshot::new();
    paintable.snapshot(&snapshot, width as f64, height as f64);
    if let Some(node) = snapshot.to_node() {
        if let Some(native) = window.native() {
            if let Some(renderer) = native.renderer() {
                let texture = renderer.render_texture(&node, None);
                let _ = texture.save_to_png(path);
            }
        }
    }
}

fn maybe_schedule_automated_screenshots(
    window: &adw::ApplicationWindow,
    sidebar_list: &gtk4::ListBox,
) {
    let Some(dir_os) = std::env::var_os("NOOS_CAPTURE_SCREENSHOTS_DIR") else {
        return;
    };
    let out_dir = std::path::PathBuf::from(dir_os);
    let _ = std::fs::create_dir_all(&out_dir);

    let steps: [(u64, i32, &str); 5] = [
        (500, 0, "overview.png"),
        (1100, 3, "pacman-recommendations.png"),
        (1700, 4, "aur-columns.png"),
        (2300, 5, "flatpak-flathub.png"),
        (2900, 6, "appimages-drag-drop.png"),
    ];

    for (delay_ms, row_idx, file_name) in steps {
        let win = window.clone();
        let list = sidebar_list.clone();
        let out_path = out_dir.join(file_name);
        glib::timeout_add_local_once(Duration::from_millis(delay_ms), move || {
            if let Some(row) = list.row_at_index(row_idx) {
                list.select_row(Some(&row));
            }
            let win_capture = win.clone();
            glib::timeout_add_local_once(Duration::from_millis(350), move || {
                save_window_screenshot(&win_capture, &out_path);
            });
        });
    }

    let win_close = window.clone();
    glib::timeout_add_local_once(Duration::from_millis(3600), move || {
        win_close.close();
    });
}

fn relaunch_self_application(window: &adw::ApplicationWindow) {
    let usr_bin = std::path::PathBuf::from("/usr/bin/noos-package-manager");
    let cargo_bin = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .map(|h| h.join(".cargo/bin/noos-package-manager"));

    let target_exe = if usr_bin.is_file() {
        usr_bin
    } else if let Some(cb) = cargo_bin.filter(|p| p.is_file()) {
        cb
    } else {
        std::env::current_exe()
            .unwrap_or_else(|_| std::path::PathBuf::from("noos-package-manager"))
    };

    let _ = std::process::Command::new(target_exe)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();

    window.destroy();
}

fn widen_preferences_page_clamp(widget: &gtk4::Widget) {
    if let Ok(clamp) = widget.clone().downcast::<adw::Clamp>() {
        clamp.set_maximum_size(1280);
        clamp.set_tightening_threshold(960);
    }
    let mut child_opt = widget.first_child();
    while let Some(child) = child_opt {
        widen_preferences_page_clamp(&child);
        child_opt = child.next_sibling();
    }
}

/// Met à jour le badge compteur de la cloche de notifications.
fn update_notification_badge(badge: &gtk4::Label, unread: usize) {
    if unread > 0 {
        badge.set_label(&unread.to_string());
        badge.set_visible(true);
    } else {
        badge.set_visible(false);
    }
}

/// Ajoute une notification utilisateur à l'état et met à jour le badge de la cloche.
fn add_notification(state_rc: &Rc<RefCell<AppUiState>>, badge: &gtk4::Label, message: impl Into<String>) {
    let msg = message.into();
    {
        let mut st = state_rc.borrow_mut();
        st.notifications.push(msg.clone());
        // Garde les 100 notifications les plus récentes.
        let len = st.notifications.len();
        if len > 100 {
            st.notifications.drain(..len - 100);
        }
        st.notification_unread += 1;
        let unread = st.notification_unread;
        drop(st);
        update_notification_badge(badge, unread);
    }
}

fn update_selection_buttons(
    state_rc: &Rc<RefCell<AppUiState>>,
    actions: &HeaderActionButtons,
) {
    let count = state_rc.borrow().selected_packages.len();
    actions
        .batch_install_btn
        .set_label(&format!("Installer ({count})"));
    actions.batch_install_btn.set_sensitive(count > 0);
    actions.clear_selection_btn.set_visible(count > 0);
}

fn resolve_sidebar_toggle_icon(collapsed: bool) -> &'static str {
    let preferred = if collapsed {
        "pane-show-symbolic"
    } else {
        "pane-hide-symbolic"
    };
    if let Some(display) = gtk4::gdk::Display::default() {
        let theme = gtk4::IconTheme::for_display(&display);
        if theme.has_icon(preferred) {
            return preferred;
        }
    }
    "sidebar-show-symbolic"
}

#[allow(clippy::too_many_arguments)]
fn apply_sidebar_collapsed_state(
    split_view: &adw::NavigationSplitView,
    sidebar_list: &gtk4::ListBox,
    sidebar_header: &adw::HeaderBar,
    sidebar_header_box: &gtk4::Box,
    collapsed_window_controls: &gtk4::WindowControls,
    sidebar_toggle_btn: &gtk4::Button,
    badges: &CategoryBadgeWidgets,
    version_label: &gtk4::Label,
    settings_label: &gtk4::Label,
    settings_content: &gtk4::Box,
    is_collapsed: bool,
    updates_count: usize,
) {
    if is_collapsed {
        sidebar_header.set_show_start_title_buttons(false);
        sidebar_header_box.set_hexpand(false);
        sidebar_header_box.set_halign(gtk4::Align::Center);
        collapsed_window_controls.set_visible(!collapsed_window_controls.is_empty());
        sidebar_toggle_btn.set_icon_name(resolve_sidebar_toggle_icon(true));
        sidebar_toggle_btn
            .set_tooltip_text(Some("Déplier le volet latéral (afficher les libellés)"));

        sidebar_list.set_margin_start(4);
        sidebar_list.set_margin_end(4);

        for rw in &badges.sidebar_rows {
            rw.title_label.set_visible(false);
            rw.hbox.set_halign(gtk4::Align::Center);
            rw.hbox.set_spacing(0);
            rw.hbox.set_margin_start(4);
            rw.hbox.set_margin_end(4);
            rw.icon.set_halign(gtk4::Align::Center);
        }

        badges.installed_label.set_visible(false);
        badges.updates_label.set_visible(false);
        badges.appimages_label.set_visible(false);

        version_label.set_halign(gtk4::Align::Center);
        version_label.set_margin_start(0);
        version_label.set_margin_end(0);

        settings_label.set_visible(false);
        settings_content.set_halign(gtk4::Align::Center);

        split_view.set_min_sidebar_width(SIDEBAR_COLLAPSED_WIDTH);
        split_view.set_max_sidebar_width(SIDEBAR_COLLAPSED_WIDTH);
        split_view.set_sidebar_width_fraction(SIDEBAR_COLLAPSED_FRACTION);
    } else {
        split_view.set_max_sidebar_width(SIDEBAR_EXPANDED_MAX_WIDTH);
        split_view.set_min_sidebar_width(SIDEBAR_EXPANDED_MIN_WIDTH);
        split_view.set_sidebar_width_fraction(SIDEBAR_EXPANDED_FRACTION);

        collapsed_window_controls.set_visible(false);
        sidebar_header.set_show_start_title_buttons(true);
        sidebar_header_box.set_hexpand(true);
        sidebar_header_box.set_halign(gtk4::Align::End);
        sidebar_toggle_btn.set_icon_name(resolve_sidebar_toggle_icon(false));
        sidebar_toggle_btn.set_tooltip_text(Some(
            "Réduire le volet latéral (afficher uniquement les icônes)",
        ));

        sidebar_list.set_margin_start(4);
        sidebar_list.set_margin_end(4);

        for rw in &badges.sidebar_rows {
            rw.title_label.set_visible(true);
            rw.hbox.set_halign(gtk4::Align::Fill);
            rw.hbox.set_spacing(6);
            rw.hbox.set_margin_start(4);
            rw.hbox.set_margin_end(2);
            rw.icon.set_halign(gtk4::Align::Start);
        }

        badges.installed_label.set_visible(true);
        badges.updates_label.set_visible(updates_count > 0);
        badges.appimages_label.set_visible(true);

        version_label.set_halign(gtk4::Align::Start);
        version_label.set_margin_start(10);
        version_label.set_margin_end(8);

        settings_label.set_visible(true);
        settings_content.set_halign(gtk4::Align::Start);
    }
}

fn create_sidebar_badge(is_update_badge: bool) -> gtk4::Label {
    let label = gtk4::Label::new(Some("0"));
    label.set_valign(gtk4::Align::Center);
    if is_update_badge {
        label.add_css_class("update-badge");
        label.set_visible(false);
    } else {
        label.add_css_class("sidebar-count-badge");
    }
    label
}

fn create_sidebar_row(category: SidebarCategory, badge: &gtk4::Label) -> SidebarRowWidgets {
    let hbox = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .margin_top(7)
        .margin_bottom(7)
        .margin_start(4)
        .margin_end(2)
        .build();

    let resolved_icon = if matches!(category, SidebarCategory::Flatpak) {
        if let Some(display) = gtk4::gdk::Display::default() {
            let theme = gtk4::IconTheme::for_display(&display);
            if theme.has_icon("flatpak-symbolic") {
                "flatpak-symbolic"
            } else {
                category.icon_name()
            }
        } else {
            category.icon_name()
        }
    } else {
        category.icon_name()
    };

    let icon = gtk4::Image::from_icon_name(resolved_icon);
    hbox.append(&icon);

    let title_label = gtk4::Label::builder()
        .label(category.title())
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk4::pango::EllipsizeMode::End)
        .build();
    hbox.append(&title_label);

    hbox.append(badge);

    let row = gtk4::ListBoxRow::builder()
        .child(&hbox)
        .tooltip_text(category.title())
        .build();

    SidebarRowWidgets {
        row,
        hbox,
        icon,
        title_label,
        category,
    }
}

fn make_row_with_selection(
    item: &PackageItem,
    state_rc: &Rc<RefCell<AppUiState>>,
    actions: &HeaderActionButtons,
    cmd_tx: &Sender<BackendCommand>,
) -> adw::ActionRow {
    let (is_checked, show_source_badge, use_aur_columns) = {
        let st = state_rc.borrow();
        (
            st.selected_packages.contains_key(&item.id),
            matches!(st.selected_category, SidebarCategory::Installed),
            matches!(st.selected_category, SidebarCategory::Aur),
        )
    };
    let state_for_check = Rc::clone(state_rc);
    let actions_for_check = actions.clone();
    let item_for_check = item.clone();

    let dialog_for_info = actions.details_dialog.clone();
    let item_for_info = item.clone();
    let tx_for_info = cmd_tx.clone();

    let dialog_for_version = actions.aur_versions_dialog.clone();
    let item_for_version = item.clone();
    let tx_for_version = cmd_tx.clone();

    build_package_action_row(
        item,
        is_checked,
        show_source_badge,
        use_aur_columns,
        cmd_tx,
        move |checked| {
            {
                let mut st = state_for_check.borrow_mut();
                if checked {
                    st.selected_packages
                        .insert(item_for_check.id.clone(), item_for_check.clone());
                } else {
                    st.selected_packages.remove(&item_for_check.id);
                }
            }
            update_selection_buttons(&state_for_check, &actions_for_check);
        },
        move || {
            dialog_for_info.show_loading(&item_for_info);
            let _ = tx_for_info.send(BackendCommand::FetchPackageDetails {
                item: item_for_info.clone(),
            });
        },
        move || {
            dialog_for_version.show_loading(&item_for_version);
            let _ = tx_for_version.send(BackendCommand::FetchAurVersions {
                package: item_for_version.name.clone(),
            });
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn attach_update_ignore_button(
    row: &adw::ActionRow,
    package_id: &str,
    ignored: bool,
    state_rc: &Rc<RefCell<AppUiState>>,
    prefs_page: &adw::PreferencesPage,
    stack: &gtk4::Stack,
    empty_status_page: &adw::StatusPage,
    badges: &Rc<CategoryBadgeWidgets>,
    actions: &HeaderActionButtons,
    cmd_tx: &Sender<BackendCommand>,
) {
    let button = gtk4::Button::builder()
        .icon_name(if ignored {
            "view-reveal-symbolic"
        } else {
            "view-conceal-symbolic"
        })
        .tooltip_text(if ignored {
            "Réactiver cette mise à jour"
        } else {
            "Ignorer cette mise à jour"
        })
        .valign(gtk4::Align::Center)
        .css_classes(["flat", "circular"])
        .build();

    let package_id = package_id.to_string();
    let state_clone = Rc::clone(state_rc);
    let prefs_clone = prefs_page.clone();
    let stack_clone = stack.clone();
    let empty_clone = empty_status_page.clone();
    let badges_clone = Rc::clone(badges);
    let actions_clone = actions.clone();
    let tx_clone = cmd_tx.clone();

    button.connect_clicked(move |_| {
        backend::settings::set_update_ignored(&package_id, !ignored);
        {
            let mut st = state_clone.borrow_mut();
            if ignored {
                st.ignored_updates.remove(&package_id);
            } else {
                st.ignored_updates.insert(package_id.clone());
            }
        }
        rebuild_package_list_ui(
            &state_clone,
            &prefs_clone,
            &stack_clone,
            &empty_clone,
            &badges_clone,
            &actions_clone,
            &tx_clone,
        );
    });

    row.add_suffix(&button);
}

fn rebuild_package_list_ui(
    state_rc: &Rc<RefCell<AppUiState>>,
    prefs_page: &adw::PreferencesPage,
    stack: &gtk4::Stack,
    empty_status_page: &adw::StatusPage,
    badges: &Rc<CategoryBadgeWidgets>,
    actions: &HeaderActionButtons,
    cmd_tx: &Sender<BackendCommand>,
) {
    let (
        filtered_items,
        ignored_items,
        recommendations,
        flatpak_recommendations,
        category,
        active_query,
        has_remote_search,
        aur_helper,
        total_counts,
    ) = {
        let mut st = state_rc.borrow_mut();

        for old_group in st.active_groups.drain(..) {
            prefs_page.remove(&old_group);
        }

        let has_remote = st.search_results.is_some() && !st.active_query.is_empty();
        let base_slice: &[PackageItem] = match &st.search_results {
            Some(results) if !st.active_query.is_empty() => results.as_slice(),
            _ => st.catalog_packages.as_slice(),
        };

        let q_low = st.active_query.to_lowercase();
        let is_local_filter = st.search_results.is_none() && !q_low.is_empty();

        let rec_count: usize = st
            .recommendations
            .iter()
            .map(|(_, list)| {
                if is_local_filter {
                    list.iter()
                        .filter(|p| {
                            p.name.to_lowercase().contains(&q_low)
                                || p.description.to_lowercase().contains(&q_low)
                        })
                        .count()
                } else {
                    list.len()
                }
            })
            .sum();

        let fp_rec_count: usize = st
            .flatpak_recommendations
            .iter()
            .map(|(_, list)| {
                if is_local_filter {
                    list.iter()
                        .filter(|p| {
                            p.name.to_lowercase().contains(&q_low)
                                || p.description.to_lowercase().contains(&q_low)
                        })
                        .count()
                } else {
                    list.len()
                }
            })
            .sum();

        let ignored = st.ignored_updates.clone();

        let mut c_all = 0usize;
        let mut c_installed = 0usize;
        let mut c_updates = 0usize;
        let mut c_pacman = 0usize;
        let mut c_aur = 0usize;
        let mut c_flatpak = 0usize;
        let mut c_appimage = 0usize;

        for pkg in base_slice {
            if is_local_filter
                && !pkg.name.to_lowercase().contains(&q_low)
                && !pkg.description.to_lowercase().contains(&q_low)
            {
                continue;
            }
            c_all += 1;
            if pkg.is_installed {
                c_installed += 1;
            }
            if pkg.update_available.is_some() && !ignored.contains(&pkg.id) {
                c_updates += 1;
            }
            match pkg.source {
                PackageSource::Pacman => c_pacman += 1,
                PackageSource::Aur => {
                    if !pkg.is_installed {
                        c_aur += 1;
                    }
                }
                PackageSource::Flatpak => c_flatpak += 1,
                PackageSource::AppImage(_) => c_appimage += 1,
            }
        }

        if !has_remote {
            c_pacman = rec_count;
            c_flatpak = fp_rec_count.max(c_flatpak);
            c_all = rec_count + c_flatpak + c_aur + c_appimage;
        }

        let cat = st.selected_category;
        let mut items: Vec<PackageItem> = base_slice
            .iter()
            .filter(|pkg| {
                if !cat.matches(pkg) {
                    return false;
                }
                if matches!(cat, SidebarCategory::Updates) && ignored.contains(&pkg.id) {
                    return false;
                }
                if is_local_filter {
                    return pkg.matches_search_query(&q_low);
                }
                true
            })
            .cloned()
            .collect();

        let mut ignored_items: Vec<PackageItem> = if matches!(cat, SidebarCategory::Updates) {
            base_slice
                .iter()
                .filter(|pkg| {
                    cat.matches(pkg)
                        && ignored.contains(&pkg.id)
                        && (!is_local_filter || pkg.matches_search_query(&q_low))
                })
                .cloned()
                .collect()
        } else {
            Vec::new()
        };
        ignored_items.sort_by_key(|item| item.name.to_lowercase());

        if matches!(cat, SidebarCategory::Aur) {
            let sort_mode = st.aur_sort_mode;
            items.sort_by(|a, b| sort_mode.compare_items(a, b));
        } else if !q_low.is_empty() {
            items.sort_by(|a, b| a.compare_search_relevance(b, &q_low));
        } else if matches!(cat, SidebarCategory::Pacman | SidebarCategory::Flatpak) {
            items.sort_by(|a, b| {
                a.is_installed
                    .cmp(&b.is_installed)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        } else if matches!(cat, SidebarCategory::All) {
            items.sort_by_key(|a| a.name.to_lowercase());
        }

        let filtered_recs: Vec<(RecommendationCategory, Vec<PackageItem>)> = st
            .recommendations
            .iter()
            .filter_map(|(rec_cat, list)| {
                let mut sub: Vec<PackageItem> = list
                    .iter()
                    .filter(|p| {
                        if is_local_filter {
                            p.name.to_lowercase().contains(&q_low)
                                || p.description.to_lowercase().contains(&q_low)
                        } else {
                            true
                        }
                    })
                    .cloned()
                    .collect();
                sub.sort_by(|a, b| {
                    a.is_installed
                        .cmp(&b.is_installed)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                if sub.is_empty() {
                    None
                } else {
                    Some((*rec_cat, sub))
                }
            })
            .collect();

        let filtered_fp_recs: Vec<(RecommendationCategory, Vec<PackageItem>)> = st
            .flatpak_recommendations
            .iter()
            .filter_map(|(rec_cat, list)| {
                let mut sub: Vec<PackageItem> = list
                    .iter()
                    .filter(|p| {
                        if is_local_filter {
                            p.name.to_lowercase().contains(&q_low)
                                || p.description.to_lowercase().contains(&q_low)
                        } else {
                            true
                        }
                    })
                    .cloned()
                    .collect();
                sub.sort_by(|a, b| {
                    a.is_installed
                        .cmp(&b.is_installed)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                if sub.is_empty() {
                    None
                } else {
                    Some((*rec_cat, sub))
                }
            })
            .collect();

        (
            items,
            ignored_items,
            filtered_recs,
            filtered_fp_recs,
            cat,
            st.active_query.clone(),
            has_remote,
            st.aur_helper,
            (
                c_all,
                c_installed,
                c_updates,
                c_pacman,
                c_aur,
                c_flatpak,
                c_appimage,
            ),
        )
    };

    let sidebar_collapsed = state_rc.borrow().sidebar_collapsed;
    let (c_all, c_installed, c_updates, c_pacman, c_aur, c_flatpak, c_appimage) = total_counts;
    badges.all_label.set_label(&c_all.to_string());
    badges.all_label.set_visible(false);
    badges.installed_label.set_label(&c_installed.to_string());
    badges.installed_label.set_visible(!sidebar_collapsed);
    badges.updates_label.set_label(&c_updates.to_string());
    badges
        .updates_label
        .set_visible(!sidebar_collapsed && c_updates > 0);
    badges.pacman_label.set_label(&c_pacman.to_string());
    badges.pacman_label.set_visible(false);
    badges.aur_label.set_label(&c_aur.to_string());
    badges.aur_label.set_visible(false);
    badges.flatpak_label.set_label(&c_flatpak.to_string());
    badges.flatpak_label.set_visible(false);
    badges.appimages_label.set_label(&c_appimage.to_string());
    badges.appimages_label.set_visible(!sidebar_collapsed);

    for rw in &badges.sidebar_rows {
        let tip = match rw.category {
            SidebarCategory::Installed => format!("{} ({c_installed})", rw.category.title()),
            SidebarCategory::Updates if c_updates > 0 => {
                format!("{} ({c_updates})", rw.category.title())
            }
            SidebarCategory::AppImages => format!("{} ({c_appimage})", rw.category.title()),
            _ => rw.category.title().to_string(),
        };
        rw.row.set_tooltip_text(Some(&tip));
    }

    actions.update_all_btn.set_visible(c_updates > 0);
    actions
        .aur_sort_box
        .set_visible(matches!(category, SidebarCategory::Aur));
    update_selection_buttons(state_rc, actions);

    let mut created_groups = Vec::new();

    if !has_remote_search
        && matches!(category, SidebarCategory::Flatpak)
        && !flatpak_recommendations.is_empty()
    {
        let mut recommended_ids = std::collections::HashSet::new();
        for (rec_cat, rec_items) in &flatpak_recommendations {
            let escaped_title = glib::markup_escape_text(rec_cat.flathub_title());
            let escaped_desc = glib::markup_escape_text(rec_cat.description());
            let group = adw::PreferencesGroup::builder()
                .title(escaped_title.as_str())
                .description(escaped_desc.as_str())
                .build();

            for item in rec_items {
                recommended_ids.insert(item.id.clone());
                let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                group.add(&row);
            }

            prefs_page.add(&group);
            created_groups.push(group);
        }

        let extra_installed_fp: Vec<&PackageItem> = filtered_items
            .iter()
            .filter(|p| !recommended_ids.contains(&p.id))
            .collect();
        if !extra_installed_fp.is_empty() {
            let escaped_title = glib::markup_escape_text("Autres applications Flatpak installées");
            let escaped_desc = glib::markup_escape_text(
                "Applications Flatpak installées sur le système hors sélection recommandée",
            );
            let extra_group = adw::PreferencesGroup::builder()
                .title(escaped_title.as_str())
                .description(escaped_desc.as_str())
                .build();

            for item in extra_installed_fp {
                let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                extra_group.add(&row);
            }

            prefs_page.add(&extra_group);
            created_groups.push(extra_group);
        }

        state_rc.borrow_mut().active_groups = created_groups;
        stack.set_visible_child_name("list");
        return;
    }

    if !has_remote_search
        && matches!(category, SidebarCategory::Pacman | SidebarCategory::All)
        && !recommendations.is_empty()
    {
        for (rec_cat, rec_items) in &recommendations {
            let escaped_title = glib::markup_escape_text(rec_cat.title());
            let escaped_desc = glib::markup_escape_text(rec_cat.description());
            let group = adw::PreferencesGroup::builder()
                .title(escaped_title.as_str())
                .description(escaped_desc.as_str())
                .build();

            for item in rec_items {
                let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                group.add(&row);
            }

            prefs_page.add(&group);
            created_groups.push(group);
        }

        if matches!(category, SidebarCategory::All) {
            for (rec_cat, rec_items) in &flatpak_recommendations {
                let escaped_title = glib::markup_escape_text(rec_cat.flathub_title());
                let escaped_desc = glib::markup_escape_text(rec_cat.description());
                let group = adw::PreferencesGroup::builder()
                    .title(escaped_title.as_str())
                    .description(escaped_desc.as_str())
                    .build();

                for item in rec_items {
                    let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                    group.add(&row);
                }

                prefs_page.add(&group);
                created_groups.push(group);
            }

            let extra_items: Vec<&PackageItem> = filtered_items
                .iter()
                .filter(|p| {
                    !matches!(p.source, PackageSource::Pacman | PackageSource::Flatpak)
                })
                .collect();

            if !extra_items.is_empty() {
                let escaped_title = glib::markup_escape_text("Paquets AUR & AppImages locales");
                let escaped_desc = glib::markup_escape_text(
                    "Applications communautaires et portables présentes sur la machine",
                );
                let extra_group = adw::PreferencesGroup::builder()
                    .title(escaped_title.as_str())
                    .description(escaped_desc.as_str())
                    .build();

                for item in extra_items.into_iter().take(MAX_RENDERED_ROWS) {
                    let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                    extra_group.add(&row);
                }

                prefs_page.add(&extra_group);
                created_groups.push(extra_group);
            }
        }

        state_rc.borrow_mut().active_groups = created_groups;
        stack.set_visible_child_name("list");
        return;
    }

    if matches!(category, SidebarCategory::AppImages) {
        let drop_group = build_appimage_dropzone_group(cmd_tx);
        prefs_page.add(&drop_group);
        created_groups.push(drop_group);

        let installed_desc = format!(
            "{} AppImage(s) installée(s) dans ~/Applications (et dossiers locaux) — Raccourcis du menu démarrer gérés automatiquement",
            filtered_items.len()
        );
        let escaped_title = glib::markup_escape_text("AppImages installées");
        let escaped_desc = glib::markup_escape_text(&installed_desc);

        let check_upd_btn = gtk4::Button::builder()
            .label("Vérifier les mises à jour")
            .icon_name("software-update-available-symbolic")
            .valign(gtk4::Align::Center)
            .tooltip_text("Interroger GitHub Releases pour vérifier si des nouvelles versions existent pour vos AppImages")
            .build();
        {
            let tx_chk = cmd_tx.clone();
            let stack_chk = stack.clone();
            let loading_chk = actions.loading_status_page.clone();
            check_upd_btn.connect_clicked(move |btn| {
                btn.set_sensitive(false);
                loading_chk.set_title("Vérification des mises à jour AppImages…");
                loading_chk.set_description(Some(
                    "Interrogation de GitHub Releases et synchronisation du catalogue…",
                ));
                stack_chk.set_visible_child_name("loading");
                let _ = tx_chk.send(BackendCommand::CheckAppImageUpdates);
            });
        }

        let installed_group = adw::PreferencesGroup::builder()
            .title(escaped_title.as_str())
            .description(escaped_desc.as_str())
            .header_suffix(&check_upd_btn)
            .build();

        if filtered_items.is_empty() {
            let empty_row = adw::ActionRow::builder()
                .title("Aucune AppImage installée pour le moment")
                .subtitle(
                    "Glissez-déposez un fichier .AppImage dans la zone ci-dessus : il sera déplacé dans ~/Applications et ajouté au menu démarrer.",
                )
                .build();
            let empty_icon = gtk4::Image::from_icon_name("application-x-executable-symbolic");
            empty_icon.set_pixel_size(28);
            empty_row.add_prefix(&empty_icon);
            installed_group.add(&empty_row);
        } else {
            for item in filtered_items.iter().take(MAX_RENDERED_ROWS) {
                let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                installed_group.add(&row);
            }
        }

        prefs_page.add(&installed_group);
        created_groups.push(installed_group);
        state_rc.borrow_mut().active_groups = created_groups;
        stack.set_visible_child_name("list");
        return;
    }

    if filtered_items.is_empty() && ignored_items.is_empty() {
        configure_empty_status_page(empty_status_page, category, &active_query, actions);
        stack.set_visible_child_name("empty");
        return;
    }

    // Dans l'onglet Mises à jour : tri alphabétique et catégorisation en Pacman, AUR, Flatpak et AppImages
    if matches!(category, SidebarCategory::Updates) {
        let mut sorted_updates = filtered_items;
        sorted_updates.sort_by_key(|a| a.name.to_lowercase());

        let pacman_upds: Vec<&PackageItem> = sorted_updates
            .iter()
            .filter(|p| matches!(p.source, PackageSource::Pacman))
            .collect();
        let aur_upds: Vec<&PackageItem> = sorted_updates
            .iter()
            .filter(|p| matches!(p.source, PackageSource::Aur))
            .collect();
        let flatpak_upds: Vec<&PackageItem> = sorted_updates
            .iter()
            .filter(|p| matches!(p.source, PackageSource::Flatpak))
            .collect();
        let appimage_upds: Vec<&PackageItem> = sorted_updates
            .iter()
            .filter(|p| matches!(p.source, PackageSource::AppImage(_)))
            .collect();

        let sections: [(&str, &str, Vec<&PackageItem>); 4] = [
            (
                "Pacman · Dépôts officiels",
                "Mises à jour des paquets système Arch Linux synchronisés via Pacman",
                pacman_upds,
            ),
            (
                "AUR · Arch User Repository",
                "Mises à jour des paquets communautaires AUR compilés via Yay",
                aur_upds,
            ),
            (
                "Flatpak · Applications Flathub",
                "Mises à jour des applications conteneurisées Flatpak",
                flatpak_upds,
            ),
            (
                "AppImages · Applications portables",
                "Mises à jour disponibles sur GitHub Releases pour ~/Applications",
                appimage_upds,
            ),
        ];

        for (sec_title, sec_desc, sec_items) in sections {
            if sec_items.is_empty() {
                continue;
            }
            let full_title = format!("{sec_title} ({})", sec_items.len());
            let escaped_title = glib::markup_escape_text(&full_title);
            let escaped_desc = glib::markup_escape_text(sec_desc);
            let sec_group = adw::PreferencesGroup::builder()
                .title(escaped_title.as_str())
                .description(escaped_desc.as_str())
                .build();

            for item in sec_items {
                let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                attach_update_ignore_button(
                    &row,
                    &item.id,
                    false,
                    state_rc,
                    prefs_page,
                    stack,
                    empty_status_page,
                    badges,
                    actions,
                    cmd_tx,
                );
                sec_group.add(&row);
            }

            prefs_page.add(&sec_group);
            created_groups.push(sec_group);
        }

        if !ignored_items.is_empty() {
            let escaped_title = glib::markup_escape_text(&format!(
                "Mises à jour ignorées ({})",
                ignored_items.len()
            ));
            let ignored_group = adw::PreferencesGroup::builder()
                .title(escaped_title.as_str())
                .description(
                    "Ces mises à jour sont exclues de la mise à jour globale.                      Utilisez le bouton de réactivation pour les réintégrer.",
                )
                .build();

            for item in &ignored_items {
                let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
                attach_update_ignore_button(
                    &row,
                    &item.id,
                    true,
                    state_rc,
                    prefs_page,
                    stack,
                    empty_status_page,
                    badges,
                    actions,
                    cmd_tx,
                );
                ignored_group.add(&row);
            }

            prefs_page.add(&ignored_group);
            created_groups.push(ignored_group);
        }

        state_rc.borrow_mut().active_groups = created_groups;
        stack.set_visible_child_name("list");
        return;
    }

    let total_found = filtered_items.len();

    // Dans l'onglet AUR : pagination par pages de AUR_PAGE_SIZE paquets, sans bannière générale d'obsolescence
    if matches!(category, SidebarCategory::Aur) {
        let total_pages = total_found.div_ceil(AUR_PAGE_SIZE).max(1);
        let (current_page, can_load_more, prefetch_wave) = {
            let mut st = state_rc.borrow_mut();
            if st.aur_current_page >= total_pages {
                st.aur_current_page = total_pages - 1;
            }
            let current = st.aur_current_page;
            let has_more_waves = st.active_query.is_empty()
                && st.aur_next_wave_index < backend::aur::AUR_DISCOVERY_WAVES.len();
            let prefetch = if has_more_waves && !st.aur_loading_more && current + 3 >= total_pages {
                st.aur_loading_more = true;
                Some(st.aur_next_wave_index)
            } else {
                None
            };
            (current, has_more_waves, prefetch)
        };

        if let Some(wave_index) = prefetch_wave {
            let _ = cmd_tx.send(BackendCommand::LoadMoreAurPackages { wave_index });
        }

        let start_idx = current_page * AUR_PAGE_SIZE;
        let end_idx = (start_idx + AUR_PAGE_SIZE).min(total_found);
        let page_slice = &filtered_items[start_idx..end_idx];

        let helper_note = match aur_helper {
            Some(h) => format!(" · Assistant AUR détecté : {}", h.display_name()),
            None => String::from(" · Aucun assistant AUR détecté (yay)"),
        };

        let plus_suffix = if can_load_more { "+" } else { "" };
        let group_desc = format!(
            "{} — Page {} sur {total_pages}{plus_suffix} (paquets {} à {} sur {total_found}{plus_suffix}){helper_note}",
            category.subtitle(),
            current_page + 1,
            start_idx + 1,
            end_idx,
        );

        let escaped_group_title = glib::markup_escape_text(category.title());
        let escaped_group_desc = glib::markup_escape_text(&group_desc);
        let group = adw::PreferencesGroup::builder()
            .title(escaped_group_title.as_str())
            .description(escaped_group_desc.as_str())
            .build();

        if total_pages > 1 || can_load_more {
            let top_nav = build_aur_header_pagination_box(
                current_page,
                total_pages,
                can_load_more,
                state_rc,
                prefs_page,
                stack,
                empty_status_page,
                badges,
                actions,
                cmd_tx,
            );
            group.set_header_suffix(Some(&top_nav));
        }

        let header_cols_row = build_aur_columns_header_row();
        group.add(&header_cols_row);

        for item in page_slice {
            let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
            group.add(&row);
        }

        if total_pages > 1 || can_load_more {
            let pagination_row = build_aur_pagination_row(
                current_page,
                total_pages,
                can_load_more,
                total_found,
                start_idx,
                end_idx,
                state_rc,
                prefs_page,
                stack,
                empty_status_page,
                badges,
                actions,
                cmd_tx,
            );
            group.add(&pagination_row);
        }

        prefs_page.add(&group);
        created_groups.push(group);
        state_rc.borrow_mut().active_groups = created_groups;
        stack.set_visible_child_name("list");
        return;
    }

    let displayed_count = total_found.min(MAX_RENDERED_ROWS);

    let group_desc = if total_found > MAX_RENDERED_ROWS {
        format!(
            "{} — Affichage des {displayed_count} premiers résultats sur {total_found}",
            category.subtitle()
        )
    } else {
        format!("{} — {total_found} paquet(s)", category.subtitle())
    };

    let escaped_group_title = glib::markup_escape_text(category.title());
    let escaped_group_desc = glib::markup_escape_text(&group_desc);
    let group = adw::PreferencesGroup::builder()
        .title(escaped_group_title.as_str())
        .description(escaped_group_desc.as_str())
        .build();

    for item in filtered_items.iter().take(MAX_RENDERED_ROWS) {
        let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
        group.add(&row);
    }

    prefs_page.add(&group);
    created_groups.push(group);
    state_rc.borrow_mut().active_groups = created_groups;
    stack.set_visible_child_name("list");
}

#[allow(clippy::too_many_arguments)]
fn build_aur_header_pagination_box(
    current_page: usize,
    total_pages: usize,
    can_load_more: bool,
    state_rc: &Rc<RefCell<AppUiState>>,
    prefs_page: &adw::PreferencesPage,
    stack: &gtk4::Stack,
    empty_status_page: &adw::StatusPage,
    badges: &Rc<CategoryBadgeWidgets>,
    actions: &HeaderActionButtons,
    cmd_tx: &Sender<BackendCommand>,
) -> gtk4::Box {
    let nav_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk4::Align::Center)
        .build();

    let prev_btn = gtk4::Button::builder()
        .icon_name("go-previous-symbolic")
        .sensitive(current_page > 0)
        .tooltip_text("Page précédente")
        .build();
    {
        let st_clone = Rc::clone(state_rc);
        let page_clone = prefs_page.clone();
        let stack_clone = stack.clone();
        let empty_clone = empty_status_page.clone();
        let badges_clone = Rc::clone(badges);
        let actions_clone = actions.clone();
        let tx_clone = cmd_tx.clone();
        prev_btn.connect_clicked(move |_| {
            {
                let mut st = st_clone.borrow_mut();
                st.aur_current_page = st.aur_current_page.saturating_sub(1);
            }
            rebuild_package_list_ui(
                &st_clone,
                &page_clone,
                &stack_clone,
                &empty_clone,
                &badges_clone,
                &actions_clone,
                &tx_clone,
            );
        });
    }
    nav_box.append(&prev_btn);

    let plus_suffix = if can_load_more { "+" } else { "" };
    let page_lbl = gtk4::Label::builder()
        .label(format!(
            "Page {} / {total_pages}{plus_suffix}",
            current_page + 1
        ))
        .margin_start(4)
        .margin_end(4)
        .css_classes(["dim-label"])
        .build();
    nav_box.append(&page_lbl);

    let next_btn = gtk4::Button::builder()
        .icon_name("go-next-symbolic")
        .sensitive(current_page + 1 < total_pages || can_load_more)
        .tooltip_text("Page suivante")
        .build();
    {
        let st_clone = Rc::clone(state_rc);
        let page_clone = prefs_page.clone();
        let stack_clone = stack.clone();
        let empty_clone = empty_status_page.clone();
        let badges_clone = Rc::clone(badges);
        let actions_clone = actions.clone();
        let tx_clone = cmd_tx.clone();
        next_btn.connect_clicked(move |_| {
            let mut load_wave: Option<usize> = None;
            let mut should_rebuild = false;
            {
                let mut st = st_clone.borrow_mut();
                if st.aur_current_page + 1 < total_pages {
                    st.aur_current_page += 1;
                    should_rebuild = true;
                } else if st.aur_next_wave_index < backend::aur::AUR_DISCOVERY_WAVES.len() {
                    st.aur_advance_page_on_load = true;
                    if !st.aur_loading_more {
                        st.aur_loading_more = true;
                        load_wave = Some(st.aur_next_wave_index);
                    }
                }
            }
            if let Some(wave_index) = load_wave {
                let _ = tx_clone.send(BackendCommand::LoadMoreAurPackages { wave_index });
            }
            if should_rebuild {
                rebuild_package_list_ui(
                    &st_clone,
                    &page_clone,
                    &stack_clone,
                    &empty_clone,
                    &badges_clone,
                    &actions_clone,
                    &tx_clone,
                );
            }
        });
    }
    nav_box.append(&next_btn);

    nav_box
}

#[allow(clippy::too_many_arguments)]
fn build_aur_pagination_row(
    current_page: usize,
    total_pages: usize,
    can_load_more: bool,
    total_found: usize,
    start_idx: usize,
    end_idx: usize,
    state_rc: &Rc<RefCell<AppUiState>>,
    prefs_page: &adw::PreferencesPage,
    stack: &gtk4::Stack,
    empty_status_page: &adw::StatusPage,
    badges: &Rc<CategoryBadgeWidgets>,
    actions: &HeaderActionButtons,
    cmd_tx: &Sender<BackendCommand>,
) -> gtk4::ListBoxRow {
    let hbox = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(8)
        .margin_start(14)
        .margin_end(14)
        .margin_top(6)
        .margin_bottom(6)
        .valign(gtk4::Align::Center)
        .build();

    let plus_suffix = if can_load_more { "+" } else { "" };
    let info_lbl = gtk4::Label::builder()
        .label(format!(
            "Paquets {}–{} sur {total_found}{plus_suffix} (Page {} / {total_pages}{plus_suffix})",
            start_idx + 1,
            end_idx,
            current_page + 1,
        ))
        .xalign(0.0)
        .hexpand(true)
        .css_classes(["dim-label"])
        .build();
    hbox.append(&info_lbl);

    let nav_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .valign(gtk4::Align::Center)
        .build();

    let prev_btn = gtk4::Button::builder()
        .label("← Précédent")
        .sensitive(current_page > 0)
        .tooltip_text("Aller à la page précédente de paquets AUR")
        .build();
    {
        let st_clone = Rc::clone(state_rc);
        let page_clone = prefs_page.clone();
        let stack_clone = stack.clone();
        let empty_clone = empty_status_page.clone();
        let badges_clone = Rc::clone(badges);
        let actions_clone = actions.clone();
        let tx_clone = cmd_tx.clone();
        prev_btn.connect_clicked(move |_| {
            {
                let mut st = st_clone.borrow_mut();
                st.aur_current_page = st.aur_current_page.saturating_sub(1);
            }
            rebuild_package_list_ui(
                &st_clone,
                &page_clone,
                &stack_clone,
                &empty_clone,
                &badges_clone,
                &actions_clone,
                &tx_clone,
            );
        });
    }
    nav_box.append(&prev_btn);

    let max_visible_buttons = 7usize;
    let half = max_visible_buttons / 2;
    let start_page = if total_pages <= max_visible_buttons || current_page <= half {
        0
    } else if current_page + half >= total_pages {
        total_pages - max_visible_buttons
    } else {
        current_page - half
    };
    let end_page = (start_page + max_visible_buttons).min(total_pages);

    for page_idx in start_page..end_page {
        let page_btn = gtk4::Button::builder()
            .label((page_idx + 1).to_string())
            .valign(gtk4::Align::Center)
            .tooltip_text(format!("Aller à la page {}", page_idx + 1))
            .build();
        if page_idx == current_page {
            page_btn.add_css_class("suggested-action");
            page_btn.set_sensitive(false);
        } else {
            page_btn.add_css_class("flat");
            let st_clone = Rc::clone(state_rc);
            let page_clone = prefs_page.clone();
            let stack_clone = stack.clone();
            let empty_clone = empty_status_page.clone();
            let badges_clone = Rc::clone(badges);
            let actions_clone = actions.clone();
            let tx_clone = cmd_tx.clone();
            page_btn.connect_clicked(move |_| {
                {
                    let mut st = st_clone.borrow_mut();
                    st.aur_current_page = page_idx;
                }
                rebuild_package_list_ui(
                    &st_clone,
                    &page_clone,
                    &stack_clone,
                    &empty_clone,
                    &badges_clone,
                    &actions_clone,
                    &tx_clone,
                );
            });
        }
        nav_box.append(&page_btn);
    }

    let next_btn = gtk4::Button::builder()
        .label("Suivant →")
        .sensitive(current_page + 1 < total_pages || can_load_more)
        .tooltip_text("Aller à la page suivante de paquets AUR")
        .build();
    {
        let st_clone = Rc::clone(state_rc);
        let page_clone = prefs_page.clone();
        let stack_clone = stack.clone();
        let empty_clone = empty_status_page.clone();
        let badges_clone = Rc::clone(badges);
        let actions_clone = actions.clone();
        let tx_clone = cmd_tx.clone();
        next_btn.connect_clicked(move |_| {
            let mut load_wave: Option<usize> = None;
            let mut should_rebuild = false;
            {
                let mut st = st_clone.borrow_mut();
                if st.aur_current_page + 1 < total_pages {
                    st.aur_current_page += 1;
                    should_rebuild = true;
                } else if st.aur_next_wave_index < backend::aur::AUR_DISCOVERY_WAVES.len() {
                    st.aur_advance_page_on_load = true;
                    if !st.aur_loading_more {
                        st.aur_loading_more = true;
                        load_wave = Some(st.aur_next_wave_index);
                    }
                }
            }
            if let Some(wave_index) = load_wave {
                let _ = tx_clone.send(BackendCommand::LoadMoreAurPackages { wave_index });
            }
            if should_rebuild {
                rebuild_package_list_ui(
                    &st_clone,
                    &page_clone,
                    &stack_clone,
                    &empty_clone,
                    &badges_clone,
                    &actions_clone,
                    &tx_clone,
                );
            }
        });
    }
    nav_box.append(&next_btn);

    hbox.append(&nav_box);

    gtk4::ListBoxRow::builder()
        .activatable(false)
        .selectable(false)
        .css_classes(["aur-pagination-row"])
        .child(&hbox)
        .build()
}

fn build_appimage_dropzone_group(cmd_tx: &Sender<BackendCommand>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Ajouter une AppImage (Glisser-Déposer)")
        .description(
            "Déposez vos fichiers .AppImage ici : ils seront automatiquement déplacés dans ~/Applications, rendus exécutables (+x) et ajoutés au menu démarrer.",
        )
        .build();

    let drop_box = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(10)
        .halign(gtk4::Align::Fill)
        .hexpand(true)
        .css_classes(["appimage-dropzone"])
        .build();

    let icon = gtk4::Image::from_icon_name("folder-download-symbolic");
    icon.set_pixel_size(44);
    drop_box.append(&icon);

    let title_lbl = gtk4::Label::builder()
        .label("Glissez et déposez vos fichiers .AppImage ici")
        .css_classes(["title-4"])
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .build();
    drop_box.append(&title_lbl);

    let sub_lbl = gtk4::Label::builder()
        .label("Déplacement automatique vers ~/Applications · Permission +x · Raccourci dans le menu démarrer")
        .css_classes(["dim-label"])
        .wrap(true)
        .justify(gtk4::Justification::Center)
        .build();
    drop_box.append(&sub_lbl);

    let browse_content = adw::ButtonContent::builder()
        .label("Sélectionner un fichier .AppImage…")
        .icon_name("document-open-symbolic")
        .build();
    let browse_btn = gtk4::Button::builder()
        .child(&browse_content)
        .halign(gtk4::Align::Center)
        .margin_top(4)
        .css_classes(["suggested-action", "pill"])
        .build();

    {
        let tx_browse = cmd_tx.clone();
        browse_btn.connect_clicked(move |btn| {
            let filter = gtk4::FileFilter::new();
            filter.set_name(Some("Fichiers AppImage (*.AppImage)"));
            filter.add_pattern("*.AppImage");
            filter.add_pattern("*.appimage");

            let filters = gtk4::gio::ListStore::new::<gtk4::FileFilter>();
            filters.append(&filter);

            let dialog = gtk4::FileDialog::builder()
                .title("Importer une AppImage dans ~/Applications")
                .filters(&filters)
                .modal(true)
                .build();

            let root_win = btn
                .root()
                .and_then(|r| r.downcast::<gtk4::Window>().ok());
            let tx_inner = tx_browse.clone();
            dialog.open_multiple(
                root_win.as_ref(),
                None::<&gtk4::gio::Cancellable>,
                move |res| {
                    if let Ok(files) = res {
                        let mut paths = Vec::new();
                        for i in 0..files.n_items() {
                            if let Some(obj) = files.item(i) {
                                if let Ok(gfile) = obj.downcast::<gtk4::gio::File>() {
                                    if let Some(p) = gfile.path() {
                                        paths.push(p);
                                    }
                                }
                            }
                        }
                        if !paths.is_empty() {
                            let _ = tx_inner.send(BackendCommand::ImportAppImages { paths });
                        }
                    }
                },
            );
        });
    }
    drop_box.append(&browse_btn);

    let drop_target_files = gtk4::DropTarget::new(
        gtk4::gdk::FileList::static_type(),
        gtk4::gdk::DragAction::COPY,
    );
    {
        let box_enter = drop_box.clone();
        drop_target_files.connect_enter(move |_, _, _| {
            box_enter.add_css_class("appimage-dropzone-active");
            gtk4::gdk::DragAction::COPY
        });
    }
    {
        let box_leave = drop_box.clone();
        drop_target_files.connect_leave(move |_| {
            box_leave.remove_css_class("appimage-dropzone-active");
        });
    }
    {
        let box_drop = drop_box.clone();
        let tx_drop = cmd_tx.clone();
        drop_target_files.connect_drop(move |_, value, _, _| {
            box_drop.remove_css_class("appimage-dropzone-active");
            if let Ok(file_list) = value.get::<gtk4::gdk::FileList>() {
                let paths: Vec<std::path::PathBuf> = file_list
                    .files()
                    .into_iter()
                    .filter_map(|f| f.path())
                    .collect();
                if !paths.is_empty() {
                    let _ = tx_drop.send(BackendCommand::ImportAppImages { paths });
                    return true;
                }
            }
            false
        });
    }
    drop_box.add_controller(drop_target_files);

    group.add(&drop_box);
    group
}

fn configure_empty_status_page(
    status_page: &adw::StatusPage,
    category: SidebarCategory,
    active_query: &str,
    actions: &HeaderActionButtons,
) {
    if !active_query.is_empty() {
        actions.empty_check_updates_btn.set_visible(false);
        status_page.set_icon_name(Some("system-search-symbolic"));
        status_page.set_title("Aucun résultat trouvé");
        status_page.set_description(Some(&format!(
            "Aucun paquet ne correspond à « {active_query} » dans la catégorie « {} ».",
            category.title()
        )));
        return;
    }

    actions
        .empty_check_updates_btn
        .set_visible(matches!(category, SidebarCategory::Updates));

    match category {
        SidebarCategory::Updates => {
            status_page.set_icon_name(Some("object-select-symbolic"));
            status_page.set_title("Système entièrement à jour");
            status_page.set_description(Some(
                "Aucune mise à jour en attente pour vos paquets Pacman, AUR, Flatpak ou AppImages.",
            ));
        }
        SidebarCategory::AppImages => {
            status_page.set_icon_name(Some("application-x-executable-symbolic"));
            status_page.set_title("Aucune AppImage détectée");
            status_page.set_description(Some(
                "Glissez-déposez vos fichiers .AppImage ou placez-les dans ~/Applications.",
            ));
        }
        SidebarCategory::Aur => {
            status_page.set_icon_name(Some("system-software-install-symbolic"));
            status_page.set_title("Aucun paquet AUR affiché");
            status_page.set_description(Some(
                "Utilisez la barre de recherche en haut pour explorer l'API AUR RPC v5.",
            ));
        }
        _ => {
            status_page.set_icon_name(Some("package-x-generic-symbolic"));
            status_page.set_title("Aucun paquet dans cette vue");
            status_page.set_description(Some(category.subtitle()));
        }
    }
}

fn spawn_backend_worker(
    cmd_rx: Receiver<BackendCommand>,
    ui_tx: async_channel::Sender<UiEvent>,
    cancel_handle: Arc<InstallCancelHandle>,
) {
    std::thread::spawn(move || {
        let rt = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(4)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                let _ = ui_tx.send_blocking(UiEvent::OperationError {
                    title: String::from("Erreur critique du moteur asynchrone"),
                    detail: e.to_string(),
                });
                return;
            }
        };

        while let Ok(command) = cmd_rx.recv() {
            let tx = ui_tx.clone();
            let cancel_ref = Arc::clone(&cancel_handle);
            rt.block_on(async move {
                handle_backend_command(command, &cancel_ref, &tx).await;
            });
        }
    });
}

async fn handle_backend_command(
    command: BackendCommand,
    cancel_handle: &InstallCancelHandle,
    ui_tx: &async_channel::Sender<UiEvent>,
) {
    match command {
        BackendCommand::RefreshCatalog => {
            if let Some(cached_snapshot) = backend::cache::load_cached_snapshot() {
                // 1. Affichage instantané depuis le cache local sur disque
                let _ = ui_tx.send(UiEvent::CatalogLoaded(cached_snapshot.clone())).await;

                // 2. Calcul rapide du différentiel local (paquets ajoutés/retirés/modifiés)
                let _ = ui_tx
                    .send(UiEvent::LoadingStarted {
                        message: String::from(
                            "Calcul du différentiel local et actualisation des mises à jour…",
                        ),
                    })
                    .await;

                let base_for_updates =
                    match backend::apply_incremental_catalog_diff(&cached_snapshot).await {
                        Ok((diff_snapshot, has_diff)) => {
                            if has_diff {
                                let _ = ui_tx
                                    .send(UiEvent::CatalogLoaded(diff_snapshot.clone()))
                                    .await;
                                let _ = ui_tx
                                    .send(UiEvent::LoadingStarted {
                                        message: String::from(
                                            "Actualisation des mises à jour (Pacman, AUR, Flatpak, AppImages)…",
                                        ),
                                    })
                                    .await;
                            }
                            diff_snapshot
                        }
                        Err(_) => cached_snapshot,
                    };

                // 3. Actualisation asynchrone des mises à jour en arrière-plan
                match backend::refresh_catalog_updates(base_for_updates, false).await {
                    Ok(updated_snapshot) => {
                        let _ = ui_tx.send(UiEvent::CatalogLoaded(updated_snapshot)).await;
                    }
                    Err(err) => {
                        let _ = ui_tx
                            .send(UiEvent::OperationError {
                                title: String::from(
                                    "Échec de l'actualisation des mises à jour",
                                ),
                                detail: err.to_string(),
                            })
                            .await;
                    }
                }
            } else {
                let _ = ui_tx
                    .send(UiEvent::LoadingStarted {
                        message: String::from(
                            "Initialisation du catalogue local et création du cache…",
                        ),
                    })
                    .await;

                match backend::load_system_catalog().await {
                    Ok(snapshot) => {
                        let _ = ui_tx.send(UiEvent::CatalogLoaded(snapshot)).await;
                    }
                    Err(err) => {
                        let _ = ui_tx
                            .send(UiEvent::OperationError {
                                title: String::from("Échec du chargement du catalogue"),
                                detail: err.to_string(),
                            })
                            .await;
                    }
                }
            }
        }
        BackendCommand::CheckAllUpdates => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from(
                        "Vérification complète des mises à jour (Pacman, AUR, Flatpak, AppImages)…",
                    ),
                })
                .await;

            let check_res = if let Some(cached_snapshot) = backend::cache::load_cached_snapshot() {
                let base_snapshot = match backend::apply_incremental_catalog_diff(&cached_snapshot)
                    .await
                {
                    Ok((diff_snap, _)) => diff_snap,
                    Err(_) => cached_snapshot,
                };
                backend::refresh_catalog_updates(base_snapshot, true).await
            } else {
                backend::load_system_catalog_with_options(true).await
            };

            match check_res {
                Ok(snapshot) => {
                    let summary_toast = if snapshot.updates_count > 0 {
                        format!(
                            "Vérification terminée : {} mise(s) à jour disponible(s).",
                            snapshot.updates_count
                        )
                    } else {
                        String::from(
                            "Vérification terminée : tous vos paquets (Pacman, AUR, Flatpak, AppImages) sont à jour.",
                        )
                    };
                    let _ = ui_tx
                        .send(UiEvent::UpdatesChecked {
                            snapshot,
                            summary_toast,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: String::from("Échec de la vérification des mises à jour"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::Search { query } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: format!("Recherche de « {query} » sur Pacman, AUR et AppImages…"),
                })
                .await;

            match backend::search_all_sources(&query).await {
                Ok((results, warnings)) => {
                    let _ = ui_tx
                        .send(UiEvent::SearchCompleted {
                            query,
                            results,
                            warnings,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: format!("Échec de la recherche « {query} »"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::FetchPackageDetails { item } => {
            match backend::details::fetch_package_detailed_info(&item).await {
                Ok(info) => {
                    let _ = ui_tx
                        .send(UiEvent::PackageDetailsLoaded(Box::new(info)))
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: format!("Impossible de charger la fiche de « {} »", item.name),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::PrepareInstallBatch { items } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from(
                        "Vérification des dépendances optionnelles (Pacman / AUR)…",
                    ),
                })
                .await;

            match backend::resolve_optional_dependencies(&items).await {
                Ok(optional_deps) if optional_deps.iter().any(|d| !d.is_installed) => {
                    let _ = ui_tx
                        .send(UiEvent::OptionalDependenciesPrompt {
                            items,
                            optional_deps,
                        })
                        .await;
                }
                _ => {
                    backend::install_batch_with_progress(&items, cancel_handle, ui_tx).await;
                }
            }
        }
        BackendCommand::InstallBatch { items } => {
            backend::install_batch_with_progress(&items, cancel_handle, ui_tx).await;
        }
        BackendCommand::InstallPackage { name, source } => {
            let single = PackageItem {
                id: format!("{}:{name}", source.label().to_lowercase()),
                name,
                version: String::new(),
                description: String::new(),
                source,
                is_installed: false,
                update_available: None,
                aur_meta: None,
            };
            backend::install_batch_with_progress(&[single], cancel_handle, ui_tx).await;
        }
        BackendCommand::UpdatePackage { name, source } => {
            let single = PackageItem {
                id: format!("{}:{name}", source.label().to_lowercase()),
                name,
                version: String::new(),
                description: String::new(),
                source,
                is_installed: true,
                update_available: None,
                aur_meta: None,
            };
            backend::install_batch_with_progress(&[single], cancel_handle, ui_tx).await;
        }
        BackendCommand::RemovePackage { name, source } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: format!("Désinstallation de « {name} » en cours…"),
                })
                .await;

            match backend::remove_item(&name, &source).await {
                Ok(msg) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationSuccess {
                            message: msg,
                            reload_catalog: true,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: format!("Échec de la désinstallation de « {name} »"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::ExecuteAppImage { name, path } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: format!("Lancement de l'AppImage « {name} »…"),
                })
                .await;

            let exec_res =
                tokio::task::spawn_blocking(move || backend::appimage::execute_appimage(&path))
                    .await;

            match exec_res {
                Ok(Ok(msg)) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationSuccess {
                            message: msg,
                            reload_catalog: true,
                        })
                        .await;
                }
                Ok(Err(err)) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: format!("Impossible d'exécuter « {name} »"),
                            detail: err.to_string(),
                        })
                        .await;
                }
                Err(join_err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: format!("Erreur d'exécution pour « {name} »"),
                            detail: join_err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::ImportAppImages { paths } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from(
                        "Déplacement de l'AppImage vers ~/Applications et création du raccourci menu démarrer…",
                    ),
                })
                .await;

            let import_res = tokio::task::spawn_blocking(move || {
                let mut imported_names = Vec::new();
                let mut errors = Vec::new();
                for path in paths {
                    match backend::appimage::integrate_dropped_appimage(&path) {
                        Ok(item) => imported_names.push(item.name),
                        Err(e) => errors.push(e.to_string()),
                    }
                }
                (imported_names, errors)
            })
            .await;

            match import_res {
                Ok((imported, errors)) => {
                    if !imported.is_empty() {
                        let _ = ui_tx
                            .send(UiEvent::OperationSuccess {
                                message: format!(
                                    "AppImage(s) installée(s) dans ~/Applications avec raccourci menu démarrer : {}",
                                    imported.join(", ")
                                ),
                                reload_catalog: true,
                            })
                            .await;
                    }
                    if !errors.is_empty() {
                        let _ = ui_tx
                            .send(UiEvent::OperationError {
                                title: String::from("Erreur lors de l'importation AppImage"),
                                detail: errors.join(" · "),
                            })
                            .await;
                    }
                }
                Err(join_err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: String::from("Erreur interne d'importation AppImage"),
                            detail: join_err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::CheckAppImageUpdates => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from(
                        "Vérification des mises à jour des AppImages sur GitHub Releases…",
                    ),
                })
                .await;

            let scanned = tokio::task::spawn_blocking(backend::appimage::scan_default_directories)
                .await
                .ok()
                .and_then(|r| r.ok())
                .unwrap_or_default();

            let msg = if scanned.is_empty() {
                tokio::time::sleep(Duration::from_millis(600)).await;
                String::from(
                    "Aucune AppImage n'est installée dans ~/Applications (0 fichier à vérifier).",
                )
            } else {
                let updates = backend::appimage::check_appimage_updates(&scanned).await;
                let count = updates.len();
                if count > 0 {
                    format!("{count} mise(s) à jour d'AppImage détectée(s) sur GitHub Releases.")
                } else {
                    format!(
                        "Vérification terminée : vos {} AppImage(s) sont à jour.",
                        scanned.len()
                    )
                }
            };

            let _ = ui_tx
                .send(UiEvent::OperationSuccess {
                    message: msg,
                    reload_catalog: true,
                })
                .await;
        }
        BackendCommand::UpdateAppImage { item } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: format!(
                        "Téléchargement de la mise à jour de l'AppImage « {} »…",
                        item.name
                    ),
                })
                .await;

            match backend::appimage::update_appimage_from_github(&item).await {
                Ok(msg) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationSuccess {
                            message: msg,
                            reload_catalog: true,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: format!(
                                "Échec de la mise à jour de l'AppImage « {} »",
                                item.name
                            ),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::LaunchFlatpak { app_id, name } => {
            match backend::flatpak::launch_flatpak_app(&app_id) {
                Ok(msg) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationSuccess {
                            message: msg,
                            reload_catalog: false,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: format!("Impossible de lancer « {name} »"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::UpdateAllSystem { items, ignored } => {
            backend::update_all_system_and_aur_with_progress(
                &items,
                &ignored,
                cancel_handle,
                ui_tx,
            )
            .await;
        }
        BackendCommand::CheckPacmanLock => {
            if let Some(lock_info) = backend::pacman::inspect_pacman_lock().await {
                let _ = ui_tx.send(UiEvent::PacmanLockDetected(lock_info)).await;
            } else {
                let _ = ui_tx.send(UiEvent::PacmanLockCleared).await;
            }
        }
        BackendCommand::RepairPacmanLock => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from(
                        "Réparation et déverrouillage de la base de données Pacman (/var/lib/pacman/db.lck)…",
                    ),
                })
                .await;

            match backend::pacman::repair_pacman_lock().await {
                Ok(message) => {
                    let _ = ui_tx.send(UiEvent::PacmanLockRepaired { message }).await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: String::from("Échec de la réparation du verrouillage Pacman"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::FetchAurVersions { package } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: format!("Lecture de l'historique AUR de « {package} »…"),
                })
                .await;

            match backend::aur_versions::list_versions(&package).await {
                Ok((package_base, versions)) => {
                    let _ = ui_tx
                        .send(UiEvent::AurVersionsLoaded {
                            package,
                            package_base,
                            versions,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::AurVersionsError(err.to_string()))
                        .await;
                }
            }
        }
        BackendCommand::InstallAurVersion {
            package,
            package_base,
            version,
            commit,
        } => {
            let target = format!("{package} {version}");
            let _ = ui_tx
                .send(UiEvent::InstallStarted {
                    target_names: vec![target.clone()],
                })
                .await;

            let elevation = Elevation::detect();
            match backend::aur_versions::install_version(
                &package,
                &package_base,
                &version,
                &commit,
                &elevation,
                cancel_handle,
                ui_tx,
            )
            .await
            {
                Ok(message) => {
                    let _ = ui_tx
                        .send(UiEvent::InstallFinished {
                            success: true,
                            message,
                            installed_ids: vec![format!("aur:{package}")],
                        })
                        .await;
                    let _ = ui_tx.send(UiEvent::CatalogReloadRequested).await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::InstallFinished {
                            success: false,
                            message: format!("Échec de l'installation de {target} : {err}"),
                            installed_ids: Vec::new(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::ListReflectorMirrors => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from("Analyse des miroirs et mesure des débits…"),
                })
                .await;

            match backend::reflector::list_mirrors().await {
                Ok(mirrors) => {
                    let _ = ui_tx.send(UiEvent::MirrorListLoaded(mirrors)).await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::ReflectorError(err.to_string()))
                        .await;
                }
            }
        }
        BackendCommand::ApplyMirrorlist { servers } => {
            let elevation = Elevation::detect();
            match backend::reflector::apply_mirrorlist(&elevation, &servers).await {
                Ok(message) => {
                    let _ = ui_tx
                        .send(UiEvent::MirrorListApplied {
                            message: message.clone(),
                        })
                        .await;
                    let _ = ui_tx
                        .send(UiEvent::OperationSuccess {
                            message,
                            reload_catalog: false,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: String::from("Application des miroirs"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::GetPacmanCacheSize => {
            match backend::pacman::pacman_cache_size_bytes().await {
                Ok(bytes) => {
                    let _ = ui_tx.send(UiEvent::PacmanCacheSize { bytes }).await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::PacmanCacheError(err.to_string()))
                        .await;
                }
            }
        }
        BackendCommand::CleanPacmanCache => {
            let elevation = Elevation::detect();
            match backend::pacman::clean_pacman_cache(&elevation).await {
                Ok(message) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationSuccess {
                            message,
                            reload_catalog: false,
                        })
                        .await;
                    if let Ok(bytes) = backend::pacman::pacman_cache_size_bytes().await {
                        let _ = ui_tx.send(UiEvent::PacmanCacheSize { bytes }).await;
                    }
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::PacmanCacheError(err.to_string()))
                        .await;
                }
            }
        }
        BackendCommand::CheckServices => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from("Vérification des services à redémarrer…"),
                })
                .await;

            let elevation = Elevation::detect();
            match backend::needrestart::detect(&elevation).await {
                Ok(report) => {
                    if report.services.is_empty()
                        && !backend::needrestart::reboot_recommended(report.kernel_status)
                    {
                        let _ = ui_tx
                            .send(UiEvent::OperationSuccess {
                                message: String::from(
                                    "Aucun service ne nécessite de redémarrage.",
                                ),
                                reload_catalog: false,
                            })
                            .await;
                    } else {
                        let _ = ui_tx
                            .send(UiEvent::ServicesNeedRestart {
                                services: report.services,
                                kernel_status: report.kernel_status,
                            })
                            .await;
                    }
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: String::from("Vérification des services"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::RestartServices {
            services,
            kernel_status,
        } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from("Redémarrage des services…"),
                })
                .await;

            match backend::needrestart::restart_services(&services).await {
                Ok(report) => {
                    let _ = ui_tx
                        .send(UiEvent::ServicesRestarted {
                            restarted: report.restarted,
                            failures: report.failures,
                            kernel_status,
                        })
                        .await;
                }
                Err(err) => {
                    let _ = ui_tx
                        .send(UiEvent::OperationError {
                            title: String::from("Redémarrage des services"),
                            detail: err.to_string(),
                        })
                        .await;
                }
            }
        }
        BackendCommand::LoadMoreAurPackages { wave_index } => {
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from("Chargement de paquets AUR supplémentaires…"),
                })
                .await;
            let items = backend::load_more_aur_discovery_wave(wave_index).await;
            let _ = ui_tx
                .send(UiEvent::AurMorePackagesLoaded { wave_index, items })
                .await;
        }
    }
}

fn merge_preserved_aur_catalog(new_packages: &mut Vec<PackageItem>, old_packages: &[PackageItem]) {
    let existing_names: std::collections::HashSet<String> =
        new_packages.iter().map(|p| p.name.clone()).collect();
    for pkg in old_packages {
        if pkg.source == PackageSource::Aur
            && !pkg.is_installed
            && !existing_names.contains(&pkg.name)
        {
            new_packages.push(pkg.clone());
        }
    }
}
