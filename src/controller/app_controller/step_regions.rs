//! Floor-step area tagging: the assessor selects an area of the storey floor
//! — the SAME way a phase area is drawn (an open path that collapses onto the
//! outline, or a straight cut that splits it) — and assigns it a floor level
//! (metres from the storey datum; + raised, − lowered).
//!
//! Like phase regions, a step region is NOT persisted in the BER as a region
//! of its own. It's session-only controller state whose job is to re-split the
//! resolved floor regions at commit (`FloorRegions::with_levels`, applied in
//! `document::resolve_storey`): the durable record is `level_m` on the floor
//! regions the split produces, and on reload the session regions are rebuilt
//! from those (`document::step_regions_from_floors`).
use super::*;

impl AppController {
    /// True when the editor is tagging floor-step areas (drives the UI toggle
    /// and routes `DrawPerpWalls` strokes to step carving).
    pub fn step_mode(&self) -> bool {
        self.step_mode
    }

    /// Enter/leave step-tagging mode. Entering drops into draw mode (so a
    /// stroke carves an area straight away), acts on the storey OUTLINE, and is
    /// mutually exclusive with living-area / exposure / phase modes. Leaving
    /// clears any half-finished pick.
    pub fn set_step_mode(&mut self, on: bool) {
        self.step_mode = on;
        self.abandon_step_shape();
        if on {
            self.living_area_mode = false;
            self.exposure_mode = false;
            self.phase_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.roof_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
    }

    /// Discard the in-flight step area (if any) and return to the outline,
    /// dropping its half-dimensioned buffer (we DON'T sync it back). Used to
    /// cancel a pick or leave step mode.
    fn abandon_step_shape(&mut self) {
        self.document.step_shape = None;
        self.pending_step_cut = None;
        if self.document.active == ActiveShape::StepArea {
            self.document.active = ActiveShape::Outline;
            self.load_active_into_buffer();
            self.document.pending_arc = None;
            self.edit_drag_prompt = None;
            self.vertex_drop_prompt = None;
            self.document.edit_state = EditState::default();
            self.compute_next_dimension_prompt();
        }
    }

    /// Make a carved step area the ACTIVE shape and start dimensioning its
    /// drawn walls (mirrors `begin_phase_area_dimensioning` — a step area only
    /// re-levels the floor once its level is assigned). The outline-pinned
    /// walls are pre-known (in the slot's `known_params`), so only the walls
    /// the user actually drew are prompted.
    fn begin_step_area_dimensioning(&mut self, slot: ShapeSlot, undo_snap: HistorySnapshot) {
        self.document.begin_step_area(slot);
        self.pending_step_cut = None;
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
    }

    /// UI entry point: a step-tagging `DrawPerpWalls` stroke in world coords
    /// (the area sits in the storey's metre space). Like phase areas: a single
    /// straight CUT splits the storey into two regions to pick from; any other
    /// stroke carves a sub-area (endpoints snap to the outline). The carved ring
    /// is held pending until the user assigns a level to it.
    pub fn apply_step_region_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Paste-ready stroke dump (matches the living-area / phase-area dumps).
        dump_stroke("step-area", &pts);
        // Make the outline current, then carve against the AVAILABLE region —
        // the outline MINUS the steps already drawn. A step may overlap a phase
        // area (the floor partition composes sequentially), but never another
        // step: carving against the existing step polygons keeps the new one
        // from overlapping them and snaps it to their shared edges.
        self.sync_active();
        let idx = self.current_storey_idx.unwrap_or(0);
        let step_rings: Vec<Vec<Point2f32>> = self
            .step_regions
            .get(&idx)
            .map(|rs| rs.iter().map(|r| r.polygon.clone()).collect())
            .unwrap_or_default();
        println!("── floor-step sketch ───────────────────────────────");

        // One call does subtract → pick piece → clip → cut-or-carve against the
        // available region (outline minus the steps already drawn); we only
        // dispatch the outcome. The chosen piece comes back for wall pinning.
        match self
            .document
            .carve_or_cut_in_available(&pts, &step_rings, &self.tuning.subarea)
        {
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                println!("  → step cut splits the available region into two — tap one to keep");
                println!("────────────────────────────────────────────────────");
                let sa = self.pinned_subarea_slot(&boundary, a);
                let sb = self.pinned_subarea_slot(&boundary, b);
                self.pending_step_cut = Some([sa, sb]);
            }
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                println!("  → step-area plan: {:?}", plan);
                let bad = plan.non_orthogonal_edges();
                if bad.is_empty() {
                    println!("  → orthogonal ✓");
                } else {
                    println!("  → ⚠ NON-ORTHOGONAL edges: {:?}", bad);
                }
                println!("────────────────────────────────────────────────────");
                if let Err(e) = plan.validate_simple() {
                    eprintln!("step area rejected (walls cross): {:?}", e);
                    return;
                }
                // Become a dimensionable shape (like a phase area): the user
                // dimensions the drawn walls, THEN assigns a level. The exact
                // boundary matters — it decides which floor pieces step.
                let undo_snap = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                self.begin_step_area_dimensioning(slot, undo_snap);
            }
            Err(e) => {
                println!("────────────────────────────────────────────────────");
                eprintln!("step carve_or_cut: {}", e);
            }
        }
    }

    /// True once the in-flight step area is fully DIMENSIONED — wall lengths
    /// AND position — and awaiting its level (the UI shows the amount field).
    /// While the length loop or a position prompt is still in progress, that
    /// prompt shows instead.
    pub fn step_pick_pending(&self) -> bool {
        self.document.active == ActiveShape::StepArea
            && self.document.dimensioning.current_prompt.is_none()
            && self.document.dimensioning.current_position_prompt.is_none()
            && self.document.floor_plan.get_points().len() >= 3
    }

    /// The in-flight step area's ring (world coords) for the UI to highlight —
    /// the LIVE dimensioned buffer, so it tracks dimensioning. Empty when no
    /// step area is active.
    pub fn pending_step_polygon(&self) -> Vec<(f64, f64)> {
        if self.document.active == ActiveShape::StepArea {
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

    /// The two candidate regions of a pending step cut (world coords), for the
    /// UI to highlight while the user picks one. Empty when none pending.
    pub fn pending_step_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.pending_step_cut
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

    /// Pick the pending-cut region containing `(world_x, world_y)`; it becomes
    /// the active step area and starts dimensioning. A tap outside both cancels.
    /// No-op if no cut is pending.
    pub fn choose_step_cut_region(&mut self, world_x: f32, world_y: f32) {
        let Some([a, b]) = self.pending_step_cut.take() else {
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
            let undo_snap = self.current_history_snapshot();
            self.begin_step_area_dimensioning(slot, undo_snap);
        }
    }

    /// Assign `level_m` (metres from the storey datum; + raised, − lowered) to
    /// the just-dimensioned step area: store its DIMENSIONED ring as a session
    /// step region and re-commit, which re-splits the floor regions by it. Then
    /// return to the outline. A near-zero amount (|level| < 0.01 m) is rejected
    /// with an `Err` — the pending area stays open so the user can retype.
    pub fn assign_step_level_to_pending(&mut self, level_m: f32) -> Result<(), String> {
        if self.document.active != ActiveShape::StepArea {
            return Ok(()); // nothing awaiting a level — no-op
        }
        if !level_m.is_finite() || level_m.abs() < 0.01 {
            // A zero step is no step — keep the prompt open.
            return Err("Floor step must be at least 0.01 m.".into());
        }
        // The live buffer IS the dimensioned step area; its resolved ring is what
        // we store (metre-precise, so the floor split is accurate).
        let polygon = self.document.floor_plan.get_points();
        if polygon.len() < 3 {
            return Err("The step area has no usable outline — cancel and redraw it.".into());
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        self.step_regions.entry(idx).or_default().push(StepRegion {
            polygon,
            level_m: level_m as f64,
        });
        self.abandon_step_shape(); // discard the live buffer, return to outline
        self.mark_dirty()
    }

    /// Discard the in-flight step area / pending cut without assigning a level.
    pub fn cancel_pending_step(&mut self) {
        self.abandon_step_shape();
    }

    /// The current storey's committed step regions, as `(ring, label)` in world
    /// coords, for the UI to tint and label (label e.g. "−0.30 m" / "+0.15 m").
    /// Index-aligned with deletion (see
    /// [`delete_step_region`](Self::delete_step_region)). Empty when none.
    pub fn step_region_polygons(&self) -> Vec<(Vec<(f64, f64)>, String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        self.step_regions
            .get(&idx)
            .map(|regions| {
                regions
                    .iter()
                    .map(|r| {
                        let ring = r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                        (ring, format!("{:+.2} m", r.level_m))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Delete the current storey's step region at `index` and re-commit (the
    /// floor pieces it had stepped fall back to the datum level on the next
    /// derive). No-op if out of range.
    pub fn delete_step_region(&mut self, index: usize) {
        let idx = self.current_storey_idx.unwrap_or(0);
        let removed = self.step_regions.get_mut(&idx).is_some_and(|regions| {
            if index < regions.len() {
                regions.remove(index);
                true
            } else {
                false
            }
        });
        if removed {
            let _ = self.mark_dirty();
        }
    }
}
