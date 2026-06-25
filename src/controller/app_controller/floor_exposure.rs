//! Stage-1 inter-storey (party) auto-exposure pass: across the whole storey
//! stack, mark party WALLS shared with the storey above/below, and auto-split
//! PARTIAL floor/ceiling overlaps so the shared sub-region exists with its own
//! centroid and resolves NonHeatLoss (party floor) / Party (party ceiling).
//!
//! This is the deliberate ONE-SHOT orchestration of the `document` party
//! helpers (the controller sequences: snapshot → carve → re-resolve → stamp →
//! mark dirty), distinct from the per-frame full-containment auto-derive
//! ([`derive_horizontal_exposure_all`]) which only handles fully-stacked
//! surfaces. The model mutation itself lives in `document`; this file owns the
//! shell concerns (undo snapshot, re-resolve sequencing, dirty marking).
use super::*;
use crate::controller::app_controller::SelectMode;
use ber_model::ber::structural::FloorExposure;
use domain::points::Point2f64;

impl AppController {
    /// Run the inter-storey party auto-exposure pass over the WHOLE stack:
    /// mark party walls (shared with the storey above/below) and auto-split
    /// partial floor/ceiling overlaps. Each carved party piece (`r ∩ neighbour`,
    /// hence fully contained in the neighbour footprint) is marked
    /// NonHeatLoss / Party by the per-frame full-containment auto-derive
    /// ([`auto_derive_horizontal_exposure`], run from `commit_to_ber` →
    /// `derive_horizontal_exposure_all`) — so no explicit per-piece stamp is
    /// needed here. Respects manual precedence (`or_insert`). One undoable action;
    /// undo reverts EVERY storey's record (full snapshot). No-op without a record.
    pub fn auto_expose_all_storeys(&mut self) {
        // Bring the resolved record current so the planners read live footprints.
        self.materialize();
        if self.current_record.is_none() {
            return;
        }
        // FULL snapshot — this pass pushes carves onto EVERY storey's
        // `floor_splits`/`roof_splits`, so undo must revert the whole stack, not
        // just the active storey's record.
        let snap = self.current_history_snapshot_full();
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0)
            .min(self.document.storeys.len());

        for idx in 0..n {
            // Neighbour footprints from the RESOLVED record (cloned to release the
            // immutable borrow before the mutable doc / record writes below).
            let (below_fp, above_fp) = {
                let storeys = &self.current_record.as_ref().unwrap().data.storeys;
                let below = idx
                    .checked_sub(1)
                    .and_then(|b| storeys.get(b))
                    .and_then(|s| s.floors.footprint_ring());
                let above = storeys.get(idx + 1).and_then(|s| s.floors.footprint_ring());
                (below, above)
            };

            // Walls: mark the still-Unset party sub-ranges shared with either
            // neighbour. (Pure model mutation in the StoreyDoc.)
            self.document.storeys[idx]
                .auto_mark_party_walls(below_fp.as_deref(), above_fp.as_deref());

            // Floors + ceilings: plan the partial-overlap carves against the
            // neighbour footprints, reading the resolved storey. The sample
            // points the planners return are ignored — the per-frame
            // full-containment derive marks the carved pieces.
            let storey = self.current_record.as_ref().unwrap().data.storeys[idx].clone();
            let mut floor_rings: Vec<Vec<Point2f64>> = Vec::new();
            let mut roof_rings: Vec<Vec<Point2f64>> = Vec::new();
            if let Some(below) = &below_fp {
                let (rings, _) = self.document.storeys[idx].plan_party_floor_carves(&storey, below);
                floor_rings = rings;
            }
            if let Some(above) = &above_fp {
                let (rings, _) =
                    self.document.storeys[idx].plan_party_ceiling_carves(&storey, above);
                roof_rings = rings;
            }

            // De-dup against the durable splits already present (re-entering the
            // tool and confirming again replans the same geometry — idempotent —
            // but `extend` would grow the serialized Vecs unboundedly). Skip any
            // planned ring whose quantized centroid already keys an existing split.
            let record = self.current_record.as_mut().unwrap();
            Self::extend_splits_deduped(&mut record.data.storeys[idx].floor_splits, floor_rings);
            let has_roof_split = {
                let before = record.data.storeys[idx].roof_splits.len();
                Self::extend_splits_deduped(&mut record.data.storeys[idx].roof_splits, roof_rings);
                record.data.storeys[idx].roof_splits.len() > before
            };
            // Roofs are regions-as-truth — `resolve_storey` only re-fits them, so
            // the `roof_splits` carve must be applied via `recompose_roofs` (it
            // reads the durable field). Floors: `resolve_storey` applies
            // `floor_splits` every resolve, so no extra call there.
            if has_roof_split {
                self.recompose_roofs(idx);
            }
        }

        // Re-resolve so the carved sub-regions exist with their own centroids;
        // the per-frame full-containment auto-derive (inside the commit) marks
        // each contained party piece NonHeatLoss / Party.
        let _ = self.commit_to_ber();
        self.push_history_snapshot(snap);
    }

    /// Append each planned carve `ring` to `splits` UNLESS a split with the same
    /// quantized centroid key already exists — the de-dup that stops re-runs of
    /// the (idempotent) auto-pass from growing the serialized split Vecs.
    fn extend_splits_deduped(splits: &mut Vec<Vec<Point2f64>>, rings: Vec<Vec<Point2f64>>) {
        let mut seen: HashSet<document::CentroidKey> = splits
            .iter()
            .map(|r| document::ring_centroid_key(r))
            .collect();
        for ring in rings {
            if seen.insert(document::ring_centroid_key(&ring)) {
                splits.push(ring);
            }
        }
    }

    // ── Floor-exposure TOOL (Stages 2-4) ───────────────────────────────────
    //
    // The tool drives the manual half of floor-exposure marking, layered on top
    // of the Stage-1 auto pass: a storey-count gate, an all-storeys overlaid
    // floor view, and selection (set-all / click / carve / remaining) + apply.
    // Selection spans the whole stack — keyed by `(storey_idx, centroid key)`
    // (NOT region `ItemId`, which is re-minted every resolve / `materialize`).

    /// True when the floor-exposure tool is active (routes the canvas to the
    /// overlaid all-storeys floor view + selection, gates the normal plan off).
    pub fn floor_exposure_mode(&self) -> bool {
        self.floor_exposure_mode
    }

    /// True while the storey-count gate prompt is up (the tool is on but the
    /// auto-pass hasn't run yet). The overlaid view + selection wait for confirm.
    pub fn floor_exposure_gate_pending(&self) -> bool {
        self.floor_exposure_gate_pending
    }

    /// The active floor-exposure Select subtool, if any.
    pub fn floor_exposure_select(&self) -> Option<SelectMode> {
        self.floor_exposure_select
    }

    /// Enter or leave the floor-exposure tool. Mutually exclusive with the other
    /// Mapping modes (mirrors [`set_exposure_mode`](Self::set_exposure_mode)'s
    /// clearing). On enter the storey-count GATE is raised — the auto-pass only
    /// runs once the assessor confirms every storey is added. All selection
    /// state is cleared either way.
    pub fn set_floor_exposure_mode(&mut self, on: bool) {
        // Drop any half-dimensioned carve buffer before the tool toggles.
        if self.floor_exposure_carve_in_flight {
            self.abandon_floor_exposure_carve_shape();
        }
        self.floor_exposure_mode = on;
        self.floor_exposure_select = None;
        self.floor_exposure_selected.clear();
        if on {
            self.floor_exposure_gate_pending = true;
            // Leave every other Mapping mode (each setter clears its own
            // pick/drag state) so the canvas isn't shared.
            self.set_exposure_mode(false);
            self.set_openings_mode(false);
            self.set_living_area_mode(false);
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.roof_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
        } else {
            self.floor_exposure_gate_pending = false;
        }
    }

    /// Gate answer YES — every storey is added. Lower the gate and run the
    /// one-shot inter-storey party auto-exposure pass (Stage 1).
    pub fn confirm_all_storeys_added(&mut self) {
        self.floor_exposure_gate_pending = false;
        self.auto_expose_all_storeys();
    }

    /// Gate answer NOT-YET — exit the tool so the assessor can add the
    /// remaining storeys first.
    pub fn defer_storey_count(&mut self) {
        self.set_floor_exposure_mode(false);
    }

    /// Set (or clear) the floor-exposure Select subtool. `All`/`Remaining` seed
    /// the selection across the whole stack; `Click`/`Carve` keep the current
    /// set (refined by taps / the carved region). No-op while the gate is up.
    pub fn set_floor_exposure_select_mode(&mut self, mode: Option<SelectMode>) {
        if self.floor_exposure_gate_pending {
            return;
        }
        // Switching away from Carve drops any in-flight (half-dimensioned) carve.
        if self.floor_exposure_carve_in_flight && mode != Some(SelectMode::Carve) {
            self.abandon_floor_exposure_carve_shape();
        }
        self.floor_exposure_select = mode;
        if matches!(mode, Some(SelectMode::All | SelectMode::Remaining)) {
            self.reseed_floor_exposure_selection();
        }
    }

    /// The selected floor regions, keyed `(storey_idx, centroid key)` — the
    /// overlaid view halos exactly these. Keyed by centroid (not `ItemId`)
    /// because floor-region ids are re-minted on every resolve.
    pub fn floor_exposure_selected_ids(&self) -> &HashSet<(usize, document::CentroidKey)> {
        &self.floor_exposure_selected
    }

    /// Clear every selected floor region ("Deselect all"). Keeps the active
    /// Select subtool so the assessor can keep picking.
    pub fn clear_floor_exposure_selection(&mut self) {
        self.floor_exposure_selected.clear();
    }

    /// Toggle the floor region under a tapped point (world metres) in/out of the
    /// selection — hit-tested across EVERY storey. When several storeys' floors
    /// overlap the point, the SMALLEST-area region wins (the most specific
    /// sub-region — e.g. a carved party patch over the full footprint below it),
    /// so a tap lands on the piece the assessor most likely means. No-op while
    /// the gate is up or no Click subtool is active.
    pub fn toggle_floor_exposure_at(&mut self, world_x: f64, world_y: f64, tol: f64) {
        if self.floor_exposure_gate_pending
            || !matches!(self.floor_exposure_select, Some(SelectMode::Click))
        {
            return;
        }
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        // Smallest-area floor region under the point across the whole stack;
        // store its centroid key (stable across re-resolves, unlike the id).
        let mut best: Option<(f64, usize, document::CentroidKey)> = None;
        for (idx, storey) in record.data.storeys.iter().enumerate() {
            if let Some(id) = storey.surface_at(SurfaceKind::Floors, world_x, world_y, tol) {
                if let Some(region) = storey
                    .floors
                    .regions
                    .iter()
                    // Party floors aren't shown, so a tap can't select one.
                    .find(|r| r.id == id && r.exposure != FloorExposure::NonHeatLoss)
                {
                    let area = region.area();
                    if best.as_ref().is_none_or(|(a, _, _)| area < *a) {
                        best = Some((area, idx, document::centroid_key(region.centroid())));
                    }
                }
            }
        }
        if let Some((_, idx, key)) = best {
            let sel = (idx, key);
            if !self.floor_exposure_selected.insert(sel) {
                self.floor_exposure_selected.remove(&sel);
            }
        }
    }

    /// A floor-exposure CARVE stroke (world coords): carve a region against a
    /// reference storey's outline (the same `carve_or_cut_in_available` path the
    /// other carves use), then DIMENSION it precisely — mirroring the
    /// Structure-phase carve — before it SPLITS the floor. Drawing the region
    /// puts it into the live dimensioning buffer as a `PhaseArea` shape, so the
    /// standard floating number-box prompts drive it; once fully dimensioned the
    /// ring is staged + committed ([`stage_floor_exposure_carve`]), splitting the
    /// floor on every overlapping storey and selecting the resulting piece(s).
    ///
    /// All-tab buffer caveat: the dimensioning buffer is the document's active
    /// storey's, but on the All tab the live buffer is empty by design. So a
    /// REFERENCE storey is chosen (the active storey if its outline is non-empty,
    /// else the largest footprint / storey 0) and loaded into the buffer first;
    /// the carve + dimensioning run against THAT. The pinned ring ends up in
    /// plan/world coords, which is what every storey's split is applied in.
    ///
    /// A straight cut (two halves) isn't supported here — this tool requires a
    /// closed carve region — so a `Cut` outcome is ignored.
    pub fn apply_floor_exposure_carve_stroke(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if self.floor_exposure_gate_pending
            || !matches!(self.floor_exposure_select, Some(SelectMode::Carve))
        {
            return;
        }
        // Already dimensioning a carve — ignore a second stroke until it's done.
        if self.floor_exposure_carve_in_flight {
            return;
        }
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Host the in-flight carve's dimensioning on a reference storey (the
        // PhaseArea buffer lives on the active storey).
        let Some(ref_idx) = self.floor_exposure_reference_storey() else {
            return;
        };
        self.document.set_active_storey(ref_idx);
        self.document.active = ActiveShape::Outline;
        self.load_active_into_buffer();
        // Clip the carve to the WHOLE BUILDING footprint (union of every storey's
        // floor), NOT just the active/reference storey's outline — otherwise a
        // carve drawn over area belonging to another storey gets clipped away
        // (e.g. you opened the tool from the smaller upper storey). Each storey's
        // floor is still clipped individually when the split is applied
        // (`with_splits`), so the union is just the outer bound. Falls back to the
        // reference storey's outline if no union can be built.
        let boundary = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.floors_footprint_union())
            .map(|ring| {
                ring.iter()
                    .map(|p| domain::points::to_f32(*p))
                    .collect::<Vec<_>>()
            })
            .and_then(|ring| manip_plan::FloorPlan::from_clean_ring(&ring).ok())
            .unwrap_or_else(|| self.document.outline.floor_plan.clone());
        match boundary.carve_or_cut(&pts, &self.tuning.subarea, false) {
            Ok(SubareaOutcome::Area(plan)) => {
                if plan.validate_simple().is_err() {
                    return;
                }
                let undo = self.current_history_snapshot_full();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                self.begin_floor_exposure_carve_dimensioning(slot, ref_idx, undo);
            }
            // A straight cut produces two halves, not an enclosed carve region —
            // this tool requires a closed region, so ignore it.
            Ok(SubareaOutcome::Cut(..)) => {}
            Err(e) => log::error!("floor exposure carve_or_cut: {}", e),
        }
    }

    /// The reference storey whose outline a floor-exposure carve is dimensioned
    /// against: the active storey if it has a non-empty outline, else the storey
    /// with the largest footprint (falling back to storey 0). `None` when there
    /// are no storeys with an outline at all.
    fn floor_exposure_reference_storey(&self) -> Option<usize> {
        let outline_len = |i: usize| {
            self.document
                .storeys
                .get(i)
                .map(|s| s.outline.floor_plan.get_points().len())
                .unwrap_or(0)
        };
        // Prefer the active storey when it has a drawn outline.
        if let Some(idx) = self.current_storey_idx {
            if outline_len(idx) >= 3 {
                return Some(idx);
            }
        }
        // Else the largest-footprint storey (by resolved floor footprint area),
        // falling back to the first storey that has an outline at all.
        let record = self.current_record.as_ref()?;
        let mut best: Option<(f64, usize)> = None;
        for (idx, storey) in record.data.storeys.iter().enumerate() {
            if outline_len(idx) < 3 {
                continue;
            }
            let area = storey.floors.regions.iter().map(|r| r.area()).sum::<f64>();
            if best.as_ref().is_none_or(|(a, _)| area > *a) {
                best = Some((area, idx));
            }
        }
        best.map(|(_, idx)| idx)
    }

    /// Make the carved region the in-flight `PhaseArea` split shape and start
    /// dimensioning its drawn walls (mirrors `begin_structure_split_dimensioning`).
    /// A carve that needs no dimensioning commits immediately via the staging
    /// hook. The full snapshot is held for undo (the commit touches every
    /// overlapping storey's `floor_splits`).
    fn begin_floor_exposure_carve_dimensioning(
        &mut self,
        slot: ShapeSlot,
        ref_idx: usize,
        undo: HistorySnapshot,
    ) {
        self.document.begin_phase_area(slot);
        self.floor_exposure_carve_in_flight = true;
        self.floor_exposure_carve_ref = Some(ref_idx);
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo);
        self.maybe_stage_floor_exposure_carve();
    }

    /// The in-flight floor-exposure carve ring (world coords) for the overlaid
    /// view to highlight — the LIVE dimensioned buffer, so it tracks dimensioning.
    /// Empty when no carve is in-flight.
    pub fn pending_floor_exposure_carve_polygon(&self) -> Vec<(f64, f64)> {
        if self.floor_exposure_carve_in_flight && self.document.active == ActiveShape::PhaseArea {
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

    /// True once the in-flight floor-exposure carve is fully DIMENSIONED (wall
    /// lengths AND position) and ready to commit.
    fn floor_exposure_carve_dimensioned(&self) -> bool {
        self.floor_exposure_carve_in_flight
            && self.document.active == ActiveShape::PhaseArea
            && self.document.dimensioning.current_prompt.is_none()
            && self.document.dimensioning.current_position_prompt.is_none()
            && self.document.floor_plan.get_points().len() >= 3
    }

    /// STAGE + COMMIT the floor-exposure carve if its drawn walls are fully
    /// dimensioned — a cheap no-op unless a carve is in-flight and ready. Called
    /// at the end of every dimension/position apply (the shared hook with the
    /// Structure carve), so a carve that finishes on either kind of prompt commits.
    pub fn maybe_stage_floor_exposure_carve(&mut self) {
        if self.floor_exposure_carve_dimensioned() {
            self.stage_floor_exposure_carve();
        }
    }

    /// Discard the in-flight carve shape and return to the outline, dropping its
    /// half-dimensioned buffer (mirrors `abandon_structure_split_shape`). The
    /// carve borrowed the reference storey as the active editing target; restore
    /// the active storey to whatever the tab selects (so a later storey switch
    /// stays consistent).
    fn abandon_floor_exposure_carve_shape(&mut self) {
        self.document.phase_shape = None;
        self.floor_exposure_carve_in_flight = false;
        self.floor_exposure_carve_ref = None;
        if self.document.active == ActiveShape::PhaseArea {
            self.document.active = ActiveShape::Outline;
            self.load_active_into_buffer();
            self.document.pending_arc = None;
            self.edit_drag_prompt = None;
            self.vertex_drop_prompt = None;
            self.document.edit_state = EditState::default();
            self.compute_next_dimension_prompt();
        }
        // Re-point the document's active storey at the selected tab (the All tab
        // leaves whatever was active — the overlaid view reads `current_record`,
        // not the buffer — but an active tab must stay in sync).
        if let Some(idx) = self.current_storey_idx {
            self.document.set_active_storey(idx);
            self.load_active_into_buffer();
        }
    }

    /// Capture the dimensioned carve ring and SPLIT the floor on every storey
    /// whose footprint the ring overlaps, then SELECT the resulting carved
    /// piece(s) across the stack — the Structure-phase carve flow specialised for
    /// floor exposure (split → select, no "edit attributes" step). The undo
    /// snapshot was already pushed when the carve began (full snapshot — this
    /// touches multiple storeys' `floor_splits`), so the whole commit is one
    /// undoable action. Cleans up the in-flight shape either way.
    fn stage_floor_exposure_carve(&mut self) {
        if !self.floor_exposure_carve_in_flight {
            return;
        }
        let ring32 = self.document.floor_plan.get_points();
        if ring32.len() < 3 {
            self.abandon_floor_exposure_carve_shape();
            return;
        }
        let ring: Vec<Point2f64> = ring32.iter().map(|p| domain::points::to_f64(*p)).collect();
        // Drop the in-flight buffer FIRST (restores the outline + clears the
        // flags), so the split + re-resolve below run on a clean editing model.
        self.abandon_floor_exposure_carve_shape();
        self.commit_floor_exposure_carve(&ring);
    }

    /// Push the carve `ring` onto the durable `floor_splits` of every storey whose
    /// FLOOR footprint it overlaps, re-resolve the whole stack, then add the
    /// resulting carved floor piece(s) to the selection. The split ring is clipped
    /// to each floor by `FloorRegions::with_splits` (boolean intersection), so a
    /// ring that pokes past a setback storey splits only the overlapping part and
    /// a non-overlapping storey is left untouched. No undo snapshot here — the
    /// caller pushed the full snapshot when the carve began.
    fn commit_floor_exposure_carve(&mut self, ring: &[Point2f64]) {
        // Which storeys' floor footprints does the carve overlap? (Overlap = the
        // ring splits at least one floor region into a piece inside it.)
        let overlapped: Vec<usize> = {
            let Some(record) = self.current_record.as_ref() else {
                return;
            };
            record
                .data
                .storeys
                .iter()
                .enumerate()
                // Only split storeys with a LABELLABLE (non-party) floor under the
                // ring — party (NonHeatLoss) floors are auto-classified and hidden,
                // so there's nothing to subdivide there.
                .filter(|(_, s)| {
                    let ids = s.surfaces_in_ring(SurfaceKind::Floors, ring);
                    s.floors
                        .regions
                        .iter()
                        .any(|r| ids.contains(&r.id) && r.exposure != FloorExposure::NonHeatLoss)
                })
                .map(|(idx, _)| idx)
                .collect()
        };
        if overlapped.is_empty() {
            return;
        }
        for idx in &overlapped {
            if let Some(storey) = self
                .current_record
                .as_mut()
                .and_then(|r| r.data.storeys.get_mut(*idx))
            {
                storey.push_structure_split(SurfaceKind::Floors, ring, Vec::new());
            }
        }
        let _ = self.mark_dirty();
        // Re-resolve EVERY storey so each overlapping storey's `floor_splits`
        // apply (the active-storey-only `materialize` would miss non-active
        // storeys); keep the derived revision in step so a later no-edit
        // materialize is a no-op.
        self.refresh_all_storeys();
        self.derived_revision = self.revision;
        // Select the carved piece(s): the floor region(s) now sitting inside the
        // ring on each overlapping storey.
        self.select_floors_in_region(ring);
    }

    /// Add every floor region inside `region_ring` (world coords) — across ALL
    /// storeys — to the selection.
    fn select_floors_in_region(&mut self, region_ring: &[Point2f64]) {
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let hits: Vec<(usize, document::CentroidKey)> = record
            .data
            .storeys
            .iter()
            .enumerate()
            .flat_map(|(idx, s)| {
                let ids = s.surfaces_in_ring(SurfaceKind::Floors, region_ring);
                s.floors
                    .regions
                    .iter()
                    // Only the labellable (non-party) floors are shown/selectable.
                    .filter(move |r| {
                        ids.contains(&r.id) && r.exposure != FloorExposure::NonHeatLoss
                    })
                    .map(move |r| (idx, document::centroid_key(r.centroid())))
            })
            .collect();
        self.floor_exposure_selected.extend(hits);
    }

    /// Re-seed the selection for an active `All`/`Remaining` subtool across the
    /// whole stack: `All` takes every floor region; `Remaining` only the ones
    /// still `Unset`. No-op when no seed tool is active.
    fn reseed_floor_exposure_selection(&mut self) {
        let Some(mode) = self.floor_exposure_select else {
            return;
        };
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let mut keys: HashSet<(usize, document::CentroidKey)> = HashSet::new();
        for (idx, storey) in record.data.storeys.iter().enumerate() {
            for region in &storey.floors.regions {
                // Party (NonHeatLoss) floors aren't shown/labellable, so neither
                // seed tool touches them. `All` = every labellable floor;
                // `Remaining` = the still-`Unset` ones.
                let take = match mode {
                    SelectMode::All => region.exposure != FloorExposure::NonHeatLoss,
                    SelectMode::Remaining => region.exposure == FloorExposure::Unset,
                    _ => false,
                };
                if take {
                    keys.insert((idx, document::centroid_key(region.centroid())));
                }
            }
        }
        self.floor_exposure_selected = keys;
    }

    /// Apply `exposure` to every selected floor region across the stack: snapshot
    /// for undo, write each region's durable per-storey override
    /// ([`StoreyDoc::floor_exposure`], keyed by centroid), then re-derive +
    /// persist. Parallel to `set_floor_construction_for`. No-op on empty
    /// selection / no record.
    pub fn set_floor_exposure_for(&mut self, exposure: FloorExposure) {
        if self.floor_exposure_selected.is_empty() {
            return;
        }
        let snap = self.current_history_snapshot();
        // The selection IS keyed by the centroid key — the same key
        // `StoreyDoc::floor_exposure` uses — so write it directly, no region
        // lookup. (Cloned out first so the &mut doc write doesn't fight the
        // immutable selection borrow.)
        let keys: Vec<(usize, document::CentroidKey)> =
            self.floor_exposure_selected.iter().copied().collect();
        for (idx, key) in keys {
            if let Some(sd) = self.document.storeys.get_mut(idx) {
                sd.floor_exposure.insert(key, exposure);
            }
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(snap);
    }

    /// The overlaid all-storeys floor view data for the canvas + legend: per
    /// storey, its floor regions as `(id, ring world coords, exposure)`. The
    /// canvas fits ONE transform to the union of every storey's floors and draws
    /// each storey in its own palette shade with the exposure as a coloured
    /// outline; the selected set ([`floor_exposure_selected_ids`]) is haloed.
    pub fn floor_exposure_view(&self) -> Vec<FloorExposureStoreyView> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        record
            .data
            .storeys
            .iter()
            .enumerate()
            .map(|(idx, storey)| FloorExposureStoreyView {
                idx,
                regions: storey
                    .floors
                    .regions
                    .iter()
                    // The view is a labelling worklist: party (NonHeatLoss) floors
                    // are auto-classified and need no label, so they drop off.
                    .filter(|r| r.exposure != FloorExposure::NonHeatLoss)
                    .map(|r| {
                        let ring: Vec<(f64, f64)> = {
                            let mut pts: Vec<(f64, f64)> =
                                r.polygon.exterior().coords().map(|c| (c.x, c.y)).collect();
                            if pts.len() >= 2 && pts[0] == pts[pts.len() - 1] {
                                pts.pop();
                            }
                            pts
                        };
                        FloorExposureRegionView {
                            id: r.id,
                            ring,
                            exposure: r.exposure,
                            selected: self
                                .floor_exposure_selected
                                .contains(&(idx, document::centroid_key(r.centroid()))),
                        }
                    })
                    .collect(),
            })
            .collect()
    }
}

/// One storey's floors for the overlaid floor-exposure view.
pub struct FloorExposureStoreyView {
    pub idx: usize,
    pub regions: Vec<FloorExposureRegionView>,
}

/// One floor region in the overlaid view: stable id, plan ring (world coords,
/// no closing duplicate), its exposure, and whether it's currently selected.
pub struct FloorExposureRegionView {
    pub id: ItemId,
    pub ring: Vec<(f64, f64)>,
    pub exposure: FloorExposure,
    pub selected: bool,
}
