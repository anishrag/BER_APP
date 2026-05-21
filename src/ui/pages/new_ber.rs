use crate::ui::app::NewBerState;
use crate::ui::pages::events::{Page, UiEvent};
use chrono::NaiveDate;
use egui::{RichText, Ui};
use egui_extras::DatePickerButton;

#[derive(Debug)]
pub enum NewBerField {
    Name(String),
    DateAssessed(NaiveDate),
}

#[derive(Debug)]
pub enum NewBerEvent {
    CreateClicked,
}

pub struct NewBerPage;

impl NewBerPage {
    pub fn render<F>(ui: &mut Ui, state: &mut NewBerState, mut on_event: F)
    where
        F: FnMut(UiEvent),
    {
        ui.heading("Create New BER");
        ui.add_space(20.0);

        if ui.button("← Back to Home").clicked() {
            on_event(UiEvent::NavigateTo(Page::Home));
        }

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(20.0);

        // Default the date to today the first time the form opens.
        let date = state
            .date_assessed
            .get_or_insert_with(|| chrono::Local::now().naive_local().date());

        egui::Grid::new("new_ber_form")
            .num_columns(2)
            .spacing([40.0, 10.0])
            .show(ui, |ui| {
                ui.label("BER Name:");
                ui.text_edit_singleline(&mut state.name);
                ui.end_row();

                ui.label("Date Assessed:");
                ui.add(DatePickerButton::new(date));
                ui.end_row();
            });

        ui.add_space(20.0);

        let can_create = !state.name.trim().is_empty();
        if !can_create {
            ui.label(
                RichText::new("Please enter a name").color(egui::Color32::from_rgb(220, 80, 80)),
            );
        }

        ui.add_space(20.0);

        ui.horizontal(|ui| {
            if ui
                .add_enabled(can_create, egui::Button::new("Create BER"))
                .clicked()
            {
                on_event(UiEvent::NewBer(NewBerEvent::CreateClicked));
            }
        });
    }
}
