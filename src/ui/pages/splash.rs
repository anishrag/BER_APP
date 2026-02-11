use crate::ui::pages::events::UiEvent;
use eframe::egui;

pub struct SplashPage;

impl SplashPage {
    pub fn render<F>(
        ui: &mut egui::Ui,
        progress_text: String,
        progress_value: f32,
        mut _on_event: F,
    ) where
        F: FnMut(UiEvent),
    {
        ui.vertical_centered(|ui| {
            ui.add_space(100.0);

            // App title/logo
            ui.heading(egui::RichText::new("BER App").size(32.0));

            ui.add_space(50.0);

            // Progress text
            ui.label(progress_text);

            ui.add_space(20.0);

            // Progress bar
            let progress_bar = egui::ProgressBar::new(progress_value)
                .show_percentage()
                .animate(true);
            ui.add(progress_bar);

            ui.add_space(20.0);

            // Optional: Loading spinner
            if progress_value < 1.0 {
                ui.spinner();
            }
        });
    }
}
