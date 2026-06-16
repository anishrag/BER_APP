//! Living-area sub-shapes: potential living spaces drawn INSIDE a storey, the
//! same way the storey outline is drawn (orthogonal sketch → trim/extend/curve
//! → dimension). Multiple are allowed and all are kept (the largest is chosen
//! as THE living area later). They inherit the storey's metre scale, so they
//! aren't re-calibrated — dimensioning just sets their walls.
//!
//! The editor keeps the ACTIVE shape live in `floor_plan`/`dimensioning`; the
//! outline and each living area otherwise rest in `ShapeSlot`s. Switching the
//! active shape saves the live buffer back to its slot and loads the target —
//! so every existing drawing/dimensioning/edit method keeps working unchanged
//! on whatever shape is active.
use super::*;

impl AppController {
    /// Write the live buffer (`floor_plan`/`dimensioning`) back into the slot
    /// of whatever shape is currently active, so the slots are current before
    /// they're read (persist / switch / snapshot).
    pub(super) fn sync_active(&mut self) {
        self.document.sync_active();
    }

    /// Load the active shape's slot into the live buffer.
    pub(super) fn load_active_into_buffer(&mut self) {
        self.document.load_active_into_buffer();
    }

    /// Switch which shape the editor acts on: save the current live buffer to
    /// its slot, then load the target. Transient per-shape state (the active
    /// dimension prompt is in `dimensioning`; provisional arc + edit/drag
    /// prompts are abandoned on a switch).
    pub(super) fn activate_shape(&mut self, target: ActiveShape) {
        if self.document.active == target {
            return;
        }
        self.sync_active();
        self.document.active = target;
        self.load_active_into_buffer();
        self.document.pending_arc = None;
        self.pending_cut = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        self.document.edit_state = EditState::default();
        self.compute_next_dimension_prompt();
    }

    /// True when the editor is drawing/editing living areas rather than the
    /// storey outline (drives the UI toggle and DrawPerpWalls routing).
    pub fn living_area_mode(&self) -> bool {
        self.living_area_mode
    }

    /// Switch between editing the storey outline and the living areas. Entering
    /// living-area mode activates the most-recent living area (if any); leaving
    /// returns to the outline.
    pub fn set_living_area_mode(&mut self, on: bool) {
        self.living_area_mode = on;
        self.pending_cut = None;
        if on {
            self.exit_openings_mode();
            // Mutually exclusive with the ceiling-step line and roof-point
            // tools (the other area tools clear each other the same way).
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.roof_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            // Drop straight into draw mode so a stroke makes a new living area
            // without the user also having to pick the tool.
            self.set_edit_mode(EditTools::DrawPerpWalls);
            if !self.document.living_areas.is_empty() {
                self.activate_shape(ActiveShape::LivingArea(
                    self.document.living_areas.len() - 1,
                ));
            }
            // else: nothing to edit yet — the next DrawPerpWalls creates the
            // first living area (the outline stays active until then).
        } else {
            self.activate_shape(ActiveShape::Outline);
        }
    }

    /// Number of living areas drawn so far.
    pub fn living_area_count(&self) -> usize {
        self.document.living_areas.len()
    }

    /// Index of the living area the tools currently act on, if any.
    pub fn active_living_area_index(&self) -> Option<usize> {
        match self.document.active {
            ActiveShape::LivingArea(i) => Some(i),
            ActiveShape::Outline | ActiveShape::PhaseArea | ActiveShape::StepArea => None,
        }
    }

    /// Resolved outlines (world coords) of every living area, for rendering them
    /// over the storey in a distinct colour. Derived DIRECTLY from the editing
    /// model (the active area from the live buffer, the rest from their slots) —
    /// no round-trip through the committed BER. Index-aligned with
    /// [`living_area_wall_labels`](Self::living_area_wall_labels).
    pub fn living_area_polygons(&self) -> Vec<Vec<(f64, f64)>> {
        self.document
            .living_areas
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let plan = if self.document.active == ActiveShape::LivingArea(i) {
                    &self.document.floor_plan
                } else {
                    &slot.floor_plan
                };
                plan.get_points()
                    .into_iter()
                    .map(|p| (p.x as f64, p.y as f64))
                    .collect()
            })
            .collect()
    }

    /// Per living area, the walls whose length is known — as
    /// `(start, end, length)` in world coords — so the UI can label them the
    /// same way the storey's dimensioned walls are labelled. The active area is
    /// read from the live buffer + its dimensioning; the others from their
    /// slots. Index-aligned with [`living_area_polygons`](Self::living_area_polygons).
    pub fn living_area_wall_labels(&self) -> Vec<Vec<((f64, f64), (f64, f64), f32)>> {
        self.document
            .living_areas
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let (plan, dim) = if self.document.active == ActiveShape::LivingArea(i) {
                    (&self.document.floor_plan, &self.document.dimensioning)
                } else {
                    (&slot.floor_plan, &slot.dimensioning)
                };
                let pts = plan.get_points();
                let n = pts.len();
                if n < 3 {
                    return Vec::new();
                }
                plan.walls_with_known_length(&dim.known_params)
                    .into_iter()
                    .filter(|(ring, _)| *ring == 0)
                    .filter_map(|(ring, e)| {
                        let a = pts[e % n];
                        let b = pts[(e + 1) % n];
                        let len = plan.wall_length(ring, e)?;
                        Some(((a.x as f64, a.y as f64), (b.x as f64, b.y as f64), len))
                    })
                    .collect()
            })
            .collect()
    }

    /// Commit a freshly-carved living-area `plan` — the [`Area`] outcome of
    /// `carve_or_cut`. Validates the geometry, inherits the storey's scale,
    /// pre-marks the walls shared with the outline as known (so dimensioning only
    /// asks about the drawn walls), makes it the active shape, appends it, and
    /// starts dimensioning. Returns a user-facing message if the region
    /// self-crosses. (The clip/carve geometry lives in `manip_2d_plan`; this owns
    /// only the commit workflow.)
    ///
    /// [`Area`]: manip_2d_plan::SubareaOutcome::Area
    fn commit_living_area_plan(
        &mut self,
        boundary: &FloorPlan,
        plan: FloorPlan,
    ) -> Result<(), String> {
        let undo_snap = self.current_history_snapshot();
        log::debug!("── living-area plan ─────────────────────────────────");
        log::debug!("  → living-area plan: {:?}", plan);
        let bad = plan.non_orthogonal_edges();
        if bad.is_empty() {
            log::debug!("  → orthogonal ✓");
        } else {
            log::debug!("  → ⚠ NON-ORTHOGONAL edges: {:?}", bad);
        }
        log::debug!("────────────────────────────────────────────────────");
        // Fallback guardrail: the geometry layer un-intersects a folded ring, so
        // this rarely fires — but refuse a survivor rather than commit bad geometry.
        if let Err(e) = plan.validate_simple() {
            return Err(format!(
                "the living area's walls cross ({:?}) — redraw it so it doesn't fold over itself",
                e
            ));
        }
        let slot = self.pinned_subarea_slot(boundary, plan);
        self.commit_new_living_area(slot, undo_snap);
        Ok(())
    }

    /// Build a `ShapeSlot` for a freshly-constructed sub-area plan: inherit the
    /// storey's scale, and pre-mark as KNOWN the parameters pinned to `boundary`
    /// (walls snapped onto it) — so dimensioning only asks about the walls the
    /// user actually drew. `boundary` is the storey outline for a living area, or
    /// the available region (outline minus existing phases) for a phase area, so a
    /// wall shared with a neighbour phase is pre-known too.
    pub(super) fn pinned_subarea_slot(&self, boundary: &FloorPlan, plan: FloorPlan) -> ShapeSlot {
        let slot = self.document.pinned_slot_for(boundary, plan);
        if !slot.dimensioning.known_params.is_empty() {
            log::debug!(
                "  pinned-to-outline params (pre-known): {:?}",
                slot.dimensioning.known_params
            );
        }
        slot
    }

    /// Append `slot` as a new living area, make it active, persist, and start
    /// dimensioning it. Shared by a freehand draw and a cut-region pick.
    fn commit_new_living_area(&mut self, slot: ShapeSlot, undo_snap: HistorySnapshot) {
        self.document.add_active_living_area(slot);
        self.living_area_mode = true;
        self.pending_cut = None;
        let _ = self.mark_dirty();
        self.warn_if_non_orthogonal("draw living area");
        // Dimension the new area's walls (no scale calibration — inherited).
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
    }

    /// UI entry point: a living-area DrawPerpWalls stroke, already converted to
    /// world coords (the area sits in the storey's space). Drops the raw canvas
    /// stroke. A single straight CUT line splits the storey into two regions and
    /// asks the user to pick one (see [`pending_cut_regions`]); any other stroke
    /// starts a new living area directly (endpoints snap to the outline; a
    /// stroke that doesn't reach it becomes a free-standing room).
    pub fn apply_living_area_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        dump_stroke("living-area", &pts);
        // Save the live buffer so the outline we carve against is current.
        self.sync_active();
        // Living areas never overlap: carve against the outline MINUS the existing
        // living areas (the available region), so a new one tiles against them and
        // snaps to their shared walls. The available region may be disconnected;
        // the cut runs against the piece the stroke is in.
        let holes: Vec<Vec<Point2f32>> = self
            .document
            .living_areas
            .iter()
            .map(|s| s.floor_plan.get_points())
            .collect();
        // One call into `document` does subtract → pick piece → clip → cut-or-carve;
        // we only dispatch the outcome to the right workflow.
        match self
            .document
            .carve_or_cut_in_available(&pts, &holes, &self.tuning.subarea)
        {
            // A straight cut → offer both halves to pick from. Each region's walls
            // along the boundary are pinned (pre-known); only the cut wall is set.
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                log::debug!("── cut splits the available region into two — tap one to keep ──");
                self.pending_cut = Some([
                    self.pinned_subarea_slot(&boundary, a),
                    self.pinned_subarea_slot(&boundary, b),
                ]);
            }
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                if let Err(e) = self.commit_living_area_plan(&boundary, plan) {
                    log::error!("living area: {}", e);
                }
            }
            Err(e) => log::error!("living-area carve_or_cut: {}", e),
        }
    }

    /// The two candidate regions of a pending cut (world coords), for the UI to
    /// highlight while the user picks. Empty when no cut is pending.
    pub fn pending_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.pending_cut
            .as_ref()
            .map(|regions| {
                regions
                    .iter()
                    .map(|s| {
                        s.floor_plan
                            .get_points()
                            .iter()
                            .map(|p| (p.x as f64, p.y as f64))
                            .collect()
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// True while a cut is awaiting the user's pick.
    pub fn cut_pick_pending(&self) -> bool {
        self.pending_cut.is_some()
    }

    /// Pick the pending-cut region containing `(world_x, world_y)` as the living
    /// area. A tap outside both regions cancels the pick. No-op if none pending.
    pub fn choose_cut_region(&mut self, world_x: f32, world_y: f32) {
        let Some([a, b]) = self.pending_cut.take() else {
            return;
        };
        let chosen = if a.floor_plan.contains_point(world_x, world_y) {
            Some(a)
        } else if b.floor_plan.contains_point(world_x, world_y) {
            Some(b)
        } else {
            None // tapped outside both → cancel
        };
        if let Some(slot) = chosen {
            // Guardrail: don't commit a self-intersecting region.
            if let Err(e) = slot.floor_plan.validate_simple() {
                log::warn!("cut region rejected (walls cross): {:?}", e);
                return;
            }
            let undo_snap = self.current_history_snapshot();
            self.commit_new_living_area(slot, undo_snap);
        }
    }

    /// Delete the living area at `index` (living areas are non-editable — to
    /// change one, delete and redraw). Undoable; persists.
    pub fn delete_living_area(&mut self, index: usize) {
        if index >= self.document.living_areas.len() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        // Detach from any active living area before mutating the list, so the
        // live buffer + `active` index stay valid.
        self.activate_shape(ActiveShape::Outline);
        self.document.living_areas.remove(index);
        // Keep working on the most-recent remaining area if still in LA mode.
        if self.living_area_mode && !self.document.living_areas.is_empty() {
            self.activate_shape(ActiveShape::LivingArea(
                self.document.living_areas.len() - 1,
            ));
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }
}
