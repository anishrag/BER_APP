//! Structure-phase surface SELECTION: the app-shell interaction layer over the
//! current storey's resolved regions (walls / roofs / floors). The selected set
//! holds region [`ItemId`]s of the active [`SurfaceKind`]; it's seeded by the
//! Select subtools (`Select all` fills it with every surface, `Select
//! remaining` with only the ones not yet done, `Select` starts empty) and
//! refined by tapping or by carving a region to pick.
//!
//! The hit-testing/geometry itself lives one crate down: which surface a tap
//! lands on and which surfaces sit inside a carved region are pure read-only
//! queries over the BER `Storey` data model — see [`Storey::surface_at`],
//! [`Storey::surfaces_in_ring`], [`Storey::surface_ids`] in `ber_model`. This
//! module only sequences those queries, drives the carve through `document`,
//! and holds the transient selected set (app-shell interaction state — the
//! regions live on the BER record, not the editing `Document`).
use super::*;
use ber_model::ber::common::{ConstructionPhase, Photo};
use ber_model::ber::structural::{
    CeilingConstructionType, CeilingExposure, ConstructionMakeup, ConstructionType,
    FloorConstructionType, FloorExposure, Storey, SurfaceExposure, WallExposure,
};
use domain::points::{Point2f32, Point2f64};

/// One attribute-GROUP in the exploded "Edit attributes" session: the selected
/// surfaces that currently share a construction, plus the `(type, make-up)` the
/// popup edits. `ty` is kind-tagged (`ConstructionType`) so one group type spans
/// walls/floors/ceilings. `phase` is the derived axis (floor zones), read-only.
#[derive(Debug, Clone)]
pub struct EditGroup {
    pub ids: Vec<ItemId>,
    pub ty: ConstructionType,
    pub makeup: ConstructionMakeup,
    pub phase: ConstructionPhase,
    /// The boundary-condition exposure shared by every surface in the group,
    /// kind-tagged ([`SurfaceExposure`]) so the dropdown spans walls/floors/
    /// ceilings. Part of the grouping key, so same-construction-but-different-
    /// exposure surfaces split into separate groups.
    pub exposure: SurfaceExposure,
    /// Walls only: `true` when the group's wall(s) are FACE-PARTITIONED (their
    /// exposure is owned per-2-D-region by the wall partition tool, not by a
    /// single along-wall value) — the dropdown is shown disabled. Always
    /// `false` for floors/ceilings.
    pub exposure_locked: bool,
    /// Editable label for the group, defaulting to "Group 1", "Group 2", … —
    /// the assessor can rename it. Session-scoped (reset when the edit reopens).
    pub name: String,
    /// The group construction's attached photos (session copy, seeded from the
    /// construction; the photos view shows these and the camera button appends).
    pub photos: Vec<Photo>,
}

/// The in-flight "Edit attributes" session: the current selection partitioned by
/// shared construction, plus which group's popup is active. The session can span
/// several surface kinds at once (Walls + Floors + …) — each group's kind is
/// carried by its `ty` ([`ConstructionType::kind`]), so the session itself isn't
/// tied to one kind.
#[derive(Debug, Clone)]
pub struct StructureEdit {
    pub groups: Vec<EditGroup>,
    pub active: usize,
    /// When `Some(i)`, the build-up LAYER editor is open for group `i` (the
    /// "Specify…" flow): the canvas shows that editor — type a layer or pick from
    /// the reusable library, ordered inside→outside — instead of the group's
    /// normal attribute window. `None` = no layer editor open.
    pub specifying: Option<usize>,
    /// When `Some(i)`, the PHOTOS view is open for group `i` (the camera button):
    /// the canvas shows that group's photo thumbnails with a `+` to add more.
    pub viewing_photos: Option<usize>,
    /// Canvas presentation of the session. `false` (default) keeps the plan in
    /// place — zoomed out, each group recoloured where it sits, with one full
    /// attribute window floating over each group. `true` is the exploded "fan"
    /// view (groups pulled radially out from the centre) — wired but not yet the
    /// default; a future toggle flips this.
    pub exploded: bool,
}

/// A construction already in use on the current storey — offered in the attribute
/// editor so a selection can JOIN an existing group instead of re-entering its
/// attributes. Picking one applies its `(ty, make-up, phase)` to the active group
/// (so they share one construction / DEAP group). `name` is the construction's
/// stored label, for the pick list.
#[derive(Debug, Clone)]
pub struct ExistingGroup {
    pub ty: ConstructionType,
    pub makeup: ConstructionMakeup,
    pub phase: ConstructionPhase,
    pub name: Option<String>,
}

impl AppController {
    /// The Structure-phase selected region ids — the canvas shades exactly these.
    pub fn structure_selected_ids(&self) -> &HashSet<ItemId> {
        &self.structure_selected
    }

    /// Whether the WHOLE house's Structure attribution is complete: every
    /// storey's walls, roofs and floors are fully attributed. Gates the
    /// Structure-phase "Review" entry. `false` with no record / no storeys.
    pub fn structure_complete(&self) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        let b = &record.data;
        !b.storeys.is_empty()
            && (0..b.storeys.len()).all(|i| {
                b.structure_surfaces_complete(i, SurfaceKind::Walls)
                    && b.structure_surfaces_complete(i, SurfaceKind::Roofs)
                    && b.structure_surfaces_complete(i, SurfaceKind::Floors)
            })
    }

    /// Distinct COMPLETE constructions already assigned to the current storey's
    /// surfaces (walls, floors, ceilings) — the "existing groups" the attribute
    /// editor offers so a selection can join one rather than re-typing its
    /// attributes. Each `ty` is kind-tagged, so the editor filters to its kind.
    pub fn structure_existing_groups(&self) -> Vec<ExistingGroup> {
        fn add(
            out: &mut Vec<ExistingGroup>,
            ty: ConstructionType,
            makeup: ConstructionMakeup,
            phase: ConstructionPhase,
            name: Option<String>,
        ) {
            if out
                .iter()
                .any(|g| g.ty == ty && g.makeup == makeup && g.phase == phase)
            {
                return;
            }
            out.push(ExistingGroup {
                ty,
                makeup,
                phase,
                name,
            });
        }
        let mut out = Vec::new();
        let Some(idx) = self.current_storey_idx else {
            return out;
        };
        let Some(record) = self.current_record.as_ref() else {
            return out;
        };
        let Some(storey) = record.data.storeys.get(idx) else {
            return out;
        };
        for r in &storey.walls.regions {
            if let Some(c) = record.data.wall_construction(r.construction_id) {
                if c.is_complete() {
                    add(
                        &mut out,
                        ConstructionType::Wall(c.construction_type),
                        c.base.construction_makeup.clone(),
                        c.base.construction_phase.clone(),
                        c.base.name.clone(),
                    );
                }
            }
        }
        for r in &storey.floors.regions {
            if let Some(c) = record.data.floor_construction(r.construction_id) {
                if c.is_complete() {
                    add(
                        &mut out,
                        ConstructionType::Floor(c.construction_type),
                        c.base.construction_makeup.clone(),
                        c.base.construction_phase.clone(),
                        c.base.name.clone(),
                    );
                }
            }
        }
        for r in &storey.roofs.regions {
            if let Some(c) = record.data.ceiling_construction(r.construction_id) {
                if c.is_complete() {
                    add(
                        &mut out,
                        ConstructionType::Ceiling(c.construction_type),
                        c.base.construction_makeup.clone(),
                        c.base.construction_phase.clone(),
                        c.base.name.clone(),
                    );
                }
            }
        }
        out
    }

    /// Assign a wall CONSTRUCTION (type + make-up + phase) to the given resolved
    /// WALL region ids, keyed by each wall's parametric `SurfaceRef` so it
    /// survives the re-derivation. `phase` defaults to the derived one but is
    /// editable; the next materialize stamps it (`stamp_wall_constructions`).
    /// Snapshots for undo.
    pub fn set_wall_construction_for(
        &mut self,
        ids: &[ItemId],
        ty: ber_model::ber::structural::WallConstructionType,
        makeup: ber_model::ber::structural::ConstructionMakeup,
        phase: ConstructionPhase,
    ) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        // Map each resolved wall id to its durable key: a perimeter wall by its
        // parametric SurfaceRef (base midpoint), a RISER (interior step face, no
        // perimeter ref) by its quantized base edge — mirroring riser exposure, so
        // step-face constructions survive the re-derivation too.
        #[allow(clippy::type_complexity)]
        let (refs, riser_keys): (Vec<manip_plan::SurfaceRef>, Vec<document::RiserKey>) = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            let plan = &self.document.storeys[idx].outline.floor_plan;
            let mut refs = Vec::new();
            let mut risers = Vec::new();
            for r in ids
                .iter()
                .filter_map(|id| storey.walls.regions.iter().find(|r| r.id == *id))
            {
                let Some((e0, e1)) = r.base_edge() else {
                    continue;
                };
                let mid =
                    Point2f32::new(((e0.x + e1.x) * 0.5) as f32, ((e0.y + e1.y) * 0.5) as f32);
                if let Some(sref) = plan.surface_ref_at(mid, 0.05) {
                    refs.push(sref);
                } else {
                    risers.push(document::riser_key(
                        Point2f64::new(e0.x, e0.y),
                        Point2f64::new(e1.x, e1.y),
                    ));
                }
            }
            (refs, risers)
        };
        if refs.is_empty() && riser_keys.is_empty() {
            return;
        }
        self.record_history();
        let sd = &mut self.document.storeys[idx];
        for r in refs {
            sd.wall_constructions
                .insert(r, (ty, makeup.clone(), phase.clone()));
        }
        for k in riser_keys {
            sd.riser_constructions
                .insert(k, (ty, makeup.clone(), phase.clone()));
        }
        let _ = self.mark_dirty();
    }

    /// Assign a FLOOR construction (type + make-up) to the given resolved FLOOR
    /// region ids, keyed by each region's plan [`centroid`](ber_model::ber::structural::FloorRegion::centroid)
    /// so it survives the re-derivation. The PHASE axis stays derived; the next
    /// materialize composes the two (`document::stamp_floor_constructions`).
    pub fn set_floor_construction_for(
        &mut self,
        ids: &[ItemId],
        ty: FloorConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let keys: Vec<document::CentroidKey> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            ids.iter()
                .filter_map(|id| storey.floors.regions.iter().find(|r| r.id == *id))
                .map(|r| document::centroid_key(r.centroid()))
                .collect()
        };
        if keys.is_empty() {
            return;
        }
        self.record_history();
        for k in keys {
            self.document.storeys[idx]
                .floor_constructions
                .insert(k, (ty, makeup.clone(), phase.clone()));
        }
        let _ = self.mark_dirty();
    }

    /// Assign a CEILING/ROOF construction (type + make-up) to the given resolved
    /// ROOF region ids, keyed by each region's plan centroid. Mirror of
    /// [`set_floor_construction_for`](Self::set_floor_construction_for) over the
    /// storey's `roofs` regions (`document::stamp_ceiling_constructions`).
    pub fn set_ceiling_construction_for(
        &mut self,
        ids: &[ItemId],
        ty: CeilingConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let keys: Vec<document::CentroidKey> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            ids.iter()
                .filter_map(|id| storey.roofs.regions.iter().find(|r| r.id == *id))
                .map(|r| document::centroid_key(r.centroid()))
                .collect()
        };
        if keys.is_empty() {
            return;
        }
        self.record_history();
        for k in keys {
            self.document.storeys[idx]
                .ceiling_constructions
                .insert(k, (ty, makeup.clone(), phase.clone()));
        }
        let _ = self.mark_dirty();
    }

    /// Set the boundary-condition EXPOSURE of an attribute group and apply it to
    /// that group's surfaces, dispatching to the right durable store by the
    /// exposure value's kind. Mirror of
    /// [`set_structure_group_attrs`](Self::set_structure_group_attrs) for the
    /// exposure dropdown. A locked (face-partitioned) wall group is left to the
    /// per-region wall-partition tool — the UI disables the dropdown, but skip
    /// here too as belt-and-braces. A kind mismatch between the picked exposure
    /// and the group's kind is a no-op (shouldn't happen — the dropdown only
    /// offers this kind's variants). Updates the in-session `group.exposure` so
    /// the dropdown reflects the new value immediately.
    pub fn set_structure_group_exposure(&mut self, group_index: usize, exposure: SurfaceExposure) {
        let (kind, ids, locked) = {
            let Some(e) = self.structure_edit.as_ref() else {
                return;
            };
            let Some(g) = e.groups.get(group_index) else {
                return;
            };
            // The picked exposure must match the group's kind; otherwise no-op.
            if exposure.kind() != g.ty.kind() {
                return;
            }
            (g.ty.kind(), g.ids.clone(), g.exposure_locked)
        };
        match (kind, exposure) {
            (SurfaceKind::Floors, SurfaceExposure::Floor(e)) => {
                self.set_floor_exposure_for_ids(&ids, e)
            }
            (SurfaceKind::Roofs, SurfaceExposure::Ceiling(e)) => {
                self.set_ceiling_exposure_for_ids(&ids, e)
            }
            (SurfaceKind::Walls, SurfaceExposure::Wall(e)) => {
                if locked {
                    return; // face-partitioned: owned per-region by the partition tool
                }
                self.set_wall_exposure_for_ids(&ids, e)
            }
            _ => return, // kind/variant mismatch — no-op
        }
        // Reflect the new value in the session group so the dropdown updates.
        if let Some(g) = self
            .structure_edit
            .as_mut()
            .and_then(|e| e.groups.get_mut(group_index))
        {
            g.exposure = exposure;
        }
    }

    /// Set the FLOOR exposure of the given resolved FLOOR region ids, keyed by
    /// each region's plan centroid (`document.floor_exposure`) so it survives
    /// re-derivation (`stamp_floor_exposure`). Mirror of
    /// [`set_floor_construction_for`](Self::set_floor_construction_for). One
    /// snapshot for the whole write.
    fn set_floor_exposure_for_ids(&mut self, ids: &[ItemId], exposure: FloorExposure) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let keys: Vec<document::CentroidKey> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            ids.iter()
                .filter_map(|id| storey.floors.regions.iter().find(|r| r.id == *id))
                .map(|r| document::centroid_key(r.centroid()))
                .collect()
        };
        if keys.is_empty() {
            return;
        }
        self.record_history();
        for k in keys {
            self.document.storeys[idx]
                .floor_exposure
                .insert(k, exposure);
        }
        let _ = self.mark_dirty();
    }

    /// Set the CEILING/ROOF exposure of the given resolved ROOF region ids,
    /// keyed by each region's plan centroid (`document.ceiling_exposure`,
    /// `stamp_ceiling_exposure`). Mirror of
    /// [`set_floor_exposure_for_ids`](Self::set_floor_exposure_for_ids) over the
    /// storey's `roofs`.
    fn set_ceiling_exposure_for_ids(&mut self, ids: &[ItemId], exposure: CeilingExposure) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let keys: Vec<document::CentroidKey> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            ids.iter()
                .filter_map(|id| storey.roofs.regions.iter().find(|r| r.id == *id))
                .map(|r| document::centroid_key(r.centroid()))
                .collect()
        };
        if keys.is_empty() {
            return;
        }
        self.record_history();
        for k in keys {
            self.document.storeys[idx]
                .ceiling_exposure
                .insert(k, exposure);
        }
        let _ = self.mark_dirty();
    }

    /// Set the WALL exposure of the given resolved WALL region ids. For each
    /// PERIMETER region, recover its perimeter `VertexId` + along-wall span
    /// (`document::StoreyDoc::recover_wall_region_span`) and repaint ONLY that
    /// span (`set_wall_section`), so other sections on the same wall are
    /// preserved. A FACE-PARTITIONED wall's region is SKIPPED (owned per-region
    /// by the partition tool). A region with NO perimeter match is a RISER → its
    /// override goes in `document.riser_exposure` keyed by the base edge. One
    /// snapshot for the whole group write.
    fn set_wall_exposure_for_ids(&mut self, ids: &[ItemId], exposure: WallExposure) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        // Resolve each id to (perimeter span | riser key) up front, reading the
        // resolved record + the document, before the mutable document borrow.
        enum WallWrite {
            Section(manip_plan::VertexId, f32, f32, f32), // vid, start_m, end_m, len
            Riser(document::RiserKey),
        }
        let writes: Vec<WallWrite> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            let Some(doc) = self.document.storeys.get(idx) else {
                return;
            };
            ids.iter()
                .filter_map(|id| storey.walls.regions.iter().find(|r| r.id == *id))
                .filter_map(|r| match doc.recover_wall_region_span(r) {
                    Some((vid, start_m, end_m, len)) => {
                        // Face-partitioned walls are owned per-2-D-region — skip.
                        if doc.is_wall_partitioned(vid) {
                            None
                        } else {
                            Some(WallWrite::Section(vid, start_m, end_m, len))
                        }
                    }
                    // No perimeter match → riser: key by its base edge.
                    None => r.base_edge().map(|(a, b)| {
                        WallWrite::Riser(document::riser_key(
                            domain::points::Point2f64::new(a.x, a.y),
                            domain::points::Point2f64::new(b.x, b.y),
                        ))
                    }),
                })
                .collect()
        };
        if writes.is_empty() {
            return;
        }
        self.record_history();
        for w in writes {
            match w {
                WallWrite::Section(vid, start_m, end_m, len) => {
                    self.document.storeys[idx].set_wall_section(vid, start_m, end_m, len, exposure);
                }
                WallWrite::Riser(key) => {
                    self.document.storeys[idx]
                        .riser_exposure
                        .insert(key, exposure);
                }
            }
        }
        let _ = self.mark_dirty();
    }

    /// The in-flight "Edit attributes" session, if any.
    pub fn structure_edit(&self) -> Option<&StructureEdit> {
        self.structure_edit.as_ref()
    }

    /// Open the "Edit attributes" session: partition the current selection by
    /// shared construction `(type, make-up, phase)` into groups (each editable
    /// separately). Works for the active surface kind — walls, floors, or
    /// ceilings/roofs. No-op for an empty selection or no storey.
    pub fn begin_structure_edit(&mut self) {
        // "Edit attributes" is when we commit to the selection — NOW perform any
        // deferred carve splits (turning staged rings into real sub-regions and
        // selecting them), so selecting a carve never changed geometry on its own.
        self.commit_pending_structure_carves();
        if self.structure_selected.is_empty() {
            return;
        }
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let Some(storey) = record.data.storeys.get(idx) else {
            return;
        };
        // The selection can span kinds (Walls + Floors + …); each id's kind is
        // detected from the storey, then its construction's (type-kind, make-up,
        // phase) read and tagged with `ConstructionType` so the rest of the
        // editor stays kind-agnostic. A region whose construction is missing
        // reads as that kind's default type + default make-up / Original phase.
        type Attrs = (
            ConstructionType,
            ConstructionMakeup,
            ConstructionPhase,
            SurfaceExposure,
            bool, // exposure_locked
            Option<String>,
            Vec<Photo>,
        );
        let Some(doc) = self.document.storeys.get(idx) else {
            return;
        };
        let attrs_of = |id: ItemId| -> Option<Attrs> {
            match storey.surface_kind_of(id)? {
                SurfaceKind::Walls => {
                    let r = storey.walls.regions.iter().find(|r| r.id == id)?;
                    // A perimeter wall whose recovered VertexId is face-partitioned
                    // is locked; a riser (no perimeter match) is never locked.
                    let locked = doc
                        .recover_wall_region_span(r)
                        .map(|(vid, ..)| doc.is_wall_partitioned(vid))
                        .unwrap_or(false);
                    let (ty, makeup, phase, name, photos) =
                        match record.data.wall_construction(r.construction_id) {
                            Some(c) => (
                                ConstructionType::Wall(c.construction_type),
                                c.base.construction_makeup.clone(),
                                c.base.construction_phase.clone(),
                                c.base.name.clone(),
                                c.base.photos.clone(),
                            ),
                            None => (
                                ConstructionType::Wall(Default::default()),
                                Default::default(),
                                Default::default(),
                                None,
                                Vec::new(),
                            ),
                        };
                    Some((
                        ty,
                        makeup,
                        phase,
                        SurfaceExposure::Wall(r.exposure),
                        locked,
                        name,
                        photos,
                    ))
                }
                SurfaceKind::Floors => {
                    let r = storey.floors.regions.iter().find(|r| r.id == id)?;
                    let (ty, makeup, phase, name, photos) =
                        match record.data.floor_construction(r.construction_id) {
                            Some(c) => (
                                ConstructionType::Floor(c.construction_type),
                                c.base.construction_makeup.clone(),
                                c.base.construction_phase.clone(),
                                c.base.name.clone(),
                                c.base.photos.clone(),
                            ),
                            None => (
                                ConstructionType::Floor(Default::default()),
                                Default::default(),
                                Default::default(),
                                None,
                                Vec::new(),
                            ),
                        };
                    Some((
                        ty,
                        makeup,
                        phase,
                        SurfaceExposure::Floor(r.exposure),
                        false,
                        name,
                        photos,
                    ))
                }
                SurfaceKind::Roofs => {
                    let r = storey.roofs.regions.iter().find(|r| r.id == id)?;
                    let (ty, makeup, phase, name, photos) =
                        match record.data.ceiling_construction(r.construction_id) {
                            Some(c) => (
                                ConstructionType::Ceiling(c.construction_type),
                                c.base.construction_makeup.clone(),
                                c.base.construction_phase.clone(),
                                c.base.name.clone(),
                                c.base.photos.clone(),
                            ),
                            None => (
                                ConstructionType::Ceiling(Default::default()),
                                Default::default(),
                                Default::default(),
                                None,
                                Vec::new(),
                            ),
                        };
                    Some((
                        ty,
                        makeup,
                        phase,
                        SurfaceExposure::Ceiling(r.exposure),
                        false,
                        name,
                        photos,
                    ))
                }
            }
        };
        let mut groups: Vec<EditGroup> = Vec::new();
        for id in &self.structure_selected {
            // Skip ids that no longer resolve to a live region of any kind.
            let Some((ty, makeup, phase, exposure, exposure_locked, cname, photos)) = attrs_of(*id)
            else {
                continue;
            };
            // `exposure_locked` is part of the key so a face-partitioned wall
            // (locked) never shares a group with an editable wall of the same
            // construction — the dropdown's enabled-ness must be uniform per group.
            match groups.iter_mut().find(|g| {
                g.ty == ty
                    && g.makeup == makeup
                    && g.phase == phase
                    && g.exposure == exposure
                    && g.exposure_locked == exposure_locked
            }) {
                Some(g) => g.ids.push(*id),
                None => groups.push(EditGroup {
                    ids: vec![*id],
                    ty,
                    makeup,
                    phase,
                    exposure,
                    exposure_locked,
                    // The construction's stored name, if any (set in a prior
                    // session); the "Group N" fallback is filled in below.
                    name: cname.unwrap_or_default(),
                    photos,
                }),
            }
        }
        // Any group without a stored name defaults to "Group N" (1-based).
        for (i, g) in groups.iter_mut().enumerate() {
            if g.name.trim().is_empty() {
                g.name = format!("Group {}", i + 1);
            }
        }
        if !groups.is_empty() {
            self.structure_edit = Some(StructureEdit {
                groups,
                active: 0,
                exploded: false,
                specifying: None,
                viewing_photos: None,
            });
        }
    }

    /// The app-wide reusable construction-layer library (the build-up editor's
    /// "previously typed layers" pick list).
    pub fn construction_layer_library(&self) -> &[ber_model::ber::structural::ConstructionLayer] {
        &self.layer_library
    }

    /// Open the build-up LAYER editor for `group` (the "Specify…" flow). Ensures
    /// the group's make-up is `Layers` (starting empty if it was something else),
    /// then marks the editor open.
    pub fn begin_specify_layers(&mut self, group: usize) {
        let makeup = self
            .structure_edit
            .as_ref()
            .and_then(|e| e.groups.get(group))
            .map(|g| g.makeup.clone());
        let Some(makeup) = makeup else {
            return;
        };
        if !matches!(makeup, ConstructionMakeup::Layers(_)) {
            self.set_structure_group_layers(group, Vec::new());
        }
        if let Some(e) = self.structure_edit.as_mut() {
            e.specifying = Some(group);
        }
    }

    /// Close the build-up layer editor.
    pub fn end_specify_layers(&mut self) {
        if let Some(e) = self.structure_edit.as_mut() {
            e.specifying = None;
        }
    }

    /// Replace `group`'s build-up with `layers` (its make-up becomes
    /// `Layers(layers)`) and apply it durably. Also saves each NAMED layer to the
    /// app-wide library (idempotent) so a typed layer becomes pickable next time —
    /// the build-up editor commits the whole list here on close.
    pub fn set_structure_group_layers(
        &mut self,
        group: usize,
        layers: Vec<ber_model::ber::structural::ConstructionLayer>,
    ) {
        let Some((ty, phase)) = self
            .structure_edit
            .as_ref()
            .and_then(|e| e.groups.get(group))
            .map(|g| (g.ty, g.phase.clone()))
        else {
            return;
        };
        // Persist any named layers to the reusable library + refresh the cache.
        let repo = crate::engine::ber_repository::BerRepository::new(&self.conn);
        let mut added = false;
        for l in &layers {
            if !l.name().trim().is_empty() && repo.add_construction_layer(l).is_ok() {
                added = true;
            }
        }
        if added {
            self.layer_library = repo.list_construction_layers().unwrap_or_default();
        }
        self.set_structure_group_attrs(group, ty, ConstructionMakeup::Layers(layers), phase);
    }

    /// Copy a source image into the photo store and return a `Photo` pointing at
    /// the stored copy. The MODULAR core every photo-attach site shares (file
    /// I/O lives here, not in the UI). `None` on an I/O failure.
    ///
    /// `quality` is the per-site tier: the compact tiers downscale + re-encode
    /// the capture toward a target byte size (most shots don't need full
    /// resolution); `Full` keeps the original bytes verbatim (text/OCR shots).
    pub(super) fn store_photo_file(
        &self,
        source: &std::path::Path,
        quality: PhotoQuality,
    ) -> Option<ber_model::ber::common::Photo> {
        std::fs::create_dir_all(&self.photos_dir).ok()?;
        // Canonicalize to an ABSOLUTE dir so the stored path loads as a
        // `file://` thumbnail regardless of the process's cwd.
        let dir =
            std::fs::canonicalize(&self.photos_dir).unwrap_or_else(|_| self.photos_dir.clone());
        let id = ItemId::default();
        // Recompression is an ANDROID concern: phone cameras hand us full-res
        // ~12 MP JPEGs, so each compact tier downscales + re-encodes once at a
        // fixed quality. On desktop (dev) the picked file is stored verbatim —
        // no point spending the decode/encode there. `Full`, the desktop path,
        // and any decode/encode failure all keep the original bytes (extension
        // and all).
        let spec = if cfg!(target_os = "android") {
            quality.compression()
        } else {
            None
        };
        let dest = match spec {
            Some((max_dim, jpeg_quality)) => {
                let dest = dir.join(format!("{}.jpg", id.0));
                if Self::recompress(source, &dest, max_dim, jpeg_quality) {
                    Some(dest)
                } else {
                    None
                }
            }
            None => None,
        };
        let dest = match dest {
            Some(dest) => dest,
            None => {
                let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("jpg");
                let dest = dir.join(format!("{}.{ext}", id.0));
                std::fs::copy(source, &dest).ok()?;
                dest
            }
        };
        // Generate a small preview. Phone photos are ~12 MP; rendering the
        // full-res original as a thumbnail is slow to decode and costs ~48 MB
        // of texture each — unworkable on a tablet. Best-effort: on failure we
        // leave `thumbnail_path` None and fall back to the original.
        let thumb_dest = dir.join(format!("{}_thumb.jpg", id.0));
        let thumbnail_path = Self::write_thumbnail(&dest, &thumb_dest)
            .then(|| thumb_dest.to_string_lossy().into_owned());
        Some(ber_model::ber::common::Photo {
            id,
            file_path: dest.to_string_lossy().into_owned(),
            thumbnail_path,
            description: None,
            // Construction build-up photos have no elevation side (that's only
            // for the Mapping-phase external Front/Right/Left/Back photos).
            side: None,
        })
    }

    /// Decode `src`, downscale to fit `THUMB_MAX` px on the long edge, and write
    /// a JPEG to `dest`. Returns `false` (and writes nothing) on any failure.
    fn write_thumbnail(src: &std::path::Path, dest: &std::path::Path) -> bool {
        const THUMB_MAX: u32 = 480;
        let Ok(reader) = image::ImageReader::open(src).and_then(|r| r.with_guessed_format()) else {
            return false;
        };
        let Ok(img) = reader.decode() else {
            return false;
        };
        // `thumbnail` preserves aspect ratio and fits within the bounds.
        let thumb = img.thumbnail(THUMB_MAX, THUMB_MAX);
        thumb.to_rgb8().save(dest).is_ok()
    }

    /// Decode `src`, downscale to fit `max_dim` px on the long edge, and write a
    /// JPEG to `dest` at `jpeg_quality` (1..=100) in a SINGLE encode pass.
    /// Returns `false` and writes nothing on any decode/encode failure, so the
    /// caller can fall back to storing the original bytes verbatim.
    pub(super) fn recompress(
        src: &std::path::Path,
        dest: &std::path::Path,
        max_dim: u32,
        jpeg_quality: u8,
    ) -> bool {
        use image::ImageEncoder;
        let Ok(reader) = image::ImageReader::open(src).and_then(|r| r.with_guessed_format()) else {
            return false;
        };
        let Ok(img) = reader.decode() else {
            return false;
        };
        // `thumbnail` only ever shrinks (keeps aspect ratio); flatten to RGB so
        // the JPEG encoder always accepts the buffer (no alpha channel).
        let scaled = img.thumbnail(max_dim, max_dim).to_rgb8();
        let (w, h) = (scaled.width(), scaled.height());
        let mut buf = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, jpeg_quality);
        if encoder
            .write_image(scaled.as_raw(), w, h, image::ExtendedColorType::Rgb8)
            .is_err()
        {
            return false;
        }
        std::fs::write(dest, buf).is_ok()
    }

    /// Attach a photo (from `source`) to a Structure group's construction(s) —
    /// the build-up floaty's camera button. Stores the file, then pushes the
    /// `Photo` onto each of the group's (shared) constructions. Marks dirty.
    pub fn add_group_photo(
        &mut self,
        group: usize,
        source: &std::path::Path,
        quality: PhotoQuality,
    ) {
        let Some(photo) = self.store_photo_file(source, quality) else {
            return;
        };
        let (kind, ids) = match self
            .structure_edit
            .as_ref()
            .and_then(|e| e.groups.get(group))
        {
            Some(g) => (g.ty.kind(), g.ids.clone()),
            None => return,
        };
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let cids: Vec<ItemId> = {
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            let region_cid = |id: ItemId| -> Option<ItemId> {
                match kind {
                    SurfaceKind::Walls => storey
                        .walls
                        .regions
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.construction_id),
                    SurfaceKind::Floors => storey
                        .floors
                        .regions
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.construction_id),
                    SurfaceKind::Roofs => storey
                        .roofs
                        .regions
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.construction_id),
                }
            };
            let mut seen = HashSet::new();
            ids.iter()
                .filter_map(|id| region_cid(*id))
                .filter(|c| seen.insert(*c))
                .collect()
        };
        for cid in cids {
            record.data.add_construction_photo(kind, cid, photo.clone());
        }
        // Mirror onto the session group so the open photos view updates at once.
        if let Some(g) = self
            .structure_edit
            .as_mut()
            .and_then(|e| e.groups.get_mut(group))
        {
            g.photos.push(photo);
        }
        let _ = self.mark_dirty();
    }

    /// Open / close the PHOTOS view for a group (the camera button): shows the
    /// group's thumbnails with a `+` to add more.
    pub fn begin_view_photos(&mut self, group: usize) {
        if let Some(e) = self.structure_edit.as_mut() {
            if group < e.groups.len() {
                e.viewing_photos = Some(group);
            }
        }
    }

    pub fn end_view_photos(&mut self) {
        if let Some(e) = self.structure_edit.as_mut() {
            e.viewing_photos = None;
        }
    }

    /// Rename a group's editable label — the in-place window title AND the
    /// group's construction NAME, so the label survives reopening the editor (and
    /// reload). Updates the session group, then writes the name onto each of the
    /// group's constructions (`Ber::set_construction_name`). No undo snapshot
    /// (cosmetic); marks dirty so the rename persists.
    pub fn set_structure_group_name(&mut self, group: usize, name: String) {
        let (kind, ids) = {
            let Some(e) = self.structure_edit.as_mut() else {
                return;
            };
            let Some(g) = e.groups.get_mut(group) else {
                return;
            };
            g.name = name.clone();
            (g.ty.kind(), g.ids.clone())
        };
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        // Each region in the group shares one construction (grouped by it); map
        // the region ids to their construction ids, then name those.
        let cids: Vec<ItemId> = {
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            let region_cid = |id: ItemId| -> Option<ItemId> {
                match kind {
                    SurfaceKind::Walls => storey
                        .walls
                        .regions
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.construction_id),
                    SurfaceKind::Floors => storey
                        .floors
                        .regions
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.construction_id),
                    SurfaceKind::Roofs => storey
                        .roofs
                        .regions
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.construction_id),
                }
            };
            let mut seen = std::collections::HashSet::new();
            ids.iter()
                .filter_map(|id| region_cid(*id))
                .filter(|c| seen.insert(*c))
                .collect()
        };
        let stored = if name.trim().is_empty() {
            None
        } else {
            Some(name)
        };
        for cid in cids {
            record.data.set_construction_name(kind, cid, stored.clone());
        }
        let _ = self.mark_dirty();
    }

    /// Switch which attribute-group's popup is active (one at a time).
    pub fn set_active_structure_group(&mut self, i: usize) {
        if let Some(e) = self.structure_edit.as_mut() {
            if i < e.groups.len() {
                e.active = i;
            }
        }
    }

    /// Set a group's construction `(type, make-up, phase)` and apply it to that
    /// group's surfaces, dispatching to the right durable store by the type's
    /// kind (writes the store, snapshots for undo). `phase` defaults to the
    /// group's derived value but is editable in the attribute editor.
    pub fn set_structure_group_attrs(
        &mut self,
        group: usize,
        ty: ConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) {
        let ids = {
            let Some(e) = self.structure_edit.as_mut() else {
                return;
            };
            let Some(g) = e.groups.get_mut(group) else {
                return;
            };
            g.ty = ty;
            g.makeup = makeup.clone();
            g.phase = phase.clone();
            g.ids.clone()
        };
        match ty {
            ConstructionType::Wall(t) => self.set_wall_construction_for(&ids, t, makeup, phase),
            ConstructionType::Floor(t) => self.set_floor_construction_for(&ids, t, makeup, phase),
            ConstructionType::Ceiling(t) => {
                self.set_ceiling_construction_for(&ids, t, makeup, phase)
            }
        }
    }

    /// Close the "Edit attributes" session (attributes apply as they're set).
    /// When the `Remaining` subtool is active this is the "work until done" loop:
    /// re-seed it so the surfaces just completed drop out and the selection
    /// shrinks toward empty (materialize first so the re-resolved constructions
    /// the seed reads are current).
    pub fn end_structure_edit(&mut self) {
        self.close_structure_edit();
        if self.structure_select == Some(SelectMode::Remaining) {
            self.materialize();
            self.reseed_structure_selection();
        }
    }

    /// Tear down any open "Edit attributes" session. Safe to call when none is
    /// open. The single close path for every clear site (Done, phase switch, kind
    /// switch). The in-place edit view is UI-computed (the canvas fits the plan
    /// around the floating windows), so closing touches no controller view state.
    pub(super) fn close_structure_edit(&mut self) {
        self.structure_edit = None;
    }

    /// Toggle the surface under a tapped point (world metres) in/out of the
    /// selection. The active [`StructureSelection::kind`] decides which regions
    /// are hit-tested; no-op when no Select subtool is active.
    pub fn toggle_structure_surface_at(&mut self, world_x: f64, world_y: f64, tol: f64) {
        if self.structure_select.is_none() {
            return;
        }
        // With several modes active, a tap can land on more than one kind (a
        // wall edge over a floor, say). Resolve by priority — wall → roof →
        // floor — and toggle the first hit (see the overlap-pick rule).
        let hit = {
            let Some(storey) = self.current_storey() else {
                return;
            };
            [SurfaceKind::Walls, SurfaceKind::Roofs, SurfaceKind::Floors]
                .into_iter()
                .filter(|k| self.structure_kinds.contains(k))
                .find_map(|k| storey.surface_at(k, world_x, world_y, tol))
        };
        if let Some(id) = hit {
            if !self.structure_selected.insert(id) {
                self.structure_selected.remove(&id);
            }
        }
    }

    /// Clear every selected surface (the "Deselect all" subtool). Keeps the
    /// active Select subtool, so the assessor can keep picking.
    pub fn clear_structure_surface_selection(&mut self) {
        self.structure_selected.clear();
        self.pending_structure_carves.clear();
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
    }

    /// The staged (not-yet-split) carve rings, plan coords, for the canvas to
    /// shade as the pending selection. Empty when no carve is staged.
    pub fn pending_structure_carve_polygons(&self) -> Vec<Vec<(f64, f64)>> {
        self.pending_structure_carves
            .iter()
            .map(|ring| ring.iter().map(|p| (p.x, p.y)).collect())
            .collect()
    }

    /// A Structure-phase CARVE/CUT stroke (world coords): an approximate stroke
    /// that the assessor then DIMENSIONS, which STAGES a pending sub-region (NOT
    /// yet split). Carve the storey the SAME way a phase area is drawn. A straight
    /// cut splits it into two candidate slots to pick from (see
    /// [`structure_cut_regions`]); any other stroke becomes a dimensionable
    /// sub-area straight away. Once dimensioned the ring is staged
    /// ([`stage_structure_carve`]); the actual geometry split is DEFERRED to
    /// "Edit attributes" ([`commit_pending_structure_carves`]).
    pub fn apply_structure_carve_stroke(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if self.structure_select.is_none() {
            return;
        }
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Carve against the whole outline (no holes) — the split ring is fixed
        // by dimensioning before it carves the surface.
        self.sync_active();
        match self
            .document
            .carve_or_cut_in_available(&pts, &[], &self.tuning.subarea)
        {
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                self.pending_structure_cut = Some([
                    self.pinned_subarea_slot(&boundary, a),
                    self.pinned_subarea_slot(&boundary, b),
                ]);
            }
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                if plan.validate_simple().is_err() {
                    return;
                }
                let Some(kind) = self.primary_structure_kind() else {
                    return;
                };
                let undo = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                self.begin_structure_split_dimensioning(slot, kind, undo);
            }
            Err(e) => log::error!("structure carve_or_cut: {}", e),
        }
    }

    /// The two candidate regions of a pending cut (world coords), for the UI to
    /// highlight while the assessor picks one. Empty when none is pending.
    pub fn structure_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.pending_structure_cut
            .as_ref()
            .map(|regions| {
                regions
                    .iter()
                    .map(|slot| {
                        slot.floor_plan
                            .get_points()
                            .iter()
                            .map(|p| (p.x as f64, p.y as f64))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pick which candidate region to split: it becomes the in-flight split
    /// shape and starts dimensioning. A tap outside both cancels.
    pub fn choose_structure_cut_region(&mut self, world_x: f32, world_y: f32) {
        if self.structure_select.is_none() {
            return;
        }
        let Some([a, b]) = self.pending_structure_cut.take() else {
            return;
        };
        let chosen = if a.floor_plan.contains_point(world_x, world_y) {
            Some(a)
        } else if b.floor_plan.contains_point(world_x, world_y) {
            Some(b)
        } else {
            None
        };
        if let Some(slot) = chosen {
            let Some(kind) = self.primary_structure_kind() else {
                return;
            };
            let undo = self.current_history_snapshot();
            self.begin_structure_split_dimensioning(slot, kind, undo);
        }
    }

    /// True while a carve/cut is awaiting the assessor's region pick.
    pub fn structure_cut_pending(&self) -> bool {
        self.pending_structure_cut.is_some()
    }

    /// Make a carved/picked region the in-flight split shape and start
    /// dimensioning its drawn walls (mirrors `begin_phase_area_dimensioning`).
    /// A carve that needed no dimensioning commits immediately via
    /// `maybe_commit_structure_split`.
    fn begin_structure_split_dimensioning(
        &mut self,
        slot: ShapeSlot,
        kind: SurfaceKind,
        undo: HistorySnapshot,
    ) {
        self.document.begin_phase_area(slot);
        self.pending_structure_cut = None;
        self.structure_split_kind = Some(kind);
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo);
        self.maybe_stage_structure_carve();
    }

    /// The in-flight split ring (world coords) for the UI to highlight — the
    /// LIVE dimensioned buffer, so it tracks dimensioning. Empty when no split
    /// is in-flight.
    pub fn pending_structure_split_polygon(&self) -> Vec<(f64, f64)> {
        if self.structure_split_kind.is_some() && self.document.active == ActiveShape::PhaseArea {
            self.document
                .floor_plan
                .get_points()
                .iter()
                .map(|p| (p.x as f64, p.y as f64))
                .collect()
        } else {
            Vec::new()
        }
    }

    /// True once the in-flight split shape is fully DIMENSIONED (wall lengths
    /// AND position) and ready to commit.
    fn structure_split_dimensioned(&self) -> bool {
        self.structure_split_kind.is_some()
            && self.document.active == ActiveShape::PhaseArea
            && self.document.dimensioning.current_prompt.is_none()
            && self.document.dimensioning.current_position_prompt.is_none()
            && self.document.floor_plan.get_points().len() >= 3
    }

    /// STAGE the carve if its drawn walls are fully dimensioned — a cheap no-op
    /// unless a structure carve is in-flight and ready. Called at the end of
    /// every dimension/position apply so a carve that finishes on either kind of
    /// prompt is staged.
    pub fn maybe_stage_structure_carve(&mut self) {
        if self.structure_split_dimensioned() {
            self.stage_structure_carve();
        }
    }

    /// Discard the in-flight split shape and return to the outline, dropping its
    /// half-dimensioned buffer (mirrors `abandon_phase_shape`).
    fn abandon_structure_split_shape(&mut self) {
        self.document.phase_shape = None;
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
        if self.document.active == ActiveShape::PhaseArea {
            self.document.active = ActiveShape::Outline;
            self.load_active_into_buffer();
            self.document.pending_arc = None;
            self.edit_drag_prompt = None;
            self.vertex_drop_prompt = None;
            self.document.edit_state = EditState::default();
            self.compute_next_dimension_prompt();
        }
    }

    /// STAGE the dimensioned carve as a PENDING selection — capture its ring and
    /// drop the in-flight shape, WITHOUT touching any geometry. The actual split
    /// is deferred to "Edit attributes" (`commit_pending_structure_carves`), so a
    /// carve the assessor then decides is wrong changes nothing. The pending ring
    /// is shaded on the canvas as the selection until then.
    fn stage_structure_carve(&mut self) {
        if self.structure_split_kind.is_none() {
            return;
        }
        let ring32 = self.document.floor_plan.get_points();
        if ring32.len() < 3 {
            self.abandon_structure_split_shape();
            return;
        }
        let ring64: Vec<Point2f64> = ring32.iter().map(|p| domain::points::to_f64(*p)).collect();
        self.pending_structure_carves.push(ring64);
        self.abandon_structure_split_shape();
    }

    /// COMMIT every pending carve into real splits — the deferred half of
    /// [`stage_structure_carve`], run when "Edit attributes" opens. For each
    /// staged ring, push it onto each active kind's durable split field (walls:
    /// the fixed interior cut segment, only where the cut crosses a wall;
    /// floors/roofs: the whole ring), then re-resolve ONCE and auto-select the
    /// resulting pieces across every active kind. Snapshots for undo so opening
    /// the editor stays reversible. No-op when nothing is pending.
    pub(super) fn commit_pending_structure_carves(&mut self) {
        if self.pending_structure_carves.is_empty() {
            return;
        }
        // A split needs an active storey (never the read-only All-storeys tab).
        let Some(idx) = self.current_storey_idx else {
            self.pending_structure_carves.clear();
            return;
        };
        let kinds: Vec<SurfaceKind> = self.structure_kinds.iter().copied().collect();
        let rings = std::mem::take(&mut self.pending_structure_carves);

        // Walls split only along a carve ring's INTERIOR edges (where it crosses a
        // wall); floors/roofs take the whole ring. Compute the wall cuts per ring
        // up front (needs `&self.document`) before the `&mut` record borrow.
        let want_walls = kinds.contains(&SurfaceKind::Walls);
        let per_ring: Vec<(Vec<Point2f64>, Vec<[Point2f64; 2]>)> = rings
            .into_iter()
            .map(|ring64| {
                let cuts = if want_walls {
                    let ring32: Vec<Point2f32> =
                        ring64.iter().map(|p| domain::points::to_f32(*p)).collect();
                    self.document.outline.floor_plan.interior_cut_edges(&ring32)
                } else {
                    Vec::new()
                };
                (ring64, cuts)
            })
            .collect();

        // Does anything actually SUBDIVIDE? (an area kind active, or a cut that
        // crossed a wall). A wall carve whose ring runs entirely along existing
        // walls crosses none mid-span → nothing to split, but carve is still a
        // SELECTION gesture, so we select the enclosed surfaces regardless.
        let will_split = kinds
            .iter()
            .any(|k| matches!(k, SurfaceKind::Floors | SurfaceKind::Roofs))
            || per_ring.iter().any(|(_, c)| !c.is_empty());

        if will_split {
            self.record_history();
            let mut pushed_roof = false;
            if let Some(storey) = self
                .current_record
                .as_mut()
                .and_then(|r| r.data.storeys.get_mut(idx))
            {
                for (ring64, cuts) in &per_ring {
                    for kind in &kinds {
                        match kind {
                            SurfaceKind::Walls => {
                                if !cuts.is_empty() {
                                    storey.push_structure_split(
                                        SurfaceKind::Walls,
                                        ring64,
                                        cuts.clone(),
                                    );
                                }
                            }
                            SurfaceKind::Floors => {
                                storey.push_structure_split(
                                    SurfaceKind::Floors,
                                    ring64,
                                    Vec::new(),
                                );
                            }
                            SurfaceKind::Roofs => {
                                storey.push_structure_split(SurfaceKind::Roofs, ring64, Vec::new());
                                pushed_roof = true;
                            }
                        }
                    }
                }
            } else {
                return;
            }
            // Regions-as-truth: a ROOF carve is a one-shot coplanar face split on
            // the stored roofs (replayed via `recompose_roofs`); floors/walls
            // replay their durable splits in `resolve_storey`.
            if pushed_roof {
                self.recompose_roofs(idx);
            }
            let _ = self.mark_dirty();
            self.materialize(); // re-resolve applies the splits + re-homes openings
        }

        // Select the carved area's surfaces across every active kind — whether or
        // not anything was subdivided (with no split the already-resolved regions
        // are current, so this picks the surfaces the ring encloses).
        for (ring64, _) in &per_ring {
            self.select_surfaces_in_region(ring64);
        }
    }

    /// Add every surface of the active kind inside `region_ring` (world coords)
    /// to the selection — the carve-pick query, delegated to the storey.
    fn select_surfaces_in_region(&mut self, region_ring: &[Point2f64]) {
        if self.structure_select.is_none() {
            return;
        }
        let kinds: Vec<SurfaceKind> = self.structure_kinds.iter().copied().collect();
        let hits: Vec<ItemId> = match self.current_storey() {
            Some(storey) => kinds
                .iter()
                .flat_map(|k| storey.surfaces_in_ring(*k, region_ring))
                .collect(),
            None => return,
        };
        self.structure_selected.extend(hits);
    }

    /// Re-seed the selection for an active `All`/`Remaining` subtool ACROSS every
    /// active mode (the union): `All` takes every surface of each active kind;
    /// `Remaining` takes only the ones still left to do — those whose
    /// construction isn't complete yet ([`Ber::incomplete_surface_ids`]).
    /// No-op when no seed tool is active.
    pub(super) fn reseed_structure_selection(&mut self) {
        let Some(mode) = self.structure_select else {
            return;
        };
        if !matches!(mode, SelectMode::All | SelectMode::Remaining) {
            return;
        }
        let kinds: Vec<SurfaceKind> = self.structure_kinds.iter().copied().collect();
        let mut ids: HashSet<ItemId> = HashSet::new();
        for kind in kinds {
            match mode {
                SelectMode::All => {
                    if let Some(s) = self.current_storey() {
                        ids.extend(s.surface_ids(kind));
                    }
                }
                SelectMode::Remaining => {
                    if let (Some(record), Some(idx)) =
                        (self.current_record.as_ref(), self.current_storey_idx)
                    {
                        ids.extend(record.data.incomplete_surface_ids(idx, kind));
                    }
                }
                _ => {}
            }
        }
        self.structure_selected = ids;
    }

    /// Drop every selected id belonging to `kind` (used when a mode is toggled
    /// OFF in an additive `Click`/`Carve` selection — the rest stays put).
    pub(super) fn drop_selection_of_kind(&mut self, kind: SurfaceKind) {
        let drop_ids: HashSet<ItemId> = match self.current_storey() {
            Some(s) => s.surface_ids(kind).into_iter().collect(),
            None => return,
        };
        self.structure_selected.retain(|id| !drop_ids.contains(id));
    }

    /// The highest-priority active mode (wall → roof → floor) — the kind a carve
    /// SPLIT acts on when several modes are active. `None` when no mode is active.
    pub(super) fn primary_structure_kind(&self) -> Option<SurfaceKind> {
        [SurfaceKind::Walls, SurfaceKind::Roofs, SurfaceKind::Floors]
            .into_iter()
            .find(|k| self.structure_kinds.contains(k))
    }

    /// The current storey's resolved regions, if a storey is in scope.
    fn current_storey(&self) -> Option<&Storey> {
        self.current_record
            .as_ref()?
            .data
            .storeys
            .get(self.current_storey_idx?)
    }
}

/// Stored-photo quality tier, chosen per attach site. Drives how a captured
/// image is recompressed into the photo store: the compact tiers downscale +
/// re-encode toward a target byte size (most shots don't need full resolution),
/// while `Full` keeps the original bytes verbatim for shots where text must stay
/// legible (labels, datasheets — OCR/zoom).
///
/// The integer [`level`](Self::level) is also handed to the Android camera
/// activity over JNI (`requestPhoto(int)`); keep it in sync with the levels in
/// `android/app/src/main/java/.../MainActivity.java`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoQuality {
    Low,
    Medium,
    High,
    Full,
}

impl PhotoQuality {
    /// Stable integer level passed across the JNI boundary to the camera
    /// activity. Kept in sync with `requestPhoto(int)` in `MainActivity.java`.
    #[cfg_attr(not(target_os = "android"), allow(dead_code))]
    pub fn level(self) -> i32 {
        match self {
            PhotoQuality::Low => 0,
            PhotoQuality::Medium => 1,
            PhotoQuality::High => 2,
            PhotoQuality::Full => 3,
        }
    }

    /// Recompression spec for the compact tiers: `(max long-edge px, JPEG
    /// quality 1..=100)`. `Full` returns `None` — its original bytes are stored
    /// verbatim. The quality is applied in a SINGLE encode pass (no byte-budget
    /// search), so the stored size varies with the photo but the work is one
    /// downscale + one encode.
    fn compression(self) -> Option<(u32, u8)> {
        match self {
            PhotoQuality::Low => Some((1280, 50)),
            PhotoQuality::Medium => Some((1600, 70)),
            PhotoQuality::High => Some((2048, 85)),
            PhotoQuality::Full => None,
        }
    }
}
