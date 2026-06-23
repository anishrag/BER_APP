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

use ber_model::ber::common::ConstructionPhase;
use ber_model::ber::structural::{
    CeilingConstructionType, ConstructionLayer, ConstructionMakeup, ConstructionType,
    FloorConstructionType, SurfaceCounts, WallConstructionType,
};

use super::SketchEvent;
use crate::controller::app_controller::{EditGroup, SelectMode, StructureEdit, SurfaceKind};
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
    phases: &[(ConstructionPhase, String)],
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
        Some(e) => edit_session(ui, e, phases, enabled, on_event),
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
            group_editor_ui(ui, g.ty.kind(), e.active, g, phases, enabled, on_event);
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
    phases: &[(ConstructionPhase, String)],
    enabled: bool,
    on_event: &mut F,
) {
    // Heading level 1: the surface kind — centred + bold.
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(kind_heading(kind))
                .size(EDITOR_FONT + 2.0)
                .strong(),
        );
    });
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
    // "+" adds another photo (the modular capture button).
    if let Some(path) = crate::ui::widgets::photo_button(
        ui,
        "➕  Add photo",
        true,
        crate::ui::widgets::PhotoConfig {
            default_quality: crate::ui::widgets::PhotoQuality::High,
        },
    ) {
        on_event(UiEvent::Sketch(SketchEvent::AddGroupPhoto {
            group: group_index,
            path,
        }));
    }
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
