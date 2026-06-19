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
//! rebuild the session list from the derived roofs, so the in-session list
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
        if on && !self.ceiling_step_available() {
            return;
        }
        // Drop a half-finished line AND abandon a half-dimensioned carve —
        // BEFORE the flag flips (the carve is detected via the OLD mode).
        self.cancel_pending_ceiling_step();
        self.ceiling_step_mode = on;
        if on {
            self.exit_openings_mode();
            self.living_area_mode = false;
            self.exposure_mode = false;
            self.phase_mode = false;
            self.step_mode = false;
            self.roof_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
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

    /// UI entry point: a ceiling-step stroke in world (metre) coords. An OPEN
    /// stroke fits to an axis-aligned line
    /// ([`StoreyDoc::fit_ceiling_step_line`]) and, on success, advances
    /// straight to the side pick; a stroke that isn't line-like or doesn't
    /// cross the footprint is rejected (logged). A CLOSED stroke (the same
    /// closure test the floor-step carve uses) CARVES an area instead —
    /// see [`apply_ceiling_carve_stroke`](Self::apply_ceiling_carve_stroke).
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
        // Flush the live buffer to its slot: both the fit and the carve work
        // against `document.outline`, which is only synced on shape switches.
        self.sync_active();
        // A CLOSED stroke is an AREA, not a line — the floor-step parity path.
        if manip_plan::stroke_is_closed(&pts, self.tuning.subarea.close_gap_m) {
            self.apply_ceiling_carve_stroke(&pts);
            return;
        }
        match self.document.fit_ceiling_step_line(&pts) {
            Ok((axis, coord_m)) => self.begin_ceiling_step_side(axis, coord_m),
            Err(e) => log::error!("ceiling-step line: {}", e),
        }
    }

    /// A CLOSED ceiling-step stroke: carve the area against the AVAILABLE
    /// region — the outline minus the ceiling steps already committed —
    /// exactly as the floor step does (shared edges snap, no overlap), and
    /// start dimensioning it as the active `StepArea` shape: wall lengths,
    /// then the position prompts (snapped-to-wall axes skip the question via
    /// the pinned slot's `position_free_axes`). The SAME delta prompt as the
    /// line flow follows once dimensioning resolves. Rejections are logged;
    /// nothing is mutated on failure.
    fn apply_ceiling_carve_stroke(&mut self, pts: &[Point2f32]) {
        let idx = self.current_storey_idx.unwrap_or(0);
        let step_rings: Vec<Vec<Point2f32>> = self
            .ceiling_step_regions
            .get(&idx)
            .map(|rs| rs.iter().map(|r| r.polygon.clone()).collect())
            .unwrap_or_default();
        match self
            .document
            .carve_or_cut_in_available(pts, &step_rings, &self.tuning.subarea)
        {
            Ok((boundary, SubareaOutcome::Area(plan))) => {
                if let Err(e) = plan.validate_simple() {
                    log::warn!("ceiling step area rejected (walls cross): {:?}", e);
                    return;
                }
                let undo_snap = self.current_history_snapshot();
                let slot = self.pinned_subarea_slot(&boundary, plan);
                // No session tag: the active `StepArea` shape under
                // ceiling-step mode IS the in-flight carve, so undo (a
                // document restore) cleanly rewinds the whole pending state.
                self.begin_step_area_dimensioning(slot, undo_snap);
            }
            // `stroke_is_closed` gated this path and a closed loop always
            // carves to an Area — defensive (a cut would need a side pick
            // this flow doesn't have).
            Ok((_, SubareaOutcome::Cut(..))) => {
                log::warn!("ceiling-step carve: closed stroke resolved to a cut — ignored");
            }
            Err(e) => log::error!("ceiling-step carve: {}", e),
        }
    }

    /// Both line-creation paths land here the moment the axis + coordinate
    /// are known: try the elective wall snap ([`StoreyDoc::snap_ceiling_step_line`])
    /// — within tolerance of a non-extreme parallel outline wall the line's
    /// coordinate BECOMES the wall's, and the drawn coordinate rides along in
    /// the snap record so the tag's "Don't snap" can revert. Then await the
    /// side tap.
    fn begin_ceiling_step_side(&mut self, axis: ParameterAxis, coord_m: f32) {
        let extras = self.ceiling_step_snap_references(axis);
        let (coord_m, snap) = match self.document.snap_ceiling_step_line(axis, coord_m, &extras) {
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

    /// Extra reference coordinates (on `axis`) a drawn ceiling-step line may
    /// snap to, beyond the current outline walls: the parallel edges of EXISTING
    /// floor AND ceiling regions in this storey (internal floor/ceiling step
    /// boundaries) and of the storey BELOW (its outline + floor/ceiling steps) —
    /// so a line drawn near any existing line locks onto it. Gathered from the
    /// materialized records the shell already holds; the geometry extraction is
    /// [`document::region_line_coords`] / [`document::ceiling_line_coords`].
    fn ceiling_step_snap_references(&self, axis: ParameterAxis) -> Vec<f32> {
        let Some(idx) = self.current_storey_idx else {
            return Vec::new();
        };
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut coords = Vec::new();
        let mut gather = |storey: &datatypes::ber::structural::Storey| {
            coords.extend(document::region_line_coords(&storey.floors, axis));
            coords.extend(document::ceiling_line_coords(&storey.roofs, axis));
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
            // No line stage open and no carve being dimensioned — the tap
            // places a fresh line point. (A tap while a carve is in flight is
            // ignored: its prompts are toolbar-driven, like the floor step's.)
            None if self.ceiling_step_mode && !self.ceiling_carve_in_flight() => {
                self.pending_ceiling_step = Some(PendingCeilingStep::Orientation {
                    point: Point2f32::new(world_x, world_y),
                });
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
    pub fn ceiling_step_amount_pending(&self) -> bool {
        matches!(
            self.pending_ceiling_step,
            Some(PendingCeilingStep::Amount { .. })
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
            PendingCeilingStep::Orientation { .. } => return None,
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
    /// re-derive, then CANONICALIZE the session list from the derived roofs
    /// so it equals the post-reload reconstruction. Outside undo —
    /// phases/steps parity.
    pub fn assign_ceiling_step_delta(&mut self, delta_m: f32) -> Result<(), String> {
        // The line's parameters, or `None` for a dimensioned carve.
        let line = match self.pending_ceiling_step.as_ref() {
            Some(PendingCeilingStep::Amount {
                axis,
                coord_m,
                side,
                ..
            }) => Some((*axis, *coord_m, *side)),
            _ if self.ceiling_carve_amount_pending() => None,
            _ => return Ok(()), // nothing awaiting an amount — no-op
        };
        if !delta_m.is_finite() {
            return Err("Ceiling step must be a number.".into());
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
        self.pending_ceiling_step = None;
        self.mark_dirty()?;
        // Canonicalize: derive now, then make the session list the EXACT
        // derived pieces (delta = flat z − default). In-session state thus
        // matches what a reload would reconstruct — idempotence by construction.
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
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

    /// The current storey's committed HEIGHT-STEP regions, as `(ring, label)`
    /// in world coords, for the UI's amber tint + label (label e.g. "+0.20 m"
    /// / "−0.30 m"). Same-height DIVIDERS (`|Δ| < 1e-3`) are EXCLUDED — they'd
    /// otherwise read as a solid amber ring labelled "+0.00 m"; the UI draws
    /// them as a faint dashed cut instead (see
    /// [`ceiling_divider_lines`](Self::ceiling_divider_lines)). NOT
    /// index-aligned with `ceiling_step_regions` once dividers exist — for
    /// deletion the UI uses the world ring to map back.
    pub fn ceiling_step_region_polygons(&self) -> Vec<(Vec<(f64, f64)>, String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        self.ceiling_step_regions
            .get(&idx)
            .map(|regions| {
                regions
                    .iter()
                    .filter(|r| r.delta_m.abs() >= 1e-3)
                    .map(|r| {
                        let ring = r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                        (ring, Self::ceiling_step_region_label(r.delta_m))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The label for a committed ceiling-step region: "divider" for a
    /// same-height partition (`|Δ| < 1e-3`), else the signed delta
    /// ("+0.20 m"). Used by the region list/test surface; the amber feed
    /// ([`ceiling_step_region_polygons`](Self::ceiling_step_region_polygons))
    /// excludes dividers entirely.
    pub fn ceiling_step_region_label(delta_m: f64) -> String {
        if delta_m.abs() < 1e-3 {
            "divider".to_string()
        } else {
            format!("{:+.2} m", delta_m)
        }
    }

    /// The current storey's same-height DIVIDER cut segments (world coords) —
    /// one per committed `|Δ| < 1e-3` ceiling-step region, for the UI's faint
    /// dashed pass. The cut geometry (the divider's interior bbox edge on the
    /// split axis, shared with the other piece) is pure coordinate math, so it
    /// lives in [`manip_plan::interior_split_edge`]; this just iterates the
    /// session dividers and hands each ring + the outline bbox to it. View data
    /// only — derived, not stored.
    pub fn ceiling_divider_lines(&self) -> Vec<((f64, f64), (f64, f64))> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(((ox0, oy0), (ox1, oy1))) = self.document.outline.floor_plan.bounding_box() else {
            return Vec::new();
        };
        let outline_bbox = ((ox0 as f64, oy0 as f64), (ox1 as f64, oy1 as f64));
        self.ceiling_step_regions
            .get(&idx)
            .map(|regions| {
                regions
                    .iter()
                    .filter(|r| r.delta_m.abs() < 1e-3)
                    .filter_map(|r| {
                        let ring: Vec<(f64, f64)> =
                            r.polygon.iter().map(|p| (p.x as f64, p.y as f64)).collect();
                        manip_plan::interior_split_edge(&ring, outline_bbox)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Delete the current storey's ceiling-step region matching `ring` (the
    /// world ring the UI feed carries) and re-commit — the roof pieces it had
    /// stepped re-flatten to the default height on the next derive. A RING, not
    /// a list index: the displayed feed
    /// ([`ceiling_step_region_polygons`](Self::ceiling_step_region_polygons))
    /// is divider-filtered, so its indices don't map to the raw region Vec.
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
        }
    }
}
