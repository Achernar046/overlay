//! minimal eframe app เพื่อวัด baseline RAM ของ framework + GPU driver (dev tool)
use eframe::egui;

struct App;

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        ui.label("minimal");
    }
}

fn main() -> eframe::Result<()> {
    eframe::run_native(
        "minimal-eframe",
        eframe::NativeOptions::default(),
        Box::new(|_| Ok(Box::new(App))),
    )
}
