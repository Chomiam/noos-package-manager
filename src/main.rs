pub mod backend;
pub mod error;
pub mod ui;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;

const APP_ID: &str = "org.noos.PackageManager";

fn main() -> glib::ExitCode {
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
