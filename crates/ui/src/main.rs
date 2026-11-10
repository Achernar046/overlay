//! sysmon-ui — entry point (egui dashboard, Phase 4)

use eframe::egui;
use sysmon_core::Config;

mod app;

use crate::app::SystemMonitorApp;

fn main() -> eframe::Result<()> {
    let mut warn = |msg: &str| eprintln!("[config] {msg}");
    let config = Config::load_or_default("sysmon.toml", &mut warn);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 720.0])
            .with_min_inner_size([720.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "sysmon — System Monitor",
        options,
        Box::new(move |_cc| Ok(Box::new(SystemMonitorApp::new(config)))),
    )
}
