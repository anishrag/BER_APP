//! The FLOOR-exposure TOOL: a storey-count gate, an all-storeys overlaid
//! floor view, and selection (set-all / click / carve / remaining) + apply.
//! The inter-storey classification itself is no longer a tool concern: the
//! `derive_inter_storey` pipeline tail (see `derive_horizontal_exposure_all`)
//! stamps every storey-junction floor `InterStorey` on every re-resolve, and
//! the tool simply HIDES those derived pieces — everything else (party
//! `NonHeatLoss` floors included) is assessor-paintable here.
use super::*;
use crate::controller::app_controller::SelectMode;
use ber_model::ber::structural::FloorExposure;
use domain::points::Point2f64;

impl AppController {
    // ── Floor-exposure TOOL ────────────────────────────────────────────────
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
    /// assessor hasn't confirmed the stack is complete). The overlaid view +
    /// selection wait for confirm.
    pub fn floor_exposure_gate_pending(&self) -> bool {
        self.floor_exposure_gate_pending
    }

    /// The active floor-exposure Select subtool, if any.
    pub fn floor_exposure_select(&self) -> Option<SelectMode> {
        self.floor_exposure_select
    }

    /// Enter or leave the floor-exposure tool. Mutually exclusive with the other
    /// Mapping modes (mirrors [`set_exposure_mode`](Self::set_exposure_mode)'s
    /// clearing). On enter the storey-count GATE is raised — the overlaid view
    /// only opens once the assessor confirms every storey is added (the
    /// InterStorey derivation reads the whole stack, so labelling before it's
    /// complete would waste work). All selection state is cleared either way.
    pub fn set_floor_exposure_mode(&mut self, on: bool) {
        // Drop any half-dimensioned carve buffer before the tool toggles.
        if self.floor_exposure_carve_in_flight {
            self.abandon_floor_exposure_carve_shape();
        }
        self.floor_exposure_mode = on;
        self.floor_exposure_select = None;
        self.floor_exposure_selected.clear();
        self.floor_exposure_pending_cut = None;
        if on {
            // A PLAN task (all-storeys floor takeover, 2D-only input) — see
            // `set_roof_mode`: entering from 3D would show it with dead input.
            self.set_sketch_mode(SketchMode::Storey2D);
            self.floor_exposure_gate_pending = true;
            // Leave every other Mapping mode (each setter clears its own
            // pick/drag state) so the canvas isn't shared.
            self.set_ceiling_exposure_mode(false);
            self.set_exposure_mode(false);
            self.set_openings_mode(false);
            self.set_living_area_mode(false);
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.ceiling_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
        } else {
            self.floor_exposure_gate_pending = false;
        }
    }

    /// Gate answer YES — every storey is added. Lower the gate and bring the
    /// resolved record current (the pipeline's `derive_inter_storey` tail has
    /// then classified every storey-junction surface, so the view opens on the
    /// final worklist). No one-shot pass any more — the derivation runs on
    /// every re-resolve.
    pub fn confirm_all_storeys_added(&mut self) {
        self.floor_exposure_gate_pending = false;
        self.materialize();
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
        // Switching away from Carve drops any in-flight (half-dimensioned) carve
        // and any cut awaiting its side pick.
        if mode != Some(SelectMode::Carve) {
            if self.floor_exposure_carve_in_flight {
                self.abandon_floor_exposure_carve_shape();
            }
            self.floor_exposure_pending_cut = None;
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
                    // Derived InterStorey floors aren't shown, so a tap can't
                    // select one.
                    .find(|r| r.id == id && r.exposure != FloorExposure::InterStorey)
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
        // Snap-tag capture: the pre-action state, taken BEFORE the document
        // mutation below (set_active_storey / load_active_into_buffer) so a
        // replay restores the caller's original state. FULL — the eventual
        // commit touches every overlapping storey's splits. A pending CUT
        // pushes no undo entry, so its capture carries this `before` itself.
        // Also drop stale decisions from unrelated ops so only THIS stroke's
        // snaps are tagged.
        let before = self.current_history_snapshot_full();
        self.begin_snap_capture();
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
            .floor_exposure_whole_building_boundary()
            .unwrap_or_else(|| self.document.outline.floor_plan.clone());
        match boundary.carve_or_cut(&pts, &self.tuning.subarea, false) {
            Ok(SubareaOutcome::Area(plan)) => {
                if plan.validate_simple().is_err() {
                    // Nothing committed — drop the rejected carve's decisions.
                    let _ = manip_plan::take_snap_decisions();
                    return;
                }
                let undo = self.current_history_snapshot_full();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                // The carve dimensions against the WHOLE building, so the
                // "stay inside the storey" guardrail must check the union too,
                // not the (possibly smaller) reference storey's outline.
                self.document.subarea_containment_override = Some(boundary);
                self.begin_floor_exposure_carve_dimensioning(slot, ref_idx, undo);
                self.capture_snap_action(SnapAction::Subarea {
                    flow: SubareaFlow::FloorExposure,
                    points: world_points.to_vec(),
                    pick: None,
                });
            }
            // A straight edge-to-edge cut splits the whole building into two
            // candidate halves; pin both against the SAME whole-building boundary
            // (so a later pick can dimension either) and stash the pending cut.
            // The side pick (`choose_floor_exposure_cut_region`) sets the
            // containment override + begins dimensioning, mirroring the Area path.
            // The active storey stays `ref_idx` (set above) so the pick can host it.
            Ok(SubareaOutcome::Cut(a, b)) => {
                self.floor_exposure_pending_cut = Some([
                    self.pinned_subarea_slot(&boundary, a),
                    self.pinned_subarea_slot(&boundary, b),
                ]);
                self.capture_snap_action_pending(
                    SnapAction::Subarea {
                        flow: SubareaFlow::FloorExposure,
                        points: world_points.to_vec(),
                        pick: None,
                    },
                    before,
                    manip_plan::take_snap_decisions(),
                );
            }
            Err(e) => {
                log::error!("floor exposure carve_or_cut: {}", e);
                let _ = manip_plan::take_snap_decisions();
            }
        }
    }

    /// The two candidate halves of a pending floor-exposure cut (world coords),
    /// for the overlaid view to shade while the assessor picks a side. Empty when
    /// no cut is pending. Mirror of
    /// [`structure_cut_regions`](Self::structure_cut_regions).
    pub fn floor_exposure_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.floor_exposure_pending_cut
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

    /// True while a floor-exposure cut is awaiting the assessor's side pick.
    pub fn floor_exposure_cut_pending(&self) -> bool {
        self.floor_exposure_pending_cut.is_some()
    }

    /// Pick which candidate half a floor-exposure cut should dimension + split:
    /// take the pending cut, choose the slot whose plan holds the tap (a tap
    /// outside both cancels — clears the pending cut), set the whole-building
    /// containment override (as the Area carve does), then begin dimensioning the
    /// chosen half. From there it's the SAME dimension → stage → commit path as
    /// the Area carve. The active storey is already `ref_idx` (set when the cut
    /// stroke was applied), so the carve hosts there.
    pub fn choose_floor_exposure_cut_region(&mut self, world_x: f32, world_y: f32) {
        if self.floor_exposure_gate_pending
            || !matches!(self.floor_exposure_select, Some(SelectMode::Carve))
        {
            return;
        }
        let Some([a, b]) = self.floor_exposure_pending_cut.take() else {
            return;
        };
        let chosen = if a.floor_plan.contains_point(world_x, world_y) {
            Some(a)
        } else if b.floor_plan.contains_point(world_x, world_y) {
            Some(b)
        } else {
            None
        };
        let Some(slot) = chosen else {
            // Tap outside both — cancel (pending cut already cleared); the
            // stroke's snap tags describe geometry that no longer exists.
            self.record_subarea_pick(SubareaFlow::FloorExposure, None);
            return;
        };
        // Host the chosen half's dimensioning on the reference storey's outline
        // (the same storey + buffer the cut stroke set up — re-establish it in
        // case a re-resolve happened between stroke and pick).
        let Some(ref_idx) = self.floor_exposure_reference_storey() else {
            return;
        };
        self.document.set_active_storey(ref_idx);
        self.document.active = ActiveShape::Outline;
        self.load_active_into_buffer();
        let undo = self.current_history_snapshot_full();
        // The chosen half dimensions against the WHOLE building, so the "stay
        // inside the storey" guardrail must check the union too (as the Area path).
        if let Some(boundary) = self.floor_exposure_whole_building_boundary() {
            self.document.subarea_containment_override = Some(boundary);
        }
        self.begin_floor_exposure_carve_dimensioning(slot, ref_idx, undo);
        // The stroke's tags stay live: replay now re-runs stroke + pick.
        self.record_subarea_pick(SubareaFlow::FloorExposure, Some((world_x, world_y)));
    }

    /// The whole-building floor-footprint union as a [`FloorPlan`] — the boundary
    /// a floor-exposure carve/cut clips + dimensions against (so a region drawn
    /// over another storey's area isn't lost). `None` when no union can be built.
    pub(super) fn floor_exposure_whole_building_boundary(&self) -> Option<manip_plan::FloorPlan> {
        let ring = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.floors_footprint_union())?;
        let ring32: Vec<Point2f32> = ring.iter().map(|p| domain::points::to_f32(*p)).collect();
        manip_plan::FloorPlan::from_clean_ring(&ring32).ok()
    }

    /// The reference storey whose outline a floor-exposure carve is dimensioned
    /// against: the active storey if it has a non-empty outline, else the storey
    /// with the largest footprint (falling back to storey 0). `None` when there
    /// are no storeys with an outline at all.
    pub(super) fn floor_exposure_reference_storey(&self) -> Option<usize> {
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
        // Drop the whole-building containment override set when the carve began
        // (on the reference storey), so later same-storey carves use the outline.
        if let Some(ref_idx) = self.floor_exposure_carve_ref {
            if let Some(sd) = self.document.storeys.get_mut(ref_idx) {
                sd.subarea_containment_override = None;
            }
        }
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
                // Only split storeys whose ring genuinely SUBDIVIDES a labellable
                // (non-InterStorey) floor — derived InterStorey floors are
                // auto-classified and hidden, so there's nothing to subdivide
                // there. An interior carve and an edge-to-edge cut both pass (the cut's half-ring
                // shares the outline on three sides, so the whole-floor centroid
                // sits on the cut edge — an intersection test, not a centroid test,
                // is what catches it).
                .filter(|(_, s)| s.ring_splits_labellable_floor(ring))
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
                // Area-fraction test (not centroid): select only the floor pieces
                // actually INSIDE the carved/chosen ring, so an L-shaped cut
                // remainder (whose centroid falls in the notch) is NOT picked.
                let ids = s.surfaces_mostly_in_ring(SurfaceKind::Floors, region_ring, 0.5);
                s.floors
                    .regions
                    .iter()
                    // Only the labellable (non-InterStorey) floors are
                    // shown/selectable.
                    .filter(move |r| {
                        ids.contains(&r.id) && r.exposure != FloorExposure::InterStorey
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
                // Derived InterStorey floors aren't shown/labellable, so
                // neither seed tool touches them. `All` = every labellable
                // floor; `Remaining` = the still-`Unset` ones.
                let take = match mode {
                    SelectMode::All => region.exposure != FloorExposure::InterStorey,
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
                    // The view is a labelling worklist: derived InterStorey
                    // floors are auto-classified and need no label, so they
                    // drop off. Party (NonHeatLoss) floors stay visible — the
                    // assessor paints them.
                    .filter(|r| r.exposure != FloorExposure::InterStorey)
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
