//! Position dimensioning of phase/step sub-areas — pure model mutation.
//!
//! A carved sub-area's WALL LENGTHS are dimensioned by the length loop
//! (`dimensioning.rs`), but its absolute POSITION inside the storey may still
//! be approximate: a fully interior area keeps its stroke coordinates
//! verbatim, and a cut region whose cut-length prompt was skipped leaves the
//! cut line at its drawn coordinate. This module asks the follow-up question:
//! "how far is this area (or this wall) from a storey-outline wall?" — one
//! prompt per unresolved degree of freedom, strictly AFTER the length loop.
//!
//! The approximate-stroke principle: a free axis resolves ONLY via an entered
//! value, an explicit skip, or a LOGGED zero-reference auto-resolve — never
//! silently from geometry coincidence.

use crate::{ActiveShape, StoreyDoc};
use domain::points::Point2f32;
use manip_plan::{find_flipped_edge, FloorPlan, ParameterAxis, ParameterId, VertexId};

/// Coordinate tolerance for reference dedupe and "is this wall already on the
/// reference?" tests, in plan units (metres once the scale is set). Matches
/// the contact-scan eps in `manip_plan`'s pin/free-axes scan.
const COORD_TOL: f32 = 1e-3;

/// One "where exactly is this area?" question: pin its position along `axis`
/// by a measured distance from a storey-outline wall.
#[derive(Debug, Clone)]
pub struct PositionPrompt {
    pub axis: ParameterAxis,
    pub kind: PositionPromptKind,
    /// Cyclable (area wall, outline wall) measurement options, nearest first.
    /// Never empty (an axis with no references auto-resolves instead).
    pub options: Vec<PositionOption>,
    pub current_idx: usize,
}

/// How applying the entered distance resolves the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionPromptKind {
    /// No exact coordinate on `axis` — applying rigid-translates the whole
    /// area (`FloorPlan::translate_all`) so the measured wall lands exactly.
    Translate,
    /// The area is anchored but this wall's perpendicular coord (e.g. a cut
    /// line) is unset — applying sets the parameter (+ repair).
    Wall { param: ParameterId },
    /// A ceiling-step LINE's coordinate — built by the dedicated
    /// `LinePositionPrompt` flow (`ceiling_step.rs`) and resolved by the SHELL
    /// (`StoreyDoc::resolve_ceiling_line_position`), never by the dimensioning
    /// loop: there's no shape or parameter behind a line. The variant exists so
    /// the shared position-panel UI data can tag the prompt kind; it never
    /// reaches `apply_position`/`skip_current_position` (`unreachable!`) and is
    /// never serialized (`current_position_prompt` is `serde(skip)`).
    Line,
    /// A roof-point (apex) coordinate on one UN-snapped axis — built by the
    /// shell's pending-roof-point flow, which reuses the `LinePositionPrompt`
    /// machinery per axis (X then Y) and resolves it itself
    /// (`StoreyDoc::resolve_ceiling_line_position`), never by the dimensioning
    /// loop: there's no shape or parameter behind a tapped point. Like `Line`,
    /// the variant exists so the shared position-panel UI data can tag the
    /// prompt kind; it never reaches `apply_position`/`skip_current_position`
    /// (`unreachable!`) and is never serialized.
    Point,
}

/// One candidate measurement. NOTE the two VertexIds index DIFFERENT plans:
/// `area_wall_vid` lives in the ACTIVE (area) plan — the live editing buffer —
/// while `outline_wall_vid` lives in the storey OUTLINE plan
/// (`StoreyDoc::outline.floor_plan`). Resolve each against the right plan.
#[derive(Debug, Clone, Copy)]
pub struct PositionOption {
    /// Start vertex of the measured area wall, in the ACTIVE (area) plan.
    pub area_wall_vid: VertexId,
    /// Start vertex of the reference wall, in the storey OUTLINE plan.
    pub outline_wall_vid: VertexId,
    /// The outline wall's coordinate on the prompt's `axis`.
    pub reference_value: f32,
    /// `signum(area − reference)` at build time — which side of the reference
    /// the area was drawn on, preserved when the entered distance is applied.
    pub sign: f32,
    /// `|area − reference|` — the approximate drawn distance (prefill value).
    pub current_distance: f32,
}

/// The `(lo, hi)` extent of `plan`'s footprint on `axis` — its bounding box on
/// that axis. A degenerate/empty plan reports `(0.0, 0.0)`. Used by the Centre
/// path to centre a shape's span within its reference footprint's span.
fn axis_extent(plan: &FloorPlan, axis: ParameterAxis) -> (f32, f32) {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for p in plan.get_points() {
        let c = match axis {
            ParameterAxis::X => p.x,
            ParameterAxis::Y => p.y,
        };
        lo = lo.min(c);
        hi = hi.max(c);
    }
    if lo > hi {
        (0.0, 0.0)
    } else {
        (lo, hi)
    }
}

/// Ring-0 walls of `plan` PERPENDICULAR to `axis` (vertical walls for X),
/// as `(start VertexId, coordinate on axis)`, in ring order.
fn axis_walls(plan: &FloorPlan, axis: ParameterAxis) -> Vec<(VertexId, f32)> {
    plan.perimeter_walls()
        .into_iter()
        .filter_map(|(vid, a, b)| {
            let coord = match axis {
                ParameterAxis::X => ((a.x - b.x).abs() < COORD_TOL).then_some(a.x),
                ParameterAxis::Y => ((a.y - b.y).abs() < COORD_TOL).then_some(a.y),
            };
            coord.map(|c| (vid, c))
        })
        .collect()
}

/// The outline reference walls for a prompt on `axis`: ring-0 outline walls
/// whose PERPENDICULAR axis is `axis`, deduped by coordinate within tolerance
/// (collinear outline walls are one reference; the first in ring order wins).
/// `pub(crate)`: the ceiling-step line prompt (`ceiling_step.rs`) reuses the
/// same reference-wall scan.
pub(crate) fn reference_walls(outline: &FloorPlan, axis: ParameterAxis) -> Vec<(VertexId, f32)> {
    let mut refs: Vec<(VertexId, f32)> = Vec::new();
    for (vid, c) in axis_walls(outline, axis) {
        if !refs.iter().any(|(_, rc)| (rc - c).abs() < COORD_TOL) {
            refs.push((vid, c));
        }
    }
    refs
}

/// Carry the cycled reference selection across a recompute when the prompt's
/// identity `(axis, kind)` is unchanged; otherwise start at the nearest.
fn with_preserved_idx(prev: &Option<PositionPrompt>, mut new: PositionPrompt) -> PositionPrompt {
    if let Some(p) = prev {
        if p.axis == new.axis && p.kind == new.kind {
            new.current_idx = p.current_idx.min(new.options.len().saturating_sub(1));
        }
    }
    new
}

// ── Position-dimensioning operations ────────────────────────────────────────
// Pure model mutation over the active shape's plan + dimensioning state. The
// shell (`AppController`) wraps these with undo and persistence, exactly as it
// does the length loop in `dimensioning.rs`.
impl StoreyDoc {
    /// Recompute `current_position_prompt`. `None` unless the active shape is
    /// a PhaseArea/StepArea AND the wall-length loop is done. Order: one
    /// `Translate` prompt per free, unresolved axis (X before Y), then one
    /// `Wall` prompt per unset movable param on a NON-free axis (a cut line)
    /// not skipped. Zero-reference items auto-resolve (logged — never
    /// silently). Preserves `current_idx` when the prompt identity
    /// `(axis, kind)` is unchanged across recomputes.
    pub fn recompute_position_prompt(&mut self) {
        // In-flight phase/step areas are always position-prompted; a freshly
        // DRAWN upper-storey OUTLINE is too, but ONLY while its reference (the
        // storey-below footprint, `position_reference_override`) is supplied — a
        // GROUND storey has no reference, so its outline stays unprompted. Living
        // areas carry the free-axes record (uniform `pinned_slot_for`) but are
        // never asked.
        let outline_positionable =
            self.active == ActiveShape::Outline && self.position_reference_override.is_some();
        if !matches!(self.active, ActiveShape::PhaseArea | ActiveShape::StepArea)
            && !outline_positionable
        {
            self.dimensioning.current_position_prompt = None;
            return;
        }
        // Strictly AFTER the length loop — lengths first, then position.
        if self.dimensioning.current_prompt.is_some() {
            self.dimensioning.current_position_prompt = None;
            return;
        }
        let prev = self.dimensioning.current_position_prompt.take();

        // 1) Translate prompts: the area's whole position on a free axis.
        let pending_axes: Vec<ParameterAxis> = self
            .dimensioning
            .position_free_axes
            .iter()
            .copied()
            .filter(|a| !self.dimensioning.position_resolved_axes.contains(a))
            .collect();
        for axis in pending_axes {
            let options = self.translate_options(axis);
            if options.is_empty() {
                // Approximate-stroke principle: nothing to measure against →
                // the axis resolves itself, LOGGED (never silently).
                log::info!(
                    "position prompt on {axis:?} auto-resolved: no outline reference \
                     wall to measure the area against — drawn position accepted"
                );
                self.dimensioning.position_resolved_axes.push(axis);
                continue;
            }
            self.dimensioning.current_position_prompt = Some(with_preserved_idx(
                &prev,
                PositionPrompt {
                    axis,
                    kind: PositionPromptKind::Translate,
                    options,
                    current_idx: 0,
                },
            ));
            return;
        }

        // 2) Wall prompts: an anchored area whose wall's perpendicular coord
        //    (e.g. a cut line) was never set — its drawn coordinate is
        //    approximate. Scoped to NON-free axes: free axes are Translate's.
        for (param, axis, area_wall_vid, coord) in self.unset_wall_candidates() {
            let refs = reference_walls(self.position_reference(), axis);
            if refs.is_empty() {
                log::info!(
                    "position prompt for P{param} ({axis:?}) auto-resolved: no outline \
                     reference wall to measure from — drawn coordinate accepted"
                );
                self.dimensioning.position_skipped_params.insert(param);
                continue;
            }
            let mut options: Vec<PositionOption> = refs
                .into_iter()
                .map(|(outline_wall_vid, reference_value)| PositionOption {
                    area_wall_vid,
                    outline_wall_vid,
                    reference_value,
                    sign: (coord - reference_value).signum(),
                    current_distance: (coord - reference_value).abs(),
                })
                .collect();
            options.sort_by(|a, b| a.current_distance.total_cmp(&b.current_distance));
            self.dimensioning.current_position_prompt = Some(with_preserved_idx(
                &prev,
                PositionPrompt {
                    axis,
                    kind: PositionPromptKind::Wall { param },
                    options,
                    current_idx: 0,
                },
            ));
            return;
        }

        self.dimensioning.current_position_prompt = None;
    }

    /// Apply the entered distance via the selected option. `Translate`:
    /// rigid-move the whole area on the axis so the measured wall lands
    /// exactly (side preserved via the option's `sign`); `Wall`: set the
    /// parameter + validate/repair like `apply_dimension`. Rolls back and
    /// returns a user-facing message on self-intersect / edge flip / outside
    /// the storey outline. Marks the measured wall's perpendicular param (if
    /// any) known, records the axis resolved, recomputes the next prompt.
    pub fn apply_position(&mut self, value: f32) -> Result<(), String> {
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "distance must be a non-negative number (got {})",
                value
            ));
        }
        let prompt = self
            .dimensioning
            .current_position_prompt
            .clone()
            .ok_or_else(|| "no active position prompt".to_string())?;
        let opt = *prompt
            .options
            .get(prompt.current_idx)
            .ok_or_else(|| "position prompt has no selected option".to_string())?;

        // Where the measured wall must land: the entered distance from the
        // reference, on the side the area was drawn (sign preservation).
        let target = opt.reference_value + opt.sign * value;
        log::debug!(
            "position dimension ({:?} {:?}): wall vid {} → {} m from reference wall vid {} \
             (ref {:.3}, side {:+.0}) ⇒ coord {:.3} [drawn {:.3}]",
            prompt.kind,
            prompt.axis,
            opt.area_wall_vid,
            value,
            opt.outline_wall_vid,
            opt.reference_value,
            opt.sign,
            target,
            opt.reference_value + opt.sign * opt.current_distance,
        );

        match prompt.kind {
            PositionPromptKind::Translate => {
                self.apply_translate_target(&prompt, target, &value.to_string())
            }
            PositionPromptKind::Wall { param } => {
                self.apply_wall_target(&prompt, param, target, &value.to_string())
            }
            PositionPromptKind::Line => unreachable!("Line prompts are shell-resolved"),
            PositionPromptKind::Point => unreachable!("Point prompts are shell-resolved"),
        }
    }

    /// The plan the position loop measures against: the injected storey-below
    /// footprint ([`position_reference_override`](StoreyDoc::position_reference_override))
    /// ONLY while the OUTLINE itself is active (a freshly-drawn upper storey
    /// positioned over the one below); for any sub-area (phase/step/exposure)
    /// it's always this storey's own outline. Scoping to `Outline` is what
    /// keeps a lingering override — left set after the upper outline finalised
    /// — from mis-referencing a later same-session sub-area carve against the
    /// below footprint (which, for an OVERHANGING upper storey, differs from
    /// the actual outline).
    pub fn position_reference(&self) -> &FloorPlan {
        match self.active {
            ActiveShape::Outline => self
                .position_reference_override
                .as_ref()
                .unwrap_or(&self.outline.floor_plan),
            _ => &self.outline.floor_plan,
        }
    }

    /// The containment guardrail shared by both position kinds: `Some(message)`
    /// when the just-mutated `floor_plan` would fall outside the reference
    /// footprint. SKIPPED (always `None`) for an `Outline` — a freshly-drawn
    /// upper storey may legitimately overhang the storey below (an extension /
    /// cantilever). Phase/step sub-areas must stay inside the storey. `label`
    /// prefixes the user-facing rejection.
    fn position_guardrail_violation(&self, label: &str) -> Option<String> {
        if self.active == ActiveShape::Outline {
            return None;
        }
        let reference = self.position_reference();
        let tol = (reference.bounding_box_diagonal() * 0.01).max(1e-3);
        reference.any_point_outside(&self.floor_plan, tol).then(|| {
            format!(
                "{label} would push the area outside the storey — it must stay \
                 inside the floor plan. Pick a distance that fits."
            )
        })
    }

    /// Rigid-translate the whole active shape on `prompt.axis` so its measured
    /// wall lands at `target` (`FloorPlan::translate_all`), then run the shared
    /// success bookkeeping (containment guardrail, pin the measured wall's
    /// perpendicular param, record the axis resolved, recompute the next
    /// prompt). Shared by the typed-distance path ([`apply_position`]) and the
    /// Centre path ([`center_current_position`]) — `label` prefixes the
    /// rejection message ("centre", or the typed value). Rolls back on
    /// rejection.
    fn apply_translate_target(
        &mut self,
        prompt: &PositionPrompt,
        target: f32,
        label: &str,
    ) -> Result<(), String> {
        let opt = *prompt
            .options
            .get(prompt.current_idx)
            .ok_or_else(|| "position prompt has no selected option".to_string())?;
        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(opt.area_wall_vid)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        // Model-internal rollback snapshot (not the app's undo).
        let snapshot = self.floor_plan.clone();
        let known_params_before = self.dimensioning.known_params.clone();

        let current = {
            let p = self.floor_plan.polygons()[ring][edge_start];
            match prompt.axis {
                ParameterAxis::X => p.x,
                ParameterAxis::Y => p.y,
            }
        };
        let (dx, dy) = match prompt.axis {
            ParameterAxis::X => (target - current, 0.0),
            ParameterAxis::Y => (0.0, target - current),
        };
        self.floor_plan.translate_all(dx, dy);

        if let Some(msg) = self.position_guardrail_violation(label) {
            self.floor_plan = snapshot;
            self.dimensioning.known_params = known_params_before;
            log::warn!("  → REJECTED: would push the area outside the storey.");
            return Err(msg);
        }
        self.finish_position_apply(ring, edge_start, prompt.axis);
        Ok(())
    }

    /// Set the un-set perpendicular param of an anchored area's wall (a cut
    /// line) to `target` + validate/repair like `apply_dimension`, then the
    /// shared success bookkeeping. Rolls back on self-intersect / edge flip with
    /// no shape-preserving fit, or on the containment guardrail.
    fn apply_wall_target(
        &mut self,
        prompt: &PositionPrompt,
        param: ParameterId,
        target: f32,
        label: &str,
    ) -> Result<(), String> {
        let opt = *prompt
            .options
            .get(prompt.current_idx)
            .ok_or_else(|| "position prompt has no selected option".to_string())?;
        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(opt.area_wall_vid)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        let snapshot = self.floor_plan.clone();
        let known_params_before = self.dimensioning.known_params.clone();

        let edge_signs_before = snapshot.edge_signs();
        // Known up front: repair only scales NOT-known params, so delaying this
        // would let it clobber the user's value.
        self.dimensioning.known_params.insert(param);
        self.floor_plan.set_parameter(param, target);
        let invalid = self.floor_plan.validate_simple().is_err();
        let inverted =
            find_flipped_edge(&edge_signs_before, &self.floor_plan.edge_signs()).is_some();
        if (invalid || inverted)
            && !self.floor_plan.repair_unset_params(
                prompt.axis,
                &self.dimensioning.known_params,
                &snapshot,
            )
        {
            self.floor_plan = snapshot;
            self.dimensioning.known_params = known_params_before;
            log::warn!("  → REJECTED: no shape-preserving fit found. Rolled back.");
            return Err(format!(
                "{label} can't be applied without changing the shape — there's no way \
                 to move the un-set walls that keeps every wall's direction. \
                 Pick a distance that fits."
            ));
        }

        if let Some(msg) = self.position_guardrail_violation(label) {
            self.floor_plan = snapshot;
            self.dimensioning.known_params = known_params_before;
            log::warn!("  → REJECTED: would push the area outside the storey.");
            return Err(msg);
        }
        self.finish_position_apply(ring, edge_start, prompt.axis);
        Ok(())
    }

    /// Shared success tail for both apply kinds: the measured wall's position is
    /// now exact — pin its perpendicular param (if it has one; an anchored
    /// constant needs none), record the axis resolved, then a full recompute
    /// (lengths first): setting a `Wall` param known can make another wall
    /// length-promptable — ask it as a LENGTH before any further position
    /// question. The hook then refreshes the position prompt either way.
    fn finish_position_apply(&mut self, ring: usize, edge_start: usize, axis: ParameterAxis) {
        if let Some((p, _)) = self.floor_plan.wall_movable_param(ring, edge_start) {
            self.dimensioning.known_params.insert(p);
        }
        if !self.dimensioning.position_resolved_axes.contains(&axis) {
            self.dimensioning.position_resolved_axes.push(axis);
        }
        log::debug!("  ✓ position accepted on {:?}.", axis);
        self.recompute_prompt();
    }

    /// The 'Centre' button for a whole-shape `Translate` position prompt:
    /// compute the coordinate that centres the shape's extent within the
    /// REFERENCE footprint's extent on the prompt's axis, then apply it through
    /// the SAME translate path as a typed distance — the ceiling-tools' Centre
    /// pattern (compute the coord that centres, feed the shared apply — cf.
    /// [`centred_rect_lo`](crate::centred_rect_lo), which does the same over a
    /// ceiling region rather than a footprint bbox). `Err` when there is no
    /// `Translate` prompt open.
    pub fn center_current_position(&mut self) -> Result<(), String> {
        let prompt = self
            .dimensioning
            .current_position_prompt
            .clone()
            .ok_or_else(|| "no active position prompt".to_string())?;
        if prompt.kind != PositionPromptKind::Translate {
            return Err("only a whole-shape position can be centred".to_string());
        }
        let opt = *prompt
            .options
            .get(prompt.current_idx)
            .ok_or_else(|| "position prompt has no selected option".to_string())?;
        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(opt.area_wall_vid)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        let axis = prompt.axis;
        let (shape_lo, shape_hi) = axis_extent(&self.floor_plan, axis);
        let (ref_lo, ref_hi) = axis_extent(self.position_reference(), axis);
        // Centre the shape's extent within the reference span (span_mid − len/2,
        // like `centred_rect_lo`), then translate so the measured wall lands at
        // its current coord plus the same delta — the target the shared path
        // expects.
        let len = shape_hi - shape_lo;
        let centred_lo = (ref_lo + ref_hi) / 2.0 - len / 2.0;
        let measured_current = {
            let p = self.floor_plan.polygons()[ring][edge_start];
            match axis {
                ParameterAxis::X => p.x,
                ParameterAxis::Y => p.y,
            }
        };
        let target = measured_current + (centred_lo - shape_lo);
        self.apply_translate_target(&prompt, target, "centre")
    }

    /// The 'Centre between 2 points' action for a whole-shape `Translate`
    /// position prompt: given two ALREADY-SNAPPED world points, translate the
    /// shape so its axis-extent MIDPOINT lands on the two points' midpoint
    /// **on the prompt's axis ONLY** — a vertical (Y) prompt uses
    /// `(a.y + b.y)/2` and ignores x entirely; a horizontal (X) prompt uses
    /// `(a.x + b.x)/2` and ignores y. Same whole-shape-extent semantics as
    /// [`center_current_position`], just targeting the PICKED midpoint instead
    /// of the reference footprint's, and applied through the SAME translate
    /// path (guardrail + rollback + `finish_position_apply`). The caller (the
    /// shell) does the snapping — it holds the plans and tolerance; this is
    /// pure model mutation. `Err` when there is no `Translate` prompt open.
    pub fn center_current_position_between(
        &mut self,
        a: Point2f32,
        b: Point2f32,
    ) -> Result<(), String> {
        let prompt = self
            .dimensioning
            .current_position_prompt
            .clone()
            .ok_or_else(|| "no active position prompt".to_string())?;
        if prompt.kind != PositionPromptKind::Translate {
            return Err("only a whole-shape position can be centred".to_string());
        }
        let opt = *prompt
            .options
            .get(prompt.current_idx)
            .ok_or_else(|| "position prompt has no selected option".to_string())?;
        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(opt.area_wall_vid)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        let axis = prompt.axis;
        // The midpoint on the PROMPT'S axis only — the other axis is ignored.
        let target_mid = match axis {
            ParameterAxis::X => (a.x + b.x) / 2.0,
            ParameterAxis::Y => (a.y + b.y) / 2.0,
        };
        let (shape_lo, shape_hi) = axis_extent(&self.floor_plan, axis);
        // Centre the shape's extent on the picked midpoint (mid − len/2), then
        // translate so the measured wall lands at its current coord plus that
        // same delta — the target the shared path expects (cf. `center_current_position`).
        let len = shape_hi - shape_lo;
        let centred_lo = target_mid - len / 2.0;
        let measured_current = {
            let p = self.floor_plan.polygons()[ring][edge_start];
            match axis {
                ParameterAxis::X => p.x,
                ParameterAxis::Y => p.y,
            }
        };
        let target = measured_current + (centred_lo - shape_lo);
        self.apply_translate_target(&prompt, target, "centre between two points")
    }

    /// Skip the current position prompt — accept the drawn position. Records
    /// the axis (`Translate`) or the param (`Wall`) so it isn't re-asked, then
    /// recomputes the next prompt.
    pub fn skip_current_position(&mut self) {
        let Some(prompt) = self.dimensioning.current_position_prompt.take() else {
            return;
        };
        match prompt.kind {
            PositionPromptKind::Translate => {
                if !self
                    .dimensioning
                    .position_resolved_axes
                    .contains(&prompt.axis)
                {
                    self.dimensioning.position_resolved_axes.push(prompt.axis);
                }
                log::debug!(
                    "position prompt skipped: {:?} accepted at the drawn position",
                    prompt.axis
                );
            }
            PositionPromptKind::Wall { param } => {
                self.dimensioning.position_skipped_params.insert(param);
                log::debug!(
                    "position prompt skipped: P{} stays at its drawn coordinate",
                    param
                );
            }
            PositionPromptKind::Line => unreachable!("Line prompts are shell-resolved"),
            PositionPromptKind::Point => unreachable!("Point prompts are shell-resolved"),
        }
        self.recompute_position_prompt();
    }

    /// Advance the current prompt to the next reference option (wraps).
    /// No-op without a prompt.
    pub fn cycle_position_reference(&mut self) {
        if let Some(prompt) = &mut self.dimensioning.current_position_prompt {
            if !prompt.options.is_empty() {
                prompt.current_idx = (prompt.current_idx + 1) % prompt.options.len();
            }
        }
    }

    /// Build the `Translate` options for `axis`: each deduped outline
    /// reference wall paired with the area wall minimising `|Δ|` with
    /// `|Δ| > tol` (an area wall already ON the reference isn't a
    /// measurement), sorted by current distance ascending.
    fn translate_options(&self, axis: ParameterAxis) -> Vec<PositionOption> {
        let area_walls = axis_walls(&self.floor_plan, axis);
        let mut options: Vec<PositionOption> = reference_walls(self.position_reference(), axis)
            .into_iter()
            .filter_map(|(outline_wall_vid, reference_value)| {
                area_walls
                    .iter()
                    .filter(|(_, c)| (c - reference_value).abs() > COORD_TOL)
                    .min_by(|(_, a), (_, b)| {
                        (a - reference_value)
                            .abs()
                            .total_cmp(&(b - reference_value).abs())
                    })
                    .map(|&(area_wall_vid, coord)| PositionOption {
                        area_wall_vid,
                        outline_wall_vid,
                        reference_value,
                        sign: (coord - reference_value).signum(),
                        current_distance: (coord - reference_value).abs(),
                    })
            })
            .collect();
        options.sort_by(|a, b| a.current_distance.total_cmp(&b.current_distance));
        options
    }

    /// The `Wall`-prompt candidates, in ring order: each un-set movable param
    /// of a ring-0 area wall whose perpendicular axis is NOT free (the free
    /// axes belong to `Translate`) and that wasn't skipped, as
    /// `(param, axis, wall start vid, current coordinate)`. One entry per
    /// param (the first wall in ring order represents it).
    fn unset_wall_candidates(&self) -> Vec<(ParameterId, ParameterAxis, VertexId, f32)> {
        let n = self.floor_plan.polygons().first().map_or(0, |r| r.len());
        let mut seen = std::collections::HashSet::new();
        let mut candidates = Vec::new();
        for edge_start in 0..n {
            let Some((param, axis)) = self.floor_plan.wall_movable_param(0, edge_start) else {
                continue;
            };
            if self.dimensioning.position_free_axes.contains(&axis)
                || self.dimensioning.known_params.contains(&param)
                || self.dimensioning.position_skipped_params.contains(&param)
                || !seen.insert(param)
            {
                continue;
            }
            let Some(vid) = self.floor_plan.vertex_id_at(0, edge_start) else {
                continue;
            };
            let Some(coord) = self.floor_plan.parameter_value(param) else {
                continue;
            };
            candidates.push((param, axis, vid, coord));
        }
        candidates
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionState, ShapeSlot};
    use domain::points::Point2f32;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A StoreyDoc with a finalised 10×10 metre outline in its slot.
    fn storey_10x10() -> StoreyDoc {
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap();
        let known = outline.all_active_params();
        let mut doc = StoreyDoc::default();
        doc.outline = ShapeSlot {
            floor_plan: outline,
            dimensioning: DimensionState {
                scale_set: true,
                known_params: known,
                ..Default::default()
            },
        };
        doc.load_active_into_buffer();
        doc
    }

    /// Carve-equivalent of a fully interior 2×2 square at (2,2)–(4,4): both
    /// position axes free (no boundary contact).
    fn interior_square_slot(doc: &StoreyDoc) -> ShapeSlot {
        let plan =
            FloorPlan::from_clean_ring(&[p(2., 2.), p(4., 2.), p(4., 4.), p(2., 4.)]).unwrap();
        doc.pinned_slot_for(&doc.outline.floor_plan, plan, &[])
    }

    /// Drive the wall-length loop to completion by confirming each prompted
    /// wall at its current (drawn) length.
    fn complete_length_loop(doc: &mut StoreyDoc) {
        for _ in 0..20 {
            let Some(prompt) = doc.dimensioning.current_prompt.clone() else {
                return;
            };
            let (r, e) = doc
                .floor_plan
                .find_vertex(prompt.wall_start_vertex)
                .unwrap();
            let len = doc.floor_plan.wall_length(r, e).unwrap();
            doc.apply_dimension(len).unwrap();
        }
        panic!("length loop didn't terminate");
    }

    fn min_x(fp: &FloorPlan) -> f32 {
        fp.get_points()
            .iter()
            .map(|p| p.x)
            .fold(f32::INFINITY, f32::min)
    }
    fn min_y(fp: &FloorPlan) -> f32 {
        fp.get_points()
            .iter()
            .map(|p| p.y)
            .fold(f32::INFINITY, f32::min)
    }

    /// A free-standing (fully interior) phase area prompts for its X position
    /// then its Y position once the length loop completes; applying each
    /// entered distance rigid-translates the area so the measured wall lands
    /// exactly, wall lengths preserved.
    #[test]
    fn interior_phase_area_prompts_x_then_y_and_translates() {
        let mut doc = storey_10x10();
        let slot = interior_square_slot(&doc);
        assert_eq!(
            doc.outline
                .floor_plan
                .free_position_axes(&slot.floor_plan, &[]),
            vec![ParameterAxis::X, ParameterAxis::Y]
        );
        doc.begin_phase_area(slot);
        doc.recompute_prompt();
        // Lengths first: no position prompt while the length loop runs.
        assert!(doc.dimensioning.current_prompt.is_some());
        assert!(doc.dimensioning.current_position_prompt.is_none());
        complete_length_loop(&mut doc);

        // X prompt: outline refs x=0 and x=10, nearest first (left wall at
        // x=2 measured from x=0 → 2 m, vs right wall x=4 from x=10 → 6 m).
        let prompt = doc
            .dimensioning
            .current_position_prompt
            .clone()
            .expect("X position prompt");
        assert_eq!(prompt.axis, ParameterAxis::X);
        assert_eq!(prompt.kind, PositionPromptKind::Translate);
        assert_eq!(prompt.options.len(), 2);
        let near = &prompt.options[0];
        assert!((near.reference_value - 0.0).abs() < 1e-4);
        assert!((near.current_distance - 2.0).abs() < 1e-4);
        assert!(near.sign > 0.0);
        assert!((prompt.options[1].current_distance - 6.0).abs() < 1e-4);

        // 3 m from the left storey wall → the square shifts to x ∈ [3, 5].
        doc.apply_position(3.0).unwrap();
        assert!((min_x(&doc.floor_plan) - 3.0).abs() < 1e-4);
        assert!(doc
            .dimensioning
            .position_resolved_axes
            .contains(&ParameterAxis::X));

        // Then the Y prompt; 5 m from the bottom wall → y ∈ [5, 7].
        let prompt = doc
            .dimensioning
            .current_position_prompt
            .clone()
            .expect("Y position prompt");
        assert_eq!(prompt.axis, ParameterAxis::Y);
        doc.apply_position(5.0).unwrap();
        assert!((min_y(&doc.floor_plan) - 5.0).abs() < 1e-4);
        assert!(doc.dimensioning.current_position_prompt.is_none());

        // Rigid translation: the 2×2 footprint is intact.
        let pts = doc.floor_plan.get_points();
        let max_x = pts.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
        let max_y = pts.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        assert!((max_x - 5.0).abs() < 1e-4 && (max_y - 7.0).abs() < 1e-4);
    }

    /// Skip accepts the drawn position: it records the axis resolved and
    /// advances to the next prompt, leaving the geometry untouched.
    #[test]
    fn skip_resolves_the_axis_and_advances() {
        let mut doc = storey_10x10();
        let slot = interior_square_slot(&doc);
        doc.begin_phase_area(slot);
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        let before = doc.floor_plan.get_points();

        assert_eq!(
            doc.dimensioning
                .current_position_prompt
                .as_ref()
                .unwrap()
                .axis,
            ParameterAxis::X
        );
        doc.skip_current_position();
        assert!(doc
            .dimensioning
            .position_resolved_axes
            .contains(&ParameterAxis::X));
        assert_eq!(
            doc.dimensioning
                .current_position_prompt
                .as_ref()
                .unwrap()
                .axis,
            ParameterAxis::Y
        );
        doc.skip_current_position();
        assert!(doc.dimensioning.current_position_prompt.is_none());
        assert_eq!(doc.floor_plan.get_points(), before, "skip moves nothing");
    }

    /// A fully snapped area (corner contact pins both axes) is NEVER position
    /// prompted once its lengths are applied — no over-prompting.
    #[test]
    fn corner_hugging_area_is_never_position_prompted() {
        let mut doc = storey_10x10();
        let plan =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(3., 0.), p(3., 3.), p(0., 3.)]).unwrap();
        let slot = doc.pinned_slot_for(&doc.outline.floor_plan, plan, &[]);
        assert!(
            slot.dimensioning.position_free_axes.is_empty(),
            "corner contact pins both axes"
        );
        doc.begin_phase_area(slot);
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        assert!(doc.dimensioning.current_prompt.is_none());
        assert!(
            doc.dimensioning.current_position_prompt.is_none(),
            "fully snapped area must not be asked where it is"
        );
    }

    /// Living areas carry the free-axes record but are NEVER position-prompted.
    #[test]
    fn living_area_never_prompts() {
        let mut doc = storey_10x10();
        let slot = interior_square_slot(&doc);
        doc.add_active_living_area(slot);
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        assert_eq!(
            doc.dimensioning.position_free_axes,
            vec![ParameterAxis::X, ParameterAxis::Y],
            "the record is carried"
        );
        assert!(doc.dimensioning.current_prompt.is_none());
        assert!(
            doc.dimensioning.current_position_prompt.is_none(),
            "but a living area is never asked"
        );
    }

    /// Porches carry the free-axes record but — like living areas — are NEVER
    /// position-prompted.
    #[test]
    fn porch_never_prompts() {
        let mut doc = storey_10x10();
        let slot = interior_square_slot(&doc);
        doc.add_active_porch(slot);
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        assert_eq!(
            doc.dimensioning.position_free_axes,
            vec![ParameterAxis::X, ParameterAxis::Y],
            "the record is carried"
        );
        assert!(doc.dimensioning.current_prompt.is_none());
        assert!(
            doc.dimensioning.current_position_prompt.is_none(),
            "but a porch is never asked"
        );
    }

    /// A translate distance that would push the area outside the storey is
    /// rejected with a full rollback: geometry, resolved axes, and the prompt
    /// itself are unchanged.
    #[test]
    fn translate_outside_the_storey_is_rejected_with_rollback() {
        let mut doc = storey_10x10();
        let slot = interior_square_slot(&doc);
        doc.begin_phase_area(slot);
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        let before = doc.floor_plan.get_points();
        let known_before = doc.dimensioning.known_params.clone();

        // Option 0 measures from x=0 going +x; 20 m puts the square at
        // x ∈ [20, 22] — outside the 10×10 storey.
        let err = doc.apply_position(20.0).unwrap_err();
        assert!(err.contains("outside the storey"), "got: {err}");
        assert_eq!(doc.floor_plan.get_points(), before, "geometry rolled back");
        assert_eq!(doc.dimensioning.known_params, known_before);
        assert!(doc.dimensioning.position_resolved_axes.is_empty());
        let prompt = doc
            .dimensioning
            .current_position_prompt
            .as_ref()
            .expect("prompt still pending");
        assert_eq!(prompt.axis, ParameterAxis::X);
    }

    /// A cut wall whose length prompt was skipped (its param left un-set)
    /// resurfaces as a `Wall`-kind position prompt; applying the distance sets
    /// the parameter so the cut line lands exactly.
    #[test]
    fn skipped_cut_length_resurfaces_as_wall_prompt() {
        let mut doc = storey_10x10();
        // Left part of the storey: every translation axis pinned by the
        // outline, but the cut line at x=4 is approximate.
        let plan =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(4., 0.), p(4., 10.), p(0., 10.)]).unwrap();
        let slot = doc.pinned_slot_for(&doc.outline.floor_plan, plan, &[]);
        assert!(slot.dimensioning.position_free_axes.is_empty());
        doc.begin_phase_area(slot);
        doc.recompute_prompt();

        // Skip every length prompt → the cut param stays un-set.
        for _ in 0..20 {
            if doc.dimensioning.current_prompt.is_none() {
                break;
            }
            doc.skip_current_dimension();
        }
        let prompt = doc
            .dimensioning
            .current_position_prompt
            .clone()
            .expect("Wall position prompt for the cut line");
        assert_eq!(prompt.axis, ParameterAxis::X);
        let PositionPromptKind::Wall { param } = prompt.kind else {
            panic!("expected a Wall prompt, got {:?}", prompt.kind);
        };
        assert_eq!(prompt.options.len(), 2, "refs x=0 and x=10");
        // Nearest first: the cut at x=4 is 4 m from x=0, 6 m from x=10.
        assert!((prompt.options[0].reference_value - 0.0).abs() < 1e-4);
        assert!((prompt.options[0].current_distance - 4.0).abs() < 1e-4);

        // Cycle wraps through the options and back.
        doc.cycle_position_reference();
        assert_eq!(
            doc.dimensioning
                .current_position_prompt
                .as_ref()
                .unwrap()
                .current_idx,
            1
        );
        doc.cycle_position_reference();
        assert_eq!(
            doc.dimensioning
                .current_position_prompt
                .as_ref()
                .unwrap()
                .current_idx,
            0
        );

        // 6 m from the selected reference (x=0, drawn side +) → cut at x=6.
        doc.apply_position(6.0).unwrap();
        assert!((doc.floor_plan.parameter_value(param).unwrap() - 6.0).abs() < 1e-4);
        assert!(doc.dimensioning.known_params.contains(&param));
        assert!(doc.dimensioning.current_position_prompt.is_none());
    }

    /// Position progress survives the slot round-trip: `sync_active` writes it
    /// to the shape's slot and `load_active_into_buffer` restores it.
    #[test]
    fn slot_sync_round_trip_preserves_position_state() {
        let mut doc = storey_10x10();
        let slot = interior_square_slot(&doc);
        doc.begin_phase_area(slot);
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        doc.skip_current_position(); // resolve X
        assert_eq!(
            doc.dimensioning.position_resolved_axes,
            vec![ParameterAxis::X]
        );

        doc.sync_active();
        doc.dimensioning = DimensionState::default(); // clobber the buffer
        doc.load_active_into_buffer();
        assert_eq!(
            doc.dimensioning.position_free_axes,
            vec![ParameterAxis::X, ParameterAxis::Y]
        );
        assert_eq!(
            doc.dimensioning.position_resolved_axes,
            vec![ParameterAxis::X]
        );
        // The prompt itself is transient but re-derivable.
        doc.recompute_position_prompt();
        assert_eq!(
            doc.dimensioning
                .current_position_prompt
                .as_ref()
                .unwrap()
                .axis,
            ParameterAxis::Y
        );
    }

    // ── Upper-storey OUTLINE vertical placement ─────────────────────────────
    // A freshly-drawn upper outline is positioned against the storey BELOW
    // (supplied via `position_reference_override`), not its own outline slot.

    /// Carve a FULL-WIDTH upper box (y ∈ [2, 5.41]) over a 10×10 storey below,
    /// as the new-storey flow does: the box hugs the below's left/right walls
    /// (X pinned) but floats vertically (Y free). Returns the doc with the box
    /// active as the Outline; the caller decides whether to supply the below
    /// footprint as the position reference (the controller does this post-carve
    /// for an upper storey, but NOT for a ground storey).
    fn carve_full_width_upper_box() -> StoreyDoc {
        let prev_ring = [p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
        let stroke = vec![
            p(0.05, 2.0),
            p(9.95, 2.0),
            p(9.95, 5.41),
            p(0.05, 5.41),
            p(0.05, 2.05),
        ];
        let mut doc = StoreyDoc::default();
        doc.carve_outline_from_previous(
            &stroke,
            &prev_ring,
            &manip_plan::SubareaTuning::default(),
            &[],
            manip_plan::ring_diagonal(&prev_ring) * 0.06,
        )
        .expect("carve applies");
        doc
    }

    fn ten_by_ten_reference() -> FloorPlan {
        FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap()
    }

    fn max_y(fp: &FloorPlan) -> f32 {
        fp.get_points()
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max)
    }

    /// Carving a full-width upper box records Y as its only free axis, and once
    /// the length loop completes a `Translate` position prompt appears on Y with
    /// the below-storey top/bottom walls (y=0, y=10) as the reference options.
    #[test]
    fn upper_outline_full_width_box_prompts_translate_on_y() {
        let mut doc = carve_full_width_upper_box();
        assert_eq!(
            doc.dimensioning.position_free_axes,
            vec![ParameterAxis::Y],
            "full-width box: X pinned to the storey below, Y free"
        );
        doc.position_reference_override = Some(ten_by_ten_reference());
        doc.recompute_prompt();
        complete_length_loop(&mut doc);

        let prompt = doc
            .dimensioning
            .current_position_prompt
            .clone()
            .expect("Y position prompt after the length loop");
        assert_eq!(prompt.axis, ParameterAxis::Y);
        assert_eq!(prompt.kind, PositionPromptKind::Translate);
        assert_eq!(prompt.options.len(), 2, "below top + bottom walls");
        let mut refs: Vec<f32> = prompt.options.iter().map(|o| o.reference_value).collect();
        refs.sort_by(f32::total_cmp);
        assert!((refs[0] - 0.0).abs() < 1e-3 && (refs[1] - 10.0).abs() < 1e-3);
    }

    /// The reference-wall vid on an upper-outline `Translate` option is minted
    /// by the model in the storey-BELOW footprint (`position_reference()` → the
    /// override), NOT the box's own outline slot. Resolving it in that same plan
    /// yields a genuine parallel reference wall — perpendicular to the prompt
    /// axis, sitting exactly at `reference_value`; resolving the same vid against
    /// the box lands on an unrelated wall (the mis-placed red highlight, report
    /// bcc699f8). Locks the two-plan invariant.
    #[test]
    fn upper_outline_reference_wall_resolves_in_the_below_footprint() {
        let mut doc = carve_full_width_upper_box();
        doc.position_reference_override = Some(ten_by_ten_reference());
        doc.recompute_prompt();
        complete_length_loop(&mut doc);

        let prompt = doc
            .dimensioning
            .current_position_prompt
            .clone()
            .expect("Y position prompt");
        assert_eq!(prompt.axis, ParameterAxis::Y);
        let opt = prompt.options[prompt.current_idx];

        // Resolve the vid in the SAME plan the model minted it (the override):
        // a Y prompt measures against a HORIZONTAL below wall (perpendicular to
        // Y) sitting at `reference_value` (y=0 or y=10) — a real parallel wall.
        let (a, b) = doc
            .position_reference()
            .wall_endpoints(opt.outline_wall_vid)
            .expect("reference wall lives in the below footprint plan");
        assert!(
            (a.y - b.y).abs() < 1e-3,
            "reference wall is perpendicular to the Y prompt axis (horizontal)"
        );
        assert!((a.y - opt.reference_value).abs() < 1e-3);
        assert!(
            (opt.reference_value - 0.0).abs() < 1e-3 || (opt.reference_value - 10.0).abs() < 1e-3,
            "a below top/bottom wall, not a box edge (the box is y∈[2,5.41])"
        );

        // The bug resolved that SAME vid against the box's own outline slot,
        // where it is absent or a DIFFERENT wall — never the below wall at
        // `reference_value`. The box has no wall at y=0/y=10, so it can never
        // masquerade as the reference.
        if let Some((ba, bb)) = doc.outline.floor_plan.wall_endpoints(opt.outline_wall_vid) {
            let matches_reference =
                (ba.y - bb.y).abs() < 1e-3 && (ba.y - opt.reference_value).abs() < 1e-3;
            assert!(
                !matches_reference,
                "box plan must not masquerade as the below reference wall"
            );
        }
    }

    /// Applying a distance translates the upper outline on Y so the measured
    /// wall lands at that distance from the chosen reference — and a distance
    /// that pushes the box PAST the below footprint (an overhang / cantilever)
    /// is ACCEPTED, not rejected by a containment guardrail.
    #[test]
    fn upper_outline_distance_translates_and_allows_overhang() {
        let mut doc = carve_full_width_upper_box();
        doc.position_reference_override = Some(ten_by_ten_reference());
        doc.recompute_prompt();
        complete_length_loop(&mut doc);

        // Option 0 measures the bottom wall (y=2) from the below floor (y=0).
        let prompt = doc.dimensioning.current_position_prompt.clone().unwrap();
        let near = prompt
            .options
            .iter()
            .position(|o| (o.reference_value - 0.0).abs() < 1e-3)
            .unwrap();
        doc.dimensioning
            .current_position_prompt
            .as_mut()
            .unwrap()
            .current_idx = near;

        // 8 m from y=0 → bottom at y=8, top at y≈11.41 — OVER the 10 m below top.
        doc.apply_position(8.0).unwrap();
        assert!(
            (min_y(&doc.floor_plan) - 8.0).abs() < 1e-3,
            "bottom landed at 8"
        );
        assert!(
            max_y(&doc.floor_plan) > 10.0,
            "the top overhangs the storey below — accepted"
        );
        assert!(doc
            .dimensioning
            .position_resolved_axes
            .contains(&ParameterAxis::Y));
        assert!(doc.dimensioning.current_position_prompt.is_none());
    }

    /// `center_current_position` centres the box's Y extent within the below
    /// footprint's Y extent: a 3.41 m box over a 10 m storey lands centred at
    /// mid-line (y=5), rigidly (its height is preserved).
    #[test]
    fn upper_outline_centre_centres_within_the_below_footprint() {
        let mut doc = carve_full_width_upper_box();
        doc.position_reference_override = Some(ten_by_ten_reference());
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        assert_eq!(
            doc.dimensioning
                .current_position_prompt
                .as_ref()
                .unwrap()
                .axis,
            ParameterAxis::Y
        );
        let height_before = max_y(&doc.floor_plan) - min_y(&doc.floor_plan);

        doc.center_current_position().unwrap();
        let (lo, hi) = (min_y(&doc.floor_plan), max_y(&doc.floor_plan));
        assert!(
            ((lo + hi) / 2.0 - 5.0).abs() < 1e-3,
            "box centred on the below mid-line (got lo {lo}, hi {hi})"
        );
        assert!(
            (hi - lo - height_before).abs() < 1e-3,
            "centred rigidly — height preserved"
        );
        assert!(doc.dimensioning.current_position_prompt.is_none());
    }

    /// A fully-shared (reproduced) upper outline — every wall on the storey
    /// below — has NO free position axis, so it is never position-prompted even
    /// with the reference supplied.
    #[test]
    fn reproduced_upper_outline_has_no_position_prompt() {
        let prev_ring = [p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
        let stroke = vec![p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0., 0.)];
        let mut doc = StoreyDoc::default();
        doc.carve_outline_from_previous(
            &stroke,
            &prev_ring,
            &manip_plan::SubareaTuning::default(),
            &[],
            manip_plan::ring_diagonal(&prev_ring) * 0.06,
        )
        .expect("carve applies");
        assert!(
            doc.dimensioning.position_free_axes.is_empty(),
            "a reproduced outline is pinned on both axes"
        );
        doc.position_reference_override = Some(ten_by_ten_reference());
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        assert!(doc.dimensioning.current_position_prompt.is_none());
    }

    /// A GROUND storey has no storey below, so no `position_reference_override`
    /// is supplied — its outline is never position-prompted even when its carve
    /// left a free axis (defensive: the ground outline isn't drawn via the carve
    /// path, but the guard must hold regardless).
    #[test]
    fn ground_outline_without_reference_is_never_position_prompted() {
        let mut doc = carve_full_width_upper_box();
        assert_eq!(doc.dimensioning.position_free_axes, vec![ParameterAxis::Y]);
        // No reference override supplied.
        assert!(doc.position_reference_override.is_none());
        doc.recompute_prompt();
        complete_length_loop(&mut doc);
        assert!(
            doc.dimensioning.current_position_prompt.is_none(),
            "with no reference below, the outline is not position-prompted"
        );
    }

    /// A STALE `position_reference_override` (left set after an upper outline
    /// finalised) must NOT leak into a same-session sub-area carve: once a
    /// phase/step area is active, the position loop references the actual
    /// storey OUTLINE, not the below footprint — critical when the upper storey
    /// OVERHANGS the below (the two differ). Here the outline spans y ∈ [8, 16]
    /// while the stale override (below footprint) spans y ∈ [0, 10]; a phase
    /// area drawn at y ∈ [12, 14] is inside the outline but OUTSIDE the below.
    #[test]
    fn stale_override_does_not_leak_into_a_subarea_carve() {
        // Actual upper OUTLINE (overhangs the below on Y).
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 8.), p(10., 8.), p(10., 16.), p(0., 16.)]).unwrap();
        let known = outline.all_active_params();
        let mut doc = StoreyDoc::default();
        doc.outline = ShapeSlot {
            floor_plan: outline,
            dimensioning: DimensionState {
                scale_set: true,
                known_params: known,
                ..Default::default()
            },
        };
        // Stale override from finalising the upper outline: the BELOW footprint.
        doc.position_reference_override = Some(
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap(),
        );
        doc.active = ActiveShape::Outline;
        doc.load_active_into_buffer();

        // A phase area fully inside the OUTLINE but above the below footprint.
        let plan =
            FloorPlan::from_clean_ring(&[p(2., 12.), p(4., 12.), p(4., 14.), p(2., 14.)]).unwrap();
        let slot = doc.pinned_slot_for(&doc.outline.floor_plan, plan, &[]);
        doc.begin_phase_area(slot);
        doc.recompute_prompt();
        complete_length_loop(&mut doc);

        // X prompt first (outline x=0/x=10, indistinguishable from below) — skip
        // it to reach the discriminating Y prompt.
        assert_eq!(
            doc.dimensioning
                .current_position_prompt
                .as_ref()
                .unwrap()
                .axis,
            ParameterAxis::X
        );
        doc.skip_current_position();

        // Y prompt references the OUTLINE walls (y=8, y=16) — NOT the stale
        // below footprint (y=0, y=10). This is the leak that the fix closes.
        let prompt = doc
            .dimensioning
            .current_position_prompt
            .clone()
            .expect("Y position prompt");
        assert_eq!(prompt.axis, ParameterAxis::Y);
        let mut refs: Vec<f32> = prompt.options.iter().map(|o| o.reference_value).collect();
        refs.sort_by(f32::total_cmp);
        assert!(
            (refs[0] - 8.0).abs() < 1e-3 && (refs[1] - 16.0).abs() < 1e-3,
            "references are the outline walls (8, 16), not the below footprint (0, 10): {refs:?}"
        );

        // Pick the reference at y=8 (outline bottom) and place the area's bottom
        // 5 m above it → y ∈ [13, 15]: inside the outline [8,16] but OUTSIDE the
        // below footprint [0,10]. The guardrail measures against the OUTLINE, so
        // this valid placement is ACCEPTED (the bug would reject it).
        let near = prompt
            .options
            .iter()
            .position(|o| (o.reference_value - 8.0).abs() < 1e-3)
            .unwrap();
        doc.dimensioning
            .current_position_prompt
            .as_mut()
            .unwrap()
            .current_idx = near;
        doc.apply_position(5.0)
            .expect("a placement inside the outline (but over the below) is accepted");
        assert!((min_y(&doc.floor_plan) - 13.0).abs() < 1e-3);
    }

    /// Old persisted `DimensionState` JSON (no position fields) still loads —
    /// the new fields are `serde(default)` and the prompt is `serde(skip)`.
    #[test]
    fn old_dimension_state_json_without_position_fields_loads() {
        let json = r#"{
            "known_params": [3],
            "scale_set": true,
            "dimensioned_walls": [7],
            "skipped_walls": []
        }"#;
        let ds: DimensionState = serde_json::from_str(json).unwrap();
        assert!(ds.scale_set);
        assert!(ds.known_params.contains(&3));
        assert!(ds.position_free_axes.is_empty());
        assert!(ds.position_resolved_axes.is_empty());
        assert!(ds.position_skipped_params.is_empty());
        assert!(ds.current_position_prompt.is_none());
    }
}
