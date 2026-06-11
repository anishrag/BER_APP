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
use manip_2d_plan::{find_flipped_edge, FloorPlan, ParameterAxis, ParameterId, VertexId};

/// Coordinate tolerance for reference dedupe and "is this wall already on the
/// reference?" tests, in plan units (metres once the scale is set). Matches
/// the contact-scan eps in `manip_2d_plan`'s pin/free-axes scan.
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
        // Only in-flight phase/step areas are position-prompted. Living areas
        // carry the free-axes record (uniform `pinned_slot_for`) but are never
        // asked; the outline has no boundary to be positioned against.
        if !matches!(self.active, ActiveShape::PhaseArea | ActiveShape::StepArea) {
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
            let refs = reference_walls(&self.outline.floor_plan, axis);
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
        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(opt.area_wall_vid)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        // Where the measured wall must land: the entered distance from the
        // reference, on the side the area was drawn (sign preservation).
        let target = opt.reference_value + opt.sign * value;
        log::debug!(
            "position dimension ({:?} {:?}): wall vid {} → {} m from outline wall vid {} \
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

        // Model-internal rollback snapshot (not the app's undo).
        let snapshot = self.floor_plan.clone();
        let known_params_before = self.dimensioning.known_params.clone();

        match prompt.kind {
            PositionPromptKind::Translate => {
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
            }
            PositionPromptKind::Wall { param } => {
                let edge_signs_before = snapshot.edge_signs();
                // Known up front: repair only scales NOT-known params, so
                // delaying this would let it clobber the user's value.
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
                        "{} can't be applied without changing the shape — there's no way \
                         to move the un-set walls that keeps every wall's direction. \
                         Pick a distance that fits.",
                        value
                    ));
                }
            }
            PositionPromptKind::Line => unreachable!("Line prompts are shell-resolved"),
            PositionPromptKind::Point => unreachable!("Point prompts are shell-resolved"),
        }

        // Guardrail (BOTH kinds): the area must stay inside the storey.
        let tol = (self.outline.floor_plan.bounding_box_diagonal() * 0.01).max(1e-3);
        if self
            .outline
            .floor_plan
            .any_point_outside(&self.floor_plan, tol)
        {
            self.floor_plan = snapshot;
            self.dimensioning.known_params = known_params_before;
            log::warn!("  → REJECTED: would push the area outside the storey.");
            return Err(format!(
                "{} would push the area outside the storey — it must stay \
                 inside the floor plan. Pick a distance that fits.",
                value
            ));
        }

        // Success: the measured wall's position is now exact — pin its
        // perpendicular param (if it has one; an anchored constant needs none).
        if let Some((p, _)) = self.floor_plan.wall_movable_param(ring, edge_start) {
            self.dimensioning.known_params.insert(p);
        }
        if !self
            .dimensioning
            .position_resolved_axes
            .contains(&prompt.axis)
        {
            self.dimensioning.position_resolved_axes.push(prompt.axis);
        }
        log::debug!("  ✓ position accepted on {:?}.", prompt.axis);
        // Full recompute (lengths first): setting a Wall param known can make
        // another wall length-promptable — ask it as a LENGTH before any
        // further position question. The hook then refreshes the position
        // prompt either way.
        self.recompute_prompt();
        Ok(())
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
        let mut options: Vec<PositionOption> = reference_walls(&self.outline.floor_plan, axis)
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
        doc.pinned_slot_for(&doc.outline.floor_plan, plan)
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
            doc.outline.floor_plan.free_position_axes(&slot.floor_plan),
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
        let slot = doc.pinned_slot_for(&doc.outline.floor_plan, plan);
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
        let slot = doc.pinned_slot_for(&doc.outline.floor_plan, plan);
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
