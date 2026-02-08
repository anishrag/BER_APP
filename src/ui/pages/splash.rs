use crate::ui::app::BerApp;
use eframe::egui;

pub struct SplashPage;

impl SplashPage {
    pub fn render(ui: &mut egui::Ui, app: &mut BerApp) {
        // Start loading on first render
        if !app.splash_state.loading_started {
            app.start_loading();
        }

        ui.vertical_centered(|ui| {
            ui.add_space(100.0);

            // App title/logo
            ui.heading(egui::RichText::new("BER App").size(32.0));

            ui.add_space(50.0);

            // Progress text
            ui.label(&app.splash_state.progress_text);

            ui.add_space(20.0);

            // Progress bar
            let progress_bar = egui::ProgressBar::new(app.splash_state.progress_value)
                .show_percentage()
                .animate(true);
            ui.add(progress_bar);

            ui.add_space(20.0);

            // Optional: Loading spinner
            if app.splash_state.progress_value < 1.0 {
                ui.spinner();
            }
        });
    }
}
