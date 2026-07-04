use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::{metre_input, text_input};
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
    /// Extensions, in order added. Capped at 4 (one row per `Extension1..4`
    /// variant). Each has a user nickname ("Back extension") + a build year.
    pub extensions: Vec<ExtensionInput>,
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
}

impl ExtensionInput {
    fn new(phase: ConstructionPhase) -> Self {
        Self {
            nickname: String::new(),
            year: String::new(),
            phase,
            last_valid_year: None,
            year_unknown: false,
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
        date_assessed: NaiveDate,
        phases: &ConstructionPhases,
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
                }
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
                if text_input(ui, &mut state.name, f32::INFINITY, "").changed {
                    on_event(UiEvent::KeyDetails(KeyDetailsEvent::Rename(
                        state.name.clone(),
                    )));
                }
                ui.end_row();

                ui.label("Date Assessed:");
                ui.add(DatePickerButton::new(date));
                ui.end_row();

                ui.label("Year of Construction:");
                if metre_input(ui, &mut state.construction_year, 80.0, "e.g. 1985").changed {
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
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &phases,
        );
        assert_eq!(state.extensions.len(), 1);
        let row = &state.extensions[0];
        assert_eq!(row.phase, ConstructionPhase::Extension2);
        assert!(row.year_unknown);
        assert!(row.year.is_empty());
        assert_eq!(row.last_valid_year, None);
        assert_eq!(row.nickname, "Side return");
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
            NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            &phases,
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
