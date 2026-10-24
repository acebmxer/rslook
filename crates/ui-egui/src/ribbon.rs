//! The ribbon: tab strip and the selected tab's groups of buttons.

use crate::icons::{self, Icon};
use crate::theme::LIGHT as T;
use crate::{MailApp, RibbonTab};
use egui::{Align2, CornerRadius, FontId, Rect, Sense, Stroke, Ui, pos2, vec2};
use rslook_engine::ReadingPane;
use serde_json::{Value, json};

const TABS: &[(RibbonTab, &str)] = &[
    (RibbonTab::File, "File"),
    (RibbonTab::Home, "Home"),
    (RibbonTab::SendReceive, "Send / Receive"),
    (RibbonTab::Folder, "Folder"),
    (RibbonTab::View, "View"),
    (RibbonTab::Help, "Help"),
];

pub(crate) fn show(app: &mut MailApp, ui: &mut Ui) {
    egui::Panel::top("ribbon_tabs").exact_size(30.0).show_separator_line(false).frame(egui::Frame::NONE.fill(T.tabs)).show(ui, |ui| {
        ui.horizontal_centered(|ui| {
            ui.add_space(4.0);
            for &(tab, label) in TABS {
                tab_button(app, ui, tab, label);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(10.0);
                let search = ui.add(
                    egui::TextEdit::singleline(&mut app.search_text)
                        .hint_text("Search Current Mailbox")
                        .desired_width(260.0)
                        .id(egui::Id::new("search_box")),
                );
                if std::mem::take(&mut app.focus_search) {
                    search.request_focus();
                }
                if search.changed() {
                    let text = app.search_text.clone();
                    app.dispatch("search.set", json!({ "text": text }));
                }
            });
        });
    });
    egui::Panel::top("ribbon_body")
        .exact_size(94.0)
        .frame(egui::Frame::NONE.fill(T.ribbon).stroke(Stroke::new(1.0, T.border)).inner_margin(egui::Margin { left: 6, right: 6, top: 4, bottom: 2 }))
        .show(ui, |ui| {
            ui.horizontal(|ui| match app.tab {
                RibbonTab::Home | RibbonTab::File => home(app, ui),
                RibbonTab::View => view(app, ui),
                RibbonTab::SendReceive => send_receive(app, ui),
                RibbonTab::Folder => folder(app, ui),
                RibbonTab::Help => {
                    let (done, total) = rslook_engine::catalog::parity();
                    ui.label(format!("Rslook {} — clean-room Outlook classic parity: {done}/{total} features", env!("CARGO_PKG_VERSION")));
                }
            });
        });
}

fn tab_button(app: &mut MailApp, ui: &mut Ui, tab: RibbonTab, label: &str) {
    let font = FontId::proportional(13.0);
    let galley = ui.painter().layout_no_wrap(label.to_string(), font.clone(), T.text);
    let size = vec2(galley.size().x + 22.0, 28.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let p = ui.painter();
    if tab == RibbonTab::File {
        p.rect_filled(rect.shrink2(vec2(1.0, 3.0)), 2.0, if resp.hovered() { T.accent_dark } else { T.accent });
        p.text(rect.center(), Align2::CENTER_CENTER, label, font, egui::Color32::WHITE);
    } else {
        let active = app.tab == tab;
        if resp.hovered() && !active {
            p.rect_filled(rect.shrink2(vec2(1.0, 3.0)), 2.0, T.hover);
        }
        p.text(rect.center(), Align2::CENTER_CENTER, label, font, if active { T.accent } else { T.text });
        if active {
            let y = rect.bottom() - 3.0;
            p.line_segment([pos2(rect.left() + 8.0, y), pos2(rect.right() - 8.0, y)], Stroke::new(2.5, T.accent));
        }
    }
    if resp.clicked() {
        app.tab = tab;
    }
}

/// A ribbon group: contents then the group name underneath, with a separator on the right.
fn group(ui: &mut Ui, name: &str, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.set_height(86.0);
        ui.horizontal(|ui| {
            ui.set_height(68.0);
            add(ui);
        });
        ui.add_space(2.0);
        let w = ui.min_rect().width();
        let (r, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());
        ui.painter().text(r.center(), Align2::CENTER_CENTER, name, FontId::proportional(11.0), T.text_dim);
    });
    let (sep, _) = ui.allocate_exact_size(vec2(9.0, 84.0), Sense::hover());
    ui.painter().line_segment([pos2(sep.center().x, sep.top() + 4.0), pos2(sep.center().x, sep.bottom() - 4.0)], Stroke::new(1.0, T.border));
}

/// A large button (icon over label).
fn big(app: &mut MailApp, ui: &mut Ui, icon: Icon, label: &str, cmd: &str, params: Value) {
    let enabled = app.enabled(cmd);
    let w = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(12.0), T.text).size().x.max(36.0) + 14.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 68.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(3), T.hover);
    }
    let color = if enabled { T.accent } else { T.border };
    let ir = Rect::from_center_size(pos2(rect.center().x, rect.top() + 22.0), vec2(32.0, 32.0));
    icons::paint(p, ir, icon, color);
    p.text(pos2(rect.center().x, rect.top() + 50.0), Align2::CENTER_TOP, label, FontId::proportional(12.0), if enabled { T.text } else { T.text_dim });
    let resp = resp.on_hover_text(shortcut_tip(label, cmd));
    if resp.clicked() {
        app.dispatch(cmd, params);
    }
}

/// A small button (icon left of label), stacked three to a column.
fn small(app: &mut MailApp, ui: &mut Ui, icon: Icon, label: &str, cmd: &str, params: Value) {
    let enabled = app.enabled(cmd);
    let w = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(12.0), T.text).size().x + 34.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, 22.0), if enabled { Sense::click() } else { Sense::hover() });
    let p = ui.painter();
    if enabled && resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(3), T.hover);
    }
    let color = if enabled { T.accent } else { T.border };
    icons::paint(p, Rect::from_center_size(pos2(rect.left() + 13.0, rect.center().y), vec2(16.0, 16.0)), icon, color);
    p.text(pos2(rect.left() + 26.0, rect.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.0), if enabled { T.text } else { T.text_dim });
    let resp = resp.on_hover_text(shortcut_tip(label, cmd));
    if resp.clicked() {
        app.dispatch(cmd, params);
    }
}

fn shortcut_tip(label: &str, cmd: &str) -> String {
    match rslook_engine::find(cmd).and_then(|c| c.shortcut) {
        Some(k) => format!("{label} ({k})"),
        None => label.to_string(),
    }
}

fn column(ui: &mut Ui, add: impl FnOnce(&mut Ui)) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        add(ui);
    });
}

fn home(app: &mut MailApp, ui: &mut Ui) {
    group(ui, "New", |ui| big(app, ui, Icon::NewMail, "New Email", "mail.new", json!({})));
    group(ui, "Delete", |ui| {
        column(ui, |ui| {
            small(app, ui, Icon::Junk, "Junk", "mail.junk", json!({}));
        });
        big(app, ui, Icon::Delete, "Delete", "mail.delete", json!({}));
        big(app, ui, Icon::Archive, "Archive", "mail.archive", json!({}));
    });
    group(ui, "Respond", |ui| {
        big(app, ui, Icon::Reply, "Reply", "mail.reply", json!({}));
        big(app, ui, Icon::ReplyAll, "Reply All", "mail.reply_all", json!({}));
        big(app, ui, Icon::Forward, "Forward", "mail.forward", json!({}));
    });
    group(ui, "Move", |ui| big(app, ui, Icon::Move, "Move to Inbox", "mail.move", json!({"kind": "inbox"})));
    group(ui, "Tags", |ui| {
        column(ui, |ui| {
            let unread = app.session.selected.and_then(|id| app.session.store.message(id)).is_some_and(|m| !m.read);
            if unread {
                small(app, ui, Icon::Unread, "Mark Read", "mail.mark_read", json!({}));
            } else {
                small(app, ui, Icon::Unread, "Mark Unread", "mail.mark_unread", json!({}));
            }
            small(app, ui, Icon::Flag, "Follow Up", "mail.toggle_flag", json!({}));
        });
    });
    group(ui, "Undo", |ui| big(app, ui, Icon::Undo, "Undo", "edit.undo", json!({})));
}

fn view(app: &mut MailApp, ui: &mut Ui) {
    group(ui, "Layout", |ui| {
        big(app, ui, Icon::Mail, "Folder Pane", "view.folder_pane", json!({}));
        column(ui, |ui| {
            let cur = app.session.reading_pane;
            for (pos, label, v) in [(ReadingPane::Right, "Right", "right"), (ReadingPane::Bottom, "Bottom", "bottom"), (ReadingPane::Off, "Off", "off")] {
                let mark = if cur == pos { "● " } else { "   " };
                small(app, ui, Icon::Mail, &format!("{mark}Reading Pane: {label}"), "view.reading_pane", json!({"position": v}));
            }
        });
    });
}

fn send_receive(app: &mut MailApp, ui: &mut Ui) {
    group(ui, "Send & Receive", |ui| {
        ui.label("Accounts and sync arrive with the IMAP/SMTP milestone.");
        let _ = app;
    });
}

fn folder(app: &mut MailApp, ui: &mut Ui) {
    group(ui, "Import", |ui| {
        ui.label("Import a message with the mail.import_eml command (File ▸ Open & Export coming).");
        let _ = app;
    });
}
