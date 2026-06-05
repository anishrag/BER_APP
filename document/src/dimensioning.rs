//! Dimensioning operations on the active shape — pure model mutation.
//! The shell (`AppController`) wraps these with undo + persistence.
use crate::{ActiveShape, DimensionPrompt, Document};
use manip_2d_plan::{find_flipped_edge, ParameterAxis};

// ── Dimensioning operations ─────────────────────────────────────────────────
// Pure model mutation over the active shape's plan + dimensioning state. The
// shell (`AppController`) wraps these with undo (snapshot/restore) and persistence
// (`commit_to_ber`); none of that lives here.
impl Document {
    /// Apply the user's entered length for the current dimension prompt. The first
    /// entry scales the whole plan from pixels to metres; later entries set the
    /// prompt's controlled parameter, then repair the un-set walls and (for a
    /// sub-area) reject a value that would push it outside the storey. A zero is a
    /// trim-remnant collapse. Rolls back the plan internally on an invalid result;
    /// returns a user-facing message on rejection.
    pub fn apply_dimension(&mut self, value: f32) -> Result<(), String> {
        // Zero is allowed (a trim-remnant collapse); only negative/non-finite reject.
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "dimension must be a non-negative number (got {})",
                value
            ));
        }
        let prompt = self
            .dimensioning
            .current_prompt
            .clone()
            .ok_or_else(|| "no active dimension prompt".to_string())?;

        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        // Zero length → collapse this wall (TRIM remnant only; the first dimension
        // can't be zero since it sets scale). Geometry lives in the plan.
        if value == 0.0 {
            if !self.dimensioning.scale_set {
                return Err("the first dimension can't be zero".into());
            }
            self.floor_plan
                .collapse_trim_wall(ring, edge_start, prompt.controlled_param)
                .map_err(|e| format!("{}", e))?;
            self.resync_dimensioning();
            self.recompute_prompt();
            return Ok(());
        }

        let pre_length = self.floor_plan.wall_length(ring, edge_start);
        log::debug!(
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

        // Snapshot to roll back if the dimension makes the polygon invalid
        // (self-intersecting / collapsed) or simple-but-inverted (an edge flips
        // direction past a fixed coord). This is a model-internal rollback, not
        // the app's undo.
        let snapshot = self.floor_plan.clone();
        let scale_set_before = self.dimensioning.scale_set;
        let known_params_before = self.dimensioning.known_params.clone();
        let dimensioned_walls_before = self.dimensioning.dimensioned_walls.clone();
        let edge_signs_before = snapshot.edge_signs();

        // Mark the just-set param known up front: repair only scales NOT-known
        // params, so delaying this would let it clobber the user's value. Rolled
        // back with the rest on full rejection.
        self.dimensioning
            .known_params
            .insert(prompt.controlled_param);
        self.dimensioning
            .dimensioned_walls
            .push(prompt.wall_start_vertex);

        if !self.dimensioning.scale_set {
            // First dimension — convert the whole plan from pixels to metres.
            let pixel_length = self
                .floor_plan
                .wall_length(ring, edge_start)
                .ok_or_else(|| "couldn't read wall length".to_string())?;
            if pixel_length < 1e-6 {
                return Err("wall has zero length, can't compute scale".into());
            }
            let scale = value / pixel_length;
            log::debug!("  first dimension — scaling whole plan by {:.6}", scale);
            self.floor_plan.scale_all(scale);
            self.dimensioning.scale_set = true;
        } else {
            // Subsequent dimensions — set just the controlled parameter.
            if let Err(e) =
                self.floor_plan
                    .set_wall_length(ring, edge_start, value, prompt.controlled_param)
            {
                self.floor_plan = snapshot;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                log::warn!("  → REJECTED (set_wall_length error): {:?}", e);
                return Err(format!("set_wall_length: {:?}", e));
            }
            log::debug!(
                "  set_wall_length: P{} = {:?}",
                prompt.controlled_param,
                self.floor_plan.parameter_value(prompt.controlled_param)
            );
        }

        // Redistribute un-set walls if the result is invalid or inverted; reject
        // only when no shape-preserving fit exists.
        let dim_axis = self
            .floor_plan
            .param_axis(prompt.controlled_param)
            .unwrap_or(ParameterAxis::Y);
        let invalid = self.floor_plan.validate_simple().is_err();
        let inverted =
            find_flipped_edge(&edge_signs_before, &self.floor_plan.edge_signs()).is_some();
        if invalid || inverted {
            log::debug!(
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
                log::warn!("  → REJECTED: no shape-preserving fit found. Rolled back.");
                return Err(format!(
                    "{} can't be set without changing the shape — there's no way to move \
                     the un-set walls that keeps every wall's direction. Set the \
                     neighbouring walls first, or pick a value that fits.",
                    value
                ));
            }
            log::debug!("  repair succeeded (shape preserved).");
        }

        // Aesthetic nudge (un-set params only; user's values stay exact).
        self.floor_plan
            .aesthetic_rescale(&self.dimensioning.known_params);

        // Guardrail: a SUB-AREA dimension must not push it outside the storey.
        if self.active != ActiveShape::Outline {
            let tol = (self.outline.floor_plan.bounding_box_diagonal() * 0.01).max(1e-3);
            if self
                .outline
                .floor_plan
                .any_point_outside(&self.floor_plan, tol)
            {
                self.floor_plan = snapshot;
                self.dimensioning.scale_set = scale_set_before;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                log::warn!("  → REJECTED: would push the sub-area outside the storey.");
                return Err(format!(
                    "{} would push the area outside the storey — it must stay \
                     inside the floor plan. Pick a value that fits.",
                    value
                ));
            }
        }
        let post_length = self.floor_plan.wall_length(ring, edge_start);
        log::debug!(
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
        self.recompute_prompt();
        Ok(())
    }

    /// Skip the current prompt's wall: suppress it from future prompts (the param
    /// it would set stays un-set, but other walls can still set it).
    pub fn skip_current_dimension(&mut self) {
        if let Some(prompt) = self.dimensioning.current_prompt.take() {
            self.dimensioning
                .skipped_walls
                .insert(prompt.wall_start_vertex);
            let (ring, edge) = self
                .floor_plan
                .find_vertex(prompt.wall_start_vertex)
                .unwrap_or((0, 0));
            log::debug!(
                "wall skipped: ring {}/edge {} (would have set P{}). \
                 The heuristic will continue looking for other walls.",
                ring,
                edge,
                prompt.controlled_param
            );
            self.recompute_prompt();
        }
    }

    /// Recompute `current_prompt`: the first wall depending on exactly one unknown
    /// parameter, or `None` when every active param is known.
    pub fn recompute_prompt(&mut self) {
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

    /// Drop dimensioning state that no longer refers to a live param/vertex after
    /// a topology change (trim-collapse, collinear merge, arc splice).
    pub fn resync_dimensioning(&mut self) {
        let active = self.floor_plan.all_active_params();
        self.dimensioning
            .known_params
            .retain(|p| active.contains(p));
        self.dimensioning
            .dimensioned_walls
            .retain(|v| self.floor_plan.find_vertex(*v).is_some());
        self.dimensioning
            .skipped_walls
            .retain(|v| self.floor_plan.find_vertex(*v).is_some());
    }
}
