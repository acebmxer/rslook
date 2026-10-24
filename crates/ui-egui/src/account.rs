//! The Add Account / Account Settings dialog. Every edit and button dispatches an `account.*`
//! command; the dialog only mirrors `Session::account_dialog`.

use crate::MailApp;
use crate::theme::LIGHT as T;
use egui::{RichText, TextEdit, Ui};
use rslook_engine::account::Security;
use serde_json::json;

const FIELD_W: f32 = 300.0;

fn row(ui: &mut Ui, label: &str, field: &mut String, hint: &str, password: bool) {
    ui.label(label);
    ui.add(TextEdit::singleline(field).hint_text(hint).password(password).desired_width(FIELD_W));
    ui.end_row();
}

fn server(ui: &mut Ui, id: &str, host: &mut String, port: &mut String, security: &mut Security, host_hint: &str) {
    ui.label("Server");
    ui.add(TextEdit::singleline(host).hint_text(host_hint).desired_width(FIELD_W));
    ui.end_row();
    ui.label("Port");
    ui.horizontal(|ui| {
        ui.add(TextEdit::singleline(port).desired_width(60.0));
        ui.add_space(12.0);
        ui.label("Encryption");
        egui::ComboBox::from_id_salt(id).selected_text(security.label()).show_ui(ui, |ui| {
            for s in [Security::Tls, Security::StartTls, Security::None] {
                ui.selectable_value(security, s, s.label());
            }
        });
    });
    ui.end_row();
}

pub(crate) fn dialog(app: &mut MailApp, ctx: &egui::Context) {
    let Some(mut d) = app.session.account_dialog.clone() else {
        app.confirm_remove = false;
        return;
    };
    let before = d.clone();
    let title = if d.editing { "Account Settings" } else { "Add Account" };
    let queued = app.session.queue().len();
    // Buttons act after the field edits of the same frame are sent.
    let mut action: Option<&str> = None;
    egui::Modal::new(egui::Id::new("account_dialog")).show(ctx, |ui| {
        ui.set_width(460.0);
        ui.label(RichText::new(title).size(18.0).color(T.text));
        ui.add_space(4.0);
        ui.label(RichText::new("IMAP and SMTP mail account. The password is kept in your system keychain, never in a file.").size(12.0).color(T.text_dim));
        ui.add_space(10.0);
        egui::Grid::new("account_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            row(ui, "Your name", &mut d.name, "Shown to people you write to", false);
            row(ui, "Email address", &mut d.email, "name@example.com", false);
            row(ui, "User name", &mut d.username, "Usually your email address", false);
            let hint = if d.editing { "Leave blank to keep the saved password" } else { "" };
            row(ui, "Password", &mut d.password, hint, true);
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Incoming mail (IMAP)").strong());
        egui::Grid::new("imap_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            server(ui, "imap_security", &mut d.imap_host, &mut d.imap_port, &mut d.imap_security, "imap.example.com");
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Outgoing mail (SMTP)").strong());
        egui::Grid::new("smtp_fields").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            server(ui, "smtp_security", &mut d.smtp_host, &mut d.smtp_port, &mut d.smtp_security, "smtp.example.com");
        });
        if d.imap_security == Security::None || d.smtp_security == Security::None {
            ui.add_space(4.0);
            ui.label(RichText::new("Without encryption your password crosses the network unprotected.").size(12.0).color(T.danger));
        }
        if let Some(e) = &d.error {
            ui.add_space(6.0);
            ui.label(RichText::new(e).color(T.danger));
        }
        ui.add_space(12.0);
        if app.confirm_remove {
            let lost = if queued > 0 { format!(" {queued} change(s) not yet made on the server will be lost.") } else { String::new() };
            ui.label(
                RichText::new(format!("Remove this account? Rslook forgets the password and deletes the copy of the mailbox on this computer. Mail on the server is not touched.{lost}"))
                    .color(T.text),
            );
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Remove").clicked() {
                    app.confirm_remove = false;
                    action = Some("account.remove");
                }
                if ui.button("Keep").clicked() {
                    app.confirm_remove = false;
                }
            });
            return;
        }
        ui.horizontal(|ui| {
            if ui.add(egui::Button::new(RichText::new("Save").color(egui::Color32::WHITE)).fill(T.accent)).clicked() {
                action = Some("account.save");
            }
            if ui.button("Cancel").clicked() {
                action = Some("account.cancel");
            }
            if d.editing {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Remove Account…").clicked() {
                        app.confirm_remove = true;
                    }
                });
            }
        });
    });
    if d != before {
        app.dispatch(
            "account.set",
            json!({
                "name": d.name, "email": d.email, "username": d.username, "password": d.password,
                "imap_host": d.imap_host, "imap_port": d.imap_port, "imap_security": d.imap_security.id(),
                "smtp_host": d.smtp_host, "smtp_port": d.smtp_port, "smtp_security": d.smtp_security.id(),
            }),
        );
    }
    if let Some(id) = action {
        app.dispatch(id, json!({}));
    }
}
