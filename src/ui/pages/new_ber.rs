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

/// Parse a year string into a plausible construction year (1700–2100), or
/// `None` if it's empty/garbage/out of range.
pub fn parse_year(s: &str) -> Option<u32> {
    s.trim()
        .parse::<u32>()
        .ok()
        .filter(|y| (1700..=2100).contains(y))
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

                ui.label("Year of Construction:");
                ui.add(
                    egui::TextEdit::singleline(&mut state.construction_year)
                        .hint_text("e.g. 1985")
                        .desired_width(80.0),
                );
                ui.end_row();
            });

        ui.add_space(10.0);

        // Extensions: each adds a selectable construction phase (Extension1..4)
        // with a descriptive nickname (shown in the UI) + its own build year.
        // Capped at 4 — matching the phase enum.
        ui.label(RichText::new("Extensions").strong());
        let mut remove: Option<usize> = None;
        for (i, ext) in state.extensions.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("Extension {}:", i + 1));
                ui.add(
                    egui::TextEdit::singleline(&mut ext.nickname)
                        .hint_text("name e.g. Back extension")
                        .desired_width(160.0),
                );
                ui.label("year");
                ui.add(
                    egui::TextEdit::singleline(&mut ext.year)
                        .hint_text("2008")
                        .desired_width(60.0),
                );
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            state.extensions.remove(i);
        }
        if state.extensions.len() < 4 && ui.button("+ Add extension").clicked() {
            state
                .extensions
                .push(crate::ui::app::ExtensionInput::default());
        }

        ui.add_space(20.0);

        let year_ok = parse_year(&state.construction_year).is_some();
        let can_create = !state.name.trim().is_empty() && year_ok;
        if state.name.trim().is_empty() {
            ui.label(
                RichText::new("Please enter a name").color(egui::Color32::from_rgb(220, 80, 80)),
            );
        } else if !year_ok {
            ui.label(
                RichText::new("Please enter a valid year of construction")
                    .color(egui::Color32::from_rgb(220, 80, 80)),
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
