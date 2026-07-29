//! Construction-phase area tagging: the assessor selects an area of the storey
//! — the SAME way a living area is drawn (an open path that collapses onto the
//! outline, or a straight cut that splits it) — and assigns it a construction
//! phase (= year of construction, chosen from the dates entered up front).
//!
//! Unlike living areas, a phase region is NOT persisted in the BER. It's
//! session-only controller state whose job is to stamp the phase onto fabric
//! surfaces at commit (`document::stamp_wall_phases`): every surface whose
//! representative point falls inside the polygon takes its phase. The durable
//! record of a surface's phase is its `construction_id` — so a region keeps
//! stamping anything new drawn in its footprint during the session, and reload
//! (no polygons) leaves already-tagged surfaces untouched.
use super::*;

// The display-region type itself lives in `domain::view` (both this
// controller and the `ber_render` drawing crate need it); re-exported here so
// controller/UI paths keep working.
pub use domain::view::PhaseDisplayRegion;

impl AppController {
    /// True when the editor is tagging construction-phase areas (drives the UI
    /// toggle and routes `DrawPerpWalls` strokes to phase carving).
    pub fn phase_mode(&self) -> bool {
        self.phase_mode
    }

    /// Enter/leave phase-tagging mode. Entering drops into draw mode (so a
    /// stroke carves an area straight away), acts on the storey OUTLINE, and is
    /// mutually exclusive with living-area / exposure / step modes. Leaving
    /// clears any half-finished pick.
    pub fn set_phase_mode(&mut self, on: bool) {
        // Leaving the outline draw tool: drop any un-decided re-draw stroke
        // (a stray second outline) so it doesn't linger on the canvas. (n3)
        self.pending_redraw = None;
        self.phase_mode = on;
        self.abandon_phase_shape();
        if on {
            self.exit_openings_mode();
            self.living_area_mode = false;
            self.porch_mode = false;
            // The raw flag writes above skip the setters' own cleanup, and the
            // `activate_shape(Outline)` below early-returns when the outline is
            // already active — clear the living-area/porch staged cut-pick
            // explicitly so it can't linger under phase mode (its pick would
            // commit a sub-area the assessor no longer sees staged).
            self.pending_cut = None;
            self.exposure_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.ceiling_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            self.dormer_mode = false;
            self.pending_rect_is_dormer = false;
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

    /// The current storey's construction-phase region edges as world-metre
    /// reference walls — the segments a carve (living area, ceiling step, …) can
    /// level onto and be pinned to, so a wall drawn beside a phase "year" line
    /// lands on it. Empty when the storey has no phase regions. Shared by every
    /// sub-area carve that opts into phase-line snapping.
    /// EVERY region boundary on the current storey a drawn sub-area may snap
    /// onto: the construction-PHASE lines
    /// ([`phase_reference_walls`](Self::phase_reference_walls)) plus the
    /// resolved FLOOR- and CEILING-region edges.
    ///
    /// The rule this encodes: **a stroke should snap to any line the assessor
    /// can see on the plan.** Floor steps, ceiling sections and phase
    /// boundaries are all drawn on the canvas, and an area drawn along one is
    /// meant to share that line — they're the same wall in the building.
    ///
    /// Each carve used to assemble its own set ad hoc and they had drifted
    /// badly apart: living areas and phases got phase lines only, porches got
    /// phase + divider lines, and the FLOOR-step carve passed `&[]` — it could
    /// not snap to anything at all. So whether a drawn edge landed on a visible
    /// line depended on which tool you happened to be holding, with nothing to
    /// explain the difference (field reports: a ceiling section over a dropped
    /// floor, then a living area over both).
    ///
    /// The carve boundary itself is NOT included — that comes from
    /// `carve_or_cut_in_available`'s available region, which already supplies
    /// the outline and any same-kind neighbours. Overlap between the two is
    /// harmless: levelling a wall onto the same line twice is idempotent.
    pub(super) fn subarea_reference_walls(&self) -> Vec<[Point2f32; 2]> {
        let mut refs = self.phase_reference_walls();
        let idx = self.current_storey_idx.unwrap_or(0);
        if let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        {
            refs.extend(document::floor_region_edges(&storey.floors));
            refs.extend(document::ceiling_region_edges(&storey.ceilings));
        }
        refs
    }

    pub(super) fn phase_reference_walls(&self) -> Vec<[Point2f32; 2]> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(regions) = self.phase_regions.get(&idx) else {
            return Vec::new();
        };
        let mut segments: Vec<[Point2f32; 2]> = Vec::new();
        for region in regions {
            // The phase ring is f64; snap references are f32 canvas-space.
            let ring: Vec<Point2f32> = region.polygon.iter().copied().map(to_f32).collect();
            let n = ring.len();
            for i in 0..n {
                segments.push([ring[i], ring[(i + 1) % n]]);
            }
        }
        segments
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
        // Snap-tag capture: the pre-action state (a pending CUT pushes no undo
        // entry, so its capture carries this `before` itself), and drop stale
        // decisions from unrelated ops so only THIS stroke's snaps are tagged.
        let before = self.current_history_snapshot();
        self.begin_snap_capture();
        // Make the outline current, then carve against the OUTLINE ONLY (no phase
        // holes) — a drawn phase is ALLOWED to overlap the phases already tagged.
        // The resolve's latest-wins partition (`document::resolve`) then removes
        // the overlap from the older phase (shrinking it to a donut), so the new
        // phase claims its full footprint. Snapping to neighbours' shared edges
        // still happens via `snap_refs` below (the phase-edge/step reference
        // walls), so a boundary drawn beside an existing phase line lands flush on
        // it — the carve and the snap are orthogonal. Mirrors the STRUCTURE-phase
        // carve, which also passes `&[]` for the holes.
        self.sync_active();
        // Reference walls the carve levels the drawn phase boundary onto (in
        // addition to the outline): floor + ceiling STEP lines AND the edges of
        // the construction-phase regions already drawn (c4a2c3fc) — so a phase
        // boundary drawn beside an existing step OR phase edge lands flush on it
        // (like the wall-exposure tool and the living-area carve). The SAME set
        // is passed to the carve (`carve_or_cut_in_available`) and to the pin
        // (`pinned_subarea_slot_refs`) so a snapped wall is pre-known and isn't
        // re-prompted.
        let mut snap_refs = self.step_line_segments();
        snap_refs.extend(self.phase_reference_walls());
        log::debug!("── construction-phase sketch ───────────────────────");

        // One call does subtract → pick piece → cut-or-carve against the OUTLINE
        // (no holes → overlap with existing phases is allowed; latest-wins in
        // resolve settles it); we only dispatch the outcome. The chosen piece
        // comes back for wall pinning.
        let wall_snap_tol = self.snap_tol_carve();
        match self.document.carve_or_cut_in_available(
            &pts,
            &[],
            &self.tuning.subarea,
            &snap_refs,
            wall_snap_tol,
        ) {
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                log::debug!("  → phase cut splits the available region into two — tap one to keep");
                log::debug!("────────────────────────────────────────────────────");
                let sa = self.pinned_subarea_slot_refs(&boundary, a, &snap_refs);
                let sb = self.pinned_subarea_slot_refs(&boundary, b, &snap_refs);
                self.pending_phase_cut = Some([sa, sb]);
                self.capture_snap_action_pending(
                    SnapAction::Subarea {
                        flow: SubareaFlow::PhaseRegion,
                        points: world_points.to_vec(),
                        pick: None,
                    },
                    before,
                    manip_plan::take_snap_decisions(),
                );
            }
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                log::debug!("  → phase-area plan: {:?}", plan);
                let bad = plan.non_orthogonal_edges();
                if bad.is_empty() {
                    log::debug!("  → orthogonal ✓");
                } else {
                    log::debug!("  → ⚠ NON-ORTHOGONAL edges: {:?}", bad);
                }
                log::debug!("────────────────────────────────────────────────────");
                if let Err(e) = plan.validate_simple() {
                    log::warn!("phase area rejected (walls cross): {:?}", e);
                    // Nothing committed — drop the rejected carve's decisions.
                    let _ = manip_plan::take_snap_decisions();
                    return;
                }
                // Become a dimensionable shape (like a living area): the user
                // dimensions the drawn walls, THEN picks a phase. The exact
                // boundary matters — it decides which surfaces are in the phase.
                let undo_snap = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot_refs(&boundary, plan, &snap_refs);
                self.begin_phase_area_dimensioning(slot, undo_snap);
                self.capture_snap_action(SnapAction::Subarea {
                    flow: SubareaFlow::PhaseRegion,
                    points: world_points.to_vec(),
                    pick: None,
                });
            }
            Err(e) => {
                log::debug!("────────────────────────────────────────────────────");
                log::error!("phase carve_or_cut: {}", e);
                let _ = manip_plan::take_snap_decisions();
            }
        }
    }

    /// True once the in-flight phase area is fully DIMENSIONED — wall lengths
    /// AND position — and awaiting its phase pick (the UI shows the phase
    /// picker). While the length loop or a position prompt is still in
    /// progress, that prompt shows instead.
    pub fn phase_pick_pending(&self) -> bool {
        self.document.active == ActiveShape::PhaseArea
            && self.document.dimensioning.current_prompt.is_none()
            && self.document.dimensioning.current_position_prompt.is_none()
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
            // The stroke's tags stay live: replay now re-runs stroke + pick.
            self.record_subarea_pick(SubareaFlow::PhaseRegion, Some((world_x, world_y)));
        } else {
            // Cancelled — the cut (and the snaps its tags describe) is gone.
            self.record_subarea_pick(SubareaFlow::PhaseRegion, None);
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
        // we store (metre-precise, so surface containment is accurate). Widened
        // to f64 to match the reconstructed/inherited zones' precision.
        let polygon: Vec<Point2f64> = self
            .document
            .floor_plan
            .get_points()
            .into_iter()
            .map(to_f64)
            .collect();
        if polygon.len() < 3 {
            return;
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        self.phase_regions
            .entry(idx)
            .or_default()
            .push(PhaseRegion { polygon, phase });
        self.abandon_phase_shape(); // discard the live buffer, return to outline
        self.canonicalize_phase_regions_after_assign(idx);
    }

    /// Canonicalize the session phase list right after a tag lands: commit
    /// eagerly (the resolve partitions the floors — with same-construction
    /// pieces re-unioned), then rebuild the session zones from the resolved
    /// floors (`reconstruct_phase_regions_for`) — the SAME canonical form a
    /// reload produces. Without this the raw list keeps carrying a redundant
    /// same-phase entry (e.g. "assign the rest to Original" beside an
    /// already-Original area, bug c78ebed1) that re-seeds a boundary between
    /// two same-phase zones into every later snap/stamp. Guarded on
    /// `measurements_complete`: mid-dimensioning the resolve returns the
    /// PREVIOUS storey untouched, and reconstructing from those stale floors
    /// would wipe the tag that was just pushed.
    fn canonicalize_phase_regions_after_assign(&mut self, idx: usize) {
        let _ = self.commit_to_ber();
        if self
            .document
            .storeys
            .get(idx)
            .is_some_and(|sd| sd.measurements_complete())
        {
            self.reconstruct_phase_regions_for(idx);
        }
    }

    /// Discard the in-flight phase area / pending cut without tagging.
    pub fn cancel_pending_phase(&mut self) {
        self.abandon_phase_shape();
    }

    /// The current storey's committed phase zones projected for the UI to tint
    /// and label. DISPLAY-ONLY: sourced from the RESOLVED, mutually-exclusive
    /// floor regions (`Ber::storey_phase_zone_polygons`) — NOT the raw session
    /// `self.phase_regions` rings (which still drive snapping / stamping and are
    /// left untouched). So an older phase a newer one overlaps shows as a DONUT
    /// with its hole, and each zone's label sits inside its own material (never
    /// in the hole). `Unassigned` remainder is skipped. Empty when none.
    ///
    /// Because the zones are the resolved exclusive floors (unioned per phase),
    /// this is NO LONGER index-aligned with `self.phase_regions` — deletion
    /// ([`delete_phase_region`](Self::delete_phase_region)) still indexes the raw
    /// session list, which the picker in the prompt dock drives separately.
    pub fn phase_region_polygons(&self) -> Vec<PhaseDisplayRegion> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(ber) = self.current_record.as_ref().map(|r| &r.data) else {
            return Vec::new();
        };
        let phases = &ber.construction_phases;
        ber.storey_phase_zone_polygons(idx)
            .into_iter()
            .map(|z| PhaseDisplayRegion {
                exterior: z.exterior.iter().map(|p| (p.x, p.y)).collect(),
                holes: z
                    .holes
                    .iter()
                    .map(|h| h.iter().map(|p| (p.x, p.y)).collect())
                    .collect(),
                label_pos: (z.label_anchor.x, z.label_anchor.y),
                label: phases.display_label(&z.phase),
            })
            .collect()
    }

    /// Labels for the DELETE panel's construction-year rows, index-aligned with
    /// the RAW session `self.phase_regions` — the exact list
    /// [`delete_phase_region`](Self::delete_phase_region) indexes. Kept separate
    /// from [`phase_region_polygons`](Self::phase_region_polygons) (the display
    /// zones, unioned per phase) because the union loses the one-row-per-tag
    /// mapping the delete affordance relies on. Empty when none.
    pub fn phase_region_delete_labels(&self) -> Vec<String> {
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
                        phases
                            .map_or_else(|| r.phase.default_name(), |ph| ph.display_label(&r.phase))
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
                let label = phases.display_label(&p);
                (p, label)
            })
            .collect()
    }

    /// Stamp the storey's LEFTOVER fabric — the outline minus the phase areas
    /// already drawn — as explicit `Original` region(s) and re-commit. The
    /// untagged remainder otherwise resolves as `Unassigned` (the gate's "not yet
    /// assigned" state); this is the one-tap "everything else is the original
    /// build" affordance that satisfies the Construction-phase checklist row.
    /// Each leftover ring becomes a session [`PhaseRegion`] tagged `Original`
    /// (the durable record is the floor regions' `construction_id`, like every
    /// other phase area). No-op when the outline is fully covered already.
    pub fn assign_rest_to_original(&mut self) {
        self.sync_active();
        let idx = self.current_storey_idx.unwrap_or(0);
        // `available_region_rings` works on the f32 outline; narrow the f64
        // phase rings to f32 for the boolean, widen the leftover back to f64.
        let phase_rings: Vec<Vec<Point2f32>> = self
            .phase_regions
            .get(&idx)
            .map(|rs| {
                rs.iter()
                    .map(|r| r.polygon.iter().copied().map(to_f32).collect())
                    .collect()
            })
            .unwrap_or_default();
        let rings = self.document.available_region_rings(&phase_rings);
        if rings.is_empty() {
            return;
        }
        let entry = self.phase_regions.entry(idx).or_default();
        for ring in rings {
            entry.push(PhaseRegion {
                polygon: ring.into_iter().map(to_f64).collect(),
                phase: ConstructionPhase::Original,
            });
        }
        // Canonicalize immediately: the leftover strip usually abuts an
        // existing same-phase zone, and the raw two-entry list would otherwise
        // keep re-seeding a boundary between two Original zones (bug c78ebed1).
        self.canonicalize_phase_regions_after_assign(idx);
    }

    /// Delete the current storey's phase region at `index` and re-commit (so
    /// surfaces it had stamped fall back to their existing phase — see the
    /// preserve-existing rule in `document::stamp_wall_phases`). No-op if out of range.
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
