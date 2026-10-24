//! Headless screenshot of the Rslook window (offscreen wgpu; no display needed).
//!
//! `cargo run -p rslook-ui-egui --example ui_shot -- /abs/out.png [command[=json] ...]`
//!
//! Each extra argument runs a command first, e.g. `mail.reply` or
//! `view.reading_pane={"position":"bottom"}`. `tab:<file|send_receive|folder|view|help>`
//! picks the ribbon tab.

use rslook_ui_egui::{MailApp, RibbonTab};

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "/tmp/rslook.png".into());
    let mut app = MailApp::new(rslook_engine::sample::sample_session());
    for a in args {
        if let Some(tab) = a.strip_prefix("tab:") {
            app.tab = match tab {
                "file" => RibbonTab::File,
                "send_receive" => RibbonTab::SendReceive,
                "folder" => RibbonTab::Folder,
                "view" => RibbonTab::View,
                "help" => RibbonTab::Help,
                _ => RibbonTab::Home,
            };
            continue;
        }
        let (id, params) = match a.split_once('=') {
            Some((id, p)) => (id.to_string(), serde_json::from_str(p).expect("json params")),
            None => (a.clone(), serde_json::json!({})),
        };
        app.execute(&id, &params);
        if let Some(e) = &app.error {
            eprintln!("{id}: {e}");
        }
    }
    let mut harness = egui_kittest::Harness::builder()
        .with_size(egui::vec2(1400.0, 860.0))
        .with_pixels_per_point(1.5)
        .wgpu()
        .build_ui_state(|ui, app: &mut MailApp| app.ui(ui), app);
    harness.run_steps(6);
    let img = harness.render().expect("render");
    img.save(&out).expect("save png");
    println!("{out}");
}
