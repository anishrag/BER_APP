pub mod adapters;
pub mod app;
pub mod pages;
pub mod render;

use eframe::egui;
use std::cell::RefCell;
use std::rc::Rc;

use crate::controller::app_controller::AppController;
use app::BerApp;

/// Re-exported so `main.rs` doesn't need to import `eframe` directly.
pub type RunError = eframe::Error;

/// Launch the eframe app (desktop): fixed dev window.
pub fn run(controller: Rc<RefCell<AppController>>) -> Result<(), RunError> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([800.0, 600.0])
            .with_title("BER App"),
        ..Default::default()
    };
    launch(controller, options)
}

/// Launch on Android: the platform owns the surface size (no fixed
/// inner_size), and eframe needs the `AndroidApp` handle to build its event
/// loop (eframe 0.33's `android_app` field).
#[cfg(target_os = "android")]
pub fn run_android(
    controller: Rc<RefCell<AppController>>,
    android_app: winit::platform::android::activity::AndroidApp,
) -> Result<(), RunError> {
    let options = eframe::NativeOptions {
        android_app: Some(android_app),
        ..Default::default()
    };
    launch(controller, options)
}

/// The single place eframe is wired to the controller; both entry points
/// funnel here so the renderer/backend choice is one line.
fn launch(
    controller: Rc<RefCell<AppController>>,
    options: eframe::NativeOptions,
) -> Result<(), RunError> {
    eframe::run_native(
        "BER App",
        options,
        Box::new(|_cc| Ok(Box::new(BerApp::new(controller)))),
    )
}
