//! Flat-topped DORMER input: in dormer mode a CLOSED stroke draws a raised
//! rectangular patch carved into an already-sloped attic ceiling. Unlike the
//! pavilion RECT tool ([`ceiling_rects`](super::ceiling_rects)) — which stamps
//! four `CeilingApex` records and RESLOPES the whole ceiling region up to the
//! flat top — a dormer does NOT reslope its surroundings: the patch is
//! flattened to its top height while the neighbouring roof keeps its slope, so
//! the dormer's vertical walls fall out of [`CeilingRegions::riser_faces`] for
//! free. V1 scope: rectangular, flat top, fully inside one roof slope (an
//! island dormer).
//!
//! The tool REUSES the pavilion rect's drawing / length / position / centre
//! FSM ([`PendingCeilingRect`](super::ceiling_rects::PendingCeilingRect)) —
//! the same LenX → LenY → PosX → PosY → Height flow — discriminated by the
//! controller's `pending_rect_is_dormer` flag (set when the stroke was drawn
//! in dormer mode). Only the FINAL height step routes differently: instead of
//! four apexes it validates ([`document::validate_dormer`]) and appends a
//! durable [`Dormer`] via [`Ber::add_dormer`], then re-derives. The pavilion
//! path (flag false) is byte-for-byte unchanged. Outside undo — the ceiling
//! commits' shape throughout.
use super::ceiling_rects::{PendingCeilingRect, RectPins};
use super::*;
use ber_model::ber::structural::Dormer;

impl AppController {
    /// True when the dormer tool can be used — exactly the ceiling-point /
    /// rect gate ([`ceiling_point_available`](Self::ceiling_point_available)):
    /// a storey is being edited, its outline is finalised, and its default
    /// ceiling height is set (a dormer carves a derived ceiling region, which
    /// only exists once the height is). Drives the UI toggle's enablement AND
    /// guards the setter.
    pub fn dormer_available(&self) -> bool {
        self.ceiling_point_available()
    }

    /// True when the editor is placing dormers (drives the UI toggle and
    /// routes canvas strokes to the dormer input).
    pub fn dormer_mode(&self) -> bool {
        self.dormer_mode
    }

    /// Enter/leave dormer mode. Entering is a NO-OP while the tool is
    /// unavailable (see [`dormer_available`](Self::dormer_available));
    /// otherwise it drops into draw mode, forces the OUTLINE active (so no
    /// carved-area position prompt can coexist), and is mutually exclusive
    /// with the living-area / exposure / phase / floor-step / ceiling-step /
    /// ceiling-point modes. Leaving clears any half-finished rect. Mirrors
    /// [`set_roof_point_mode`](Self::set_roof_point_mode).
    pub fn set_dormer_mode(&mut self, on: bool) {
        // Leaving the outline draw tool: drop any un-decided re-draw stroke.
        self.pending_redraw = None;
        if on && !self.dormer_available() {
            return;
        }
        self.dormer_mode = on;
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.pending_roof_rect = None;
        self.pending_rect_is_dormer = false;
        self.pending_rect_pins = RectPins::default();
        if on {
            self.exit_openings_mode();
            self.living_area_mode = false;
            self.porch_mode = false;
            self.exposure_mode = false;
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.ceiling_point_mode = false;
            self.continuation_tool = None;
            self.pending_ceiling_continuation = None;
            self.reset_ceiling_confirm = false;
            // A surviving cut-pick would fire on the same stroke's tap.
            self.pending_cut = None;
            self.pending_phase_cut = None;
            self.pending_step_cut = None;
            self.last_snap_action = None;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
    }

    /// True while the in-flight rect is a DORMER (drawn in dormer mode) — the
    /// UI reads this to route the HEIGHT stage to
    /// [`assign_dormer_height`](Self::assign_dormer_height) and title its
    /// prompt "Dormer height (m)". Gated on a pending rect so a stale flag
    /// never reads true.
    pub fn pending_rect_is_dormer(&self) -> bool {
        self.pending_roof_rect.is_some() && self.pending_rect_is_dormer
    }

    /// UI entry point: a CLOSED stroke (world/metre coords) drawn in dormer
    /// mode. Only a closed rectangle is a dormer (its own footprint), so —
    /// unlike the ridge tool's stroke, which also accepts a line — a
    /// non-closed stroke is rejected (logged). Marks the shared rect FSM as a
    /// dormer and hands off to the pavilion rect fit
    /// ([`apply_roof_rect_stroke`](Self::apply_roof_rect_stroke)): same
    /// axis-aligned fit + LenX question, only the height commit differs. A fit
    /// failure clears the dormer flag (no rect is pending).
    pub fn apply_dormer_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if !self.dormer_mode || self.ceiling_rect_pending() {
            return;
        }
        let raw: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if raw.len() < 2 {
            return;
        }
        // Flush the live buffer to its slot: the fit reads `document.outline`.
        self.sync_active();
        if !manip_plan::stroke_is_closed(&raw, self.tuning.subarea.close_gap_m) {
            log::error!("dormer: draw a CLOSED rectangle for the dormer footprint");
            return;
        }
        // The FSM pushes no undo entry (dormers commit at the HEIGHT stage), so
        // the snap-tag replay must carry its own pre-fit snapshot. Take it
        // BEFORE the edge snap so an un-snap replay restores exactly this state.
        let before = self.current_history_snapshot();
        self.begin_snap_capture();
        // Bring the derived ceiling/wall regions current so the reference gather
        // sees them, then assemble the edges a new dormer should snap flush to:
        // surrounding ceiling/roof-region edges (incl. ridge/step boundaries),
        // floor+ceiling riser bases, and every EXISTING dormer's footprint.
        // (`ceiling_carve_reference_segments` returns every ceiling-region edge,
        // which for the top storey includes the outline/eave — but v1 dormers
        // are islands strictly inside the roof, so a side snapped onto the eave
        // just fails `validate_dormer` at commit; it's self-guarding, not useful
        // to snap to. Filtering the outline out for the dormer is a follow-up.)
        self.materialize();
        let mut refs = self.ceiling_carve_reference_segments();
        refs.extend(self.step_line_segments());
        refs.extend(self.other_dormer_edge_segments());
        let tol = self.snap_tol_dormer();
        // Fit the RAW stroke to an axis-aligned rectangle FIRST, then EDGE-snap
        // its four sides onto the references — a whole-SIDE snap (not a
        // per-point snap), so a cheek running alongside a shorter reference
        // snaps its entire side flush and pins it (matching the carve tools'
        // wall-leveling). A point pre-snap would leave the fitted side off the
        // reference when only part of the cheek ran alongside it.
        self.pending_rect_is_dormer = true;
        self.apply_roof_rect_stroke(&raw);
        let Some(PendingCeilingRect::LenX { lo, len }) = self.pending_roof_rect.as_ref() else {
            // The fit failed (logged) — no rect is pending; drop the flag and
            // any orphaned decisions.
            self.pending_rect_is_dormer = false;
            let _ = manip_plan::take_snap_decisions();
            return;
        };
        let (lo, len) = (*lo, *len);
        // Edge-snap the fitted rectangle's sides onto the references: overwrite
        // the pending geometry with the snapped `(lo, len)` and record which
        // sides PINNED (their dimension prompt is skipped, their coordinate
        // un-overridable). The snap records an elective decision per snapped
        // side and respects the forced-choice policy on replay, so the snap-tag
        // / tap-to-unsnap machinery drives it exactly as the carve tools'.
        let (plo, phi) = (
            Point2f32::new(lo.0, lo.1),
            Point2f32::new(lo.0 + len.0, lo.1 + len.1),
        );
        let (lo, len, pins) = manip_plan::snap_axis_rect_to_refs(plo, phi, &refs, tol);
        self.pending_roof_rect = Some(PendingCeilingRect::LenX { lo, len });
        self.pending_rect_pins = RectPins::from_sides(pins);
        // Skip any LENGTH stage a fully-pinned axis determines (a fully-pinned
        // rect runs the finalize check here — a rejection is logged, leaving the
        // FSM at LenX for the user to cancel/redraw).
        if let Err(e) = self.open_rect_len_stage(ParameterAxis::X, lo, len) {
            log::error!("dormer placement: {}", e);
        }
        // Capture for the elective snap-tag overlay + tap-to-unsnap replay.
        self.capture_snap_action_pending(
            SnapAction::Dormer {
                points: world_points.to_vec(),
            },
            before,
            manip_plan::take_snap_decisions(),
        );
    }

    /// The current storey's already-committed dormer footprint EDGES as
    /// world-metre segments, so a NEW dormer snaps flush to a neighbour. Reads
    /// the durable `storey.dormers[*].footprint` rings (as
    /// [`dormer_views`](Self::dormer_views) does). Empty when there's no record.
    fn other_dormer_edge_segments(&self) -> Vec<[Point2f32; 2]> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        let mut segs = Vec::new();
        for d in &storey.dormers {
            let n = d.footprint.len();
            for i in 0..n {
                let a = d.footprint[i];
                let b = d.footprint[(i + 1) % n];
                segs.push([
                    Point2f32::new(a.x as f32, a.y as f32),
                    Point2f32::new(b.x as f32, b.y as f32),
                ]);
            }
        }
        segs
    }

    /// Assign the dormer's flat-top HEIGHT: materialize (validation runs
    /// against the RESOLVED ceilings), validate
    /// ([`document::validate_dormer`] — the footprint strictly inside ONE
    /// ceiling region, which MAY be sloped, and the top at or above the roof
    /// over the whole footprint), then append the durable [`Dormer`]
    /// ([`Ber::add_dormer`] — quantization included) and re-derive. The
    /// footprint is the FSM's fitted rectangle ring. The pending rect stays
    /// open on rejection so the user can retype. Outside undo — the rect
    /// commit's shape, but composing a non-resloping patch instead of four
    /// apexes.
    ///
    /// [`Dormer`]: ber_model::ber::structural::Dormer
    /// [`Ber::add_dormer`]: ber_model::ber::structural::ber_struct::Ber::add_dormer
    pub fn assign_dormer_height(&mut self, z_m: f32) -> Result<(), String> {
        let Some(PendingCeilingRect::Height { lo, len }) = self.pending_roof_rect.as_ref() else {
            return Ok(()); // nothing awaiting a height — no-op
        };
        let (lo, len) = (*lo, *len);
        // The fitted rectangle ring (CCW), the form `add_dormer` /
        // `validate_dormer` take — LO, +X, +XY, +Y.
        let footprint = vec![
            Point2f64::new(lo.0 as f64, lo.1 as f64),
            Point2f64::new((lo.0 + len.0) as f64, lo.1 as f64),
            Point2f64::new((lo.0 + len.0) as f64, (lo.1 + len.1) as f64),
            Point2f64::new(lo.0 as f64, (lo.1 + len.1) as f64),
        ];
        let idx = self.current_storey_idx.unwrap_or(0);
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a dormer to")?;
        document::validate_dormer(storey, &footprint, z_m as f64)?;
        self.current_record
            .as_mut()
            .expect("checked above")
            .data
            .add_dormer(
                idx,
                Dormer {
                    footprint,
                    top_z_m: z_m as f64,
                },
            );
        self.pending_roof_rect = None;
        self.pending_rect_is_dormer = false;
        self.pending_rect_pins = RectPins::default();
        self.mark_dirty()?;
        // Regions-as-truth: recompose the ceilings from the durable inputs once
        // (the dormer replays via `with_flat_patches`), re-derive, and refresh
        // the session ceiling-step display cache.
        self.recompose_ceilings(idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// The current storey's COMMITTED dormers as `(footprint ring, label)` in
    /// world coords for the UI's outline + label overlay — the rect family's
    /// sibling ([`ceiling_rect_views`](Self::ceiling_rect_views)), labelled
    /// "⌐ 2.60 m". A general ring (not a fixed quad) so a future
    /// non-rectangular dormer overlays unchanged.
    pub fn dormer_views(&self) -> Vec<(Vec<(f64, f64)>, String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        storey
            .dormers
            .iter()
            .map(|d| {
                let ring: Vec<(f64, f64)> = d.footprint.iter().map(|p| (p.x, p.y)).collect();
                (ring, format!("⌐ {:.2} m", d.top_z_m))
            })
            .collect()
    }
}
