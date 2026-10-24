#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Outlook-classic-style front end: ribbon, folder pane, message list, reading pane,
//! navigation bar and status bar. The UI is thin: every action dispatches a command id
//! through [`Session::run`].

pub mod icons;
mod panes;
mod ribbon;
pub mod theme;

use egui::{Key, Modifiers};
use rslook_engine::{Session, commands};
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RibbonTab {
    File,
    #[default]
    Home,
    SendReceive,
    Folder,
    View,
    Help,
}

pub struct MailApp {
    pub session: Session,
    pub tab: RibbonTab,
    /// Last command error, shown in the status bar.
    pub error: Option<String>,
    /// The search box's text; changes dispatch `search.set`.
    pub search_text: String,
    focus_search: bool,
    styled: bool,
    pending: Vec<(String, Value)>,
}

impl MailApp {
    pub fn new(session: Session) -> Self {
        let search_text = session.search.clone();
        Self { session, tab: RibbonTab::Home, error: None, search_text, focus_search: false, styled: false, pending: Vec::new() }
    }

    /// Queues a command; queued commands run once the frame's widgets are laid out.
    pub fn dispatch(&mut self, id: &str, params: Value) {
        self.pending.push((id.to_string(), params));
    }

    /// Runs a command now and records its outcome.
    pub fn execute(&mut self, id: &str, params: &Value) {
        match self.session.run(id, params) {
            Ok(()) => self.error = None,
            Err(e) => {
                log::info!("{id}: {e}");
                self.error = Some(e.to_string());
            }
        }
    }

    pub fn enabled(&self, id: &str) -> bool {
        rslook_engine::find(id).is_some_and(|c| (c.enabled)(&self.session))
    }

    fn flush(&mut self) {
        for (id, p) in std::mem::take(&mut self.pending) {
            self.execute(&id, &p);
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        if !self.styled {
            // Light theme (a dark theme comes later with the desktop-appearance setting).
            ctx.set_theme(egui::Theme::Light);
            self.styled = true;
        }
        self.shortcuts(&ctx);
        ribbon::show(self, ui);
        panes::status_bar(self, ui);
        panes::nav_bar(self, ui);
        panes::main_area(self, ui);
        self.flush();
    }

    /// Keyboard shortcuts from the command registry. Bare keys (Delete, Backspace, Insert) only
    /// act when no text field has focus, so typing is never hijacked.
    fn shortcuts(&mut self, ctx: &egui::Context) {
        let typing = ctx.memory(|m| m.focused().is_some());
        let mut hits = Vec::new();
        for c in commands() {
            let Some((mods, key)) = c.shortcut.and_then(parse_shortcut) else { continue };
            if mods.is_none() && typing && key != Key::Escape {
                continue;
            }
            if ctx.input_mut(|i| i.consume_key(mods, key)) {
                hits.push(c.id);
            }
        }
        for id in hits {
            if id == "search.set" {
                // Ctrl+E focuses the search box; the typed text runs the command.
                self.focus_search = true;
            } else if self.enabled(id) {
                self.execute(id, &json!({}));
            }
        }
    }
}

/// Parses `Ctrl+Shift+R` style shortcuts.
pub fn parse_shortcut(s: &str) -> Option<(Modifiers, Key)> {
    let mut mods = Modifiers::NONE;
    let mut key = None;
    for part in s.split('+') {
        match part {
            "Ctrl" => mods = mods | Modifiers::COMMAND,
            "Shift" => mods = mods | Modifiers::SHIFT,
            "Alt" => mods = mods | Modifiers::ALT,
            "." => key = Some(Key::Period),
            "," => key = Some(Key::Comma),
            "" => {}
            k => key = Key::from_name(k),
        }
    }
    key.map(|k| (mods, k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shortcut_parses() {
        for c in commands() {
            if let Some(s) = c.shortcut {
                assert!(parse_shortcut(s).is_some(), "{} has unparseable shortcut {s}", c.id);
            }
        }
    }
}
