use std::cell::Cell;
use std::rc::Rc;

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
        } => show_selection(
            parent,
            request_id,
            "Options de compilation",
            clean_title(&title),
            options,
            answers,
            default,
        ),
        YayPromptKind::Confirm { title, default } => show_selection(
            parent,
            request_id,
            "Question de compilation",
            clean_title(&title),
            vec![String::from("Oui"), String::from("Non")],
            vec![String::from("y"), String::from("n")],
            if default { 0 } else { 1 },
        ),
        YayPromptKind::Text { title, default } => show_text(parent, request_id, title, default),
    }
}

/// Retire le préfixe décoratif (`:: `) des lignes de sortie de Yay.
fn clean_title(title: &str) -> String {
    title.trim().trim_start_matches(':').trim().to_string()
}

/// Fenêtre commune aux menus de fournisseurs et aux questions fermées.
///
/// Les options sont présentées sous forme de cases à cocher groupées : une
/// seule case peut être cochée à la fois.
fn show_selection(
    parent: &impl IsA<gtk4::Window>,
    request_id: u64,
    heading: &str,
    body: String,
    options: Vec<String>,
    answers: Vec<String>,
    default: usize,
) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading(heading)
        .body(format!("{body}\n\nChoisissez une seule option."))
        .build();

    let selected = Rc::new(Cell::new(default.min(options.len().saturating_sub(1))));
    let list = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    list.set_margin_top(6);
    list.set_margin_bottom(6);
    list.set_margin_start(6);
    list.set_margin_end(6);

    let mut group: Option<gtk4::CheckButton> = None;
    for (index, label) in options.iter().enumerate() {
        let check = gtk4::CheckButton::with_label(label);
        check.set_active(index == selected.get());
        if let Some(first) = &group {
            check.set_group(Some(first));
        } else {
            group = Some(check.clone());
        }
        let selected = Rc::clone(&selected);
        check.connect_toggled(move |button| {
            if button.is_active() {
                selected.set(index);
            }
        });
        list.append(&check);
    }

    let scroller = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vscrollbar_policy(gtk4::PolicyType::Automatic)
        .propagate_natural_height(true)
        .max_content_height(320)
        .kinetic_scrolling(true)
        .child(&list)
        .build();
    scroller.set_size_request(420, -1);
    scroller.add_css_class("prompt-option-list");
    dialog.set_extra_child(Some(&scroller));

    dialog.add_response("cancel", "Annuler");
    dialog.add_response("ok", "Valider");
    dialog.set_default_response(Some("ok"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("ok", adw::ResponseAppearance::Suggested);

    dialog.connect_response(None, move |dlg, response| {
        dlg.close();
        if response == "ok" {
            let index = selected.get();
            let value = answers
                .get(index)
                .cloned()
                .unwrap_or_else(|| (index + 1).to_string());
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

fn show_text(parent: &impl IsA<gtk4::Window>, request_id: u64, title: String, default: String) {
    let dialog = adw::MessageDialog::builder()
        .transient_for(parent)
        .modal(true)
        .heading("Question de compilation")
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
