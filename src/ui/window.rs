use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend;
use crate::backend::models::{
    AurHelper, AurSortMode, InstallCancelHandle, PackageItem, PackageSource,
    RecommendationCategory, SidebarCategory,
};
use crate::ui::details_dialog::PackageDetailsDialog;
use crate::ui::install_dialog::InstallManagerUi;
use crate::ui::messages::{BackendCommand, UiEvent};
use crate::ui::row::{build_aur_columns_header_row, build_package_action_row};

const MAX_RENDERED_ROWS: usize = 250;

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
    updates_count: usize,
    aur_helper: Option<AurHelper>,
    active_groups: Vec<adw::PreferencesGroup>,
    debounce_source: Option<glib::SourceId>,
}

struct CategoryBadgeWidgets {
    all_label: gtk4::Label,
    installed_label: gtk4::Label,
    updates_label: gtk4::Label,
    pacman_label: gtk4::Label,
    aur_label: gtk4::Label,
    flatpak_label: gtk4::Label,
    appimages_label: gtk4::Label,
}

#[derive(Clone)]
struct HeaderActionButtons {
    batch_install_btn: gtk4::Button,
    clear_selection_btn: gtk4::Button,
    update_all_btn: gtk4::Button,
    aur_sort_box: gtk4::Box,
    details_dialog: PackageDetailsDialog,
}

pub fn build_main_window(app: &adw::Application) -> adw::ApplicationWindow {
    let (cmd_tx, cmd_rx) = mpsc::channel::<BackendCommand>();
    let (ui_tx, ui_rx) = async_channel::unbounded::<UiEvent>();
    let cancel_handle = Arc::new(InstallCancelHandle::new());

    spawn_backend_worker(cmd_rx, ui_tx, Arc::clone(&cancel_handle));

    let state = Rc::new(RefCell::new(AppUiState::default()));

    let toast_overlay = adw::ToastOverlay::new();

    let split_view = adw::NavigationSplitView::builder()
        .min_sidebar_width(240.0)
        .max_sidebar_width(300.0)
        .sidebar_width_fraction(0.25)
        .build();

    // --- Construction de la Sidebar ---
    let sidebar_toolbar = adw::ToolbarView::new();
    let sidebar_header = adw::HeaderBar::builder()
        .show_end_title_buttons(false)
        .build();
    let sidebar_title = adw::WindowTitle::new("Noos", "Gestionnaire Arch");
    sidebar_header.set_title_widget(Some(&sidebar_title));
    sidebar_toolbar.add_top_bar(&sidebar_header);

    let sidebar_list = gtk4::ListBox::builder()
        .selection_mode(gtk4::SelectionMode::Single)
        .css_classes(["navigation-sidebar"])
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(6)
        .margin_end(6)
        .build();

    let all_badge = create_sidebar_badge(false);
    let installed_badge = create_sidebar_badge(false);
    let updates_badge = create_sidebar_badge(true);
    let pacman_badge = create_sidebar_badge(false);
    let aur_badge = create_sidebar_badge(false);
    let flatpak_badge = create_sidebar_badge(false);
    let appimages_badge = create_sidebar_badge(false);

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
        let row = create_sidebar_row(category, badge_ref);
        sidebar_list.append(&row);
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

    let sidebar_page = adw::NavigationPage::builder()
        .title("Catégories")
        .tag("sidebar")
        .child(&sidebar_toolbar)
        .build();

    // --- Construction de la Vue de Contenu ---
    let content_toolbar = adw::ToolbarView::new();
    let content_header = adw::HeaderBar::new();

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

    let empty_status_page = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title("Aucun paquet à afficher")
        .description("Modifiez votre recherche ou sélectionnez une autre catégorie.")
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
        .title("Noos Package Manager")
        .default_width(1180)
        .default_height(760)
        .width_request(760)
        .height_request(500)
        .content(&toast_overlay)
        .build();

    // Fenêtre du Gestionnaire d'installation et Fenêtre d'informations détaillées
    let install_manager = InstallManagerUi::new(&window, Arc::clone(&cancel_handle));
    let details_dialog = PackageDetailsDialog::new(&window);

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
    });

    let header_actions = HeaderActionButtons {
        batch_install_btn: batch_install_btn.clone(),
        clear_selection_btn: clear_selection_btn.clone(),
        update_all_btn: update_all_btn.clone(),
        aur_sort_box: aur_sort_box.clone(),
        details_dialog,
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
            let _ = tx_batch.send(BackendCommand::InstallBatch {
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
            }
            nav_content.set_title(category.title());

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
                if text.is_empty() {
                    st.search_results = None;
                }
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

    // --- Connexion du bouton de rafraîchissement ---
    {
        let tx_refresh = cmd_tx.clone();
        let search_ref = search_entry.clone();
        refresh_btn.connect_clicked(move |_| {
            let current_query = search_ref.text().trim().to_string();
            if current_query.is_empty() {
                let _ = tx_refresh.send(BackendCommand::RefreshCatalog);
            } else {
                let _ = tx_refresh.send(BackendCommand::Search {
                    query: current_query,
                });
            }
        });
    }

    // --- Connexion du bouton "Tout mettre à jour" ---
    {
        let tx_upd_all = cmd_tx.clone();
        update_all_btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            let _ = tx_upd_all.send(BackendCommand::UpdateAllSystem);
        });
    }

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
        let install_mgr_ui = install_manager;
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
                        actions_ui.update_all_btn.set_sensitive(true);

                        if !snapshot.non_fatal_warnings.is_empty() {
                            let joined = snapshot.non_fatal_warnings.join(" · ");
                            banner_ui.set_title(&joined);
                            banner_ui.set_revealed(true);
                        } else {
                            banner_ui.set_revealed(false);
                        }

                        {
                            let mut st = state_ui.borrow_mut();
                            st.catalog_packages = snapshot.packages;
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

                        maybe_schedule_automated_screenshots(&win_for_shots, &sidebar_for_shots);
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
                    UiEvent::PackageDetailsLoaded(info) => {
                        actions_ui.details_dialog.show_details(&info);
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

                        install_mgr_ui.on_install_finished(success, &message);

                        if success {
                            {
                                let mut st = state_ui.borrow_mut();
                                for id in &installed_ids {
                                    st.selected_packages.remove(id);
                                }
                            }
                            update_selection_buttons(&state_ui, &actions_ui);

                            let toast = adw::Toast::builder()
                                .title(&message)
                                .timeout(4)
                                .build();
                            toast_ui.add_toast(toast);

                            let _ = tx_ui.send(BackendCommand::RefreshCatalog);
                        } else {
                            update_selection_buttons(&state_ui, &actions_ui);
                            banner_ui.set_title(&message);
                            banner_ui.set_revealed(true);
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
                        actions_ui.update_all_btn.set_sensitive(true);

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
                        spinner_ui.set_visible(false);
                        actions_ui.update_all_btn.set_sensitive(true);
                        update_selection_buttons(&state_ui, &actions_ui);

                        let toast = adw::Toast::builder()
                            .title(&title)
                            .timeout(5)
                            .build();
                        toast_ui.add_toast(toast);

                        banner_ui.set_title(&format!("{title} — {detail}"));
                        banner_ui.set_revealed(true);

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
            }
        });
    }

    let _ = cmd_tx.send(BackendCommand::RefreshCatalog);

    window
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

fn create_sidebar_row(category: SidebarCategory, badge: &gtk4::Label) -> gtk4::ListBoxRow {
    let hbox = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(12)
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(10)
        .margin_end(10)
        .build();

    let icon = gtk4::Image::from_icon_name(category.icon_name());
    hbox.append(&icon);

    let title_label = gtk4::Label::builder()
        .label(category.title())
        .xalign(0.0)
        .hexpand(true)
        .build();
    hbox.append(&title_label);

    hbox.append(badge);

    gtk4::ListBoxRow::builder().child(&hbox).build()
}

fn make_row_with_selection(
    item: &PackageItem,
    state_rc: &Rc<RefCell<AppUiState>>,
    actions: &HeaderActionButtons,
    cmd_tx: &Sender<BackendCommand>,
) -> adw::ActionRow {
    let (is_checked, show_source_badge) = {
        let st = state_rc.borrow();
        (
            st.selected_packages.contains_key(&item.id),
            matches!(st.selected_category, SidebarCategory::Installed),
        )
    };
    let state_for_check = Rc::clone(state_rc);
    let actions_for_check = actions.clone();
    let item_for_check = item.clone();

    let dialog_for_info = actions.details_dialog.clone();
    let item_for_info = item.clone();
    let tx_for_info = cmd_tx.clone();

    build_package_action_row(
        item,
        is_checked,
        show_source_badge,
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
    )
}

fn rebuild_package_list_ui(
    state_rc: &Rc<RefCell<AppUiState>>,
    prefs_page: &adw::PreferencesPage,
    stack: &gtk4::Stack,
    empty_status_page: &adw::StatusPage,
    badges: &CategoryBadgeWidgets,
    actions: &HeaderActionButtons,
    cmd_tx: &Sender<BackendCommand>,
) {
    let (
        filtered_items,
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
            if pkg.update_available.is_some() {
                c_updates += 1;
            }
            match pkg.source {
                PackageSource::Pacman => c_pacman += 1,
                PackageSource::Aur => c_aur += 1,
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
                if is_local_filter {
                    return pkg.name.to_lowercase().contains(&q_low)
                        || pkg.description.to_lowercase().contains(&q_low);
                }
                true
            })
            .cloned()
            .collect();

        if matches!(cat, SidebarCategory::Pacman | SidebarCategory::Flatpak) {
            items.sort_by(|a, b| {
                a.is_installed
                    .cmp(&b.is_installed)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        } else if matches!(cat, SidebarCategory::Aur) {
            let sort_mode = st.aur_sort_mode;
            items.sort_by(|a, b| sort_mode.compare_items(a, b));
        } else if matches!(cat, SidebarCategory::All) && !has_remote {
            let sort_mode = st.aur_sort_mode;
            items.sort_by(|a, b| match (&a.source, &b.source) {
                (PackageSource::Aur, PackageSource::Aur) => sort_mode.compare_items(a, b),
                _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            });
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

    let (c_all, c_installed, c_updates, c_pacman, c_aur, c_flatpak, c_appimage) = total_counts;
    badges.all_label.set_label(&c_all.to_string());
    badges.installed_label.set_label(&c_installed.to_string());
    badges.updates_label.set_label(&c_updates.to_string());
    badges.updates_label.set_visible(c_updates > 0);
    badges.pacman_label.set_label(&c_pacman.to_string());
    badges.aur_label.set_label(&c_aur.to_string());
    badges.flatpak_label.set_label(&c_flatpak.to_string());
    badges.appimages_label.set_label(&c_appimage.to_string());

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
            check_upd_btn.connect_clicked(move |btn| {
                btn.set_sensitive(false);
                let _ = tx_chk.send(BackendCommand::CheckAppImageUpdates);
                btn.set_sensitive(true);
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

    if filtered_items.is_empty() {
        configure_empty_status_page(empty_status_page, category, &active_query);
        stack.set_visible_child_name("empty");
        return;
    }

    let total_found = filtered_items.len();
    let displayed_count = total_found.min(MAX_RENDERED_ROWS);

    let helper_note = match (category, aur_helper) {
        (SidebarCategory::Aur, Some(h)) => {
            format!(" · Assistant AUR détecté : {}", h.display_name())
        }
        (SidebarCategory::Aur, None) => {
            String::from(" · Aucun assistant AUR détecté (paru/yay)")
        }
        _ => String::new(),
    };

    let group_desc = if total_found > MAX_RENDERED_ROWS {
        format!(
            "{} — Affichage des {displayed_count} premiers résultats sur {total_found}{helper_note}",
            category.subtitle()
        )
    } else {
        format!(
            "{} — {total_found} paquet(s){helper_note}",
            category.subtitle()
        )
    };

    let escaped_group_title = glib::markup_escape_text(category.title());
    let escaped_group_desc = glib::markup_escape_text(&group_desc);
    let group = adw::PreferencesGroup::builder()
        .title(escaped_group_title.as_str())
        .description(escaped_group_desc.as_str())
        .build();

    if matches!(category, SidebarCategory::Aur) {
        let obsolete_pkgs: Vec<&str> = filtered_items
            .iter()
            .filter(|p| p.is_aur_out_of_date())
            .map(|p| p.name.as_str())
            .collect();

        if !obsolete_pkgs.is_empty() {
            let warn_title = format!(
                "⚠️ {} paquet(s) AUR marqué(s) comme obsolète(s) (Out-of-Date)",
                obsolete_pkgs.len()
            );
            let warn_sub = format!(
                "Les paquets suivants ont été signalés comme obsolètes sur AUR : {}",
                obsolete_pkgs.join(", ")
            );
            let warn_row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&warn_title).as_str())
                .subtitle(glib::markup_escape_text(&warn_sub).as_str())
                .build();
            let warn_icon = gtk4::Image::from_icon_name("dialog-warning-symbolic");
            warn_icon.set_pixel_size(24);
            warn_row.add_prefix(&warn_icon);
            group.add(&warn_row);
        }

        let header_cols_row = build_aur_columns_header_row();
        group.add(&header_cols_row);
    }

    for item in filtered_items.iter().take(MAX_RENDERED_ROWS) {
        let row = make_row_with_selection(item, state_rc, actions, cmd_tx);
        group.add(&row);
    }

    prefs_page.add(&group);
    created_groups.push(group);
    state_rc.borrow_mut().active_groups = created_groups;
    stack.set_visible_child_name("list");
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
) {
    if !active_query.is_empty() {
        status_page.set_icon_name(Some("system-search-symbolic"));
        status_page.set_title("Aucun résultat trouvé");
        status_page.set_description(Some(&format!(
            "Aucun paquet ne correspond à « {active_query} » dans la catégorie « {} ».",
            category.title()
        )));
        return;
    }

    match category {
        SidebarCategory::Updates => {
            status_page.set_icon_name(Some("object-select-symbolic"));
            status_page.set_title("Système entièrement à jour");
            status_page.set_description(Some(
                "Aucune mise à jour en attente pour vos paquets Pacman ou AUR.",
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
            let _ = ui_tx
                .send(UiEvent::LoadingStarted {
                    message: String::from(
                        "Synchronisation des recommandations, paquets installés et AppImages…",
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

            let updates = backend::appimage::check_appimage_updates(&scanned).await;
            let count = updates.len();
            let msg = if count > 0 {
                format!("{count} mise(s) à jour d'AppImage détectée(s) sur GitHub Releases.")
            } else {
                String::from("Toutes vos AppImages sont à jour.")
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
        BackendCommand::UpdateAllSystem => {
            backend::update_all_system_and_aur_with_progress(cancel_handle, ui_tx).await;
        }
    }
}
