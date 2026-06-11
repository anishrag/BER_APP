//! Dimensioning workflow for `AppController`: the prompt loop, applying a
//! measured length (or a zero-collapse), and starting/advancing dimensioning.
//! Split out of the controller core; see "Responsibility placement" in CLAUDE.md.
use super::*;

impl AppController {
    /// Snapshot of the current dimension prompt for the UI to render, or
    /// `None` when nothing is being asked.
    pub fn current_dimension_prompt(&self) -> Option<DimensionPrompt> {
        self.document.dimensioning.current_prompt.clone()
    }

    /// True when there's an active dimension prompt waiting for the user.
    pub fn dimensioning_active(&self) -> bool {
        self.document.dimensioning.current_prompt.is_some()
    }

    /// True once the user has entered at least one dimension. Until then the
    /// polygon is in pixel space; after the first dimension lands, everything
    /// converts to real units in one scale_all() pass.
    pub fn dimensioning_scale_set(&self) -> bool {
        self.document.dimensioning.scale_set
    }

    /// Resolved positions (ring index, edge-start vertex index) of every wall
    /// the user has explicitly dimensioned. The UI uses this to decide which
    /// wall length labels to render — derived/un-set walls show no label.
    pub fn dimensioned_walls(&self) -> Vec<(usize, usize)> {
        self.document
            .dimensioning
            .dimensioned_walls
            .iter()
            .filter_map(|vid| self.document.floor_plan.find_vertex(*vid))
            .collect()
    }

    /// Every wall whose length is currently *known* — meaning all parameters
    /// in its length expression are in `known_params`. This is the superset
    /// of `dimensioned_walls`: it also catches walls whose length is implied
    /// by equality (e.g. the opposite side of a rectangle, after one side
    /// was dimensioned) or by sums/differences of already-set parameters.
    /// The UI uses this to label every wall whose length can be displayed.
    pub fn walls_with_known_length(&self) -> Vec<(usize, usize)> {
        self.document
            .floor_plan
            .walls_with_known_length(&self.document.dimensioning.known_params)
    }

    /// Resolved view of the current prompt for the UI: world-coord endpoints
    /// of the primary wall + all length-equivalent walls + current length.
    /// None when there's no active prompt.
    pub fn dimension_prompt_for_ui(&self) -> Option<DimensionPromptUiData> {
        let prompt = self.document.dimensioning.current_prompt.as_ref()?;
        let (ring, edge_start) = self
            .document
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)?;
        let polygons = self.document.floor_plan.polygons();
        let current_length = self.document.floor_plan.wall_length(ring, edge_start)?;

        // Equivalent walls = primary + any other walls with the same length
        // expression. Ensure the primary is first.
        let mut equiv = self
            .document
            .floor_plan
            .equivalent_length_walls(ring, edge_start);
        if let Some(pos) = equiv.iter().position(|w| *w == (ring, edge_start)) {
            if pos != 0 {
                equiv.swap(0, pos);
            }
        } else {
            equiv.insert(0, (ring, edge_start));
        }

        let walls: Vec<WallPositions> = equiv
            .into_iter()
            .filter_map(|(r, e)| {
                let pts = polygons.get(r)?;
                let nr = pts.len();
                if nr < 2 {
                    return None;
                }
                let pa = pts.get(e)?;
                let pb = pts.get((e + 1) % nr)?;
                Some(WallPositions {
                    wall_a: (pa.x as f64, pa.y as f64),
                    wall_b: (pb.x as f64, pb.y as f64),
                })
            })
            .collect();

        if walls.is_empty() {
            return None;
        }
        Some(DimensionPromptUiData {
            walls,
            current_length,
        })
    }

    /// Resolved view of the current POSITION prompt for the UI: world-coord
    /// endpoints of the measured area wall + the outline reference wall, the
    /// axis, and the current (drawn) distance to prefill the textbox with.
    /// `None` when there's no active position prompt.
    ///
    /// CRITICAL: the prompt's two `VertexId`s index DIFFERENT plans — the area
    /// wall resolves against the LIVE editing buffer (`document.floor_plan`),
    /// the reference wall against the storey outline plan
    /// (`document.outline.floor_plan`).
    pub fn position_prompt_for_ui(&self) -> Option<PositionPromptUiData> {
        // A pending ceiling-step LINE outranks the document's shape-position
        // loop (it can't coexist with one anyway — ceiling-step mode forces
        // the outline active). The "area wall" is the line itself (its span
        // endpoints); the reference wall resolves in the OUTLINE plan. Kind
        // `Line` is part of the prompt identity, so the UI prefill refills
        // when a shape prompt follows a line prompt (or vice versa).
        if let Some(PendingCeilingStep::Position { axis, prompt, .. }) =
            self.pending_ceiling_step.as_ref()
        {
            let option = prompt.options.get(prompt.current_idx)?;
            let (a, b) = self.pending_ceiling_step_line()?;
            return Some(PositionPromptUiData {
                area_wall: WallPositions {
                    wall_a: a,
                    wall_b: b,
                },
                reference_wall: wall_positions_in(
                    &self.document.outline.floor_plan,
                    option.outline_wall_vid,
                )?,
                axis: *axis,
                kind: PositionPromptKind::Line,
                current_distance: option.current_distance,
                option_idx: prompt.current_idx,
                option_count: prompt.options.len(),
            });
        }
        // A pending roof point's per-axis question outranks the shape loop
        // the same way (roof-point mode forces the outline active too). The
        // "area wall" is the POINT itself — a degenerate wall whose two
        // endpoints coincide; the canvas's highlight + arrow degrade
        // gracefully to a dot + an arrow to it. Kind `Point` keeps the UI
        // prefill identity distinct from line and shape prompts.
        if let Some(PendingRoofPoint::Position {
            point,
            axis,
            prompt,
            ..
        }) = self.pending_roof_point.as_ref()
        {
            let option = prompt.options.get(prompt.current_idx)?;
            let p = (point.x as f64, point.y as f64);
            return Some(PositionPromptUiData {
                area_wall: WallPositions {
                    wall_a: p,
                    wall_b: p,
                },
                reference_wall: wall_positions_in(
                    &self.document.outline.floor_plan,
                    option.outline_wall_vid,
                )?,
                axis: *axis,
                kind: PositionPromptKind::Point,
                current_distance: option.current_distance,
                option_idx: prompt.current_idx,
                option_count: prompt.options.len(),
            });
        }
        // A pending roof RIDGE's cross-coordinate question (question 1), same
        // shape as the ceiling-step line's (it reuses that prompt machinery):
        // the "area wall" is the ridge SEGMENT itself (its extent endpoints),
        // the reference wall resolves in the OUTLINE plan, kind `Line`.
        if let Some(PendingRoofRidge::CrossPosition { axis, prompt, .. }) =
            self.pending_roof_ridge.as_ref()
        {
            let option = prompt.options.get(prompt.current_idx)?;
            let (a, b) = self.pending_roof_ridge_line()?;
            return Some(PositionPromptUiData {
                area_wall: WallPositions {
                    wall_a: a,
                    wall_b: b,
                },
                reference_wall: wall_positions_in(
                    &self.document.outline.floor_plan,
                    option.outline_wall_vid,
                )?,
                axis: *axis,
                kind: PositionPromptKind::Line,
                current_distance: option.current_distance,
                option_idx: prompt.current_idx,
                option_count: prompt.options.len(),
            });
        }
        // The ridge's START question (question 2): the measured object is the
        // extent's LO ENDPOINT — a degenerate "area wall" whose two endpoints
        // coincide (the roof-point pattern: the canvas highlight + arrow
        // degrade gracefully to a dot + an arrow to it). The prompt's axis is
        // the ALONG axis, so the identity key (axis, kind, option) still
        // refreshes the prefill after the cross question.
        if let Some(PendingRoofRidge::StartPosition { prompt, .. }) =
            self.pending_roof_ridge.as_ref()
        {
            let option = prompt.options.get(prompt.current_idx)?;
            let (a, _) = self.pending_roof_ridge_line()?;
            return Some(PositionPromptUiData {
                area_wall: WallPositions {
                    wall_a: a,
                    wall_b: a,
                },
                reference_wall: wall_positions_in(
                    &self.document.outline.floor_plan,
                    option.outline_wall_vid,
                )?,
                axis: prompt.axis,
                kind: PositionPromptKind::Line,
                current_distance: option.current_distance,
                option_idx: prompt.current_idx,
                option_count: prompt.options.len(),
            });
        }
        let prompt = self
            .document
            .dimensioning
            .current_position_prompt
            .as_ref()?;
        let option = prompt.options.get(prompt.current_idx)?;
        Some(PositionPromptUiData {
            area_wall: wall_positions_in(&self.document.floor_plan, option.area_wall_vid)?,
            reference_wall: wall_positions_in(
                &self.document.outline.floor_plan,
                option.outline_wall_vid,
            )?,
            axis: prompt.axis,
            kind: prompt.kind,
            current_distance: option.current_distance,
            option_idx: prompt.current_idx,
            option_count: prompt.options.len(),
        })
    }

    /// Apply the user's entered distance for the current POSITION prompt, then
    /// persist + snapshot for undo. The model mutation (translate / set-param /
    /// guardrail / rollback) lives in [`Document::apply_position`]; this owns
    /// only the shell workflow. On rejection the snapshot is discarded —
    /// `apply_dimension`'s pattern.
    pub fn apply_position_dimension(&mut self, value: f32) -> Result<(), String> {
        // Ceiling-step LINE prompt: shell-resolved. NOTHING in the model moves
        // — the resolved coordinate just rides into the Amount stage — so no
        // undo snapshot and no mark_dirty (there's nothing to re-derive yet).
        if let Some(PendingCeilingStep::Position {
            axis, side, prompt, ..
        }) = self.pending_ceiling_step.as_ref()
        {
            let coord_m = self.document.resolve_ceiling_line_position(prompt, value)?;
            let (axis, side) = (*axis, *side);
            // Position stage implies un-snapped — no snap to carry forward.
            self.pending_ceiling_step = Some(PendingCeilingStep::Amount {
                axis,
                coord_m,
                side,
                snap: None,
            });
            return Ok(());
        }
        // Roof-point per-axis prompt: shell-resolved the same way. NOTHING in
        // the model moves — the resolved coordinate just rides into the next
        // stage (Y's question, or the height) — so no undo snapshot and no
        // mark_dirty (there's nothing to re-derive yet).
        if let Some(PendingRoofPoint::Position {
            point,
            axis,
            prompt,
            snap_x,
            snap_y,
        }) = self.pending_roof_point.as_ref()
        {
            let coord_m = self.document.resolve_ceiling_line_position(prompt, value)?;
            let (mut point, axis, snap_x, snap_y) = (*point, *axis, *snap_x, *snap_y);
            let remaining: &[ParameterAxis] = match axis {
                ParameterAxis::X => {
                    point.x = coord_m;
                    &[ParameterAxis::Y]
                }
                ParameterAxis::Y => {
                    point.y = coord_m;
                    &[]
                }
            };
            self.pending_roof_point = None;
            self.advance_roof_point(point, snap_x, snap_y, remaining);
            return Ok(());
        }
        // Roof-ridge position questions: shell-resolved the same way. NOTHING
        // in the model moves — the resolved coordinate just rides into the
        // next stage — so no undo snapshot and no mark_dirty. The CROSS
        // question (1) resolves `coord_m` (the extent does not shift: it's on
        // the other axis); the START question (2) resolves the extent's `lo`
        // (the length rides unchanged — it's question 3's).
        if let Some(PendingRoofRidge::CrossPosition {
            axis,
            lo,
            len_m,
            start_snap,
            prompt,
            ..
        }) = self.pending_roof_ridge.as_ref()
        {
            let coord_m = self.document.resolve_ceiling_line_position(prompt, value)?;
            let (axis, lo, len_m, start_snap) = (*axis, *lo, *len_m, *start_snap);
            self.pending_roof_ridge = None;
            // Cross stage implies the cross was un-snapped — none to carry.
            self.advance_roof_ridge(
                axis,
                coord_m,
                lo,
                len_m,
                None,
                start_snap,
                &[RidgeQuestion::Start],
            );
            return Ok(());
        }
        if let Some(PendingRoofRidge::StartPosition {
            axis,
            coord_m,
            len_m,
            cross_snap,
            prompt,
            ..
        }) = self.pending_roof_ridge.as_ref()
        {
            let lo = self.document.resolve_ceiling_line_position(prompt, value)?;
            let (axis, coord_m, len_m, cross_snap) = (*axis, *coord_m, *len_m, *cross_snap);
            self.pending_roof_ridge = None;
            // Start stage implies the start was un-snapped — none to carry.
            self.advance_roof_ridge(axis, coord_m, lo, len_m, cross_snap, None, &[]);
            return Ok(());
        }
        let undo_snap = self.current_history_snapshot();
        self.document.apply_position(value)?;
        self.mark_dirty()?;
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// Skip the current position prompt — accept the drawn position (see
    /// [`Document::skip_current_position`]); record it for undo first.
    pub fn skip_position_dimension(&mut self) {
        // Ceiling-step LINE prompt: accept the DRAWN coordinate as-is and
        // advance to the amount. No history / no mark_dirty — nothing moved.
        if let Some(PendingCeilingStep::Position {
            axis,
            coord_m,
            side,
            ..
        }) = self.pending_ceiling_step.as_ref()
        {
            let (axis, coord_m, side) = (*axis, *coord_m, *side);
            // Position stage implies un-snapped — no snap to carry forward.
            self.pending_ceiling_step = Some(PendingCeilingStep::Amount {
                axis,
                coord_m,
                side,
                snap: None,
            });
            return;
        }
        // Roof-point per-axis prompt: accept the TAPPED coordinate as-is and
        // advance (Y's question, or the height). No history / no mark_dirty —
        // nothing moved.
        if let Some(PendingRoofPoint::Position {
            point,
            axis,
            snap_x,
            snap_y,
            ..
        }) = self.pending_roof_point.as_ref()
        {
            let (point, axis, snap_x, snap_y) = (*point, *axis, *snap_x, *snap_y);
            let remaining: &[ParameterAxis] = match axis {
                ParameterAxis::X => &[ParameterAxis::Y],
                ParameterAxis::Y => &[],
            };
            self.pending_roof_point = None;
            self.advance_roof_point(point, snap_x, snap_y, remaining);
            return;
        }
        // Roof-ridge position questions: accept the DRAWN value as-is and
        // advance — the cross question (1) on to the start question, the
        // start question (2) on to the length. No history / no mark_dirty —
        // nothing moved.
        if let Some(PendingRoofRidge::CrossPosition {
            axis,
            coord_m,
            lo,
            len_m,
            start_snap,
            ..
        }) = self.pending_roof_ridge.as_ref()
        {
            let (axis, coord_m, lo, len_m, start_snap) =
                (*axis, *coord_m, *lo, *len_m, *start_snap);
            self.pending_roof_ridge = None;
            self.advance_roof_ridge(
                axis,
                coord_m,
                lo,
                len_m,
                None,
                start_snap,
                &[RidgeQuestion::Start],
            );
            return;
        }
        if let Some(PendingRoofRidge::StartPosition {
            axis,
            coord_m,
            lo,
            len_m,
            cross_snap,
            ..
        }) = self.pending_roof_ridge.as_ref()
        {
            let (axis, coord_m, lo, len_m, cross_snap) =
                (*axis, *coord_m, *lo, *len_m, *cross_snap);
            self.pending_roof_ridge = None;
            self.advance_roof_ridge(axis, coord_m, lo, len_m, cross_snap, None, &[]);
            return;
        }
        self.record_history();
        self.document.skip_current_position();
        let _ = self.mark_dirty();
    }

    /// Advance the position prompt to its next outline reference wall (wraps).
    /// Not a mutation of the model's geometry — no history entry.
    pub fn cycle_position_reference(&mut self) {
        // Ceiling-step LINE prompt: cycle ITS reference options.
        if let Some(PendingCeilingStep::Position { prompt, .. }) =
            self.pending_ceiling_step.as_mut()
        {
            prompt.cycle();
            return;
        }
        // Roof-point per-axis prompt: cycle ITS reference options.
        if let Some(PendingRoofPoint::Position { prompt, .. }) = self.pending_roof_point.as_mut() {
            prompt.cycle();
            return;
        }
        // Roof-ridge position questions (cross or start): cycle the open
        // one's reference options.
        if let Some(
            PendingRoofRidge::CrossPosition { prompt, .. }
            | PendingRoofRidge::StartPosition { prompt, .. },
        ) = self.pending_roof_ridge.as_mut()
        {
            prompt.cycle();
            return;
        }
        self.document.cycle_position_reference();
    }

    /// Apply the user's entered length for the current dimension prompt, then
    /// persist + snapshot for undo. The model mutation (scale / set / repair /
    /// guardrail / rollback) lives in [`Document::apply_dimension`]; this owns
    /// only the shell workflow. On rejection the snapshot is discarded.
    pub fn apply_dimension(&mut self, value: f32) -> Result<(), String> {
        let undo_snap = self.current_history_snapshot();
        self.document.apply_dimension(value)?;
        self.warn_if_non_orthogonal("dimension");
        self.mark_dirty()?;
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// Skip the current dimension prompt without entering a value (see
    /// [`Document::skip_current_dimension`]); record it for undo.
    pub fn skip_current_dimension(&mut self) {
        self.record_history();
        self.document.skip_current_dimension();
    }

    /// Recompute `current_prompt` based on which params are still unknown — see
    /// [`Document::recompute_prompt`].
    pub(super) fn compute_next_dimension_prompt(&mut self) {
        self.document.recompute_prompt();
    }

    /// Kick off dimensioning if nothing else is in the way (no pending redraw,
    /// no in-flight collinearity review). No-op otherwise.
    pub(super) fn maybe_start_dimensioning(&mut self) {
        if self.review.is_some()
            || self.pending_redraw.is_some()
            || self.document.dimensioning.current_prompt.is_some()
        {
            return;
        }
        // Collapse any zero-length edges + collinear vertices that the
        // collinearity review may have introduced (e.g. when the user marked
        // a top-left nook's edges as collinear with the polygon's outer top
        // edge, the nook's vertices end up at the same point as their
        // neighbours). Done before dimensioning starts so we don't prompt
        // for dimensions on degenerate walls. Only safe while scale isn't
        // set — afterwards the parametric IDs are referenced by known_params
        // and a rebuild would dangle them.
        if !self.document.dimensioning.scale_set {
            self.document.floor_plan.simplify_degenerate();
            if let Err(e) = self.mark_dirty() {
                eprintln!("commit_to_ber after simplify_degenerate: {}", e);
            }
        }
        self.compute_next_dimension_prompt();
    }

    /// After a topology change (trim-collapse, collinear-merge cleanup, arc
    /// splice), drop dimensioning state that no longer refers to a live
    /// param/vertex: `known_params` to currently-active params, and
    /// `dimensioned_walls` / `skipped_walls` to vertices that still exist. Safe
    /// to over-call — pruning an already-valid set is a no-op.
    pub(super) fn resync_dimensioning_to_plan(&mut self) {
        self.document.resync_dimensioning();
    }
}

/// UI-friendly snapshot of the current dimension prompt. `walls[0]` is the
/// wall the prompt was generated for ("primary"); subsequent entries are
/// other walls whose length expression is identical — the user can measure
/// any of them and enter the same value. World coordinates here are the same
/// coord system as the rendered BER polygon; the renderer transforms via
/// its `Transform2d`.
#[derive(Debug, Clone)]
pub struct DimensionPromptUiData {
    pub walls: Vec<WallPositions>,
    /// Current length of the primary wall in the plan's current units.
    pub current_length: f32,
}

/// UI-friendly snapshot of the current POSITION prompt: the area wall being
/// measured (live buffer coords) + the storey-outline reference wall it's
/// measured from, the axis the distance runs along, the current (drawn)
/// distance to prefill the textbox with, and which reference option is
/// selected out of how many. Same coord system as the rendered BER polygon.
#[derive(Debug, Clone, Copy)]
pub struct PositionPromptUiData {
    pub area_wall: WallPositions,
    pub reference_wall: WallPositions,
    pub axis: ParameterAxis,
    /// The prompt's kind — part of its IDENTITY: two consecutive `Wall`
    /// prompts can share `(axis, option_idx)`, so the prefill key must
    /// include this or the second prompt inherits the first one's text.
    pub kind: PositionPromptKind,
    pub current_distance: f32,
    pub option_idx: usize,
    pub option_count: usize,
}

/// World-coord endpoints of the wall starting at `vid` in `plan`. `None` when
/// the vertex no longer exists (stale prompt) or the ring is degenerate.
fn wall_positions_in(plan: &FloorPlan, vid: VertexId) -> Option<WallPositions> {
    let (ring, edge_start) = plan.find_vertex(vid)?;
    let polygons = plan.polygons();
    let pts = polygons.get(ring)?;
    let n = pts.len();
    if n < 2 {
        return None;
    }
    let a = pts.get(edge_start)?;
    let b = pts.get((edge_start + 1) % n)?;
    Some(WallPositions {
        wall_a: (a.x as f64, a.y as f64),
        wall_b: (b.x as f64, b.y as f64),
    })
}

// There is NO persisted sketch snapshot. The DB stores only finalised
// (complete, metre-scale) resolved geometry — `Storey.floors` (outline),
// `Storey.living_areas`, and `Storey.walls` (exposure). On reload the editor
// rebuilds everything from those resolved points (see `load_storey_into_editor`):
// the outline's parametric model is re-derived (re-edit fidelity — shared
// params, collinearity equivalences, curve arcs — is deliberately not
// preserved), living areas are rebuilt non-editable, and exposure is recovered
// from the wall regions.
