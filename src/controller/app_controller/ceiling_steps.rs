//! Ceiling-step tagging: the assessor draws an AXIS-ALIGNED LINE across the
//! storey plan (a near-straight stroke, or a tap + an H/V choice), taps the
//! side of the line where the ceiling steps, optionally pins the line's exact
//! coordinate via a position prompt, then enters the step amount (metres,
//! relative to the storey's default ceiling height; + raised, − lowered).
//!
//! A CLOSED stroke instead CARVES an area — floor-step parity: the same
//! carve-against-the-available-region machinery, dimensioned (wall lengths +
//! position prompts) as the active `StepArea` shape, then the SAME amount
//! stage as the line flow. The carve has NO session tag of its own: modes are
//! mutually exclusive, so an active `StepArea` shape while ceiling-step mode
//! is on IS the in-flight ceiling carve
//! ([`AppController::ceiling_carve_in_flight`]) — which keeps undo clean (the
//! document snapshot is the whole pending state).
//!
//! Like floor steps, a ceiling step is NOT persisted as a record of its own.
//! It's session-only controller state whose job is to re-split the resolved
//! ceiling regions at commit (`CeilingRegions::with_z_deltas`, applied in
//! `document::resolve_storey`): the durable record is the per-vertex z on the
//! roof regions the split produces, and on reload the session regions are
//! rebuilt from those (`document::ceiling_step_regions_from_roofs`).
//!
//! The line (axis + coordinate + side) is input UX only — the session record
//! is the REGION. After each assign the shell CANONICALIZES: materialize, then
//! rebuild the session list from the derived ceilings, so the in-session list
//! equals the post-reload reconstruction (idempotent reload by construction).
use super::*;
use document::{LinePositionPrompt, LineSide};

/// The elective wall snap a pending step line is carrying: the line's
/// `coord_m` already HOLDS the snapped (reference wall's) coordinate; this
/// remembers where the line was actually DRAWN so "Don't snap" can revert,
/// and which outline wall it snapped onto. A snapped line skips the position
/// prompt — its coordinate is a reference wall's, exact by construction.
#[derive(Debug, Clone, Copy)]
pub(super) struct CeilingLineSnap {
    /// The fitted/tapped coordinate before snapping — the un-snap target.
    pub(super) drawn_coord_m: f32,
    /// The outline wall the line snapped onto (start vertex, outline plan), or
    /// `None` when it snapped onto an EXTRA reference line — an existing region
    /// boundary in this storey or the storey below, which carries no outline
    /// vertex.
    pub(super) outline_wall_vid: Option<VertexId>,
}

/// The in-flight ceiling-step input, advanced stage by stage:
/// tap → [`Orientation`] → H/V → [`Side`] → side tap → ([`Position`] →)
/// [`Amount`] → assign. A stroke skips straight to [`Side`] (the fit decides
/// the orientation). Session-only, outside undo; cleared on storey switch,
/// mode exit, and cancel.
#[derive(Debug, Clone)]
pub(super) enum PendingCeilingStep {
    /// A point was tapped; awaiting the Horizontal/Vertical choice.
    Orientation { point: Point2f32 },
    /// The line is fixed; awaiting the side tap. `axis` is the axis the
    /// line's POSITION varies on (X for a vertical line).
    Side {
        axis: ParameterAxis,
        coord_m: f32,
        /// `Some` when the line snapped onto a parallel outline wall —
        /// `coord_m` is then the wall's coordinate.
        snap: Option<CeilingLineSnap>,
    },
    /// Side picked; the line's coordinate is being measured from an outline
    /// reference wall. Shell-resolved — this prompt never enters the
    /// document's position-prompt loop (there's no shape behind a line).
    /// Implies UN-snapped: a snapped line skips this stage entirely.
    Position {
        axis: ParameterAxis,
        coord_m: f32,
        side: LineSide,
        prompt: LinePositionPrompt,
    },
    /// Awaiting the step amount (the UI's Up/Down buttons own the sign).
    Amount {
        axis: ParameterAxis,
        coord_m: f32,
        side: LineSide,
        /// `Some` when the line is snapped — the position prompt was skipped
        /// (the coordinate is exact); un-snapping from here reopens it.
        snap: Option<CeilingLineSnap>,
    },
    /// An EXISTING committed step region (session-list index) selected by a
    /// tap for EDIT: the amount prompt reopens with its current value — a
    /// new value REPLACES the region's delta, the prompt's Delete removes
    /// the region entirely. Dividers (Δ 0) select like any other step — the
    /// dead-end the field report hit ("keep same height" then no way back).
    EditExisting { region: usize },
}

impl AppController {
    /// True when the ceiling-step tool can be used: a storey is being edited
    /// (false on the All tab), its outline is finalised (metric + fully
    /// dimensioned — the line must land in metre space), and its default
    /// ceiling height is set (the deltas are relative to it). Drives the UI
    /// toggle's enablement AND guards the setter (gate double-enforced).
    pub fn ceiling_step_available(&self) -> bool {
        let Some(idx) = self.current_storey_idx else {
            return false;
        };
        let height_set = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .is_some_and(|s| s.ceiling_height_m.is_some());
        height_set && self.outline_finalised()
    }

    /// True when the editor is tagging ceiling steps (drives the UI toggle
    /// and routes canvas strokes/taps to the line input).
    pub fn ceiling_step_mode(&self) -> bool {
        self.ceiling_step_mode
    }

    /// Enter/leave ceiling-step mode. Entering is a NO-OP while the tool is
    /// unavailable (see [`ceiling_step_available`](Self::ceiling_step_available));
    /// otherwise it drops into draw mode (a stroke draws the line straight
    /// away), forces the OUTLINE active (so no carved-area position prompt can
    /// coexist with the line's), and is mutually exclusive with living-area /
    /// exposure / phase / floor-step modes. Leaving clears any half-finished
    /// line or carved area.
    pub fn set_ceiling_step_mode(&mut self, on: bool) {
        // Leaving the outline draw tool: drop any un-decided re-draw stroke
        // (a stray second outline) so it doesn't linger on the canvas. (n3)
        self.pending_redraw = None;
        if on && !self.ceiling_step_available() {
            return;
        }
        // Drop a half-finished line AND abandon a half-dimensioned carve —
        // BEFORE the flag flips (the carve is detected via the OLD mode).
        self.cancel_pending_ceiling_step();
        // Leaving abandons the in-flight ceiling carve, so its tags (which
        // would replay a stroke that only means "ceiling step" under the mode)
        // must not survive the exit. Entering clears unconditionally below.
        if !on {
            self.clear_subarea_capture(SubareaFlow::CeilingStep);
        }
        self.ceiling_step_mode = on;
        if on {
            self.exit_openings_mode();
            self.living_area_mode = false;
            self.porch_mode = false;
            self.exposure_mode = false;
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            self.dormer_mode = false;
            self.pending_rect_is_dormer = false;
            // A surviving cut-pick would fire on the SAME tap as the ceiling
            // line flow (its choose_* methods have no mode guard), grafting an
            // in-flight phase/step shape under the line — clear them all.
            self.pending_cut = None;
            self.pending_phase_cut = None;
            self.pending_step_cut = None;
            // Drop any replayable snap tags from earlier strokes: their
            // choose/un-snap REPLAYS the outline action, which could move the
            // wall a pending line just snapped onto out from under it.
            self.last_snap_action = None;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
    }

    /// UI entry point: a ceiling-step stroke in world (metre) coords. Three
    /// shapes route three ways:
    /// - a single straight axis-aligned stroke is a step LINE
    ///   ([`StoreyDoc::fit_ceiling_step_line`]) → advances to the side pick;
    /// - a CLOSED stroke is an AREA (the floor-step parity path) → carved;
    /// - any OTHER open stroke — e.g. a CUT with three drawn sides whose open
    ///   ends meet the outline (the fourth side) — is also carved as an area
    ///   (the carve auto-picks the cut-off region), so it's accepted rather
    ///   than rejected as "not a straight line".
    ///
    /// Both carve routes go through
    /// [`apply_ceiling_carve_stroke`](Self::apply_ceiling_carve_stroke).
    pub fn apply_ceiling_step_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Paste-ready stroke dump (matches the living-area / step-area dumps).
        dump_stroke("ceiling-step", &pts);
        // Snap-tag capture: supersede any previous action's tags (their replay
        // could move a wall out from under whatever this stroke creates) and
        // take the pre-action state — the carve routes below surface their
        // elective snaps through the generic tag overlay (a Cut outcome pushes
        // no undo entry, so its capture carries this `before` itself). The
        // LINE route keeps its own bespoke tag and leaves no generic capture.
        self.begin_snap_capture();
        let before = self.current_history_snapshot();
        // Flush the live buffer to its slot: both the fit and the carve work
        // against `document.outline`, which is only synced on shape switches.
        self.sync_active();
        // A CLOSED stroke is an AREA, not a line — the floor-step parity path.
        if manip_plan::stroke_is_closed(&pts, self.tuning.subarea.close_gap_m) {
            self.apply_ceiling_carve_stroke(&pts, before);
            return;
        }
        // A single straight stroke is a step LINE; any other open stroke (a
        // multi-side cut whose ends meet the outline) is an AREA cut — carve it
        // through the same pipeline instead of rejecting it as "not a line".
        if manip_plan::fit_axis_line(&pts).is_none() {
            self.apply_ceiling_carve_stroke(&pts, before);
            return;
        }
        match self.document.fit_ceiling_step_line(&pts) {
            Ok((axis, coord_m)) => self.begin_ceiling_step_side(axis, coord_m),
            Err(e) => log::error!("ceiling-step line: {}", e),
        }
    }

    /// Carve a ceiling-step AREA against the AVAILABLE region — the outline
    /// minus the ceiling steps already committed — exactly as the floor step
    /// does (shared edges snap, no overlap), and start dimensioning it as the
    /// active `StepArea` shape: wall lengths, then the position prompts
    /// (snapped-to-wall axes skip the question via the pinned slot's
    /// `position_free_axes`). Handles both a CLOSED loop and an OPEN cut whose
    /// ends meet the outline (the carve resolves the latter to an auto-picked
    /// area). The SAME delta prompt as the line flow follows once dimensioning
    /// resolves. Rejections are logged; nothing is mutated on failure.
    fn apply_ceiling_carve_stroke(&mut self, pts: &[Point2f32], before: HistorySnapshot) {
        let idx = self.current_storey_idx.unwrap_or(0);
        let step_rings: Vec<Vec<Point2f32>> = self
            .ceiling_step_regions
            .get(&idx)
            .map(|rs| rs.iter().map(|r| r.polygon.clone()).collect())
            .unwrap_or_default();
        // Level onto (and pin to) the construction-phase lines, so a ceiling step
        // drawn beside a phase "year" boundary lands on it — same as living areas.
        let ref_walls = self.phase_reference_walls();
        let action_points: Vec<(f32, f32)> = pts.iter().map(|p| (p.x, p.y)).collect();
        let wall_snap_tol = self.snap_tol_carve();
        match self.document.carve_or_cut_in_available(
            pts,
            &step_rings,
            &self.tuning.subarea,
            &ref_walls,
            wall_snap_tol,
        ) {
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                if let Err(e) = plan.validate_simple() {
                    log::warn!("ceiling step area rejected (walls cross): {:?}", e);
                    // Nothing committed — drop the rejected carve's decisions.
                    let _ = manip_plan::take_snap_decisions();
                    return;
                }
                let undo_snap = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot_refs(&boundary, plan, &ref_walls);
                // No session tag: the active `StepArea` shape under
                // ceiling-step mode IS the in-flight carve, so undo (a
                // document restore) cleanly rewinds the whole pending state.
                self.begin_step_area_dimensioning(slot, undo_snap);
                self.capture_snap_action(SnapAction::Subarea {
                    flow: SubareaFlow::CeilingStep,
                    points: action_points,
                    pick: None,
                });
            }
            // A cut with corners whose ends reach the outline at two different
            // walls splits the region in TWO — reuse the floor step's side-pick
            // machinery verbatim: the pending regions highlight, a tap routes to
            // `choose_step_cut_region`, and `begin_step_area_dimensioning` (which
            // it calls) is mode-agnostic, so under ceiling-step mode the chosen
            // region dimensions and then awaits a ceiling DELTA, not a floor
            // level. No ceiling-specific cut UI — the floor cut IS the pipeline.
            Ok((boundary, SubareaOutcome::Cut(a, b))) => {
                let sa = self.pinned_subarea_slot_refs(&boundary, a, &ref_walls);
                let sb = self.pinned_subarea_slot_refs(&boundary, b, &ref_walls);
                self.pending_step_cut = Some([sa, sb]);
                self.capture_snap_action_pending(
                    SnapAction::Subarea {
                        flow: SubareaFlow::CeilingStep,
                        points: action_points,
                        pick: None,
                    },
                    before,
                    manip_plan::take_snap_decisions(),
                );
            }
            Err(e) => {
                log::error!("ceiling-step carve: {}", e);
                let _ = manip_plan::take_snap_decisions();
            }
        }
    }

    /// Both line-creation paths land here the moment the axis + coordinate
    /// are known: try the elective wall snap ([`StoreyDoc::snap_ceiling_step_line`])
    /// — within tolerance of a non-extreme parallel outline wall the line's
    /// coordinate BECOMES the wall's, and the drawn coordinate rides along in
    /// the snap record so the tag's "Don't snap" can revert. Then await the
    /// side tap.
    fn begin_ceiling_step_side(&mut self, axis: ParameterAxis, coord_m: f32) {
        let extras = self.region_snap_references(axis);
        let snap_tol = self.snap_tol_ceiling_line();
        let (coord_m, snap) = match self
            .document
            .snap_ceiling_step_line(axis, coord_m, &extras, snap_tol)
        {
            Some((snapped, outline_wall_vid)) => (
                snapped,
                Some(CeilingLineSnap {
                    drawn_coord_m: coord_m,
                    outline_wall_vid,
                }),
            ),
            None => (coord_m, None),
        };
        self.pending_ceiling_step = Some(PendingCeilingStep::Side {
            axis,
            coord_m,
            snap,
        });
    }

    /// Extra reference coordinates (on `axis`) a drawn ceiling-step line OR roof
    /// RIDGE may snap to, beyond the current outline walls: the parallel edges of
    /// EXISTING floor AND ceiling regions in this storey (internal floor/ceiling
    /// step boundaries — e.g. a raised ceiling area's edge) and of the storey
    /// BELOW — so a line/ridge drawn near any existing area edge locks onto it.
    /// Gathered from the materialized records the shell already holds; the
    /// geometry extraction is [`document::region_line_coords`] /
    /// [`document::ceiling_line_coords`]. Shared by the ceiling-step line and the
    /// ridge (`apply_roof_ridge_stroke_event`).
    pub(super) fn region_snap_references(&self, axis: ParameterAxis) -> Vec<f32> {
        let Some(idx) = self.current_storey_idx else {
            return Vec::new();
        };
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut coords = Vec::new();
        let mut gather = |storey: &ber_model::ber::structural::Storey| {
            coords.extend(document::region_line_coords(&storey.floors, axis));
            coords.extend(document::ceiling_line_coords(&storey.ceilings, axis));
        };
        if let Some(storey) = record.data.storeys.get(idx) {
            gather(storey);
        }
        if let Some(below) = idx.checked_sub(1).and_then(|b| record.data.storeys.get(b)) {
            gather(below);
        }
        coords
    }

    /// Canvas tap dispatch for the ceiling-step flow:
    /// - nothing pending (mode on) → the tap places the line's point; the H/V
    ///   picker chooses its orientation next.
    /// - side pending → `sign(tap − coord)` on the line's axis picks the side
    ///   of the line the delta applies to, then the position prompt opens (or
    ///   auto-resolves, logged, when there's no parallel reference wall).
    /// - any other stage → ignored (those are toolbar/prompt-driven).
    pub fn ceiling_step_tap(&mut self, world_x: f32, world_y: f32) {
        match self.pending_ceiling_step.as_ref() {
            // No line stage open and no carve being dimensioned. A tap INSIDE
            // an existing step region (dividers included) SELECTS it for
            // edit — the field report's dead-end was a committed step that
            // could be neither re-selected nor removed. A tap on virgin
            // ceiling places a fresh line point as before. (Placing a NEW
            // line inside an already-stepped area is still possible via a
            // drawn STROKE — the stroke path never selects.)
            None if self.ceiling_step_mode && !self.ceiling_carve_in_flight() => {
                if let Some(region) = self.ceiling_step_region_at(world_x, world_y) {
                    self.pending_ceiling_step = Some(PendingCeilingStep::EditExisting { region });
                    return;
                }
                self.pending_ceiling_step = Some(PendingCeilingStep::Orientation {
                    point: Point2f32::new(world_x, world_y),
                });
            }
            // While editing, tapping ANOTHER step region switches the
            // selection to it (tapping empty ceiling keeps the current one —
            // the prompt's Cancel is the way out).
            Some(PendingCeilingStep::EditExisting { .. }) => {
                if let Some(region) = self.ceiling_step_region_at(world_x, world_y) {
                    self.pending_ceiling_step = Some(PendingCeilingStep::EditExisting { region });
                }
            }
            Some(PendingCeilingStep::Side {
                axis,
                coord_m,
                snap,
            }) => {
                let (axis, coord_m, snap) = (*axis, *coord_m, *snap);
                let tap = match axis {
                    ParameterAxis::X => world_x,
                    ParameterAxis::Y => world_y,
                };
                let side = if tap >= coord_m {
                    LineSide::Greater
                } else {
                    LineSide::Less
                };
                self.advance_past_side(axis, coord_m, side, snap);
            }
            _ => {}
        }
    }

    /// Side chosen: open the position prompt for the line's coordinate, or —
    /// when the outline has no parallel wall to measure from — accept the
    /// drawn coordinate (the document logs the auto-resolve) and go straight
    /// to the amount. A SNAPPED line also skips the prompt: its coordinate IS
    /// a reference wall's, exact by construction — there's nothing to ask;
    /// the snap rides into the amount stage so its tag stays clickable.
    fn advance_past_side(
        &mut self,
        axis: ParameterAxis,
        coord_m: f32,
        side: LineSide,
        snap: Option<CeilingLineSnap>,
    ) {
        if snap.is_some() {
            self.pending_ceiling_step = Some(PendingCeilingStep::Amount {
                axis,
                coord_m,
                side,
                snap,
            });
            return;
        }
        self.pending_ceiling_step = Some(
            match self
                .document
                .ceiling_line_position_prompt(axis, coord_m, None)
            {
                Some(prompt) => PendingCeilingStep::Position {
                    axis,
                    coord_m,
                    side,
                    prompt,
                },
                None => PendingCeilingStep::Amount {
                    axis,
                    coord_m,
                    side,
                    snap: None,
                },
            },
        );
    }

    /// The snap tag's "Don't snap": revert the pending line to its DRAWN
    /// coordinate and drop the snap. From the side pick the stage stays put
    /// (the position prompt follows the side tap as normal); from the amount
    /// stage the skipped position question REOPENS (or auto-resolves straight
    /// back to the amount, logged). Deliberately NOT routed through the
    /// generic `choose_snap`/`unsnap_last` replay — those rewind history and
    /// re-run a stroke, but the pending line lives OUTSIDE history.
    pub fn unsnap_ceiling_step_line(&mut self) {
        match self.pending_ceiling_step.take() {
            Some(PendingCeilingStep::Side {
                axis,
                snap: Some(snap),
                ..
            }) => {
                log::debug!(
                    "ceiling-step line un-snapped from outline wall {:?} back to {:.3}",
                    snap.outline_wall_vid,
                    snap.drawn_coord_m
                );
                self.pending_ceiling_step = Some(PendingCeilingStep::Side {
                    axis,
                    coord_m: snap.drawn_coord_m,
                    snap: None,
                });
            }
            Some(PendingCeilingStep::Amount {
                axis,
                side,
                snap: Some(snap),
                ..
            }) => {
                log::debug!(
                    "ceiling-step line un-snapped from outline wall {:?} back to {:.3}",
                    snap.outline_wall_vid,
                    snap.drawn_coord_m
                );
                self.advance_past_side(axis, snap.drawn_coord_m, side, None);
            }
            other => self.pending_ceiling_step = other, // not snapped — no-op
        }
    }

    /// The pending line's wall snap as ONE standard canvas snap tag, at the
    /// line's midpoint. `options` is EMPTY and `skippable` is true — the
    /// tag's menu offers exactly "Don't snap", which the UI routes to
    /// [`unsnap_ceiling_step_line`](Self::unsnap_ceiling_step_line) (NOT the
    /// generic choose-snap replay — the line is outside history). `None`
    /// while the line isn't snapped or nothing is pending.
    pub fn ceiling_step_snap_tag(&self) -> Option<SnapTagView> {
        let (axis, snap) = match self.pending_ceiling_step.as_ref()? {
            PendingCeilingStep::Side {
                axis,
                snap: Some(snap),
                ..
            }
            | PendingCeilingStep::Amount {
                axis,
                snap: Some(snap),
                ..
            } => (*axis, *snap),
            _ => return None,
        };
        let ((ax, ay), (bx, by)) = self.pending_ceiling_step_line()?;
        let mid = (((ax + bx) / 2.0) as f32, ((ay + by) / 2.0) as f32);
        // The tag's `from` identity is the drawn point: the same midpoint at
        // the pre-snap coordinate (where the line would return on un-snap).
        let from = match axis {
            ParameterAxis::X => (snap.drawn_coord_m, mid.1),
            ParameterAxis::Y => (mid.0, snap.drawn_coord_m),
        };
        Some(SnapTagView {
            pos: mid,
            from,
            current_kind: Some(manip_plan::SnapKind::ToParallelWall),
            options: Vec::new(),
            skippable: true,
        })
    }

    /// H/V picker for a TAPPED line point: a `horizontal` line's position
    /// varies on Y (and a vertical one's on X). Validates the resulting line
    /// crosses the footprint; on rejection the tapped point stays pending so
    /// the user can pick the other orientation or cancel.
    pub fn choose_ceiling_step_orientation(&mut self, horizontal: bool) {
        let Some(PendingCeilingStep::Orientation { point }) = self.pending_ceiling_step.as_ref()
        else {
            return;
        };
        let (axis, coord_m) = if horizontal {
            (ParameterAxis::Y, point.y)
        } else {
            (ParameterAxis::X, point.x)
        };
        match self.document.validate_ceiling_step_line(axis, coord_m) {
            Ok(()) => self.begin_ceiling_step_side(axis, coord_m),
            Err(e) => log::error!("ceiling-step line: {}", e),
        }
    }

    /// True while a tapped point awaits its H/V orientation choice.
    pub fn ceiling_step_orientation_pending(&self) -> bool {
        matches!(
            self.pending_ceiling_step,
            Some(PendingCeilingStep::Orientation { .. })
        )
    }

    /// True while the drawn line awaits its side tap.
    pub fn ceiling_step_side_pending(&self) -> bool {
        matches!(
            self.pending_ceiling_step,
            Some(PendingCeilingStep::Side { .. })
        )
    }

    /// True while the resolved line awaits its step amount — or a carved
    /// ceiling-step area is fully dimensioned and awaiting its delta.
    /// The SESSION-list index of the smallest committed step region (divider
    /// or height step) containing plan point `(x, y)` — the tap-to-edit
    /// hit-test. Smallest wins so a small step nested over a larger one
    /// stays selectable.
    fn ceiling_step_region_at(&self, x: f32, y: f32) -> Option<usize> {
        let idx = self.current_storey_idx.unwrap_or(0);
        self.ceiling_step_regions
            .get(&idx)?
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.polygon.len() >= 3 && manip_plan::ring_contains_point(&r.polygon, x, y)
            })
            .min_by(|(_, a), (_, b)| {
                let area = |r: &CeilingStepRegion| {
                    let ring: Vec<domain::points::Point2f64> = r
                        .polygon
                        .iter()
                        .map(|p| domain::points::to_f64(*p))
                        .collect();
                    domain::points::ring_area(&ring).abs()
                };
                area(a)
                    .partial_cmp(&area(b))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
    }

    /// The selected-for-edit step's `(delta, ring)` while the EDIT stage is
    /// open — the UI's highlight + prompt prefill. `None` otherwise.
    pub fn ceiling_step_editing(&self) -> Option<(f64, Vec<(f64, f64)>)> {
        let Some(PendingCeilingStep::EditExisting { region }) = self.pending_ceiling_step.as_ref()
        else {
            return None;
        };
        let idx = self.current_storey_idx.unwrap_or(0);
        self.ceiling_step_regions
            .get(&idx)
            .and_then(|rs| rs.get(*region))
            .map(|r| {
                (
                    r.delta_m,
                    r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect(),
                )
            })
    }

    /// The RESOLVED ceiling height (m, storey-local) over the pending step's
    /// target area — the reference the amount prompt shows, and the base the
    /// ABSOLUTE entry converts against. The representative point is the
    /// chosen side's half-rect centre (line), the carved area's interior
    /// point, or the selected region's interior point (edit). `None` when no
    /// amount stage is open / nothing resolves there.
    pub fn ceiling_step_current_height(&self) -> Option<f64> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let pt = self.ceiling_step_target_point()?;
        self.current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| s.ceilings.top_z_at(pt))
    }

    /// The representative plan point of the pending step's target area (see
    /// [`ceiling_step_current_height`](Self::ceiling_step_current_height)).
    fn ceiling_step_target_point(&self) -> Option<domain::points::Point2f64> {
        use domain::points::Point2f64;
        match self.pending_ceiling_step.as_ref() {
            Some(PendingCeilingStep::Amount {
                axis,
                coord_m,
                side,
                ..
            }) => {
                let ((min_x, min_y), (max_x, max_y)) =
                    self.document.outline.floor_plan.bounding_box()?;
                let (lo, hi, cross_mid) = match axis {
                    ParameterAxis::X => (min_x, max_x, (min_y + max_y) / 2.0),
                    ParameterAxis::Y => (min_y, max_y, (min_x + max_x) / 2.0),
                };
                let along = match side {
                    LineSide::Greater => (*coord_m + hi) / 2.0,
                    LineSide::Less => (lo + *coord_m) / 2.0,
                };
                Some(match axis {
                    ParameterAxis::X => Point2f64::new(along as f64, cross_mid as f64),
                    ParameterAxis::Y => Point2f64::new(cross_mid as f64, along as f64),
                })
            }
            Some(PendingCeilingStep::EditExisting { region }) => {
                let idx = self.current_storey_idx.unwrap_or(0);
                let r = self.ceiling_step_regions.get(&idx)?.get(*region)?;
                let ring: Vec<Point2f64> = r
                    .polygon
                    .iter()
                    .map(|p| domain::points::to_f64(*p))
                    .collect();
                manip_plan::ring_interior_point(&ring)
            }
            _ if self.ceiling_carve_amount_pending() => {
                let ring: Vec<Point2f64> = self
                    .document
                    .floor_plan
                    .get_points()
                    .iter()
                    .map(|p| domain::points::to_f64(*p))
                    .collect();
                manip_plan::ring_interior_point(&ring)
            }
            _ => None,
        }
    }

    pub fn ceiling_step_amount_pending(&self) -> bool {
        matches!(
            self.pending_ceiling_step,
            Some(PendingCeilingStep::Amount { .. }) | Some(PendingCeilingStep::EditExisting { .. })
        ) || self.ceiling_carve_amount_pending()
    }

    /// True while a ceiling-step CARVE is in flight: modes are mutually
    /// exclusive, so an active `StepArea` shape under ceiling-step mode can
    /// only be the carved area being dimensioned. Derived (no session tag) so
    /// undo — a document restore — rewinds it with the document.
    pub(super) fn ceiling_carve_in_flight(&self) -> bool {
        self.ceiling_step_mode && self.document.active == ActiveShape::StepArea
    }

    /// True once the in-flight ceiling-step CARVE is fully DIMENSIONED — wall
    /// lengths AND position — and awaiting its delta (the line flow's amount
    /// stage). Mirrors [`step_pick_pending`](Self::step_pick_pending), which
    /// is the floor flavour of the same gate over the shared `StepArea` shape.
    fn ceiling_carve_amount_pending(&self) -> bool {
        self.ceiling_carve_in_flight()
            && self.document.dimensioning.current_prompt.is_none()
            && self.document.dimensioning.current_position_prompt.is_none()
            && self.document.floor_plan.get_points().len() >= 3
    }

    /// True while ANY ceiling-step input is in flight — a line stage
    /// (including the position stage, which shows as the shared position
    /// panel) or a carved area being dimensioned. The UI uses this to keep
    /// the toolbar reachable with the mode off and to suppress stroke
    /// capture while a stage is pending.
    pub fn ceiling_step_pending(&self) -> bool {
        self.pending_ceiling_step.is_some() || self.ceiling_carve_in_flight()
    }

    /// The pending step line's endpoints (world coords), spanning the outline
    /// bbox, for the UI's dashed-line overlay. `None` before the orientation
    /// is chosen (a point has no line yet) and when nothing is pending.
    pub fn pending_ceiling_step_line(&self) -> Option<((f64, f64), (f64, f64))> {
        let (axis, coord_m) = match self.pending_ceiling_step.as_ref()? {
            PendingCeilingStep::Orientation { .. } | PendingCeilingStep::EditExisting { .. } => {
                return None; // no line — the edit highlight is the region ring
            }
            PendingCeilingStep::Side { axis, coord_m, .. }
            | PendingCeilingStep::Position { axis, coord_m, .. }
            | PendingCeilingStep::Amount { axis, coord_m, .. } => (*axis, *coord_m),
        };
        let ((min_x, min_y), (max_x, max_y)) = self.document.outline.floor_plan.bounding_box()?;
        Some(match axis {
            ParameterAxis::X => (
                (coord_m as f64, min_y as f64),
                (coord_m as f64, max_y as f64),
            ),
            ParameterAxis::Y => (
                (min_x as f64, coord_m as f64),
                (max_x as f64, coord_m as f64),
            ),
        })
    }

    /// The two bbox halves either side of the pending line (world coords),
    /// for the UI to tint while the user taps the side that steps — the
    /// cut-pick pattern. Empty when the side pick isn't pending.
    pub fn pending_ceiling_step_side_rects(&self) -> Vec<Vec<(f64, f64)>> {
        let Some(PendingCeilingStep::Side { axis, coord_m, .. }) =
            self.pending_ceiling_step.as_ref()
        else {
            return Vec::new();
        };
        let (axis, coord) = (*axis, *coord_m as f64);
        let Some(((min_x, min_y), (max_x, max_y))) =
            self.document.outline.floor_plan.bounding_box()
        else {
            return Vec::new();
        };
        let (x0, y0, x1, y1) = (min_x as f64, min_y as f64, max_x as f64, max_y as f64);
        let rect =
            |ax: f64, ay: f64, bx: f64, by: f64| vec![(ax, ay), (bx, ay), (bx, by), (ax, by)];
        match axis {
            ParameterAxis::X => vec![rect(x0, y0, coord, y1), rect(coord, y0, x1, y1)],
            ParameterAxis::Y => vec![rect(x0, y0, x1, coord), rect(x0, coord, x1, y1)],
        }
    }

    /// Assign the step amount to the pending line — or to a fully-dimensioned
    /// carved area: rejects a near-zero step (|Δ| < 0.01 m — floor-step
    /// parity) and a drop at or past the default height (the ceiling would
    /// reach the floor); the pending line/area stays open on rejection so the
    /// user can retype. On success: push the region — the line's half-rect
    /// (the resolve projection clips it) or the carve's dimensioned ring —
    /// re-derive, then CANONICALIZE the session list from the derived ceilings
    /// so it equals the post-reload reconstruction. Outside undo —
    /// phases/steps parity.
    pub fn assign_ceiling_step_delta(&mut self, delta_m: f32) -> Result<(), String> {
        // The line's parameters, `None` for a dimensioned carve — or the
        // EDIT path (an existing region's delta REPLACED in place).
        let edit = match self.pending_ceiling_step.as_ref() {
            Some(PendingCeilingStep::EditExisting { region }) => Some(*region),
            _ => None,
        };
        let line = match self.pending_ceiling_step.as_ref() {
            Some(PendingCeilingStep::Amount {
                axis,
                coord_m,
                side,
                ..
            }) => Some((*axis, *coord_m, *side)),
            Some(PendingCeilingStep::EditExisting { .. }) => None,
            _ if self.ceiling_carve_amount_pending() => None,
            _ => return Ok(()), // nothing awaiting an amount — no-op
        };
        if !delta_m.is_finite() {
            return Err("The ceiling height must be a number.".into());
        }
        // Snap a near-zero step to EXACTLY 0.0 — a same-height divider (the
        // type-0 path and the "Keep same height" button converge here, and
        // reconstruction's `< 1e-3` flat test matches). The `Δ ≤ −default`
        // floor-clip guard below is vacuously fine at 0.
        let delta_m = if delta_m.abs() < 0.01 { 0.0 } else { delta_m };
        let idx = self.current_storey_idx.unwrap_or(0);
        let default = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| s.ceiling_height_m)
            .ok_or("Set the storey's default ceiling height first.")?;
        if delta_m as f64 <= -default {
            return Err(format!(
                "A {:.2} m drop would put the ceiling at or below the floor \
                 (default height {:.2} m).",
                -delta_m, default
            ));
        }
        if let Some(region) = edit {
            // EDIT: replace the selected region's delta in place — the same
            // recompose tail re-derives the roof under the new value.
            let Some(r) = self
                .ceiling_step_regions
                .get_mut(&idx)
                .and_then(|rs| rs.get_mut(region))
            else {
                self.pending_ceiling_step = None;
                return Err("That step no longer exists — reselect it.".into());
            };
            r.delta_m = delta_m as f64;
        } else {
            let region = match line {
                Some((axis, coord_m, side)) => self
                    .document
                    .ceiling_step_region(axis, coord_m, side, delta_m as f64)
                    .ok_or("The storey has no outline to step.")?,
                None => {
                    // The live buffer IS the dimensioned area (floor-step parity);
                    // its resolved ring is the region.
                    let polygon = self.document.floor_plan.get_points();
                    if polygon.len() < 3 {
                        return Err(
                            "The step area has no usable outline — cancel and redraw it.".into(),
                        );
                    }
                    CeilingStepRegion {
                        polygon,
                        delta_m: delta_m as f64,
                    }
                }
            };
            self.ceiling_step_regions
                .entry(idx)
                .or_default()
                .push(region);
            if line.is_none() {
                // Discard the carve's live buffer, return to the outline.
                self.abandon_step_shape();
            }
        }
        self.pending_ceiling_step = None;
        self.mark_dirty()?;
        // Regions-as-truth: recompose the ceilings from the session step list (now
        // including this step) + the durable apexes, then re-derive (re-fit +
        // walls). `reconstruct` canonicalizes the session list — the DISPLAY
        // cache — from the resulting ceilings.
        self.recompose_ceilings(idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// Assign the step by ABSOLUTE target height instead of a relative
    /// delta — the amount prompt's "At height" button: the assessor types
    /// the ceiling height the area should END UP at. Steps are stored
    /// RELATIVE (the model's `with_z_deltas` composes deltas, delta-0
    /// dividers and every reload fixpoint ride on that), so the
    /// delta is computed HERE, at assign time, against the RESOLVED height
    /// over the target area ([`ceiling_step_current_height`]) — and, when
    /// EDITING an existing step, corrected by its own current delta so the
    /// area genuinely lands at the typed height:
    /// `Δ_new = Δ_old + (typed − resolved)`. All the relative-path guards
    /// (near-zero → divider, floor clip) apply to the computed delta.
    ///
    /// [`ceiling_step_current_height`]: Self::ceiling_step_current_height
    pub fn assign_ceiling_step_absolute(&mut self, height_m: f32) -> Result<(), String> {
        if !self.ceiling_step_amount_pending() {
            return Ok(()); // nothing awaiting an amount — no-op
        }
        if !height_m.is_finite() {
            return Err("Ceiling height must be a number.".into());
        }
        // Bring the resolved ceilings current — the conversion reads them.
        self.materialize();
        let current = self
            .ceiling_step_current_height()
            .ok_or("No resolved ceiling under that area — set the default height first.")?;
        let old_delta = match self.pending_ceiling_step.as_ref() {
            Some(PendingCeilingStep::EditExisting { region }) => {
                let idx = self.current_storey_idx.unwrap_or(0);
                self.ceiling_step_regions
                    .get(&idx)
                    .and_then(|rs| rs.get(*region))
                    .map(|r| r.delta_m)
                    .unwrap_or(0.0)
            }
            _ => 0.0,
        };
        let delta = old_delta + (height_m as f64 - current);
        self.assign_ceiling_step_delta(delta as f32)
    }

    /// Delete the SELECTED existing step (the edit prompt's Delete button):
    /// remove it from the session list and recompose — its pieces re-flatten
    /// (or re-join, for a divider). No-op unless the edit stage is open.
    pub fn delete_selected_ceiling_step(&mut self) {
        let Some(PendingCeilingStep::EditExisting { region }) = self.pending_ceiling_step.take()
        else {
            return;
        };
        let idx = self.current_storey_idx.unwrap_or(0);
        if let Some(rs) = self.ceiling_step_regions.get_mut(&idx) {
            if region < rs.len() {
                rs.remove(region);
                let _ = self.mark_dirty();
                self.recompose_ceilings(idx);
                self.materialize();
                self.reconstruct_ceiling_step_regions_for(idx);
            }
        }
    }

    /// Split the active ceiling-step line/area into two SAME-HEIGHT pieces
    /// (a divider, no riser) — the UI's "Keep same height" button. Just a
    /// 0.0 delta through [`assign_ceiling_step_delta`](Self::assign_ceiling_step_delta),
    /// named so the button reads intentionally; the partition bounds a ridge's
    /// own area without changing either side's height.
    pub fn keep_ceiling_same_height(&mut self) -> Result<(), String> {
        self.assign_ceiling_step_delta(0.0)
    }

    /// Discard the in-flight ceiling-step line — or carved area — at any
    /// stage. A carve is an active `StepArea` shape, so its live buffer is
    /// abandoned too (back to the outline, floor-step parity).
    pub fn cancel_pending_ceiling_step(&mut self) {
        if self.ceiling_carve_in_flight() {
            self.abandon_step_shape();
        }
        self.pending_ceiling_step = None;
    }

    /// The current storey's committed step regions, as `(ring, label)` in
    /// world coords. EVERY region is listed (index-aligned with the raw
    /// session list; deletion maps by world ring) so all are selectable and
    /// deletable — the field report's dead-end was an invisible,
    /// unremovable divider. But a same-height DIVIDER carries an EMPTY label
    /// (`Δ 0`): the canvas draws no amber fill or text for it (report
    /// cb7a6b9b — a divider is just a boundary between two same-height
    /// sections, shown as the dashed cut
    /// [`ceiling_divider_lines`](Self::ceiling_divider_lines), not badged as
    /// a highlighted "divider" section); the delete panel substitutes its
    /// own row text. Real HEIGHT steps carry the delta AND the resulting
    /// height ("+0.30 m → 2.70 m", against the storey default — overlapping
    /// steps compose, so treat it as a guide) and keep the amber tint.
    pub fn ceiling_step_region_polygons(&self) -> Vec<(Vec<(f64, f64)>, String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let default = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| s.ceiling_height_m);
        self.ceiling_step_regions
            .get(&idx)
            .map(|regions| {
                regions
                    .iter()
                    .map(|r| {
                        let ring = r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                        let label = match (r.delta_m.abs() < 1e-3, default) {
                            (true, _) => Self::ceiling_step_region_label(r.delta_m),
                            (false, Some(d)) => format!(
                                "{} → {:.2} m",
                                Self::ceiling_step_region_label(r.delta_m),
                                d + r.delta_m
                            ),
                            (false, None) => Self::ceiling_step_region_label(r.delta_m),
                        };
                        (ring, label)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The label for a committed ceiling-step region: EMPTY for a same-height
    /// partition (`|Δ| < 1e-3` — a divider carries no height text; it shows
    /// as the dashed boundary only, report cb7a6b9b), else the signed delta
    /// ("+0.20 m"). The amber feed
    /// ([`ceiling_step_region_polygons`](Self::ceiling_step_region_polygons))
    /// appends the resulting height to a real step; an empty label there is
    /// the divider marker the canvas skips and the delete panel re-texts.
    pub fn ceiling_step_region_label(delta_m: f64) -> String {
        if delta_m.abs() < 1e-3 {
            String::new()
        } else {
            format!("{:+.2} m", delta_m)
        }
    }

    /// The current storey's same-height DIVIDER cut segments (world coords) —
    /// one per committed `|Δ| < 1e-3` ceiling-step region, for the UI's faint
    /// dashed pass. A projection of
    /// [`ceiling_divider_snap_segments`](Self::ceiling_divider_snap_segments)
    /// (the same geometry the porch carve pre-snaps onto) to the canvas's f64
    /// pairs. View data only — derived, not stored.
    pub fn ceiling_divider_lines(&self) -> Vec<((f64, f64), (f64, f64))> {
        self.ceiling_divider_snap_segments()
            .into_iter()
            .map(|[a, b]| ((a.x as f64, a.y as f64), (b.x as f64, b.y as f64)))
            .collect()
    }

    /// The current storey's same-height DIVIDER cut segments as world-metre
    /// snap references — the shell gathers the session step regions (session
    /// state lives here) and the model derives each divider's interior cut
    /// edge ([`StoreyDoc::ceiling_divider_segments`], geometry in
    /// [`manip_plan::interior_split_edge`]). ONE source of truth for "where
    /// the divider lines are": the canvas dashes
    /// ([`ceiling_divider_lines`](Self::ceiling_divider_lines)) and the porch
    /// carve's line pre-snap both read this. Empty without dividers.
    ///
    /// [`StoreyDoc::ceiling_divider_segments`]: document::StoreyDoc::ceiling_divider_segments
    pub(super) fn ceiling_divider_snap_segments(&self) -> Vec<[Point2f32; 2]> {
        let idx = self.current_storey_idx.unwrap_or(0);
        self.ceiling_step_regions
            .get(&idx)
            .map(|regions| self.document.ceiling_divider_segments(regions))
            .unwrap_or_default()
    }

    /// Delete the current storey's ceiling-step region matching `ring` (the
    /// world ring the UI feed carries) and re-commit — the roof pieces it had
    /// stepped re-flatten to the default height on the next derive. A RING,
    /// not a list index (kept from the divider-filtered era; ring identity
    /// stays the stabler contract). Dividers are deletable like any step.
    /// No-op when no region's ring matches.
    pub fn delete_ceiling_step_region(&mut self, ring: &[(f64, f64)]) {
        let idx = self.current_storey_idx.unwrap_or(0);
        let raw = self.ceiling_step_regions.get(&idx).and_then(|rs| {
            rs.iter().position(|r| {
                r.polygon.len() == ring.len()
                    && r.polygon.iter().zip(ring).all(|(p, (rx, ry))| {
                        (p.x as f64 - rx).abs() < 1e-6 && (p.y as f64 - ry).abs() < 1e-6
                    })
            })
        });
        if let Some(raw) = raw {
            self.ceiling_step_regions
                .get_mut(&idx)
                .expect("entry exists")
                .remove(raw);
            let _ = self.mark_dirty();
            // Regions-as-truth: recompose from the remaining session steps so the
            // removed step's pieces re-flatten, then re-derive + re-canonicalize.
            self.recompose_ceilings(idx);
            self.materialize();
            self.reconstruct_ceiling_step_regions_for(idx);
        }
    }
}
