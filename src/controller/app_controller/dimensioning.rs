//! Dimensioning workflow for `AppController`: the prompt loop, applying a
//! measured length (or a zero-collapse), and starting/advancing dimensioning.
//! Split out of the controller core; see "Responsibility placement" in CLAUDE.md.
use super::*;
use manip_2d_plan::find_flipped_edge;

impl AppController {
    /// Snapshot of the current dimension prompt for the UI to render, or
    /// `None` when nothing is being asked.
    pub fn current_dimension_prompt(&self) -> Option<DimensionPrompt> {
        self.dimensioning.current_prompt.clone()
    }

    /// True when there's an active dimension prompt waiting for the user.
    pub fn dimensioning_active(&self) -> bool {
        self.dimensioning.current_prompt.is_some()
    }

    /// True once the user has entered at least one dimension. Until then the
    /// polygon is in pixel space; after the first dimension lands, everything
    /// converts to real units in one scale_all() pass.
    pub fn dimensioning_scale_set(&self) -> bool {
        self.dimensioning.scale_set
    }

    /// Resolved positions (ring index, edge-start vertex index) of every wall
    /// the user has explicitly dimensioned. The UI uses this to decide which
    /// wall length labels to render — derived/un-set walls show no label.
    pub fn dimensioned_walls(&self) -> Vec<(usize, usize)> {
        self.dimensioning
            .dimensioned_walls
            .iter()
            .filter_map(|vid| self.floor_plan.find_vertex(*vid))
            .collect()
    }

    /// Every wall whose length is currently *known* — meaning all parameters
    /// in its length expression are in `known_params`. This is the superset
    /// of `dimensioned_walls`: it also catches walls whose length is implied
    /// by equality (e.g. the opposite side of a rectangle, after one side
    /// was dimensioned) or by sums/differences of already-set parameters.
    /// The UI uses this to label every wall whose length can be displayed.
    pub fn walls_with_known_length(&self) -> Vec<(usize, usize)> {
        self.floor_plan
            .walls_with_known_length(&self.dimensioning.known_params)
    }

    /// Resolved view of the current prompt for the UI: world-coord endpoints
    /// of the primary wall + all length-equivalent walls + current length.
    /// None when there's no active prompt.
    pub fn dimension_prompt_for_ui(&self) -> Option<DimensionPromptUiData> {
        let prompt = self.dimensioning.current_prompt.as_ref()?;
        let (ring, edge_start) = self.floor_plan.find_vertex(prompt.wall_start_vertex)?;
        let polygons = self.floor_plan.polygons();
        let current_length = self.floor_plan.wall_length(ring, edge_start)?;

        // Equivalent walls = primary + any other walls with the same length
        // expression. Ensure the primary is first.
        let mut equiv = self.floor_plan.equivalent_length_walls(ring, edge_start);
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

    /// Apply the user's entered length for the current dimension prompt.
    /// First entry: scale the whole plan so this wall has that length.
    /// Subsequent entries: set the prompt's controlled parameter directly.
    pub fn apply_dimension(&mut self, value: f32) -> Result<(), String> {
        // Zero is allowed (and means "collapse this wall" — see below); only
        // negative or non-finite values are rejected.
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "dimension must be a non-negative number (got {})",
                value
            ));
        }
        // Snapshot for undo. Only pushed at the very end on success — any
        // early `return Err(...)` (including internal rollbacks) discards
        // the snapshot.
        let undo_snap = self.current_history_snapshot();
        let prompt = self
            .dimensioning
            .current_prompt
            .clone()
            .ok_or_else(|| "no active dimension prompt".to_string())?;

        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        // Zero length → collapse this wall: drop the endpoint carrying the
        // wall's adjustable parameter so its two neighbours meet directly. This
        // is ONLY allowed for a TRIM remnant — zeroing a regular wall would
        // silently delete a wall of the drawn shape, so that's rejected.
        // The first dimension can't be zero (it sets scale).
        if value == 0.0 {
            // First dimension sets scale, so it can't be zero — that's
            // dimensioning-workflow policy. The geometry (locate the cut point,
            // verify it's a trim cut, remove + validate) lives in the plan.
            if !self.dimensioning.scale_set {
                return Err("the first dimension can't be zero".into());
            }
            self.floor_plan
                .collapse_trim_wall(ring, edge_start, prompt.controlled_param)
                .map_err(|e| format!("{}", e))?;
            // Prune dimensioning state for the removed wall/param.
            let active = self.floor_plan.all_active_params();
            self.dimensioning
                .known_params
                .retain(|p| active.contains(p));
            self.dimensioning
                .dimensioned_walls
                .retain(|vid| self.floor_plan.find_vertex(*vid).is_some());
            self.commit_to_ber()?;
            self.compute_next_dimension_prompt();
            self.push_history_snapshot(undo_snap);
            return Ok(());
        }

        let pre_length = self.floor_plan.wall_length(ring, edge_start);
        println!(
            "dimension #{}: ring {}/edge {} (vertex_id {}, param P{}) → {} m  [was {:.3}, scale_set={}]",
            self.dimensioning.dimensioned_walls.len() + 1,
            ring,
            edge_start,
            prompt.wall_start_vertex,
            prompt.controlled_param,
            value,
            pre_length.unwrap_or(f32::NAN),
            self.dimensioning.scale_set,
        );

        // Snapshot so we can roll back if the dimension produces an invalid
        // (self-intersecting / collapsed / direction-flipped) polygon. The
        // parametric model alone doesn't enforce "all right-side vertical
        // segments sum to the spine height" — that's an emergent property
        // of the polygon being simple. Likewise it doesn't enforce that
        // edges keep going in the same direction; a parameter change can
        // push a coord past a fixed boundary and flip an edge without
        // producing a self-intersection.
        let snapshot = self.floor_plan.clone();
        let scale_set_before = self.dimensioning.scale_set;
        let known_params_before = self.dimensioning.known_params.clone();
        let dimensioned_walls_before = self.dimensioning.dimensioned_walls.clone();
        let edge_signs_before = snapshot.edge_signs();

        // Optimistically mark the just-set param as known up front. The repair
        // logic only scales params NOT in `known_params`; if we delayed the
        // insert until after the validate+repair, the repair would treat the
        // user's param as un-set and scale it down — both clobbering the
        // user's input and preventing convergence (the just-set wall and a
        // sibling wall scale in lockstep, never resolving their overlap).
        // On full rejection we'll roll back to `known_params_before`.
        self.dimensioning
            .known_params
            .insert(prompt.controlled_param);
        self.dimensioning
            .dimensioned_walls
            .push(prompt.wall_start_vertex);

        if !self.dimensioning.scale_set {
            // First dimension — convert the whole plan from pixels to real units
            // in one stroke by scaling every parameter and constant uniformly.
            let pixel_length = self
                .floor_plan
                .wall_length(ring, edge_start)
                .ok_or_else(|| "couldn't read wall length".to_string())?;
            if pixel_length < 1e-6 {
                return Err("wall has zero length, can't compute scale".into());
            }
            let scale = value / pixel_length;
            println!("  first dimension — scaling whole plan by {:.6}", scale);
            self.floor_plan.scale_all(scale);
            self.dimensioning.scale_set = true;
        } else {
            // Subsequent dimensions — adjust just the controlled parameter so
            // the wall takes the new length, preserving its direction along axis.
            if let Err(e) =
                self.floor_plan
                    .set_wall_length(ring, edge_start, value, prompt.controlled_param)
            {
                self.floor_plan = snapshot;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                println!("  → REJECTED (set_wall_length error): {:?}", e);
                return Err(format!("set_wall_length: {:?}", e));
            }
            println!(
                "  set_wall_length: P{} = {:?}",
                prompt.controlled_param,
                self.floor_plan.parameter_value(prompt.controlled_param)
            );
        }

        // After applying the dimension the polygon may be invalid (self-
        // intersecting / collapsed) OR simple-but-inverted: a large value can
        // push an un-set wall past a fixed coord and flip its direction — a
        // silent shape change. Both mean the un-set walls need redistributing.
        // `repair_unset_params` moves ONLY the un-set walls to a valid,
        // inversion-free arrangement, so the rest of the shape accommodates the
        // value rather than a wall flipping inside-out. We reject only if no
        // such arrangement exists (the value can't be honoured without changing
        // the shape's topology) — never just because an un-set wall would have
        // flipped, since that's exactly what the repair is for.
        let dim_axis = self
            .floor_plan
            .param_axis(prompt.controlled_param)
            .unwrap_or(ParameterAxis::Y);
        let invalid = self.floor_plan.validate_simple().is_err();
        let inverted =
            find_flipped_edge(&edge_signs_before, &self.floor_plan.edge_signs()).is_some();
        if invalid || inverted {
            println!(
                "  post-apply needs repair (invalid={invalid}, inverted={inverted}) — \
                 redistributing un-set walls on axis {dim_axis:?}"
            );
            if !self.floor_plan.repair_unset_params(
                dim_axis,
                &self.dimensioning.known_params,
                &snapshot,
            ) {
                self.floor_plan = snapshot;
                self.dimensioning.scale_set = scale_set_before;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                println!("  → REJECTED: no shape-preserving fit found. Rolled back.");
                return Err(format!(
                    "{} can't be set without changing the shape — there's no way to move \
                     the un-set walls that keeps every wall's direction. Set the \
                     neighbouring walls first, or pick a value that fits.",
                    value
                ));
            }
            println!("  repair succeeded (shape preserved).");
        }

        // known_params already updated optimistically above.
        // Apply an aesthetic nudge if the polygon's aspect got extreme from
        // this dimension. Only touches un-set parameters; user's set values
        // stay exact.
        self.floor_plan
            .aesthetic_rescale(&self.dimensioning.known_params);
        self.commit_to_ber()?;
        let post_length = self.floor_plan.wall_length(ring, edge_start);
        println!(
            "  ✓ accepted. wall now {:.3} m. known params: {:?}. dimensioned walls: {}/{}",
            post_length.unwrap_or(f32::NAN),
            {
                let mut k: Vec<_> = self.dimensioning.known_params.iter().copied().collect();
                k.sort();
                k
            },
            self.dimensioning.dimensioned_walls.len(),
            self.floor_plan.all_active_params().len(),
        );
        self.compute_next_dimension_prompt();
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// Skip the current dimension prompt without entering a value. The
    /// specific WALL is suppressed from future prompts; the param it would
    /// have set remains un-set — but other walls (if any) can still set it.
    pub fn skip_current_dimension(&mut self) {
        self.record_history();
        if let Some(prompt) = self.dimensioning.current_prompt.take() {
            self.dimensioning
                .skipped_walls
                .insert(prompt.wall_start_vertex);
            let (ring, edge) = self
                .floor_plan
                .find_vertex(prompt.wall_start_vertex)
                .unwrap_or((0, 0));
            println!(
                "wall skipped: ring {}/edge {} (would have set P{}). \
                 The heuristic will continue looking for other walls.",
                ring, edge, prompt.controlled_param
            );
            self.compute_next_dimension_prompt();
        }
    }

    /// Recompute `current_prompt` based on which params are still unknown.
    /// Picks the first wall whose length depends on exactly one unknown
    /// parameter; clears the prompt when every active param is known.
    pub(super) fn compute_next_dimension_prompt(&mut self) {
        // The "which wall has exactly one unknown parameter" search is parametric
        // reasoning (in `FloorPlan`); the controller only owns the workflow —
        // turning the result into the active prompt. Skip is per-WALL: a skipped
        // wall doesn't preclude prompting another wall that sets the same param.
        self.dimensioning.current_prompt = self
            .floor_plan
            .next_promptable_wall(
                &self.dimensioning.known_params,
                &self.dimensioning.skipped_walls,
            )
            .map(|(wall_start_vertex, controlled_param)| DimensionPrompt {
                wall_start_vertex,
                controlled_param,
            });
    }

    /// Kick off dimensioning if nothing else is in the way (no pending redraw,
    /// no in-flight collinearity review). No-op otherwise.
    pub(super) fn maybe_start_dimensioning(&mut self) {
        if self.review.is_some()
            || self.pending_redraw.is_some()
            || self.dimensioning.current_prompt.is_some()
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
        if !self.dimensioning.scale_set {
            self.floor_plan.simplify_degenerate();
            if let Err(e) = self.commit_to_ber() {
                eprintln!("commit_to_ber after simplify_degenerate: {}", e);
            }
        }
        self.compute_next_dimension_prompt();
    }
}
