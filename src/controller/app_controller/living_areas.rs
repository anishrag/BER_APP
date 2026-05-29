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
        let slot = ShapeSlot {
            floor_plan: self.floor_plan.clone(),
            dimensioning: self.dimensioning.clone(),
        };
        match self.active {
            ActiveShape::Outline => self.outline = slot,
            ActiveShape::LivingArea(i) => {
                if let Some(s) = self.living_areas.get_mut(i) {
                    *s = slot;
                }
            }
        }
    }

    /// Load the active shape's slot into the live buffer.
    fn load_active_into_buffer(&mut self) {
        let slot = match self.active {
            ActiveShape::Outline => self.outline.clone(),
            ActiveShape::LivingArea(i) => self.living_areas.get(i).cloned().unwrap_or_default(),
        };
        self.floor_plan = slot.floor_plan;
        self.dimensioning = slot.dimensioning;
    }

    /// Switch which shape the editor acts on: save the current live buffer to
    /// its slot, then load the target. Transient per-shape state (the active
    /// dimension prompt is in `dimensioning`; provisional arc + edit/drag
    /// prompts are abandoned on a switch).
    pub(super) fn activate_shape(&mut self, target: ActiveShape) {
        if self.active == target {
            return;
        }
        self.sync_active();
        self.active = target;
        self.load_active_into_buffer();
        self.pending_arc = None;
        self.pending_cut = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        self.edit_state = EditState::default();
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
            // Drop straight into draw mode so a stroke makes a new living area
            // without the user also having to pick the tool.
            self.set_edit_mode(EditTools::DrawPerpWalls);
            if !self.living_areas.is_empty() {
                self.activate_shape(ActiveShape::LivingArea(self.living_areas.len() - 1));
            }
            // else: nothing to edit yet — the next DrawPerpWalls creates the
            // first living area (the outline stays active until then).
        } else {
            self.activate_shape(ActiveShape::Outline);
        }
    }

    /// Number of living areas drawn so far.
    pub fn living_area_count(&self) -> usize {
        self.living_areas.len()
    }

    /// Index of the living area the tools currently act on, if any.
    pub fn active_living_area_index(&self) -> Option<usize> {
        match self.active {
            ActiveShape::LivingArea(i) => Some(i),
            ActiveShape::Outline => None,
        }
    }

    /// Resolved outlines (world coords) of every living area, for rendering
    /// them over the storey in a distinct colour. Read from the in-memory
    /// `Storey.living_areas` (the `Ber` struct in `current_record`): BER data
    /// rendered from BER. `commit_to_ber` keeps it per-frame current.
    pub fn living_area_polygons(&self) -> Vec<Vec<(f64, f64)>> {
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.first())
        else {
            return Vec::new();
        };
        storey
            .living_areas
            .to_vertex_lists()
            .into_iter()
            .map(|verts| verts.into_iter().map(|p| (p.x, p.y)).collect())
            .collect()
    }

    /// Per living area, the walls whose length is known — as
    /// `(start, end, length)` in world coords — so the UI can label them the
    /// same way the storey's dimensioned walls are labelled. The active area is
    /// read from the live buffer + its dimensioning; the others from their
    /// slots. Index-aligned with [`living_area_polygons`](Self::living_area_polygons).
    pub fn living_area_wall_labels(&self) -> Vec<Vec<((f64, f64), (f64, f64), f32)>> {
        self.living_areas
            .iter()
            .enumerate()
            .map(|(i, slot)| {
                let (plan, dim) = if self.active == ActiveShape::LivingArea(i) {
                    (&self.floor_plan, &self.dimensioning)
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

    /// Start a NEW living area from a freehand stroke (world coords), drawn the
    /// same way as the storey outline. The stroke is an OPEN path: its
    /// endpoints snap onto the storey outline and the area is closed along the
    /// shared house walls (so they aren't redrawn). A stroke that doesn't reach
    /// the outline becomes a free-standing interior room instead. It inherits
    /// the storey's scale (a living area inside a dimensioned storey is already
    /// in metres — no re-calibration; dimensioning only sets its walls),
    /// becomes the active shape, and is appended to the list. Then prompts to
    /// dimension its walls.
    pub fn start_new_living_area(&mut self, world_points: &[Point2f32]) -> Result<(), String> {
        if world_points.len() < 3 {
            return Err("living-area stroke needs at least 3 points".into());
        }
        let undo_snap = self.current_history_snapshot();
        // RDP tolerance scaled to the drawn extent, so it's sensible whether the
        // storey is in metres (dimensioned) or still pixels.
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in world_points {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        let diag = ((max_x - min_x).powi(2) + (max_y - min_y).powi(2)).sqrt();
        let tol = (diag * 0.02).max(1e-3);

        // Save whatever's active first, so the outline we carve against is
        // current (and the active living area's edits aren't lost).
        self.sync_active();
        // Diagnostic logging for living-area sketches (paste this when a sketch
        // comes out wrong): the raw stroke, the outline we snap/carve against,
        // and the resulting parametric plan.
        println!("── living-area sketch ──────────────────────────────");
        println!(
            "  drawn: {} pts  extent x[{:.2}, {:.2}] y[{:.2}, {:.2}]  tol={:.3}",
            world_points.len(),
            min_x,
            max_x,
            min_y,
            max_y,
            tol
        );
        println!("  stroke pts: {:?}", world_points);
        println!("  outline ring: {:?}", self.outline.floor_plan.get_points());
        let plan = self
            .outline
            .floor_plan
            .carve_subarea(world_points, tol)
            .map_err(|e| format!("Failed to draw living area: {:?}", e))?;
        println!("  → living-area plan: {:?}", plan);
        let bad = plan.non_orthogonal_edges();
        if bad.is_empty() {
            println!("  → orthogonal ✓");
        } else {
            println!("  → ⚠ NON-ORTHOGONAL edges: {:?}", bad);
        }
        println!("────────────────────────────────────────────────────");
        // Fallback guardrail: the geometry layer un-intersects a folded ring
        // (`untangle_ring`), so this should rarely fire — but if a self-crossing
        // somehow survives, refuse it rather than commit bad geometry.
        if let Err(e) = plan.validate_simple() {
            return Err(format!(
                "the living area's walls cross ({:?}) — redraw it so it doesn't fold over itself",
                e
            ));
        }
        let slot = self.living_area_slot(plan);
        self.commit_new_living_area(slot, undo_snap);
        Ok(())
    }

    /// Build a `ShapeSlot` for a freshly-constructed living-area plan: inherit
    /// the storey's scale, and pre-mark as KNOWN the parameters pinned to the
    /// outline (walls snapped onto the storey) — so dimensioning only asks
    /// about the walls the user actually drew.
    fn living_area_slot(&self, plan: FloorPlan) -> ShapeSlot {
        let pinned = self.outline.floor_plan.params_pinned_to_outline(&plan);
        if !pinned.is_empty() {
            println!("  pinned-to-outline params (pre-known): {:?}", pinned);
        }
        ShapeSlot {
            dimensioning: DimensionState {
                scale_set: self.outline.dimensioning.scale_set,
                known_params: pinned,
                ..Default::default()
            },
            floor_plan: plan,
        }
    }

    /// Append `slot` as a new living area, make it active, persist, and start
    /// dimensioning it. Shared by a freehand draw and a cut-region pick.
    fn commit_new_living_area(&mut self, slot: ShapeSlot, undo_snap: HistorySnapshot) {
        self.living_areas.push(slot);
        let idx = self.living_areas.len() - 1;
        self.active = ActiveShape::LivingArea(idx);
        self.living_area_mode = true;
        self.pending_cut = None;
        self.load_active_into_buffer();
        self.pending_arc = None;
        self.edit_state = EditState::default();
        let _ = self.commit_to_ber();
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
        // Save the live buffer so the outline we clamp/split/carve against is
        // current.
        self.sync_active();
        // Guardrail FIRST: snap any part of the stroke drawn OUTSIDE the storey
        // back onto its boundary, before anything else looks at it.
        let pts = self.outline.floor_plan.clamp_inside(&pts);

        // Tolerance scaled to the drawn extent (matches start_new_living_area).
        let (mut mnx, mut mny, mut mxx, mut mxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in &pts {
            mnx = mnx.min(p.x);
            mny = mny.min(p.y);
            mxx = mxx.max(p.x);
            mxy = mxy.max(p.y);
        }
        let tol = (((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt() * 0.02).max(1e-3);

        // A straight cut → offer both halves to pick from. Each region's walls
        // along the storey are pinned (pre-known); only the cut wall is set.
        match self.outline.floor_plan.split_by_cut(&pts, tol) {
            Ok(Some((a, b))) => {
                println!("── cut splits the storey into two regions — tap one to keep ──");
                self.pending_cut = Some([self.living_area_slot(a), self.living_area_slot(b)]);
                return;
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("split_by_cut: {:?}", e);
            }
        }
        if let Err(e) = self.start_new_living_area(&pts) {
            eprintln!("start_new_living_area: {}", e);
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
                eprintln!("cut region rejected (walls cross): {:?}", e);
                return;
            }
            let undo_snap = self.current_history_snapshot();
            self.commit_new_living_area(slot, undo_snap);
        }
    }

    /// Discard a pending cut without creating anything.
    pub fn cancel_pending_cut(&mut self) {
        self.pending_cut = None;
    }

    /// Delete the living area at `index` (living areas are non-editable — to
    /// change one, delete and redraw). Undoable; persists.
    pub fn delete_living_area(&mut self, index: usize) {
        if index >= self.living_areas.len() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        // Detach from any active living area before mutating the list, so the
        // live buffer + `active` index stay valid.
        self.activate_shape(ActiveShape::Outline);
        self.living_areas.remove(index);
        // Keep working on the most-recent remaining area if still in LA mode.
        if self.living_area_mode && !self.living_areas.is_empty() {
            self.activate_shape(ActiveShape::LivingArea(self.living_areas.len() - 1));
        }
        let _ = self.commit_to_ber();
        self.push_history_snapshot(undo_snap);
    }
}

/// A shape is finalised — nothing left to dimension — when it has no wall the
/// dimensioning loop would still prompt for. (The caller also checks the metre
/// scale for the outline; living areas inherit it.)
pub(super) fn shape_finalised(dim: &DimensionState, plan: &FloorPlan) -> bool {
    plan.next_promptable_wall(&dim.known_params, &dim.skipped_walls)
        .is_none()
}

/// Rebuild the runtime living-area slots from their resolved polygons on load.
/// Living areas are non-editable, so each is reconstructed from exact points
/// (`from_clean_ring`) with a COMPLETE dimensioning state (metre scale already
/// set, all params known) — they render and can be deleted, but aren't
/// re-dimensioned. Degenerate (<3-vertex) polygons are skipped.
pub(super) fn reconstruct_living_areas(areas: &FloorRegions) -> Vec<ShapeSlot> {
    areas
        .to_vertex_lists()
        .into_iter()
        .filter(|verts| verts.len() >= 3)
        .filter_map(|verts| {
            let pts: Vec<Point2f32> = verts
                .iter()
                .map(|p| Point2f32::new(p.x as f32, p.y as f32))
                .collect();
            let plan = FloorPlan::from_clean_ring(&pts).ok()?;
            let known_params = plan.all_active_params();
            Some(ShapeSlot {
                floor_plan: plan,
                dimensioning: DimensionState {
                    known_params,
                    scale_set: true,
                    dimensioned_walls: Vec::new(),
                    skipped_walls: HashSet::new(),
                    current_prompt: None,
                },
            })
        })
        .collect()
}
