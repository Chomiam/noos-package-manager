use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;

use crate::backend::elevation::{
    askpass_session_is_safe, authentication_is_cancelled, mark_authentication_cancelled,
    ASKPASS_SESSION_ENV,
};

/// Point d'entrée du mode caché « askpass » invoqué par `sudo -A`.
///
/// Le binaire principal est utilisé comme programme askpass afin de ne dépendre
/// d'aucun utilitaire externe (zenity, ksshaskpass, etc.). Le mot de passe est
/// uniquement transmis à sudo sur la sortie standard : il n'est jamais écrit sur disque.
pub fn run_askpass() -> ! {
    let code = run_askpass_inner();
    std::process::exit(code);
}

fn run_askpass_inner() -> i32 {
    let session_dir = env::var_os(ASKPASS_SESSION_ENV)
        .map(PathBuf::from)
        .filter(|path| askpass_session_is_safe(path));

    if let Some(dir) = &session_dir {
        if authentication_is_cancelled(dir) {
            return 1;
        }
    }

    let Some(password) = prompt_password_gui(session_dir.as_deref()) else {
        if let Some(dir) = &session_dir {
            mark_authentication_cancelled(dir);
        }
        return 1;
    };

    print_password(&password)
}

fn print_password(password: &str) -> i32 {
    let mut stdout = io::stdout().lock();
    if writeln!(stdout, "{password}")
        .and_then(|_| stdout.flush())
        .is_err()
    {
        return 1;
    }
    0
}

fn prompt_password_gui(session_dir: Option<&Path>) -> Option<String> {
    if gtk4::init().is_err() {
        eprintln!(
            "noos-package-manager: impossible d'ouvrir la fenêtre d'authentification (aucun affichage graphique disponible)."
        );
        return None;
    }

    let main_loop = glib::MainLoop::new(None, false);
    let result: std::rc::Rc<std::cell::RefCell<Option<String>>> =
        std::rc::Rc::new(std::cell::RefCell::new(None));

    let window = gtk4::Window::builder()
        .title("Authentification administrateur")
        .default_width(460)
        .resizable(false)
        .modal(true)
        .build();

    let container = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Vertical)
        .spacing(12)
        .margin_top(20)
        .margin_bottom(20)
        .margin_start(20)
        .margin_end(20)
        .build();

    let icon = gtk4::Image::from_icon_name("dialog-password-symbolic");
    icon.set_pixel_size(36);
    icon.set_halign(gtk4::Align::Start);
    container.append(&icon);

    let title = gtk4::Label::builder()
        .use_markup(true)
        .label("<b><span size=\"large\">Authentification administrateur</span></b>")
        .xalign(0.0)
        .build();
    container.append(&title);

    let description = gtk4::Label::builder()
        .label(
            "noos-package-manager va installer, mettre à jour ou supprimer des paquets \
             système. Une seule saisie de mot de passe suffit pour toute l'opération \
             (Pacman et AUR).",
        )
        .wrap(true)
        .xalign(0.0)
        .css_classes(["dim-label"])
        .build();
    container.append(&description);

    let user = env::var("USER").unwrap_or_else(|_| String::from("administrateur"));
    let user_label = gtk4::Label::builder()
        .label(format!("Mot de passe de « {user} » :"))
        .xalign(0.0)
        .build();
    container.append(&user_label);

    let entry = gtk4::PasswordEntry::builder()
        .show_peek_icon(true)
        .placeholder_text("Mot de passe")
        .activates_default(true)
        .hexpand(true)
        .build();
    container.append(&entry);

    let error_label = gtk4::Label::builder()
        .label("Saisissez votre mot de passe.")
        .xalign(0.0)
        .visible(false)
        .css_classes(["error"])
        .build();
    container.append(&error_label);

    let buttons = gtk4::Box::builder()
        .orientation(gtk4::Orientation::Horizontal)
        .spacing(6)
        .halign(gtk4::Align::End)
        .build();

    let cancel_button = gtk4::Button::with_label("Annuler");
    let unlock_button = gtk4::Button::with_label("Déverrouiller");
    unlock_button.add_css_class("suggested-action");

    buttons.append(&cancel_button);
    buttons.append(&unlock_button);
    container.append(&buttons);

    window.set_child(Some(&container));
    window.set_default_widget(Some(&unlock_button));

    let submit: std::rc::Rc<dyn Fn()> = {
        let result = std::rc::Rc::clone(&result);
        let window = window.clone();
        let entry = entry.clone();
        let error_label = error_label.clone();
        std::rc::Rc::new(move || {
            let password = entry.text().to_string();
            if password.is_empty() {
                error_label.set_visible(true);
                entry.grab_focus();
                return;
            }
            *result.borrow_mut() = Some(password);
            window.close();
        })
    };

    {
        let submit = std::rc::Rc::clone(&submit);
        entry.connect_activate(move |_| submit());
    }
    {
        let submit = std::rc::Rc::clone(&submit);
        unlock_button.connect_clicked(move |_| submit());
    }

    {
        let window_cancel = window.clone();
        cancel_button.connect_clicked(move |_| {
            window_cancel.close();
        });
    }

    {
        let main_loop = main_loop.clone();
        window.connect_close_request(move |_| {
            main_loop.quit();
            glib::Propagation::Proceed
        });
    }

    {
        let key_controller = gtk4::EventControllerKey::new();
        let window_key = window.clone();
        key_controller.connect_key_pressed(move |_, key, _, _| {
            if key == gtk4::gdk::Key::Escape {
                window_key.close();
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        window.add_controller(key_controller);
    }

    let entry_focus = entry.clone();
    glib::idle_add_local_once(move || {
        entry_focus.grab_focus();
    });

    // Ferme automatiquement la fenêtre si l'opération est annulée ou si le
    // répertoire de session a été supprimé par l'application principale.
    if let Some(session_dir) = session_dir {
        let session_dir = session_dir.to_path_buf();
        let main_loop_watch = main_loop.clone();
        glib::timeout_add_local(Duration::from_millis(500), move || {
            if session_dir.exists() {
                glib::ControlFlow::Continue
            } else {
                main_loop_watch.quit();
                glib::ControlFlow::Break
            }
        });
    }

    window.present();
    main_loop.run();

    let password = result.borrow_mut().take();
    password
}
