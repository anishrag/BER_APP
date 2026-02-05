use crate::ui::app::{BerApp, Page};
use eframe::egui;

pub struct HomePage;

impl HomePage {
    pub fn render(ui: &mut egui::Ui, app: &mut BerApp) {
        ui.heading("Home");

        ui.add_space(20.0);

        ui.horizontal(|ui| {
            if ui.button("New BER").clicked() {
                app.navigate_to(Page::NewBer);
            }

            if ui.button("View Sketch").clicked() {
                app.navigate_to(Page::Sketch);
            }

            if ui.button("Key Details").clicked() {
                app.navigate_to(Page::KeyDeets);
            }

            if ui.button("More Details").clicked() {
                app.navigate_to(Page::MoreDeets);
            }
        });

        ui.add_space(20.0);

        ui.label("Welcome to the BER App!");

        // TODO: Add your home page content here
        // - List of recent BERs
        // - Quick actions
        // - Statistics
        // etc.
    }
}
