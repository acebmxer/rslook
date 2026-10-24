#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

//! Outlook-classic-style front end: ribbon, folder pane, message list, reading pane,
//! navigation bar and status bar. The UI is thin: every action dispatches a command id
//! through [`Session::run`].

mod account;
pub mod icons;
mod panes;
mod ribbon;
pub mod theme;

use egui::{Key, Modifiers};
use rslook_engine::{Session, commands};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

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
    /// Runs Send/Receive jobs off the UI thread. `None` in tests and screenshots.
    worker: Option<rslook_sync::Worker>,
    last_save: Instant,
    /// The Remove Account button was pressed once and waits for confirmation.
    confirm_remove: bool,
}

/// How often a changed mailbox is written to the offline cache.
const SAVE_EVERY: Duration = Duration::from_secs(2);

impl MailApp {
    pub fn new(session: Session) -> Self {
        let search_text = session.search.clone();
        Self {
            session,
            tab: RibbonTab::Home,
            error: None,
            search_text,
            focus_search: false,
            styled: false,
            pending: Vec::new(),
            worker: None,
            last_save: Instant::now(),
            confirm_remove: false,
        }
    }

    /// Connects the Send/Receive worker.
    pub fn attach_worker(&mut self, worker: rslook_sync::Worker) {
        self.worker = Some(worker);
    }

    /// Hands finished jobs to the session, starts the next one, and saves the cache.
    fn pump(&mut self, ctx: &egui::Context) {
        if let Some(w) = &self.worker {
            for r in w.poll() {
                self.session.apply_result(r);
            }
            if let Some(job) = self.session.next_job()
                && !w.submit(job)
            {
                self.session.sync_state = rslook_engine::sync::SyncState::Error("Send/Receive stopped working; restart Rslook.".into());
            }
        }
        if self.session.busy() {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
        if self.session.dirty && self.last_save.elapsed() >= SAVE_EVERY {
            self.last_save = Instant::now();
            if let Err(e) = self.session.save_cache() {
                log::warn!("saving the offline cache: {e}");
                self.error = Some(format!("Could not save the offline cache: {e}"));
            }
        } else if self.session.dirty {
            ctx.request_repaint_after(SAVE_EVERY);
        }
    }

    /// Writes any unsaved changes now (call on exit).
    pub fn save_now(&mut self) {
        if let Err(e) = self.session.save_cache() {
            log::warn!("saving the offline cache: {e}");
        }
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
        account::dialog(self, &ctx);
        self.flush();
        self.pump(&ctx);
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
            "Ctrl" => mods |= Modifiers::COMMAND,
            "Shift" => mods |= Modifiers::SHIFT,
            "Alt" => mods |= Modifiers::ALT,
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

    /// Draws every ribbon tab and the account dialog headlessly; typing into the dialog and
    /// pressing Save goes through the `account.*` commands.
    #[test]
    fn account_dialog_and_tabs_render_and_dispatch() {
        let app = MailApp::new(rslook_engine::sample::sample_session());
        let mut h = egui_kittest::Harness::builder().with_size(egui::vec2(1400.0, 860.0)).build_ui_state(|ui, app: &mut MailApp| app.ui(ui), app);
        for tab in [RibbonTab::File, RibbonTab::Home, RibbonTab::SendReceive, RibbonTab::Folder, RibbonTab::View, RibbonTab::Help] {
            h.state_mut().tab = tab;
            h.run_steps(2);
        }
        h.state_mut().execute("account.add", &json!({}));
        h.run_steps(2);
        assert!(h.state().session.account_dialog.is_some());
        h.state_mut().execute(
            "account.set",
            &json!({"email": "test@example.test", "password": "pw", "imap_host": "imap.example.test", "smtp_host": "smtp.example.test"}),
        );
        h.run_steps(2);
        h.state_mut().execute("account.save", &json!({}));
        assert!(h.state().error.is_none(), "{:?}", h.state().error);
        assert!(h.state().session.account().is_some());
        h.state_mut().tab = RibbonTab::SendReceive;
        h.run_steps(2);
        assert!(h.state().enabled("sendrecv.all"));
        assert!(h.state().session.has_requests(), "saving asks for a first Send/Receive");
    }
}
