mod controller;
mod engine;

use std::rc::Rc;

use controller::app_controller::AppController;
use controller::pages::home_controller::HomeController;
use controller::pages::key_deets_controller::KeyDeetsController;
use controller::pages::more_deets_controller::MoreDeetsController;
use controller::pages::new_ber_controller::NewBerController;
use controller::pages::sketch_window_controller::SketchWindowController;
use controller::pages::splash_controller::SplashController;

slint::include_modules!();

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Create the shared AppController
    let app_controller = Rc::new(AppController::new());

    // Instantiate Slint main window
    let main_window = MainWindow::new()?;

    // ---------------------------
    // Setup SplashController
    // ---------------------------
    let splash_screen = SplashScreen::new()?;
    let splash_controller = SplashController::new(main_window.as_weak(), app_controller.clone());
    splash_controller.start_loading(); // starts async loading immediately

    // ---------------------------
    // Setup HomeController
    // ---------------------------
    let home_window = HomeWindow::new()?;
    let home_controller = HomeController::new(home_window.as_weak(), app_controller.clone());

    // ---------------------------
    // Setup SketchWindowController
    // ---------------------------
    let sketch_window = SketchWindow::new()?; // assuming separate window
    let sketch_controller =
        SketchWindowController::new(sketch_window.as_weak(), app_controller.clone());

    // ---------------------------
    // Setup NewBERController
    // ---------------------------
    let new_ber_window = NewBERWindow::new()?; // separate window
    let new_ber_controller =
        NewBerController::new(new_ber_window.as_weak(), app_controller.clone());

    // ---------------------------
    // Setup KeyDeetsController
    // ---------------------------
    let key_deets_window = KeyDeetsWindow::new()?;
    let key_deets_controller =
        KeyDeetsController::new(key_deets_window.as_weak(), app_controller.clone());

    // ---------------------------
    // Setup MoreDeetsController
    // ---------------------------
    let more_deets_window = MoreDeetsWindow::new()?;
    let more_deets_controller =
        MoreDeetsController::new(more_deets_window.as_weak(), app_controller.clone());

    // ---------------------------
    // Run the main Slint window
    // ---------------------------
    main_window.run()?;

    Ok(())
}
