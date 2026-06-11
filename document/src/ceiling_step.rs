//! Ceiling-step logic: session-only areas that raise or lower part of the
//! storey ceiling at commit, entered as an AXIS-ALIGNED LINE + side + delta.
//!
//! A ceiling step is NOT persisted as a record of its own — the durable record
//! is the per-vertex z on the persisted `CeilingRegions` it re-splits
//! (`with_z_deltas` in the resolve projection). The shell owns the per-storey
//! region *storage* (session state, deliberately outside undo and the editing
//! snapshot); the logic over the regions — fitting/validating the input line,
//! the position prompt for its coordinate, building the half-rect area, and
//! rebuilding the session list from persisted roofs — lives here.
//!
//! The line (axis + coordinate + side) is input UX only. The session record is
//! the REGION: the shell canonicalizes after each assign by materializing and
//! rebuilding its list from the derived roofs
//! ([`ceiling_step_regions_from_roofs`]), so the in-session list equals the
//! post-reload reconstruction — idempotent reload by construction.

use crate::position::reference_walls;
use crate::step::ring_area2;
use crate::StoreyDoc;
use datatypes::ber::ber_struct::Ber;
use domain::points::Point2f32;
use manip_2d_plan::{ParameterAxis, VertexId};

/// Which side of the step line the delta applies to: coordinates on the
/// line's axis LESS or GREATER than the line's coordinate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineSide {
    Less,
    Greater,
}

/// A session-only ceiling-step area: a world-space (metre) ring + the z DELTA
/// inside it, RELATIVE to the storey's default ceiling height (+ raised,
/// − lowered). Overlapping areas COMPOSE (deltas sum) when applied in order
/// by `CeilingRegions::with_z_deltas`; after canonicalization the areas are
/// the exact derived pieces, which never overlap.
#[derive(Debug, Clone)]
pub struct CeilingStepRegion {
    pub polygon: Vec<Point2f32>,
    pub delta_m: f64,
}

/// Rebuild the session ceiling-step regions for storey `idx` from its
/// persisted roofs — APEX-AWARE: each region's BASE height is read with the
/// storey's roof apexes dropped (`CeilingRegion::fan_base_z`, which equals
/// `flat_z` for unfanned regions), and a region whose base differs from the
/// storey's default by ≥ 1 mm → one step region with `delta = base − default`.
/// A fan piece over a stepped area thus emits its TRIANGLE ring at the step's
/// delta — that's the apex-over-step reload fixpoint: re-deriving applies the
/// triangle steps first, then `with_apexes` re-fans them to the identical
/// surface (composition order: default → steps → apexes). Known UX cost: the
/// step list fragments into triangles under a fan — accepted for now. Empty
/// when the storey is missing or its ceiling height is unset (steps only
/// exist relative to a default). Genuinely irregular sloped regions (no
/// shared base once apex vertices drop) are skipped, as are degenerate
/// sub-mm² slivers. Without this, the next commit would re-derive the default
/// flat ceiling with no areas and flatten the steps.
pub fn ceiling_step_regions_from_roofs(ber: &Ber, idx: usize) -> Vec<CeilingStepRegion> {
    let Some(storey) = ber.storeys.get(idx) else {
        return Vec::new();
    };
    let Some(default_z) = storey.ceiling_height_m else {
        return Vec::new();
    };
    let mut regions: Vec<CeilingStepRegion> = Vec::new();
    for region in &storey.roofs.regions {
        let Some(z) = region.fan_base_z(&storey.roof_apexes) else {
            continue;
        };
        let delta = z - default_z;
        if delta.abs() < 1e-3 {
            continue;
        }
        let ring: Vec<Point2f32> = region
            .polygon
            .exterior
            .iter()
            .map(|v| Point2f32::new(v.x as f32, v.y as f32))
            .collect();
        if ring.len() >= 3 && ring_area2(&ring) >= 2e-6 {
            regions.push(CeilingStepRegion {
                polygon: ring,
                delta_m: delta,
            });
        }
    }
    regions
}

/// The "how far is the step line from an outline wall?" question — the line
/// analogue of [`PositionPrompt`](crate::PositionPrompt), shell-resolved
/// (there's no shape or parameter behind a line, so it does NOT enter the
/// `recompute_position_prompt` loop). The shell holds it in its pending-step
/// state and resolves it via [`StoreyDoc::resolve_ceiling_line_position`].
#[derive(Debug, Clone)]
pub struct LinePositionPrompt {
    /// The axis the line's POSITION varies on (X for a vertical line).
    pub axis: ParameterAxis,
    /// Cyclable outline reference walls, nearest first. Never empty
    /// (a line with no parallel reference auto-resolves instead).
    pub options: Vec<LinePositionOption>,
    pub current_idx: usize,
}

/// One candidate measurement for the line's coordinate. The reference wall is
/// in the storey OUTLINE plan (`StoreyDoc::outline.floor_plan`) — there is no
/// area plan; the measured object is the line itself.
#[derive(Debug, Clone, Copy)]
pub struct LinePositionOption {
    /// Start vertex of the reference wall, in the storey OUTLINE plan.
    pub outline_wall_vid: VertexId,
    /// The outline wall's coordinate on the prompt's `axis`.
    pub reference_value: f32,
    /// `signum(line − reference)` at build time — which side of the reference
    /// the line was drawn on, preserved when the entered distance is applied.
    pub sign: f32,
    /// `|line − reference|` — the approximate drawn distance (prefill value).
    pub current_distance: f32,
}

impl LinePositionPrompt {
    /// Advance to the next reference option (wraps). No-op when empty.
    pub fn cycle(&mut self) {
        if !self.options.is_empty() {
            self.current_idx = (self.current_idx + 1) % self.options.len();
        }
    }
}

/// User-facing rejection for a line that doesn't cross the storey footprint.
/// Kept stable — tests (and the shell) match on it.
const LINE_OFF_FOOTPRINT_MSG: &str =
    "The step line must cross the storey footprint — pick a position inside the floor plan.";

/// Coordinate tolerance for the crosses-the-footprint test, in metres: a line
/// within a millimetre of the bounding box edge steps nothing.
const LINE_CROSS_TOL: f32 = 1e-3;

/// How close (metres) a drawn/tapped step line must land to a parallel outline
/// wall to SNAP onto it. Hand precision on a tablet, matching the spirit of
/// the other snap tolerances; promotable to `tuning.yaml` later if it needs
/// per-device adjustment.
const CEILING_LINE_SNAP_TOL_M: f32 = 0.2;

// ── Ceiling-step operations ─────────────────────────────────────────────────
// Pure queries over the storey outline. The shell (`AppController`) owns the
// pending-step state machine, the region storage, and canonicalization.
impl StoreyDoc {
    /// Fit a near-straight `stroke` (world/metre space) to an axis-aligned
    /// step line: [`manip_2d_plan::fit_axis_line`] + the crosses-the-footprint
    /// validation. Returns the axis the line's position varies on (X for a
    /// vertical line) and its coordinate. `Err` is user-facing.
    pub fn fit_ceiling_step_line(
        &self,
        stroke: &[Point2f32],
    ) -> Result<(ParameterAxis, f32), String> {
        let (axis, coord) = manip_2d_plan::fit_axis_line(stroke).ok_or_else(|| {
            "That stroke isn't a straight line — draw one straight stroke across the ceiling."
                .to_string()
        })?;
        self.validate_ceiling_step_line(axis, coord)?;
        Ok((axis, coord))
    }

    /// The crosses-the-footprint validation: the line's coordinate must lie
    /// strictly inside the OUTLINE's bounding box on its axis (a line on or
    /// beyond the edge steps nothing). Bbox-level on purpose — the half-rect
    /// area always intersects the footprint when the coordinate is inside the
    /// bbox, and the resolve clip handles the rest. `Err` is user-facing.
    pub fn validate_ceiling_step_line(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
    ) -> Result<(), String> {
        let Some(((min_x, min_y), (max_x, max_y))) = self.outline.floor_plan.bounding_box() else {
            return Err(LINE_OFF_FOOTPRINT_MSG.to_string());
        };
        let (lo, hi) = match axis {
            ParameterAxis::X => (min_x, max_x),
            ParameterAxis::Y => (min_y, max_y),
        };
        if coord_m <= lo + LINE_CROSS_TOL || coord_m >= hi - LINE_CROSS_TOL {
            return Err(LINE_OFF_FOOTPRINT_MSG.to_string());
        }
        Ok(())
    }

    /// Snap a drawn/tapped step line onto the nearest parallel outline wall
    /// within [`CEILING_LINE_SNAP_TOL_M`]. The candidates are the SAME deduped
    /// reference walls the position prompt offers ([`reference_walls`]) — so a
    /// snapped line IS at a reference wall's coordinate, exact by
    /// construction, which is exactly why the shell SKIPS the position prompt
    /// for it: there is no "how far from the wall?" question left to ask.
    /// Walls at the outline bbox EXTREMES on the axis are EXCLUDED — a line
    /// snapped onto the boundary leaves one side of it empty (a step that
    /// steps nothing), and
    /// [`validate_ceiling_step_line`](Self::validate_ceiling_step_line) would
    /// reject the coordinate anyway.
    ///
    /// Returns `(snapped coordinate, outline wall vid)`, nearest wall wins;
    /// `None` when no qualifying wall is within tolerance. Pure — the shell
    /// decides when to call and remembers the drawn coordinate for un-snapping.
    pub fn snap_ceiling_step_line(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
    ) -> Option<(f32, VertexId)> {
        let ((min_x, min_y), (max_x, max_y)) = self.outline.floor_plan.bounding_box()?;
        let (lo, hi) = match axis {
            ParameterAxis::X => (min_x, max_x),
            ParameterAxis::Y => (min_y, max_y),
        };
        reference_walls(&self.outline.floor_plan, axis)
            .into_iter()
            .filter(|(_, c)| (c - lo).abs() > LINE_CROSS_TOL && (c - hi).abs() > LINE_CROSS_TOL)
            .map(|(vid, c)| (vid, c, (c - coord_m).abs()))
            .filter(|(_, _, d)| *d <= CEILING_LINE_SNAP_TOL_M)
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(vid, c, _)| (c, vid))
    }

    /// Build the position prompt for a drawn step line: the OUTLINE walls
    /// PARALLEL to it (the walls whose perpendicular axis is `axis`, deduped
    /// by coordinate), nearest first. `None` when there is no parallel
    /// reference — the approximate-stroke principle's LOGGED auto-resolve:
    /// the caller accepts the drawn coordinate.
    pub fn ceiling_line_position_prompt(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
    ) -> Option<LinePositionPrompt> {
        let refs = reference_walls(&self.outline.floor_plan, axis);
        if refs.is_empty() {
            log::info!(
                "ceiling-step line position on {axis:?} auto-resolved: no parallel outline \
                 wall to measure from — drawn coordinate {coord_m:.3} accepted"
            );
            return None;
        }
        let mut options: Vec<LinePositionOption> = refs
            .into_iter()
            .map(|(outline_wall_vid, reference_value)| LinePositionOption {
                outline_wall_vid,
                reference_value,
                sign: (coord_m - reference_value).signum(),
                current_distance: (coord_m - reference_value).abs(),
            })
            .collect();
        options.sort_by(|a, b| a.current_distance.total_cmp(&b.current_distance));
        Some(LinePositionPrompt {
            axis,
            options,
            current_idx: 0,
        })
    }

    /// Resolve the prompt with an entered distance: the line's exact
    /// coordinate is `reference + sign · value` (the side the line was drawn
    /// on is preserved). Rejects a non-finite or negative distance and a
    /// result that no longer crosses the footprint. `Err` is user-facing;
    /// nothing is mutated — the shell carries the returned coordinate forward.
    pub fn resolve_ceiling_line_position(
        &self,
        prompt: &LinePositionPrompt,
        value: f32,
    ) -> Result<f32, String> {
        if !value.is_finite() || value < 0.0 {
            return Err(format!(
                "distance must be a non-negative number (got {})",
                value
            ));
        }
        let opt = prompt
            .options
            .get(prompt.current_idx)
            .ok_or_else(|| "position prompt has no selected option".to_string())?;
        let coord = opt.reference_value + opt.sign * value;
        self.validate_ceiling_step_line(prompt.axis, coord)?;
        Ok(coord)
    }

    /// The step region for a resolved line: a cheap bounding-box HALF-RECT on
    /// the chosen side, grown by a margin so it safely overhangs the footprint
    /// — the resolve projection clips it (`with_z_deltas`), and the shell
    /// canonicalizes the session list to the exact derived pieces right after.
    /// `None` when the outline is empty or the half-rect is degenerate (the
    /// coordinate is outside the bbox on that side — `validate` rejects that
    /// earlier).
    pub fn ceiling_step_region(
        &self,
        axis: ParameterAxis,
        coord_m: f32,
        side: LineSide,
        delta_m: f64,
    ) -> Option<CeilingStepRegion> {
        let ((min_x, min_y), (max_x, max_y)) = self.outline.floor_plan.bounding_box()?;
        let margin = ((max_x - min_x).max(max_y - min_y) * 0.1).max(1.0);
        let (x0, x1, y0, y1) = match (axis, side) {
            (ParameterAxis::X, LineSide::Less) => {
                (min_x - margin, coord_m, min_y - margin, max_y + margin)
            }
            (ParameterAxis::X, LineSide::Greater) => {
                (coord_m, max_x + margin, min_y - margin, max_y + margin)
            }
            (ParameterAxis::Y, LineSide::Less) => {
                (min_x - margin, max_x + margin, min_y - margin, coord_m)
            }
            (ParameterAxis::Y, LineSide::Greater) => {
                (min_x - margin, max_x + margin, coord_m, max_y + margin)
            }
        };
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(CeilingStepRegion {
            polygon: vec![
                Point2f32::new(x0, y0),
                Point2f32::new(x1, y0),
                Point2f32::new(x1, y1),
                Point2f32::new(x0, y1),
            ],
            delta_m,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionState, ShapeSlot};
    use manip_2d_plan::FloorPlan;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A StoreyDoc with a finalised 10×6 metre outline in its slot.
    fn storey_10x6() -> StoreyDoc {
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 6.), p(0., 6.)]).unwrap();
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

    /// A near-vertical stroke fits to a line whose position varies on X; one
    /// outside the footprint is rejected with the stable user-facing message.
    #[test]
    fn fit_accepts_a_crossing_stroke_and_rejects_one_outside() {
        let doc = storey_10x6();
        let stroke = vec![p(4.02, 0.5), p(3.97, 2.0), p(4.03, 4.0), p(3.99, 5.5)];
        let (axis, coord) = doc.fit_ceiling_step_line(&stroke).expect("crossing line");
        assert_eq!(axis, ParameterAxis::X);
        assert!((coord - 4.0).abs() < 0.05);

        // Outside the footprint (x ≈ 14) → rejected, stable message.
        let outside = vec![p(14.0, 0.5), p(14.0, 5.5)];
        let err = doc.fit_ceiling_step_line(&outside).unwrap_err();
        assert_eq!(err, LINE_OFF_FOOTPRINT_MSG);
        // Not line-like at all → the fit message, not the footprint one.
        let blob = vec![p(2., 2.), p(4., 2.), p(4., 4.), p(2., 4.), p(2., 2.1)];
        assert!(doc
            .fit_ceiling_step_line(&blob)
            .unwrap_err()
            .contains("straight"));
    }

    /// validate: strictly inside the bbox on the line's axis; the edges (and
    /// an empty outline) are out.
    #[test]
    fn validate_requires_the_line_inside_the_bbox() {
        let doc = storey_10x6();
        assert!(doc
            .validate_ceiling_step_line(ParameterAxis::X, 5.0)
            .is_ok());
        assert!(doc
            .validate_ceiling_step_line(ParameterAxis::Y, 3.0)
            .is_ok());
        for (axis, coord) in [
            (ParameterAxis::X, 0.0),
            (ParameterAxis::X, 10.0),
            (ParameterAxis::X, -2.0),
            (ParameterAxis::Y, 6.0),
            (ParameterAxis::Y, 9.0),
        ] {
            assert_eq!(
                doc.validate_ceiling_step_line(axis, coord).unwrap_err(),
                LINE_OFF_FOOTPRINT_MSG,
                "{axis:?} {coord} must be rejected"
            );
        }
        let empty = StoreyDoc::default();
        assert!(empty
            .validate_ceiling_step_line(ParameterAxis::X, 5.0)
            .is_err());
    }

    /// The position prompt lists the PARALLEL outline walls nearest first; an
    /// outline with no parallel reference yields `None` (logged auto-resolve).
    #[test]
    fn position_prompt_lists_parallel_walls_nearest_first() {
        let doc = storey_10x6();
        // Vertical line at x = 4: references are the vertical outline walls
        // x = 0 (4 m away) and x = 10 (6 m away).
        let prompt = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0)
            .expect("two parallel walls");
        assert_eq!(prompt.axis, ParameterAxis::X);
        assert_eq!(prompt.options.len(), 2);
        assert!((prompt.options[0].reference_value - 0.0).abs() < 1e-4);
        assert!((prompt.options[0].current_distance - 4.0).abs() < 1e-4);
        assert!(prompt.options[0].sign > 0.0);
        assert!((prompt.options[1].reference_value - 10.0).abs() < 1e-4);
        assert!((prompt.options[1].current_distance - 6.0).abs() < 1e-4);
        assert!(prompt.options[1].sign < 0.0);

        // Cycle wraps through the options and back.
        let mut prompt = prompt;
        prompt.cycle();
        assert_eq!(prompt.current_idx, 1);
        prompt.cycle();
        assert_eq!(prompt.current_idx, 0);

        // No outline at all → no reference → None (auto-resolve).
        let empty = StoreyDoc::default();
        assert!(empty
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0)
            .is_none());
    }

    /// resolve: `reference + sign · value`, preserving the drawn side; rejects
    /// negative, non-finite, and off-footprint results.
    #[test]
    fn resolve_position_respects_sign_and_rejects_bad_values() {
        let doc = storey_10x6();
        let mut prompt = doc
            .ceiling_line_position_prompt(ParameterAxis::X, 4.0)
            .unwrap();
        // Nearest ref x = 0, drawn side +: 3 m → x = 3.
        assert!((doc.resolve_ceiling_line_position(&prompt, 3.0).unwrap() - 3.0).abs() < 1e-4);
        // Cycled to x = 10, drawn side −: 2.5 m → x = 7.5.
        prompt.cycle();
        assert!((doc.resolve_ceiling_line_position(&prompt, 2.5).unwrap() - 7.5).abs() < 1e-4);

        assert!(doc
            .resolve_ceiling_line_position(&prompt, -1.0)
            .unwrap_err()
            .contains("non-negative"));
        assert!(doc
            .resolve_ceiling_line_position(&prompt, f32::NAN)
            .unwrap_err()
            .contains("non-negative"));
        // 15 m from x = 10 going − lands at x = −5: off the footprint.
        assert_eq!(
            doc.resolve_ceiling_line_position(&prompt, 15.0)
                .unwrap_err(),
            LINE_OFF_FOOTPRINT_MSG
        );
    }

    /// The half-rect covers exactly the chosen side of the line, overhanging
    /// the footprint by the margin on the other three sides.
    #[test]
    fn ceiling_step_region_builds_the_half_rect_on_the_chosen_side() {
        let doc = storey_10x6();
        let region = doc
            .ceiling_step_region(ParameterAxis::X, 4.0, LineSide::Greater, 0.3)
            .expect("half-rect");
        assert!((region.delta_m - 0.3).abs() < 1e-12);
        assert_eq!(region.polygon.len(), 4);
        let min_x = region
            .polygon
            .iter()
            .map(|p| p.x)
            .fold(f32::INFINITY, f32::min);
        let max_x = region
            .polygon
            .iter()
            .map(|p| p.x)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            (min_x - 4.0).abs() < 1e-4,
            "Greater side starts at the line"
        );
        assert!(max_x > 10.0, "overhangs the footprint (resolve clips)");
        let min_y = region
            .polygon
            .iter()
            .map(|p| p.y)
            .fold(f32::INFINITY, f32::min);
        let max_y = region
            .polygon
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(min_y < 0.0 && max_y > 6.0, "full extent across the line");

        // Less side of a horizontal line: y ∈ [overhang, 2].
        let region = doc
            .ceiling_step_region(ParameterAxis::Y, 2.0, LineSide::Less, -0.2)
            .unwrap();
        let max_y = region
            .polygon
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!((max_y - 2.0).abs() < 1e-4, "Less side ends at the line");

        // Empty outline → None.
        assert!(StoreyDoc::default()
            .ceiling_step_region(ParameterAxis::X, 4.0, LineSide::Less, 0.3)
            .is_none());
    }

    /// A StoreyDoc with a finalised staircase outline whose vertical walls sit
    /// at x = 0, 6.75, 7, 10 and horizontal walls at y = 0, 2, 4, 6 — i.e.
    /// NON-extreme parallel references at x ∈ {6.75, 7} and y ∈ {2, 4}.
    fn storey_staircase() -> StoreyDoc {
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(10., 0.),
            p(10., 2.),
            p(7., 2.),
            p(7., 4.),
            p(6.75, 4.),
            p(6.75, 6.),
            p(0., 6.),
        ])
        .unwrap();
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

    /// Within tolerance of a non-extreme parallel wall the line snaps, and
    /// when TWO qualify the NEAREST wins; beyond tolerance there's no snap.
    #[test]
    fn snap_picks_the_nearest_non_extreme_wall_within_tolerance() {
        let doc = storey_staircase();
        // x = 6.9 is within 0.2 of BOTH x = 7 (0.1 away) and x = 6.75
        // (0.15 away) — the nearer wall, x = 7, wins.
        let (coord, _) = doc
            .snap_ceiling_step_line(ParameterAxis::X, 6.9)
            .expect("within tolerance");
        assert!(
            (coord - 7.0).abs() < 1e-6,
            "nearest wall wins (got {coord})"
        );
        // x = 6.82 flips the nearest to 6.75.
        let (coord, _) = doc.snap_ceiling_step_line(ParameterAxis::X, 6.82).unwrap();
        assert!((coord - 6.75).abs() < 1e-6);
        // The Y axis snaps to its non-extreme walls the same way.
        let (coord, _) = doc.snap_ceiling_step_line(ParameterAxis::Y, 2.1).unwrap();
        assert!((coord - 2.0).abs() < 1e-6);
        // Beyond tolerance: nearest non-extreme wall is 1.75 m away.
        assert!(doc.snap_ceiling_step_line(ParameterAxis::X, 5.0).is_none());
    }

    /// Walls at the outline bbox extremes never snap (a line on the boundary
    /// steps nothing) — even when they're the only parallel walls at all.
    #[test]
    fn snap_excludes_bbox_extreme_walls() {
        let doc = storey_staircase();
        // Near the x = 0 / x = 10 extremes — inside the footprint, within
        // tolerance of the boundary walls, but those never qualify.
        assert!(doc.snap_ceiling_step_line(ParameterAxis::X, 0.1).is_none());
        assert!(doc.snap_ceiling_step_line(ParameterAxis::X, 9.9).is_none());
        // A plain rectangle has ONLY extreme walls: nothing ever snaps.
        let rect = storey_10x6();
        assert!(rect
            .snap_ceiling_step_line(ParameterAxis::X, 0.15)
            .is_none());
        assert!(rect.snap_ceiling_step_line(ParameterAxis::Y, 5.9).is_none());
        // No outline at all → nothing to snap to.
        assert!(StoreyDoc::default()
            .snap_ceiling_step_line(ParameterAxis::X, 1.0)
            .is_none());
    }

    /// Reconstruction from persisted roofs: flat regions off the default by
    /// ≥ 1 mm become step regions with `delta = z − default`; the default
    /// piece, slivers, and a height-unset storey yield nothing.
    #[test]
    fn reconstruction_from_roofs_keeps_only_stepped_flat_regions() {
        use datatypes::ber::structural::{CeilingRegions, Storey};
        use domain::points::Point2f64;

        let square = |x0: f64, x1: f64| {
            vec![
                Point2f64::new(x0, 0.0),
                Point2f64::new(x1, 0.0),
                Point2f64::new(x1, 6.0),
                Point2f64::new(x0, 6.0),
            ]
        };
        let cid = Default::default();
        let mut roofs = CeilingRegions::default_for_footprint(vec![square(0.0, 4.0)], 2.4, cid);
        roofs.regions.extend(
            CeilingRegions::default_for_footprint(vec![square(4.0, 10.0)], 2.7, cid).regions,
        );
        let mut ber = Ber::default();
        ber.storeys.push(Storey {
            roofs,
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        });

        let regions = ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(regions.len(), 1, "only the off-default region");
        assert!((regions[0].delta_m - 0.3).abs() < 1e-9);
        assert!(regions[0].polygon.iter().any(|p| (p.x - 10.0).abs() < 1e-6));

        // Height unset → no default to be relative to → empty.
        ber.storeys[0].ceiling_height_m = None;
        assert!(ceiling_step_regions_from_roofs(&ber, 0).is_empty());
        // Missing storey → empty.
        assert!(ceiling_step_regions_from_roofs(&ber, 7).is_empty());
    }

    /// APEX-AWARE reconstruction: a fan piece whose BASE (apex vertices
    /// dropped) is off the default emits its triangle ring at the step delta;
    /// a fan over the default base emits nothing; a genuinely irregular slope
    /// (no apex match) is skipped.
    #[test]
    fn reconstruction_is_apex_aware() {
        use datatypes::ber::structural::{
            CeilingRegion, CeilingRegions, Polygon3D, RoofApex, Storey,
        };
        use domain::points::{Point2f64, Point3f64};

        let square = |x0: f64, x1: f64| {
            vec![
                Point2f64::new(x0, 0.0),
                Point2f64::new(x1, 0.0),
                Point2f64::new(x1, 4.0),
                Point2f64::new(x0, 4.0),
            ]
        };
        let cid = Default::default();
        // Left half: stepped to 2.7 then fanned to an apex at (1, 2, 4.2).
        let apex = RoofApex {
            point: Point2f64::new(1.0, 2.0),
            z_m: 4.2,
        };
        let mut roofs = CeilingRegions::default_for_footprint(vec![square(0.0, 2.0)], 2.7, cid)
            .with_apexes(std::slice::from_ref(&apex));
        let n_fan = roofs.regions.len();
        assert_eq!(n_fan, 4, "one triangle per boundary edge");
        // Right half: fanned at the DEFAULT base — reconstructs to nothing.
        roofs.regions.extend(
            CeilingRegions::default_for_footprint(vec![square(2.0, 4.0)], 2.4, cid)
                .with_apexes(&[RoofApex {
                    point: Point2f64::new(3.0, 2.0),
                    z_m: 3.5,
                }])
                .regions,
        );
        // An irregular slope no apex explains → skipped.
        roofs.regions.push(CeilingRegion {
            id: Default::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(10.0, 0.0, 2.0),
                Point3f64::new(12.0, 0.0, 2.5),
                Point3f64::new(11.0, 2.0, 3.0),
            ]),
            opes: Default::default(),
            construction_id: cid,
        });
        let mut ber = Ber::default();
        ber.storeys.push(Storey {
            roofs,
            ceiling_height_m: Some(2.4),
            roof_apexes: vec![
                apex,
                RoofApex {
                    point: Point2f64::new(3.0, 2.0),
                    z_m: 3.5,
                },
            ],
            ..Storey::default()
        });

        let regions = ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(regions.len(), n_fan, "only the stepped fan's triangles");
        let mut total = 0.0;
        for r in &regions {
            assert!((r.delta_m - 0.3).abs() < 1e-9, "each recovers the delta");
            assert_eq!(r.polygon.len(), 3, "triangle rings, apex corner kept");
            total += ring_area2(&r.polygon) / 2.0;
        }
        assert!((total - 8.0).abs() < 1e-4, "triangles tile the 2×4 half");
    }
}
