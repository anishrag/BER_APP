//! Sketch entry points and stroke application: mode plumbing, the
//! apply-stroke family (outline/add-section/extend/trim), pending-redraw
//! decisions, and provisional arcs.
use super::*;
use std::sync::OnceLock;

/// The extend/trim splice-pipeline overlay (`render_splice_debug`) is a
/// development aid — the coloured raw→simplified→snapped→…→spliced phases. It's
/// OFF by default (it lingered on the canvas after every trim); set
/// `BER_SPLICE_DEBUG=1` to see it. Cached like `BER_TOUCH`.
fn splice_debug_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("BER_SPLICE_DEBUG").is_ok_and(|v| v != "0"))
}

/// A completed stroke that's waiting for the user to confirm what to do with it.
/// Populated when `DrawPerpWalls` finishes a stroke while a floor plan already
/// exists — the controller doesn't apply the stroke until the user picks:
/// Replace / Add-new-storey / Ignore.
#[derive(Debug, Clone)]
pub struct PendingRedraw {
    pub stroke_points: Vec<Point2f32>,
    pub closed: bool,
}

impl AppController {
    /// Is there a pending DrawPerpWalls stroke awaiting the user's redraw decision?
    pub fn has_pending_redraw(&self) -> bool {
        self.pending_redraw.is_some()
    }

    /// User said: replace the current storey with the pending stroke.
    pub fn confirm_replace_storey(&mut self) -> Result<(), String> {
        let Some(pending) = self.pending_redraw.take() else {
            return Ok(());
        };
        self.apply_drawperp_stroke(&pending.stroke_points, pending.closed)
    }

    /// User said: add a new storey using the pending stroke as its outline.
    /// Stub — implementation deferred to the multi-storey feature.
    pub fn confirm_add_storey_from_current(&mut self) {
        if let Some(_pending) = self.pending_redraw.take() {
            log::debug!("Add new storey from current — TODO (stub for upcoming feature)");
        }
    }

    /// User said: discard the pending stroke without doing anything.
    pub fn discard_pending_redraw(&mut self) {
        self.pending_redraw = None;
    }

    /// SKETCHING STUFF--------------------------------------------
    pub fn sketch_stroke(&self) -> Option<Stroke> {
        self.sketch_engine.stroke.clone()
    }

    pub fn sketch_mode(&self) -> SketchMode {
        self.sketch_engine.mode
    }

    pub fn set_sketch_mode(&mut self, mode: SketchMode) {
        // 3D needs real storey heights (walls/ceilings are extruded to them).
        // The UI's floating 2D/3D toggle is disabled until the scope's
        // heights are set (entered via the Heights category); this guard is
        // defense in depth — a 3D request without heights is a silent no-op.
        if matches!(mode, SketchMode::Storey3D) && !self.ceiling_heights_set() {
            return;
        }
        self.sketch_engine.set_mode(mode);
    }

    pub fn display_mode(&self) -> DisplayMode {
        self.sketch_engine.display
    }

    /// Set the canvas display mode (which surface kind is highlighted; the
    /// others fade). Drives the Structure-phase Walls/Ceilings/Floors tools and is
    /// reset to a sensible default on phase switch.
    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.sketch_engine.display = mode;
    }

    /// User-initiated tool pick (the toolbar's ChangeEditTool): exits the
    /// canvas-claiming overlay modes FIRST so the picked tool's strokes
    /// actually land. Without this, an active exposure / openings / roof /
    /// partition mode kept owning the canvas and silently swallowed every
    /// Trim/Extend stroke while the tool button showed selected (the field
    /// porch-trim failure). Each exit is guarded on the mode actually being
    /// on — the setters clear their own transient state (e.g. `set_roof_mode`
    /// drops `pending_redraw`), which must not happen collaterally.
    ///
    /// Internal flows that programmatically pick a tool (living areas, phase /
    /// step / ceiling flows) keep calling [`set_edit_mode`](Self::set_edit_mode),
    /// which leaves modes alone.
    pub fn select_edit_tool(&mut self, mode: EditTools) {
        if self.openings_mode {
            self.set_openings_mode(false);
        }
        if self.exposure_mode {
            self.set_exposure_mode(false);
        }
        if self.floor_exposure_mode {
            self.set_floor_exposure_mode(false);
        }
        if self.ceiling_exposure_mode {
            self.set_ceiling_exposure_mode(false);
        }
        if self.roof_mode {
            self.set_roof_mode(false);
        }
        if self.partition_mode() {
            self.close_wall_partition();
        }
        self.set_edit_mode(mode);
    }

    pub fn set_edit_mode(&mut self, mode: EditTools) {
        // Picking an edit tool leaves navigation mode (clicks/drags act again).
        self.navigation_mode = false;
        // Switching tools ends the interaction that produced the last action's
        // elective-snap tags — drop them so they don't linger on the canvas
        // under the new tool (they belong only to the tool/stroke that made them).
        self.last_snap_action = None;
        if !matches!(mode, EditTools::Move) {
            // Leaving Move mode clears the selection so a stale highlight
            // doesn't carry over into the next tool.
            self.document.edit_state = EditState::default();
        }
        self.sketch_engine.set_edit_tools(mode);
    }

    /// Whether the canvas is in CAD-style navigation mode (no tool selected — a
    /// click/drag only moves the view).
    pub fn navigation_mode(&self) -> bool {
        self.navigation_mode
    }

    /// Drop into navigation mode: deselect every tool so the canvas does nothing
    /// on a tap. Called on a view (2D↔3D) or page change. Turning each area tool
    /// off also clears its transient state; the edit-tool drawing is gated on the
    /// `navigation_mode` flag (there's no "no edit tool" otherwise).
    pub fn enter_navigation_mode(&mut self) {
        self.set_openings_mode(false);
        self.set_exposure_mode(false);
        self.set_floor_exposure_mode(false);
        self.set_ceiling_exposure_mode(false);
        self.set_phase_mode(false);
        self.set_step_mode(false);
        self.set_ceiling_step_mode(false);
        self.set_roof_point_mode(false);
        self.set_living_area_mode(false);
        self.set_porch_mode(false);
        self.close_wall_partition();
        // Deselecting every tool ends the interaction the snap tags belong to —
        // clear them so they don't hang on the canvas in navigation mode.
        self.last_snap_action = None;
        self.navigation_mode = true;
    }

    pub fn edit_mode(&self) -> EditTools {
        self.sketch_engine.edit_tool
    }

    pub fn sketch_start_stroke(&mut self, start: Point2f32) -> Result<(), String> {
        // A new stroke invalidates any pending collinearity review — the
        // polygon is about to change.
        self.review = None;
        self.sketch_engine
            .start_stroke(start)
            .map_err(|e| format!("Failed to start stroke: {:?}", e))
    }

    /// Drop the in-flight stroke without dispatching it (two-finger gesture
    /// retraction). Nothing was committed, so no undo snapshot is taken.
    pub fn sketch_cancel_stroke(&mut self) {
        self.sketch_engine.cancel_stroke();
    }

    pub fn sketch_add_point(&mut self, point: Point2f32) -> Result<(), String> {
        self.sketch_engine
            .add_point(point)
            .map_err(|e| format!("Failed to add point: {:?}", e))
    }

    pub fn sketch_end_stroke(&mut self, _point: Point2f32) -> Result<(), String> {
        let edit_tool = self.sketch_engine.edit_tool;
        // Extend strokes don't need to self-close (they start and end at
        // different points on the existing polygon). Take the raw stroke
        // out of the engine without running the close-check; other tools
        // still go through end_stroke which requires close.
        let stroke_opt = if matches!(
            edit_tool,
            EditTools::ExtendOrtho
                | EditTools::ExtendCurve
                | EditTools::Trim
                | EditTools::TrimOrtho
                | EditTools::TrimCurve
        ) {
            self.sketch_engine.take_stroke_raw()
        } else {
            self.sketch_engine
                .end_stroke()
                .map_err(|e| format!("Failed to end stroke: {:?}", e))?
        };
        if let Some(stroke) = stroke_opt {
            log::debug!("Stroke completed with {} points", stroke.points.len());
            // Only THIS stroke's elective snaps may end up in the capture below
            // — drop any stale decisions an unrelated op left behind.
            self.begin_snap_capture();
            // Keep the raw stroke + tool so this action can be REPLAYED under a
            // forced snap policy (recorded after the dispatch, if it snapped).
            let captured = SnapAction::Freehand {
                tool: edit_tool,
                points: stroke.points.clone(),
                closed: stroke.is_closed,
            };
            match edit_tool {
                EditTools::DrawPerpWalls => {
                    // New-storey flow gating: while the slab THICKNESS is
                    // pending the stroke is ignored; in ChooseMethod/Browse
                    // the stroke is an implicit choose_draw_outline.
                    if self.new_storey_blocks_outline_stroke() {
                        return Ok(());
                    }
                    // Drawing the primary outline.
                    if self.document.floor_plan.ring_count() > 0 {
                        // A plan already exists — don't silently overwrite.
                        // Stash the stroke and let the UI ask the assessor
                        // what to do (Replace / Add new storey / Ignore).
                        self.pending_redraw = Some(PendingRedraw {
                            stroke_points: stroke.points,
                            closed: stroke.is_closed,
                        });
                    } else {
                        // No existing plan — apply directly.
                        self.apply_drawperp_stroke(&stroke.points, stroke.is_closed)?;
                    }
                }
                EditTools::AddDetachedSection => {
                    self.apply_addsection_stroke(&stroke.points, stroke.is_closed)?;
                }
                EditTools::ExtendOrtho | EditTools::ExtendCurve => {
                    if let Err(e) = self.apply_extend_stroke(&stroke.points) {
                        // For now just log; in future we'll surface this
                        // to the UI alongside the dimension error.
                        log::error!("apply_extend_stroke: {}", e);
                    }
                }
                EditTools::Trim | EditTools::TrimOrtho | EditTools::TrimCurve => {
                    if let Err(e) = self.apply_trim_stroke(&stroke.points) {
                        log::error!("apply_trim_stroke: {}", e);
                    }
                }
                _ => {
                    // Other tools don't consume the stroke at this layer yet.
                }
            }
            // If the action made elective snaps, remember it for replay/un-snap.
            self.capture_snap_action(captured);
        }
        Ok(())
    }

    /// Apply a primary-outline stroke: replace the floor plan, persist, and
    /// populate the collinearity review. Resets dimensioning state — the new
    /// plan is a fresh pixel-scale sketch that hasn't been dimensioned yet.
    /// Diagnostic: log any non-orthogonal (slanted) edge of the active shape.
    /// A no-op when fully orthogonal. Called after operations that should leave
    /// the shape rectilinear (draw / extend-ortho / carve / dimension) so a
    /// "wall isn't straight" report shows up in the log with the exact edge.
    /// (A plain `Trim` may create a deliberate diagonal cut, so it's not checked.)
    pub(super) fn warn_if_non_orthogonal(&self, ctx: &str) {
        let bad = self.document.floor_plan.non_orthogonal_edges();
        if bad.is_empty() {
            return;
        }
        log::warn!("⚠ NON-ORTHOGONAL edge(s) after {ctx} ({}):", bad.len());
        for (r, e, a, b) in &bad {
            log::warn!(
                "    ring {r} edge {e}: ({:.3},{:.3})→({:.3},{:.3})  dx={:.4} dy={:.4}",
                a.x,
                a.y,
                b.x,
                b.y,
                (a.x - b.x).abs(),
                (a.y - b.y).abs()
            );
        }
    }

    pub(super) fn apply_drawperp_stroke(
        &mut self,
        points: &[Point2f32],
        closed: bool,
    ) -> Result<(), String> {
        // Snapshot pre-stroke (may be empty / pre-dim) for undo.
        let undo_snap = self.current_history_snapshot();
        dump_stroke("draw-outline", points);
        log::debug!(
            "[new-storey] PIXEL outline path: storey={:?}, {} points (this is the from-scratch \
             pixel path)",
            self.current_storey_idx,
            points.len()
        );
        if matches!(self.current_storey_idx, Some(i) if i > 0) {
            log::warn!(
                "[new-storey] WARNING: pixel path on upper storey {:?} — the world/snap path did \
                 NOT trigger (expected the previous storey's watermark + metre-space capture). The \
                 outline won't snap to the storey below and will re-scale from scratch.",
                self.current_storey_idx
            );
        }
        // Method-2 snap: when this is the FIRST stroke on a new upper storey
        // and a previous storey exists, snap each freehand point onto the
        // previous storey's outline so the new footprint reuses those exact
        // coordinates wherever the user drew close. Threshold scales to the
        // previous storey's diagonal (see `snap_to_previous_storey_outline`).
        let snapped_owned = self.snap_to_previous_storey_outline(points);
        let points: &[Point2f32] = snapped_owned.as_deref().unwrap_or(points);
        self.document.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
            .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
        log::debug!("floor plan: {:?}", self.document.floor_plan);
        self.warn_if_non_orthogonal("draw outline");
        // An upper storey's outline was just replaced WHOLESALE (the Replace
        // path / a fresh draw): the inherited-level seed must re-fire against
        // the new footprint, and the old footprint's step regions die with it.
        self.reset_inherited_seed_for_redraw();
        self.document.dimensioning = DimensionState::default();
        self.push_history_snapshot(undo_snap);
        self.commit_review_redimension()
    }

    /// If the editor is on a NEW storey (current outline empty) and a previous
    /// storey exists, snap each stroke point that lands within ~6% of the
    /// previous outline's diagonal onto that outline. Returns `None` (caller
    /// uses the raw stroke as-is) when no snap applies — first storey, no
    /// previous geometry, or current storey already has an outline.
    pub(super) fn snap_to_previous_storey_outline(
        &self,
        points: &[Point2f32],
    ) -> Option<Vec<Point2f32>> {
        // Only on a fresh storey — once an outline exists this is a redraw, not
        // a method-2 trace, and we don't want to silently snap the user's stroke.
        if self.document.floor_plan.ring_count() > 0 {
            return None;
        }
        let ring = self.previous_storey_ring()?;
        // Viewport-relative snap radius: forgiving enough that a wall the user
        // drew "close" to the storey below lands on it, yet far below the depth
        // of a real extension (e.g. a garage bump-out), which stays un-snapped.
        let tol = self.snap_tol_extend();
        let snapped = manip_plan::snap_stroke_to_ring(points, &ring, tol);
        let moved = points
            .iter()
            .zip(snapped.iter())
            .filter(|(a, b)| (a.x - b.x).abs() > 1e-4 || (a.y - b.y).abs() > 1e-4)
            .count();
        log::debug!(
            "[new-storey] snap to previous outline: tol {:.3} m, {}/{} points pulled onto it",
            tol,
            moved,
            points.len()
        );
        Some(snapped)
    }

    /// Apply the FIRST outline stroke of a new upper storey, captured in the
    /// previous storey's *world* (metre) space (see [`SketchEvent::OutlineStrokeWorld`]).
    ///
    /// Because the stroke is already in metres, the new storey inherits scale
    /// for free: endpoints snap onto the previous outline, and every wall the
    /// new footprint *shares* with the previous storey is pinned (via
    /// [`FloorPlan::params_pinned_to_outline`]) so it isn't re-prompted — only
    /// the genuinely-new walls (e.g. a garage the upper storey extends over)
    /// remain to dimension. The geometry (snap, orthogonalize, pin) all lives
    /// in `manip_plan`; here we only sequence it.
    pub fn apply_outline_stroke_world(&mut self, world_points: &[(f32, f32)]) {
        log::debug!(
            "[new-storey] WORLD outline path: storey={:?}, {} world points",
            self.current_storey_idx,
            world_points.len()
        );
        dump_stroke(
            "new-storey",
            &world_points
                .iter()
                .map(|(x, y)| Point2f32::new(*x, *y))
                .collect::<Vec<_>>(),
        );
        // New-storey flow gating (same as the pixel path): while the slab
        // THICKNESS is pending the stroke is ignored — but still drained from
        // the engine so it doesn't linger; in ChooseMethod/Browse the stroke
        // is an implicit choose_draw_outline and applies normally.
        if self.new_storey_blocks_outline_stroke() {
            let _ = self.sketch_engine.take_stroke_raw();
            return;
        }
        // Run the engine's pixel-based close-check to learn whether the user
        // closed the loop; the pixel points themselves are discarded in favour
        // of the world points the UI already converted.
        let closed = match self.sketch_engine.end_stroke() {
            Ok(Some(stroke)) => stroke.is_closed,
            Ok(None) => {
                log::debug!("[new-storey] no stroke in engine — nothing to apply");
                return;
            }
            Err(e) => {
                log::error!("[new-storey] end_stroke failed: {:?}", e);
                return;
            }
        };
        // Only THIS carve's elective snaps may end up in the capture below —
        // drop any stale decisions an unrelated op left behind. (Every
        // capturing entry point clears on entry, so a FAILED carve's leftover
        // decisions can't be misattributed to the next captured action either.)
        self.begin_snap_capture();
        if let Err(e) = self.apply_outline_world_inner(world_points, closed) {
            log::error!("[new-storey] apply_outline_stroke_world failed: {}", e);
            return;
        }
        // If the carve made elective snaps, remember the action so the tag
        // overlay can offer alternatives / un-snap and replay it.
        self.capture_snap_action(SnapAction::OutlineWorld {
            points: world_points.to_vec(),
            closed,
        });
    }

    /// New storey via the **living-area carve**: trace the storey below as a
    /// reference, follow it where the trace hugs it and build fresh walls where it
    /// diverges — `allow_outside` so overhangs/extensions past the old footprint
    /// are kept rather than clamped in. The carve's boundary-follow subsumes the
    /// old reproduce/splice/extension cases in one pass.
    pub(super) fn apply_outline_world_inner(
        &mut self,
        world_points: &[(f32, f32)],
        _closed: bool,
    ) -> Result<(), String> {
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        let prev_ring = self
            .previous_storey_ring()
            .ok_or_else(|| "no previous storey to draw on".to_string())?;
        // Pre-snap the stroke onto the storey-below's INTERIOR construction-phase
        // boundaries (the carve only snaps to the outline ring itself): a wall
        // traced beside the phase "year" division below lands exactly ON it, so
        // the inherited phase seed splits the new floor along the real line.
        // Segments lying along the previous outline are already filtered out
        // (`document::phase_snap_segments`) — double targets would fight the
        // carve's own outline snap. Same tolerance rule as the outline snap
        // (`snap_to_previous_storey_outline`); the moves are elective, so the
        // snap-tag overlay picks them up like any other. The SAME segments then
        // go to the carve as reference walls: a snapped wall is a known wall,
        // so a wall lying on a phase line is pinned pre-known instead of
        // re-prompted — and a replay after "Don't snap" leaves the wall off the
        // line, so its dimension question correctly reopens.
        let phase_segs = match (self.current_storey_idx, self.current_record.as_ref()) {
            (Some(idx), Some(rec)) if idx > 0 => {
                document::phase_snap_segments(&rec.data, idx - 1, &prev_ring)
            }
            _ => Vec::new(),
        };
        let extend_tol = self.snap_tol_extend();
        let pts = manip_plan::snap_stroke_to_segments(
            &pts,
            &phase_segs,
            extend_tol,
            manip_plan::SnapKind::ToPhaseBoundary,
        );
        let undo_snap = self.current_history_snapshot();
        let (pinned, active) = self.document.carve_outline_from_previous(
            &pts,
            &prev_ring,
            &self.tuning.subarea,
            &phase_segs,
            extend_tol,
        )?;
        // Supply the storey-below footprint as the position reference so the
        // dimensioning loop, once the wall LENGTHS are pinned, asks where
        // VERTICALLY (or horizontally) to place a free upper outline — a
        // full-width box that inherited only its width from the walls below
        // still floats along the other axis. Transient (`#[serde(skip)]`), so
        // it's re-supplied on reload for a not-yet-finalised upper outline
        // ([`reestablish_position_reference_override`]).
        self.document.position_reference_override =
            manip_plan::FloorPlan::from_clean_ring(&prev_ring).ok();
        self.warn_if_non_orthogonal("draw outline (carve)");
        // The carve replaced the upper storey's outline WHOLESALE: re-pend
        // the inherited-level seed (the tail below / the dimensioning tails
        // re-seed against the NEW footprint) and drop the step regions
        // canonicalized against the old one. No-op on a fresh storey.
        self.reset_inherited_seed_for_redraw();
        log::debug!(
            "[new-storey] carved outline (reuse of living-area carve): {} verts, pinned {} of {} \
             active params ({} new wall param(s) left to dimension)",
            self.document.floor_plan.get_points().len(),
            pinned,
            active,
            active.saturating_sub(pinned),
        );
        self.push_history_snapshot(undo_snap);
        self.commit_review_redimension()?;
        // A fully-pinned carve (every wall shared with the storey below) is
        // finalised right here — the seed point for the inherited floor
        // levels (no-op while walls remain to dimension; those seed from the
        // dimensioning tails instead).
        self.maybe_seed_inherited_levels();
        Ok(())
    }

    /// Apply an AddDetachedSection stroke: append a ring (or replace if
    /// there's no outline yet), persist, populate review.
    pub(super) fn apply_addsection_stroke(
        &mut self,
        points: &[Point2f32],
        closed: bool,
    ) -> Result<(), String> {
        let undo_snap = self.current_history_snapshot();
        let had_outline = self.document.floor_plan.ring_count() > 0;
        if !had_outline {
            self.document.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
                .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
            // Same as DrawPerpWalls when there was nothing: reset dimensioning.
            self.document.dimensioning = DimensionState::default();
        } else {
            self.document
                .floor_plan
                .add_polygon(points, 10.0, closed)
                .map_err(|e| format!("Failed to add section: {:?}", e))?;
            // Existing known_params remain valid — apply_orthogonal_constraints
            // reuses param IDs for coords that match (ring 0's coords didn't
            // change). The new ring contributes additional unknown params we'll
            // pick up in the next dimensioning round.
            // KNOWN LIMITATION: if dimensions were already set (scale_set),
            // the new ring's vertex coords are in pixels while existing rings
            // are in meters. Mixed units will look wrong on the canvas until
            // a per-stroke pixels→meters conversion is added.
        }
        log::debug!(
            "floor plan (after AddDetachedSection): {:?}",
            self.document.floor_plan
        );
        // History snapshotted BEFORE the commit (consistent with the other
        // apply_* methods) so an undo entry exists even if the commit fails.
        self.push_history_snapshot(undo_snap);
        self.commit_review_redimension()
    }

    /// Entry point for Extend strokes from the UI. The stroke comes in as
    /// (f32, f32) world-coord pairs (already converted from canvas pixels
    /// by sketch.rs via `fit_transform.inverse`). Clears any leftover
    /// canvas-pixel stroke from the sketch engine, then forwards to the
    /// merge logic.
    /// Returns the rejection message when the stroke fails — the UI shows it
    /// (a silently-logged rejection reads as "nothing happened" on the tablet).
    pub fn apply_extend_stroke_event(&mut self, world_points: &[(f32, f32)]) -> Result<(), String> {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        log::debug!("Extend stroke (world coords): {} points", pts.len());
        dump_stroke("extend", &pts);
        let _ = manip_plan::take_splice_debug(); // clear stale phases
        self.begin_snap_capture();
        let res = self.apply_extend_stroke(&pts);
        self.splice_debug = self.take_splice_debug_if_enabled();
        if let Err(e) = &res {
            log::error!("apply_extend_stroke: {}", e);
        }
        self.capture_snap_action(SnapAction::Freehand {
            tool: self.sketch_engine.edit_tool,
            points: pts,
            closed: false,
        });
        res
    }

    /// Apply an Extend stroke. The geometry — snapping, orthogonalizing, and
    /// splicing the path into the parametric plan — lives in `FloorPlan`; here
    /// we only orchestrate app state. Dispatches by the active tool: `ExtendOrtho`
    /// adds a stepped porch, `ExtendCurve` a faceted arc. Because the splice
    /// preserves existing vertex/parameter IDs, the dimensioning state stays
    /// valid: we keep `known_params` (dropping only any that were removed)
    /// instead of resetting, so the user is asked to dimension just the new
    /// walls.
    pub(super) fn apply_extend_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        // Below-storey snap (ORTHO extend only): pull the extend stroke's FREE
        // points onto the storey-BELOW outline so a dormer wall the assessor
        // drew a touch short of the floor-below edge lands exactly ON it. Two
        // scopings keep it safe:
        //   - ORTHO only — a CURVE extend fits an ARC (`begin_arc_stroke`), for
        //     which straight-ring snapping is wrong, so it's skipped there.
        //   - FREE points only — the ATTACH points (those on/near the CURRENT
        //     outline being extended) are FROZEN, so a shallow setback (the
        //     current wall itself within tolerance of a below edge) can't drag
        //     the splice seats off the wall. Only the genuinely-new bump points
        //     may snap.
        // ELECTIVE + tagged — the free-point snaps record `SnapDecision`s the
        // caller drains into the replayable capture (snap-tag "don't snap" +
        // forced-policy replay), never silent. Tolerances scale with the ring
        // diagonals (no hardcoded metres, per the manip_plan rule); the below
        // tol reuses the new-storey ratio for consistency with how an upper
        // storey's own outline snaps onto the one below.
        let mut pts = points.to_vec();
        if self.sketch_engine.edit_tool != EditTools::ExtendCurve {
            if let Some(prev_ring) = self.previous_storey_ring() {
                let cur_ring = self.document.floor_plan.get_points();
                let tol = self.snap_tol_extend();
                // "On/near the current outline" → an attach seat, frozen.
                let attach_tol = (manip_plan::ring_diagonal(&cur_ring) * 0.03).max(1e-3);
                let free_idx: Vec<usize> = (0..pts.len())
                    .filter(|&i| manip_plan::dist_to_ring(&cur_ring, pts[i]) > attach_tol)
                    .collect();
                if !free_idx.is_empty() {
                    // The free points are contiguous for a real extend (attach at
                    // the ends), so snapping the extracted run reproduces in-place
                    // grouping; the attach points are written back untouched.
                    let free_pts: Vec<Point2f32> = free_idx.iter().map(|&i| pts[i]).collect();
                    let snapped = manip_plan::snap_stroke_to_ring(&free_pts, &prev_ring, tol);
                    for (k, &i) in free_idx.iter().enumerate() {
                        pts[i] = snapped[k];
                    }
                }
            }
        }
        let points: &[Point2f32] = &pts;

        // ExtendCurve defers: hold a provisional arc until its attach lines are
        // dimensioned (see `begin_arc_stroke`), rather than faceting now.
        if self.sketch_engine.edit_tool == EditTools::ExtendCurve {
            return self.begin_arc_stroke(points);
        }
        let undo_snap = self.current_history_snapshot();
        // Perimeter BEFORE the splice — the source for re-homing split-wall
        // attributes once `extend_ortho` may have carved a wall into two flanks.
        let old_perim = self.document.floor_plan.perimeter_walls();
        let minted = self
            .document
            .floor_plan
            .extend_ortho(points)
            .map_err(|e| format!("{}", e))?;
        log::debug!("Extend: {} new param(s) {:?}", minted.len(), minted);
        self.warn_if_non_orthogonal("extend");
        // The porch is a fix-up to the ENVELOPE — once spliced, that it was a
        // "porch" is irrelevant. Drop any vertex it left collinear (e.g. a
        // corner a flush porch turned into a mid-wall point) so the outline
        // stays minimal, then re-sync dimensioning state.
        self.clean_envelope_collinear();
        // A wall the porch opened in its MIDDLE is now two flanks; the far one
        // got a freshly-minted start vid with no exposure/construction entry.
        // Copy the source wall's attributes onto it (from the POST-splice live
        // buffer) so BOTH flanks inherit — before the resolve re-derives walls.
        let new_perim = self.document.floor_plan.perimeter_walls();
        self.document
            .rehome_split_wall_attributes(&old_perim, &new_perim);
        // A far wall the pre-snap dropped onto the storey-BELOW outline is now a
        // KNOWN wall — its position is implied by the snap, exactly as a shared
        // wall's is in the new-storey outline flow. Pin it (via the same
        // `params_pinned_to_outline` contact scan) so the dimension loop does
        // NOT prompt for it and a large height entry can't push the wall PAST
        // the outline (the field overshoot). Restricted to the extend's NEWLY
        // minted walls that landed on the below outline, so pre-existing walls
        // keep whatever the assessor already set. Purely geometric: a far wall
        // that did NOT snap isn't on the below outline, so nothing is pinned and
        // it's still prompted normally.
        if !minted.is_empty() {
            if let Some(prev_ring) = self.previous_storey_ring() {
                if let Ok(prev_plan) = manip_plan::FloorPlan::from_clean_ring(&prev_ring) {
                    let minted_set: std::collections::HashSet<manip_plan::ParameterId> =
                        minted.iter().copied().collect();
                    let pinned = prev_plan.params_pinned_to_outline(&self.document.floor_plan, &[]);
                    self.document
                        .dimensioning
                        .known_params
                        .extend(pinned.into_iter().filter(|p| minted_set.contains(p)));
                }
            }
        }
        // History snapshotted before the commit (consistent across apply_*).
        self.push_history_snapshot(undo_snap);
        self.mark_dirty()?;
        // Ask about collinearity ONLY for the porch's new walls — e.g. whether
        // a new porch front lines up with an earlier one. We never assume it;
        // and we don't re-surface pairs among the pre-existing walls. (Extend
        // uses this param-scoped review, so it can't share `commit_review_redimension`.)
        let suggestions = self
            .document
            .floor_plan
            .collinear_suggestions_for_params(&minted);
        if !suggestions.is_empty() {
            self.review = Some(CollinearityReview {
                suggestions,
                cursor: 0,
                rejections: Vec::new(),
            });
        }
        // Kicks off dimensioning for the porch's new walls — but defers while a
        // collinearity review is pending (it returns early when `review` is set;
        // resolving the review then starts dimensioning).
        self.maybe_start_dimensioning();
        Ok(())
    }

    /// Entry point for Trim strokes from the UI (world coords, like Extend).
    /// The stroke's first and last points define the straight cut line.
    /// Returns the rejection message when the stroke fails — the UI shows it
    /// (a silently-logged rejection reads as "nothing happened" on the tablet).
    pub fn apply_trim_stroke_event(&mut self, world_points: &[(f32, f32)]) -> Result<(), String> {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        log::debug!("Trim stroke (world coords): {} points", pts.len());
        dump_stroke("trim", &pts);
        let _ = manip_plan::take_splice_debug(); // clear stale phases
        self.begin_snap_capture();
        let res = self.apply_trim_stroke(&pts);
        // Capture the pipeline phases (recorded even when the splice is rejected)
        // for the UI's coloured debug overlay — only when explicitly enabled.
        self.splice_debug = self.take_splice_debug_if_enabled();
        if let Err(e) = &res {
            log::error!("apply_trim_stroke: {}", e);
        }
        self.capture_snap_action(SnapAction::Freehand {
            tool: self.sketch_engine.edit_tool,
            points: pts,
            closed: false,
        });
        res
    }

    /// Drain the splice pipeline's recorded phases, KEEPING them for the overlay
    /// only when `BER_SPLICE_DEBUG` is set; otherwise discard them so nothing is
    /// drawn (the phases always accumulate in `manip_plan` and must be drained
    /// each op regardless, else they'd leak into the next stroke).
    fn take_splice_debug_if_enabled(&self) -> Vec<manip_plan::SplicePhase> {
        let phases = manip_plan::take_splice_debug();
        if splice_debug_enabled() {
            phases
        } else {
            Vec::new()
        }
    }

    /// DEBUG: the splice-pipeline phases captured from the last extend/trim, as
    /// `(label, world points, closed)` per phase — for the UI's coloured overlay.
    pub fn splice_debug_overlay(&self) -> Vec<(String, Vec<(f32, f32)>, bool)> {
        self.splice_debug
            .iter()
            .map(|ph| {
                let pts = ph.points.iter().map(|p| (p.x, p.y)).collect();
                (ph.label.clone(), pts, ph.closed)
            })
            .collect()
    }

    /// Apply a Trim, discarding the smaller side. Dispatches by the active
    /// tool: `Trim` cuts a straight (possibly diagonal) line; `TrimOrtho` carves
    /// an orthogonal stepped cut; `TrimCurve` carves a faceted curved recess.
    /// The geometry lives in `FloorPlan::trim_by_stroke` / `trim_ortho` /
    /// `trim_curve`; here we only orchestrate app state. Surviving walls keep
    /// their parameters.
    pub(super) fn apply_trim_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        if points.len() < 2 {
            return Err("trim stroke needs at least 2 points".into());
        }
        // TrimCurve defers like ExtendCurve: a provisional arc dimensioned by
        // its attach lines, then faceted in. The drawn bulge direction (inward)
        // is what carves the recess — no separate keep-larger step needed.
        if self.sketch_engine.edit_tool == EditTools::TrimCurve {
            return self.begin_arc_stroke(points);
        }
        let undo_snap = self.current_history_snapshot();
        // Perimeter BEFORE the splice — the source for re-homing split-wall
        // attributes if the trim carves a wall into two flanks.
        let old_perim = self.document.floor_plan.perimeter_walls();
        let minted = match self.sketch_engine.edit_tool {
            EditTools::TrimOrtho => self.document.floor_plan.trim_ortho(points),
            _ => self.document.floor_plan.trim_by_stroke(points),
        }
        .map_err(|e| format!("{}", e))?;
        log::debug!("Trim: {} new param(s) {:?}", minted.len(), minted);
        self.clean_envelope_collinear();
        // A wall the trim opened in its MIDDLE is now two flanks; the far one got
        // a freshly-minted start vid. Copy the source wall's attributes onto it
        // (from the POST-splice live buffer) so BOTH flanks inherit — before the
        // resolve re-derives walls.
        let new_perim = self.document.floor_plan.perimeter_walls();
        self.document
            .rehome_split_wall_attributes(&old_perim, &new_perim);
        // History snapshotted before the commit (consistent across apply_*). A
        // plain Trim never asks about collinearity, so no review step here.
        self.push_history_snapshot(undo_snap);
        self.mark_dirty()?;
        self.maybe_start_dimensioning();
        Ok(())
    }

    /// Begin a provisional curved wall (shared by `ExtendCurve`/`TrimCurve`).
    /// `FloorPlan::begin_arc` fits the stroke to a circular arc and splices only
    /// its two endpoints onto the outline (a straight placeholder; the shape is
    /// unchanged). We suppress that placeholder edge from dimensioning prompts —
    /// so only the two attach stubs are asked about — and stash the arc in
    /// `pending_arc`. Once both stubs are dimensioned (or both endpoints reused
    /// corners, so there's nothing to dimension), the UI offers a radius slider
    /// ([`pending_arc_bulge`](Self::pending_arc_bulge)); accepting it
    /// ([`commit_pending_arc`](Self::commit_pending_arc)) bakes the curve in.
    /// Nothing about the geometry changes until then.
    pub(super) fn begin_arc_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        let undo_snap = self.current_history_snapshot();
        let (start, end, bulge_ratio) = self
            .document
            .floor_plan
            .begin_arc(points)
            .map_err(|e| format!("{}", e))?;
        // Don't prompt the straight placeholder edge — only its attach stubs.
        if let Some(skip) = self.document.floor_plan.adjacent_edge_start(start, end) {
            self.document.dimensioning.skipped_walls.insert(skip);
        }
        self.document.pending_arc = Some(PendingArc {
            start,
            end,
            bulge_ratio,
        });
        log::debug!(
            "Curve: provisional arc (bulge ratio {:.3}); dimension the two attach lines to bake it in",
            bulge_ratio
        );
        self.resync_dimensioning_to_plan();
        self.mark_dirty()?;
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// The provisional curve's current bulge (signed sagitta / chord), but only
    /// once it's ready to have its radius tuned: a pending arc whose attach
    /// lines are all set, with no dimension prompt outstanding. The UI binds a
    /// radius slider to this; `None` hides the slider (still drawing /
    /// dimensioning the attach lines).
    pub fn pending_arc_bulge(&self) -> Option<f32> {
        let pa = self.document.pending_arc.as_ref()?;
        if self.document.dimensioning.current_prompt.is_some()
            || !self.document.pending_arc_ready(pa)
        {
            return None;
        }
        Some(pa.bulge_ratio)
    }

    /// Set the provisional curve's bulge from the radius slider (clamped). Just
    /// updates the amber preview; commit happens on accept. No-op with no arc.
    pub fn set_pending_arc_bulge(&mut self, ratio: f32) {
        if let Some(pa) = self.document.pending_arc.as_mut() {
            pa.bulge_ratio = ratio.clamp(-1.5, 1.5);
        }
    }

    /// Accept the provisional curve at its current radius: facet the arc into
    /// the plan and LOCK the facet params (so later dimensioning of other walls
    /// can't let `repair`/`aesthetic_rescale` redistribute the facet vertices
    /// and flatten the curve). Clears the pending arc. If faceting fails the arc
    /// is dropped (the straight placeholder remains) rather than left stuck.
    pub fn commit_pending_arc(&mut self) -> Result<(), String> {
        let Some(pa) = self.document.pending_arc.clone() else {
            return Ok(());
        };
        let undo_snap = self.current_history_snapshot();
        let facet_params =
            match self
                .document
                .floor_plan
                .finish_arc(pa.start, pa.end, pa.bulge_ratio)
            {
                Ok(params) => params,
                Err(e) => {
                    log::warn!(
                        "finish_arc failed ({e}); dropping the provisional curve, keeping \
                     the straight placeholder. bulge={:.3}",
                        pa.bulge_ratio
                    );
                    self.document.pending_arc = None;
                    self.push_history_snapshot(undo_snap);
                    return Ok(());
                }
            };
        self.document
            .dimensioning
            .known_params
            .extend(facet_params.iter().copied());
        self.document.pending_arc = None;
        log::debug!(
            "Curve: committed at bulge {:.3} ({} facet params locked)",
            pa.bulge_ratio,
            facet_params.len()
        );
        self.mark_dirty()?;
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// World-coord points of the provisional arc preview, if one is mid-flow.
    /// The UI draws this as an overlay; the model still holds the straight
    /// placeholder until the attach dimensions are entered.
    pub fn pending_arc_preview(&self) -> Option<Vec<(f64, f64)>> {
        let pa = self.document.pending_arc.as_ref()?;
        let pts = self
            .document
            .floor_plan
            .arc_preview(pa.start, pa.end, pa.bulge_ratio)?;
        Some(pts.iter().map(|p| (p.x as f64, p.y as f64)).collect())
    }
}
