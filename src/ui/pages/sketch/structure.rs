//! The Structure-phase tool area — the swap-in for the Mapping categories when
//! the workspace is in the Structure phase.
//!
//! Layout: a global MODE row (Wall / Roof / Floor) picks which surface kind the
//! tools act on, then one global SELECT row (`Click` / `Carve` / `All` /
//! `Remaining` + `Deselect all`) selects surfaces of that kind. An "Edit
//! attributes" button opens a per-construction GROUP editor over the selection:
//! the selection is partitioned by shared construction, and each group's
//! `(type, build-up)` is editable separately (one group active at a time). The
//! phase axis stays derived from the floor zones (shown read-only). Walls only
//! for now (construction has a parametric home only for walls).

use ber_model::ber::structural::{ConstructionMakeup, SurfaceCounts, WallConstructionType};

use super::SketchEvent;
use crate::controller::app_controller::{SelectMode, StructureEdit, SurfaceKind};
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

/// Wall construction TYPE options (the group editor).
const WALL_TYPES: &[(WallConstructionType, &str)] = &[
    (WallConstructionType::CavityWall, "Cavity"),
    (WallConstructionType::FilledCavityWall, "Filled cavity"),
    (WallConstructionType::ConcreteHollowBlocks, "Hollow block"),
    (WallConstructionType::TimberConstruction, "Timber"),
    (WallConstructionType::ICF, "ICF"),
];

/// Render the Structure tool area: the global MODE row + the global SELECT row +
/// the "Edit attributes" group editor (when a session is open). `kind` is the
/// active mode; `select` the active Select subtool; `edit` the in-flight session;
/// `enabled` gates interaction.
pub fn structure_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    kind: SurfaceKind,
    select: Option<SelectMode>,
    edit: Option<&StructureEdit>,
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
        // No session: offer to open one (walls only for now).
        None => {
            if ui
                .add_enabled(
                    enabled && kind == SurfaceKind::Walls,
                    egui::Button::new("Edit attributes"),
                )
                .on_hover_text("Set construction for the selected walls")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::BeginStructureEdit));
            }
        }
        Some(e) => edit_session(ui, e, enabled, on_event),
    }
}

/// The per-group attribute editor: pick a group (one active), then set that
/// group's wall type + build-up. The phase is the group's derived value (shown
/// read-only). Mixed selections appear as several groups (e.g. an older + a
/// newer phase), each editable separately.
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
    // Group selector (one active at a time).
    if e.groups.len() > 1 {
        ui.horizontal_wrapped(|ui| {
            for (i, g) in e.groups.iter().enumerate() {
                let active = i == e.active;
                if ui
                    .add_enabled(
                        enabled,
                        egui::Button::selectable(
                            active,
                            format!("Group {}  ·  {}", i + 1, g.ids.len()),
                        ),
                    )
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::SetActiveStructureGroup(i)));
                }
            }
        });
    }
    if let Some(g) = e.groups.get(e.active) {
        ui.label(
            egui::RichText::new(format!("Phase: {:?}", g.phase))
                .size(11.0)
                .weak(),
        );
        ui.label(egui::RichText::new("Wall type").size(11.0).weak());
        ui.horizontal_wrapped(|ui| {
            for (t, label) in WALL_TYPES {
                let sel = g.ty == *t;
                if ui
                    .add_enabled(enabled, egui::Button::selectable(sel, *label))
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupAttrs {
                        group: e.active,
                        ty: *t,
                        makeup: g.makeup.clone(),
                    }));
                }
            }
        });
        ui.label(egui::RichText::new("Build-up").size(11.0).weak());
        ui.horizontal_wrapped(|ui| {
            for (label, mk) in [
                ("Default", ConstructionMakeup::Default),
                ("Non-heat-loss", ConstructionMakeup::NonHeatLoss),
            ] {
                let sel = g.makeup == mk;
                if ui
                    .add_enabled(enabled, egui::Button::selectable(sel, label))
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupAttrs {
                        group: e.active,
                        ty: g.ty,
                        makeup: mk.clone(),
                    }));
                }
            }
        });
    }
    ui.add_space(2.0);
    if ui.add_enabled(enabled, egui::Button::new("Done")).clicked() {
        on_event(UiEvent::Sketch(SketchEvent::EndStructureEdit));
    }
}

fn count_for(kind: SurfaceKind, counts: SurfaceCounts) -> usize {
    match kind {
        SurfaceKind::Walls => counts.walls,
        SurfaceKind::Roofs => counts.roofs,
        SurfaceKind::Floors => counts.floors,
    }
}
