//! The CEILING-exposure TOOL — the mirror of the floor-exposure tool
//! ([`floor_exposure`](super::floor_exposure)) for the storey `ceilings`
//! regions and [`CeilingExposure`]: a storey-count gate, an all-storeys
//! overlaid CEILING view, and selection (set-all / click / carve / remaining)
//! + apply. The inter-storey classification is the pipeline's
//! `derive_inter_storey` tail, not a tool pass.
//!
//! Selection spans the whole stack — keyed by `(storey_idx, centroid key)` (NOT
//! region `ItemId`, which is re-minted every resolve / `materialize`). The
//! HIDDEN/derived class is `InterStorey`: the view + selection + carve filter
//! it OUT, showing only the to-be-set ceilings. `Party` is assessor-paintable
//! (another dwelling above — an external fact) and stays visible/selectable.
//! Differences from the floor tool: surfaces are `storey.ceilings.regions`
//! (`SurfaceKind::Ceilings`); a carve splits ROOFS — it pushes the dimensioned
//! ring onto each overlapped storey's `ceiling_splits`, then
//! `recompose_ceilings` applies it (ceilings are regions-as-truth).
use super::*;
use crate::controller::app_controller::SelectMode;
use ber_model::ber::structural::CeilingExposure;
use domain::points::Point2f64;

impl AppController {
    /// True when the ceiling-exposure tool is active (routes the canvas to the
    /// overlaid all-storeys ceiling view + selection, gates the normal plan off).
    pub fn ceiling_exposure_mode(&self) -> bool {
        self.ceiling_exposure_mode
    }

    /// True while the storey-count gate prompt is up (the tool is on but the
    /// assessor hasn't confirmed the stack is complete). The overlaid view +
    /// selection wait for confirm.
    pub fn ceiling_exposure_gate_pending(&self) -> bool {
        self.ceiling_exposure_gate_pending
    }

    /// The active ceiling-exposure Select subtool, if any.
    pub fn ceiling_exposure_select(&self) -> Option<SelectMode> {
        self.ceiling_exposure_select
    }

    /// Enter or leave the ceiling-exposure tool. Mutually exclusive with the
    /// other Mapping modes (mirrors [`set_floor_exposure_mode`](Self::set_floor_exposure_mode)).
    /// On enter the storey-count GATE is raised — the overlaid view only opens
    /// once the assessor confirms every storey is added. All selection state
    /// is cleared either way.
    pub fn set_ceiling_exposure_mode(&mut self, on: bool) {
        // Drop any half-dimensioned carve buffer before the tool toggles.
        if self.ceiling_exposure_carve_in_flight {
            self.abandon_ceiling_exposure_carve_shape();
        }
        self.ceiling_exposure_mode = on;
        self.ceiling_exposure_select = None;
        self.ceiling_exposure_selected.clear();
        self.ceiling_exposure_pending_cut = None;
        if on {
            // A PLAN task (all-storeys ceiling takeover, 2D-only input) — see
            // `set_roof_mode`: entering from 3D would show it with dead input.
            self.set_sketch_mode(SketchMode::Storey2D);
            self.ceiling_exposure_gate_pending = true;
            // Leave every other Mapping mode (each setter clears its own
            // pick/drag state) so the canvas isn't shared.
            self.set_floor_exposure_mode(false);
            self.set_exposure_mode(false);
            self.set_openings_mode(false);
            self.set_living_area_mode(false);
            self.set_porch_mode(false);
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.ceiling_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            self.dormer_mode = false;
            self.pending_rect_is_dormer = false;
        } else {
            self.ceiling_exposure_gate_pending = false;
        }
    }

    /// Gate answer YES — every storey is added. Lower the gate and bring the
    /// resolved record current (the pipeline's `derive_inter_storey` tail has
    /// then classified every storey-junction ceiling, so the view opens on the
    /// final worklist). No one-shot pass any more — the derivation runs on
    /// every re-resolve.
    pub fn confirm_all_storeys_added_ceiling(&mut self) {
        self.ceiling_exposure_gate_pending = false;
        self.materialize();
    }

    /// Gate answer NOT-YET — exit the tool so the assessor can add the
    /// remaining storeys first.
    pub fn defer_storey_count_ceiling(&mut self) {
        self.set_ceiling_exposure_mode(false);
    }

    /// Set (or clear) the ceiling-exposure Select subtool. `All`/`Remaining`
    /// seed the selection across the whole stack; `Click`/`Carve` keep the
    /// current set. No-op while the gate is up.
    pub fn set_ceiling_exposure_select_mode(&mut self, mode: Option<SelectMode>) {
        if self.ceiling_exposure_gate_pending {
            return;
        }
        // Switching away from Carve drops any in-flight (half-dimensioned) carve
        // and any cut awaiting its side pick.
        if mode != Some(SelectMode::Carve) {
            if self.ceiling_exposure_carve_in_flight {
                self.abandon_ceiling_exposure_carve_shape();
            }
            self.ceiling_exposure_pending_cut = None;
        }
        self.ceiling_exposure_select = mode;
        if matches!(mode, Some(SelectMode::All | SelectMode::Remaining)) {
            self.reseed_ceiling_exposure_selection();
        }
    }

    /// The selected ceiling regions, keyed `(storey_idx, centroid key)` — the
    /// overlaid view halos exactly these. Keyed by centroid (not `ItemId`)
    /// because ceiling-region ids are re-minted on every resolve.
    pub fn ceiling_exposure_selected_ids(&self) -> &HashSet<(usize, document::CentroidKey)> {
        &self.ceiling_exposure_selected
    }

    /// Clear every selected ceiling region ("Deselect all"). Keeps the active
    /// Select subtool so the assessor can keep picking.
    pub fn clear_ceiling_exposure_selection(&mut self) {
        self.ceiling_exposure_selected.clear();
    }

    /// Toggle the ceiling region under a tapped point (world metres) in/out of
    /// the selection — hit-tested across EVERY storey. When several storeys'
    /// ceilings overlap the point, the SMALLEST-area region wins (the most
    /// specific sub-region), so a tap lands on the piece the assessor most likely
    /// means. No-op while the gate is up or no Click subtool is active.
    pub fn toggle_ceiling_exposure_at(&mut self, world_x: f64, world_y: f64, tol: f64) {
        if self.ceiling_exposure_gate_pending
            || !matches!(self.ceiling_exposure_select, Some(SelectMode::Click))
        {
            return;
        }
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        // Smallest-area ceiling region under the point across the whole stack;
        // store its centroid key (stable across re-resolves, unlike the id).
        let mut best: Option<(f64, usize, document::CentroidKey)> = None;
        for (idx, storey) in record.data.storeys.iter().enumerate() {
            if let Some(id) = storey.surface_at(SurfaceKind::Ceilings, world_x, world_y, tol) {
                if let Some(region) = storey
                    .ceilings
                    .regions
                    .iter()
                    // Derived InterStorey ceilings aren't shown, so a tap
                    // can't select one.
                    .find(|r| r.id == id && r.exposure != CeilingExposure::InterStorey)
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
            if !self.ceiling_exposure_selected.insert(sel) {
                self.ceiling_exposure_selected.remove(&sel);
            }
        }
    }

    /// A ceiling-exposure CARVE stroke (world coords): carve a region against the
    /// whole-building outline, then DIMENSION it precisely before it SPLITS the
    /// ROOFS — the ceiling mirror of
    /// [`apply_floor_exposure_carve_stroke`](Self::apply_floor_exposure_carve_stroke).
    /// Drawing the region puts it into the live dimensioning buffer as a
    /// `PhaseArea` shape; once fully dimensioned the ring is staged + committed
    /// ([`stage_ceiling_exposure_carve`]), splitting the ceilings on every
    /// overlapping storey and selecting the resulting piece(s). A straight cut
    /// (two halves) isn't supported — this tool requires a closed carve region —
    /// so a `Cut` outcome is ignored.
    pub fn apply_ceiling_exposure_carve_stroke(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if self.ceiling_exposure_gate_pending
            || !matches!(self.ceiling_exposure_select, Some(SelectMode::Carve))
        {
            return;
        }
        // Already dimensioning a carve — ignore a second stroke until it's done.
        if self.ceiling_exposure_carve_in_flight {
            return;
        }
        let raw: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if raw.len() < 2 {
            return;
        }
        // Snap-tag capture: the pre-action state, taken BEFORE the document
        // mutation below (set_active_storey / load_active_into_buffer) so a
        // replay restores the caller's original state. FULL — the eventual
        // commit touches every overlapping storey's splits. A pending CUT
        // pushes no undo entry, so its capture carries this `before` itself.
        // The stale-decision drop sits BEFORE the reference pre-snap so ITS
        // decisions are captured with the carve's.
        let before = self.current_history_snapshot_full();
        self.begin_snap_capture();
        // Pre-snap the stroke onto INTERIOR reference features the whole-building
        // clip can't see: every storey's roof-region division edges ("end of the
        // roof", incl. the flat/sloped split) and every storey's wall base edges
        // (the wall of the floor above projects onto this ceiling). `carve_or_cut`
        // below still clips + aligns to the whole-building outline; a vertex
        // pre-snapped onto an interior line sits away from that outline, so the
        // boundary-follow leaves it on the division.
        let ref_segments = self.ceiling_carve_reference_segments();
        let snap_tol = self.snap_tol_ceiling_exposure();
        let pts = manip_plan::snap_stroke_to_segments(
            &raw,
            &ref_segments,
            snap_tol,
            manip_plan::SnapKind::ToReferenceSegment,
        );
        // Host the in-flight carve's dimensioning on a reference storey (the
        // PhaseArea buffer lives on the active storey). Reuse the floor tool's
        // reference-storey pick (footprints are shared by floors + ceilings).
        let Some(ref_idx) = self.floor_exposure_reference_storey() else {
            return;
        };
        self.document.set_active_storey(ref_idx);
        self.document.active = ActiveShape::Outline;
        self.load_active_into_buffer();
        // Clip the carve to the WHOLE BUILDING footprint (union of every storey's
        // floor — ceilings share the footprint extent), NOT just the active/reference
        // storey's outline, so a carve drawn over area belonging to another storey
        // isn't clipped away. Each storey's roof is still clipped individually when
        // the split is applied. Falls back to the reference storey's outline.
        let boundary = self
            .floor_exposure_whole_building_boundary()
            .unwrap_or_else(|| self.document.outline.floor_plan.clone());
        let wall_snap_tol = self.snap_tol_carve();
        match boundary.carve_or_cut(&pts, &self.tuning.subarea, false, Some(wall_snap_tol)) {
            Ok(SubareaOutcome::Area(plan)) => {
                if plan.validate_simple().is_err() {
                    // Nothing committed — drop the rejected carve's decisions.
                    let _ = manip_plan::take_snap_decisions();
                    return;
                }
                let undo = self.current_history_snapshot_full();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                // The carve dimensions against the WHOLE building, so the
                // "stay inside the storey" guardrail must check the union too.
                self.document.subarea_containment_override = Some(boundary);
                self.begin_ceiling_exposure_carve_dimensioning(slot, ref_idx, undo);
                self.capture_snap_action(SnapAction::Subarea {
                    flow: SubareaFlow::CeilingExposure,
                    points: world_points.to_vec(),
                    pick: None,
                });
            }
            // A straight edge-to-edge cut splits the whole building into two
            // candidate halves; pin both against the SAME whole-building boundary
            // and stash the pending cut. The side pick
            // (`choose_ceiling_exposure_cut_region`) sets the override + begins
            // dimensioning, mirroring the Area path. The active storey stays
            // `ref_idx` so the pick hosts there.
            Ok(SubareaOutcome::Cut(a, b)) => {
                self.ceiling_exposure_pending_cut = Some([
                    self.pinned_subarea_slot(&boundary, a),
                    self.pinned_subarea_slot(&boundary, b),
                ]);
                self.capture_snap_action_pending(
                    SnapAction::Subarea {
                        flow: SubareaFlow::CeilingExposure,
                        points: world_points.to_vec(),
                        pick: None,
                    },
                    before,
                    manip_plan::take_snap_decisions(),
                );
            }
            Err(e) => {
                log::error!("ceiling exposure carve_or_cut: {}", e);
                let _ = manip_plan::take_snap_decisions();
            }
        }
    }

    /// The plan reference segments a ceiling-exposure carve stroke should snap
    /// onto BEFORE the whole-building clip — features that aren't on the carve's
    /// boundary outline but ARE real ceiling edges the assessor traces toward:
    ///
    /// - **Roof-region division edges** — every storey's `ceilings.regions[*]`
    ///   exterior, as consecutive vertex pairs projected to xy (z dropped). This
    ///   includes the flat/sloped-ridge split ("end of the roof").
    /// - **Wall base edges** — every storey's `walls.regions[*].base_edge()`
    ///   projected to xy: a storey's ceiling sits under the storey above, whose
    ///   walls project onto it ("the wall of the floor above"). Gathering ALL
    ///   storeys' wall bases covers it without working out which storey is above.
    ///
    /// Returns world-metre `[Point2f32; 2]` segments (duplicates harmless — the
    /// snap just picks the nearest). Empty when there's no record.
    pub(crate) fn ceiling_carve_reference_segments(&self) -> Vec<[Point2f32; 2]> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let xy = |p: &domain::points::Point3f64| {
            domain::points::to_f32(domain::points::Point2f64::new(p.x, p.y))
        };
        let mut segments: Vec<[Point2f32; 2]> = Vec::new();
        for storey in &record.data.storeys {
            // Roof-region edges (incl. the flat/sloped division).
            for region in &storey.ceilings.regions {
                let ext = &region.polygon.exterior;
                let n = ext.len();
                for i in 0..n {
                    segments.push([xy(&ext[i]), xy(&ext[(i + 1) % n])]);
                }
            }
            // Wall base edges (the floor-above wall projects onto this ceiling).
            for wall in &storey.walls.regions {
                if let Some((a, b)) = wall.base_edge() {
                    segments.push([xy(&a), xy(&b)]);
                }
            }
        }
        segments
    }

    /// The two candidate halves of a pending ceiling-exposure cut (world coords),
    /// for the overlaid view to shade while the assessor picks a side. Empty when
    /// no cut is pending. Mirror of
    /// [`floor_exposure_cut_regions`](Self::floor_exposure_cut_regions).
    pub fn ceiling_exposure_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.ceiling_exposure_pending_cut
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

    /// True while a ceiling-exposure cut is awaiting the assessor's side pick.
    pub fn ceiling_exposure_cut_pending(&self) -> bool {
        self.ceiling_exposure_pending_cut.is_some()
    }

    /// Pick which candidate half a ceiling-exposure cut should dimension + split
    /// — the ceiling mirror of
    /// [`choose_floor_exposure_cut_region`](Self::choose_floor_exposure_cut_region).
    /// A tap outside both cancels. From the begin call it's the SAME dimension →
    /// stage → commit (`commit_ceiling_exposure_carve`) path as the Area carve.
    pub fn choose_ceiling_exposure_cut_region(&mut self, world_x: f32, world_y: f32) {
        if self.ceiling_exposure_gate_pending
            || !matches!(self.ceiling_exposure_select, Some(SelectMode::Carve))
        {
            return;
        }
        let Some([a, b]) = self.ceiling_exposure_pending_cut.take() else {
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
            self.record_subarea_pick(SubareaFlow::CeilingExposure, None);
            return;
        };
        // Host the chosen half on the reference storey's outline (re-establish the
        // storey + buffer the cut stroke set up, in case a re-resolve intervened).
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
        self.begin_ceiling_exposure_carve_dimensioning(slot, ref_idx, undo);
        // The stroke's tags stay live: replay now re-runs stroke + pick.
        self.record_subarea_pick(SubareaFlow::CeilingExposure, Some((world_x, world_y)));
    }

    /// Make the carved region the in-flight `PhaseArea` split shape and start
    /// dimensioning its drawn walls (mirrors
    /// [`begin_floor_exposure_carve_dimensioning`](Self::begin_floor_exposure_carve_dimensioning)).
    fn begin_ceiling_exposure_carve_dimensioning(
        &mut self,
        slot: ShapeSlot,
        ref_idx: usize,
        undo: HistorySnapshot,
    ) {
        self.document.begin_phase_area(slot);
        self.ceiling_exposure_carve_in_flight = true;
        self.ceiling_exposure_carve_ref = Some(ref_idx);
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo);
        self.maybe_stage_ceiling_exposure_carve();
    }

    /// The in-flight ceiling-exposure carve ring (world coords) for the overlaid
    /// view to highlight — the LIVE dimensioned buffer, so it tracks dimensioning.
    /// Empty when no carve is in-flight.
    pub fn pending_ceiling_exposure_carve_polygon(&self) -> Vec<(f64, f64)> {
        if self.ceiling_exposure_carve_in_flight && self.document.active == ActiveShape::PhaseArea {
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

    /// True once the in-flight ceiling-exposure carve is fully DIMENSIONED (wall
    /// lengths AND position) and ready to commit.
    fn ceiling_exposure_carve_dimensioned(&self) -> bool {
        self.ceiling_exposure_carve_in_flight
            && self.document.active == ActiveShape::PhaseArea
            && self.document.dimensioning.current_prompt.is_none()
            && self.document.dimensioning.current_position_prompt.is_none()
            && self.document.floor_plan.get_points().len() >= 3
    }

    /// STAGE + COMMIT the ceiling-exposure carve if its drawn walls are fully
    /// dimensioned — a cheap no-op unless a carve is in-flight and ready. Called
    /// at the end of every dimension/position apply (the shared hook with the
    /// Structure + floor carves), so a carve that finishes on either kind of
    /// prompt commits.
    pub fn maybe_stage_ceiling_exposure_carve(&mut self) {
        if self.ceiling_exposure_carve_dimensioned() {
            self.stage_ceiling_exposure_carve();
        }
    }

    /// Discard the in-flight carve shape and return to the outline, dropping its
    /// half-dimensioned buffer (mirrors
    /// [`abandon_floor_exposure_carve_shape`](Self::abandon_floor_exposure_carve_shape)).
    fn abandon_ceiling_exposure_carve_shape(&mut self) {
        // Drop the whole-building containment override set when the carve began.
        if let Some(ref_idx) = self.ceiling_exposure_carve_ref {
            if let Some(sd) = self.document.storeys.get_mut(ref_idx) {
                sd.subarea_containment_override = None;
            }
        }
        self.document.phase_shape = None;
        self.ceiling_exposure_carve_in_flight = false;
        self.ceiling_exposure_carve_ref = None;
        if self.document.active == ActiveShape::PhaseArea {
            self.document.active = ActiveShape::Outline;
            self.load_active_into_buffer();
            self.document.pending_arc = None;
            self.edit_drag_prompt = None;
            self.vertex_drop_prompt = None;
            self.document.edit_state = EditState::default();
            self.compute_next_dimension_prompt();
        }
        // Re-point the document's active storey at the selected tab.
        if let Some(idx) = self.current_storey_idx {
            self.document.set_active_storey(idx);
            self.load_active_into_buffer();
        }
    }

    /// Capture the dimensioned carve ring and SPLIT the ceilings on every storey
    /// whose footprint the ring overlaps, then SELECT the resulting carved
    /// piece(s) — the ceiling mirror of
    /// [`stage_floor_exposure_carve`](Self::stage_floor_exposure_carve). The undo
    /// snapshot was already pushed when the carve began. Cleans up the in-flight
    /// shape either way.
    fn stage_ceiling_exposure_carve(&mut self) {
        if !self.ceiling_exposure_carve_in_flight {
            return;
        }
        let ring32 = self.document.floor_plan.get_points();
        if ring32.len() < 3 {
            self.abandon_ceiling_exposure_carve_shape();
            return;
        }
        let ring: Vec<Point2f64> = ring32.iter().map(|p| domain::points::to_f64(*p)).collect();
        // Drop the in-flight buffer FIRST (restores the outline + clears the
        // flags), so the split + re-resolve below run on a clean editing model.
        self.abandon_ceiling_exposure_carve_shape();
        self.commit_ceiling_exposure_carve(&ring);
    }

    /// Push the carve `ring` onto the durable `ceiling_splits` of every storey whose
    /// ROOF footprint it overlaps, recompose each (ceilings are regions-as-truth),
    /// re-resolve the whole stack, then add the resulting carved ceiling piece(s)
    /// to the selection. Only storeys with a LABELLABLE (non-InterStorey) ceiling
    /// under the ring are split. No undo snapshot here — the caller pushed the
    /// full snapshot when the carve began.
    fn commit_ceiling_exposure_carve(&mut self, ring: &[Point2f64]) {
        // Which storeys' roof footprints does the carve overlap, AND carry a
        // labellable (non-InterStorey) ceiling under the ring? Derived
        // InterStorey ceilings are auto-classified and hidden, so there's
        // nothing to subdivide there.
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
                // (non-InterStorey) ceiling. An interior carve and an edge-to-edge cut
                // both pass — the cut's half-ring shares the outline on three
                // sides, so an intersection test (not a region-centroid test) is
                // what catches it.
                .filter(|(_, s)| s.ring_splits_labellable_ceiling(ring))
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
                storey.push_structure_split(SurfaceKind::Ceilings, ring, Vec::new());
            }
            // Ceilings are regions-as-truth — `resolve_storey` only re-fits them, so
            // the `ceiling_splits` carve must be applied via `recompose_ceilings` (it
            // reads the durable field) before the re-resolve.
            self.recompose_ceilings(*idx);
        }
        let _ = self.mark_dirty();
        // Re-resolve EVERY storey so each overlapping storey's recomposed ceilings +
        // stamps land; keep the derived revision in step so a later no-edit
        // materialize is a no-op.
        self.refresh_all_storeys();
        self.derived_revision = self.revision;
        // Select the carved piece(s): the ceiling region(s) now sitting inside the
        // ring on each overlapping storey.
        self.select_ceilings_in_region(ring);
    }

    /// Add every ceiling region inside `region_ring` (world coords) — across ALL
    /// storeys — to the selection.
    fn select_ceilings_in_region(&mut self, region_ring: &[Point2f64]) {
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let hits: Vec<(usize, document::CentroidKey)> = record
            .data
            .storeys
            .iter()
            .enumerate()
            .flat_map(|(idx, s)| {
                // Area-fraction test (not centroid): select only the ceiling
                // pieces actually INSIDE the carved/chosen ring, so an L-shaped
                // cut remainder (centroid in the notch) is NOT picked.
                let ids = s.surfaces_mostly_in_ring(SurfaceKind::Ceilings, region_ring, 0.5);
                s.ceilings
                    .regions
                    .iter()
                    // Only the labellable (non-InterStorey) ceilings are
                    // shown/selectable.
                    .filter(move |r| {
                        ids.contains(&r.id) && r.exposure != CeilingExposure::InterStorey
                    })
                    .map(move |r| (idx, document::centroid_key(r.centroid())))
            })
            .collect();
        self.ceiling_exposure_selected.extend(hits);
    }

    /// Re-seed the selection for an active `All`/`Remaining` subtool across the
    /// whole stack: `All` takes every ceiling region; `Remaining` only the ones
    /// still `Unset`. No-op when no seed tool is active.
    fn reseed_ceiling_exposure_selection(&mut self) {
        let Some(mode) = self.ceiling_exposure_select else {
            return;
        };
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let mut keys: HashSet<(usize, document::CentroidKey)> = HashSet::new();
        for (idx, storey) in record.data.storeys.iter().enumerate() {
            for region in &storey.ceilings.regions {
                // Derived InterStorey ceilings aren't shown/labellable, so
                // neither seed tool touches them. `All` = every labellable
                // ceiling; `Remaining` = the still-`Unset` ones.
                let take = match mode {
                    SelectMode::All => region.exposure != CeilingExposure::InterStorey,
                    SelectMode::Remaining => region.exposure == CeilingExposure::Unset,
                    _ => false,
                };
                if take {
                    keys.insert((idx, document::centroid_key(region.centroid())));
                }
            }
        }
        self.ceiling_exposure_selected = keys;
    }

    /// Apply `exposure` to every selected ceiling region across the stack:
    /// snapshot for undo, write each region's durable per-storey override
    /// ([`StoreyDoc::ceiling_exposure`], keyed by centroid), then re-derive +
    /// persist. Parallel to
    /// [`set_floor_exposure_for`](Self::set_floor_exposure_for). No-op on empty
    /// selection / no record.
    pub fn set_ceiling_exposure_for(&mut self, exposure: CeilingExposure) {
        if self.ceiling_exposure_selected.is_empty() {
            return;
        }
        let snap = self.current_history_snapshot();
        let keys: Vec<(usize, document::CentroidKey)> =
            self.ceiling_exposure_selected.iter().copied().collect();
        for (idx, key) in keys {
            if let Some(sd) = self.document.storeys.get_mut(idx) {
                sd.ceiling_exposure.insert(key, exposure);
            }
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(snap);
    }

    /// The overlaid all-storeys CEILING view data for the canvas + legend: per
    /// storey, its ceiling regions as `(id, ring world coords, exposure)`. The
    /// canvas fits ONE transform to the union of every storey's footprint and
    /// draws each storey in its own palette shade with the exposure as a coloured
    /// outline; the selected set is haloed. The derived `InterStorey` ceilings
    /// drop off — the view is a labelling worklist; `Party` (another dwelling
    /// above) is an assessor label and stays visible.
    pub fn ceiling_exposure_view(&self) -> Vec<CeilingExposureStoreyView> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        record
            .data
            .storeys
            .iter()
            .enumerate()
            .map(|(idx, storey)| CeilingExposureStoreyView {
                idx,
                regions: storey
                    .ceilings
                    .regions
                    .iter()
                    // Derived InterStorey ceilings are auto-classified and
                    // need no label.
                    .filter(|r| r.exposure != CeilingExposure::InterStorey)
                    .map(|r| {
                        // The ceiling `Polygon3D` exterior projected to xy (drop z),
                        // closing duplicate removed to match the floor ring shape.
                        let ring: Vec<(f64, f64)> = {
                            let mut pts: Vec<(f64, f64)> =
                                r.polygon.exterior.iter().map(|v| (v.x, v.y)).collect();
                            if pts.len() >= 2 && pts[0] == pts[pts.len() - 1] {
                                pts.pop();
                            }
                            pts
                        };
                        CeilingExposureRegionView {
                            id: r.id,
                            ring,
                            exposure: r.exposure,
                            selected: self
                                .ceiling_exposure_selected
                                .contains(&(idx, document::centroid_key(r.centroid()))),
                        }
                    })
                    .collect(),
            })
            .collect()
    }
}

/// One storey's ceilings for the overlaid ceiling-exposure view.
pub struct CeilingExposureStoreyView {
    pub idx: usize,
    pub regions: Vec<CeilingExposureRegionView>,
}

/// One ceiling region in the overlaid view: stable id, plan ring (world coords,
/// no closing duplicate), its exposure, and whether it's currently selected.
pub struct CeilingExposureRegionView {
    pub id: ItemId,
    pub ring: Vec<(f64, f64)>,
    pub exposure: CeilingExposure,
    pub selected: bool,
}
