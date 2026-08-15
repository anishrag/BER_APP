//! The Stage-2 (Structure) REVIEW flow: a takeover that cycles through every
//! construction group (wall / floor / roof), every window type, and every door
//! across the whole house. For each it shows the recorded details + photos
//! beside the orbitable 3D house with that group's surfaces shaded, so the
//! assessor can sanity-check the attribution at a glance. Confirming records
//! `Ber.structure_reviewed`.
//!
//! UI-free, like the rest of the controller's review logic: this builds the flat
//! item list + view-data from the record and tracks the scroll cursor; the
//! `src/ui` overlay renders it and the existing 3D pipeline does the shading.
use std::collections::HashSet;

use ber_model::ber::common::ItemId;
use ber_model::ber::fenestration::{Frame, Glazing, WindowType};
use ber_model::ber::structural::ConstructionMakeup;

use super::*;

/// Which kind of group a review item is — lets the UI tag/title it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StructureReviewKind {
    Wall,
    Floor,
    Roof,
    WindowType,
    Door,
}

impl StructureReviewKind {
    pub fn label(self) -> &'static str {
        match self {
            StructureReviewKind::Wall => "Wall construction",
            StructureReviewKind::Floor => "Floor construction",
            StructureReviewKind::Roof => "Roof construction",
            StructureReviewKind::WindowType => "Window type",
            StructureReviewKind::Door => "Door",
        }
    }
}

/// Rename affordances for a review item — the review is where the assessor
/// firms up the shorthand names typed during the survey (they feed the
/// letters and every picker). Kind-aware: a construction offers its group
/// name + phase nickname + phase merge; a window type / door offers ONE name
/// field.
pub enum StructureReviewRename {
    /// A wall/floor/roof construction group (see
    /// [`StructureReviewConstructionRename`]).
    Construction(StructureReviewConstructionRename),
    /// A shared window TYPE: rename its model name (via
    /// [`AppController::rename_window_type`] — the model rejects an empty
    /// name or one held by another type), or edit its survey fields as a
    /// GROUP (via [`AppController::set_window_type_fields`] — windows point
    /// at the type by id, so one Save re-attributes every window of it).
    WindowType {
        /// The type's id (rename / field-edit target).
        id: ItemId,
        /// Its current model name ("" = none) — the edit-field prefill.
        name: String,
        /// What a blank name displays as (the glazing·frame type label) —
        /// the edit-field placeholder.
        placeholder: String,
        /// The parked rejection hint from the last refused rename of THIS
        /// type, shown next to the field.
        error: Option<String>,
        /// The type's current survey fields — the "Edit fields" form prefill.
        fields: WindowTypeFields,
    },
    /// A door: rename its free-text type label — committing retypes EVERY
    /// door sharing it (doors group by label, not a shared registry) — or
    /// edit its survey fields (kind / draught-stripped / certified). Unlike
    /// windows the field edit targets THIS door alone: doors have no shared
    /// type record.
    Door {
        /// The door's id (the field-edit target).
        id: ItemId,
        /// The door's current type label — the edit-field prefill AND the
        /// bulk-rename key.
        type_name: String,
        /// The door's current survey fields — the "Edit fields" prefill.
        fields: DoorFields,
    },
}

/// A door's editable survey fields, prefilled into the review's door
/// "Edit fields" form ([`StructureReviewRename::Door`]).
#[derive(Clone)]
pub struct DoorFields {
    pub kind: Option<ber_model::ber::fenestration::DoorKind>,
    /// `None` on records from before the field existed.
    pub draught_stripped: Option<bool>,
    /// `None` on records from before the field existed (= not certified).
    pub certified: Option<bool>,
}

/// A shared window type's editable survey fields, prefilled into the review's
/// "Edit fields" form ([`StructureReviewRename::WindowType`]). One commit
/// applies to every window of the type.
#[derive(Clone)]
pub struct WindowTypeFields {
    pub glazing: Glazing,
    pub frame: Frame,
    /// Install year; `0` = unknown.
    pub year: u32,
    pub certified: bool,
    /// `None` on records from before the field existed.
    pub draught_stripped: Option<bool>,
    /// "Got proof" of the install year (receipt / stamp photo) — when false,
    /// BERresolve offers a chance to attach year documentation office-side.
    pub year_proof: bool,
    /// Glazing gap(s) in mm (two entries for triple glazing).
    pub gap: Vec<u32>,
}

/// The construction-group rename targets + prefills
/// ([`StructureReviewRename::Construction`]).
pub struct StructureReviewConstructionRename {
    /// The construction group's id (rename target).
    pub group_id: ItemId,
    /// Its current custom name ("" = none, showing the type label) — the
    /// edit-field prefill.
    pub group_name: String,
    /// The item's construction phase (nickname rename target).
    pub phase: ber_model::ber::common::ConstructionPhase,
    /// The phase's current nickname ("" = none) — the edit-field prefill.
    pub phase_name: String,
    /// The phase's default name ("Original", "Extension 1", …) — the
    /// edit-field placeholder / what a blank rename falls back to.
    pub phase_default: String,
    /// The OTHER defined phases this item's phase could MERGE into, with
    /// their display labels ("Original (1990)", …). Empty when the dwelling
    /// has only one defined phase (nothing to merge with).
    pub merge_targets: Vec<(ber_model::ber::common::ConstructionPhase, String)>,
}

/// Everything the review overlay needs for one item: its kind, a title, the
/// detail lines (label/value pairs) to list, the photos to show, and the set of
/// 3D surface / opening ids to shade. `cursor`/`total` drive the scroll readout.
/// Built fresh each frame by [`AppController::structure_review_view`].
pub struct StructureReviewView {
    pub cursor: usize,
    pub total: usize,
    pub kind: StructureReviewKind,
    pub title: String,
    pub detail_lines: Vec<(String, String)>,
    pub photos: Vec<PhotoView>,
    pub highlight: HashSet<ItemId>,
    /// Present on construction items: the rename targets + prefills.
    pub rename: Option<StructureReviewRename>,
}

impl AppController {
    /// Whether the Structure review takeover is open.
    pub fn structure_review_mode(&self) -> bool {
        self.structure_review_mode
    }

    /// Open / close the Structure review. Entering resets to the first item and
    /// the default isometric camera (a clean starting viewpoint to orbit from).
    pub fn set_structure_review_mode(&mut self, on: bool) {
        self.structure_review_mode = on;
        // A parked window-type rename hint belongs to the closing/last review
        // session — a fresh session starts clean.
        self.window_type_rename_error = None;
        if on {
            self.structure_review_cursor = 0;
            self.reset_view();
        }
    }

    /// Step the review to the next / previous item (wraps). `delta` is +1 / -1.
    pub fn step_structure_review(&mut self, delta: i32) {
        let n = self.structure_review_items().len() as i32;
        if n == 0 {
            return;
        }
        let cur = (self.structure_review_cursor as i32).min(n - 1);
        self.structure_review_cursor = ((((cur + delta) % n) + n) % n) as usize;
    }

    /// Whether the dwelling's Structure review has been confirmed.
    pub fn structure_reviewed(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.structure_reviewed)
    }

    /// Rename a construction group from the review (blank clears back to the
    /// type label) — the assessor firming up a survey-time shorthand. The
    /// model owns the write ([`Ber::rename_construction_group`]); display-only,
    /// nothing re-links.
    ///
    /// [`Ber::rename_construction_group`]: ber_model::ber::ber_struct::Ber
    pub fn rename_structure_group(&mut self, id: ItemId, name: String) {
        if let Some(record) = self.current_record.as_mut() {
            if record.data.rename_construction_group(id, &name) {
                let _ = self.mark_dirty();
            }
        }
    }

    /// Rename a door TYPE from the review: every door whose trimmed
    /// `type_name` is `old` takes `new`, across all storeys — doors group by
    /// their free-text label, so the label IS the group
    /// ([`Ber::rename_door_type`] owns the rule; empty/unchanged `new` is a
    /// no-op). Like [`rename_structure_group`](Self::rename_structure_group):
    /// display-only, no undo snapshot; marks dirty only when something
    /// actually renamed.
    ///
    /// [`Ber::rename_door_type`]: ber_model::ber::ber_struct::Ber
    pub fn rename_door_type(&mut self, old: String, new: String) {
        if let Some(record) = self.current_record.as_mut() {
            if record.data.rename_door_type(&old, &new) > 0 {
                let _ = self.mark_dirty();
            }
        }
    }

    /// Set door `id`'s survey fields from the review's door "Edit fields"
    /// form. This door only — doors have no shared type record.
    pub fn set_door_fields(
        &mut self,
        id: ItemId,
        kind: ber_model::ber::fenestration::DoorKind,
        draught_stripped: bool,
        certified: bool,
    ) {
        if let Some(record) = self.current_record.as_mut() {
            if record
                .data
                .update_door_fields(id, kind, draught_stripped, certified)
            {
                let _ = self.mark_dirty();
            }
        }
    }

    /// Rename a construction phase's nickname from the review (blank clears
    /// back to the default "Original" / "Extension N"). House-wide: every
    /// group in that phase shows the new name.
    pub fn rename_construction_phase(
        &mut self,
        phase: ber_model::ber::common::ConstructionPhase,
        name: String,
    ) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.construction_phases.set_nickname(phase, name);
            let _ = self.mark_dirty();
        }
    }

    /// MERGE construction phase `from` into `to` from the review — the model
    /// re-points every reference ([`Ber::merge_construction_phases`]); the
    /// per-storey phase editing regions are then reconstructed so the session
    /// state reflects the merged attribution.
    ///
    /// [`Ber::merge_construction_phases`]: ber_model::ber::ber_struct::Ber
    pub fn merge_structure_phase(
        &mut self,
        from: ber_model::ber::common::ConstructionPhase,
        to: ber_model::ber::common::ConstructionPhase,
    ) {
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        if !record.data.merge_construction_phases(&from, &to) {
            return;
        }
        let n = record.data.storeys.len();
        for idx in 0..n {
            self.reconstruct_phase_regions_for(idx);
        }
        let _ = self.mark_dirty();
    }

    /// Confirm "all's good": mark the dwelling Structure-reviewed (persisted) and
    /// close the review.
    pub fn confirm_structure_review(&mut self) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.structure_reviewed = true;
        }
        let _ = self.mark_dirty();
        self.set_structure_review_mode(false);
    }

    /// The current review item (clamped to the list), or `None` when no record is
    /// loaded or there's nothing to review.
    pub fn structure_review_view(&self) -> Option<StructureReviewView> {
        let mut items = self.structure_review_items();
        if items.is_empty() {
            return None;
        }
        let total = items.len();
        let cursor = self.structure_review_cursor.min(total - 1);
        let mut item = items.swap_remove(cursor);
        item.cursor = cursor;
        item.total = total;
        Some(item)
    }

    /// Build the flat whole-house review list, in order: wall constructions,
    /// floor constructions, roof constructions, window types, doors. Constructions
    /// / window types not referenced by any surface are skipped (nothing to show).
    pub(super) fn structure_review_items(&self) -> Vec<StructureReviewView> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let data = &record.data;
        let mut items = Vec::new();

        // ── Wall / floor / roof construction groups (house-wide) ──────────────
        for c in &data.wall_constructions {
            let ids = wall_region_ids(data, c.base.id);
            if ids.is_empty() {
                continue;
            }
            items.push(StructureReviewView {
                cursor: 0,
                total: 0,
                kind: StructureReviewKind::Wall,
                title: group_title(&c.base.name, c.construction_type.label()),
                detail_lines: construction_lines(
                    c.construction_type.label(),
                    &c.base,
                    &data.construction_phases,
                    ids.len(),
                    "Walls",
                ),
                photos: photo_views(&c.base.photos),
                highlight: ids,
                rename: Some(StructureReviewRename::Construction(rename_info(
                    &c.base,
                    &data.construction_phases,
                ))),
            });
        }
        for c in &data.floor_constructions {
            let ids = floor_region_ids(data, c.base.id);
            if ids.is_empty() {
                continue;
            }
            items.push(StructureReviewView {
                cursor: 0,
                total: 0,
                kind: StructureReviewKind::Floor,
                title: group_title(&c.base.name, c.construction_type.label()),
                detail_lines: construction_lines(
                    c.construction_type.label(),
                    &c.base,
                    &data.construction_phases,
                    ids.len(),
                    "Floors",
                ),
                photos: photo_views(&c.base.photos),
                highlight: ids,
                rename: Some(StructureReviewRename::Construction(rename_info(
                    &c.base,
                    &data.construction_phases,
                ))),
            });
        }
        for c in &data.ceiling_constructions {
            let ids = ceiling_region_ids(data, c.base.id);
            if ids.is_empty() {
                continue;
            }
            items.push(StructureReviewView {
                cursor: 0,
                total: 0,
                kind: StructureReviewKind::Roof,
                title: group_title(&c.base.name, c.construction_type.label()),
                detail_lines: construction_lines(
                    c.construction_type.label(),
                    &c.base,
                    &data.construction_phases,
                    ids.len(),
                    "Ceilings",
                ),
                photos: photo_views(&c.base.photos),
                highlight: ids,
                rename: Some(StructureReviewRename::Construction(rename_info(
                    &c.base,
                    &data.construction_phases,
                ))),
            });
        }

        // ── Window types (house-wide) ─────────────────────────────────────────
        for t in &data.window_types {
            let ids: HashSet<ItemId> = data
                .storeys
                .iter()
                .flat_map(|s| s.windows.iter())
                .filter(|w| w.window_type_id == t.id)
                .map(|w| w.id)
                .collect();
            if ids.is_empty() {
                continue;
            }
            items.push(StructureReviewView {
                cursor: 0,
                total: 0,
                kind: StructureReviewKind::WindowType,
                title: window_type_title(t),
                detail_lines: window_type_lines(t, ids.len()),
                photos: photo_views(&t.photos),
                highlight: ids,
                rename: Some(StructureReviewRename::WindowType {
                    id: t.id,
                    name: t.specifics.name().unwrap_or_default().to_string(),
                    placeholder: format!(
                        "{} · {}",
                        glazing_label(&t.glazing),
                        frame_label(&t.frame)
                    ),
                    error: self
                        .window_type_rename_error
                        .as_ref()
                        .filter(|(eid, _)| *eid == t.id)
                        .map(|(_, hint)| hint.clone()),
                    fields: WindowTypeFields {
                        glazing: t.glazing.clone(),
                        frame: t.frame.clone(),
                        year: t.year,
                        certified: t.certified,
                        draught_stripped: t.draught_stripped,
                        year_proof: t.year_proof,
                        gap: t.gap.clone(),
                    },
                }),
            });
        }

        // ── Doors (each individually, across all storeys) ─────────────────────
        for door in data.storeys.iter().flat_map(|s| s.doors.iter()) {
            items.push(StructureReviewView {
                cursor: 0,
                total: 0,
                kind: StructureReviewKind::Door,
                title: if door.type_name.trim().is_empty() {
                    "Door".to_string()
                } else {
                    door.type_name.clone()
                },
                detail_lines: vec![
                    ("Type".to_string(), door.type_name.clone()),
                    (
                        "Kind".to_string(),
                        door.kind
                            .as_ref()
                            .map(door_kind_label)
                            .unwrap_or("—")
                            .to_string(),
                    ),
                    ("Width".to_string(), format!("{:.2} m", door.width_m)),
                    ("Height".to_string(), format!("{:.2} m", door.height_m)),
                    (
                        "Draught stripped".to_string(),
                        yes_no_unknown(door.draught_stripped),
                    ),
                    ("Certified".to_string(), yes_no_unknown(door.certified)),
                ],
                photos: photo_views(&door.photos),
                highlight: std::iter::once(door.id).collect(),
                rename: Some(StructureReviewRename::Door {
                    id: door.id,
                    type_name: door.type_name.clone(),
                    fields: DoorFields {
                        kind: door.kind,
                        draught_stripped: door.draught_stripped,
                        certified: door.certified,
                    },
                }),
            });
        }

        items
    }
}

/// Ids of wall regions (across all storeys) referencing construction `cid`.
/// Walls out of structure scope (`WallExposure::in_structure_scope` —
/// non-heat-loss / inter-storey) are omitted: the review neither lists nor
/// highlights them, and a construction referenced only by such walls is
/// skipped entirely (its id set comes back empty).
fn wall_region_ids(data: &Ber, cid: ItemId) -> HashSet<ItemId> {
    data.storeys
        .iter()
        .flat_map(|s| s.walls.regions.iter())
        .filter(|r| r.exposure.in_structure_scope())
        .filter(|r| r.construction_id == cid)
        .map(|r| r.id)
        .collect()
}

/// Ids of floor regions (across all storeys) referencing construction `cid`.
/// Out-of-scope floors (party / derived inter-storey) are omitted — same
/// contract as [`wall_region_ids`].
fn floor_region_ids(data: &Ber, cid: ItemId) -> HashSet<ItemId> {
    data.storeys
        .iter()
        .flat_map(|s| s.floors.regions.iter())
        .filter(|r| r.exposure.in_structure_scope())
        .filter(|r| r.construction_id == cid)
        .map(|r| r.id)
        .collect()
}

/// Ids of roof/ceiling regions (across all storeys) referencing construction `cid`.
/// Out-of-scope ceilings (party / derived inter-storey) are omitted — same
/// contract as [`wall_region_ids`].
fn ceiling_region_ids(data: &Ber, cid: ItemId) -> HashSet<ItemId> {
    data.storeys
        .iter()
        .flat_map(|s| s.ceilings.regions.iter())
        .filter(|r| r.exposure.in_structure_scope())
        .filter(|r| r.construction_id == cid)
        .map(|r| r.id)
        .collect()
}

fn photo_views(photos: &[ber_model::ber::common::Evidence]) -> Vec<PhotoView> {
    photos
        .iter()
        .map(|p| PhotoView {
            id: p.id,
            file_path: p.file_path.clone(),
            thumbnail_path: p.thumbnail_path.clone(),
            description: p.description.clone(),
        })
        .collect()
}

/// Title for a construction group: its nickname if set, else the type label.
fn group_title(name: &Option<String>, type_label: &str) -> String {
    match name.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        Some(n) => n.to_string(),
        None => type_label.to_string(),
    }
}

/// The rename targets + prefills for one construction item.
fn rename_info(
    base: &ber_model::ber::structural::ConstructionBase,
    phases: &ber_model::ber::common::ConstructionPhases,
) -> StructureReviewConstructionRename {
    StructureReviewConstructionRename {
        group_id: base.id,
        group_name: base.name.clone().unwrap_or_default(),
        phase: base.construction_phase.clone(),
        phase_name: phases
            .nickname_of(&base.construction_phase)
            .unwrap_or_default()
            .to_string(),
        phase_default: base.construction_phase.default_name(),
        // Only a DEFINED phase can merge (the model guards it too) — an
        // Unassigned item offers nothing.
        merge_targets: if phases.is_defined(&base.construction_phase) {
            phases
                .years
                .keys()
                .filter(|p| **p != base.construction_phase)
                .map(|p| (p.clone(), phases.display_label(p)))
                .collect()
        } else {
            Vec::new()
        },
    }
}

fn construction_lines(
    type_label: &str,
    base: &ber_model::ber::structural::ConstructionBase,
    phases: &ber_model::ber::common::ConstructionPhases,
    surface_count: usize,
    surface_word: &str,
) -> Vec<(String, String)> {
    let mut lines = vec![
        ("Type".to_string(), type_label.to_string()),
        (
            "Make-up".to_string(),
            makeup_label(&base.construction_makeup),
        ),
        // Phase name WITH its year (e.g. "Extension 2 (2010)") — the year is
        // what dates the fabric, so the review shows it alongside the phase.
        (
            "Phase".to_string(),
            phases.display_label(&base.construction_phase),
        ),
    ];
    if let Some(n) = base.name.as_ref().filter(|s| !s.trim().is_empty()) {
        lines.push(("Name".to_string(), n.clone()));
    }
    lines.push((surface_word.to_string(), surface_count.to_string()));
    lines
}

fn window_type_title(t: &WindowType) -> String {
    match t.specifics.name() {
        Some(n) if !n.trim().is_empty() => n.to_string(),
        _ => format!("{} · {}", glazing_label(&t.glazing), frame_label(&t.frame)),
    }
}

fn window_type_lines(t: &WindowType, window_count: usize) -> Vec<(String, String)> {
    let mut lines = vec![
        ("Glazing".to_string(), glazing_label(&t.glazing)),
        ("Frame".to_string(), frame_label(&t.frame)),
    ];
    if t.year != 0 {
        lines.push(("Year".to_string(), t.year.to_string()));
    }
    lines.push((
        "Year proof".to_string(),
        if t.year_proof { "Yes" } else { "No" }.to_string(),
    ));
    if !t.gap.is_empty() {
        let gaps = t
            .gap
            .iter()
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("/");
        lines.push(("Gap".to_string(), format!("{gaps} mm")));
    }
    lines.push((
        "Certified".to_string(),
        if t.certified { "Yes" } else { "No" }.to_string(),
    ));
    if let Some(n) = t.specifics.name().filter(|s| !s.trim().is_empty()) {
        lines.push(("Name".to_string(), n.to_string()));
    }
    lines.push(("Windows".to_string(), window_count.to_string()));
    lines
}

fn makeup_label(m: &ConstructionMakeup) -> String {
    match m {
        ConstructionMakeup::NonHeatLoss => "No heat loss".to_string(),
        ConstructionMakeup::Default => "Default".to_string(),
        ConstructionMakeup::Tbd => "Not set".to_string(),
        ConstructionMakeup::Unknown => "Unknown".to_string(),
        // The dry-lining states (walls' combined ✓ dropdown): the slab is
        // always included, with or without extra insulation. A pb-less
        // DefaultPlus is the legacy bare added-insulation shape.
        ConstructionMakeup::DefaultPlus {
            extra_insulation_mm: 0,
            plasterboard: true,
        } => "Default + plasterboard".to_string(),
        ConstructionMakeup::DefaultPlus {
            extra_insulation_mm,
            plasterboard: true,
        } => {
            format!("Default + pb + {extra_insulation_mm}mm insulation")
        }
        ConstructionMakeup::DefaultPlus {
            extra_insulation_mm,
            plasterboard: false,
        } => {
            format!("Default + {extra_insulation_mm}mm insulation")
        }
        ConstructionMakeup::SomeInsulation => "Some insulation (unmeasurable)".to_string(),
        ConstructionMakeup::Layers(layers) => format!("Custom ({} layers)", layers.len()),
    }
}

fn glazing_label(g: &Glazing) -> String {
    match g {
        Glazing::Single => "Single".to_string(),
        Glazing::Double => "Double".to_string(),
        Glazing::Triple => "Triple".to_string(),
        Glazing::Other(s) => s.clone(),
    }
}

fn frame_label(f: &Frame) -> String {
    match f {
        Frame::WoodUPVC => "Wood/UPVC".to_string(),
        Frame::Metal => "Metal".to_string(),
        Frame::Other(s) => s.clone(),
    }
}

/// A tri-state survey flag for a detail line: unset (pre-field record) shows
/// as "—" so the reviewer sees it was never asked, not silently "No".
fn yes_no_unknown(v: Option<bool>) -> String {
    match v {
        Some(true) => "Yes".to_string(),
        Some(false) => "No".to_string(),
        None => "—".to_string(),
    }
}

fn door_kind_label(k: &ber_model::ber::fenestration::DoorKind) -> &'static str {
    use ber_model::ber::fenestration::DoorKind;
    match k {
        DoorKind::Solid => "Solid",
        DoorKind::HalfGlazed => "Half glazed",
        DoorKind::FullyGlazed => "Fully glazed",
        DoorKind::AssemblyPanel => "Assembly panel",
    }
}
