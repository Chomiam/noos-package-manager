use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::backend::yay_interaction::{submit_prompt_answer, YayPromptAnswer, YayPromptKind};

/// Affiche une fenêtre de choix pour un prompt Yay/makepkg.
pub fn show_yay_prompt(parent: &impl IsA<gtk4::Window>, request_id: u64, prompt: YayPromptKind) {
    match prompt {
        YayPromptKind::Choice {
            title,
            options,
            answers,
            default,
        } => show_choice(parent, request_id, title, options, answers, default),
        YayPromptKind::Confirm { title, default } => {
            show_confirm(parent, request_id, title, default)
        }
        YayPromptKind::Text { title, default } => show_text(parent, request_id, title, default),
    }
}

/// Retire le préfixe décoratif (`:: `) des lignes de sortie de Yay.
fn clean_title(title: &str) -> String {
    title.trim().trim_start_matches(':').trim().to_string()
}

fn show_choice(
    parent: &impl IsA<gtk4::Window>,
    request_id: u64,
    title: String,
    options: Vec<String>,
    answers: Vec<String>,
    default: usize,
) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading("Choix de compilation AUR")
        .body(clean_title(&title))
        .build();

    let container = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    container.set_size_request(420, -1);
    let labels: Vec<&str> = options.iter().map(String::as_str).collect();
    let dropdown = gtk4::DropDown::from_strings(&labels);
    dropdown.set_selected(default.min(options.len().saturating_sub(1)) as u32);
    dropdown.set_hexpand(true);
    dropdown.set_enable_search(options.len() > 10);
    container.append(&dropdown);
    dialog.set_extra_child(Some(&container));

    dialog.add_response("cancel", "Annuler");
    dialog.add_response("ok", "Valider");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);

    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response == "ok" {
            let selected = dropdown.selected() as usize;
            let value = answers
                .get(selected)
                .cloned()
                .unwrap_or_else(|| (selected + 1).to_string());
            submit_prompt_answer(YayPromptAnswer {
                request_id,
                cancelled: false,
                value: Some(value),
            });
        } else {
            submit_prompt_answer(YayPromptAnswer {
                request_id,
                cancelled: true,
                value: None,
            });
        }
    });
    dialog.present();
}

fn show_confirm(parent: &impl IsA<gtk4::Window>, request_id: u64, title: String, default: bool) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading("Choix de compilation AUR")
        .body(clean_title(&title))
        .build();

    dialog.add_response("no", "Non");
    dialog.add_response("yes", "Oui");
    dialog.set_default_response(Some(if default { "yes" } else { "no" }));
    dialog.set_close_response("no");
    dialog.set_response_appearance("yes", adw::ResponseAppearance::Suggested);

    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        submit_prompt_answer(YayPromptAnswer {
            request_id,
            cancelled: false,
            value: Some(if response == "yes" {
                String::from("y")
            } else {
                String::from("n")
            }),
        });
    });
    dialog.present();
}

fn show_text(parent: &impl IsA<gtk4::Window>, request_id: u64, title: String, default: String) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading("Choix de compilation AUR")
        .body(clean_title(&title))
        .build();

    let entry = gtk4::Entry::builder()
        .text(&default)
        .hexpand(true)
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&entry));

    dialog.add_response("cancel", "Annuler");
    dialog.add_response("ok", "Valider");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);

    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response == "ok" {
            submit_prompt_answer(YayPromptAnswer {
                request_id,
                cancelled: false,
                value: Some(entry.text().to_string()),
            });
        } else {
            submit_prompt_answer(YayPromptAnswer {
                request_id,
                cancelled: true,
                value: None,
            });
        }
    });
    dialog.present();
}
