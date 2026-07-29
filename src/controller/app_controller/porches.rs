//! Porch sub-shapes: porch regions drawn INSIDE a storey exactly the way a
//! living area is (orthogonal sketch → trim/extend/curve → dimension, carved
//! against the outline, never position-prompted). A porch records its labelled
//! region (label + area) only — it has no effect on resolve/calculations yet;
//! a later resolve-phase step decides inclusion/exclusion.
//!
//! The workflow mirrors `living_areas.rs` one-for-one: the shared slot
//! machinery (`sync_active`/`activate_shape`/`pinned_subarea_slot_refs`) and
//! the shared cut-pick (`pending_cut`/`choose_cut_region` — the staged cut
//! carries its `SubareaFlow`, so the pick commits here whenever a porch stroke
//! staged it) live there; this file owns only the porch-specific
//! mode/commit/delete/view surface.
use super::*;

impl AppController {
    /// True when the editor is drawing/editing porches rather than the storey
    /// outline (drives the UI toggle and DrawPerpWalls routing).
    pub fn porch_mode(&self) -> bool {
        self.porch_mode
    }

    /// Switch between editing the storey outline and the porches. Entering
    /// porch mode activates the most-recent porch (if any); leaving returns to
    /// the outline. Mirrors `set_living_area_mode` (same mutual-exclusion set;
    /// the two area tools also clear each other).
    pub fn set_porch_mode(&mut self, on: bool) {
        // Leaving the outline draw tool: drop any un-decided re-draw stroke
        // (a stray second outline) so it doesn't linger on the canvas. (n3)
        self.pending_redraw = None;
        self.porch_mode = on;
        self.pending_cut = None;
        if on {
            self.exit_openings_mode();
            // Leave every other mutually-exclusive MAPPING mode (each setter
            // clears its own pick/drag state) — the same sweep as
            // `set_living_area_mode`, plus living-area mode itself (the two
            // sub-area tools are mutually exclusive). Drawing walls /
            // extend-ortho / extend-curve stay usable WITHIN a porch — those
            // are edit tools, not Mapping modes, so they're not cleared here.
            self.set_living_area_mode(false);
            self.set_exposure_mode(false);
            self.set_floor_exposure_mode(false);
            self.set_ceiling_exposure_mode(false);
            self.set_roof_mode(false);
            self.set_phase_mode(false);
            self.set_step_mode(false);
            // Mutually exclusive with the ceiling-step line and roof-point
            // tools (the other area tools clear each other the same way).
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.ceiling_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            self.dormer_mode = false;
            self.pending_rect_is_dormer = false;
            // Drop straight into draw mode so a stroke makes a new porch
            // without the user also having to pick the tool.
            self.set_edit_mode(EditTools::DrawPerpWalls);
            if !self.document.porches.is_empty() {
                self.activate_shape(ActiveShape::Porch(self.document.porches.len() - 1));
            }
            // else: nothing to edit yet — the next DrawPerpWalls creates the
            // first porch (the outline stays active until then).
        } else {
            self.activate_shape(ActiveShape::Outline);
        }
    }

    /// Number of porches drawn so far.
    pub fn porch_count(&self) -> usize {
        self.document.porches.len()
    }

    /// Index of the porch the tools currently act on, if any.
    pub fn active_porch_index(&self) -> Option<usize> {
        match self.document.active {
            ActiveShape::Porch(i) => Some(i),
            ActiveShape::Outline
            | ActiveShape::LivingArea(_)
            | ActiveShape::PhaseArea
            | ActiveShape::StepArea => None,
        }
    }

    /// Resolved outlines (world coords) of every porch, for rendering them over
    /// the storey in a distinct colour. Derived DIRECTLY from the editing model
    /// (the active porch from the live buffer, the rest from their slots) — no
    /// round-trip through the committed BER. Index-aligned with
    /// [`porch_wall_labels`](Self::porch_wall_labels).
    pub fn porch_polygons(&self) -> Vec<Vec<(f64, f64)>> {
        self.document
            .porches
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let plan = if self.document.active == ActiveShape::Porch(i) {
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

    /// Per porch, the walls whose length is known — as `(start, end, length)`
    /// in world coords — so the UI can label them the same way the storey's
    /// dimensioned walls are labelled. The active porch is read from the live
    /// buffer + its dimensioning; the others from their slots. Index-aligned
    /// with [`porch_polygons`](Self::porch_polygons).
    pub fn porch_wall_labels(&self) -> Vec<Vec<((f64, f64), (f64, f64), f32)>> {
        self.document
            .porches
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let (plan, dim) = if self.document.active == ActiveShape::Porch(i) {
                    (&self.document.floor_plan, &self.document.dimensioning)
                } else {
                    (&slot.floor_plan, &slot.dimensioning)
                };
                plan.known_length_wall_segments(&dim.known_params)
            })
            .collect()
    }

    /// Commit a freshly-carved porch `plan` — the [`Area`] outcome of
    /// `carve_or_cut`. Validates the geometry, inherits the storey's scale,
    /// pre-marks the walls shared with the outline as known (so dimensioning
    /// only asks about the drawn walls), makes it the active shape, appends it,
    /// and starts dimensioning. Returns a user-facing message if the region
    /// self-crosses. (Mirrors `commit_living_area_plan`.)
    ///
    /// [`Area`]: manip_plan::SubareaOutcome::Area
    fn commit_porch_plan(
        &mut self,
        boundary: &FloorPlan,
        plan: FloorPlan,
        ref_walls: &[[Point2f32; 2]],
    ) -> Result<(), String> {
        let undo_snap = self.current_history_snapshot();
        log::debug!("── porch plan ───────────────────────────────────────");
        log::debug!("  → porch plan: {:?}", plan);
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
                "the porch's walls cross ({:?}) — redraw it so it doesn't fold over itself",
                e
            ));
        }
        let slot = self.pinned_subarea_slot_refs(boundary, plan, ref_walls);
        self.commit_new_porch(slot, undo_snap);
        Ok(())
    }

    /// Append `slot` as a new porch, make it active, persist, and start
    /// dimensioning it. Shared by a freehand draw and a cut-region pick
    /// (`choose_cut_region` routes here when the staged cut's flow is Porch).
    pub(super) fn commit_new_porch(&mut self, slot: ShapeSlot, undo_snap: HistorySnapshot) {
        self.document.add_active_porch(slot);
        self.porch_mode = true;
        self.pending_cut = None;
        let _ = self.mark_dirty();
        self.warn_if_non_orthogonal("draw porch");
        // Dimension the new porch's walls (no scale calibration — inherited).
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
    }

    /// UI entry point: a porch DrawPerpWalls stroke, already converted to world
    /// coords (the porch sits in the storey's space). Drops the raw canvas
    /// stroke. A single straight CUT line splits the storey into two regions and
    /// asks the user to pick one (the shared [`pending_cut_regions`] flow); any
    /// other stroke starts a new porch directly (endpoints snap to the outline;
    /// a stroke that doesn't reach it becomes a free-standing room). Mirrors
    /// `apply_living_area_stroke_event`.
    ///
    /// [`pending_cut_regions`]: Self::pending_cut_regions
    pub fn apply_porch_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        dump_stroke("porch", &pts);
        // Snap-tag capture: the pre-action state (a pending CUT pushes no undo
        // entry, so its capture carries this `before` itself), and drop stale
        // decisions from unrelated ops so only THIS stroke's snaps are tagged.
        let before = self.current_history_snapshot();
        self.begin_snap_capture();
        // Save the live buffer so the outline we carve against is current.
        self.sync_active();
        // ALL the storey's line references are live while drawing a porch:
        // pre-snap the stroke onto the interior structure lines the carve's
        // outline clip can't see — the ceiling DIVIDER cuts ("Divide ceiling")
        // and the floor/ceiling STEP (riser) lines — so a porch edge drawn a
        // touch off one lands exactly ON it. Same diagonal-scaled tolerance
        // convention as the extend/new-storey snap; ELECTIVE + tagged (the
        // moves record grouped `SnapDecision`s the capture below drains, so
        // the tag overlay can un-snap and the `Subarea` replay re-runs this
        // entry point under the forced policy). The construction-PHASE lines
        // stay on the wall-leveling path below, exactly as for living areas.
        let mut line_refs = self.ceiling_divider_snap_segments();
        line_refs.extend(self.step_line_segments());
        let pts = manip_plan::snap_stroke_to_segments(
            &pts,
            &line_refs,
            self.snap_tol_extend(),
            manip_plan::SnapKind::ToReferenceSegment,
        );
        // Reference walls the carve levels the drawn porch onto besides the
        // outline: the construction-PHASE lines (same as living areas — a wall
        // drawn beside a phase line lands flush on it and is pinned pre-known)
        // PLUS the SAME divider/step lines the stroke was just pre-snapped
        // onto, so a snapped wall levels exactly onto its line and is pinned
        // pre-known. NB declining is a TWO-round affair by design: "Don't
        // snap" on the pre-snap leaves the raw stroke off the line, but the
        // carve leveler then re-catches the wall onto the same line as a
        // fresh ToParallelWall decision (its own policy key — the tolerances
        // share one band), so the user declines that too before the wall
        // stays off the line and its dimension question reopens (pinned by
        // the two-round decline in the snap test).
        let mut ref_walls = self.subarea_reference_walls();
        ref_walls.extend(line_refs);
        // Porches never overlap each other: carve against the outline MINUS the
        // existing porches (the available region), so a new one tiles against
        // them and snaps to their shared walls.
        let holes: Vec<Vec<Point2f32>> = self
            .document
            .porches
            .iter()
            .map(|s| s.floor_plan.get_points())
            .collect();
        // One call into `document` does subtract → pick piece → clip → cut-or-carve;
        // we only dispatch the outcome to the right workflow.
        let wall_snap_tol = self.snap_tol_carve();
        match self.document.carve_or_cut_in_available(
            &pts,
            &holes,
            &self.tuning.subarea,
            &ref_walls,
            wall_snap_tol,
        ) {
            // A straight cut → offer both halves to pick from. Each region's walls
            // along the boundary / a phase line are pinned; only the cut wall is set.
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                log::debug!("── cut splits the available region into two — tap one to keep ──");
                self.pending_cut = Some((
                    SubareaFlow::Porch,
                    [
                        self.pinned_subarea_slot_refs(&boundary, a, &ref_walls),
                        self.pinned_subarea_slot_refs(&boundary, b, &ref_walls),
                    ],
                ));
                self.capture_snap_action_pending(
                    SnapAction::Subarea {
                        flow: SubareaFlow::Porch,
                        points: world_points.to_vec(),
                        pick: None,
                    },
                    before,
                    manip_plan::take_snap_decisions(),
                );
            }
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                if let Err(e) = self.commit_porch_plan(&boundary, plan, &ref_walls) {
                    log::error!("porch: {}", e);
                    // Nothing committed — drop the failed carve's decisions.
                    let _ = manip_plan::take_snap_decisions();
                } else {
                    self.capture_snap_action(SnapAction::Subarea {
                        flow: SubareaFlow::Porch,
                        points: world_points.to_vec(),
                        pick: None,
                    });
                }
            }
            Err(e) => {
                log::error!("porch carve_or_cut: {}", e);
                let _ = manip_plan::take_snap_decisions();
            }
        }
    }

    /// Delete the porch at `index` (porches are non-editable — to change one,
    /// delete and redraw). Undoable; persists. Mirrors `delete_living_area`.
    pub fn delete_porch(&mut self, index: usize) {
        if index >= self.document.porches.len() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        // Detach from any active porch before mutating the list, so the live
        // buffer + `active` index stay valid.
        self.activate_shape(ActiveShape::Outline);
        self.document.porches.remove(index);
        // Keep working on the most-recent remaining porch if still in the mode.
        if self.porch_mode && !self.document.porches.is_empty() {
            self.activate_shape(ActiveShape::Porch(self.document.porches.len() - 1));
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }
}
