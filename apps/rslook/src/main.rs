#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// No console window behind the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use rslook_engine::Session;
use rslook_engine::sync::{Paths, SyncRequest};
use rslook_ui_egui::MailApp;
use std::sync::Arc;

struct App(MailApp);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }

    fn on_exit(&mut self) {
        self.0.save_now();
    }
}

/// The saved account and its offline cache, or the sample mailbox when there is no account.
fn open_session(paths: Option<Paths>) -> (Session, Option<String>) {
    let Some(paths) = paths else {
        return (rslook_engine::sample::sample_session(), Some("No home folder found; settings will not be saved.".into()));
    };
    let mut note = None;
    let account = match rslook_engine::account::load_accounts(&paths.accounts_file()) {
        Ok(list) => list.into_iter().next(),
        Err(e) => {
            note = Some(e.to_string());
            None
        }
    };
    let mut session = match account {
        // No account yet: the sample mailbox, until one is added on the File tab.
        None => rslook_engine::sample::sample_session(),
        Some(account) => {
            let cached = match rslook_engine::sync::load_cache(&paths.cache_file(&account)) {
                Ok(c) => c,
                Err(e) => {
                    note = Some(format!("{e}; starting with an empty mailbox"));
                    None
                }
            };
            let mut s = Session::empty(Default::default());
            s.open_account(account, cached);
            s.request(SyncRequest::All);
            s
        }
    };
    session.set_secrets(Arc::new(rslook_engine::account::OsKeychain));
    session.set_paths(paths);
    (session, note)
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (session, note) = if args.iter().any(|a| a == "--empty") {
        (Session::empty(rslook_engine::mail::Address::new("", "me@localhost")), None)
    } else {
        open_session(Paths::platform())
    };
    let mut app = MailApp::new(session);
    app.error = note;
    let importable = |a: &str| {
        let a = a.to_ascii_lowercase();
        [".eml", ".msg", ".pst"].iter().any(|e| a.ends_with(e))
    };
    for path in args.iter().filter(|a| importable(a)) {
        app.execute("mail.import", &serde_json::json!({ "path": path }));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Inbox - Rslook")
            .with_app_id("rslook")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Rslook",
        options,
        Box::new(move |cc| {
            let ctx = cc.egui_ctx.clone();
            match rslook_sync::Worker::spawn(Default::default(), Box::new(move || ctx.request_repaint())) {
                Ok(w) => app.attach_worker(w),
                Err(e) => app.error = Some(format!("Send/Receive is unavailable: {e}")),
            }
            Ok(Box::new(App(app)))
        }),
    )
}
