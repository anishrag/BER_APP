use crate::{AppNavigation, MainWindow, Page, SplashState};
use slint::{ComponentHandle, Weak};
use std::rc::Rc;
use std::sync::mpsc::Sender;
use std::thread;

use super::app_controller::AppController;

/// Splash page controller
pub struct SplashController {
    ui: Weak<MainWindow>,              // weak reference to the UI
    app_controller: Rc<AppController>, // shared application logic
}

impl SplashController {
    /// Create a new splash controller
    pub fn new(ui: Weak<MainWindow>, app_controller: Rc<AppController>) -> Self {
        Self { ui, app_controller }
    }

    /// Connect UI callbacks (if any) to the controller
    pub fn setup_callbacks(&self, _ui: &MainWindow) {
        // Currently no explicit callbacks, but you could listen for cancel events etc.
    }

    pub fn start_loading(&self) {
        let ui_weak = self.ui.clone();
        let app_controller = self.app_controller.clone();

        // Create a channel for progress updates
        let (tx, rx) = std::sync::mpsc::channel::<(String, f32)>();

        // Start loading in AppController (already spawns a thread)
        app_controller.load_data_async(tx);

        // Start a thread to listen for progress
        thread::spawn(move || {
            // Listen for progress updates in the main thread
            for (text, progress) in rx {
                let ui_weak_clone = ui_weak.clone();
                let text_clone = text.clone();

                slint::invoke_from_event_loop(move || {
                    if let Some(ui) = ui_weak_clone.upgrade() {
                        // Access globals directly - no forwarding!
                        let splash_state = ui.global::<SplashState>();
                        splash_state.set_progress_text(text_clone.into());
                        splash_state.set_progress_value(progress);

                        if progress >= 1.0 {
                            // Navigate using the global
                            let nav = ui.global::<AppNavigation>();
                            println!("Loading complete, navigating to Home page");
                            nav.set_current_page(Page::Home);
                        }
                    }
                })
                .ok();
            }
        });
    }
}
