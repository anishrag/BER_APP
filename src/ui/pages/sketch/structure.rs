//! The Structure-phase tool area — the swap-in for the Mapping categories when
//! the workspace is in the Structure phase.
//!
//! Layout: a global MODE row (Wall / Roof / Floor) picks which surface kind the
//! tools act on, then one global SELECT row (`Click` / `Carve` / `All` /
//! `Remaining` + `Deselect all`) selects surfaces of that kind. An "Edit
//! attributes" button opens a per-construction GROUP editor over the selection:
//! the selection is partitioned by shared construction, and each group's
//! `(type, build-up)` is editable separately (one group active at a time). The
//! phase axis stays derived from the floor zones (shown read-only). Works for
//! every kind — walls key construction by parametric `SurfaceRef`, floors and
//! ceilings/roofs by region centroid.

use ber_model::ber::structural::{
    CeilingConstructionType, ConstructionMakeup, ConstructionType, FloorConstructionType,
    SurfaceCounts, WallConstructionType,
};

use super::SketchEvent;
use crate::controller::app_controller::{EditGroup, SelectMode, StructureEdit, SurfaceKind};
use crate::ui::pages::events::UiEvent;

/// Font size for the per-group attribute editor (the floating windows + the
/// exploded-view side panel). Larger than the tool-row labels so the editable
/// controls read clearly at the zoomed-out edit scale.
const EDITOR_FONT: f32 = 13.5;
/// Width of the editable group-name field in the attribute editor.
const EDIT_NAME_W: f32 = 130.0;

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

/// Wall construction TYPE options (the group editor).
const WALL_TYPES: &[(WallConstructionType, &str)] = &[
    (WallConstructionType::CavityWall, "Cavity"),
    (WallConstructionType::FilledCavityWall, "Filled cavity"),
    (WallConstructionType::ConcreteHollowBlocks, "Hollow block"),
    (WallConstructionType::TimberConstruction, "Timber"),
    (WallConstructionType::ICF, "ICF"),
];

/// Floor construction TYPE options (the group editor).
const FLOOR_TYPES: &[(FloorConstructionType, &str)] = &[
    (FloorConstructionType::SolidFloor, "Solid"),
    (FloorConstructionType::SuspendedFloor, "Suspended"),
    (FloorConstructionType::ExposedSemiExposedFloor, "Exposed"),
    (FloorConstructionType::NonHeatLossFloor, "Non-heat-loss"),
    (FloorConstructionType::Other, "Other"),
];

/// Ceiling/roof construction TYPE options (the group editor).
const CEILING_TYPES: &[(CeilingConstructionType, &str)] = &[
    (CeilingConstructionType::FlatRoof, "Flat"),
    (CeilingConstructionType::SlopedInsOnRafter, "Ins. on rafter"),
    (
        CeilingConstructionType::SlopedInsOnCeiling,
        "Ins. on ceiling",
    ),
    (
        CeilingConstructionType::SlopedInsOnCeilingAndRafter,
        "Ins. both",
    ),
    (CeilingConstructionType::NonHeatLossCeiling, "Non-heat-loss"),
    (CeilingConstructionType::Other, "Other"),
];

/// Render the Structure tool area: the global MODE row + the global SELECT row +
/// the "Edit attributes" group editor (when a session is open). `kind` is the
/// active mode; `select` the active Select subtool; `edit` the in-flight session;
/// `enabled` gates interaction.
pub fn structure_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    kinds: &std::collections::BTreeSet<SurfaceKind>,
    select: Option<SelectMode>,
    edit: Option<&StructureEdit>,
    counts: SurfaceCounts,
    enabled: bool,
    on_event: &mut F,
) {
    ui.label(egui::RichText::new("Mode").size(11.0).weak());
    ui.horizontal_wrapped(|ui| {
        // The mode buttons are NON-EXCLUSIVE — each toggles its kind in/out of
        // the active set, so the assessor can work several at once (Walls +
        // Floors). The event toggles; the controller keeps ≥1 active.
        for (m, label) in MODES {
            let active = kinds.contains(m);
            let count = count_for(*m, counts);
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(active, format!("{label}  ·  {count}")),
                )
                .clicked()
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
                let next = if active { None } else { Some(*mode) };
                on_event(UiEvent::Sketch(SketchEvent::SetStructureSelectMode(next)));
            }
        }
        if ui
            .add_enabled(enabled, egui::Button::new("Deselect all").small())
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::DeselectAllStructure));
        }
    });

    ui.add_space(4.0);
    ui.separator();
    match edit {
        // No session: offer to open one (any kind — walls / floors / roofs).
        None => {
            if ui
                .add_enabled(enabled, egui::Button::new("Edit attributes"))
                .on_hover_text("Set construction for the selected surfaces")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::BeginStructureEdit));
            }
        }
        Some(e) => edit_session(ui, e, enabled, on_event),
    }
}

/// The side-panel summary for an open "Edit attributes" session. In the default
/// (in-place) view the editing happens in the floating per-group windows the
/// canvas anchors over each group, so the panel only states the group count and
/// offers "Done". In the exploded `fan` view the canvas popups merely switch the
/// active group, so the panel hosts the group selector + the active group's
/// editor (via [`group_editor_ui`]). The phase is each group's derived value
/// (shown read-only). Mixed selections appear as several groups (e.g. an older +
/// a newer phase), each editable separately.
fn edit_session<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    e: &StructureEdit,
    enabled: bool,
    on_event: &mut F,
) {
    ui.label(
        egui::RichText::new(format!("Edit attributes — {} group(s)", e.groups.len()))
            .size(11.0)
            .weak(),
    );
    if e.exploded {
        // Group selector (one active at a time) + the active group's editor.
        if e.groups.len() > 1 {
            ui.horizontal_wrapped(|ui| {
                for (i, g) in e.groups.iter().enumerate() {
                    let active = i == e.active;
                    if ui
                        .add_enabled(enabled, egui::Button::selectable(active, g.name.clone()))
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::SetActiveStructureGroup(i)));
                    }
                }
            });
        }
        if let Some(g) = e.groups.get(e.active) {
            group_editor_ui(ui, g.ty.kind(), e.active, g, enabled, on_event);
        }
    } else {
        ui.label(
            egui::RichText::new("Set each group's construction in its window on the plan.")
                .size(11.0)
                .weak(),
        );
    }
    ui.add_space(2.0);
    if ui.add_enabled(enabled, egui::Button::new("Done")).clicked() {
        on_event(UiEvent::Sketch(SketchEvent::EndStructureEdit));
    }
}

/// One group's attribute editor: a two-level heading (the surface KIND, then the
/// group's editable NAME), the derived phase (read-only), the construction TYPE
/// (a dropdown whose default text doubles as the field heading, e.g. "Wall
/// type"), and the build-up row (Default / Unknown / Specify…). Each control
/// emits the matching event for `group_index`. Shared by the side panel
/// (exploded view's active group) and the canvas's floating per-group windows
/// (the default in-place view), so both edit a group identically.
pub(crate) fn group_editor_ui<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    kind: SurfaceKind,
    group_index: usize,
    g: &EditGroup,
    enabled: bool,
    on_event: &mut F,
) {
    // Heading level 1: the surface kind.
    ui.label(
        egui::RichText::new(kind_heading(kind))
            .size(EDITOR_FONT + 2.0)
            .strong(),
    );
    // Heading level 2: the editable group name (defaults to "Group N").
    let mut name = g.name.clone();
    let renamed = ui
        .push_id(("structure_group_name", group_index), |ui| {
            crate::ui::widgets::pads::text_input(ui, &mut name, EDIT_NAME_W, "name")
        })
        .inner
        .changed;
    if renamed {
        on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupName {
            group: group_index,
            name: name.clone(),
        }));
    }

    ui.label(
        egui::RichText::new(format!("Phase: {}", g.phase.default_name()))
            .size(EDITOR_FONT)
            .weak(),
    );
    // Each kind offers its own types; the chosen type is tagged with
    // `ConstructionType` so the event/controller stay kind-agnostic. The TYPE is
    // a dropdown (not a button row) to keep the window narrow; its UNSET text is
    // the field heading ("Wall type" / …), so no separate label is needed.
    let types: Vec<(ConstructionType, &str)> = match kind {
        SurfaceKind::Walls => WALL_TYPES
            .iter()
            .map(|(t, l)| (ConstructionType::Wall(*t), *l))
            .collect(),
        SurfaceKind::Floors => FLOOR_TYPES
            .iter()
            .map(|(t, l)| (ConstructionType::Floor(*t), *l))
            .collect(),
        SurfaceKind::Roofs => CEILING_TYPES
            .iter()
            .map(|(t, l)| (ConstructionType::Ceiling(*t), *l))
            .collect(),
    };
    let current = types
        .iter()
        .find(|(t, _)| *t == g.ty)
        .map(|(_, l)| *l)
        .unwrap_or(type_label(kind));
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(("structure_type", group_index))
            .selected_text(egui::RichText::new(current).size(EDITOR_FONT))
            .show_ui(ui, |ui| {
                for (ty, label) in &types {
                    if ui
                        .selectable_label(
                            g.ty == *ty,
                            egui::RichText::new(*label).size(EDITOR_FONT),
                        )
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupAttrs {
                            group: group_index,
                            ty: *ty,
                            makeup: g.makeup.clone(),
                        }));
                    }
                }
            });
    });
    ui.label(egui::RichText::new("Build-up").size(EDITOR_FONT).weak());
    ui.horizontal_wrapped(|ui| {
        // Default fabric, Unknown (`Tbd` — still "not done"), or Specify a custom
        // layer build-up (`Layers`; the layer editor itself is future work).
        for (label, mk) in [
            ("Default", ConstructionMakeup::Default),
            ("Unknown", ConstructionMakeup::Tbd),
            ("Specify…", ConstructionMakeup::Layers(Vec::new())),
        ] {
            let sel = match (&g.makeup, &mk) {
                (ConstructionMakeup::Layers(_), ConstructionMakeup::Layers(_)) => true,
                (a, b) => a == b,
            };
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(sel, egui::RichText::new(label).size(EDITOR_FONT)),
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupAttrs {
                    group: group_index,
                    ty: g.ty,
                    makeup: mk.clone(),
                }));
            }
        }
    });
}

/// Heading-level-1 label for a group: the surface kind.
fn kind_heading(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Walls => "Wall",
        SurfaceKind::Floors => "Floor",
        SurfaceKind::Roofs => "Roof",
    }
}

/// The construction-type dropdown's UNSET placeholder (doubles as its heading).
fn type_label(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Walls => "Wall type",
        SurfaceKind::Floors => "Floor type",
        SurfaceKind::Roofs => "Roof type",
    }
}

fn count_for(kind: SurfaceKind, counts: SurfaceCounts) -> usize {
    match kind {
        SurfaceKind::Walls => counts.walls,
        SurfaceKind::Roofs => counts.roofs,
        SurfaceKind::Floors => counts.floors,
    }
}
