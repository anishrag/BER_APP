pub mod adapters;
pub mod app;
pub mod pages;

use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use crate::controller::app_controller::AppController;
use app::BerApp;

/// Re-exported so `main.rs` doesn't need to import `eframe` directly.
pub type RunError = eframe::Error;

/// Launch the eframe app. This is the only function that wires eframe to the controller.
pub fn run(controller: Rc<RefCell<AppController>>) -> Result<(), RunError> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("BER App"),
        ..Default::default()
    };

    eframe::run_native(
        "BER App",
        options,
        Box::new(|_cc| Ok(Box::new(BerApp::new(controller)))),
    )
}
