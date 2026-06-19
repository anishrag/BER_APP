//! The Structure-phase tool area — the swap-in for the Mapping categories when
//! the workspace is in the Structure phase.
//!
//! Layout: each surface KIND (Walls / Roofs / Floors) is a collapsible HEADING.
//! Under it sit two kinds of subtool:
//!
//! - **Select** subtools (`Click` / `Carve` / `All` / `Remaining`) plus a
//!   `Deselect all` button: pick which surfaces of that kind are in scope.
//!   Exactly one Select subtool is active across the whole panel; the canvas
//!   shades the selected surfaces. `Click` taps surfaces one at a time; `Carve`
//!   draws a cut (the living-area flow) and selects the surfaces inside the
//!   resolved region; `All`/`Remaining` fill the selection (`Remaining` =
//!   `All` until its criteria are wired in).
//! - **Set** subtools, grouped under their own headings (Construction phase /
//!   Wall type / …): the per-region properties to stamp onto the selection.
//!   These are PLACEHOLDERS today — they render but do nothing.

use datatypes::ber::structural::SurfaceCounts;

use super::SketchEvent;
use crate::controller::app_controller::{SelectMode, StructureSelection, SurfaceKind};
use crate::ui::pages::events::UiEvent;

/// A group of Set placeholder buttons under its own sub-heading.
struct SetGroup {
    heading: &'static str,
    options: &'static [&'static str],
}

/// One surface kind's section: the heading + its Set groups. (The Select
/// subtools are the same three for every kind, so they're not in the spec.)
struct SurfaceSpec {
    kind: SurfaceKind,
    label: &'static str,
    sets: &'static [SetGroup],
}

/// Placeholder Set toolset per surface kind — the properties to set on each
/// region. Values are stand-ins so the panel shape is real; wiring comes later.
const SURFACES: &[SurfaceSpec] = &[
    SurfaceSpec {
        kind: SurfaceKind::Walls,
        label: "Walls",
        sets: &[
            SetGroup {
                heading: "Construction phase",
                options: &["Existing", "Extension", "New build"],
            },
            SetGroup {
                heading: "Wall type",
                options: &["Cavity", "Filled cavity", "Hollow block", "Timber", "ICF"],
            },
            SetGroup {
                heading: "Build-up",
                options: &["Default", "Non-heat-loss"],
            },
        ],
    },
    SurfaceSpec {
        kind: SurfaceKind::Roofs,
        label: "Roofs",
        sets: &[
            SetGroup {
                heading: "Construction phase",
                options: &["Existing", "Extension", "New build"],
            },
            SetGroup {
                heading: "Roof type",
                options: &[
                    "Flat",
                    "Sloped (rafter)",
                    "Sloped (ceiling)",
                    "Sloped (both)",
                    "Non-heat-loss",
                ],
            },
            SetGroup {
                heading: "Build-up",
                options: &["Default", "Non-heat-loss"],
            },
        ],
    },
    SurfaceSpec {
        kind: SurfaceKind::Floors,
        label: "Floors",
        sets: &[
            SetGroup {
                heading: "Construction phase",
                options: &["Existing", "Extension", "New build"],
            },
            SetGroup {
                heading: "Floor type",
                options: &["Solid", "Suspended", "Exposed", "Non-heat-loss"],
            },
            SetGroup {
                heading: "Build-up",
                options: &["Default", "Non-heat-loss"],
            },
        ],
    },
];

/// The Select subtools, in panel order.
const SELECT_MODES: &[(SelectMode, &str)] = &[
    (SelectMode::Click, "Click"),
    (SelectMode::Carve, "Carve"),
    (SelectMode::All, "All"),
    (SelectMode::Remaining, "Remaining"),
];

/// Render the Structure tool area into `ui` (the ribbon slot in portrait, the
/// side panel in landscape). `selection` is the active Select subtool (drives
/// which buttons read as selected); `counts` shows per-kind element counts on
/// the headings; `enabled` gates interaction (off on the read-only All tab).
pub fn structure_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    selection: Option<StructureSelection>,
    counts: SurfaceCounts,
    enabled: bool,
    on_event: &mut F,
) {
    for spec in SURFACES {
        let count = count_for(spec.kind, counts);
        egui::CollapsingHeader::new(format!("{}  ·  {count}", spec.label))
            .default_open(true)
            .show(ui, |ui| {
                select_row(ui, spec.kind, selection, enabled, on_event);
                for group in spec.sets {
                    set_group(ui, group, enabled);
                }
            });
    }
}

/// The Select / Select all / Select remaining row for one surface kind. Each is
/// a toggle: clicking the active one clears the selection (un-shades).
fn select_row<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    kind: SurfaceKind,
    selection: Option<StructureSelection>,
    enabled: bool,
    on_event: &mut F,
) {
    ui.label(egui::RichText::new("Select").size(11.0).weak());
    ui.horizontal_wrapped(|ui| {
        for (mode, label) in SELECT_MODES {
            let active = selection == Some(StructureSelection { kind, mode: *mode });
            if ui
                .add_enabled(enabled, egui::Button::selectable(active, *label))
                .clicked()
            {
                let next = if active {
                    None
                } else {
                    Some(StructureSelection { kind, mode: *mode })
                };
                on_event(UiEvent::Sketch(SketchEvent::SetStructureSelection(next)));
            }
        }
        // Clear the current selection without leaving the Select tool.
        if ui
            .add_enabled(enabled, egui::Button::new("Deselect all").small())
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::DeselectAllStructure));
        }
    });
}

/// A Set sub-heading with its row of placeholder value buttons. The buttons do
/// nothing yet (the set logic is wired in a later step).
fn set_group(ui: &mut egui::Ui, group: &SetGroup, enabled: bool) {
    ui.add_space(2.0);
    ui.label(egui::RichText::new(group.heading).size(11.0).weak());
    ui.horizontal_wrapped(|ui| {
        for option in group.options {
            // Placeholder: rendered but inert until the set events are wired.
            let _ = ui.add_enabled(enabled, egui::Button::new(*option).small());
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
