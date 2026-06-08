//! Construction-phase area tagging: the assessor selects an area of the storey
//! — the SAME way a living area is drawn (an open path that collapses onto the
//! outline, or a straight cut that splits it) — and assigns it a construction
//! phase (= year of construction, chosen from the dates entered up front).
//!
//! Unlike living areas, a phase region is NOT persisted in the BER. It's
//! session-only controller state whose job is to stamp the phase onto fabric
//! surfaces at commit (`resolve_storey_phases` in `mod.rs`): every surface whose
//! representative point falls inside the polygon takes its phase. The durable
//! record of a surface's phase is its `construction_id` — so a region keeps
//! stamping anything new drawn in its footprint during the session, and reload
//! (no polygons) leaves already-tagged surfaces untouched.
use super::*;

/// A session-only tagged area: a world-space (metre) ring + the phase to stamp
/// onto surfaces inside it. Keyed per storey in `AppController::phase_regions`.
#[derive(Debug, Clone)]
pub(super) struct PhaseRegion {
    pub polygon: Vec<Point2f32>,
    pub phase: ConstructionPhase,
}

impl AppController {
    /// True when the editor is tagging construction-phase areas (drives the UI
    /// toggle and routes `DrawPerpWalls` strokes to phase carving).
    pub fn phase_mode(&self) -> bool {
        self.phase_mode
    }

    /// Enter/leave phase-tagging mode. Entering drops into draw mode (so a
    /// stroke carves an area straight away), acts on the storey OUTLINE, and is
    /// mutually exclusive with living-area / exposure modes. Leaving clears any
    /// half-finished pick.
    pub fn set_phase_mode(&mut self, on: bool) {
        self.phase_mode = on;
        self.abandon_phase_shape();
        if on {
            self.living_area_mode = false;
            self.exposure_mode = false;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
    }

    /// Discard the in-flight phase area (if any) and return to the outline,
    /// dropping its half-dimensioned buffer (we DON'T sync it back). Used to
    /// cancel a pick or leave phase mode.
    fn abandon_phase_shape(&mut self) {
        self.document.phase_shape = None;
        self.pending_phase_cut = None;
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

    /// Make a carved phase area the ACTIVE shape and start dimensioning its
    /// drawn walls (mirrors `commit_new_living_area`, minus the persist — a phase
    /// area only stamps surfaces once its phase is assigned). The outline-pinned
    /// walls are pre-known (in the slot's `known_params`), so only the walls the
    /// user actually drew are prompted.
    fn begin_phase_area_dimensioning(&mut self, slot: ShapeSlot, undo_snap: HistorySnapshot) {
        self.document.begin_phase_area(slot);
        self.pending_phase_cut = None;
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
    }

    /// UI entry point: a phase-tagging `DrawPerpWalls` stroke in world coords
    /// (the area sits in the storey's metre space). Like living areas: a single
    /// straight CUT splits the storey into two regions to pick from; any other
    /// stroke carves a sub-area (endpoints snap to the outline). The carved ring
    /// is held pending until the user picks a phase for it.
    pub fn apply_phase_region_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Paste-ready stroke dump (matches the living-area / curve-debug dumps).
        dump_stroke("phase-area", &pts);
        // Make the outline current, then carve against the AVAILABLE region —
        // the outline MINUS the phases already drawn. Carving against it makes the
        // new phase snap to the neighbours' shared edges (and the outline) and
        // keeps it from overlapping them, so the phases tile with no gaps and the
        // shared walls land on the same coordinates (pre-known, not re-dimensioned).
        self.sync_active();
        let idx = self.current_storey_idx.unwrap_or(0);
        let phase_rings: Vec<Vec<Point2f32>> = self
            .phase_regions
            .get(&idx)
            .map(|rs| rs.iter().map(|r| r.polygon.clone()).collect())
            .unwrap_or_default();
        println!("── construction-phase sketch ───────────────────────");

        // One call does subtract → pick piece → clip → cut-or-carve against the
        // available region (outline minus the phases already drawn); we only
        // dispatch the outcome. The chosen piece comes back for wall pinning.
        match self
            .document
            .carve_or_cut_in_available(&pts, &phase_rings, &self.tuning.subarea)
        {
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                println!("  → phase cut splits the available region into two — tap one to keep");
                println!("────────────────────────────────────────────────────");
                let sa = self.pinned_subarea_slot(&boundary, a);
                let sb = self.pinned_subarea_slot(&boundary, b);
                self.pending_phase_cut = Some([sa, sb]);
            }
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                println!("  → phase-area plan: {:?}", plan);
                let bad = plan.non_orthogonal_edges();
                if bad.is_empty() {
                    println!("  → orthogonal ✓");
                } else {
                    println!("  → ⚠ NON-ORTHOGONAL edges: {:?}", bad);
                }
                println!("────────────────────────────────────────────────────");
                if let Err(e) = plan.validate_simple() {
                    eprintln!("phase area rejected (walls cross): {:?}", e);
                    return;
                }
                // Become a dimensionable shape (like a living area): the user
                // dimensions the drawn walls, THEN picks a phase. The exact
                // boundary matters — it decides which surfaces are in the phase.
                let undo_snap = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                self.begin_phase_area_dimensioning(slot, undo_snap);
            }
            Err(e) => {
                println!("────────────────────────────────────────────────────");
                eprintln!("phase carve_or_cut: {}", e);
            }
        }
    }

    /// True once the in-flight phase area is fully DIMENSIONED and awaiting its
    /// phase pick (the UI shows the phase picker). While dimensioning is still in
    /// progress the dimension prompt shows instead.
    pub fn phase_pick_pending(&self) -> bool {
        self.document.active == ActiveShape::PhaseArea
            && self.document.dimensioning.current_prompt.is_none()
            && self.document.floor_plan.get_points().len() >= 3
    }

    /// The in-flight phase area's ring (world coords) for the UI to highlight —
    /// the LIVE dimensioned buffer, so it tracks dimensioning. Empty when no
    /// phase area is active.
    pub fn pending_phase_polygon(&self) -> Vec<(f64, f64)> {
        if self.document.active == ActiveShape::PhaseArea {
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

    /// The two candidate regions of a pending phase cut (world coords), for the
    /// UI to highlight while the user picks one. Empty when none pending.
    pub fn pending_phase_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.pending_phase_cut
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
    /// the active phase area and starts dimensioning. A tap outside both cancels.
    /// No-op if no cut is pending.
    pub fn choose_phase_cut_region(&mut self, world_x: f32, world_y: f32) {
        let Some([a, b]) = self.pending_phase_cut.take() else {
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
            self.begin_phase_area_dimensioning(slot, undo_snap);
        }
    }

    /// Assign `phase` to the just-dimensioned phase area: store its DIMENSIONED
    /// ring as a session phase region and re-commit, which re-resolves every
    /// surface's phase (the new polygon stamps anything inside it). Then return to
    /// the outline.
    pub fn assign_phase_to_pending(&mut self, phase: ConstructionPhase) {
        if self.document.active != ActiveShape::PhaseArea {
            return;
        }
        // The live buffer IS the dimensioned phase area; its resolved ring is what
        // we store (metre-precise, so surface containment is accurate).
        let polygon = self.document.floor_plan.get_points();
        if polygon.len() < 3 {
            return;
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        self.phase_regions
            .entry(idx)
            .or_default()
            .push(PhaseRegion { polygon, phase });
        self.abandon_phase_shape(); // discard the live buffer, return to outline
        let _ = self.mark_dirty();
    }

    /// Discard the in-flight phase area / pending cut without tagging.
    pub fn cancel_pending_phase(&mut self) {
        self.abandon_phase_shape();
    }

    /// The current storey's committed phase regions, as `(ring, label)` in world
    /// coords, for the UI to tint and label. Index-aligned with deletion (see
    /// [`delete_phase_region`](Self::delete_phase_region)). Empty when none.
    pub fn phase_region_polygons(&self) -> Vec<(Vec<(f64, f64)>, String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let phases = self
            .current_record
            .as_ref()
            .map(|r| &r.data.construction_phases);
        self.phase_regions
            .get(&idx)
            .map(|regions| {
                regions
                    .iter()
                    .map(|r| {
                        let ring = r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                        (ring, phase_label(&r.phase, phases))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The phases the assessor may assign, as `(phase, label)` — the construction
    /// dates entered up front (label includes the year). Drives the phase picker.
    pub fn construction_phase_choices(&self) -> Vec<(ConstructionPhase, String)> {
        let Some(phases) = self
            .current_record
            .as_ref()
            .map(|r| &r.data.construction_phases)
        else {
            return Vec::new();
        };
        phases
            .defined_phases()
            .into_iter()
            .map(|p| {
                let label = phase_label(&p, Some(phases));
                (p, label)
            })
            .collect()
    }

    /// Delete the current storey's phase region at `index` and re-commit (so
    /// surfaces it had stamped fall back to their existing phase — see the
    /// preserve-existing rule in `resolve_storey_phases`). No-op if out of range.
    pub fn delete_phase_region(&mut self, index: usize) {
        let idx = self.current_storey_idx.unwrap_or(0);
        let removed = self.phase_regions.get_mut(&idx).is_some_and(|regions| {
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

/// The construction phase to stamp on a surface at `(x, y)` (storey metre
/// space), given its `existing` phase. The agreed rule: a phase polygon
/// containing the point WINS (latest tag first); else preserve a non-`Original`
/// existing phase; else `Original`. `phase_polys` is the storey's session areas
/// as `(ring, phase)`.
pub(super) fn resolved_phase(
    phase_polys: &[(Vec<Point2f32>, ConstructionPhase)],
    x: f32,
    y: f32,
    existing: ConstructionPhase,
) -> ConstructionPhase {
    if let Some((_, phase)) = phase_polys
        .iter()
        .rev()
        .find(|(ring, _)| manip_2d_plan::ring_contains_point(ring, x, y))
    {
        phase.clone()
    } else if existing != ConstructionPhase::Original {
        existing
    } else {
        ConstructionPhase::Original
    }
}

/// A human label for a phase: the user's nickname if given (e.g. "Back
/// extension"), else the default name ("Original", "Extension 1"), with the
/// year appended when known — e.g. "Back extension (2008)", "Original (1985)".
fn phase_label(phase: &ConstructionPhase, phases: Option<&ConstructionPhases>) -> String {
    let name = phases
        .and_then(|p| p.nickname_of(phase))
        .map(|s| s.to_string())
        .unwrap_or_else(|| match phase {
            ConstructionPhase::Original => "Original".to_string(),
            ConstructionPhase::Extension1 => "Extension 1".to_string(),
            ConstructionPhase::Extension2 => "Extension 2".to_string(),
            ConstructionPhase::Extension3 => "Extension 3".to_string(),
            ConstructionPhase::Extension4 => "Extension 4".to_string(),
            ConstructionPhase::Other(s) => s.clone(),
        });
    match phases.and_then(|p| p.year_of(phase)) {
        Some(year) => format!("{name} ({year})"),
        None => name,
    }
}
