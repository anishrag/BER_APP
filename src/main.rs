use eframe::egui;
use rusqlite::Connection;
use std::rc::Rc;

mod controller;
mod engine;
mod ui;

use controller::app_controller::AppController;
use ui::app::BerApp;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let conn = Connection::open_in_memory()?;

    // Create the shared AppController
    let app_controller = Rc::new(AppController::new(conn));

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("BER App"),
        ..Default::default()
    };

    eframe::run_native(
        "BER App",
        options,
        Box::new(|_cc| Ok(Box::new(BerApp::new(app_controller)))),
    )
}
