//! Structure-phase surface SELECTION: the app-shell interaction layer over the
//! current storey's resolved regions (walls / roofs / floors). The selected set
//! holds region [`ItemId`]s of the active [`SurfaceKind`]; it's seeded by the
//! Select subtools (`Select all` / `Select remaining` fill it, `Select` starts
//! empty) and refined by tapping or by carving a region to pick.
//!
//! The hit-testing/geometry itself lives one crate down: which surface a tap
//! lands on and which surfaces sit inside a carved region are pure read-only
//! queries over the BER `Storey` data model — see [`Storey::surface_at`],
//! [`Storey::surfaces_in_ring`], [`Storey::surface_ids`] in `ber_model`. This
//! module only sequences those queries, drives the carve through `document`,
//! and holds the transient selected set (app-shell interaction state — the
//! regions live on the BER record, not the editing `Document`).
use super::*;
use ber_model::ber::common::ConstructionPhase;
use ber_model::ber::structural::{ConstructionMakeup, Storey, WallConstructionType};
use domain::points::Point2f32;

/// One attribute-GROUP in the exploded "Edit attributes" session: the selected
/// surfaces that currently share a construction, plus the `(type, make-up)` the
/// popup edits. `phase` is the derived axis (floor zones), shown read-only.
#[derive(Debug, Clone)]
pub struct EditGroup {
    pub ids: Vec<ItemId>,
    pub ty: WallConstructionType,
    pub makeup: ConstructionMakeup,
    pub phase: ConstructionPhase,
}

/// The in-flight "Edit attributes" session: the current selection partitioned by
/// shared construction, plus which group's popup is active. Walls only for now
/// (construction has a parametric home only for walls).
#[derive(Debug, Clone)]
pub struct StructureEdit {
    pub kind: SurfaceKind,
    pub groups: Vec<EditGroup>,
    pub active: usize,
}

impl AppController {
    /// The Structure-phase selected region ids — the canvas shades exactly these.
    pub fn structure_selected_ids(&self) -> &HashSet<ItemId> {
        &self.structure_selected
    }

    /// Assign a wall CONSTRUCTION (type + make-up) to the given resolved WALL
    /// region ids, keyed by each wall's parametric `SurfaceRef` so it survives
    /// the re-derivation. The PHASE axis stays derived from the floor zones; the
    /// next materialize composes the two (`document::stamp_wall_constructions`).
    /// Snapshots for undo.
    pub fn set_wall_construction_for(
        &mut self,
        ids: &[ItemId],
        ty: ber_model::ber::structural::WallConstructionType,
        makeup: ber_model::ber::structural::ConstructionMakeup,
    ) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        // Map each resolved wall id → its parametric SurfaceRef (base midpoint).
        let refs: Vec<manip_plan::SurfaceRef> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            let Some(storey) = record.data.storeys.get(idx) else {
                return;
            };
            let plan = &self.document.storeys[idx].outline.floor_plan;
            ids.iter()
                .filter_map(|id| storey.walls.regions.iter().find(|r| r.id == *id))
                .filter_map(|r| {
                    let (e0, e1) = r.base_edge()?;
                    let mid =
                        Point2f32::new(((e0.x + e1.x) * 0.5) as f32, ((e0.y + e1.y) * 0.5) as f32);
                    plan.surface_ref_at(mid, 0.05)
                })
                .collect()
        };
        if refs.is_empty() {
            return;
        }
        self.record_history();
        for r in refs {
            self.document.storeys[idx]
                .wall_constructions
                .insert(r, (ty, makeup.clone()));
        }
        let _ = self.mark_dirty();
    }

    /// The in-flight "Edit attributes" session, if any.
    pub fn structure_edit(&self) -> Option<&StructureEdit> {
        self.structure_edit.as_ref()
    }

    /// Open the "Edit attributes" session: partition the current WALL selection
    /// by shared construction `(type, make-up, phase)` into groups (each editable
    /// separately). No-op for a non-wall mode, an empty selection, or no storey.
    pub fn begin_structure_edit(&mut self) {
        if self.structure_kind != SurfaceKind::Walls || self.structure_selected.is_empty() {
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
        let mut groups: Vec<EditGroup> = Vec::new();
        for id in &self.structure_selected {
            let Some(r) = storey.walls.regions.iter().find(|r| r.id == *id) else {
                continue;
            };
            let (ty, makeup, phase) = record
                .data
                .wall_construction(r.construction_id)
                .map(|c| {
                    (
                        c.construction_type,
                        c.base.construction_makeup.clone(),
                        c.base.construction_phase.clone(),
                    )
                })
                .unwrap_or_default();
            match groups
                .iter_mut()
                .find(|g| g.ty == ty && g.makeup == makeup && g.phase == phase)
            {
                Some(g) => g.ids.push(*id),
                None => groups.push(EditGroup {
                    ids: vec![*id],
                    ty,
                    makeup,
                    phase,
                }),
            }
        }
        if !groups.is_empty() {
            self.structure_edit = Some(StructureEdit {
                kind: SurfaceKind::Walls,
                groups,
                active: 0,
            });
        }
    }

    /// Switch which attribute-group's popup is active (one at a time).
    pub fn set_active_structure_group(&mut self, i: usize) {
        if let Some(e) = self.structure_edit.as_mut() {
            if i < e.groups.len() {
                e.active = i;
            }
        }
    }

    /// Set a group's construction `(type, make-up)` and apply it to that group's
    /// surfaces (writes the durable store, snapshots for undo).
    pub fn set_structure_group_attrs(
        &mut self,
        group: usize,
        ty: WallConstructionType,
        makeup: ConstructionMakeup,
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
            g.ids.clone()
        };
        self.set_wall_construction_for(&ids, ty, makeup);
    }

    /// Close the "Edit attributes" session (attributes apply as they're set).
    pub fn end_structure_edit(&mut self) {
        self.structure_edit = None;
    }

    /// Toggle the surface under a tapped point (world metres) in/out of the
    /// selection. The active [`StructureSelection::kind`] decides which regions
    /// are hit-tested; no-op when no Select subtool is active.
    pub fn toggle_structure_surface_at(&mut self, world_x: f64, world_y: f64, tol: f64) {
        if self.structure_select.is_none() {
            return;
        }
        let kind = self.structure_kind;
        let hit = self
            .current_storey()
            .and_then(|s| s.surface_at(kind, world_x, world_y, tol));
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
        self.pending_structure_cut = None;
        self.structure_split_kind = None;
    }

    /// A Structure-phase CARVE/CUT stroke (world coords): an approximate stroke
    /// that the assessor then DIMENSIONS, which SPLITS the surface, after which
    /// the chosen piece auto-selects. Carve the storey the SAME way a phase area
    /// is drawn. A straight cut splits it into two candidate slots to pick from
    /// (see [`structure_cut_regions`]); any other stroke becomes a dimensionable
    /// sub-area straight away. The split itself happens at commit, once the
    /// drawn walls are dimensioned (see [`commit_structure_split`]).
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
                let undo = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                self.begin_structure_split_dimensioning(slot, self.structure_kind, undo);
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
            let undo = self.current_history_snapshot();
            self.begin_structure_split_dimensioning(slot, self.structure_kind, undo);
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
        self.maybe_commit_structure_split();
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

    /// Commit the split if its drawn walls are fully dimensioned — a cheap
    /// no-op unless a structure split is in-flight and ready. Called at the end
    /// of every dimension/position apply so a split that finishes on either kind
    /// of prompt commits.
    pub fn maybe_commit_structure_split(&mut self) {
        if self.structure_split_dimensioned() {
            self.commit_structure_split();
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

    /// Commit the dimensioned split: push its ring (walls: the fixed interior
    /// cut segment) onto the storey's durable split field, re-resolve so the
    /// split replays, re-home any wall openings across a wall split, then
    /// auto-select the chosen piece's surfaces. Mirrors the construction-phase
    /// commit but writes the BER's durable split fields (replayed by
    /// `resolve_storey`), not session-only regions.
    fn commit_structure_split(&mut self) {
        let Some(kind) = self.structure_split_kind else {
            return;
        };
        // A split needs an active storey (never the read-only All-storeys tab —
        // its `current_storey_idx` is `None` and `refresh_storey` would no-op).
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let ring32 = self.document.floor_plan.get_points();
        if ring32.len() < 3 {
            return;
        }
        let ring64: Vec<Point2f64> = ring32.iter().map(|p| domain::points::to_f64(*p)).collect();

        // Walls store the fixed cut SEGMENT(s) — every interior edge of the
        // carved region, each crossing at most one wall; floors/roofs store the
        // whole carved ring. A wall carve touching no wall (all edges on the
        // outline) yields no cut — warn and bail rather than shade nothing split.
        let wall_cuts = if kind == SurfaceKind::Walls {
            let edges = self.document.outline.floor_plan.interior_cut_edges(&ring32);
            if edges.is_empty() {
                log::warn!("structure wall carve produced no interior cut edge — no split");
                self.abandon_structure_split_shape();
                return;
            }
            edges
        } else {
            Vec::new()
        };

        // Push onto the storey's durable split field; `resolve_storey` replays it
        // (and, for walls, re-homes the crossed walls' openings onto the
        // sub-walls before the orphan-prune).
        let Some(storey) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx))
        else {
            return;
        };
        storey.push_structure_split(kind, &ring64, wall_cuts);

        self.abandon_structure_split_shape();
        let _ = self.mark_dirty();
        self.materialize(); // re-resolve applies the split + re-homes openings

        // Auto-select the chosen piece's surfaces of the split kind.
        self.select_surfaces_in_region(&ring64);
    }

    /// Add every surface of the active kind inside `region_ring` (world coords)
    /// to the selection — the carve-pick query, delegated to the storey.
    fn select_surfaces_in_region(&mut self, region_ring: &[Point2f64]) {
        if self.structure_select.is_none() {
            return;
        }
        let kind = self.structure_kind;
        let hits = match self.current_storey() {
            Some(storey) => storey.surfaces_in_ring(kind, region_ring),
            None => return,
        };
        self.structure_selected.extend(hits);
    }

    /// Replace the selection with every surface id of `kind` (drives `Select
    /// all` / — as a placeholder until the criteria land — `Select remaining`).
    pub(super) fn select_all_structure_surfaces(&mut self, kind: SurfaceKind) {
        let ids = self
            .current_storey()
            .map(|s| s.surface_ids(kind))
            .unwrap_or_default();
        self.structure_selected = ids.into_iter().collect();
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
