use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::{metre_input, text_input};
use ber_model::ber::ber_struct::IntakeDetails;
use ber_model::ber::common::{ConstructionPhase, ConstructionPhases, Year};
use chrono::NaiveDate;
use egui::{RichText, Ui};
use egui_extras::DatePickerButton;

/// The extension phase variants, in canonical (lowest-first) order.
const EXTENSION_PHASES: [ConstructionPhase; 4] = [
    ConstructionPhase::Extension1,
    ConstructionPhase::Extension2,
    ConstructionPhase::Extension3,
    ConstructionPhase::Extension4,
];

/// Key-Details intents: every field edit writes through to the live record
/// (no Create/Save button — the workspace edits the record directly).
#[derive(Debug)]
pub enum KeyDetailsEvent {
    Rename(String),
    /// Property address — normally seeded from the intake booking form; editable
    /// here so a from-scratch BER can carry it too (the cloud push and the DEAP
    /// export both want it).
    SetAddress(String),
    /// Eircode — same story as the address.
    SetEircode(String),
    SetDateAssessed(NaiveDate),
    SetConstructionPhases(ConstructionPhases),
    /// Client-provided intake answers (heating, window/door years, insulation,
    /// MPRN, property type, reason) — the whole set, recomputed on any edit.
    SetIntake(IntakeDetails),
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
    /// Property address + eircode (on `Ber` itself, not `IntakeDetails`) —
    /// seeded by the booking-form sync, editable here for from-scratch BERs.
    pub address: String,
    pub eircode: String,
    pub date_assessed: Option<NaiveDate>,
    /// Year the house was built (→ `ConstructionPhase::Original`).
    pub construction_year: String,
    /// The "got proof" checkbox for the original build year — the assessor
    /// found sufficient online proof, so the client needn't evidence the date.
    pub construction_year_proof: bool,
    /// Extensions, in order added. Capped at 4 (one row per `Extension1..4`
    /// variant). Each has a user nickname ("Back extension") + a build year.
    pub extensions: Vec<ExtensionInput>,
    // --- Client intake answers (staging; not yet in the DEAP model) ---
    pub property_type: String,
    pub heating_system: String,
    pub window_year: String,
    /// The "unknown" checkbox for the window-installation year — mutually
    /// exclusive with a typed `window_year` (mirrors the extension-year row).
    pub window_year_unknown: bool,
    pub door_year: String,
    pub insulation_walls: bool,
    pub insulation_roof: bool,
    pub insulation_floor: bool,
    pub insulation_notes: String,
    pub mprn: String,
    pub reason: String,
    // --- On-site question answers (asked at the property; None = unanswered) ---
    pub builder_letter_possible: Option<bool>,
    pub architect_letter_extension: Option<bool>,
    pub architect_letter_original: Option<bool>,
    pub builder_letter_works: Option<bool>,
    pub works_evidence_available: Option<bool>,
}

/// One extension row on the form: a descriptive nickname, a year, and the
/// phase variant the row OWNS. The variant is assigned at row-add (lowest
/// free one) and stays with the row for its lifetime — rows are never
/// re-keyed, so session phase-region tags (which store the variant) can't
/// silently change year when another row is removed. Removing a row frees
/// its variant for the next add; gaps between rows are fine.
pub struct ExtensionInput {
    pub nickname: String,
    pub year: String,
    pub phase: ConstructionPhase,
    /// The most recent successfully-parsed year. A mid-edit unparseable year
    /// (e.g. backspacing "2008" to "200") keeps contributing this instead of
    /// transiently dropping the phase — which would persist on that keystroke
    /// and orphan any areas tagged with it. A row that never had a valid year
    /// contributes nothing.
    pub last_valid_year: Option<Year>,
    /// The "unknown" checkbox: the assessor designates the extension NOW and
    /// fills the real year in later. Mutually exclusive with the year box —
    /// ticking clears it (and the remembered year); typing a year unticks.
    /// While ticked the row commits the phase with the year UNKNOWN, which
    /// counts as answered (the phase is assignable, labelled "(unk)").
    pub year_unknown: bool,
    /// The "got proof" checkbox: the assessor found sufficient online proof of
    /// this extension's year, so the client needn't be asked to evidence it.
    pub proof: bool,
}

impl ExtensionInput {
    fn new(phase: ConstructionPhase) -> Self {
        Self {
            nickname: String::new(),
            year: String::new(),
            phase,
            last_valid_year: None,
            year_unknown: false,
            proof: false,
        }
    }
}

/// The lowest `Extension1..4` variant not already owned by a row, or `None`
/// when all four are taken (the add-row cap).
fn next_free_extension_phase(extensions: &[ExtensionInput]) -> Option<ConstructionPhase> {
    EXTENSION_PHASES
        .into_iter()
        .find(|v| !extensions.iter().any(|e| e.phase == *v))
}

/// Builds from this year on get the "architect letter possible for the
/// original build?" on-site question (recent enough that certification
/// documents plausibly exist).
const ARCHITECT_LETTER_YEAR: u32 = 2010;

/// A Yes/No answer pair for an on-site question. Clicking the selected answer
/// again clears it back to unanswered. Returns whether the answer changed.
fn yes_no_buttons(ui: &mut Ui, answer: &mut Option<bool>) -> bool {
    let mut changed = false;
    for (label, value) in [("Yes", true), ("No", false)] {
        let selected = *answer == Some(value);
        if ui.selectable_label(selected, label).clicked() {
            *answer = if selected { None } else { Some(value) };
            changed = true;
        }
    }
    changed
}

/// The row's display label, from its owned variant ("Extension 1" .. "4").
fn extension_label(phase: &ConstructionPhase) -> &'static str {
    match phase {
        ConstructionPhase::Extension1 => "Extension 1:",
        ConstructionPhase::Extension2 => "Extension 2:",
        ConstructionPhase::Extension3 => "Extension 3:",
        ConstructionPhase::Extension4 => "Extension 4:",
        _ => "Extension:",
    }
}

impl KeyDetailsState {
    /// Rebuild the form's backing strings from the loaded record: the Original
    /// phase's year → the construction-year field, each set Extension1..4 →
    /// an extension row (nickname + year).
    pub fn from_record(
        name: String,
        address: String,
        eircode: String,
        date_assessed: NaiveDate,
        phases: &ConstructionPhases,
        intake: &IntakeDetails,
    ) -> Self {
        let construction_year = phases
            .year_of(&ConstructionPhase::Original)
            .map(|y| y.to_string())
            .unwrap_or_default();
        // Each set extension keeps its ACTUAL variant (gaps allowed) — the
        // stored phase-region tags reference these variants, so reconstruction
        // must not re-key them. A defined-but-unknown year reconstructs as the
        // ticked checkbox with an empty year box.
        let extensions = EXTENSION_PHASES
            .iter()
            .filter(|v| phases.is_defined(v))
            .map(|v| {
                let year = phases.year_of(v);
                ExtensionInput {
                    nickname: phases.nickname_of(v).unwrap_or("").to_string(),
                    year: year.map(|y| y.to_string()).unwrap_or_default(),
                    phase: v.clone(),
                    last_valid_year: year,
                    year_unknown: phases.year_is_unknown(v),
                    proof: phases.has_year_proof(v),
                }
            })
            .collect();
        Self {
            name,
            address,
            eircode,
            date_assessed: Some(date_assessed),
            construction_year,
            construction_year_proof: phases.has_year_proof(&ConstructionPhase::Original),
            extensions,
            property_type: intake.property_type.clone(),
            heating_system: intake.heating_system.clone(),
            window_year: intake
                .window_year
                .map(|y| y.to_string())
                .unwrap_or_default(),
            window_year_unknown: intake.window_year_unknown,
            door_year: intake.door_year.map(|y| y.to_string()).unwrap_or_default(),
            insulation_walls: intake.insulation_walls,
            insulation_roof: intake.insulation_roof,
            insulation_floor: intake.insulation_floor,
            insulation_notes: intake.insulation_notes.clone(),
            mprn: intake.mprn.clone(),
            reason: intake.reason.clone(),
            builder_letter_possible: intake.builder_letter_possible,
            architect_letter_extension: intake.architect_letter_extension,
            architect_letter_original: intake.architect_letter_original,
            builder_letter_works: intake.builder_letter_works,
            works_evidence_available: intake.works_evidence_available,
        }
    }
}

/// Build the BER's construction phases from the form: the house year →
/// `Original`, each extension row → its OWN stable variant (assigned at
/// row-add, never re-keyed). A row with "unknown" ticked commits its phase
/// with the year UNKNOWN (designated now, filled in later). Otherwise a row
/// whose year field is mid-edit unparseable keeps its last valid year
/// (refreshed here on each successful parse); a row that never had one
/// contributes nothing.
fn collect_construction_phases(state: &mut KeyDetailsState) -> ConstructionPhases {
    let mut phases = ConstructionPhases::default();
    if let Some(y) = parse_year(&state.construction_year) {
        phases.set_year(ConstructionPhase::Original, y);
    }
    phases.set_year_proof(ConstructionPhase::Original, state.construction_year_proof);
    for ext in state.extensions.iter_mut() {
        if let Some(y) = parse_year(&ext.year) {
            ext.last_valid_year = Some(y);
        }
        if ext.year_unknown {
            phases.set_year_unknown(ext.phase.clone());
            phases.set_nickname(ext.phase.clone(), ext.nickname.clone());
        } else if let Some(y) = ext.last_valid_year {
            phases.set_year(ext.phase.clone(), y);
            phases.set_nickname(ext.phase.clone(), ext.nickname.clone());
        }
        phases.set_year_proof(ext.phase.clone(), ext.proof);
    }
    phases
}

/// Gather the client-intake staging fields off the form. Years parse leniently
/// (blank/garbage → `None`); everything else is taken as-is.
fn collect_intake(state: &KeyDetailsState) -> IntakeDetails {
    IntakeDetails {
        property_type: state.property_type.clone(),
        heating_system: state.heating_system.clone(),
        // "unknown" wins over any stale text: the year commits as None + the flag.
        window_year: if state.window_year_unknown {
            None
        } else {
            parse_year(&state.window_year)
        },
        window_year_unknown: state.window_year_unknown,
        door_year: parse_year(&state.door_year),
        insulation_walls: state.insulation_walls,
        insulation_roof: state.insulation_roof,
        insulation_floor: state.insulation_floor,
        insulation_notes: state.insulation_notes.clone(),
        mprn: state.mprn.clone(),
        reason: state.reason.clone(),
        builder_letter_possible: state.builder_letter_possible,
        architect_letter_extension: state.architect_letter_extension,
        architect_letter_original: state.architect_letter_original,
        builder_letter_works: state.builder_letter_works,
        works_evidence_available: state.works_evidence_available,
    }
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
                if text_input(ui, &mut state.name, f32::INFINITY, "").changed {
                    on_event(UiEvent::KeyDetails(KeyDetailsEvent::Rename(
                        state.name.clone(),
                    )));
                }
                ui.end_row();

                ui.label("Address:");
                if text_input(ui, &mut state.address, f32::INFINITY, "").changed {
                    on_event(UiEvent::KeyDetails(KeyDetailsEvent::SetAddress(
                        state.address.clone(),
                    )));
                }
                ui.end_row();

                ui.label("Eircode:");
                if text_input(ui, &mut state.eircode, 160.0, "e.g. A98 X7Y2").changed {
                    on_event(UiEvent::KeyDetails(KeyDetailsEvent::SetEircode(
                        state.eircode.clone(),
                    )));
                }
                ui.end_row();

                ui.label("Date Assessed:");
                ui.add(DatePickerButton::new(date));
                ui.end_row();

                ui.label("Year of Construction:");
                ui.horizontal(|ui| {
                    if metre_input(ui, &mut state.construction_year, 80.0, "e.g. 1985").changed {
                        phases_changed = true;
                    }
                    // Assessor ticks this when there's sufficient online proof of
                    // the date, so they needn't ask the client for documentation.
                    if ui
                        .checkbox(&mut state.construction_year_proof, "got proof")
                        .changed()
                    {
                        phases_changed = true;
                    }
                });
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
                ui.label(extension_label(&ext.phase));
                if text_input(ui, &mut ext.nickname, 160.0, "name e.g. Back extension").changed {
                    phases_changed = true;
                }
                ui.label("year");
                if metre_input(ui, &mut ext.year, 60.0, "2008").changed {
                    // Typing owns the field — it unticks "unknown" (the two
                    // are mutually exclusive, like the insulation row).
                    ext.year_unknown = false;
                    phases_changed = true;
                }
                if ui.checkbox(&mut ext.year_unknown, "unknown").changed() {
                    if ext.year_unknown {
                        // Ticking clears the box (and the remembered year):
                        // the phase commits as year-UNKNOWN, to fill in later.
                        ext.year.clear();
                        ext.last_valid_year = None;
                    }
                    phases_changed = true;
                }
                if ui.checkbox(&mut ext.proof, "got proof").changed() {
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
        // Cap = no free Extension1..4 variant left. A new row takes the lowest
        // free variant (one freed by a remove is reused; existing rows keep
        // theirs).
        if let Some(free) = next_free_extension_phase(&state.extensions) {
            if ui.button("+ Add extension").clicked() {
                state.extensions.push(ExtensionInput::new(free));
                // A fresh row has no valid year yet — nothing to write through.
            }
        }

        ui.add_space(20.0);

        // Client-provided answers from the booking form. Prefilled at intake and
        // editable here so the assessor can confirm them or fill any blanks. Not
        // yet wired into the DEAP model — a staging area to transcribe from.
        ui.label(RichText::new("Property details (from booking form)").strong());
        ui.label(
            RichText::new("Prefilled from what the client entered — edit or fill any blanks.")
                .weak(),
        );
        ui.add_space(4.0);
        let mut intake_changed = false;
        egui::Grid::new("key_details_intake")
            .num_columns(2)
            .spacing([40.0, 10.0])
            .show(ui, |ui| {
                ui.label("Property type:");
                if text_input(ui, &mut state.property_type, 220.0, "e.g. semi-detached").changed {
                    intake_changed = true;
                }
                ui.end_row();

                ui.label("Heating system:");
                if text_input(ui, &mut state.heating_system, 220.0, "e.g. gas boiler").changed {
                    intake_changed = true;
                }
                ui.end_row();

                ui.label("Windows installed (year):");
                ui.horizontal(|ui| {
                    if metre_input(ui, &mut state.window_year, 80.0, "e.g. 2010").changed {
                        // Typing owns the field — it unticks "unknown" (mutually
                        // exclusive, like the extension-year row).
                        state.window_year_unknown = false;
                        intake_changed = true;
                    }
                    if ui
                        .checkbox(&mut state.window_year_unknown, "unknown")
                        .changed()
                    {
                        if state.window_year_unknown {
                            // Ticking clears the box: commits as year-unknown.
                            state.window_year.clear();
                        }
                        intake_changed = true;
                    }
                });
                ui.end_row();

                ui.label("Doors installed (year):");
                if metre_input(ui, &mut state.door_year, 80.0, "e.g. 2010").changed {
                    intake_changed = true;
                }
                ui.end_row();

                ui.label("Insulation:");
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut state.insulation_walls, "Walls").changed() {
                        intake_changed = true;
                    }
                    if ui.checkbox(&mut state.insulation_roof, "Roof").changed() {
                        intake_changed = true;
                    }
                    if ui.checkbox(&mut state.insulation_floor, "Floor").changed() {
                        intake_changed = true;
                    }
                });
                ui.end_row();

                ui.label("Insulation notes:");
                if text_input(ui, &mut state.insulation_notes, f32::INFINITY, "").changed {
                    intake_changed = true;
                }
                ui.end_row();

                ui.label("MPRN:");
                if text_input(ui, &mut state.mprn, 220.0, "electricity meter number").changed {
                    intake_changed = true;
                }
                ui.end_row();

                ui.label("Reason for BER:");
                if text_input(ui, &mut state.reason, 220.0, "sale / rental / grant / …").changed {
                    intake_changed = true;
                }
                ui.end_row();
            });
        // On-site questions: posed only while their trigger holds (insulation
        // works ticked / an extension row / a recent build). A hidden question
        // keeps its stored answer — unticking the trigger doesn't erase it.
        let insulation_ticked =
            state.insulation_walls || state.insulation_roof || state.insulation_floor;
        let has_extension = !state.extensions.is_empty();
        let recent_build =
            parse_year(&state.construction_year).is_some_and(|y| y >= ARCHITECT_LETTER_YEAR);
        if insulation_ticked || has_extension || recent_build {
            ui.add_space(20.0);
            ui.label(RichText::new("On-site questions").strong());
            ui.label(RichText::new("Ask the client while at the property.").weak());
            ui.add_space(4.0);
            egui::Grid::new("key_details_questions")
                .num_columns(2)
                .spacing([40.0, 10.0])
                .show(ui, |ui| {
                    if insulation_ticked {
                        ui.label("Is a letter from the builder possible?");
                        ui.horizontal(|ui| {
                            if yes_no_buttons(ui, &mut state.builder_letter_possible) {
                                intake_changed = true;
                            }
                        });
                        ui.end_row();

                        ui.label(
                            "Do they have invoices/photos of the works being done showing the \
                             materials and the areas insulated?",
                        );
                        ui.horizontal(|ui| {
                            if yes_no_buttons(ui, &mut state.works_evidence_available) {
                                intake_changed = true;
                            }
                        });
                        ui.end_row();
                    }
                    if has_extension {
                        ui.label("Is a letter from the architect available for the extension?");
                        ui.horizontal(|ui| {
                            if yes_no_buttons(ui, &mut state.architect_letter_extension) {
                                intake_changed = true;
                            }
                        });
                        ui.end_row();
                    }
                    if recent_build {
                        ui.label("Is a letter from the architect possible for the original build?");
                        ui.horizontal(|ui| {
                            if yes_no_buttons(ui, &mut state.architect_letter_original) {
                                intake_changed = true;
                            }
                        });
                        ui.end_row();
                    }
                    ui.label(
                        "Is a letter from the builder available for the works done in the house?",
                    );
                    ui.horizontal(|ui| {
                        if yes_no_buttons(ui, &mut state.builder_letter_works) {
                            intake_changed = true;
                        }
                    });
                    ui.end_row();
                });
        }

        if intake_changed {
            on_event(UiEvent::KeyDetails(KeyDetailsEvent::SetIntake(
                collect_intake(state),
            )));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn ext(phase: ConstructionPhase, year: &str) -> ExtensionInput {
        let mut e = ExtensionInput::new(phase);
        e.year = year.to_string();
        e
    }

    fn state_with(extensions: Vec<ExtensionInput>) -> KeyDetailsState {
        KeyDetailsState {
            construction_year: "1985".to_string(),
            extensions,
            ..Default::default()
        }
    }

    /// Removing a middle row must NOT re-key the later rows: each row owns its
    /// variant, so Extension3 stays Extension3 (year 2010) and Extension2 is
    /// simply gone.
    #[test]
    fn removing_middle_row_keeps_later_variants_stable() {
        let mut state = state_with(vec![
            ext(ConstructionPhase::Extension1, "2000"),
            ext(ConstructionPhase::Extension2, "2005"),
            ext(ConstructionPhase::Extension3, "2010"),
        ]);
        state.extensions.remove(1); // the UI's Remove button

        let phases = collect_construction_phases(&mut state);
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2000));
        assert_eq!(phases.year_of(&ConstructionPhase::Extension2), None);
        assert_eq!(phases.year_of(&ConstructionPhase::Extension3), Some(2010));

        // The freed variant is the one the next add reuses; rows keep theirs.
        assert_eq!(
            next_free_extension_phase(&state.extensions),
            Some(ConstructionPhase::Extension2)
        );
    }

    /// A transiently-unparseable year (mid-edit) keeps the last valid year
    /// instead of dropping the phase on that keystroke.
    #[test]
    fn invalid_mid_edit_year_keeps_last_valid_year() {
        let mut state = state_with(vec![ext(ConstructionPhase::Extension1, "2008")]);
        // First edit pass: valid year parsed and remembered.
        let phases = collect_construction_phases(&mut state);
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2008));

        // The user backspaces to an unparseable value.
        state.extensions[0].year = "200".to_string();
        let phases = collect_construction_phases(&mut state);
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2008));

        // Finishing the edit with a new valid year replaces it.
        state.extensions[0].year = "2012".to_string();
        let phases = collect_construction_phases(&mut state);
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2012));
    }

    /// A row that never had a valid year contributes nothing (as before).
    #[test]
    fn never_valid_row_contributes_nothing() {
        let mut state = state_with(vec![ext(ConstructionPhase::Extension1, "garbage")]);
        let phases = collect_construction_phases(&mut state);
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), None);
    }

    /// The "unknown" checkbox commits the row's phase with the year UNKNOWN:
    /// defined (so it's assignable — the gate counts it as answered), no known
    /// year, labelled "(unk)".
    #[test]
    fn unknown_checkbox_commits_phase_with_unknown_year() {
        let mut e = ext(ConstructionPhase::Extension1, "");
        e.nickname = "Back extension".to_string();
        e.year_unknown = true;
        let mut state = state_with(vec![e]);
        let phases = collect_construction_phases(&mut state);
        assert!(phases.is_defined(&ConstructionPhase::Extension1));
        assert!(phases.year_is_unknown(&ConstructionPhase::Extension1));
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), None);
        // `state_with` also sets the Original year, so check membership: the
        // unknown-year extension is offered alongside it.
        assert!(
            phases
                .defined_phases()
                .contains(&ConstructionPhase::Extension1),
            "unknown counts as answered — the phase is offered for tagging"
        );
        assert_eq!(
            phases.display_label(&ConstructionPhase::Extension1),
            "Back extension (unk)"
        );
    }

    /// Ticking "unknown" clears the year (box + remembered year) — the mutual
    /// exclusion the render enforces — and a later typed year (which unticks)
    /// replaces the unknown with the real year.
    #[test]
    fn unknown_then_typed_year_fills_in_later() {
        let mut state = state_with(vec![ext(ConstructionPhase::Extension1, "2008")]);
        let phases = collect_construction_phases(&mut state);
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2008));

        // Tick: the render clears the box + remembered year.
        state.extensions[0].year_unknown = true;
        state.extensions[0].year.clear();
        state.extensions[0].last_valid_year = None;
        let phases = collect_construction_phases(&mut state);
        assert!(phases.year_is_unknown(&ConstructionPhase::Extension1));

        // The assessor learns the year later and types it (which unticks).
        state.extensions[0].year = "2012".to_string();
        state.extensions[0].year_unknown = false;
        let phases = collect_construction_phases(&mut state);
        assert!(!phases.year_is_unknown(&ConstructionPhase::Extension1));
        assert_eq!(phases.year_of(&ConstructionPhase::Extension1), Some(2012));
    }

    /// `from_record` reconstructs an unknown-year extension as a row with the
    /// checkbox ticked and an empty year box — so reloading a record keeps the
    /// designation editable.
    #[test]
    fn from_record_reconstructs_unknown_year_row() {
        let mut phases = ConstructionPhases::default();
        phases.set_year(ConstructionPhase::Original, 1985);
        phases.set_year_unknown(ConstructionPhase::Extension2);
        phases.set_nickname(ConstructionPhase::Extension2, "Side return".to_string());

        let state = KeyDetailsState::from_record(
            "test".to_string(),
            String::new(),
            String::new(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &phases,
            &IntakeDetails::default(),
        );
        assert_eq!(state.extensions.len(), 1);
        let row = &state.extensions[0];
        assert_eq!(row.phase, ConstructionPhase::Extension2);
        assert!(row.year_unknown);
        assert!(row.year.is_empty());
        assert_eq!(row.last_valid_year, None);
        assert_eq!(row.nickname, "Side return");
    }

    /// On-site question answers survive the record → form → record round trip
    /// (from_record picks them up, collect_intake writes them back), including
    /// the unanswered `None` state.
    #[test]
    fn on_site_answers_round_trip_through_intake() {
        let intake = IntakeDetails {
            builder_letter_possible: Some(true),
            architect_letter_extension: Some(false),
            architect_letter_original: None,
            ..Default::default()
        };
        let state = KeyDetailsState::from_record(
            "test".to_string(),
            String::new(),
            String::new(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &ConstructionPhases::default(),
            &intake,
        );
        assert_eq!(state.builder_letter_possible, Some(true));
        assert_eq!(state.architect_letter_extension, Some(false));
        assert_eq!(state.architect_letter_original, None);
        assert_eq!(collect_intake(&state), intake);
    }

    /// The builder-letter-for-works and works-evidence (invoices/photos) on-site
    /// answers round-trip through intake alongside the existing questions.
    #[test]
    fn builder_letter_works_round_trips_through_intake() {
        let intake = IntakeDetails {
            builder_letter_works: Some(true),
            works_evidence_available: Some(false),
            ..Default::default()
        };
        let state = KeyDetailsState::from_record(
            "test".to_string(),
            String::new(),
            String::new(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &ConstructionPhases::default(),
            &intake,
        );
        assert_eq!(state.builder_letter_works, Some(true));
        assert_eq!(state.works_evidence_available, Some(false));
        assert_eq!(collect_intake(&state), intake);
    }

    /// The "got proof" checkbox round-trips per phase: the original year and
    /// each extension carry their own proof flag through the record → form →
    /// record trip, independently of the year value.
    #[test]
    fn year_proof_round_trips_per_phase() {
        let mut phases = ConstructionPhases::default();
        phases.set_year(ConstructionPhase::Original, 1990);
        phases.set_year_proof(ConstructionPhase::Original, true);
        // An extension with proof but its year still marked unknown.
        phases.set_year_unknown(ConstructionPhase::Extension1);
        phases.set_nickname(ConstructionPhase::Extension1, "Back".to_string());
        phases.set_year_proof(ConstructionPhase::Extension1, true);
        // A second extension WITHOUT proof.
        phases.set_year(ConstructionPhase::Extension2, 2005);

        let mut state = KeyDetailsState::from_record(
            "test".to_string(),
            String::new(),
            String::new(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &phases,
            &IntakeDetails::default(),
        );
        assert!(state.construction_year_proof);
        let ext1 = state
            .extensions
            .iter()
            .find(|e| e.phase == ConstructionPhase::Extension1)
            .unwrap();
        assert!(ext1.proof);
        let ext2 = state
            .extensions
            .iter()
            .find(|e| e.phase == ConstructionPhase::Extension2)
            .unwrap();
        assert!(!ext2.proof);

        let rebuilt = collect_construction_phases(&mut state);
        assert!(rebuilt.has_year_proof(&ConstructionPhase::Original));
        assert!(rebuilt.has_year_proof(&ConstructionPhase::Extension1));
        assert!(!rebuilt.has_year_proof(&ConstructionPhase::Extension2));
    }

    /// The window-year "unknown" checkbox round-trips: ticking it commits
    /// `window_year = None` + `window_year_unknown = true` (distinct from a
    /// blank year), and a stale typed year is dropped when unknown wins.
    #[test]
    fn window_year_unknown_round_trips_and_overrides_a_typed_year() {
        // Unknown ticked with leftover text → commits None + the flag.
        let mut state = KeyDetailsState {
            window_year: "2010".to_string(),
            window_year_unknown: true,
            ..Default::default()
        };
        let intake = collect_intake(&state);
        assert_eq!(intake.window_year, None);
        assert!(intake.window_year_unknown);

        // The flag survives the record → form → record round trip.
        let back = KeyDetailsState::from_record(
            "t".to_string(),
            String::new(),
            String::new(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &ConstructionPhases::default(),
            &intake,
        );
        assert!(back.window_year_unknown);
        assert!(back.window_year.is_empty());

        // A real typed year (no unknown) still commits normally.
        state.window_year_unknown = false;
        state.window_year = "2010".to_string();
        let intake2 = collect_intake(&state);
        assert_eq!(intake2.window_year, Some(2010));
        assert!(!intake2.window_year_unknown);
    }

    /// All four variants taken → no free variant → the add button's cap.
    #[test]
    fn cap_is_no_free_variant() {
        let exts: Vec<ExtensionInput> = EXTENSION_PHASES
            .into_iter()
            .map(|p| ext(p, "2000"))
            .collect();
        assert_eq!(next_free_extension_phase(&exts), None);
        assert_eq!(
            next_free_extension_phase(&exts[..3]),
            Some(ConstructionPhase::Extension4)
        );
    }

    /// `from_record` reconstructs rows with their ACTUAL variants — gaps
    /// allowed (e.g. Extension1 + Extension3 set, Extension2 free).
    #[test]
    fn from_record_preserves_variant_gaps() {
        let mut phases = ConstructionPhases::default();
        phases.set_year(ConstructionPhase::Original, 1985);
        phases.set_year(ConstructionPhase::Extension1, 2000);
        phases.set_year(ConstructionPhase::Extension3, 2010);
        phases.set_nickname(ConstructionPhase::Extension3, "Attic".to_string());

        let state = KeyDetailsState::from_record(
            "test".to_string(),
            String::new(),
            String::new(),
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &phases,
            &IntakeDetails::default(),
        );
        assert_eq!(state.extensions.len(), 2);
        assert_eq!(state.extensions[0].phase, ConstructionPhase::Extension1);
        assert_eq!(state.extensions[0].last_valid_year, Some(2000));
        assert_eq!(state.extensions[1].phase, ConstructionPhase::Extension3);
        assert_eq!(state.extensions[1].nickname, "Attic");
        assert_eq!(
            next_free_extension_phase(&state.extensions),
            Some(ConstructionPhase::Extension2)
        );
    }
}
