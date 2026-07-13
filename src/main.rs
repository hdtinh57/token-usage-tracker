mod discovery;
mod model;
mod parse_claude;
mod parse_codex;
mod pricing;
mod tail;
mod ui;
mod worker;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn main() -> eframe::Result<()> {
    let home = std::env::var("USERPROFILE").expect("USERPROFILE not set");
    let claude_root = PathBuf::from(&home).join(".claude").join("projects");
    let codex_root = PathBuf::from(&home).join(".codex").join("sessions");
    let pricing_path = std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.parent()
                .map(|directory| directory.join("pricing.json"))
        })
        .unwrap_or_else(|| PathBuf::from("pricing.json"));

    let today = chrono::Local::now().date_naive();
    let snapshot = Arc::new(Mutex::new(Arc::new(model::Stats::new(today))));

    let worker = worker::Worker::new(claude_root, codex_root, pricing_path, snapshot.clone())
        .expect("failed to initialize worker (check pricing.json permissions)");
    std::thread::spawn(move || worker.run());

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([360.0, 450.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        "Token Usage Tracker",
        options,
        Box::new(move |_cc| Ok(Box::new(ui::App::new(snapshot)))),
    )
}
