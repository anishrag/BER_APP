use eframe::egui;
use rusqlite::Connection;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};

mod controller;
mod engine;
mod ui;

use controller::app_controller::AppController;
use ui::app::BerApp;

fn main() -> Result<(), eframe::Error> {
    let conn = Connection::open_in_memory().expect("Failed to create database");
    let app_controller = Rc::new(RefCell::new(AppController::new(conn)));

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
