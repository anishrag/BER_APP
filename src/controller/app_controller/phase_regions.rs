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
        self.pending_phase_polygon = None;
        self.pending_phase_cut = None;
        if on {
            self.living_area_mode = false;
            self.exposure_mode = false;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
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
        // Make the outline we carve/split against current.
        self.sync_active();
        let pts = self.outline.floor_plan.clamp_inside(&pts);
        let tol = (manip_2d_plan::ring_diagonal(&pts) * 0.02).max(1e-3);

        match self.outline.floor_plan.split_by_cut(&pts, tol) {
            Ok(Some((a, b))) => {
                self.pending_phase_cut = Some([a.get_points(), b.get_points()]);
                return;
            }
            Ok(None) => {}
            Err(e) => eprintln!("phase split_by_cut: {:?}", e),
        }
        match self.outline.floor_plan.carve_subarea(&pts, tol) {
            Ok(plan) => {
                if let Err(e) = plan.validate_simple() {
                    eprintln!("phase area rejected (walls cross): {:?}", e);
                    return;
                }
                // `carve_subarea` now follows the boundary (open-path splice or
                // the two-pass boundary-aware loop), so the area is already inside
                // the storey — no clip needed.
                self.pending_phase_polygon = Some(plan.get_points());
            }
            Err(e) => eprintln!("phase carve_subarea: {:?}", e),
        }
    }

    /// True while a carved area is awaiting its phase pick (the UI shows the
    /// phase picker, populated from [`construction_phases`](Self::construction_phases)).
    pub fn phase_pick_pending(&self) -> bool {
        self.pending_phase_polygon.is_some()
    }

    /// The pending area's ring (world coords) for the UI to highlight while the
    /// user picks a phase. Empty when nothing is pending.
    pub fn pending_phase_polygon(&self) -> Vec<(f64, f64)> {
        self.pending_phase_polygon
            .as_ref()
            .map(|ring| ring.iter().map(|p| (p.x as f64, p.y as f64)).collect())
            .unwrap_or_default()
    }

    /// The two candidate regions of a pending phase cut (world coords), for the
    /// UI to highlight while the user picks one. Empty when none pending.
    pub fn pending_phase_cut_regions(&self) -> Vec<Vec<(f64, f64)>> {
        self.pending_phase_cut
            .as_ref()
            .map(|regions| {
                regions
                    .iter()
                    .map(|ring| ring.iter().map(|p| (p.x as f64, p.y as f64)).collect())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Pick the pending-cut region containing `(world_x, world_y)`; it becomes
    /// the pending area awaiting a phase. A tap outside both cancels. No-op if
    /// no cut is pending.
    pub fn choose_phase_cut_region(&mut self, world_x: f32, world_y: f32) {
        let Some([a, b]) = self.pending_phase_cut.take() else {
            return;
        };
        let chosen = if manip_2d_plan::ring_contains_point(&a, world_x, world_y) {
            Some(a)
        } else if manip_2d_plan::ring_contains_point(&b, world_x, world_y) {
            Some(b)
        } else {
            None // tapped outside both → cancel
        };
        self.pending_phase_polygon = chosen;
    }

    /// Assign `phase` to the pending area: store it as a session phase region for
    /// the current storey and re-commit, which re-resolves every surface's phase
    /// (the new polygon stamps anything inside it, retroactively included).
    pub fn assign_phase_to_pending(&mut self, phase: ConstructionPhase) {
        let Some(polygon) = self.pending_phase_polygon.take() else {
            return;
        };
        let idx = self.current_storey_idx.unwrap_or(0);
        self.phase_regions
            .entry(idx)
            .or_default()
            .push(PhaseRegion { polygon, phase });
        let _ = self.commit_to_ber();
    }

    /// Discard a half-finished phase pick (carved area or cut) without tagging.
    pub fn cancel_pending_phase(&mut self) {
        self.pending_phase_polygon = None;
        self.pending_phase_cut = None;
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
            let _ = self.commit_to_ber();
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
