//! Roof-point (apex) tagging: the assessor TAPS a high point inside a
//! ceiling area, pins its exact position per axis (X then Y, reusing the
//! ceiling-step line's snap + position-prompt machinery for each axis), then
//! enters the apex's ABSOLUTE height (metres above the storey floor datum).
//! Assigning fans the containing ceiling region up (or down) to the apex —
//! one triangle per boundary edge.
//!
//! Unlike the ceiling steps, an apex IS durable input (`Storey::ceiling_apexes`,
//! applied by the resolve projection AFTER the step areas — composition order
//! is law: default ceiling → steps → apexes). What the shell owns here is the
//! in-flight tap → position → height state machine, and the canonicalize step
//! after each assign/delete: a fan over a stepped piece makes the session
//! ceiling-step list re-derive as the fan's TRIANGLE pieces
//! (`ceiling_step_regions_from_roofs` is apex-aware), so the in-session list
//! equals the post-reload reconstruction — the apex-over-step reload fixpoint.
//! Outside undo — phases/steps/ceiling-steps parity.
use super::*;
use ber_model::ber::structural::CeilingApex;
use document::LinePositionPrompt;

/// The elective wall snap a pending ceiling point is carrying on ONE axis: the
/// point's coordinate on that axis already HOLDS the snapped (reference
/// wall's) coordinate; this remembers where the point was actually TAPPED so
/// "Don't snap" can revert, and which outline wall it snapped onto. A snapped
/// axis skips its position prompt — its coordinate is a reference wall's,
/// exact by construction.
#[derive(Debug, Clone, Copy)]
pub(super) struct CeilingPointAxisSnap {
    /// The tapped coordinate on this axis before snapping — the un-snap target.
    pub(super) drawn_coord_m: f32,
    /// The outline wall the axis snapped onto (start vertex, outline plan).
    pub(super) outline_wall_vid: VertexId,
}

/// The in-flight ceiling-point input, advanced stage by stage: tap →
/// ([`Position`] per un-snapped axis, X then Y →) [`Height`] → assign.
/// Session-only, outside undo; cleared on storey switch, mode exit, and
/// cancel. The snaps ride through every stage so the single snap tag stays
/// clickable until assign.
#[derive(Debug, Clone)]
pub(super) enum PendingCeilingPoint {
    /// One axis's coordinate is being measured from an outline reference
    /// wall — the ceiling-step line prompt reused per axis. Shell-resolved
    /// (`StoreyDoc::resolve_ceiling_line_position`); never enters the
    /// document's position-prompt loop (there's no shape behind a point).
    /// `axis` is the axis being asked; the OTHER axis is either already
    /// resolved (X resolves before Y opens) or snapped.
    Position {
        point: Point2f32,
        axis: ParameterAxis,
        prompt: LinePositionPrompt,
        snap_x: Option<CeilingPointAxisSnap>,
        snap_y: Option<CeilingPointAxisSnap>,
    },
    /// Both axes resolved; awaiting the apex height (metres, ABSOLUTE above
    /// the storey floor datum).
    Height {
        point: Point2f32,
        snap_x: Option<CeilingPointAxisSnap>,
        snap_y: Option<CeilingPointAxisSnap>,
    },
}

impl AppController {
    /// True when the ceiling-point tool can be used — exactly the ceiling-step
    /// gate ([`ceiling_step_available`](Self::ceiling_step_available)): a
    /// storey is being edited, its outline is finalised, and its default
    /// ceiling height is set (the apex fans a derived ceiling region, which
    /// only exists once the height is). Drives the UI toggle's enablement AND
    /// guards the setter (gate double-enforced).
    pub fn ceiling_point_available(&self) -> bool {
        self.ceiling_step_available()
    }

    /// True when the editor is tagging ceiling points (drives the UI toggle and
    /// routes canvas taps to the point input).
    pub fn ceiling_point_mode(&self) -> bool {
        self.ceiling_point_mode
    }

    /// Enter/leave ceiling-point mode. Entering is a NO-OP while the tool is
    /// unavailable (see [`ceiling_point_available`](Self::ceiling_point_available));
    /// otherwise it drops into draw mode, forces the OUTLINE active (so no
    /// carved-area position prompt can coexist with the point's), and is
    /// mutually exclusive with living-area / exposure / phase / floor-step /
    /// ceiling-step modes. Leaving clears any half-finished point.
    pub fn set_roof_point_mode(&mut self, on: bool) {
        // Leaving the outline draw tool: drop any un-decided re-draw stroke
        // (a stray second outline) so it doesn't linger on the canvas. (n3)
        self.pending_redraw = None;
        if on && !self.ceiling_point_available() {
            return;
        }
        self.ceiling_point_mode = on;
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.roof_ridge_notice = None;
        self.pending_roof_rect = None;
        self.dormer_mode = false;
        self.pending_rect_is_dormer = false;
        if on {
            self.exit_openings_mode();
            self.living_area_mode = false;
            self.exposure_mode = false;
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            // A surviving cut-pick would fire on the SAME tap as the roof
            // point (its choose_* methods have no mode guard), grafting an
            // in-flight shape under the point — clear them all.
            self.pending_cut = None;
            self.pending_phase_cut = None;
            self.pending_step_cut = None;
            // Drop any replayable snap tags from earlier strokes: their
            // choose/un-snap REPLAYS the outline action, which could move the
            // wall a pending point just snapped onto out from under it.
            self.last_snap_action = None;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
    }

    /// Canvas tap dispatch for the ceiling-point flow: with the mode on and
    /// nothing pending, the tap places the point. Validated cheaply against
    /// the outline ([`StoreyDoc::validate_roof_point`] — the resolved-ceilings
    /// check runs at assign); then each axis tries the elective wall snap
    /// ([`StoreyDoc::snap_ceiling_step_line`], reused per axis — both snapped
    /// = a corner snap for free), and the first UN-snapped axis opens its
    /// position prompt (X then Y; both snapped → straight to the height).
    pub fn ceiling_point_tap(&mut self, world_x: f32, world_y: f32) {
        if !self.ceiling_point_mode
            || self.pending_roof_point.is_some()
            || self.ceiling_ridge_pending()
            || self.ceiling_rect_pending()
        {
            return;
        }
        // Flush the live buffer to its slot: the validation + snaps read
        // `document.outline`, which is only synced on shape switches.
        self.sync_active();
        let tapped = Point2f32::new(world_x, world_y);
        if let Err(e) = self.document.validate_roof_point(tapped) {
            log::error!("ceiling point: {}", e);
            return;
        }
        let snap_tol = self.snap_tol_ceiling_line();
        let (x, snap_x) =
            match self
                .document
                .snap_roof_point_coord(ParameterAxis::X, tapped.x, snap_tol)
            {
                Some((snapped, outline_wall_vid)) => (
                    snapped,
                    Some(CeilingPointAxisSnap {
                        drawn_coord_m: tapped.x,
                        outline_wall_vid,
                    }),
                ),
                None => (tapped.x, None),
            };
        let (y, snap_y) =
            match self
                .document
                .snap_roof_point_coord(ParameterAxis::Y, tapped.y, snap_tol)
            {
                Some((snapped, outline_wall_vid)) => (
                    snapped,
                    Some(CeilingPointAxisSnap {
                        drawn_coord_m: tapped.y,
                        outline_wall_vid,
                    }),
                ),
                None => (tapped.y, None),
            };
        self.advance_roof_point(
            Point2f32::new(x, y),
            snap_x,
            snap_y,
            &[ParameterAxis::X, ParameterAxis::Y],
        );
    }

    /// Advance the pending point to the next question: the first axis in
    /// `remaining` that is UN-snapped and has a parallel outline reference
    /// opens its position prompt; a snapped axis needs no question (its
    /// coordinate is a reference wall's, exact by construction) and an axis
    /// with no reference auto-resolves (the document logs it — the tapped
    /// coordinate stands). No questions left → the height entry.
    pub(super) fn advance_roof_point(
        &mut self,
        point: Point2f32,
        snap_x: Option<CeilingPointAxisSnap>,
        snap_y: Option<CeilingPointAxisSnap>,
        remaining: &[ParameterAxis],
    ) {
        for &axis in remaining {
            let (snapped, coord) = match axis {
                ParameterAxis::X => (snap_x.is_some(), point.x),
                ParameterAxis::Y => (snap_y.is_some(), point.y),
            };
            if snapped {
                continue;
            }
            if let Some(prompt) = self
                .document
                .ceiling_line_position_prompt(axis, coord, None)
            {
                self.pending_roof_point = Some(PendingCeilingPoint::Position {
                    point,
                    axis,
                    prompt,
                    snap_x,
                    snap_y,
                });
                return;
            }
            // None → logged auto-resolve: the tapped coordinate is accepted.
        }
        self.pending_roof_point = Some(PendingCeilingPoint::Height {
            point,
            snap_x,
            snap_y,
        });
    }

    /// The snap tag's "Don't snap": revert the pending point to its DRAWN
    /// coordinates on BOTH axes, drop the snaps, and RESTART the question
    /// flow from X — an un-snapped axis's skipped position question must
    /// reopen, and re-asking an already-answered one is cheaper than
    /// remembering which answers a half-snapped flow had. Deliberately NOT
    /// routed through the generic `choose_snap`/`unsnap_last` replay — those
    /// rewind history and re-run a stroke, but the pending point lives
    /// OUTSIDE history. No-op while un-snapped or nothing is pending.
    pub fn unsnap_roof_point(&mut self) {
        let Some(pending) = self.pending_roof_point.take() else {
            return;
        };
        let (point, snap_x, snap_y) = match pending {
            PendingCeilingPoint::Position {
                point,
                snap_x,
                snap_y,
                ..
            }
            | PendingCeilingPoint::Height {
                point,
                snap_x,
                snap_y,
            } => (point, snap_x, snap_y),
        };
        if snap_x.is_none() && snap_y.is_none() {
            // Not snapped — put the stage back untouched.
            self.pending_roof_point = Some(pending);
            return;
        }
        let drawn = Point2f32::new(
            snap_x.map_or(point.x, |s| s.drawn_coord_m),
            snap_y.map_or(point.y, |s| s.drawn_coord_m),
        );
        log::debug!(
            "ceiling point un-snapped from outline wall(s) x: {:?} / y: {:?} back to the \
             drawn ({:.3}, {:.3}); restarting from X",
            snap_x.map(|s| s.outline_wall_vid),
            snap_y.map(|s| s.outline_wall_vid),
            drawn.x,
            drawn.y
        );
        self.advance_roof_point(drawn, None, None, &[ParameterAxis::X, ParameterAxis::Y]);
    }

    /// The pending point's wall snap(s) as ONE standard canvas snap tag at
    /// the point itself — the ceiling-step line's single-tag pattern.
    /// `options` is EMPTY and `skippable` is true — the tag's menu offers
    /// exactly "Don't snap", which the UI routes to
    /// [`unsnap_roof_point`](Self::unsnap_roof_point) (NOT the generic
    /// choose-snap replay — the point is outside history). Both axes snapped
    /// shows as a corner snap. `None` while the point isn't snapped on either
    /// axis or nothing is pending.
    pub fn ceiling_point_snap_tag(&self) -> Option<SnapTagView> {
        let (point, snap_x, snap_y) = match self.pending_roof_point.as_ref()? {
            PendingCeilingPoint::Position {
                point,
                snap_x,
                snap_y,
                ..
            }
            | PendingCeilingPoint::Height {
                point,
                snap_x,
                snap_y,
            } => (*point, *snap_x, *snap_y),
        };
        if snap_x.is_none() && snap_y.is_none() {
            return None;
        }
        // The tag's `from` identity is the drawn point (where the point would
        // return on un-snap).
        let from = (
            snap_x.map_or(point.x, |s| s.drawn_coord_m),
            snap_y.map_or(point.y, |s| s.drawn_coord_m),
        );
        let kind = if snap_x.is_some() && snap_y.is_some() {
            manip_plan::SnapKind::ToCorner
        } else {
            manip_plan::SnapKind::ToParallelWall
        };
        Some(SnapTagView {
            pos: (point.x, point.y),
            from,
            current_kind: Some(kind),
            options: Vec::new(),
            skippable: true,
        })
    }

    /// Assign the apex height to the pending point: materialize (the
    /// validation runs against the RESOLVED ceilings), validate
    /// ([`document::validate_roof_apex`] — strictly inside one FLAT ceiling
    /// region, height that actually slopes it), then stamp the DURABLE input
    /// (`Ber::add_ceiling_apex`), re-derive, and CANONICALIZE the session
    /// ceiling-step list from the derived ceilings (a fan over a stepped piece
    /// re-emits the step as the fan's triangles — the in-session list must
    /// equal the post-reload reconstruction). The pending point stays open on
    /// rejection so the user can retype. Outside undo —
    /// phases/steps/ceiling-steps parity.
    pub fn assign_roof_apex(&mut self, z_m: f32) -> Result<(), String> {
        let Some(PendingCeilingPoint::Height { point, .. }) = self.pending_roof_point.as_ref()
        else {
            return Ok(()); // nothing awaiting a height — no-op
        };
        let p = Point2f64::new(point.x as f64, point.y as f64);
        let idx = self.current_storey_idx.unwrap_or(0);
        // Bring the derived ceilings current — the apex is validated against the
        // resolved ceiling regions, not the editing buffer.
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a ceiling point to")?;
        document::validate_roof_apex(storey, p, z_m as f64)?;
        self.current_record
            .as_mut()
            .expect("checked above")
            .data
            .add_ceiling_apex(
                idx,
                CeilingApex {
                    point: p,
                    z_m: z_m as f64,
                },
            );
        self.pending_roof_point = None;
        self.mark_dirty()?;
        // Regions-as-truth: recompose the ceilings from the durable apexes (+ the
        // session steps) ONCE, then re-derive. The session ceiling-step list is
        // a display cache, refreshed from the resulting ceilings.
        self.recompose_ceilings(idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// Discard the in-flight ceiling point at any stage.
    pub fn cancel_pending_roof_point(&mut self) {
        self.pending_roof_point = None;
    }

    /// The current storey's committed ceiling apexes as `(position, label)` in
    /// world coords (label e.g. "▲ 4.20 m") for the UI's markers + the
    /// toolbar list. Index-aligned with deletion (see
    /// [`delete_ceiling_apex`](Self::delete_ceiling_apex)).
    pub fn ceiling_apex_views(&self) -> Vec<((f64, f64), String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        self.current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .map(|s| {
                s.ceiling_apexes
                    .iter()
                    .map(|a| ((a.point.x, a.point.y), format!("▲ {:.2} m", a.z_m)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Delete the current storey's ceiling apex at `index`
    /// (`Ber::remove_roof_apex`) and re-derive — the fan it raised
    /// re-flattens to its base on the next derive. Canonicalizes the session
    /// ceiling-step list afterwards, same as assign: the fan's triangle step
    /// pieces re-merge into the derived flat pieces. No-op if out of range.
    pub fn delete_ceiling_apex(&mut self, index: usize) {
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = record.data.storeys.get(idx) else {
            return;
        };
        if index >= storey.ceiling_apexes.len() {
            return;
        }
        let _ = storey; // index checked above; recompose rebuilds from inputs
        record.data.remove_roof_apex(idx, index);
        let _ = self.mark_dirty();
        self.recompose_ceilings(idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
    }

    /// True while one axis's position question is up (shows as the shared
    /// position panel, kind `Point`).
    pub fn ceiling_point_position_pending(&self) -> bool {
        matches!(
            self.pending_roof_point,
            Some(PendingCeilingPoint::Position { .. })
        )
    }

    /// True while the resolved point awaits its apex height.
    pub fn ceiling_point_height_pending(&self) -> bool {
        matches!(
            self.pending_roof_point,
            Some(PendingCeilingPoint::Height { .. })
        )
    }

    /// True while ANY ceiling-point input is in flight. The UI uses this to keep
    /// the toolbar reachable with the mode off and to suppress stroke capture
    /// while a stage is pending.
    pub fn ceiling_point_pending(&self) -> bool {
        self.pending_roof_point.is_some()
    }

    /// The pending point's current position (world coords) for the UI's
    /// provisional dot marker. `None` when nothing is pending.
    pub fn pending_roof_point_position(&self) -> Option<(f64, f64)> {
        let (PendingCeilingPoint::Position { point, .. }
        | PendingCeilingPoint::Height { point, .. }) = self.pending_roof_point.as_ref()?;
        Some((point.x as f64, point.y as f64))
    }
}
