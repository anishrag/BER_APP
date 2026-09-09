//! Dimensioning operations on the active shape — pure model mutation.
//! The shell (`AppController`) wraps these with undo + persistence.
use std::collections::HashSet;

use crate::{ActiveShape, DimensionPrompt, PendingArc, StoreyDoc};
use manip_plan::{find_flipped_edge, ParameterAxis};

// ── Dimensioning operations ─────────────────────────────────────────────────
// Pure model mutation over the active shape's plan + dimensioning state. The
// shell (`AppController`) wraps these with undo (snapshot/restore) and persistence
// (`commit_to_ber`); none of that lives here.
impl StoreyDoc {
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

        // Zero length → collapse this wall: a TRIM remnant deletes silently; a
        // STEP merges its flanking walls into one exactly-collinear wall (the
        // UI confirms that shape change first — see `zero_would_collapse_step`).
        // The first dimension can't be zero since it sets scale. Geometry
        // lives in the plan.
        if value == 0.0 {
            if !self.dimensioning.scale_set {
                return Err("the first dimension can't be zero".into());
            }
            match self
                .floor_plan
                .zero_collapse_kind(ring, edge_start, prompt.controlled_param)
            {
                Some(manip_plan::ZeroCollapseKind::TrimCut) => self
                    .floor_plan
                    .collapse_trim_wall(ring, edge_start, prompt.controlled_param)
                    .map_err(|e| format!("{}", e))?,
                Some(manip_plan::ZeroCollapseKind::Step) => self
                    .floor_plan
                    .collapse_step_wall(ring, edge_start, prompt.controlled_param)
                    .map_err(|e| format!("{}", e))?,
                None => {
                    return Err(
                        "0 can only collapse a trim remnant or a step — this wall is neither"
                            .into(),
                    )
                }
            }
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

        // Guardrail: a SUB-AREA dimension must not push it outside its containing
        // boundary — normally this storey's outline, but the floor-exposure carve
        // overrides it with the WHOLE-building footprint (it spans the stack), so
        // a value inside the building but outside this single storey still fits.
        if self.active != ActiveShape::Outline {
            let containment = self
                .subarea_containment_override
                .as_ref()
                .unwrap_or(&self.outline.floor_plan);
            let tol = (containment.bounding_box_diagonal() * 0.01).max(1e-3);
            if containment.any_point_outside(&self.floor_plan, tol) {
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

    /// Would entering 0 for the CURRENT dimension prompt collapse a STEP
    /// (merging its flanking walls collinear — a shape change the UI must
    /// confirm with the user before applying)? False for a trim remnant
    /// (zero deletes it silently, the established behaviour), for a wall a
    /// zero would reject, and when no prompt is active or scale isn't set.
    pub fn zero_would_collapse_step(&self) -> bool {
        if !self.dimensioning.scale_set {
            return false;
        }
        let Some(prompt) = self.dimensioning.current_prompt.as_ref() else {
            return false;
        };
        let Some((ring, edge_start)) = self.floor_plan.find_vertex(prompt.wall_start_vertex)
        else {
            return false;
        };
        self.floor_plan
            .zero_collapse_kind(ring, edge_start, prompt.controlled_param)
            == Some(manip_plan::ZeroCollapseKind::Step)
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
    /// parameter, or `None` when every active param is known. Also recomputes the
    /// follow-up position prompt (`position.rs`) — it activates only once the
    /// length loop is done, so hooking it here covers every call site.
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
        self.recompute_position_prompt();
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
        self.dimensioning
            .position_skipped_params
            .retain(|p| active.contains(p));
    }

    /// True once both of a provisional arc's attach stubs are dimensioned — every
    /// parameter the placeholder edge's length depends on is known, fixing both
    /// endpoints. The shell's cue to facet the arc in.
    pub fn pending_arc_ready(&self, pa: &PendingArc) -> bool {
        let (Some((r1, is)), Some((r2, ie))) = (
            self.floor_plan.find_vertex(pa.start),
            self.floor_plan.find_vertex(pa.end),
        ) else {
            return false;
        };
        if r1 != r2 {
            return false;
        }
        let Some(n) = self.floor_plan.polygons().get(r1).map(|p| p.len()) else {
            return false;
        };
        let edge_start = if (is + 1) % n == ie {
            is
        } else if (ie + 1) % n == is {
            ie
        } else {
            return false;
        };
        let deps = self.floor_plan.wall_param_deps(r1, edge_start);
        !deps.is_empty()
            && deps
                .iter()
                .all(|p| self.dimensioning.known_params.contains(p))
    }

    /// Every wall the assessor can COPY a length from while a dimension
    /// prompt is open — `(start, end, length)` in plan coords, the
    /// tap-to-copy label set. CONFIRMED-only: a wall qualifies only when its
    /// length is a genuine KNOWN — every parameter its length depends on has
    /// been entered/derived in the dimensioning loop
    /// (`walls_with_known_length`) — never a length that is merely
    /// geometrically derivable from the resolved ring. A not-yet-set wall
    /// carries no label, so its placeholder pixel/guess length is never shown
    /// or copied. Excluded on top of that: the prompted wall and its
    /// length-equivalents (their length IS the unknown being asked; the
    /// prompt box already shows their current value). The active living
    /// area's own known walls are labelled by the living-area path
    /// (`known_length_wall_segments`), not repeated here. Empty when no
    /// prompt is open. This set is exactly what the canvas both LABELS and
    /// offers as copy candidates — the two stay identical by construction.
    pub fn copyable_wall_labels(&self) -> Vec<((f64, f64), (f64, f64), f32)> {
        let Some(prompt) = self.dimensioning.current_prompt.as_ref() else {
            return Vec::new();
        };
        // The prompted wall + its length-equivalents, as (ring, edge) in the
        // ACTIVE plan (the prompt always lives on the live buffer).
        let excluded: HashSet<(usize, usize)> = self
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)
            .map(|(ring, edge)| {
                let mut set: HashSet<_> = self
                    .floor_plan
                    .equivalent_length_walls(ring, edge)
                    .into_iter()
                    .collect();
                set.insert((ring, edge));
                set
            })
            .unwrap_or_default();

        let mut out: Vec<((f64, f64), (f64, f64), f32)> = Vec::new();
        // Only walls whose length is genuinely SET in `known_params` — the
        // confirmed dimension-loop lengths, not every determinable wall.
        let mut push_known =
            |plan: &manip_plan::FloorPlan, known: &HashSet<_>, exclude_prompt: bool| {
                let polygons = plan.polygons();
                for (ri, e) in plan.walls_with_known_length(known) {
                    if exclude_prompt && excluded.contains(&(ri, e)) {
                        continue;
                    }
                    let Some(ring) = polygons.get(ri) else {
                        continue;
                    };
                    let n = ring.len();
                    if n < 3 {
                        continue;
                    }
                    let (a, b) = (ring[e], ring[(e + 1) % n]);
                    let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                    if len < 1e-3 {
                        continue;
                    }
                    out.push(((a.x as f64, a.y as f64), (b.x as f64, b.y as f64), len));
                }
            };

        match self.active {
            ActiveShape::Outline => {
                // The outline IS the live buffer: its confirmed walls minus
                // the prompt's own.
                push_known(&self.floor_plan, &self.dimensioning.known_params, true);
            }
            ActiveShape::PhaseArea | ActiveShape::StepArea => {
                // The settled outline's confirmed walls (the slot is current,
                // the live buffer holds the carve)…
                push_known(
                    &self.outline.floor_plan,
                    &self.outline.dimensioning.known_params,
                    false,
                );
                // …plus the in-flight carve's confirmed walls minus the
                // prompt's.
                push_known(&self.floor_plan, &self.dimensioning.known_params, true);
            }
            ActiveShape::LivingArea(_) | ActiveShape::Porch(_) => {
                // The active living area's / porch's known walls are already
                // labelled by its own path — add only the settled outline's
                // confirmed walls.
                push_known(
                    &self.outline.floor_plan,
                    &self.outline.dimensioning.known_params,
                    false,
                );
            }
        }
        out
    }
}
