//! The Import/Export dialog (File tab). Typing dispatches `import.set`, the buttons `import.run`
//! and `import.cancel`; the dialog only mirrors `Session::import_dialog`.

use crate::MailApp;
use crate::theme::LIGHT as T;
use egui::{RichText, TextEdit};
use serde_json::json;

pub(crate) fn dialog(app: &mut MailApp, ctx: &egui::Context) {
    let Some(mut d) = app.session.import_dialog.clone() else { return };
    let before = d.path.clone();
    let mut action: Option<&str> = None;
    egui::Modal::new(egui::Id::new("import_dialog")).show(ctx, |ui| {
        ui.set_width(520.0);
        ui.label(RichText::new("Import from a file").size(18.0).color(T.text));
        ui.add_space(4.0);
        ui.label(
            RichText::new(
                "An Outlook data file (.pst) is copied into a new folder named after the file, with its folders inside. \
                 A single message (.msg or .eml) goes into the current folder. Calendar items, contacts and tasks are skipped.",
            )
            .size(12.0)
            .color(T.text_dim),
        );
        ui.add_space(10.0);
        ui.label("File");
        let field = ui.add(TextEdit::singleline(&mut d.path).hint_text("/home/you/Documents/archive.pst").desired_width(f32::INFINITY));
        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            action = Some("import.run");
        }
        if let Some(e) = &d.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(T.danger));
        }
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("Import").color(egui::Color32::WHITE)).fill(T.accent)).clicked() {
                action = Some("import.run");
            }
            if ui.button("Cancel").clicked() {
                action = Some("import.cancel");
            }
        });
    });
    if d.path != before {
        app.dispatch("import.set", json!({ "path": d.path }));
    }
    if let Some(id) = action {
        app.dispatch(id, json!({}));
    }
}
