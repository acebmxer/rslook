#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
// No console window behind the app on Windows release builds.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use rslook_engine::Session;
use rslook_ui_egui::MailApp;

struct App(MailApp);

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.0.ui(ui);
    }
}

fn main() -> eframe::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let session = if args.iter().any(|a| a == "--empty") {
        Session::empty(rslook_engine::mail::Address::new("", "me@localhost"))
    } else {
        // Until accounts land, the app opens the sample mailbox.
        rslook_engine::sample::sample_session()
    };
    let mut app = MailApp::new(session);
    for path in args.iter().filter(|a| a.ends_with(".eml")) {
        app.execute("mail.import_eml", &serde_json::json!({ "path": path }));
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Inbox - Rslook")
            .with_app_id("rslook")
            .with_inner_size([1400.0, 860.0])
            .with_min_inner_size([800.0, 500.0]),
        ..Default::default()
    };
    eframe::run_native("Rslook", options, Box::new(move |_cc| Ok(Box::new(App(app)))))
}
