use crate::ui::pages::events::UiEvent;
use chrono::NaiveDate;
use datatypes::ber::common::{ConstructionPhase, ConstructionPhases};
use egui::{RichText, Ui};
use egui_extras::DatePickerButton;

/// Key-Details intents: every field edit writes through to the live record
/// (no Create/Save button — the workspace edits the record directly).
#[derive(Debug)]
pub enum KeyDetailsEvent {
    Rename(String),
    SetDateAssessed(NaiveDate),
    SetConstructionPhases(ConstructionPhases),
}

/// Parse a year string into a plausible construction year (1700–2100), or
/// `None` if it's empty/garbage/out of range.
pub fn parse_year(s: &str) -> Option<u32> {
    s.trim()
        .parse::<u32>()
        .ok()
        .filter(|y| (1700..=2100).contains(y))
}

/// Backing strings for the Key Details form, owned by `BerApp`. Pre-populated
/// from the loaded record on workspace entry (`from_record`); thereafter the
/// user's edits flow record-ward via `KeyDetailsEvent`s.
#[derive(Default)]
pub struct KeyDetailsState {
    pub name: String,
    pub date_assessed: Option<NaiveDate>,
    /// Year the house was built (→ `ConstructionPhase::Original`).
    pub construction_year: String,
    /// Extensions (→ `Extension1..4`), in order added. Capped at 4. Each has a
    /// user nickname ("Back extension") + a build year.
    pub extensions: Vec<ExtensionInput>,
}

/// One extension row on the form: a descriptive nickname and a year.
#[derive(Default)]
pub struct ExtensionInput {
    pub nickname: String,
    pub year: String,
}

impl KeyDetailsState {
    /// Rebuild the form's backing strings from the loaded record: the Original
    /// phase's year → the construction-year field, each set Extension1..4 →
    /// an extension row (nickname + year).
    pub fn from_record(
        name: String,
        date_assessed: NaiveDate,
        phases: &ConstructionPhases,
    ) -> Self {
        let construction_year = phases
            .year_of(&ConstructionPhase::Original)
            .map(|y| y.to_string())
            .unwrap_or_default();
        let extensions = [
            ConstructionPhase::Extension1,
            ConstructionPhase::Extension2,
            ConstructionPhase::Extension3,
            ConstructionPhase::Extension4,
        ]
        .iter()
        .filter_map(|v| {
            phases.year_of(v).map(|y| ExtensionInput {
                nickname: phases.nickname_of(v).unwrap_or("").to_string(),
                year: y.to_string(),
            })
        })
        .collect();
        Self {
            name,
            date_assessed: Some(date_assessed),
            construction_year,
            extensions,
        }
    }
}

/// Build the BER's construction phases from the form: the house year →
/// `Original`, each valid extension year → `Extension1..4`. Unparseable/blank
/// entries are skipped.
fn collect_construction_phases(state: &KeyDetailsState) -> ConstructionPhases {
    let mut phases = ConstructionPhases::default();
    if let Some(y) = parse_year(&state.construction_year) {
        phases.set_year(ConstructionPhase::Original, y);
    }
    let variants = [
        ConstructionPhase::Extension1,
        ConstructionPhase::Extension2,
        ConstructionPhase::Extension3,
        ConstructionPhase::Extension4,
    ];
    for (variant, ext) in variants.into_iter().zip(state.extensions.iter()) {
        if let Some(y) = parse_year(&ext.year) {
            phases.set_year(variant.clone(), y);
            phases.set_nickname(variant, ext.nickname.clone());
        }
    }
    phases
}

pub struct KeyDetailsPage;

impl KeyDetailsPage {
    pub fn render<F>(ui: &mut Ui, state: &mut KeyDetailsState, mut on_event: F)
    where
        F: FnMut(UiEvent),
    {
        ui.heading("Key Details");
        ui.add_space(20.0);

        // Any construction-year-related edit (year, extension fields, row
        // add/remove) recomputes the whole phase set once at the end.
        let mut phases_changed = false;

        // Default the date to today if the record somehow had none.
        let date = state
            .date_assessed
            .get_or_insert_with(|| chrono::Local::now().naive_local().date());
        let date_before = *date;

        egui::Grid::new("key_details_form")
            .num_columns(2)
            .spacing([40.0, 10.0])
            .show(ui, |ui| {
                ui.label("BER Name:");
                if ui.text_edit_singleline(&mut state.name).changed() {
                    on_event(UiEvent::KeyDetails(KeyDetailsEvent::Rename(
                        state.name.clone(),
                    )));
                }
                ui.end_row();

                ui.label("Date Assessed:");
                ui.add(DatePickerButton::new(date));
                ui.end_row();

                ui.label("Year of Construction:");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut state.construction_year)
                            .hint_text("e.g. 1985")
                            .desired_width(80.0),
                    )
                    .changed()
                {
                    phases_changed = true;
                }
                ui.end_row();
            });

        if *date != date_before {
            on_event(UiEvent::KeyDetails(KeyDetailsEvent::SetDateAssessed(*date)));
        }

        ui.add_space(10.0);

        // Extensions: each adds a selectable construction phase (Extension1..4)
        // with a descriptive nickname (shown in the UI) + its own build year.
        // Capped at 4 — matching the phase enum.
        ui.label(RichText::new("Extensions").strong());
        let mut remove: Option<usize> = None;
        for (i, ext) in state.extensions.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.label(format!("Extension {}:", i + 1));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut ext.nickname)
                            .hint_text("name e.g. Back extension")
                            .desired_width(160.0),
                    )
                    .changed()
                {
                    phases_changed = true;
                }
                ui.label("year");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut ext.year)
                            .hint_text("2008")
                            .desired_width(60.0),
                    )
                    .changed()
                {
                    phases_changed = true;
                }
                if ui.button("Remove").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            state.extensions.remove(i);
            phases_changed = true;
        }
        if state.extensions.len() < 4 && ui.button("+ Add extension").clicked() {
            state.extensions.push(ExtensionInput::default());
            // A fresh row has no valid year yet — nothing to write through.
        }

        ui.add_space(20.0);

        if state.name.trim().is_empty() {
            ui.label(
                RichText::new("Please enter a name").color(egui::Color32::from_rgb(220, 80, 80)),
            );
        } else if parse_year(&state.construction_year).is_none() {
            ui.label(
                RichText::new("Please enter a valid year of construction")
                    .color(egui::Color32::from_rgb(220, 80, 80)),
            );
        }

        if phases_changed {
            on_event(UiEvent::KeyDetails(KeyDetailsEvent::SetConstructionPhases(
                collect_construction_phases(state),
            )));
        }
    }
}
