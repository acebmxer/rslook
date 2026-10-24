//! Folder pane, message list, reading pane, compose form, navigation bar and status bar.

use crate::MailApp;
use crate::icons::{self, Icon};
use crate::theme::LIGHT as T;
use chrono::{DateTime, Datelike, Local, Utc};
use egui::{Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, Stroke, Ui, pos2, vec2};
use rslook_engine::mail::{Folder, FolderKind, Importance, Message};
use rslook_engine::{Module, ReadingPane};
use serde_json::json;

pub(crate) fn status_bar(app: &mut MailApp, ui: &mut Ui) {
    egui::Panel::bottom("status")
        .exact_size(24.0)
        .show_separator_line(false)
        .frame(egui::Frame::NONE.fill(T.status).inner_margin(egui::Margin { left: 10, right: 10, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let s = &app.session;
                let font = FontId::proportional(12.0);
                ui.label(RichText::new(format!("Items: {}", s.store.total_count(s.folder))).font(font.clone()).color(Color32::WHITE));
                ui.add_space(14.0);
                let unread = s.store.unread_count(s.folder);
                if unread > 0 {
                    ui.label(RichText::new(format!("Unread: {unread}")).font(font.clone()).color(Color32::WHITE));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (text, color) = match &app.error {
                        Some(e) => (e.clone(), Color32::from_rgb(0xff, 0xe0, 0x8a)),
                        None if !s.status.is_empty() => (s.status.clone(), Color32::WHITE),
                        None => ("Working offline: local mailbox".to_string(), Color32::WHITE),
                    };
                    ui.label(RichText::new(text).font(font).color(color));
                });
            });
        });
}

pub(crate) fn nav_bar(app: &mut MailApp, ui: &mut Ui) {
    egui::Panel::bottom("nav_bar")
        .exact_size(36.0)
        .frame(egui::Frame::NONE.fill(T.pane).stroke(Stroke::new(1.0, T.border)).inner_margin(egui::Margin { left: 8, right: 8, top: 0, bottom: 0 }))
        .show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                for (m, icon, cmd, tip) in [
                    (Module::Mail, Icon::Mail, "module.mail", "Mail (Ctrl+1)"),
                    (Module::Calendar, Icon::Calendar, "module.calendar", "Calendar (Ctrl+2)"),
                    (Module::People, Icon::People, "module.people", "People (Ctrl+3)"),
                    (Module::Tasks, Icon::Tasks, "module.tasks", "Tasks (Ctrl+4)"),
                ] {
                    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 30.0), Sense::click());
                    let active = app.session.module == m;
                    if resp.hovered() {
                        ui.painter().rect_filled(rect, 3.0, T.hover);
                    }
                    let color = if active { T.accent } else { T.text_dim };
                    icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(20.0, 20.0)), icon, color);
                    if active {
                        ui.painter().line_segment(
                            [pos2(rect.left() + 8.0, rect.bottom()), pos2(rect.right() - 8.0, rect.bottom())],
                            Stroke::new(2.5, T.accent),
                        );
                    }
                    if resp.on_hover_text(tip).clicked() {
                        app.dispatch(cmd, json!({}));
                    }
                }
            });
        });
}

pub(crate) fn main_area(app: &mut MailApp, ui: &mut Ui) {
    if app.session.module != Module::Mail {
        let name = match app.session.module {
            Module::Calendar => "Calendar",
            Module::People => "People",
            Module::Tasks => "Tasks",
            Module::Mail => "Mail",
        };
        egui::CentralPanel::default().frame(egui::Frame::NONE.fill(T.window)).show(ui, |ui| {
            ui.centered_and_justified(|ui| {
                ui.label(RichText::new(format!("{name} is on the roadmap (iCalendar / vCard milestones).")).size(16.0).color(T.text_dim));
            });
        });
        return;
    }
    if app.session.folder_pane {
        egui::Panel::left("folder_pane")
            .default_size(220.0)
            .size_range(150.0..=400.0)
            .resizable(true)
            .frame(egui::Frame::NONE.fill(T.pane).inner_margin(egui::Margin { left: 0, right: 0, top: 6, bottom: 6 }))
            .show(ui, |ui| folder_pane(app, ui));
    }
    let list_frame = egui::Frame::NONE.fill(T.window).stroke(Stroke::new(1.0, T.border));
    match app.session.reading_pane {
        ReadingPane::Right => {
            egui::Panel::left("message_list")
                .default_size(380.0)
                .size_range(260.0..=700.0)
                .resizable(true)
                .frame(list_frame)
                .show(ui, |ui| message_list(app, ui));
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(T.window)).show(ui, |ui| reading_area(app, ui));
        }
        ReadingPane::Bottom => {
            egui::Panel::top("message_list")
                .default_size(300.0)
                .size_range(120.0..=700.0)
                .resizable(true)
                .frame(list_frame)
                .show(ui, |ui| message_list(app, ui));
            egui::CentralPanel::default().frame(egui::Frame::NONE.fill(T.window)).show(ui, |ui| reading_area(app, ui));
        }
        ReadingPane::Off => {
            if app.session.compose.is_some() {
                egui::CentralPanel::default().frame(egui::Frame::NONE.fill(T.window)).show(ui, |ui| reading_area(app, ui));
            } else {
                egui::CentralPanel::default().frame(list_frame).show(ui, |ui| message_list(app, ui));
            }
        }
    }
}

// ----- Folder pane -----

fn folder_row(app: &mut MailApp, ui: &mut Ui, f: &Folder, depth: usize) {
    let s = &app.session;
    let selected = s.folder == f.id;
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 24.0), Sense::click());
    let p = ui.painter();
    if selected {
        p.rect_filled(rect, 0.0, T.selected);
    } else if resp.hovered() {
        p.rect_filled(rect, 0.0, T.hover);
    }
    let unread = s.store.unread_count(f.id);
    let x = rect.left() + 18.0 + depth as f32 * 14.0;
    let bold = unread > 0 && f.kind != FolderKind::Drafts;
    let font = FontId::proportional(13.0);
    let name_color = T.text;
    let g = p.layout_no_wrap(f.name.clone(), font.clone(), name_color);
    let name_w = g.size().x;
    p.galley(pos2(x, rect.center().y - g.size().y / 2.0), g, name_color);
    if bold {
        // Faux-bold for the default font: draw the name a second time half a point over.
        p.text(pos2(x + 0.5, rect.center().y), Align2::LEFT_CENTER, &f.name, font.clone(), name_color);
    }
    let badge = match f.kind {
        // Outlook shows the item count in brackets for Drafts and Outbox.
        FolderKind::Drafts | FolderKind::Outbox => {
            let n = s.store.total_count(f.id);
            (n > 0).then(|| format!("[{n}]"))
        }
        _ => (unread > 0).then(|| unread.to_string()),
    };
    if let Some(b) = badge {
        p.text(pos2(x + name_w + 8.0, rect.center().y), Align2::LEFT_CENTER, b, font, T.accent);
    }
    if resp.clicked() {
        app.dispatch("folder.select", json!({ "folder": f.id }));
    }
}

fn folder_pane(app: &mut MailApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    let folders: Vec<Folder> = app.session.store.folders().to_vec();
    let header = |ui: &mut Ui, text: &str| {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
        ui.painter().text(pos2(r.left() + 8.0, r.center().y), Align2::LEFT_CENTER, format!("⏷ {text}"), FontId::proportional(13.0), T.text);
    };
    egui::ScrollArea::vertical().show(ui, |ui| {
        header(ui, "Favorites");
        for kind in [FolderKind::Inbox, FolderKind::Sent, FolderKind::Deleted] {
            if let Some(f) = folders.iter().find(|f| f.kind == kind) {
                folder_row(app, ui, f, 0);
            }
        }
        ui.add_space(10.0);
        let account = app.session.store.account.email.clone();
        header(ui, &account);
        fn walk(app: &mut MailApp, ui: &mut Ui, all: &[Folder], parent: Option<u64>, depth: usize) {
            if depth > 16 {
                return;
            }
            for f in all.iter().filter(|f| f.parent == parent) {
                folder_row(app, ui, f, depth);
                walk(app, ui, all, Some(f.id), depth + 1);
            }
        }
        walk(app, ui, &folders, None, 0);
    });
}

// ----- Message list -----

fn day_group(date: DateTime<Utc>, now: DateTime<Local>) -> &'static str {
    let d = date.with_timezone(&Local).date_naive();
    let today = now.date_naive();
    let age = (today - d).num_days();
    if age <= 0 {
        "Today"
    } else if age == 1 {
        "Yesterday"
    } else if age < i64::from(today.weekday().num_days_from_sunday()) + 1 {
        "Earlier this week"
    } else if age < i64::from(today.weekday().num_days_from_sunday()) + 8 {
        "Last week"
    } else {
        "Older"
    }
}

fn list_date(date: DateTime<Utc>, now: DateTime<Local>) -> String {
    let local = date.with_timezone(&Local);
    let age = (now.date_naive() - local.date_naive()).num_days();
    if age <= 0 {
        local.format("%-I:%M %p").to_string()
    } else if age < 7 {
        local.format("%a %-I:%M %p").to_string()
    } else {
        local.format("%-m/%-d/%Y").to_string()
    }
}

fn message_list(app: &mut MailApp, ui: &mut Ui) {
    ui.spacing_mut().item_spacing.y = 0.0;
    let (hdr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
    let p = ui.painter();
    p.text(pos2(hdr.left() + 12.0, hdr.center().y), Align2::LEFT_CENTER, "All", FontId::proportional(13.0), T.accent);
    p.line_segment([pos2(hdr.left() + 10.0, hdr.bottom() - 5.0), pos2(hdr.left() + 30.0, hdr.bottom() - 5.0)], Stroke::new(2.0, T.accent));
    p.text(pos2(hdr.left() + 46.0, hdr.center().y), Align2::LEFT_CENTER, "Unread", FontId::proportional(13.0), T.text_dim);
    p.text(pos2(hdr.right() - 24.0, hdr.center().y), Align2::RIGHT_CENTER, "By Date", FontId::proportional(12.0), T.text_dim);
    // Newest-first sort marker: a small down arrow drawn in code (the default font lacks ↓).
    let ax = hdr.right() - 15.0;
    let ay = hdr.center().y;
    let st = Stroke::new(1.2, T.text_dim);
    p.line_segment([pos2(ax, ay - 5.0), pos2(ax, ay + 5.0)], st);
    p.line_segment([pos2(ax - 3.5, ay + 1.5), pos2(ax, ay + 5.0)], st);
    p.line_segment([pos2(ax + 3.5, ay + 1.5), pos2(ax, ay + 5.0)], st);
    p.line_segment([hdr.left_bottom(), hdr.right_bottom()], Stroke::new(1.0, T.border));

    let now = Local::now();
    let messages: Vec<Message> = app.session.store.messages_in(app.session.folder, &app.session.search).into_iter().cloned().collect();
    if messages.is_empty() {
        ui.add_space(30.0);
        let text = if app.session.search.is_empty() { "We didn't find anything to show here." } else { "No results." };
        ui.vertical_centered(|ui| ui.label(RichText::new(text).color(T.text_dim)));
        return;
    }
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut last_group = "";
        for m in &messages {
            let g = day_group(m.date, now);
            if g != last_group {
                last_group = g;
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 26.0), Sense::hover());
                ui.painter().text(pos2(r.left() + 10.0, r.center().y), Align2::LEFT_CENTER, format!("⏷ {g}"), FontId::proportional(12.0), T.text);
            }
            message_row(app, ui, m, now);
        }
    });
}

fn message_row(app: &mut MailApp, ui: &mut Ui, m: &Message, now: DateTime<Local>) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(vec2(width, 62.0), Sense::click());
    let selected = app.session.selected == Some(m.id);
    let painter = ui.painter_at(rect);
    if selected {
        painter.rect_filled(rect, 0.0, T.selected);
    } else if resp.hovered() {
        painter.rect_filled(rect, 0.0, T.hover);
    }
    if !m.read {
        painter.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), 0.0, T.accent);
    }
    let left = rect.left() + 14.0;
    let right = rect.right() - 10.0;
    let sender = if m.from.email == app.session.store.account.email {
        m.to.first().map(|a| a.display().to_string()).unwrap_or_default()
    } else {
        m.from.display().to_string()
    };
    let f13 = FontId::proportional(14.0);
    let f12 = FontId::proportional(12.5);
    painter.text(pos2(left, rect.top() + 13.0), Align2::LEFT_CENTER, &sender, f13.clone(), T.text);
    if !m.read {
        painter.text(pos2(left + 0.5, rect.top() + 13.0), Align2::LEFT_CENTER, &sender, f13, T.text);
    }
    // Right-side markers on the first line.
    let mut x = right;
    if m.flagged {
        icons::paint(&painter, Rect::from_center_size(pos2(x - 7.0, rect.top() + 13.0), vec2(14.0, 14.0)), Icon::Flag, T.flag);
        x -= 20.0;
    }
    if !m.attachments.is_empty() {
        icons::paint(&painter, Rect::from_center_size(pos2(x - 7.0, rect.top() + 13.0), vec2(14.0, 14.0)), Icon::Attach, T.text_dim);
        x -= 20.0;
    }
    if m.importance == Importance::High {
        painter.text(pos2(x - 4.0, rect.top() + 13.0), Align2::RIGHT_CENTER, "!", FontId::proportional(15.0), T.danger);
    }
    let date = list_date(m.date, now);
    let subject_color = if m.read { T.text } else { T.accent };
    painter.text(pos2(right, rect.top() + 32.0), Align2::RIGHT_CENTER, &date, f12.clone(), subject_color);
    let date_w = painter.layout_no_wrap(date, f12.clone(), T.text).size().x;
    let sub_clip = painter.with_clip_rect(Rect::from_min_max(pos2(left, rect.top()), pos2(right - date_w - 8.0, rect.bottom())));
    sub_clip.text(pos2(left, rect.top() + 32.0), Align2::LEFT_CENTER, &m.subject, f12.clone(), subject_color);
    let prev_clip = painter.with_clip_rect(Rect::from_min_max(pos2(left, rect.top()), pos2(right, rect.bottom())));
    prev_clip.text(pos2(left, rect.top() + 49.0), Align2::LEFT_CENTER, m.preview(160), f12, T.text_dim);
    painter.line_segment([pos2(rect.left() + 10.0, rect.bottom() - 0.5), pos2(rect.right(), rect.bottom() - 0.5)], Stroke::new(1.0, T.border));
    if resp.clicked() {
        app.dispatch("message.select", json!({ "id": m.id }));
    }
}

// ----- Reading pane -----

fn reading_area(app: &mut MailApp, ui: &mut Ui) {
    if app.session.compose.is_some() {
        compose(app, ui);
        return;
    }
    let Some(m) = app.session.selected.and_then(|id| app.session.store.message(id)).cloned() else {
        ui.centered_and_justified(|ui| {
            ui.label(RichText::new("Select an item to read").size(15.0).color(T.text_dim));
        });
        return;
    };
    egui::Frame::NONE.inner_margin(egui::Margin { left: 20, right: 20, top: 14, bottom: 10 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            for (icon, label, cmd) in
                [(Icon::Reply, "Reply", "mail.reply"), (Icon::ReplyAll, "Reply All", "mail.reply_all"), (Icon::Forward, "Forward", "mail.forward")]
            {
                let w = ui.painter().layout_no_wrap(label.to_string(), FontId::proportional(12.5), T.text).size().x + 34.0;
                let (r, resp) = ui.allocate_exact_size(vec2(w, 26.0), Sense::click());
                let fill = if resp.hovered() { T.hover } else { T.window };
                ui.painter().rect(r, CornerRadius::same(3), fill, Stroke::new(1.0, T.border), egui::StrokeKind::Inside);
                icons::paint(ui.painter(), Rect::from_center_size(pos2(r.left() + 14.0, r.center().y), vec2(16.0, 16.0)), icon, T.accent);
                ui.painter().text(pos2(r.left() + 26.0, r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(12.5), T.text);
                if resp.clicked() {
                    app.dispatch(cmd, json!({}));
                }
            }
        });
        ui.add_space(12.0);
        ui.label(RichText::new(&m.subject).size(20.0).color(T.text));
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(44.0, 44.0), Sense::hover());
            let hue = m.from.email.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
            let palette = [
                Color32::from_rgb(0x2b, 0x6c, 0xb0),
                Color32::from_rgb(0x8a, 0x3f, 0xa8),
                Color32::from_rgb(0x1f, 0x7a, 0x5c),
                Color32::from_rgb(0xb0, 0x5a, 0x1d),
                Color32::from_rgb(0xa8, 0x2e, 0x55),
            ];
            let color = palette.get((hue as usize) % palette.len()).copied().unwrap_or(T.accent);
            ui.painter().circle_filled(r.center(), 21.0, color);
            ui.painter().text(r.center(), Align2::CENTER_CENTER, initials(m.from.display()), FontId::proportional(16.0), Color32::WHITE);
            ui.add_space(6.0);
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(m.from.full()).size(14.0).color(T.text));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(m.date.with_timezone(&Local).format("%a %-m/%-d/%Y %-I:%M %p").to_string()).size(12.0).color(T.text_dim),
                        );
                    });
                });
                let to: Vec<String> = m.to.iter().map(|a| a.display().to_string()).collect();
                ui.label(RichText::new(format!("To: {}", to.join("; "))).size(12.5).color(T.text_dim));
                if !m.cc.is_empty() {
                    let cc: Vec<String> = m.cc.iter().map(|a| a.display().to_string()).collect();
                    ui.label(RichText::new(format!("Cc: {}", cc.join("; "))).size(12.5).color(T.text_dim));
                }
            });
        });
        if m.importance == Importance::High {
            ui.add_space(6.0);
            ui.label(RichText::new("This message was sent with High importance.").size(12.0).color(T.danger));
        }
        if !m.attachments.is_empty() {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                for a in &m.attachments {
                    let text = format!("{}  ({})", a.name, human_size(a.size));
                    let w = ui.painter().layout_no_wrap(text.clone(), FontId::proportional(12.5), T.text).size().x + 36.0;
                    let (r, _) = ui.allocate_exact_size(vec2(w, 34.0), Sense::hover());
                    ui.painter().rect(r, CornerRadius::same(3), T.pane, Stroke::new(1.0, T.border), egui::StrokeKind::Inside);
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(pos2(r.left() + 15.0, r.center().y), vec2(16.0, 16.0)),
                        Icon::Attach,
                        T.text_dim,
                    );
                    ui.painter().text(pos2(r.left() + 28.0, r.center().y), Align2::LEFT_CENTER, text, FontId::proportional(12.5), T.text);
                }
            });
        }
        ui.add_space(10.0);
        let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(sep, 0.0, T.border);
        ui.add_space(12.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(&m.body).size(14.0).color(T.text)).wrap());
        });
    });
}

fn initials(name: &str) -> String {
    let mut it = name.split_whitespace().filter_map(|w| w.chars().next());
    let a = it.next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    let b = it.next_back().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    format!("{a}{b}")
}

fn human_size(n: usize) -> String {
    match n {
        0..1024 => format!("{n} B"),
        1024..1_048_576 => format!("{} KB", n / 1024),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

// ----- Compose -----

fn compose(app: &mut MailApp, ui: &mut Ui) {
    let Some(mut d) = app.session.compose.clone() else { return };
    let before = d.clone();
    egui::Frame::NONE.inner_margin(egui::Margin { left: 16, right: 16, top: 12, bottom: 12 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            let (r, resp) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::click());
            ui.painter().rect_filled(r, 3.0, if resp.hovered() { T.accent_dark } else { T.accent });
            icons::paint(ui.painter(), Rect::from_center_size(pos2(r.center().x, r.top() + 24.0), vec2(26.0, 26.0)), Icon::Send, Color32::WHITE);
            ui.painter().text(pos2(r.center().x, r.bottom() - 12.0), Align2::CENTER_CENTER, "Send", FontId::proportional(12.5), Color32::WHITE);
            if resp.on_hover_text("Send (Ctrl+Enter)").clicked() {
                app.dispatch("compose.send", json!({}));
            }
            ui.add_space(6.0);
            ui.vertical(|ui| {
                if ui.button("Save draft").on_hover_text("Ctrl+S").clicked() {
                    app.dispatch("compose.save_draft", json!({}));
                }
                if ui.button("Discard").on_hover_text("Esc").clicked() {
                    app.dispatch("compose.discard", json!({}));
                }
            });
        });
        ui.add_space(10.0);
        for (label, field) in [("To...", &mut d.to), ("Cc...", &mut d.cc), ("Subject", &mut d.subject)] {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(64.0, 24.0), Sense::hover());
                ui.painter().text(pos2(r.left(), r.center().y), Align2::LEFT_CENTER, label, FontId::proportional(13.0), T.text);
                ui.add(egui::TextEdit::singleline(field).desired_width(ui.available_width()));
            });
            ui.add_space(4.0);
        }
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_sized(ui.available_size(), egui::TextEdit::multiline(&mut d.body));
        });
    });
    if d != before {
        app.dispatch("compose.set", json!({ "to": d.to, "cc": d.cc, "subject": d.subject, "body": d.body }));
    }
}
