//! Structure-phase surface SELECTION: the app-shell interaction layer over the
//! current storey's resolved regions (walls / ceilings / floors). The selected set
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
use ber_model::ber::roof::InferredRoofType;
use ber_model::ber::structural::{
    CeilingConstructionType, CeilingExposure, CeilingInsulation, ConstructionMakeup,
    ConstructionType, FloorConstructionType, FloorExposure, Storey, SurfaceExposure, WallExposure,
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
    /// The group's EXPLICIT name — the construction's stored name / the name
    /// the assessor typed this session — `None` for an unnamed group (whose
    /// display [`name`](Self::name) is just the "Group N" fallback). The name
    /// is part of the construction IDENTITY, so attribute applies carry this
    /// (full-group edits keep their group; subset edits fork unnamed).
    pub explicit_name: Option<String>,
    /// The group construction's attached photos (session copy, seeded from the
    /// construction; the photos view shows these and the camera button appends).
    pub photos: Vec<Photo>,
    /// CEILINGS ONLY: the group's extra-insulation spec (session copy, seeded
    /// from the construction) — the combined "Default" dropdown in the
    /// ceiling attribute editor (unknown / a measured thickness). Always
    /// `None` for walls and floors (they carry no such spec). On a
    /// plain-`Default` ceiling group `None` means "not yet specified", which
    /// keeps the group INCOMPLETE ([`EditGroup::is_complete`]) — no tick,
    /// counted by `Remaining`, the checklist row stays red — but never blocks
    /// closing the session; other make-ups answer the insulation question
    /// themselves ([`CeilingInsulation::spec_done_for`]).
    pub insulation: Option<CeilingInsulation>,
    /// Whether the box shows the FULL attribute editor. A FRESH group (its
    /// selection carries no attribution yet — type and build-up both unset)
    /// starts `false`: the box offers just an "Existing group…" dropdown plus
    /// a "New group" button, so joining an already-defined group is one pick
    /// instead of re-entering its attributes. "New group" (or picking an
    /// existing group / any attribute apply) flips it `true`; an
    /// already-attributed group starts `true` (it IS its group). Session
    /// UI-flow state, not persisted. (With no existing group of the kind to
    /// offer, the UI falls through to the full editor regardless.)
    pub show_editor: bool,
    /// Whether this group OPENED unattributed this session (the two-mode
    /// entry's starting state). Purely a COMBO-gating detail now: a
    /// session-fresh group's editor hides the in-editor "Add to existing
    /// group…" combo (the two-mode entry it came from already offered that
    /// list), while a group that opened attributed keeps it. The RESET
    /// button is on EVERY box regardless — reselecting an attributed surface
    /// after Done must still offer starting again from scratch. Unlike
    /// [`show_editor`](Self::show_editor) it never flips on an attribute
    /// apply. Session UI-flow state, not persisted.
    pub fresh: bool,
    /// CEILINGS ONLY: the roof type the drawn GEOMETRY implies for this
    /// group's regions (the region's flat/sloped shape × the dwelling roof
    /// over it — `Ber::inferred_ceiling_roof_type`). Part of the grouping
    /// key, so geometrically different ceilings never merge into one group —
    /// grouping follows geometry, and a manual dropdown override (which only
    /// changes the STORED type) never re-shuffles it. Drives the type
    /// dropdown's prefill (`Definite` + still-unset stored type) and the
    /// `Ambiguous` hint. Always `Unknown` for walls/floors. Runtime-only,
    /// like the rest of the session.
    pub inferred: InferredRoofType,
}

impl EditGroup {
    /// Whether every attribute of this group's construction is fully set —
    /// the group window's top-right tick. Composes the SAME ber_model
    /// primitives the per-construction `is_complete` tests use (a real type;
    /// a chosen build-up, photo-backed when it's a specified custom stack;
    /// and — plain-`Default` ceilings only — the extra-insulation gate),
    /// evaluated over the session's mirrored fields so the tick appears the
    /// moment the last attribute lands, without waiting for a re-resolve.
    pub fn is_complete(&self) -> bool {
        let type_set = match self.ty {
            ConstructionType::Wall(t) => t.is_set(),
            ConstructionType::Floor(t) => t.is_set(),
            ConstructionType::Ceiling(t) => t.is_set(),
        };
        let build_up_done =
            self.makeup.is_set() && (!self.makeup.requires_photo() || !self.photos.is_empty());
        let insulation_done = self.ty.kind() != SurfaceKind::Ceilings
            || CeilingInsulation::spec_done_for(
                &self.makeup,
                self.insulation,
                !self.photos.is_empty(),
            );
        type_set && build_up_done && insulation_done
    }
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
    /// The rejection hint from the last group RENAME that violated the
    /// name-integrity rule (`(group index, message)` — see
    /// `Ber::set_construction_name`): the UI shows it under that group's name
    /// box. Cleared by the next accepted rename of that group. The typed
    /// session name stays as typed so the assessor can keep editing toward a
    /// valid one (renames stream per keystroke); the DURABLE name keeps its
    /// last valid value.
    pub name_error: Option<(usize, String)>,
}

// NOTE: there is deliberately no session-level "blocker" any more — Done
// ALWAYS closes the session. The ceiling-insulation rule keeps its teeth
// through the completeness predicate alone ([`EditGroup::is_complete`] /
// `CeilingInsulation::spec_done`): an unspecified group has no tick, doesn't
// count toward the N/M headers or the checklist row, comes back via the
// `Remaining` subtool, and holds the Review-entry gate red.

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
    /// CEILINGS ONLY: the inferred-roof-type class of the region(s) this
    /// offer was read from (`Ber::inferred_ceiling_roof_type`) — part of the
    /// dedup key, mirroring [`EditGroup::inferred`], so geometrically
    /// different ceilings never collapse onto one offer. Always `Unknown`
    /// for walls/floors.
    pub inferred: InferredRoofType,
}

/// A dimensioned structure carve STAGED as a pending selection (not yet split):
/// the ring, plus the interior edges whose walls the assessor answered
/// "Exclude" to in the wall-inclusion ask. Walls along the ring's OUTLINE edges
/// (the storey's true exterior boundary) are always included; the excluded
/// edges subtract their walls from the committed selection.
#[derive(Debug, Clone)]
pub(super) struct StagedStructureCarve {
    pub(super) ring: Vec<Point2f64>,
    pub(super) excluded_edges: Vec<[Point2f64; 2]>,
    /// Which storey the ring belongs to. `None` = the ACTIVE storey (the
    /// single-storey carve path, unchanged); `Some(i)` = storey `i` (a
    /// cross-storey lasso region staged from the all-storeys view).
    pub(super) storey: Option<usize>,
    /// `true` for an all-storeys LASSO region: SELECTION-ONLY at commit (the
    /// lasso ring is a quick approximate gesture, never dimensioned, so it
    /// must not subdivide any geometry). The single-storey carve path stays
    /// `false` and keeps its deferred-split behaviour.
    pub(super) lasso: bool,
}

/// The in-flight wall-inclusion ASK over a just-dimensioned structure carve:
/// the ring being staged, its AMBIGUOUS edges (interior to the storey footprint
/// — house area continues beyond them, so wall inclusion is a judgement call),
/// a cursor through them, and the edges excluded so far. Resolves one edge per
/// Include/Exclude answer; when the last edge is answered the carve stages.
#[derive(Debug, Clone)]
pub(super) struct WallInclusionPrompt {
    ring: Vec<Point2f64>,
    edges: Vec<[Point2f64; 2]>,
    cursor: usize,
    excluded: Vec<[Point2f64; 2]>,
    /// Which storey the asked edges live on — `None` = the active storey (the
    /// single-storey carve path); `Some(i)` for a cross-storey lasso ask (the
    /// prompt text then names the storey, and the staged carve carries it).
    storey: Option<usize>,
    /// Whether the staged carve this ask resolves into is a LASSO region
    /// (selection-only at commit). See [`StagedStructureCarve::lasso`].
    lasso: bool,
}

/// UI data for the wall-inclusion ask: the CURRENT ambiguous edge (world
/// coords, for the canvas to highlight while asking), the cursor position
/// (`index` of `total`, 0-based) for the panel's "Wall 1 of 2" label, and a
/// human description of the wall being asked about ("Include the 10.0 m wall
/// on the left edge (floor-step riser)?") so the question identifies its
/// target in words as well as by the canvas highlight.
#[derive(Debug, Clone, PartialEq)]
pub struct WallInclusionPromptView {
    pub edge: ((f64, f64), (f64, f64)),
    pub index: usize,
    pub total: usize,
    pub description: String,
    /// The storey the asked edge lives on — `None` for the single-storey
    /// carve path (the edge is on the current storey). `Some(i)` for a
    /// cross-storey lasso ask: the all-storeys canvas highlights the edge in
    /// storey `i`'s cell, and the description opens with the storey's name.
    pub storey_idx: Option<usize>,
}

impl AppController {
    /// The Structure-phase selected region ids — the canvas shades exactly these.
    pub fn structure_selected_ids(&self) -> &HashSet<ItemId> {
        &self.structure_selected
    }

    /// Whether the WHOLE house's Structure attribution is complete: every
    /// storey's walls, ceilings and floors are fully attributed. Gates the
    /// Structure-phase "Review" entry. `false` with no record / no storeys.
    pub fn structure_complete(&self) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        let b = &record.data;
        !b.storeys.is_empty()
            && (0..b.storeys.len()).all(|i| {
                b.structure_surfaces_complete(i, SurfaceKind::Walls)
                    && b.structure_surfaces_complete(i, SurfaceKind::Ceilings)
                    && b.structure_surfaces_complete(i, SurfaceKind::Floors)
            })
    }

    /// Distinct COMPLETE constructions already assigned to surfaces ANYWHERE
    /// in the house (walls, floors, ceilings, across every storey) — the
    /// "existing groups" the attribute editor offers so a selection can join
    /// one rather than re-typing its attributes. The construction registry is
    /// DWELLING-level, so a group defined on one storey must be offered while
    /// editing any other (a house shares its build fabric between storeys).
    /// Each `ty` is kind-tagged, so the editor filters to its kind.
    pub fn structure_existing_groups(&self) -> Vec<ExistingGroup> {
        fn add(
            out: &mut Vec<ExistingGroup>,
            ty: ConstructionType,
            makeup: ConstructionMakeup,
            phase: ConstructionPhase,
            name: Option<String>,
            inferred: InferredRoofType,
        ) {
            // Name is part of the identity: same-fabric groups with different
            // names are distinct offers ("Porch roof" vs "Main house roof").
            // The inferred-roof-type CLASS is too (ceilings): grouping follows
            // geometry, so same-fabric ceilings under different roof geometry
            // stay distinct offers.
            let norm = |n: &Option<String>| {
                n.as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            };
            if out.iter().any(|g| {
                g.ty == ty
                    && g.makeup == makeup
                    && g.phase == phase
                    && norm(&g.name) == norm(&name)
                    && g.inferred == inferred
            }) {
                return;
            }
            out.push(ExistingGroup {
                ty,
                makeup,
                phase,
                name,
                inferred,
            });
        }
        let mut out = Vec::new();
        let Some(record) = self.current_record.as_ref() else {
            return out;
        };
        for (idx, storey) in record.data.storeys.iter().enumerate() {
            for r in &storey.walls.regions {
                // Out-of-scope walls (non-heat-loss / inter-storey) don't
                // participate in the Structure phase — a construction referenced
                // only by them isn't offered as an existing group.
                if !r.exposure.in_structure_scope() {
                    continue;
                }
                if let Some(c) = record.data.wall_construction(r.construction_id) {
                    if c.is_complete() {
                        add(
                            &mut out,
                            ConstructionType::Wall(c.construction_type),
                            c.base.construction_makeup.clone(),
                            c.base.construction_phase.clone(),
                            c.base.name.clone(),
                            InferredRoofType::Unknown,
                        );
                    }
                }
            }
            for r in &storey.floors.regions {
                if !r.exposure.in_structure_scope() {
                    continue;
                }
                if let Some(c) = record.data.floor_construction(r.construction_id) {
                    if c.is_complete() {
                        add(
                            &mut out,
                            ConstructionType::Floor(c.construction_type),
                            c.base.construction_makeup.clone(),
                            c.base.construction_phase.clone(),
                            c.base.name.clone(),
                            InferredRoofType::Unknown,
                        );
                    }
                }
            }
            for r in &storey.ceilings.regions {
                if !r.exposure.in_structure_scope() {
                    continue;
                }
                if let Some(c) = record.data.ceiling_construction(r.construction_id) {
                    if c.is_complete() {
                        add(
                            &mut out,
                            ConstructionType::Ceiling(c.construction_type),
                            c.base.construction_makeup.clone(),
                            c.base.construction_phase.clone(),
                            c.base.name.clone(),
                            record.data.inferred_ceiling_roof_type(idx, r),
                        );
                    }
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
        name: Option<String>,
    ) {
        // Map each resolved wall id to its durable key ON ITS OWN STOREY (a
        // lasso selection can span the stack; region ids are unique
        // house-wide): a perimeter wall by its parametric SurfaceRef (base
        // midpoint), a RISER (interior step face, no perimeter ref) by its
        // quantized base edge — mirroring riser exposure, so step-face
        // constructions survive the re-derivation too.
        #[allow(clippy::type_complexity)]
        let per_storey: Vec<(usize, Vec<manip_plan::SurfaceRef>, Vec<document::RiserKey>)> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let mut out = Vec::new();
            for idx in 0..record.data.storeys.len() {
                let storey = &record.data.storeys[idx];
                let Some(sd) = self.document.storeys.get(idx) else {
                    continue;
                };
                let plan = &sd.outline.floor_plan;
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
                if !refs.is_empty() || !risers.is_empty() {
                    out.push((idx, refs, risers));
                }
            }
            out
        };
        if per_storey.is_empty() {
            return;
        }
        self.record_history();
        for (idx, refs, riser_keys) in per_storey {
            let sd = &mut self.document.storeys[idx];
            for r in refs {
                sd.wall_constructions
                    .insert(r, (ty, makeup.clone(), phase.clone(), name.clone()));
            }
            for k in riser_keys {
                sd.riser_constructions
                    .insert(k, (ty, makeup.clone(), phase.clone(), name.clone()));
            }
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
        name: Option<String>,
    ) {
        // Per-storey key resolution — a lasso selection can span the stack.
        let per_storey: Vec<(usize, Vec<document::CentroidKey>)> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            record
                .data
                .storeys
                .iter()
                .enumerate()
                .map(|(idx, storey)| {
                    let keys: Vec<document::CentroidKey> = ids
                        .iter()
                        .filter_map(|id| storey.floors.regions.iter().find(|r| r.id == *id))
                        .map(|r| document::centroid_key(r.centroid()))
                        .collect();
                    (idx, keys)
                })
                .filter(|(idx, keys)| !keys.is_empty() && *idx < self.document.storeys.len())
                .collect()
        };
        if per_storey.is_empty() {
            return;
        }
        self.record_history();
        for (idx, keys) in per_storey {
            for k in keys {
                self.document.storeys[idx]
                    .floor_constructions
                    .insert(k, (ty, makeup.clone(), phase.clone(), name.clone()));
            }
        }
        let _ = self.mark_dirty();
    }

    /// Assign a CEILING/ROOF construction (type + make-up) to the given resolved
    /// ROOF region ids, keyed by each region's plan centroid. Mirror of
    /// [`set_floor_construction_for`](Self::set_floor_construction_for) over the
    /// storey's `ceilings` regions (`document::stamp_ceiling_constructions`).
    pub fn set_ceiling_construction_for(
        &mut self,
        ids: &[ItemId],
        ty: CeilingConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
        name: Option<String>,
    ) {
        // Per-storey key resolution — a lasso selection can span the stack.
        let per_storey: Vec<(usize, Vec<document::CentroidKey>)> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            record
                .data
                .storeys
                .iter()
                .enumerate()
                .map(|(idx, storey)| {
                    let keys: Vec<document::CentroidKey> = ids
                        .iter()
                        .filter_map(|id| storey.ceilings.regions.iter().find(|r| r.id == *id))
                        .map(|r| document::centroid_key(r.centroid()))
                        .collect();
                    (idx, keys)
                })
                .filter(|(idx, keys)| !keys.is_empty() && *idx < self.document.storeys.len())
                .collect()
        };
        if per_storey.is_empty() {
            return;
        }
        self.record_history();
        for (idx, keys) in per_storey {
            for k in keys {
                self.document.storeys[idx]
                    .ceiling_constructions
                    .insert(k, (ty, makeup.clone(), phase.clone(), name.clone()));
            }
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
            // A derived InterStorey group is read-only (the UI shows a disabled
            // control), and InterStorey is never a settable value — the
            // derivation owns that classification. Belt-and-braces here.
            let is_inter = |e: &SurfaceExposure| {
                matches!(
                    e,
                    SurfaceExposure::Wall(WallExposure::InterStorey)
                        | SurfaceExposure::Floor(FloorExposure::InterStorey)
                        | SurfaceExposure::Ceiling(CeilingExposure::InterStorey)
                )
            };
            if is_inter(&exposure) || is_inter(&g.exposure) {
                return;
            }
            (g.ty.kind(), g.ids.clone(), g.exposure_locked)
        };
        match (kind, exposure) {
            (SurfaceKind::Floors, SurfaceExposure::Floor(e)) => {
                self.set_floor_exposure_for_ids(&ids, e)
            }
            (SurfaceKind::Ceilings, SurfaceExposure::Ceiling(e)) => {
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
        // Per-storey key resolution — a lasso selection can span the stack.
        let per_storey: Vec<(usize, Vec<document::CentroidKey>)> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            record
                .data
                .storeys
                .iter()
                .enumerate()
                .map(|(idx, storey)| {
                    let keys: Vec<document::CentroidKey> = ids
                        .iter()
                        .filter_map(|id| storey.floors.regions.iter().find(|r| r.id == *id))
                        .map(|r| document::centroid_key(r.centroid()))
                        .collect();
                    (idx, keys)
                })
                .filter(|(idx, keys)| !keys.is_empty() && *idx < self.document.storeys.len())
                .collect()
        };
        if per_storey.is_empty() {
            return;
        }
        self.record_history();
        for (idx, keys) in per_storey {
            for k in keys {
                self.document.storeys[idx]
                    .floor_exposure
                    .insert(k, exposure);
            }
        }
        let _ = self.mark_dirty();
    }

    /// Set the CEILING/ROOF exposure of the given resolved ROOF region ids,
    /// keyed by each region's plan centroid (`document.ceiling_exposure`,
    /// `stamp_ceiling_exposure`). Mirror of
    /// [`set_floor_exposure_for_ids`](Self::set_floor_exposure_for_ids) over the
    /// storey's `ceilings`.
    fn set_ceiling_exposure_for_ids(&mut self, ids: &[ItemId], exposure: CeilingExposure) {
        // Per-storey key resolution — a lasso selection can span the stack.
        let per_storey: Vec<(usize, Vec<document::CentroidKey>)> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            record
                .data
                .storeys
                .iter()
                .enumerate()
                .map(|(idx, storey)| {
                    let keys: Vec<document::CentroidKey> = ids
                        .iter()
                        .filter_map(|id| storey.ceilings.regions.iter().find(|r| r.id == *id))
                        .map(|r| document::centroid_key(r.centroid()))
                        .collect();
                    (idx, keys)
                })
                .filter(|(idx, keys)| !keys.is_empty() && *idx < self.document.storeys.len())
                .collect()
        };
        if per_storey.is_empty() {
            return;
        }
        self.record_history();
        for (idx, keys) in per_storey {
            for k in keys {
                self.document.storeys[idx]
                    .ceiling_exposure
                    .insert(k, exposure);
            }
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
        // Resolve each id to (perimeter span | riser key) ON ITS OWN STOREY up
        // front (a lasso selection can span the stack), reading the resolved
        // record + the document, before the mutable document borrow.
        enum WallWrite {
            Section(manip_plan::VertexId, f32, f32, f32), // vid, start_m, end_m, len
            Riser(document::RiserKey),
        }
        let writes: Vec<(usize, Vec<WallWrite>)> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let mut out = Vec::new();
            for (idx, storey) in record.data.storeys.iter().enumerate() {
                let Some(doc) = self.document.storeys.get(idx) else {
                    continue;
                };
                let storey_writes: Vec<WallWrite> = ids
                    .iter()
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
                    .collect();
                if !storey_writes.is_empty() {
                    out.push((idx, storey_writes));
                }
            }
            out
        };
        if writes.is_empty() {
            return;
        }
        self.record_history();
        for (idx, storey_writes) in writes {
            for w in storey_writes {
                match w {
                    WallWrite::Section(vid, start_m, end_m, len) => {
                        self.document.storeys[idx]
                            .set_wall_section(vid, start_m, end_m, len, exposure);
                    }
                    WallWrite::Riser(key) => {
                        self.document.storeys[idx]
                            .riser_exposure
                            .insert(key, exposure);
                    }
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
    /// ceilings/ceilings. No-op for an empty selection or no storey.
    pub fn begin_structure_edit(&mut self) {
        // "Edit attributes" is when we commit to the selection — NOW perform any
        // deferred carve splits (turning staged rings into real sub-regions and
        // selecting them), so selecting a carve never changed geometry on its own.
        self.commit_pending_structure_carves();
        if self.structure_selected.is_empty() {
            return;
        }
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        // The selection can span kinds (Walls + Floors + …) AND — from the
        // all-storeys lasso — STOREYS: each id is located across the whole
        // stack (region ids are unique house-wide), then its construction's
        // (type-kind, make-up, phase) read and tagged with `ConstructionType`
        // so the rest of the editor stays kind-agnostic. A region whose
        // construction is missing reads as that kind's default type + default
        // make-up / Original phase. The construction registry is
        // dwelling-level, so one group freely covers surfaces on several
        // storeys.
        type Attrs = (
            ConstructionType,
            ConstructionMakeup,
            ConstructionPhase,
            SurfaceExposure,
            bool, // exposure_locked
            Option<String>,
            Vec<Photo>,
            Option<CeilingInsulation>, // ceilings only; None for walls/floors
            InferredRoofType,          // ceilings only; Unknown for walls/floors
        );
        let document = &self.document;
        let attrs_of = |id: ItemId| -> Option<Attrs> {
            let (idx, storey, kind) = record
                .data
                .storeys
                .iter()
                .enumerate()
                .find_map(|(i, s)| s.surface_kind_of(id).map(|k| (i, s, k)))?;
            match kind {
                SurfaceKind::Walls => {
                    let r = storey.walls.regions.iter().find(|r| r.id == id)?;
                    // Out-of-scope walls (non-heat-loss / inter-storey) never form
                    // a group. The selection paths already skip them
                    // (`WallExposure::in_structure_scope`); this catches a stale
                    // id whose wall was repainted out of scope since selection.
                    if !r.exposure.in_structure_scope() {
                        return None;
                    }
                    // A perimeter wall whose recovered VertexId is face-partitioned
                    // is locked; a riser (no perimeter match) is never locked. The
                    // partition state lives on the wall's OWN storey doc.
                    let locked = document
                        .storeys
                        .get(idx)
                        .and_then(|doc| {
                            doc.recover_wall_region_span(r)
                                .map(|(vid, ..)| doc.is_wall_partitioned(vid))
                        })
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
                        None,
                        InferredRoofType::Unknown,
                    ))
                }
                SurfaceKind::Floors => {
                    let r = storey.floors.regions.iter().find(|r| r.id == id)?;
                    // Out-of-scope floors (party / inter-storey) never form a
                    // group — mirror of the walls arm above.
                    if !r.exposure.in_structure_scope() {
                        return None;
                    }
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
                        None,
                        InferredRoofType::Unknown,
                    ))
                }
                SurfaceKind::Ceilings => {
                    let r = storey.ceilings.regions.iter().find(|r| r.id == id)?;
                    // Out-of-scope ceilings (party / inter-storey) never form
                    // a group — mirror of the walls arm above.
                    if !r.exposure.in_structure_scope() {
                        return None;
                    }
                    let (ty, makeup, phase, name, photos, insulation) =
                        match record.data.ceiling_construction(r.construction_id) {
                            Some(c) => (
                                ConstructionType::Ceiling(c.construction_type),
                                c.base.construction_makeup.clone(),
                                c.base.construction_phase.clone(),
                                c.base.name.clone(),
                                c.base.photos.clone(),
                                c.insulation,
                            ),
                            None => (
                                ConstructionType::Ceiling(Default::default()),
                                Default::default(),
                                Default::default(),
                                None,
                                Vec::new(),
                                None,
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
                        insulation,
                        // The roof type the drawn geometry implies for THIS
                        // region — the geometry axis of the grouping key.
                        record.data.inferred_ceiling_roof_type(idx, r),
                    ))
                }
            }
        };
        let mut groups: Vec<EditGroup> = Vec::new();
        for id in &self.structure_selected {
            // Skip ids that no longer resolve to a live region of any kind.
            let Some((
                ty,
                makeup,
                phase,
                exposure,
                exposure_locked,
                cname,
                photos,
                insulation,
                inferred,
            )) = attrs_of(*id)
            else {
                continue;
            };
            // `exposure_locked` is part of the key so a face-partitioned wall
            // (locked) never shares a group with an editable wall of the same
            // construction — the dropdown's enabled-ness must be uniform per group.
            // The NAME is part of the key too: same-fabric groups with
            // different names are DIFFERENT groups ("Porch roof" vs "Main
            // house roof") and must never merge in a session. So is the
            // INFERRED roof-type class (ceilings): grouping follows geometry,
            // so two same-phase ceilings whose drawn roofs imply different
            // types split even while both stored types are still unset.
            let explicit_name = cname
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            match groups.iter_mut().find(|g| {
                g.ty == ty
                    && g.makeup == makeup
                    && g.phase == phase
                    && g.exposure == exposure
                    && g.exposure_locked == exposure_locked
                    && g.explicit_name == explicit_name
                    && g.inferred == inferred
            }) {
                Some(g) => g.ids.push(*id),
                None => {
                    // A FRESH group (nothing attributed yet) ALWAYS opens on
                    // the two-mode entry — existing-group dropdown or "New
                    // group" — like every other kind; a Definite inference no
                    // longer skips it. The inference applies when the
                    // assessor taps "New group" (`begin_new_structure_group`)
                    // — or immediately below, when NO compatible offer exists
                    // and the two-mode entry would never show.
                    let type_set = match ty {
                        ConstructionType::Wall(t) => t.is_set(),
                        ConstructionType::Floor(t) => t.is_set(),
                        ConstructionType::Ceiling(t) => t.is_set(),
                    };
                    groups.push(EditGroup {
                        ids: vec![*id],
                        ty,
                        makeup: makeup.clone(),
                        phase,
                        exposure,
                        exposure_locked,
                        // The construction's stored name, if any (set in a prior
                        // session); the "Group N" fallback is filled in below.
                        name: explicit_name.clone().unwrap_or_default(),
                        explicit_name,
                        photos,
                        insulation,
                        show_editor: type_set || makeup.is_set(),
                        fresh: !(type_set || makeup.is_set()),
                        inferred,
                    })
                }
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
                name_error: None,
            });
            // PREFILL only where the two-mode entry can NEVER show: a group
            // whose geometry pins the roof type down (`Definite`) over a
            // still-unset stored type, with NO inference-compatible existing
            // offer of its kind (the UI falls through to the full editor
            // when there is nothing to join — the inferred type must be in
            // the dropdown when it opens). Groups WITH offers stay on the
            // two-mode entry and get the same seed when the assessor taps
            // "New group" (`begin_new_structure_group`). A stored pick is
            // never touched: this only fires on `Uninitialised`, so an
            // assessor override always stands.
            let offers = self.structure_existing_groups();
            let seeds: Vec<(
                usize,
                CeilingConstructionType,
                ConstructionMakeup,
                ConstructionPhase,
            )> = self
                .structure_edit
                .as_ref()
                .map(|e| {
                    e.groups
                        .iter()
                        .enumerate()
                        .filter_map(|(i, g)| match (g.ty, g.inferred) {
                            (
                                ConstructionType::Ceiling(CeilingConstructionType::Uninitialised),
                                InferredRoofType::Definite(t),
                            ) => {
                                let has_offer = offers.iter().any(|e| {
                                    e.ty.kind() == SurfaceKind::Ceilings
                                        && match e.ty {
                                            ConstructionType::Ceiling(et) => {
                                                g.inferred.allows_join(et)
                                            }
                                            _ => false,
                                        }
                                });
                                (!has_offer).then(|| (i, t, g.makeup.clone(), g.phase.clone()))
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            for (i, t, makeup, phase) in seeds {
                self.set_structure_group_attrs(i, ConstructionType::Ceiling(t), makeup, phase);
                if let Some(g) = self
                    .structure_edit
                    .as_mut()
                    .and_then(|e| e.groups.get_mut(i))
                {
                    // The no-offer prefill behaves like the pre-two-mode
                    // seeded box: full editor, in-editor join combo kept.
                    g.show_editor = true;
                    g.fresh = false;
                }
            }
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
    /// Attach a photo from `source` WITHOUT blocking the UI: synchronously
    /// fast-copy the bytes into the store (a plain file copy, no decode) so the
    /// model has a stable file to display immediately, then do the heavy work
    /// (downscale/recompress on Android + thumbnail) on a BACKGROUND thread. The
    /// finished paths are swapped into the photo by id by
    /// [`apply_finished_photos`](Self::apply_finished_photos), polled each frame.
    /// If Android wrote a fast subsampled thumbnail beside the capture (the
    /// `<source>.thumb.jpg` convention — see `MainActivity.java`), it seeds a
    /// LIGHT preview until the job's own thumbnail lands; otherwise the preview
    /// is the full copy. Returns the provisional `Photo` to attach to the model,
    /// or `None` if even the fast copy fails.
    pub(super) fn ingest_photo(
        &self,
        source: &std::path::Path,
        quality: PhotoQuality,
    ) -> Option<ber_model::ber::common::Photo> {
        std::fs::create_dir_all(&self.photos_dir).ok()?;
        // Canonicalize to an ABSOLUTE dir so the stored path loads as a
        // `file://` image regardless of the process's cwd.
        let dir =
            std::fs::canonicalize(&self.photos_dir).unwrap_or_else(|_| self.photos_dir.clone());
        let id = ItemId::default();
        // Fast provisional copy (no decode): the model points here until the
        // background job produces the recompressed file + thumbnail. On copy
        // failure, point at the source directly (still displays).
        let ext = source.extension().and_then(|e| e.to_str()).unwrap_or("jpg");
        let provisional = dir.join(format!("{}_orig.{ext}", id.0));
        let provisional_path = if std::fs::copy(source, &provisional).is_ok() {
            provisional.to_string_lossy().into_owned()
        } else {
            source.to_string_lossy().into_owned()
        };
        // Android's camera bridge writes a fast subsampled thumbnail next to the
        // capture; use it as the interim LIGHT preview if present (the background
        // job replaces it with the stored thumbnail). No-op on desktop.
        let provisional_thumb = {
            let mut s = source.as_os_str().to_os_string();
            s.push(".thumb.jpg");
            let p = std::path::PathBuf::from(s);
            p.exists().then(|| p.to_string_lossy().into_owned())
        };
        // Heavy work off-thread; the result lands in `photo_results`.
        let results = self.photo_results.clone();
        let inflight = self.photo_jobs_inflight.clone();
        let dir_t = dir.clone();
        let source_t = source.to_path_buf();
        let provisional_for_job = provisional_path.clone();
        inflight.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            let outcome = Self::process_photo_files(&dir_t, &source_t, quality, id);
            if let Ok(mut q) = results.lock() {
                q.push(PhotoJobResult {
                    id,
                    outcome,
                    provisional: provisional_for_job,
                });
            }
            inflight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
        Some(ber_model::ber::common::Photo {
            id,
            file_path: provisional_path,
            thumbnail_path: provisional_thumb,
            description: None,
            // Construction build-up photos have no elevation side (that's only
            // for the Mapping-phase external Front/Right/Left/Back photos).
            side: None,
        })
    }

    /// The heavy photo work — decode/recompress + thumbnail — runnable OFF the UI
    /// thread (no `&self`). Recompression is an Android concern: phone cameras
    /// hand us full-res ~12 MP JPEGs, so each compact tier downscales + re-encodes
    /// once at a fixed quality; on desktop (dev), `Full`, and any encode failure
    /// the original bytes are stored verbatim. Returns the final `(file_path,
    /// thumbnail_path)`; `None` on I/O failure (the provisional copy then stands).
    fn process_photo_files(
        dir: &std::path::Path,
        source: &std::path::Path,
        quality: PhotoQuality,
        id: ItemId,
    ) -> Option<(String, Option<String>)> {
        // TEMPORARY (dual hi/lo capture, 2026-07): the explicit low-res button
        // must yield a small file on EVERY platform, so `Low` recompresses on
        // desktop too (the other tiers keep desktop's store-verbatim shortcut —
        // dev machines pick already-sized files). Remove the `Low` arm together
        // with `photo_button_pair`.
        let spec = if cfg!(target_os = "android") || quality == PhotoQuality::Low {
            quality.compression()
        } else {
            None
        };
        let dest = match spec {
            Some((max_dim, jpeg_quality)) => {
                let dest = dir.join(format!("{}.jpg", id.0));
                Self::recompress(source, &dest, max_dim, jpeg_quality).then_some(dest)
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
        // full-res original costs ~48 MB of texture each — unworkable on a
        // tablet. Best-effort: on failure leave `None` and fall back to the full.
        let thumb_dest = dir.join(format!("{}_thumb.jpg", id.0));
        let thumbnail_path = Self::write_thumbnail(&dest, &thumb_dest)
            .then(|| thumb_dest.to_string_lossy().into_owned());
        Some((dest.to_string_lossy().into_owned(), thumbnail_path))
    }

    /// Drain finished background photo jobs (call each frame): swap each job's
    /// final paths into its photo by id, delete the superseded provisional copy,
    /// and mark dirty. A job whose photo was meanwhile removed (e.g. a failed
    /// opening placement) cleans up its produced files instead. A FAILED job
    /// leaves the provisional copy standing as the photo. Returns `true` if any
    /// photo changed (so the UI repaints to pick up the new paths).
    pub fn apply_finished_photos(&mut self) -> bool {
        let drained: Vec<PhotoJobResult> = {
            let Ok(mut q) = self.photo_results.lock() else {
                return false;
            };
            if q.is_empty() {
                return false;
            }
            std::mem::take(&mut *q)
        };
        let mut changed = false;
        for job in drained {
            let Some((file_path, thumbnail_path)) = job.outcome else {
                // Failed — the provisional copy stands as the photo. Nothing to do.
                continue;
            };
            let found = self
                .current_record
                .as_mut()
                .map(|r| {
                    r.data
                        .set_photo_paths(job.id, file_path.clone(), thumbnail_path.clone())
                })
                .unwrap_or(false);
            if found {
                if job.provisional != file_path {
                    let _ = std::fs::remove_file(&job.provisional);
                }
                changed = true;
            } else {
                // The photo was removed before the job finished — clean up orphans.
                let _ = std::fs::remove_file(&file_path);
                if let Some(t) = &thumbnail_path {
                    let _ = std::fs::remove_file(t);
                }
                let _ = std::fs::remove_file(&job.provisional);
            }
        }
        if changed {
            let _ = self.mark_dirty();
        }
        changed
    }

    /// Whether any photo job is still processing on a background thread — the UI
    /// keeps repainting/polling while true so finished photos appear promptly.
    pub fn photos_processing(&self) -> bool {
        self.photo_jobs_inflight
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0
    }

    /// Decode `src`, downscale to fit `THUMB_MAX` px on the long edge, and write
    /// a JPEG to `dest`. Returns `false` (and writes nothing) on any failure.
    /// `pub(super)` so the external-photo annotation path (`photos.rs`) reuses
    /// it for the re-saved image's preview.
    pub(super) fn write_thumbnail(src: &std::path::Path, dest: &std::path::Path) -> bool {
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
        use image::{ImageDecoder, ImageEncoder};
        let Ok(reader) = image::ImageReader::open(src).and_then(|r| r.with_guessed_format()) else {
            return false;
        };
        // Read the camera's EXIF orientation and BAKE it into the pixels: the
        // `image` crate and egui both ignore the EXIF tag, so without this a
        // portrait capture (stored landscape + an "orientation" tag) displays
        // sideways. `orientation()` is `NoTransforms` when there's no tag (e.g.
        // desktop-picked files), making this a no-op there.
        let Ok(mut decoder) = reader.into_decoder() else {
            return false;
        };
        let orientation = decoder
            .orientation()
            .unwrap_or(image::metadata::Orientation::NoTransforms);
        let Ok(mut img) = image::DynamicImage::from_decoder(decoder) else {
            return false;
        };
        img.apply_orientation(orientation);
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
        let Some(photo) = self.ingest_photo(source, quality) else {
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
        // Each region in the group shares one construction; the group can
        // span STOREYS (all-storeys lasso), so resolve construction ids
        // across the whole stack.
        let cids = self.construction_ids_for(kind, &ids);
        let Some(record) = self.current_record.as_mut() else {
            return;
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

    /// Set (or clear with `None`) a CEILING group's INSULATION spec — the
    /// attribute dialog's "Insulation thickness (mm)" box / "unknown"
    /// checkbox. Writes the value onto each of the group's (shared) ceiling
    /// constructions ([`Ber::set_ceiling_insulation`]) and mirrors it onto the
    /// session group so the open dialog reflects it at once. No-op for
    /// wall/floor groups. Like the group name/photos, no undo snapshot (the
    /// box edits value-by-value); marks dirty so it persists.
    pub fn set_structure_group_insulation(
        &mut self,
        group: usize,
        insulation: Option<CeilingInsulation>,
    ) {
        if self
            .structure_edit
            .as_ref()
            .and_then(|e| e.groups.get(group))
            .map(|g| g.ty.kind())
            != Some(SurfaceKind::Ceilings)
        {
            return;
        }
        // Apply any pending type / make-up FIRST. Those writes go to the durable
        // store and only land on the region's construction at the next
        // materialize; until then the region still points at its OLD construction.
        // Without this, an insulation set in the SAME session lands on that old
        // construction and is orphaned when materialize re-points the region — so
        // the completeness counter stays stuck (e.g. 0/3) until the "unknown" box
        // is toggled off and on again. Materializing first makes the region's
        // construction current, so the insulation lands where it's read from.
        self.materialize();
        let ids = match self
            .structure_edit
            .as_ref()
            .and_then(|e| e.groups.get(group))
        {
            Some(g) if g.ty.kind() == SurfaceKind::Ceilings => g.ids.clone(),
            _ => return,
        };
        // Each region in the group shares one construction; map the region ids
        // to their construction ids and write those (mirrors the photo attach).
        // Cross-storey like the other group ops.
        let cids = self.construction_ids_for(SurfaceKind::Ceilings, &ids);
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        for cid in cids {
            record.data.set_ceiling_insulation(cid, insulation);
        }
        // Mirror onto the session group, normalized the same way the model
        // stores it, so the open dialog sees the stored truth.
        if let Some(g) = self
            .structure_edit
            .as_mut()
            .and_then(|e| e.groups.get_mut(group))
        {
            g.insulation = CeilingInsulation::normalized(insulation);
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
        let (kind, ids, group_attrs) = {
            let Some(e) = self.structure_edit.as_mut() else {
                return;
            };
            let Some(g) = e.groups.get_mut(group) else {
                return;
            };
            g.name = name.clone();
            (
                g.ty.kind(),
                g.ids.clone(),
                (g.ty, g.makeup.clone(), g.phase.clone()),
            )
        };
        let stored = if name.trim().is_empty() {
            None
        } else {
            Some(name)
        };
        // NAME-INTEGRITY first: a name held by ANOTHER referenced same-kind
        // group with DIFFERENT attributes is rejected (a name means exactly
        // one set of attributes). Same attributes = the user's intent to BE
        // that group — the entry write below merges into it on the next
        // stamp. On rejection the durable state keeps its last valid value;
        // the typed session name stands so the assessor can keep typing.
        let cids = self.construction_ids_for(kind, &ids);
        let full = self.group_covers_all_members(kind, &ids);
        let rejection: Option<String> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            match stored.as_deref() {
                Some(n) => match record.data.named_construction_attrs(kind, n) {
                    // Holder with different attributes — and it isn't just
                    // this group's own construction (self-rename).
                    Some(attrs)
                        if attrs != group_attrs
                            && !cids.iter().any(|cid| {
                                record
                                    .data
                                    .wall_construction(*cid)
                                    .map(|c| c.base.name.as_deref().map(str::trim))
                                    .or_else(|| {
                                        record
                                            .data
                                            .floor_construction(*cid)
                                            .map(|c| c.base.name.as_deref().map(str::trim))
                                    })
                                    .or_else(|| {
                                        record
                                            .data
                                            .ceiling_construction(*cid)
                                            .map(|c| c.base.name.as_deref().map(str::trim))
                                    })
                                    .flatten()
                                    == Some(n.trim())
                            }) =>
                    {
                        let kind_word = match kind {
                            SurfaceKind::Walls => "wall",
                            SurfaceKind::Floors => "floor",
                            SurfaceKind::Ceilings => "roof",
                        };
                        Some(format!(
                            "\"{n}\" is already another {kind_word} group's name — a name \
                             means exactly one set of attributes, so pick a different name \
                             (matching attributes would already be one group)"
                        ))
                    }
                    _ => None,
                },
                None => None,
            }
        };
        if let Some(hint) = rejection {
            if let Some(e) = self.structure_edit.as_mut() {
                e.name_error = Some((group, hint));
            }
            return;
        }
        // The NAME is part of the construction identity, so a rename writes
        // the members' durable ENTRIES (the next stamp find-or-creates the
        // named construction). FULL group → also rename the construction in
        // place so its photos/insulation ride along (the group evolves).
        // SUBSET → entries only: the selected members FORK into their own
        // group under the new name, while the stay-behind members keep the
        // old construction untouched (name, photos, insulation, membership).
        // The fork starts with fresh evidence — photos/insulation belong to
        // the group they were recorded on.
        if full {
            let Some(record) = self.current_record.as_mut() else {
                return;
            };
            for cid in &cids {
                // Referenced holders were vetted above; orphans don't block.
                let _ = record
                    .data
                    .set_construction_name(kind, *cid, stored.clone());
            }
        }
        let (ty, makeup, phase) = group_attrs;
        match ty {
            ConstructionType::Wall(t) => {
                self.set_wall_construction_for(&ids, t, makeup, phase, stored.clone())
            }
            ConstructionType::Floor(t) => {
                self.set_floor_construction_for(&ids, t, makeup, phase, stored.clone())
            }
            ConstructionType::Ceiling(t) => {
                self.set_ceiling_construction_for(&ids, t, makeup, phase, stored.clone())
            }
        }
        if let Some(e) = self.structure_edit.as_mut() {
            // An accepted rename clears a stale hint for THIS group.
            if e.name_error.as_ref().is_some_and(|(g, _)| *g == group) {
                e.name_error = None;
            }
            if let Some(g) = e.groups.get_mut(group) {
                g.explicit_name = stored.clone();
                if !full {
                    // The fork starts without the old group's evidence.
                    g.photos.clear();
                    g.insulation = None;
                }
            }
        }
        let _ = self.mark_dirty();
    }

    /// "New group" on a fresh group box: skip the existing-group offer and
    /// open the full attribute editor for `group` — and THIS is where the
    /// roof-type inference lands: a `Definite` inference over a still-unset
    /// stored type is committed through the normal attrs path, so the type
    /// dropdown opens pre-selected (and persists / counts toward
    /// completeness like a hand-picked value). An `Ambiguous` inference
    /// leaves the dropdown unset — the editor shows its hint instead. A
    /// stored pick is never touched.
    pub fn begin_new_structure_group(&mut self, group: usize) {
        let seed = match self
            .structure_edit
            .as_mut()
            .and_then(|e| e.groups.get_mut(group))
        {
            Some(g) => {
                g.show_editor = true;
                match (g.ty, g.inferred) {
                    (
                        ConstructionType::Ceiling(CeilingConstructionType::Uninitialised),
                        InferredRoofType::Definite(t),
                    ) => Some((t, g.makeup.clone(), g.phase.clone())),
                    _ => None,
                }
            }
            None => return,
        };
        if let Some((t, makeup, phase)) = seed {
            self.set_structure_group_attrs(group, ConstructionType::Ceiling(t), makeup, phase);
        }
    }

    /// RESET a group box back to the two-mode entry ("choose again") — the
    /// bottom-right Reset button on EVERY box, fresh-flow or an attributed
    /// surface reselected after Done. DETACHES this
    /// selection's attribution: the durable per-surface construction keys are
    /// removed ([`detach_construction_for`](Self::detach_construction_for)),
    /// so the next materialize drops these members back to the unattributed
    /// default. The CONSTRUCTION ITSELF IS NEVER TOUCHED — its name, photos
    /// and any other members elsewhere stay exactly as they were (a
    /// construction left with no members simply stops being offered /
    /// reviewed, since both read member references). The session mirror
    /// clears to blank defaults and the box re-enters the two-mode entry.
    pub fn reset_structure_group(&mut self, group: usize) {
        let (kind, ids) = match self
            .structure_edit
            .as_ref()
            .and_then(|e| e.groups.get(group))
        {
            Some(g) => (g.ty.kind(), g.ids.clone()),
            None => return,
        };
        self.detach_construction_for(kind, &ids);
        if let Some(e) = self.structure_edit.as_mut() {
            if e.name_error.as_ref().is_some_and(|(g, _)| *g == group) {
                e.name_error = None;
            }
            if let Some(g) = e.groups.get_mut(group) {
                g.ty = match kind {
                    SurfaceKind::Walls => ConstructionType::Wall(Default::default()),
                    SurfaceKind::Floors => ConstructionType::Floor(Default::default()),
                    SurfaceKind::Ceilings => ConstructionType::Ceiling(Default::default()),
                };
                g.makeup = Default::default();
                g.phase = Default::default();
                g.name = format!("Group {}", group + 1);
                g.explicit_name = None;
                g.photos.clear();
                g.insulation = None;
                g.show_editor = false;
                g.fresh = true;
            }
        }
    }

    /// Detach the durable per-surface construction ASSIGNMENTS for `ids` — the
    /// inverse of the assign paths (walls by parametric `SurfaceRef` / riser
    /// base-edge key, floors + ceilings by centroid), across every storey like
    /// the setters. Detaching writes an EXPLICIT DEFAULT assignment
    /// (unattributed type + build-up, keeping the region's current phase) —
    /// NOT a key removal: the stamps only override where a key exists ("no
    /// stored assignment keeps the derived construction"), and for
    /// regions-as-truth surfaces the "derived" construction IS the old one
    /// (stored ceilings survive re-resolves verbatim; an edge-matched wall's
    /// amend carries its `construction_id` over) — a removed key would let
    /// the old attribution RESURRECT on the next materialize / reload. The
    /// default entry makes the detach durable through Done, materialize and
    /// save/reload. The shared construction registry entry is untouched, so
    /// other members keep their attribution and the group's name/photos
    /// survive. Snapshots for undo + marks dirty only when something was
    /// actually attributed.
    fn detach_construction_for(&mut self, kind: SurfaceKind, ids: &[ItemId]) {
        enum Key {
            WallRef(manip_plan::SurfaceRef),
            Riser(document::RiserKey),
            Area(document::CentroidKey),
        }
        // Each key with the phase its default entry should carry — the
        // region's CURRENT phase (the phase axis is derived from the zones;
        // a reset shouldn't shunt an extension wall back to "Original").
        let per_storey: Vec<(usize, Vec<(Key, ConstructionPhase)>)> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let mut out = Vec::new();
            for (idx, storey) in record.data.storeys.iter().enumerate() {
                let Some(sd) = self.document.storeys.get(idx) else {
                    continue;
                };
                let mut keys = Vec::new();
                match kind {
                    SurfaceKind::Walls => {
                        let plan = &sd.outline.floor_plan;
                        for r in ids
                            .iter()
                            .filter_map(|id| storey.walls.regions.iter().find(|r| r.id == *id))
                        {
                            let Some((e0, e1)) = r.base_edge() else {
                                continue;
                            };
                            let phase = record
                                .data
                                .wall_construction(r.construction_id)
                                .map(|c| c.base.construction_phase.clone())
                                .unwrap_or_default();
                            let mid = Point2f32::new(
                                ((e0.x + e1.x) * 0.5) as f32,
                                ((e0.y + e1.y) * 0.5) as f32,
                            );
                            if let Some(sref) = plan.surface_ref_at(mid, 0.05) {
                                keys.push((Key::WallRef(sref), phase));
                            } else {
                                keys.push((
                                    Key::Riser(document::riser_key(
                                        Point2f64::new(e0.x, e0.y),
                                        Point2f64::new(e1.x, e1.y),
                                    )),
                                    phase,
                                ));
                            }
                        }
                    }
                    SurfaceKind::Floors => {
                        for r in ids
                            .iter()
                            .filter_map(|id| storey.floors.regions.iter().find(|r| r.id == *id))
                        {
                            let phase = record
                                .data
                                .floor_construction(r.construction_id)
                                .map(|c| c.base.construction_phase.clone())
                                .unwrap_or_default();
                            keys.push((Key::Area(document::centroid_key(r.centroid())), phase));
                        }
                    }
                    SurfaceKind::Ceilings => {
                        for r in ids
                            .iter()
                            .filter_map(|id| storey.ceilings.regions.iter().find(|r| r.id == *id))
                        {
                            let phase = record
                                .data
                                .ceiling_construction(r.construction_id)
                                .map(|c| c.base.construction_phase.clone())
                                .unwrap_or_default();
                            keys.push((Key::Area(document::centroid_key(r.centroid())), phase));
                        }
                    }
                }
                if !keys.is_empty() {
                    out.push((idx, keys));
                }
            }
            out
        };
        // Anything actually attributed? (Skip the undo entry for a Reset on a
        // box where nothing was applied yet.) Attribution can live in either
        // side of the mirror: a non-default doc-map entry (just applied,
        // pre-materialize) or a non-default resolved construction (applied
        // earlier — including regions-as-truth carriers with no map key).
        let attributed = {
            let record = self.current_record.as_ref().unwrap();
            let region_attributed = |id: &ItemId| {
                record.data.storeys.iter().any(|s| match kind {
                    SurfaceKind::Walls => s
                        .walls
                        .regions
                        .iter()
                        .find(|r| r.id == *id)
                        .and_then(|r| record.data.wall_construction(r.construction_id))
                        .is_some_and(|c| {
                            c.construction_type.is_set() || c.base.construction_makeup.is_set()
                        }),
                    SurfaceKind::Floors => s
                        .floors
                        .regions
                        .iter()
                        .find(|r| r.id == *id)
                        .and_then(|r| record.data.floor_construction(r.construction_id))
                        .is_some_and(|c| {
                            c.construction_type.is_set() || c.base.construction_makeup.is_set()
                        }),
                    SurfaceKind::Ceilings => s
                        .ceilings
                        .regions
                        .iter()
                        .find(|r| r.id == *id)
                        .and_then(|r| record.data.ceiling_construction(r.construction_id))
                        .is_some_and(|c| {
                            c.construction_type.is_set() || c.base.construction_makeup.is_set()
                        }),
                })
            };
            let map_attributed = per_storey.iter().any(|(idx, keys)| {
                let sd = &self.document.storeys[*idx];
                keys.iter().any(|(k, _)| match k {
                    Key::WallRef(r) => sd
                        .wall_constructions
                        .get(r)
                        .is_some_and(|(ty, mk, ..)| ty.is_set() || mk.is_set()),
                    Key::Riser(rk) => sd
                        .riser_constructions
                        .get(rk)
                        .is_some_and(|(ty, mk, ..)| ty.is_set() || mk.is_set()),
                    Key::Area(ck) => match kind {
                        SurfaceKind::Floors => sd
                            .floor_constructions
                            .get(ck)
                            .is_some_and(|(ty, mk, ..)| ty.is_set() || mk.is_set()),
                        SurfaceKind::Ceilings => sd
                            .ceiling_constructions
                            .get(ck)
                            .is_some_and(|(ty, mk, ..)| ty.is_set() || mk.is_set()),
                        SurfaceKind::Walls => false,
                    },
                })
            });
            ids.iter().any(region_attributed) || map_attributed
        };
        if !attributed {
            return;
        }
        self.record_history();
        for (idx, keys) in per_storey {
            let sd = &mut self.document.storeys[idx];
            for (k, phase) in keys {
                match k {
                    Key::WallRef(r) => {
                        sd.wall_constructions
                            .insert(r, (Default::default(), Default::default(), phase, None));
                    }
                    Key::Riser(rk) => {
                        sd.riser_constructions
                            .insert(rk, (Default::default(), Default::default(), phase, None));
                    }
                    Key::Area(ck) => match kind {
                        SurfaceKind::Floors => {
                            sd.floor_constructions
                                .insert(ck, (Default::default(), Default::default(), phase, None));
                        }
                        SurfaceKind::Ceilings => {
                            sd.ceiling_constructions
                                .insert(ck, (Default::default(), Default::default(), phase, None));
                        }
                        SurfaceKind::Walls => {}
                    },
                }
            }
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
    ///
    /// NAME carry: the name is part of the construction identity. When the
    /// session group covers ALL of its construction's members, the group's
    /// explicit name rides along (the group evolves, keeping its name); when
    /// it covers a SUBSET, the diverging members leave WITHOUT the name (they
    /// split into a new unnamed group — the stay-behind members keep the
    /// named one; carrying the name would mint a same-name group with
    /// different attributes, the very thing the name-integrity rule forbids).
    pub fn set_structure_group_attrs(
        &mut self,
        group: usize,
        ty: ConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) {
        let (ids, kind_before) = {
            let Some(e) = self.structure_edit.as_ref() else {
                return;
            };
            let Some(g) = e.groups.get(group) else {
                return;
            };
            (g.ids.clone(), g.ty.kind())
        };
        let full = self.group_covers_all_members(kind_before, &ids);
        let name = {
            let Some(e) = self.structure_edit.as_mut() else {
                return;
            };
            let Some(g) = e.groups.get_mut(group) else {
                return;
            };
            g.ty = ty;
            g.makeup = makeup.clone();
            g.phase = phase.clone();
            // Any attribute apply resolves the fresh two-mode entry — the box
            // now shows the full (filled) editor, whether the values came
            // from "New group" typing or an existing-group pick.
            g.show_editor = true;
            if !full {
                // Subset divergence: the members split into a NEW unnamed
                // group — reflect that in the session too.
                g.explicit_name = None;
                g.name = format!("Group {}", group + 1);
            }
            g.explicit_name.clone()
        };
        match ty {
            ConstructionType::Wall(t) => {
                self.set_wall_construction_for(&ids, t, makeup, phase, name)
            }
            ConstructionType::Floor(t) => {
                self.set_floor_construction_for(&ids, t, makeup, phase, name)
            }
            ConstructionType::Ceiling(t) => {
                self.set_ceiling_construction_for(&ids, t, makeup, phase, name)
            }
        }
    }

    /// Whether the session group's `ids` cover EVERY resolved member of the
    /// construction(s) they currently reference — the full-group-vs-subset
    /// decision behind name carry and rename forking. A SUBSET edit must
    /// never mutate/carry what the stay-behind members share.
    fn group_covers_all_members(&self, kind: SurfaceKind, ids: &[ItemId]) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        let id_set: HashSet<ItemId> = ids.iter().copied().collect();
        self.construction_ids_for(kind, ids).iter().all(|cid| {
            record
                .data
                .construction_member_ids(kind, *cid)
                .iter()
                .all(|m| id_set.contains(m))
        })
    }

    /// Close the "Edit attributes" session (attributes apply as they're set).
    /// Done ALWAYS closes — an untouched or half-filled group is a legitimate
    /// exit (e.g. two ceilings grouped together that the assessor intends to
    /// reselect and attribute one by one): it simply stays INCOMPLETE, with
    /// the completeness predicate ([`EditGroup::is_complete`] /
    /// `CeilingInsulation::spec_done`) still enforcing the insulation rule in
    /// the group tick, the N/M headers, the checklist row, the `Remaining`
    /// subtool and the Review-entry gate. Done DESELECTS everything — the
    /// halo shouldn't linger once the assessor has finished attributing (any
    /// staged carve state clears with it). When the `Remaining` subtool is
    /// active this is the "work until done" loop: AFTER the clear, re-seed it
    /// so the still-incomplete surfaces — including any just closed
    /// unattributed — select again and the loop shrinks toward empty
    /// (materialize first so the re-resolved constructions the seed reads are
    /// current).
    pub fn end_structure_edit(&mut self) {
        self.close_structure_edit();
        self.clear_structure_surface_selection();
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
        self.toggle_structure_surface_in(None, world_x, world_y, tol);
    }

    /// The ALL-STOREYS variant of
    /// [`toggle_structure_surface_at`](Self::toggle_structure_surface_at):
    /// the tap landed in storey `idx`'s CELL (the all-storeys canvas resolves
    /// which cell, and converts the point into that storey's own world
    /// frame), so hit-test that storey's regions. Same kind priority and
    /// structure-scope filtering; region ids are unique house-wide, so the
    /// one selection set spans storeys — a few taps across cells build the
    /// same cross-storey group a lasso would.
    pub fn toggle_structure_surface_at_in(
        &mut self,
        idx: usize,
        world_x: f64,
        world_y: f64,
        tol: f64,
    ) {
        self.toggle_structure_surface_in(Some(idx), world_x, world_y, tol);
    }

    /// Shared body of the two taps: hit-test `storey_idx` (`None` = the
    /// active storey) by kind priority and toggle the first hit in/out of the
    /// selection.
    fn toggle_structure_surface_in(
        &mut self,
        storey_idx: Option<usize>,
        world_x: f64,
        world_y: f64,
        tol: f64,
    ) {
        if self.structure_select.is_none() {
            return;
        }
        // With several modes active, a tap can land on more than one kind (a
        // wall edge over a floor, say). Resolve by priority — wall → roof →
        // floor — and toggle the first hit (see the overlap-pick rule).
        let hit = {
            let Some(storey) = self.storey_for(storey_idx) else {
                return;
            };
            [
                SurfaceKind::Walls,
                SurfaceKind::Ceilings,
                SurfaceKind::Floors,
            ]
            .into_iter()
            .filter(|k| self.structure_kinds.contains(k))
            .find_map(|k| storey.structure_surface_at(k, world_x, world_y, tol))
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
        self.pending_wall_inclusion = None;
        self.queued_wall_inclusions.clear();
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
    }

    /// The staged (not-yet-split) carve rings, plan coords, for the canvas to
    /// shade as the pending selection — including the ring the wall-inclusion
    /// ask is currently walking (it's committed to the selection; only its wall
    /// membership is still being answered). Empty when no carve is staged.
    ///
    /// SINGLE-STOREY canvas view: rings tagged with another storey (lasso
    /// regions from the all-storeys view) are omitted — their coordinates are
    /// that storey's frame, not the drawn one. The all-storeys canvas reads
    /// [`pending_structure_carve_polygons_by_storey`](Self::pending_structure_carve_polygons_by_storey)
    /// instead.
    pub fn pending_structure_carve_polygons(&self) -> Vec<Vec<(f64, f64)>> {
        let mine = |storey: Option<usize>| storey.is_none() || storey == self.current_storey_idx;
        self.pending_structure_carves
            .iter()
            .filter(|c| mine(c.storey))
            .map(|c| c.ring.iter().map(|p| (p.x, p.y)).collect())
            .chain(
                self.pending_wall_inclusion
                    .as_ref()
                    .filter(|p| mine(p.storey))
                    .map(|p| p.ring.iter().map(|p| (p.x, p.y)).collect()),
            )
            .collect()
    }

    /// The staged carve rings TAGGED by storey, for the all-storeys canvas to
    /// shade each into its own cell (each ring is in its storey's own world
    /// frame). Untagged rings (single-storey staging) map to the current
    /// storey; queued lasso asks' rings are included like the active one.
    pub fn pending_structure_carve_polygons_by_storey(&self) -> Vec<(usize, Vec<(f64, f64)>)> {
        let resolve = |storey: Option<usize>| storey.or(self.current_storey_idx);
        self.pending_structure_carves
            .iter()
            .filter_map(|c| Some((resolve(c.storey)?, c.ring.as_slice())))
            .chain(
                self.pending_wall_inclusion
                    .iter()
                    .chain(self.queued_wall_inclusions.iter())
                    .filter_map(|p| Some((resolve(p.storey)?, p.ring.as_slice()))),
            )
            .map(|(i, ring)| (i, ring.iter().map(|p| (p.x, p.y)).collect()))
            .collect()
    }

    /// The current wall-inclusion question, if a staged carve is awaiting
    /// per-edge Include/Exclude answers: the edge to highlight plus its cursor
    /// position. `None` when nothing is being asked.
    pub fn wall_inclusion_prompt(&self) -> Option<WallInclusionPromptView> {
        let p = self.pending_wall_inclusion.as_ref()?;
        let e = p.edges.get(p.cursor)?;
        // A cross-storey lasso ask names its storey up front, so the question
        // identifies its target even where the canvas can't highlight it.
        let prefix = p
            .storey
            .and_then(|i| self.storey_name(i))
            .map(|n| format!("{n}: "))
            .unwrap_or_default();
        Some(WallInclusionPromptView {
            edge: ((e[0].x, e[0].y), (e[1].x, e[1].y)),
            index: p.cursor,
            total: p.edges.len(),
            description: format!(
                "{prefix}{}",
                self.wall_inclusion_description(p.storey, &p.ring, e)
            ),
            storey_idx: p.storey,
        })
    }

    /// Display name of storey `i` ("Ground Floor" / "Storey 2" fallback), for
    /// prompt text that must identify a storey in the all-storeys view.
    fn storey_name(&self, i: usize) -> Option<String> {
        let s = self.current_record.as_ref()?.data.storeys.get(i)?;
        Some(if s.name.is_empty() {
            format!("Storey {}", s.storey_number + 1)
        } else {
            s.name.clone()
        })
    }

    /// The resolved storey a staged carve / ask refers to: `Some(i)` → storey
    /// `i`; `None` → the active storey (the single-storey paths).
    fn storey_for(&self, idx: Option<usize>) -> Option<&Storey> {
        match idx {
            Some(i) => self.current_record.as_ref()?.data.storeys.get(i),
            None => self.current_storey(),
        }
    }

    /// Compose the human description of the CURRENT wall-inclusion question:
    /// which side of the carved region the asked edge sits on (screen terms —
    /// the canvas draws world +x rightward and +y downward, so "top" is what
    /// the assessor sees at the top), how long the edge is, and what the wall
    /// standing on it IS (a step riser by kind, else its exposure class).
    /// UI-facing view derivation over resolved regions, so it lives on the
    /// shell; the region query is the storey's
    /// ([`Storey::wall_ids_on_segment`]).
    fn wall_inclusion_description(
        &self,
        storey_idx: Option<usize>,
        ring: &[Point2f64],
        edge: &[Point2f64; 2],
    ) -> String {
        let (a, b) = (edge[0], edge[1]);
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        // The side: the edge midpoint's outward direction from the region
        // centroid, along the dominant axis.
        let n = ring.len().max(1) as f64;
        let (sx, sy) = ring
            .iter()
            .fold((0.0, 0.0), |(sx, sy), p| (sx + p.x, sy + p.y));
        let (mx, my) = ((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
        let (dx, dy) = (mx - sx / n, my - sy / n);
        let side = if dx.abs() > dy.abs() {
            if dx > 0.0 {
                "right"
            } else {
                "left"
            }
        } else if dy > 0.0 {
            "bottom"
        } else {
            "top"
        };
        format!(
            "Include the {len:.1} m wall on the {side} edge{}?",
            self.wall_on_edge_label(storey_idx, a, b)
        )
    }

    /// What the wall standing on segment `a`→`b` IS, as a parenthesised label:
    /// a step riser's kind when one stands there (the same id set the
    /// exclusion subtracts), otherwise the wall region's exposure class (the
    /// exposure-toolbar wording). Empty when nothing resolvable stands on the
    /// segment (the ask filter makes that unreachable in practice).
    fn wall_on_edge_label(&self, storey_idx: Option<usize>, a: Point2f64, b: Point2f64) -> String {
        let Some(storey) = self.storey_for(storey_idx) else {
            return String::new();
        };
        let ids = storey.wall_ids_on_segment(a, b);
        let Some(first) = ids.first() else {
            return String::new();
        };
        // The riser classifier lives on the storey's OWN editing doc (its
        // step lines) — index it explicitly so a lasso ask about another
        // storey classifies against the right one.
        let doc_idx = storey_idx.or(self.current_storey_idx);
        let riser = doc_idx
            .and_then(|i| self.document.storeys.get(i))
            .map(|d| d.riser_walls(storey))
            .unwrap_or_default()
            .into_iter()
            .find(|r| ids.contains(&r.id));
        if let Some(riser) = riser {
            return match riser.kind {
                document::RiserKind::Floor => " (floor-step riser)".into(),
                document::RiserKind::Ceiling => " (ceiling-step riser)".into(),
            };
        }
        let exposure = storey
            .walls
            .regions
            .iter()
            .find(|w| w.id == *first)
            .map(|w| w.exposure);
        match exposure {
            Some(WallExposure::Exposed) => " (Exposed wall)",
            Some(WallExposure::SemiExposed) => " (Semi-exposed wall)",
            Some(WallExposure::NonHeatLoss) => " (Non-heat-loss wall)",
            Some(WallExposure::Ground) => " (ground-contact wall)",
            Some(WallExposure::InterStorey) => " (inter-storey wall)",
            Some(WallExposure::Unset) => " (unclassified wall)",
            None => "",
        }
        .into()
    }

    /// Answer the current wall-inclusion question: `include: false` records the
    /// edge so its walls are subtracted from the committed selection; `true`
    /// keeps them (the `surfaces_in_ring` boundary test already picks them up).
    /// Advances to the next ambiguous edge; answering the last one STAGES the
    /// carve. No-op when nothing is being asked. The decisions apply to this
    /// staging only.
    pub fn resolve_wall_inclusion(&mut self, include: bool) {
        let Some(p) = self.pending_wall_inclusion.as_mut() else {
            return;
        };
        if let Some(edge) = p.edges.get(p.cursor) {
            if !include {
                p.excluded.push(*edge);
            }
            p.cursor += 1;
        }
        if p.cursor >= p.edges.len() {
            let p = self.pending_wall_inclusion.take().unwrap();
            self.pending_structure_carves.push(StagedStructureCarve {
                ring: p.ring,
                excluded_edges: p.excluded,
                storey: p.storey,
                lasso: p.lasso,
            });
            // A cross-storey lasso queues one ask per storey/region — answer
            // them in sequence.
            self.advance_wall_inclusion_queue();
        }
    }

    /// Pop the next queued wall-inclusion ask into the active slot, if the
    /// slot is free. No-op when an ask is already up or the queue is empty.
    fn advance_wall_inclusion_queue(&mut self) {
        if self.pending_wall_inclusion.is_none() && !self.queued_wall_inclusions.is_empty() {
            self.pending_wall_inclusion = Some(self.queued_wall_inclusions.remove(0));
        }
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
        // Snap-tag capture: the pre-action state (a pending CUT pushes no undo
        // entry, so its capture carries this `before` itself), and drop stale
        // decisions from unrelated ops so only THIS stroke's snaps are tagged.
        let before = self.current_history_snapshot();
        self.begin_snap_capture();
        // Carve against the whole outline (no holes) — the split ring is fixed
        // by dimensioning before it carves the surface.
        self.sync_active();
        // Reference walls besides the outline: the construction-PHASE lines, as
        // in the living-area flow. A carve edge drawn beside a phase line lands
        // flush on it, and — because a snapped wall is a known wall — the SAME
        // set feeds the pinning below so the snapped edge is pre-known in
        // dimensioning (the pin matches the snap; declining the snap tag
        // reopens the question).
        let ref_walls = self.phase_reference_walls();
        let wall_snap_tol = self.snap_tol_carve();
        match self.document.carve_or_cut_in_available(
            &pts,
            &[],
            &self.tuning.subarea,
            &ref_walls,
            wall_snap_tol,
        ) {
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                self.pending_structure_cut = Some([
                    self.pinned_subarea_slot_refs(&boundary, a, &ref_walls),
                    self.pinned_subarea_slot_refs(&boundary, b, &ref_walls),
                ]);
                self.capture_snap_action_pending(
                    SnapAction::Subarea {
                        flow: SubareaFlow::Structure,
                        points: world_points.to_vec(),
                        pick: None,
                    },
                    before,
                    manip_plan::take_snap_decisions(),
                );
            }
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                if plan.validate_simple().is_err() {
                    // Nothing committed — drop the rejected carve's decisions.
                    let _ = manip_plan::take_snap_decisions();
                    return;
                }
                let Some(kind) = self.primary_structure_kind() else {
                    let _ = manip_plan::take_snap_decisions();
                    return;
                };
                let undo = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot_refs(&boundary, plan, &ref_walls);
                self.begin_structure_split_dimensioning(slot, kind, undo);
                self.capture_snap_action(SnapAction::Subarea {
                    flow: SubareaFlow::Structure,
                    points: world_points.to_vec(),
                    pick: None,
                });
            }
            Err(e) => {
                log::error!("structure carve_or_cut: {}", e);
                let _ = manip_plan::take_snap_decisions();
            }
        }
    }

    /// A CROSS-STOREY LASSO carve from the all-storeys 2D view: one drawn
    /// stroke, delivered by the UI once PER STOREY in that storey's own world
    /// frame (each all-storeys cell has its own screen↔world transform, so
    /// the same screen stroke lands at different world coordinates per
    /// storey). Unlike the single-storey carve, the stroke is a FREE lasso:
    /// no splice/attach/collapse against any outline (it can span two plans
    /// plus the gap between them) and no dimensioning — the ring is closed
    /// and RDP-simplified ([`manip_plan::lasso_ring`]), clipped to each
    /// storey's footprint ([`Storey::lasso_regions`]; the gap contributes
    /// nothing), and each clipped region is STAGED as a selection-only carve.
    /// Interior clip edges that carry a (structure-scope) wall raise the same
    /// per-edge wall-inclusion ask as the single-storey carve, queued across
    /// storeys and answered in sequence.
    pub fn apply_structure_lasso_stroke(&mut self, per_storey: &[Vec<(f32, f32)>]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if !matches!(self.structure_select, Some(SelectMode::Carve)) {
            return;
        }
        // Regions must be current before the clip / wall lookups read them.
        self.materialize();
        let mut staged: Vec<StagedStructureCarve> = Vec::new();
        let mut asks: Vec<WallInclusionPrompt> = Vec::new();
        {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            for (idx, stroke) in per_storey.iter().enumerate() {
                let Some(storey) = record.data.storeys.get(idx) else {
                    continue;
                };
                let pts: Vec<Point2f32> =
                    stroke.iter().map(|(x, y)| Point2f32::new(*x, *y)).collect();
                let Some(ring32) = manip_plan::lasso_ring(&pts, &self.tuning.subarea) else {
                    continue;
                };
                let ring64: Vec<Point2f64> =
                    ring32.iter().map(|p| domain::points::to_f64(*p)).collect();
                for (region_ring, interior) in storey.lasso_regions(&ring64) {
                    // Only interior clip edges a selectable wall actually
                    // stands on are worth asking about (`wall_ids_on_segment`
                    // is structure-scope filtered, so an edge whose only wall
                    // is non-heat-loss / inter-storey asks nothing).
                    let ambiguous: Vec<[Point2f64; 2]> = interior
                        .into_iter()
                        .filter(|e| !storey.wall_ids_on_segment(e[0], e[1]).is_empty())
                        .collect();
                    if ambiguous.is_empty() {
                        staged.push(StagedStructureCarve {
                            ring: region_ring,
                            excluded_edges: Vec::new(),
                            storey: Some(idx),
                            lasso: true,
                        });
                    } else {
                        asks.push(WallInclusionPrompt {
                            ring: region_ring,
                            edges: ambiguous,
                            cursor: 0,
                            excluded: Vec::new(),
                            storey: Some(idx),
                            lasso: true,
                        });
                    }
                }
            }
        }
        self.pending_structure_carves.extend(staged);
        self.queued_wall_inclusions.extend(asks);
        self.advance_wall_inclusion_queue();
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
            // The stroke's tags stay live: replay now re-runs stroke + pick.
            self.record_subarea_pick(SubareaFlow::Structure, Some((world_x, world_y)));
        } else {
            // Cancelled — the cut (and the snaps its tags describe) is gone.
            self.record_subarea_pick(SubareaFlow::Structure, None);
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
    ///
    /// Wall inclusion along the ring's edges is only automatic where it is
    /// UNAMBIGUOUS: an edge on the storey OUTLINE has nothing beyond it (true
    /// exterior boundary), so its wall is definitely part of the carved
    /// selection. An edge with house area on the other side (interior — a
    /// straight-cut line, or any edge the dwelling continues past) is a
    /// judgement call: when Walls mode is active AND a wall region actually
    /// stands on the edge (a step riser, say — a cut through open floor has no
    /// wall to include, so it asks nothing), the edge opens the per-edge
    /// wall-inclusion ask ([`wall_inclusion_prompt`]) and the carve stages once
    /// the last edge is answered.
    ///
    /// Seam note: the interior-vs-outline classification lives in `document`
    /// ([`document::StoreyDoc::ambiguous_carve_edges`]); the edge→wall-region
    /// mapping lives on the resolved storey ([`Storey::wall_ids_on_segment`]).
    /// The shell only SEQUENCES the two at prompt-open time — classify, then
    /// keep the edges a wall stands on.
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
        // The classification is the document's (this storey's footprint is the
        // test); only relevant while the selection acts on Walls.
        let interior = if self.structure_kinds.contains(&SurfaceKind::Walls) {
            self.document.ambiguous_carve_edges(&ring64)
        } else {
            Vec::new()
        };
        self.abandon_structure_split_shape();
        // Only interior edges a wall region actually STANDS ON are worth
        // asking about. Re-derive first so the wall regions the check reads
        // are current (a no-op unless an edit is pending).
        let ambiguous: Vec<[Point2f64; 2]> = if interior.is_empty() {
            Vec::new()
        } else {
            self.materialize();
            match self.current_storey() {
                Some(storey) => interior
                    .into_iter()
                    .filter(|e| !storey.wall_ids_on_segment(e[0], e[1]).is_empty())
                    .collect(),
                None => Vec::new(),
            }
        };
        if ambiguous.is_empty() {
            self.pending_structure_carves.push(StagedStructureCarve {
                ring: ring64,
                excluded_edges: Vec::new(),
                storey: None,
                lasso: false,
            });
        } else {
            self.pending_wall_inclusion = Some(WallInclusionPrompt {
                ring: ring64,
                edges: ambiguous,
                cursor: 0,
                excluded: Vec::new(),
                storey: None,
                lasso: false,
            });
        }
    }

    /// COMMIT every pending carve into real splits — the deferred half of
    /// [`stage_structure_carve`], run when "Edit attributes" opens. For each
    /// staged ring, push it onto each active kind's durable split field (walls:
    /// the fixed interior cut segment, only where the cut crosses a wall;
    /// floors/ceilings: the whole ring), then re-resolve ONCE and auto-select the
    /// resulting pieces across every active kind. Snapshots for undo so opening
    /// the editor stays reversible. No-op when nothing is pending.
    pub(super) fn commit_pending_structure_carves(&mut self) {
        // An unanswered wall-inclusion ask flushes as INCLUDE for the remaining
        // edges (the pre-ask behaviour) — opening the editor mid-ask shouldn't
        // silently drop the carve. Queued lasso asks flush the same way.
        for p in self
            .pending_wall_inclusion
            .take()
            .into_iter()
            .chain(std::mem::take(&mut self.queued_wall_inclusions))
        {
            self.pending_structure_carves.push(StagedStructureCarve {
                ring: p.ring,
                excluded_edges: p.excluded,
                storey: p.storey,
                lasso: p.lasso,
            });
        }
        if self.pending_structure_carves.is_empty() {
            return;
        }
        let all_carves = std::mem::take(&mut self.pending_structure_carves);
        // LASSO regions are SELECTION-ONLY: an approximate all-storeys gesture
        // is never dimensioned, so it must not subdivide any geometry — just
        // select what each clipped ring encloses on its own storey.
        let (lassos, carves): (Vec<_>, Vec<_>) = all_carves.into_iter().partition(|c| c.lasso);
        for carve in &lassos {
            self.select_surfaces_in_region(carve);
        }
        if carves.is_empty() {
            return;
        }
        // A split needs an active storey (never the read-only All-storeys tab).
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let kinds: Vec<SurfaceKind> = self.structure_kinds.iter().copied().collect();

        // Walls split only along a carve ring's INTERIOR edges (where it crosses a
        // wall); floors/ceilings take the whole ring. Compute the wall cuts per ring
        // up front (needs `&self.document`) before the `&mut` record borrow.
        let want_walls = kinds.contains(&SurfaceKind::Walls);
        let per_ring: Vec<(StagedStructureCarve, Vec<[Point2f64; 2]>)> = carves
            .into_iter()
            .map(|carve| {
                let cuts = if want_walls {
                    let ring32: Vec<Point2f32> = carve
                        .ring
                        .iter()
                        .map(|p| domain::points::to_f32(*p))
                        .collect();
                    self.document.outline.floor_plan.interior_cut_edges(&ring32)
                } else {
                    Vec::new()
                };
                (carve, cuts)
            })
            .collect();

        // Does anything actually SUBDIVIDE? (an area kind active, or a cut that
        // crossed a wall). A wall carve whose ring runs entirely along existing
        // walls crosses none mid-span → nothing to split, but carve is still a
        // SELECTION gesture, so we select the enclosed surfaces regardless.
        let will_split = kinds
            .iter()
            .any(|k| matches!(k, SurfaceKind::Floors | SurfaceKind::Ceilings))
            || per_ring.iter().any(|(_, c)| !c.is_empty());

        if will_split {
            self.record_history();
            let mut pushed_roof = false;
            if let Some(storey) = self
                .current_record
                .as_mut()
                .and_then(|r| r.data.storeys.get_mut(idx))
            {
                for (carve, cuts) in &per_ring {
                    for kind in &kinds {
                        match kind {
                            SurfaceKind::Walls => {
                                if !cuts.is_empty() {
                                    storey.push_structure_split(
                                        SurfaceKind::Walls,
                                        &carve.ring,
                                        cuts.clone(),
                                    );
                                }
                            }
                            SurfaceKind::Floors => {
                                storey.push_structure_split(
                                    SurfaceKind::Floors,
                                    &carve.ring,
                                    Vec::new(),
                                );
                            }
                            SurfaceKind::Ceilings => {
                                storey.push_structure_split(
                                    SurfaceKind::Ceilings,
                                    &carve.ring,
                                    Vec::new(),
                                );
                                pushed_roof = true;
                            }
                        }
                    }
                }
            } else {
                return;
            }
            // Regions-as-truth: a ROOF carve is a one-shot coplanar face split on
            // the stored ceilings (replayed via `recompose_ceilings`); floors/walls
            // replay their durable splits in `resolve_storey`.
            if pushed_roof {
                self.recompose_ceilings(idx);
            }
            let _ = self.mark_dirty();
            self.materialize(); // re-resolve applies the splits + re-homes openings
        }

        // Select the carved area's surfaces across every active kind — whether or
        // not anything was subdivided (with no split the already-resolved regions
        // are current, so this picks the surfaces the ring encloses).
        for (carve, _) in &per_ring {
            self.select_surfaces_in_region(carve);
        }
    }

    /// Add every surface of the active kind inside the staged carve's ring
    /// (world coords) to the selection — the carve-pick query, delegated to the
    /// storey — MINUS the walls along the carve edges the assessor excluded in
    /// the wall-inclusion ask (walls on the ring's outline edges stay: they're
    /// the storey's exterior boundary, never ambiguous, never excludable).
    fn select_surfaces_in_region(&mut self, carve: &StagedStructureCarve) {
        if self.structure_select.is_none() {
            return;
        }
        let kinds: Vec<SurfaceKind> = self.structure_kinds.iter().copied().collect();
        let hits: Vec<ItemId> = match self.storey_for(carve.storey) {
            Some(storey) => {
                let excluded: HashSet<ItemId> = carve
                    .excluded_edges
                    .iter()
                    .flat_map(|e| storey.wall_ids_on_segment(e[0], e[1]))
                    .collect();
                kinds
                    .iter()
                    .flat_map(|k| storey.surfaces_in_ring(*k, &carve.ring))
                    .filter(|id| !excluded.contains(id))
                    .collect()
            }
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
        // On a specific storey the seed reads that storey; on the all-storeys
        // tab it spans the whole stack (the tab's Structure tools act
        // house-wide — same scope as the lasso carve).
        for idx in self.structure_storey_indices() {
            let Some(record) = self.current_record.as_ref() else {
                break;
            };
            for kind in &kinds {
                match mode {
                    SelectMode::All => {
                        if let Some(s) = record.data.storeys.get(idx) {
                            ids.extend(s.surface_ids(*kind));
                        }
                    }
                    SelectMode::Remaining => {
                        ids.extend(record.data.incomplete_surface_ids(idx, *kind));
                    }
                    _ => {}
                }
            }
        }
        self.structure_selected = ids;
    }

    /// The storey indices the Structure tools currently act on: just the
    /// active storey, or every storey on the all-storeys tab.
    fn structure_storey_indices(&self) -> Vec<usize> {
        match self.current_storey_idx {
            Some(i) => vec![i],
            None => {
                let n = self
                    .current_record
                    .as_ref()
                    .map(|r| r.data.storeys.len())
                    .unwrap_or(0);
                (0..n).collect()
            }
        }
    }

    /// Drop every selected id belonging to `kind` (used when a mode is toggled
    /// OFF in an additive `Click`/`Carve` selection — the rest stays put).
    pub(super) fn drop_selection_of_kind(&mut self, kind: SurfaceKind) {
        let mut drop_ids: HashSet<ItemId> = HashSet::new();
        for idx in self.structure_storey_indices() {
            if let Some(s) = self
                .current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(idx))
            {
                drop_ids.extend(s.surface_ids(kind));
            }
        }
        self.structure_selected.retain(|id| !drop_ids.contains(id));
    }

    /// The highest-priority active mode (wall → roof → floor) — the kind a carve
    /// SPLIT acts on when several modes are active. `None` when no mode is active.
    pub(super) fn primary_structure_kind(&self) -> Option<SurfaceKind> {
        [
            SurfaceKind::Walls,
            SurfaceKind::Ceilings,
            SurfaceKind::Floors,
        ]
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

    /// The distinct construction ids referenced by the given region `ids` of
    /// `kind`, searched across EVERY storey — the group operations (photo
    /// attach / rename / insulation) can act on an all-storeys lasso group
    /// whose surfaces span the stack (region ids are unique house-wide, so
    /// the search never mis-attributes).
    fn construction_ids_for(&self, kind: SurfaceKind, ids: &[ItemId]) -> Vec<ItemId> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for storey in &record.data.storeys {
            for id in ids {
                let cid = match kind {
                    SurfaceKind::Walls => storey
                        .walls
                        .regions
                        .iter()
                        .find(|r| r.id == *id)
                        .map(|r| r.construction_id),
                    SurfaceKind::Floors => storey
                        .floors
                        .regions
                        .iter()
                        .find(|r| r.id == *id)
                        .map(|r| r.construction_id),
                    SurfaceKind::Ceilings => storey
                        .ceilings
                        .regions
                        .iter()
                        .find(|r| r.id == *id)
                        .map(|r| r.construction_id),
                };
                if let Some(c) = cid {
                    if seen.insert(c) {
                        out.push(c);
                    }
                }
            }
        }
        out
    }
}

/// A completed background photo-processing job, queued for
/// [`apply_finished_photos`](AppController::apply_finished_photos) to fold into
/// the model by id.
pub(super) struct PhotoJobResult {
    /// The provisional photo's id (minted at ingest), used to find it in the model.
    id: ItemId,
    /// Final `(file_path, thumbnail_path)` on success; `None` if processing failed
    /// (the provisional copy then stands as the photo).
    outcome: Option<(String, Option<String>)>,
    /// The provisional fast-copy path, deleted once the final file supersedes it.
    provisional: String,
}

/// Stored-photo quality tier, chosen per attach site. Drives how a captured
/// image is recompressed into the photo store: the compact tiers downscale +
/// re-encode at a fixed quality (most shots don't need full resolution), while
/// `Full` keeps the original bytes verbatim for shots where text must stay
/// legible (labels, datasheets — OCR/zoom). Recompression runs on Android only;
/// desktop stores the picked file as-is.
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
            // TEMPORARY (dual hi/lo capture, 2026-07): `Low` is the explicit
            // "small file" button's tier (`photo_button_pair` in the UI) —
            // tuned aggressively (1024 px long edge, JPEG q30: clearly legible,
            // typically a few tens of KB) and, unlike the other tiers, applied
            // on EVERY platform (see `process_photo_files`). Retune/remove with
            // the dual buttons when the smarter size policy lands.
            PhotoQuality::Low => Some((1024, 30)),
            PhotoQuality::Medium => Some((1600, 70)),
            PhotoQuality::High => Some((2048, 85)),
            PhotoQuality::Full => None,
        }
    }
}
