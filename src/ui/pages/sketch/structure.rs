//! The Structure-phase tool area — the swap-in for the Mapping categories when
//! the workspace is in the Structure phase.
//!
//! Layout: a global MODE row (Wall / Roof / Floor) picks which surface kind the
//! tools act on, then one global SELECT row (`Click` / `Carve` / `All` /
//! `Remaining` + `Deselect all`) selects surfaces of that kind. Exactly one mode
//! is active; the canvas shades the selected surfaces of that kind.
//!
//! Per-surface ATTRIBUTE editing (construction phase / type / build-up) is a
//! separate flow over the current selection — NOT page tools — so it lives
//! elsewhere; this panel is only the mode + selection tools.

use ber_model::ber::structural::SurfaceCounts;

use super::SketchEvent;
use crate::controller::app_controller::{SelectMode, SurfaceKind};
use crate::ui::pages::events::UiEvent;

/// The three MODES, in panel order.
const MODES: &[(SurfaceKind, &str)] = &[
    (SurfaceKind::Walls, "Walls"),
    (SurfaceKind::Roofs, "Roofs"),
    (SurfaceKind::Floors, "Floors"),
];

/// The global Select subtools, in panel order.
const SELECT_MODES: &[(SelectMode, &str)] = &[
    (SelectMode::Click, "Click"),
    (SelectMode::Carve, "Carve"),
    (SelectMode::All, "All"),
    (SelectMode::Remaining, "Remaining"),
];

/// Render the Structure tool area: the global MODE row (with per-kind counts) +
/// the global SELECT row. `kind` is the active mode (reads as selected);
/// `select` is the active Select subtool; `enabled` gates interaction (off on
/// the read-only All-storeys tab).
pub fn structure_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    kind: SurfaceKind,
    select: Option<SelectMode>,
    counts: SurfaceCounts,
    enabled: bool,
    on_event: &mut F,
) {
    ui.label(egui::RichText::new("Mode").size(11.0).weak());
    ui.horizontal_wrapped(|ui| {
        for (m, label) in MODES {
            let active = kind == *m;
            let count = count_for(*m, counts);
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(active, format!("{label}  ·  {count}")),
                )
                .clicked()
                && !active
            {
                on_event(UiEvent::Sketch(SketchEvent::SetStructureKind(*m)));
            }
        }
    });

    ui.add_space(2.0);
    ui.label(egui::RichText::new("Select").size(11.0).weak());
    ui.horizontal_wrapped(|ui| {
        for (mode, label) in SELECT_MODES {
            let active = select == Some(*mode);
            if ui
                .add_enabled(enabled, egui::Button::selectable(active, *label))
                .clicked()
            {
                // Toggle: clicking the active subtool deactivates it.
                let next = if active { None } else { Some(*mode) };
                on_event(UiEvent::Sketch(SketchEvent::SetStructureSelectMode(next)));
            }
        }
        // Clear the current selection without leaving the active Select subtool.
        if ui
            .add_enabled(enabled, egui::Button::new("Deselect all").small())
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::DeselectAllStructure));
        }
    });
}

fn count_for(kind: SurfaceKind, counts: SurfaceCounts) -> usize {
    match kind {
        SurfaceKind::Walls => counts.walls,
        SurfaceKind::Roofs => counts.roofs,
        SurfaceKind::Floors => counts.floors,
    }
}
