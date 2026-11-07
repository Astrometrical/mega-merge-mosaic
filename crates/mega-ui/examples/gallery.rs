//! The Mega UI gallery in a window: every token and component of the
//! standard. `cargo run -p mega-ui --example gallery`

use mega_ui::gallery::{self, GalleryState};

struct App {
    state: GalleryState,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| gallery::show(ui, &mut self.state));
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Mega UI gallery")
            .with_inner_size([1200.0, 900.0]),
        ..Default::default()
    };
    eframe::run_native(
        "mega-ui-gallery",
        options,
        Box::new(|cc| {
            mega_ui::theme::apply(&cc.egui_ctx);
            Ok(Box::new(App {
                state: GalleryState::default(),
            }))
        }),
    )
}
