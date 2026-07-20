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
use ber_model::ber::structural::{CeilingApex, CeilingRegions};
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

/// Which CONTINUE-ceiling TOOL is active — two Heights-row toggles, modes
/// like the dormer/roof tools:
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ContinuationTool {
    /// "Neighbour ceiling": target region, then a neighbouring source
    /// region on the SAME storey.
    Neighbour,
    /// "Ceiling from storey": target region, then the storey above/below
    /// (auto-resolved when only one exists).
    Storey,
}

/// One labelled cross-storey source a from-storey continuation could take —
/// a lateral (plan-neighbouring) region on the ±1 storey. Presented as a
/// button when the target has SEVERAL candidates (field report deb23cbf
/// follow-up: the user chooses which neighbour to continue from, not a
/// silent nearest-pick). `offset` is the ±1 storey; `anchor` the stable
/// stored source point; `label` the button text ("From Storey 1 (north
/// area)").
#[derive(Debug, Clone)]
pub(super) struct ContinuationSourceOption {
    pub offset: i8,
    pub anchor: Point2f64,
    pub label: String,
}

/// The in-flight CONTINUE-from pick, advanced tap by tap while a
/// [`ContinuationTool`] is on: `PickTarget` (tap the region whose surface
/// changes; tapping an already-continued region CLEARS its links instead) →
/// either `PickSource` (the Neighbour tool with several same-storey
/// candidates: tap which one) or `ChooseSource` (the From-storey tool with
/// several cross-storey candidates: pick a labelled button) → commit → back
/// to `PickTarget` (paint-style — the assessor may continue several areas —
/// until the tool toggles off). A SINGLE candidate skips the choice and
/// commits straight from `PickTarget` (the fast path). Session-only,
/// outside undo; cleared on storey switch and mode exit. The DURABLE record
/// is `Storey::ceiling_continuations`.
#[derive(Debug, Clone)]
pub(super) enum PendingCeilingContinuation {
    PickTarget,
    PickSource {
        target: Point2f64,
    },
    ChooseSource {
        target: Point2f64,
        options: Vec<ContinuationSourceOption>,
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
        self.continuation_tool = None;
        self.pending_ceiling_continuation = None;
        self.reset_ceiling_confirm = false;
        self.dormer_mode = false;
        self.pending_rect_is_dormer = false;
        if on {
            self.exit_openings_mode();
            self.living_area_mode = false;
            self.porch_mode = false;
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
        // A CONTINUE-ceiling tool owns every tap while it is on.
        if self.continuation_tool.is_some() {
            self.ceiling_continuation_tap(world_x as f64, world_y as f64);
            return;
        }
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

    /// The fan-vertex markers for the canvas, `(position, fanned, anchor)`
    /// in world coords — non-empty only in the ceiling-shape editing context
    /// (roof-point mode) and only when actionable
    /// ([`document::ceiling_fan_markers`] owns the rules). `anchor` marks a
    /// gable side's CURRENT pitch anchor (rendered distinctly). The UI
    /// renders a tappable marker per entry and routes a hit to
    /// [`toggle_ceiling_fan_vertex`](Self::toggle_ceiling_fan_vertex).
    pub fn ceiling_fan_marker_views(&self) -> Vec<((f64, f64), bool, bool)> {
        if !self.ceiling_point_mode {
            return Vec::new(); // markers only during the editing context
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        self.current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .map(|storey| {
                document::ceiling_fan_markers(storey)
                    .into_iter()
                    .map(|m| ((m.pos.x, m.pos.y), m.fanned, m.anchor))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// A fan-marker TAP at `(world_x, world_y)` — the thin router over
    /// [`document::ceiling_fan_tap_action`], which resolves what the tap
    /// means against the current solve:
    ///
    /// - hip/fan marker → toggle the vertex in the stored UNFANNED set
    ///   (unfan folds its fan plane into the neighbours'; re-fan restores);
    /// - gable embedded vertex → make it that side's PITCH ANCHOR (store the
    ///   override, dropping the side's previous one);
    /// - gable current anchor → clear the side's override (auto-nearest).
    ///
    /// A tap on no marker / an ineligible vertex is a logged no-op. Then the
    /// standard ceiling-edit tail: mark dirty, recompose the ceilings from
    /// the durable inputs, re-derive, canonicalize the session step list.
    /// Outside undo — phases/steps/ceiling-points parity.
    pub fn toggle_ceiling_fan_vertex(&mut self, world_x: f64, world_y: f64) {
        if !self.ceiling_point_mode {
            return;
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return;
        };
        // The UI hit-tests in screen space and sends the marker's own world
        // position — 1 mm of slack absorbs the f64→f32→f64 event plumbing.
        let p = Point2f64::new(world_x, world_y);
        let Some(action) = document::ceiling_fan_tap_action(storey, p, 1e-3) else {
            log::debug!("fan-marker tap at ({world_x:.3}, {world_y:.3}) resolves to no action");
            return;
        };
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let applied = match &action {
            document::CeilingFanTapAction::ToggleUnfanned(v) => {
                record.data.toggle_ceiling_unfanned(idx, *v).is_some()
            }
            document::CeilingFanTapAction::SetPlaneAnchor { anchor, drop } => record
                .data
                .update_ceiling_plane_anchors(idx, drop, Some(*anchor))
                .is_some(),
            document::CeilingFanTapAction::ClearPlaneAnchor { drop } => record
                .data
                .update_ceiling_plane_anchors(idx, drop, None)
                .is_some(),
        };
        if !applied {
            return;
        }
        log::debug!("fan-marker tap applied: {action:?}");
        let _ = self.mark_dirty();
        self.recompose_ceilings(idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
    }

    /// The standard ceiling-edit tail shared by the reset / continuation
    /// mutations: mark dirty, recompose the ceilings from the durable
    /// inputs, re-derive, canonicalize the session step list. Outside undo —
    /// ceiling-edit parity.
    fn ceiling_edit_tail(&mut self, idx: usize) {
        let _ = self.mark_dirty();
        self.recompose_ceilings(idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
    }

    // ── Reset heights (ceiling shape → flat-at-default) ─────────────────────

    /// True while the destructive "Reset heights" button is ARMED (first tap
    /// done, awaiting the confirming second tap).
    pub fn reset_ceiling_shape_pending(&self) -> bool {
        self.reset_ceiling_confirm
    }

    /// First tap of "Reset heights": arm the confirm. No-op when the tool
    /// gate is closed (no finalised outline / default height).
    pub fn request_reset_ceiling_shape(&mut self) {
        if self.ceiling_point_available() {
            self.reset_ceiling_confirm = true;
        }
    }

    /// Disarm the reset confirm without doing anything.
    pub fn cancel_reset_ceiling_shape(&mut self) {
        self.reset_ceiling_confirm = false;
    }

    /// The confirming second tap of "Reset heights": clear every durable
    /// ceiling-shape input on the ACTIVE storey (`Storey::reset_ceiling_shape`
    /// — steps, face-splits, apexes + hints, unfanned, plane anchors,
    /// continuations; the default height, floor steps and dormers stay),
    /// drop the SESSION step list, and recompose — one flat region at the
    /// default height. Destructive and outside undo (ceiling-edit parity);
    /// the two-step confirm is the guard.
    pub fn confirm_reset_ceiling_shape(&mut self) {
        if !self.reset_ceiling_confirm {
            return;
        }
        self.reset_ceiling_confirm = false;
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let Some(storey) = record.data.storeys.get_mut(idx) else {
            return;
        };
        storey.reset_ceiling_shape();
        // The session ceiling-step list mirrors the durable steps — clear it
        // or the recompose would re-apply the dropped steps from the cache.
        self.ceiling_step_regions.remove(&idx);
        // Any in-flight ceiling stage refers to geometry that just vanished.
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.pending_roof_rect = None;
        self.pending_ceiling_continuation = None;
        self.ceiling_edit_tail(idx);
    }

    // ── Continue ceiling from a neighbouring region / another storey ────────

    /// True while the CONTINUE-from pick is waiting for the TARGET tap.
    pub fn ceiling_continuation_picking_target(&self) -> bool {
        matches!(
            self.pending_ceiling_continuation,
            Some(PendingCeilingContinuation::PickTarget)
        )
    }

    /// True while the CONTINUE-from pick is waiting for the SOURCE — a
    /// neighbour-region tap (`PickSource`) or a labelled cross-storey button
    /// (`ChooseSource`).
    pub fn ceiling_continuation_picking_source(&self) -> bool {
        matches!(
            self.pending_ceiling_continuation,
            Some(PendingCeilingContinuation::PickSource { .. })
                | Some(PendingCeilingContinuation::ChooseSource { .. })
        )
    }

    /// The labelled cross-storey source choices when the from-storey pick is
    /// at its `ChooseSource` stage (else empty) — the UI renders one button
    /// per label, routed to
    /// [`choose_ceiling_continuation_source`](Self::choose_ceiling_continuation_source).
    pub fn ceiling_continuation_source_options(&self) -> Vec<String> {
        match &self.pending_ceiling_continuation {
            Some(PendingCeilingContinuation::ChooseSource { options, .. }) => {
                options.iter().map(|o| o.label.clone()).collect()
            }
            _ => Vec::new(),
        }
    }

    /// The SUCCESS confirmation for the last applied continuation (naming the
    /// storeys and the resulting height range) — `None` while none is
    /// showing. See `ceiling_continuation_confirm`.
    pub fn ceiling_continuation_confirm(&self) -> Option<&str> {
        self.ceiling_continuation_confirm.as_deref()
    }

    /// The plan rings (world metres) of THIS storey's regions that are
    /// TARGETS of a committed ceiling continuation — the persistent "this
    /// area was continued" outline the canvas highlights while a
    /// CONTINUE-ceiling tool is open, so an applied continuation stays
    /// visible (not just tiny corner labels). Empty when no tool is on, no
    /// storey, or nothing is continued.
    pub fn ceiling_continued_region_rings(&self) -> Vec<Vec<(f64, f64)>> {
        if self.continuation_tool.is_none() {
            return Vec::new();
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        let mut seen: Vec<usize> = Vec::new();
        let mut rings: Vec<Vec<(f64, f64)>> = Vec::new();
        for c in &storey.ceiling_continuations {
            let tp = Point2f64::new(c.target.0 as f64, c.target.1 as f64);
            if let Some(&ri) = storey.ceilings.claim_regions(tp).first() {
                if !seen.contains(&ri) {
                    seen.push(ri);
                    rings.push(
                        storey.ceilings.regions[ri]
                            .polygon
                            .exterior
                            .iter()
                            .map(|v| (v.x, v.y))
                            .collect(),
                    );
                }
            }
        }
        rings
    }

    /// True while the "Neighbour ceiling" tool is toggled on.
    pub fn neighbour_continuation_mode(&self) -> bool {
        self.continuation_tool == Some(ContinuationTool::Neighbour)
    }

    /// True while the "Ceiling from storey" tool is toggled on.
    pub fn storey_continuation_mode(&self) -> bool {
        self.continuation_tool == Some(ContinuationTool::Storey)
    }

    /// Toggle the "Neighbour ceiling" tool (continue one ceiling area's
    /// surface from a neighbouring area's plane).
    pub fn set_neighbour_continuation_mode(&mut self, on: bool) {
        self.set_continuation_tool(on.then_some(ContinuationTool::Neighbour));
    }

    /// Toggle the "Ceiling from storey" tool (continue a ceiling area's
    /// surface from a plane BESIDE it on the storey above/below — never
    /// from a region sitting directly over/under it: a plane continues
    /// laterally, not through the slab onto its own footprint).
    pub fn set_storey_continuation_mode(&mut self, on: bool) {
        self.set_continuation_tool(on.then_some(ContinuationTool::Storey));
    }

    /// Enter/leave a CONTINUE-ceiling tool — a Heights-row MODE like the
    /// dormer/roof tools (same gate as the ceiling-shape tools; mutually
    /// exclusive with them and with every other drawing mode). Entering arms
    /// the paint-style PickTarget loop; leaving clears it.
    fn set_continuation_tool(&mut self, tool: Option<ContinuationTool>) {
        self.pending_redraw = None;
        if tool.is_some() && !self.ceiling_point_available() {
            return;
        }
        self.continuation_tool = tool;
        self.pending_ceiling_continuation = tool.map(|_| PendingCeilingContinuation::PickTarget);
        self.ceiling_continuation_notice = None;
        self.ceiling_continuation_confirm = None;
        self.reset_ceiling_confirm = false;
        self.pending_roof_point = None;
        self.pending_roof_ridge = None;
        self.roof_ridge_notice = None;
        self.pending_roof_rect = None;
        self.pending_rect_is_dormer = false;
        if tool.is_some() {
            self.exit_openings_mode();
            self.living_area_mode = false;
            self.porch_mode = false;
            self.exposure_mode = false;
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.ceiling_point_mode = false;
            self.dormer_mode = false;
            self.pending_cut = None;
            self.pending_phase_cut = None;
            self.pending_step_cut = None;
            self.last_snap_action = None;
            self.activate_shape(ActiveShape::Outline);
            self.set_edit_mode(EditTools::DrawPerpWalls);
        }
    }

    /// Step the in-flight pick back to PickTarget (the stage Cancel — the
    /// tool itself stays on; toggling the tool off is the full exit).
    pub fn cancel_ceiling_continuation(&mut self) {
        self.ceiling_continuation_notice = None;
        self.ceiling_continuation_confirm = None;
        if self.continuation_tool.is_some() {
            self.pending_ceiling_continuation = Some(PendingCeilingContinuation::PickTarget);
        } else {
            self.pending_ceiling_continuation = None;
        }
    }

    /// The inline message for the CONTINUE-ceiling tools — why the last pick
    /// tap was refused, or what to do next. NO refused tap is silent (the
    /// deb23cbf live failure: taps that "did nothing"). `None` while the
    /// picks are proceeding normally.
    pub fn ceiling_continuation_notice(&self) -> Option<&str> {
        self.ceiling_continuation_notice.as_deref()
    }

    /// A canvas tap while a CONTINUE-ceiling tool is on (routed from
    /// [`ceiling_point_tap`](Self::ceiling_point_tap)):
    ///
    /// - `PickTarget`: the tapped ceiling region becomes the TARGET — unless
    ///   it already has continuation(s), in which case the tap CLEARS them
    ///   (re-picking a continued region undoes it) and the loop restarts.
    ///   With a target chosen, the SOURCE is resolved by the tool:
    ///   - "Ceiling from storey": the lateral candidate regions across the
    ///     ±1 storeys ([`continuation_source_options`](Self::continuation_source_options)).
    ///     Exactly one → apply straight away (the fast path, with a success
    ///     confirmation); several → the `ChooseSource` stage (labelled
    ///     buttons); none → an inline refusal (why).
    ///   - "Neighbour ceiling": the lateral candidate regions on THIS
    ///     storey. Exactly one → apply; several → the `PickSource` tap
    ///     stage; none → an inline refusal.
    /// - `PickSource` ("Neighbour ceiling", several candidates): the tapped
    ///   region becomes the SOURCE. Tapping the target's own region is a
    ///   surfaced no-op (pick a NEIGHBOUR).
    /// - `ChooseSource` ("Ceiling from storey", several candidates): a
    ///   canvas tap does nothing but points at the buttons.
    ///
    /// Every commit restarts at PickTarget — paint-style, until the tool is
    /// toggled off. No refused tap is silent (the deb23cbf live failure).
    pub fn ceiling_continuation_tap(&mut self, world_x: f64, world_y: f64) {
        let Some(stage) = self.pending_ceiling_continuation.clone() else {
            return;
        };
        let tool = self.continuation_tool;
        let idx = self.current_storey_idx.unwrap_or(0);
        // Every tap is a fresh action: the prior confirmation and inline
        // notice are cleared, re-set below on a commit / any refusal.
        self.ceiling_continuation_notice = None;
        self.ceiling_continuation_confirm = None;
        // The picks resolve against the RESOLVED ceilings.
        self.materialize();
        let p = Point2f64::new(world_x, world_y);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return;
        };
        let region_of = |s: &Storey, q: Point2f64| s.ceilings.claim_regions(q).first().copied();
        match stage {
            PendingCeilingContinuation::PickTarget => {
                let Some(target_region) = region_of(storey, p) else {
                    log::debug!("continuation target tap hit no ceiling region — still picking");
                    self.ceiling_continuation_notice =
                        Some("Tap inside a ceiling area — that tap didn't land in one.".into());
                    return;
                };
                let already = storey.ceiling_continuations.iter().any(|c| {
                    let tp = Point2f64::new(c.target.0 as f64, c.target.1 as f64);
                    region_of(storey, tp) == Some(target_region)
                });
                if already {
                    // The clear affordance: tapping a continued region with
                    // the tool removes its link(s); the loop restarts.
                    let record = self.current_record.as_mut().expect("checked above");
                    let removed = record.data.clear_ceiling_continuations_at(idx, p);
                    log::debug!("cleared {removed} ceiling continuation(s) at the tapped region");
                    self.restart_continuation_pick(idx);
                    return;
                }
                match tool {
                    Some(ContinuationTool::Storey) => {
                        // The lateral candidate sources across the ±1 storeys
                        // (a region directly over/under the target never
                        // qualifies — deb23cbf: a plane continues sideways,
                        // not through the slab onto its own footprint).
                        let options = self.continuation_source_options(p, idx);
                        match options.len() {
                            0 => {
                                self.ceiling_continuation_notice = Some(self.no_source_notice(idx))
                            }
                            1 => {
                                let o = &options[0];
                                self.commit_ceiling_continuation(
                                    idx,
                                    p,
                                    o.anchor,
                                    o.offset,
                                    o.label.clone(),
                                );
                            }
                            _ => {
                                // Several — the user chooses which (buttons).
                                self.pending_ceiling_continuation =
                                    Some(PendingCeilingContinuation::ChooseSource {
                                        target: p,
                                        options,
                                    });
                            }
                        }
                    }
                    _ => {
                        // Neighbour: the lateral candidates on THIS storey.
                        let cands = storey.ceilings.lateral_source_candidates(p);
                        match cands.len() {
                            0 => {
                                self.ceiling_continuation_notice = Some(
                                    "No neighbouring ceiling area to continue from — this is \
                                     the only ceiling area on the storey."
                                        .into(),
                                );
                            }
                            1 => {
                                let anchor = storey.ceilings.region_lateral_anchor(cands[0], p);
                                if let Some(anchor) = anchor {
                                    self.commit_ceiling_continuation(
                                        idx,
                                        p,
                                        anchor,
                                        0,
                                        "a neighbouring area".into(),
                                    );
                                }
                            }
                            _ => {
                                // Several — tap which neighbour to continue from.
                                self.pending_ceiling_continuation =
                                    Some(PendingCeilingContinuation::PickSource { target: p });
                            }
                        }
                    }
                }
            }
            PendingCeilingContinuation::PickSource { target } => {
                // Neighbour tool, several candidates: the source is a tap.
                let (Some(sr), Some(tr)) = (region_of(storey, p), region_of(storey, target)) else {
                    log::debug!("continuation source tap hit no ceiling region — still picking");
                    self.ceiling_continuation_notice = Some(
                        "Tap inside a ceiling area to continue from — that tap didn't land \
                         in one."
                            .into(),
                    );
                    return;
                };
                if sr == tr {
                    log::debug!("continuation source is the target's own region — tap a neighbour");
                    self.ceiling_continuation_notice = Some(
                        "That's the area being changed — tap a NEIGHBOURING ceiling area \
                         to continue from."
                            .into(),
                    );
                    return;
                }
                self.commit_ceiling_continuation(idx, target, p, 0, "a neighbouring area".into());
            }
            PendingCeilingContinuation::ChooseSource { .. } => {
                // From-storey tool, several candidates: the source is a
                // BUTTON choice, not a canvas tap.
                self.ceiling_continuation_notice =
                    Some("Pick the storey to continue from with the buttons in the box.".into());
            }
        }
    }

    /// The labelled LATERAL source candidates for a from-storey continuation
    /// of `target` (current storey `idx`): every lateral region
    /// ([`CeilingRegions::lateral_source_candidates`]) on the ±1 storeys,
    /// each with the stable stored anchor
    /// ([`CeilingRegions::region_lateral_anchor`]) and a button label — the
    /// storey name, plus a plan-position hint ("north area") only when that
    /// storey offers more than one, so the labels stay distinct. Ordered
    /// storey-above then storey-below, nearest region first within each.
    ///
    /// [`CeilingRegions::lateral_source_candidates`]:
    /// ber_model::ber::structural::CeilingRegions::lateral_source_candidates
    /// [`CeilingRegions::region_lateral_anchor`]:
    /// ber_model::ber::structural::CeilingRegions::region_lateral_anchor
    fn continuation_source_options(
        &self,
        target: Point2f64,
        idx: usize,
    ) -> Vec<ContinuationSourceOption> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut out: Vec<ContinuationSourceOption> = Vec::new();
        for offset in [1i8, -1] {
            let src_idx = idx as i64 + offset as i64;
            let Some(su) = usize::try_from(src_idx).ok() else {
                continue;
            };
            let Some(src) = record.data.storeys.get(su) else {
                continue;
            };
            let sname = self
                .storey_name(su)
                .unwrap_or_else(|| format!("Storey {}", su + 1));
            let cands = src.ceilings.lateral_source_candidates(target);
            let multi = cands.len() > 1;
            for ri in cands {
                let Some(anchor) = src.ceilings.region_lateral_anchor(ri, target) else {
                    continue;
                };
                let label = if multi {
                    format!("{sname} ({} area)", region_position_hint(&src.ceilings, ri))
                } else {
                    sname.clone()
                };
                out.push(ContinuationSourceOption {
                    offset,
                    anchor,
                    label,
                });
            }
        }
        out
    }

    /// The inline refusal for a from-storey target with NO lateral source:
    /// names whether there simply is no ±1 storey, or whether the
    /// neighbours' ceilings only sit directly over/under the target (no
    /// region beside it to continue from).
    fn no_source_notice(&self, idx: usize) -> String {
        let exists = |i: i64| {
            self.current_record
                .as_ref()
                .and_then(|r| usize::try_from(i).ok().and_then(|i| r.data.storeys.get(i)))
                .is_some()
        };
        if !exists(idx as i64 + 1) && !exists(idx as i64 - 1) {
            "There is no storey above or below this one to continue a ceiling from.".into()
        } else {
            "The neighbouring storey's ceiling sits directly over/under that area — a \
             ceiling continues from a region BESIDE it, and there is none."
                .into()
        }
    }

    /// Pick one of the labelled from-storey source candidates (the
    /// `ChooseSource` buttons) and apply it. No-op unless that stage is up
    /// or the index is out of range.
    pub fn choose_ceiling_continuation_source(&mut self, option_idx: usize) {
        let Some(PendingCeilingContinuation::ChooseSource { target, options }) =
            self.pending_ceiling_continuation.clone()
        else {
            return;
        };
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(o) = options.get(option_idx) else {
            return;
        };
        self.commit_ceiling_continuation(idx, target, o.anchor, o.offset, o.label.clone());
    }

    /// Store a continuation link (target region ← `source` on the storey at
    /// `offset`), re-derive, restart the pick, and set the SUCCESS
    /// confirmation — the resolved height range over the target, so a
    /// single-neighbour auto-apply is visibly not a no-op (deb23cbf
    /// feedback gap). `source_desc` names the source in the confirmation
    /// ("Storey 1 (north area)" / "a neighbouring area").
    fn commit_ceiling_continuation(
        &mut self,
        idx: usize,
        target: Point2f64,
        source: Point2f64,
        offset: i8,
        source_desc: String,
    ) {
        if let Some(record) = self.current_record.as_mut() {
            record
                .data
                .add_ceiling_continuation(idx, target, source, offset);
        }
        // Re-derive so the resolved z range is current, then confirm.
        self.restart_continuation_pick(idx);
        self.ceiling_continuation_confirm =
            Some(self.continuation_confirm_message(idx, target, &source_desc));
    }

    /// The success-confirmation string for a just-applied continuation:
    /// "Continued <this storey> from <source> — <hi> m → <lo> m" (or
    /// "flat at <z> m") using the target region's RESOLVED height range.
    fn continuation_confirm_message(
        &self,
        idx: usize,
        target: Point2f64,
        source_desc: &str,
    ) -> String {
        let this = self
            .storey_name(idx)
            .unwrap_or_else(|| "this storey".to_string());
        let range = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| {
                s.ceilings
                    .claim_regions(target)
                    .first()
                    .and_then(|&ri| s.ceilings.regions[ri].z_range())
            });
        match range {
            Some((lo, hi)) if hi - lo > 1e-3 => {
                format!("Continued {this} from {source_desc} — {hi:.2} m → {lo:.2} m.")
            }
            Some((z, _)) => format!("Continued {this} from {source_desc} — flat at {z:.2} m."),
            None => format!("Continued {this} from {source_desc}."),
        }
    }

    /// The post-commit tail of a continuation edit: the standard ceiling
    /// edit tail, then back to PickTarget while the tool stays on
    /// (paint-style). Clears the transient notice + confirmation (a fresh
    /// commit re-sets the confirmation right after).
    fn restart_continuation_pick(&mut self, idx: usize) {
        self.ceiling_continuation_notice = None;
        self.ceiling_continuation_confirm = None;
        self.ceiling_edit_tail(idx);
        self.pending_ceiling_continuation = self
            .continuation_tool
            .map(|_| PendingCeilingContinuation::PickTarget);
    }
}

/// A coarse plan-position word for region `ri` within its storey's ceiling —
/// its centroid against the bounding box of all the storey's ceiling
/// regions. Plan y grows DOWNWARD (screen convention: smaller y is "north").
/// Used only to disambiguate several source-region buttons on one storey.
fn region_position_hint(regions: &CeilingRegions, ri: usize) -> String {
    let mut min_x = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    for r in &regions.regions {
        for v in &r.polygon.exterior {
            min_x = min_x.min(v.x);
            max_x = max_x.max(v.x);
            min_y = min_y.min(v.y);
            max_y = max_y.max(v.y);
        }
    }
    let c = regions.regions[ri].centroid();
    let (w, h) = ((max_x - min_x).max(1e-9), (max_y - min_y).max(1e-9));
    let nx = (c.x - (min_x + max_x) / 2.0) / (w / 2.0);
    let ny = (c.y - (min_y + max_y) / 2.0) / (h / 2.0);
    const T: f64 = 0.25;
    if nx.abs() < T && ny.abs() < T {
        return "central".to_string();
    }
    if ny.abs() >= nx.abs() {
        if ny < 0.0 {
            "north"
        } else {
            "south"
        }
    } else if nx < 0.0 {
        "west"
    } else {
        "east"
    }
    .to_string()
}
