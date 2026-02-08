use crate::ui::app::{BerApp, Page};
use chrono::NaiveDate;
use eframe::egui;

pub struct NewBerPage;

impl NewBerPage {
    pub fn render(ui: &mut egui::Ui, app: &mut BerApp) {
        ui.heading("Create New BER");
        ui.add_space(20.0);

        // Back button
        if ui.button("← Back to Home").clicked() {
            app.navigate_to(Page::Home);
        }

        ui.add_space(20.0);
        ui.separator();
        ui.add_space(20.0);

        // Form fields
        egui::Grid::new("new_ber_form")
            .num_columns(2)
            .spacing([40.0, 10.0])
            .show(ui, |ui| {
                // Name field
                ui.label("BER Name:");
                ui.text_edit_singleline(&mut app.new_ber_state.name);
                ui.end_row();

                // Date field
                ui.label("Date Assessed:");

                // Simple date input (you can enhance this with a date picker widget)
                let date_str = app.new_ber_state.date_str.clone();
                let mut temp_date = date_str.clone();
                ui.text_edit_singleline(&mut temp_date);

                if temp_date != date_str {
                    app.new_ber_state.date_str = temp_date.clone();
                    // Try to parse the date
                    if let Ok(date) = NaiveDate::parse_from_str(&temp_date, "%Y-%m-%d") {
                        app.new_ber_state.date_assessed = Some(date);
                        app.new_ber_state.date_error = None;
                    } else {
                        app.new_ber_state.date_error =
                            Some("Invalid date format (use YYYY-MM-DD)".to_string());
                    }
                }
                ui.end_row();

                // Show date format hint
                ui.label("");
                ui.label(egui::RichText::new("Format: YYYY-MM-DD").italics().small());
                ui.end_row();

                // Show error if any
                if let Some(error) = &app.new_ber_state.date_error {
                    ui.label("");
                    ui.label(egui::RichText::new(error).color(egui::Color32::RED));
                    ui.end_row();
                }
            });

        ui.add_space(30.0);

        // Create button
        let can_create = !app.new_ber_state.name.is_empty()
            && app.new_ber_state.date_assessed.is_some()
            && app.new_ber_state.date_error.is_none();

        ui.add_enabled_ui(can_create, |ui| {
            if ui.button("Create BER").clicked() {
                // Create the BER via app controller
                let _ber = app.app_controller.create_ber(
                    app.new_ber_state.name.clone(),
                    app.new_ber_state.date_assessed.unwrap(),
                );

                // Reset form
                app.new_ber_state = Default::default();

                // Navigate back to home
                app.navigate_to(Page::Home);
            }
        });

        if !can_create {
            ui.label(
                egui::RichText::new("Please fill in all fields")
                    .small()
                    .italics(),
            );
        }
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
