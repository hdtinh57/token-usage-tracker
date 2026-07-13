mod alerts;
mod discovery;
mod history;
mod model;
mod parse_claude;
mod parse_codex;
mod paths;
mod pricing;
mod quota;
mod settings;
mod tail;
mod tray;
mod ui;
mod watch;
mod worker;

use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

fn main() -> eframe::Result<()> {
    let home = std::env::var("USERPROFILE").expect("USERPROFILE not set");
    let claude_root = PathBuf::from(&home).join(".claude").join("projects");
    let codex_root = PathBuf::from(&home).join(".codex").join("sessions");
    let paths = paths::Paths::new().expect("failed to initialize app data directory");
    let settings = settings::Settings::load(&paths.settings).expect("failed to load settings.json");

    let today = chrono::Local::now().date_naive();
    let snapshot = Arc::new(Mutex::new(Arc::new(model::Stats::new(today))));
    let pricing_path = paths.pricing.clone();

    let worker = worker::Worker::new(
        claude_root,
        codex_root,
        paths.pricing,
        paths.history,
        snapshot.clone(),
    )
    .expect("failed to initialize worker (check pricing.json permissions)");
    std::thread::spawn(move || worker.run());

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([360.0, 450.0])
            .with_resizable(false),
        ..Default::default()
    };
    let (tray_tx, tray_rx) = mpsc::channel();
    let settings_path = paths.settings.clone();
    eframe::run_native(
        "Token Usage Tracker",
        options,
        Box::new(move |_cc| {
            let tray = tray::Tray::new(settings, tray_tx)?;
            Ok(Box::new(ui::App::new(
                snapshot,
                settings,
                settings_path,
                pricing_path,
                tray_rx,
                tray,
            )))
        }),
    )
}
