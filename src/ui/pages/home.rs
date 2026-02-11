use crate::ui::pages::events::{Page, UiEvent};
use eframe::egui;

pub struct HomePage;

impl HomePage {
    pub fn render<F>(ui: &mut egui::Ui, mut on_event: F)
    where
        F: FnMut(UiEvent),
    {
        ui.heading("Home");

        ui.add_space(20.0);

        ui.horizontal(|ui| {
            if ui.button("New BER").clicked() {
                on_event(UiEvent::NavigateTo(Page::NewBer));
            }

            if ui.button("View Sketch").clicked() {
                on_event(UiEvent::NavigateTo(Page::Sketch));
            }

            if ui.button("Key Details").clicked() {
                on_event(UiEvent::NavigateTo(Page::KeyDeets));
            }

            if ui.button("More Details").clicked() {
                on_event(UiEvent::NavigateTo(Page::MoreDeets));
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
