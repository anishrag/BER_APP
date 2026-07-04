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
//! ceilings/ceilings by region centroid.

use ber_model::ber::common::ConstructionPhase;
use ber_model::ber::structural::{
    CeilingConstructionType, CeilingExposure, CeilingInsulation, ConstructionLayer,
    ConstructionMakeup, ConstructionType, FloorConstructionType, FloorExposure, SurfaceExposure,
    WallConstructionType, WallExposure,
};

use super::SketchEvent;
use crate::controller::app_controller::{
    EditGroup, ExistingGroup, SelectMode, StructureEdit, StructureProgress, SurfaceKind,
};
use crate::ui::pages::events::UiEvent;

/// Font size for the per-group attribute editor (the floating windows + the
/// exploded-view side panel). Larger than the tool-row labels so the editable
/// controls read clearly at the zoomed-out edit scale.
const EDITOR_FONT: f32 = 12.5;
/// Width of the editable group-name field in the attribute editor.
const EDIT_NAME_W: f32 = 130.0;

/// The three MODES, in panel order.
const MODES: &[(SurfaceKind, &str)] = &[
    (SurfaceKind::Walls, "Walls"),
    (SurfaceKind::Ceilings, "Ceilings"),
    (SurfaceKind::Floors, "Floors"),
];

/// The global Select subtools, in panel order.
const SELECT_MODES: &[(SelectMode, &str)] = &[
    (SelectMode::Click, "Click"),
    (SelectMode::Carve, "Carve"),
    (SelectMode::All, "All"),
    (SelectMode::Remaining, "Remaining"),
];

// The TYPE options OFFERED in the group-editor dropdown (a subset of each enum —
// `Uninitialised` etc. aren't user-pickable). The display strings come from each
// type's `label()` (the single source, shared with the Structure review), so the
// options list here is just the offered variants in order.
/// Wall construction TYPE options (the group editor).
const WALL_TYPES: &[WallConstructionType] = &[
    WallConstructionType::CavityWall,
    WallConstructionType::FilledCavityWall,
    WallConstructionType::ConcreteHollowBlocks,
    WallConstructionType::TimberConstruction,
    WallConstructionType::ICF,
];

/// Floor construction TYPE options (the group editor).
const FLOOR_TYPES: &[FloorConstructionType] = &[
    FloorConstructionType::SolidFloor,
    FloorConstructionType::SuspendedFloor,
    FloorConstructionType::ExposedSemiExposedFloor,
    FloorConstructionType::NonHeatLossFloor,
    FloorConstructionType::Other,
];

/// Ceiling/roof construction TYPE options (the group editor).
const CEILING_TYPES: &[CeilingConstructionType] = &[
    CeilingConstructionType::FlatRoof,
    CeilingConstructionType::SlopedInsOnRafter,
    CeilingConstructionType::SlopedInsOnCeiling,
    CeilingConstructionType::SlopedInsOnCeilingAndRafter,
    CeilingConstructionType::NonHeatLossCeiling,
    CeilingConstructionType::Other,
];

/// JOIN an existing group: apply its `(type, make-up, phase)` to `group_index`
/// (the surfaces then share its construction) and adopt its stored name so the
/// editor visibly reflects the join. Shared by the fresh box's "Existing
/// group…" dropdown and the full editor's "Add to existing group…" combo.
fn emit_join_existing<F: FnMut(UiEvent)>(on_event: &mut F, group_index: usize, eg: &ExistingGroup) {
    on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupAttrs {
        group: group_index,
        ty: eg.ty,
        makeup: eg.makeup.clone(),
        phase: eg.phase.clone(),
    }));
    if let Some(n) = eg.name.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupName {
            group: group_index,
            name: n.to_string(),
        }));
    }
}

/// The label an existing group is offered under — its NAME, first and
/// foremost (names are the assessor's own handles for their groups). A group
/// that was never named still has a real identity (its attributes), so it
/// stays offered, but under a clearly-marked fallback — "(unnamed) Cavity" —
/// so picking it is an informed choice, never mistakable for a named group.
fn existing_group_label(eg: &ExistingGroup) -> String {
    if let Some(n) = eg.name.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return n.to_string();
    }
    let type_label = match eg.ty {
        ConstructionType::Wall(t) => t.label(),
        ConstructionType::Floor(t) => t.label(),
        ConstructionType::Ceiling(t) => t.label(),
    };
    format!("(unnamed) {type_label}")
}

/// Render the Structure tool area: the global MODE row + the global SELECT row +
/// the "Edit attributes" group editor (when a session is open). `kind` is the
/// active mode; `select` the active Select subtool; `edit` the in-flight session;
/// `enabled` gates interaction. Each mode button carries its kind's
/// "attributed/total" progress counter ("Walls · 3/12" — in-scope surfaces
/// only, per [`StructureProgress`]).
pub fn structure_panel<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    kinds: &std::collections::BTreeSet<SurfaceKind>,
    select: Option<SelectMode>,
    edit: Option<&StructureEdit>,
    progress: StructureProgress,
    phases: &[(ConstructionPhase, String)],
    existing: &[ExistingGroup],
    enabled: bool,
    // The All-storeys tab: the canvas hosts no per-group floating windows
    // there, so an open edit session is edited HERE in the panel (group
    // selector + active group's editor, like the exploded view).
    all_storeys: bool,
    on_event: &mut F,
) {
    ui.label(egui::RichText::new("Mode").size(11.0).weak());
    ui.horizontal_wrapped(|ui| {
        // The mode buttons are NON-EXCLUSIVE — each toggles its kind in/out of
        // the active set, so the assessor can work several at once (Walls +
        // Floors). The event toggles; the controller keeps ≥1 active.
        for (m, label) in MODES {
            let active = kinds.contains(m);
            let (done, total) = progress_for(*m, progress);
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(active, format!("{label}  ·  {done}/{total}")),
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
        // No session: offer to open one (any kind — walls / floors / ceilings).
        None => {
            if ui
                .add_enabled(enabled, egui::Button::new("Edit attributes"))
                .on_hover_text("Set construction for the selected surfaces")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::BeginStructureEdit));
            }
        }
        Some(e) => edit_session(ui, e, phases, existing, enabled, all_storeys, on_event),
    }
}

/// The side-panel summary for an open "Edit attributes" session. In the default
/// (in-place) view the editing happens in the floating per-group windows the
/// canvas anchors over each group, so the panel only states the group count and
/// offers "Done". In the exploded `fan` view the canvas popups merely switch the
/// active group, so the panel hosts the group selector + the active group's
/// editor (via [`group_editor_ui`]). The phase defaults to each group's derived
/// value but is editable. Mixed selections appear as several groups (e.g. an
/// older + a newer phase), each editable separately.
fn edit_session<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    e: &StructureEdit,
    phases: &[(ConstructionPhase, String)],
    existing: &[ExistingGroup],
    enabled: bool,
    all_storeys: bool,
    on_event: &mut F,
) {
    ui.label(
        egui::RichText::new(format!("Edit attributes — {} group(s)", e.groups.len()))
            .size(11.0)
            .weak(),
    );
    // The exploded view hosts the editor here; so does the ALL-STOREYS tab
    // (its canvas draws every storey's cell — no per-group floating windows).
    if e.exploded || all_storeys {
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
            group_editor_ui(
                ui,
                g.ty.kind(),
                e.active,
                g,
                phases,
                existing,
                enabled,
                e.name_error
                    .as_ref()
                    .filter(|(gi, _)| *gi == e.active)
                    .map(|(_, m)| m.as_str()),
                on_event,
            );
        }
    } else {
        ui.label(
            egui::RichText::new("Set each group's construction in its window on the plan.")
                .size(11.0)
                .weak(),
        );
    }
    ui.add_space(2.0);
    // Done ALWAYS closes the session — an untouched/half-filled group is a
    // legitimate exit (the assessor may reselect and attribute one by one).
    // It just stays INCOMPLETE: no tick, still counted by `Remaining` and the
    // checklist. The group boxes keep their inline warn hints (e.g. the
    // ceiling-insulation row's) as guidance, not a lock.
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
#[allow(clippy::too_many_arguments)]
pub(crate) fn group_editor_ui<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    kind: SurfaceKind,
    group_index: usize,
    g: &EditGroup,
    phases: &[(ConstructionPhase, String)],
    existing: &[ExistingGroup],
    enabled: bool,
    // The name-integrity rejection hint for THIS group, if its last rename
    // collided with another group's name — shown under the name box.
    name_error: Option<&str>,
    on_event: &mut F,
) {
    // Heading level 1: the surface kind + the group's surface COUNT — centred,
    // bold (e.g. "Wall (3)"). When every attribute of the group's construction
    // is fully set ([`EditGroup::is_complete`] — same predicate as the
    // checklist, incl. the ceiling-insulation gate), a tick sits at the TOP
    // RIGHT of the box (the checklist's green, same glyph).
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(format!("{} ({})", kind_heading(kind), g.ids.len()))
                .size(EDITOR_FONT + 2.0)
                .strong(),
        );
    });
    if g.is_complete() {
        let top_right = ui.max_rect().right_top();
        ui.painter().text(
            top_right + egui::vec2(-2.0, 0.0),
            egui::Align2::RIGHT_TOP,
            "✓",
            egui::FontId::proportional(EDITOR_FONT + 3.0),
            // Same green as the checklist's "all storeys done" dot / phase tick.
            egui::Color32::from_rgb(80, 180, 95),
        );
    }

    let relevant: Vec<&ExistingGroup> = existing.iter().filter(|e| e.ty.kind() == kind).collect();

    // TWO-MODE ENTRY for a fresh group (nothing attributed yet, and there IS
    // something to join): just an "Existing group…" dropdown — picking one
    // assigns this selection to that construction and fills the box with its
    // details — plus a "New group" button that opens the full editor below.
    if !g.show_editor && !relevant.is_empty() {
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt(("structure_join_existing", group_index))
                .selected_text(egui::RichText::new("Existing group…").size(EDITOR_FONT))
                .show_ui(ui, |ui| {
                    for eg in &relevant {
                        if ui
                            .selectable_label(
                                false,
                                egui::RichText::new(existing_group_label(eg)).size(EDITOR_FONT),
                            )
                            .clicked()
                        {
                            emit_join_existing(on_event, group_index, eg);
                        }
                    }
                });
        });
        if ui
            .add_enabled(
                enabled,
                egui::Button::new(egui::RichText::new("New group").size(EDITOR_FONT)),
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::BeginNewStructureGroup {
                group: group_index,
            }));
        }
        return;
    }

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
    // Name-integrity rejection: the durable name kept its last valid value;
    // the hint tells the assessor why this one didn't stick.
    if let Some(err) = name_error {
        ui.label(
            egui::RichText::new(err)
                .size(11.0)
                .color(ui.visuals().warn_fg_color),
        );
    }

    // "Add to an existing group": apply a construction already in use elsewhere
    // (its type + build-up + phase) to this selection, so they share one group /
    // DEAP entry — instead of re-entering the attributes. Picking nothing and
    // editing the fields below just creates a new group, as before. This is
    // the "move to existing group" affordance for a box that OPENED already
    // attributed; a session-fresh box ("New group" flow) doesn't repeat the
    // offer — its bottom-right Reset returns to the two-mode entry instead.
    if !relevant.is_empty() && !g.fresh {
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt(("structure_existing", group_index))
                .selected_text(egui::RichText::new("Add to existing group…").size(EDITOR_FONT))
                .show_ui(ui, |ui| {
                    for eg in &relevant {
                        if ui
                            .selectable_label(
                                false,
                                egui::RichText::new(existing_group_label(eg)).size(EDITOR_FONT),
                            )
                            .clicked()
                        {
                            emit_join_existing(on_event, group_index, eg);
                        }
                    }
                });
        });
    }

    // PHASE dropdown — defaults to the value derived from the stage-1 zones, but
    // the assessor can override it here. No label: the populated value (e.g.
    // "Original") makes clear what it is. Falls back to the phase's built-in name
    // if it isn't among the defined choices.
    let phase_text = phases
        .iter()
        .find(|(p, _)| *p == g.phase)
        .map(|(_, n)| n.clone())
        .unwrap_or_else(|| g.phase.default_name());
    ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(("structure_phase", group_index))
            .selected_text(egui::RichText::new(phase_text).size(EDITOR_FONT))
            .show_ui(ui, |ui| {
                for (p, pname) in phases {
                    if ui
                        .selectable_label(
                            g.phase == *p,
                            egui::RichText::new(pname).size(EDITOR_FONT),
                        )
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupAttrs {
                            group: group_index,
                            ty: g.ty,
                            makeup: g.makeup.clone(),
                            phase: p.clone(),
                        }));
                    }
                }
            });
    });
    // EXPOSURE dropdown — the boundary condition shared by the group's surfaces,
    // mirroring the phase dropdown. When the group is LOCKED (a face-partitioned
    // wall, whose exposure is owned per-2-D-region by the wall partition tool),
    // it's shown disabled with a hint rather than as an editable control. A
    // derived Inter-storey group is likewise READ-ONLY: `derive_inter_storey`
    // owns that classification (re-stamped on every re-resolve), so offering
    // the dropdown would only invite writes the pipeline overrides.
    let inter_storey = matches!(
        g.exposure,
        SurfaceExposure::Wall(WallExposure::InterStorey)
            | SurfaceExposure::Floor(FloorExposure::InterStorey)
            | SurfaceExposure::Ceiling(CeilingExposure::InterStorey)
    );
    if g.exposure_locked {
        ui.add_enabled(
            false,
            egui::Button::new(egui::RichText::new(g.exposure.label()).size(EDITOR_FONT)),
        )
        .on_disabled_hover_text("Exposure is per-region — set it in the wall partition tool.");
    } else if inter_storey {
        ui.add_enabled(
            false,
            egui::Button::new(egui::RichText::new(g.exposure.label()).size(EDITOR_FONT)),
        )
        .on_disabled_hover_text(
            "Backed by another storey of this dwelling — classified automatically.",
        );
    } else {
        ui.add_enabled_ui(enabled, |ui| {
            egui::ComboBox::from_id_salt(("structure_exposure", group_index))
                .selected_text(egui::RichText::new(g.exposure.label()).size(EDITOR_FONT))
                .show_ui(ui, |ui| {
                    for exp in SurfaceExposure::variants_for(kind) {
                        if ui
                            .selectable_label(
                                g.exposure == *exp,
                                egui::RichText::new(exp.label()).size(EDITOR_FONT),
                            )
                            .clicked()
                        {
                            on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupExposure {
                                group_index,
                                exposure: *exp,
                            }));
                        }
                    }
                });
        });
    }
    // Each kind offers its own types; the chosen type is tagged with
    // `ConstructionType` so the event/controller stay kind-agnostic. The TYPE is
    // a dropdown (not a button row) to keep the window narrow; its UNSET text is
    // the field heading ("Wall type" / …), so no separate label is needed.
    let types: Vec<(ConstructionType, &str)> = match kind {
        SurfaceKind::Walls => WALL_TYPES
            .iter()
            .map(|t| (ConstructionType::Wall(*t), t.label()))
            .collect(),
        SurfaceKind::Floors => FLOOR_TYPES
            .iter()
            .map(|t| (ConstructionType::Floor(*t), t.label()))
            .collect(),
        SurfaceKind::Ceilings => CEILING_TYPES
            .iter()
            .map(|t| (ConstructionType::Ceiling(*t), t.label()))
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
                            phase: g.phase.clone(),
                        }));
                    }
                }
            });
    });
    ui.horizontal_wrapped(|ui| {
        // Build-up as ICON buttons (no label — hover for the meaning). Default
        // (✓) and Unknown (?) set the make-up directly; Specify (ℹ) opens the
        // LAYER editor (the `Layers` build-up). The unset `Tbd` default isn't
        // offered.
        for (icon, tip, mk) in [
            ("✓", "Default", ConstructionMakeup::Default),
            ("?", "Unknown", ConstructionMakeup::Unknown),
        ] {
            let sel = g.makeup == mk;
            if ui
                .add_enabled(
                    enabled,
                    egui::Button::selectable(
                        sel,
                        egui::RichText::new(icon).size(EDITOR_FONT + 3.0),
                    ),
                )
                .on_hover_text(tip)
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupAttrs {
                    group: group_index,
                    ty: g.ty,
                    makeup: mk.clone(),
                    phase: g.phase.clone(),
                }));
            }
        }
        // Specify… → open the build-up layer editor.
        let specify_sel = matches!(g.makeup, ConstructionMakeup::Layers(_));
        if ui
            .add_enabled(
                enabled,
                egui::Button::selectable(
                    specify_sel,
                    egui::RichText::new("ℹ").size(EDITOR_FONT + 3.0),
                ),
            )
            .on_hover_text("Specify…")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::BeginSpecifyLayers {
                group: group_index,
            }));
        }
    });
    // CEILINGS ONLY: the insulation row — an "Insulation … mm" thickness box
    // plus an "unknown" checkbox, mutually exclusive (typing a thickness
    // unchecks "unknown"; checking it clears the box). One of the two must be
    // given before the session's Done enables, and a specified thickness also
    // needs a photo (the camera button just below) — the inline hint under the
    // row says which is missing.
    if kind == SurfaceKind::Ceilings {
        ceiling_insulation_ui(ui, group_index, g, enabled, on_event);
    }
    // Photos (next line): the camera button opens the photos view (thumbnails +
    // add). Shows a count when there are any.
    let cam = if g.photos.is_empty() {
        "📷".to_string()
    } else {
        format!("📷 {}", g.photos.len())
    };
    if ui
        .add_enabled(enabled, egui::Button::new(cam))
        .on_hover_text("Photos")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::BeginViewPhotos {
            group: group_index,
        }));
    }
    // Bottom-right RESET — on EVERY box, fresh or attributed: reselecting an
    // already-attributed surface after Done must still offer starting again
    // from scratch. It detaches THIS selection's attribution only (a shared
    // construction — its other members, name, photos — is untouched) and
    // returns the box to the two-mode entry so the assessor can choose again.
    // On an attributed box it sits alongside the "Add to existing group…"
    // combo above: moving to a group and resetting to nothing are different
    // intents (the combo lives at the top, Reset at the bottom right — no
    // crowding).
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
        if ui
            .add_enabled(
                enabled,
                egui::Button::new(egui::RichText::new("Reset").size(EDITOR_FONT)).small(),
            )
            .on_hover_text(
                "Back to \"Existing group / New group\" — un-attributes this \
                 selection (an existing group itself is untouched)",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ResetStructureGroup {
                group: group_index,
            }));
        }
    });
}

/// The CEILING insulation row of the group editor: the thickness box (mm) +
/// the "unknown" checkbox, plus the inline hint while the spec blocks the
/// session from completing. The text buffer lives in egui memory keyed by the
/// group, stored TOGETHER with the model value it corresponds to: when the
/// group's value changes underneath it (a fresh session, the checkbox, undo)
/// the buffer re-seeds from the model, while in-flight typing — whose partial
/// text ("12.") intentionally parses to the same `None` it just emitted —
/// stays untouched. Both render sites (side panel + canvas window) share the
/// buffer, so the field reads the same wherever the group is edited.
fn ceiling_insulation_ui<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    group_index: usize,
    g: &EditGroup,
    enabled: bool,
    on_event: &mut F,
) {
    let store = egui::Id::new(("ceiling_insulation", group_index));
    let (mut text, mut synced) = ui
        .ctx()
        .data_mut(|d| d.get_temp::<(String, Option<CeilingInsulation>)>(store))
        .filter(|(_, synced)| *synced == g.insulation)
        .unwrap_or_else(|| {
            let t = match g.insulation {
                Some(CeilingInsulation::ThicknessMm(v)) => format_mm(v),
                _ => String::new(),
            };
            (t, g.insulation)
        });
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new("Insulation").size(EDITOR_FONT));
        ui.add_enabled_ui(enabled, |ui| {
            if crate::ui::widgets::pads::metre_input(ui, &mut text, 46.0, "mm").changed {
                // Typing owns the value: a valid positive number specifies the
                // thickness (which also unchecks "unknown"); anything else
                // clears back to unspecified.
                let parsed = text
                    .trim()
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite() && *v > 0.0)
                    .map(CeilingInsulation::ThicknessMm);
                synced = parsed;
                on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupInsulation {
                    group: group_index,
                    insulation: parsed,
                }));
            }
        });
        ui.label(egui::RichText::new("mm").size(EDITOR_FONT));
        let mut unknown = matches!(synced, Some(CeilingInsulation::Unknown));
        if ui
            .add_enabled(
                enabled,
                egui::Checkbox::new(
                    &mut unknown,
                    egui::RichText::new("unknown").size(EDITOR_FONT),
                ),
            )
            .changed()
        {
            let next = if unknown {
                text.clear();
                Some(CeilingInsulation::Unknown)
            } else {
                None
            };
            synced = next;
            on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupInsulation {
                group: group_index,
                insulation: next,
            }));
        }
    });
    // The inline "why not done" hint, from the EFFECTIVE state this frame
    // (`synced` — the value just emitted, one frame fresher than `g`).
    if !CeilingInsulation::spec_done(synced, !g.photos.is_empty()) {
        let hint = match synced {
            Some(CeilingInsulation::ThicknessMm(_)) => "a photo of the insulation is required",
            _ => "enter the insulation thickness or mark it unknown",
        };
        ui.label(
            egui::RichText::new(hint)
                .size(EDITOR_FONT - 1.5)
                .color(ui.visuals().warn_fg_color),
        );
    }
    ui.ctx().data_mut(|d| d.insert_temp(store, (text, synced)));
}

/// Compact mm display for the insulation box's seed: whole numbers without a
/// decimal tail ("150"), fractional values as typed ("112.5").
fn format_mm(v: f32) -> String {
    if v.fract().abs() < 1e-4 {
        format!("{v:.0}")
    } else {
        format!("{v}")
    }
}

/// The PHOTOS view for a group (the camera button → centred window): the
/// group's photo thumbnails, with a "+" to add another (desktop file dialog /
/// Android camera). `Done` closes it.
pub(crate) fn photos_view_ui<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    group_index: usize,
    g: &EditGroup,
    on_event: &mut F,
) {
    ui.label(
        egui::RichText::new(format!("{} photos", kind_heading(g.ty.kind())))
            .size(EDITOR_FONT + 2.0)
            .strong(),
    );
    if g.photos.is_empty() {
        ui.label(
            egui::RichText::new("no photos yet")
                .size(EDITOR_FONT)
                .italics()
                .weak(),
        );
    }
    egui::ScrollArea::vertical()
        .max_height(420.0)
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for p in &g.photos {
                    // Prefer the lightweight preview; fall back to the original
                    // for photos saved before thumbnails existed.
                    let src = p.thumbnail_path.as_deref().unwrap_or(&p.file_path);
                    // `fit_to_exact_size` ignores the available layout space (a
                    // thin text-line height inside `horizontal_wrapped`) and
                    // renders the preview into a real box, keeping aspect ratio.
                    ui.add(
                        egui::Image::new(format!("file://{src}"))
                            .fit_to_exact_size(egui::vec2(200.0, 200.0))
                            .maintain_aspect_ratio(true)
                            .corner_radius(4.0),
                    );
                }
            });
        });

    ui.separator();
    // "+" adds another photo (the modular capture pair — TEMPORARY hi/lo
    // buttons, see `photo_button_pair`). Build-up close-ups need legible
    // detail, so this site's hi tier is High.
    ui.horizontal(|ui| {
        if let Some((path, quality)) = crate::ui::widgets::photo_button_pair(
            ui,
            "➕  Add photo",
            true,
            crate::ui::widgets::PhotoQuality::High,
        ) {
            on_event(UiEvent::Sketch(SketchEvent::AddGroupPhoto {
                group: group_index,
                path,
                quality,
            }));
        }
    });
    ui.separator();
    if ui.button("Done").clicked() {
        on_event(UiEvent::Sketch(SketchEvent::EndViewPhotos));
    }
}

/// The build-up LAYER editor for a group (the "Specify…" flow): the current
/// layers inside→outside (each removable), a picker over the app-wide library
/// ("previously typed layers"), and a form to type a NEW layer (which appends to
/// the build-up AND saves to the library). The new-layer form's text lives in
/// egui memory keyed by the group, so it persists across frames without page
/// state. `Done` closes the editor.
pub(crate) fn layer_editor_ui<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    group_index: usize,
    g: &EditGroup,
    library: &[ConstructionLayer],
    on_event: &mut F,
) {
    // Editing happens in a local (name, thickness-text) working list held in egui
    // memory keyed by the group; seeded from the group's current layers the first
    // time it's opened, committed to the build-up + library only on Done.
    let id = ui.id().with(("buildup_work", group_index));
    let mut work: Vec<(String, String)> = ui
        .ctx()
        .data_mut(|d| d.get_temp::<Vec<(String, String)>>(id))
        .unwrap_or_else(|| match &g.makeup {
            ConstructionMakeup::Layers(v) => v
                .iter()
                .map(|l| (l.name(), l.thickness_mm.to_string()))
                .collect(),
            _ => Vec::new(),
        });

    ui.label(
        egui::RichText::new(format!("{} build-up", kind_heading(g.ty.kind())))
            .size(EDITOR_FONT + 2.0)
            .strong(),
    );
    ui.label(
        egui::RichText::new("inside → outside")
            .size(EDITOR_FONT)
            .weak(),
    );

    // A "+" anywhere inserts a layer at that position — either blank (to type) or
    // picked from the reusable library (the common path). Collected, applied
    // after the row loop so we don't mutate `work` mid-iteration.
    let mut insert_at: Option<(usize, (String, String))> = None;
    let mut remove: Option<usize> = None;

    // Insert-at-top.
    if let Some(pair) = insert_menu(ui, ("ins", group_index, usize::MAX), library) {
        insert_at = Some((0, pair));
    }
    for i in 0..work.len() {
        ui.horizontal(|ui| {
            if ui.small_button("✕").on_hover_text("Remove").clicked() {
                remove = Some(i);
            }
            ui.add(
                egui::TextEdit::singleline(&mut work[i].0)
                    .desired_width(110.0)
                    .hint_text("layer"),
            );
            ui.add(
                egui::TextEdit::singleline(&mut work[i].1)
                    .desired_width(40.0)
                    .hint_text("mm"),
            );
            ui.label(egui::RichText::new("mm").size(EDITOR_FONT));
            // "+" after this layer → insert below (between layers).
            if let Some(pair) = insert_menu(ui, ("ins", group_index, i), library) {
                insert_at = Some((i + 1, pair));
            }
        });
    }
    if let Some((at, pair)) = insert_at {
        work.insert(at.min(work.len()), pair);
    }
    if let Some(i) = remove {
        if i < work.len() {
            work.remove(i);
        }
    }

    ui.separator();
    let done = ui.button("Done").clicked();
    if done {
        // Commit: drop unnamed rows, parse thickness, set the build-up (which also
        // saves the named layers to the library), then close + clear the buffer.
        let layers: Vec<ConstructionLayer> = work
            .iter()
            .filter(|(n, _)| !n.trim().is_empty())
            .map(|(n, t)| {
                ConstructionLayer::typed(
                    n.trim().to_string(),
                    t.trim().parse().unwrap_or(0),
                    0.0,
                    false,
                )
            })
            .collect();
        on_event(UiEvent::Sketch(SketchEvent::SetStructureGroupLayers {
            group: group_index,
            layers,
        }));
        on_event(UiEvent::Sketch(SketchEvent::EndSpecifyLayers));
        ui.ctx().data_mut(|d| d.remove::<Vec<(String, String)>>(id));
    } else {
        ui.ctx().data_mut(|d| d.insert_temp(id, work));
    }
}

/// A "+" button that opens a menu to insert a layer: a blank one to type, or any
/// previously-typed library layer. Returns the chosen `(name, thickness-text)`
/// when an item is clicked. `salt` disambiguates the multiple "+" menus.
fn insert_menu(
    ui: &mut egui::Ui,
    salt: (&'static str, usize, usize),
    library: &[ConstructionLayer],
) -> Option<(String, String)> {
    let mut picked = None;
    ui.push_id(salt, |ui| {
        ui.menu_button("+", |ui| {
            if ui.button("Blank layer").clicked() {
                picked = Some((String::new(), String::new()));
                ui.close();
            }
            if !library.is_empty() {
                ui.separator();
                for l in library {
                    if ui.button(layer_summary(l)).clicked() {
                        picked = Some((l.name(), l.thickness_mm.to_string()));
                        ui.close();
                    }
                }
            }
        });
    });
    picked
}

/// One-line summary of a layer for the library pick menu.
fn layer_summary(l: &ConstructionLayer) -> String {
    format!("{} · {}mm", l.name(), l.thickness_mm)
}

/// Heading-level-1 label for a group: the surface kind.
fn kind_heading(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Walls => "Wall",
        SurfaceKind::Floors => "Floor",
        SurfaceKind::Ceilings => "Roof",
    }
}

/// The construction-type dropdown's UNSET placeholder (doubles as its heading).
fn type_label(kind: SurfaceKind) -> &'static str {
    match kind {
        SurfaceKind::Walls => "Wall type",
        SurfaceKind::Floors => "Floor type",
        SurfaceKind::Ceilings => "Roof type",
    }
}

fn progress_for(kind: SurfaceKind, progress: StructureProgress) -> (usize, usize) {
    match kind {
        SurfaceKind::Walls => progress.walls,
        SurfaceKind::Ceilings => progress.ceilings,
        SurfaceKind::Floors => progress.floors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The existing-group pickers (the fresh box's "Existing group…" dropdown
    /// and the in-editor "Add to existing group…" combo share this) label
    /// entries by NAME first; a group that was never named is offered under a
    /// clearly-marked "(unnamed) <type>" fallback — honest but unmistakable.
    #[test]
    fn existing_group_labels_use_names_first() {
        let named = ExistingGroup {
            ty: ConstructionType::Wall(WallConstructionType::CavityWall),
            makeup: ConstructionMakeup::Default,
            phase: ConstructionPhase::default(),
            name: Some("Brick".into()),
        };
        assert_eq!(existing_group_label(&named), "Brick");
        let unnamed = ExistingGroup {
            name: None,
            ..named.clone()
        };
        assert_eq!(existing_group_label(&unnamed), "(unnamed) Cavity");
        // Whitespace-only names read as unnamed too.
        let blank = ExistingGroup {
            name: Some("   ".into()),
            ..named
        };
        assert_eq!(existing_group_label(&blank), "(unnamed) Cavity");
    }
}
