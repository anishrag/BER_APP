use crate::ui::pages::events::{Field, Page, UiEvent};
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
    pub fn render<F>(ui: &mut Ui, mut on_event: F)
    where
        F: FnMut(UiEvent),
    {
        ui.heading("Create New BER");
        ui.add_space(20.0);

        // ← Back to Home button
        if ui.button("← Back to Home").clicked() {
            on_event(UiEvent::NavigateTo(Page::Home));
        }

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(20.0);

        // Temporary local form state
        let mut name = String::new();
        let mut assessed_date: NaiveDate = chrono::Local::now().naive_local().date();
        let mut error: Option<String> = None;

        // Form fields in a grid
        egui::Grid::new("new_ber_form")
            .num_columns(2)
            .spacing([40.0, 10.0])
            .show(ui, |ui| {
                // BER Name
                ui.label("BER Name:");
                ui.text_edit_singleline(&mut name);
                ui.end_row();

                // Date Assessed using egui_field_editor date picker
                ui.label("Date Assessed:");
                ui.add(DatePickerButton::new(&mut assessed_date));
                ui.end_row();
            });

        ui.add_space(20.0);

        // Validate form
        if name.is_empty() {
            error = Some("Name cannot be empty".to_string());
        }

        // Show validation errors
        if let Some(err) = &error {
            ui.label(RichText::new(err).color(egui::Color32::RED));
        }

        ui.add_space(20.0);

        // Submit button
        ui.horizontal(|ui| {
            let can_create = !name.is_empty();

            if ui
                .add_enabled(can_create, egui::Button::new("Create BER"))
                .clicked()
            {
                // Emit field updates via the callback
                on_event(UiEvent::SetField(Field::NewBer(NewBerField::Name(
                    name.clone(),
                ))));
                on_event(UiEvent::SetField(Field::NewBer(NewBerField::DateAssessed(
                    assessed_date,
                ))));
                on_event(UiEvent::NewBer(NewBerEvent::CreateClicked));

                // Navigate back to home
                on_event(UiEvent::NavigateTo(Page::Home));
            }

            if !can_create {
                ui.label(RichText::new("Please fill in all fields").small().italics());
            }
        });
    }
}

// Add this state struct to ui/app.rs:
#[derive(Default)]
pub struct NewBerState {
    pub name: String,
    pub date_str: String,
    pub date_assessed: Option<NaiveDate>,
    pub date_error: Option<String>,
}
