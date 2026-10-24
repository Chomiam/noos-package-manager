pub mod backend;
pub mod error;
pub mod ui;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

const APP_ID: &str = "org.noos.PackageManager";

/// Si une ancienne instance de `noos-package-manager` tourne encore en arrière-plan avec un binaire
/// remplacé (`(deleted)`) ou sous restriction `NoNewPrivileges: 1` (ce qui bloque `pkexec`),
/// l'arrête proprement afin que la nouvelle instance prenne le relais sur D-Bus.
fn terminate_stale_or_sandboxed_instances() {
    let my_pid = std::process::id();
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return;
    };

    let mut killed_any = false;
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(pid_str) = fname.to_str() else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        if pid == my_pid {
            continue;
        }

        let proc_dir = entry.path();
        let Ok(comm) = std::fs::read_to_string(proc_dir.join("comm")) else {
            continue;
        };
        if !comm.trim().starts_with("noos-package-ma") {
            continue;
        }

        let exe_deleted = std::fs::read_link(proc_dir.join("exe"))
            .map(|p| p.to_string_lossy().contains("(deleted)"))
            .unwrap_or(false);

        let has_nnp = std::fs::read_to_string(proc_dir.join("status"))
            .map(|st| {
                st.lines().any(|line| {
                    line.strip_prefix("NoNewPrivileges:")
                        .is_some_and(|v| v.trim() != "0")
                })
            })
            .unwrap_or(false);

        if exe_deleted || has_nnp {
            let _ = std::process::Command::new("kill")
                .args(["-TERM", &pid.to_string()])
                .status();
            killed_any = true;
        }
    }

    if killed_any {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

fn main() -> glib::ExitCode {
    // Mode interne utilisé par `sudo -A` : le binaire se comporte alors comme un
    // assistant graphique askpass et ne doit surtout pas démarrer l'application.
    if std::env::var_os(crate::backend::elevation::ASKPASS_MODE_ENV).is_some() {
        ui::askpass::run_askpass();
    }

    terminate_stale_or_sandboxed_instances();
    glib::set_application_name("Gestionnaire de paquets");

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .build();

    app.connect_startup(|_| {
        ui::style::load_css();
        ui::icons::init_icon_search_paths();
    });

    app.connect_activate(|app| {
        let window = ui::window::build_main_window(app);
        window.present();
    });

    app.run()
}
