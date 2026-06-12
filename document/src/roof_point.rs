//! Roof-point (apex) and roof-ridge validation: declared high points shape
//! the ceiling region containing them — one apex fans it, two apexes in one
//! group form a ridge.
//!
//! The apexes are DURABLE input (`Storey::roof_apexes`, applied by the
//! resolve projection AFTER the ceiling-step areas — composition order is
//! law: default ceiling → steps → apexes). A ridge is NOT a separate model
//! object: it is simply two apexes grouped with the regions they shape
//! ([`CeilingRegions::apex_groups`], THE grouping authority). What lives here
//! are the assign-time validations the shell runs before `Ber::add_roof_apex`:
//!
//! - [`StoreyDoc::validate_roof_point`] — the cheap tap-time check against
//!   the OUTLINE (no `Ber` needed): the point must be strictly inside the
//!   footprint bbox on both axes, exactly the per-axis test the ceiling-step
//!   line uses.
//! - [`StoreyDoc::fit_roof_ridge_line`] — the stroke-time fit of a drawn
//!   ridge line (axis-aligned, endpoints = the stroke's own extent), again
//!   against the OUTLINE only.
//! - [`validate_roof_apex`] — the materialized check against the storey's
//!   RESOLVED roofs, GROUP-AWARE: the region strictly containing the point is
//!   looked up in the apex groups — 0 apexes is the first-point path (flat
//!   region required), 1 apex ALLOWS the second point (the ridge forms),
//!   ≥ 2 rejects (two high points per ceiling area for now).
//! - [`validate_roof_ridge`] — the materialized check for the LINE input,
//!   which places both apexes at once: both endpoints strictly inside the
//!   same un-apexed flat region.
//!
//! [`CeilingRegions::apex_groups`]:
//! datatypes::ber::structural::CeilingRegions::apex_groups

use crate::StoreyDoc;
use datatypes::ber::structural::{ridge_hub_switches, RoofApexGroup, Storey};
use domain::points::{Point2f32, Point2f64};
use manip_2d_plan::ParameterAxis;

/// User-facing rejection for a tapped point outside the storey footprint.
/// Kept stable — tests (and the shell) match on it.
const POINT_OFF_FOOTPRINT_MSG: &str =
    "The high point must be inside the storey footprint — tap a point inside the floor plan.";

/// User-facing rejection for an apex that lands in no ceiling region (or on a
/// region boundary — containment is strict at assign time). Kept stable.
const POINT_OUTSIDE_CEILING_MSG: &str =
    "Tap a point inside the ceiling — the high point must sit strictly inside one \
     ceiling area, not on a boundary.";

/// User-facing rejection for a third high point in a ceiling area whose apex
/// group already holds two (a ridge). Kept stable.
const REGION_HAS_RIDGE_MSG: &str =
    "That ceiling area already has a ridge — two high points per ceiling area for now.";

/// User-facing rejection for a ceiling area that slopes in a way no roof
/// apex explains (an irregular slope — there is no flat base to rise from).
/// Kept stable. Replaces the old one-high-point-only message: a second point
/// in a FANNED area is allowed now (it forms the ridge).
const REGION_IRREGULAR_SLOPE_MSG: &str =
    "That ceiling area slopes irregularly — high points need a flat ceiling area to rise from.";

/// User-facing rejection when the ridge between the two high points would
/// fold over the area's boundary (> 2 hub switches — the transition
/// triangles would overlap). Kept stable.
const RIDGE_TOO_IRREGULAR_MSG: &str =
    "That ceiling area is too irregular for a single ridge between those two points.";

/// User-facing rejection for ridge endpoints that don't share a region.
/// Kept stable.
const RIDGE_ENDS_SAME_REGION_MSG: &str =
    "Both ends of the ridge must sit strictly inside the same ceiling area.";

/// User-facing rejection for a ridge over a region that already has a high
/// point (the line input places BOTH apexes — it needs an empty area; add a
/// second point by tapping instead). Kept stable.
const RIDGE_REGION_OCCUPIED_MSG: &str =
    "That ceiling area already has a high point — draw the ridge over a ceiling area \
     without one, or tap to add the second point.";

/// User-facing rejection for a ridge whose endpoints coincide once quantized
/// to the stored (f32) precision. Kept stable.
const RIDGE_ENDS_COINCIDE_MSG: &str = "The ridge's two ends coincide — draw a longer ridge.";

/// User-facing rejection for a drawn ridge line outside the storey
/// footprint (its position or either endpoint). Kept stable.
const RIDGE_OFF_FOOTPRINT_MSG: &str =
    "The ridge must lie inside the storey footprint — draw it inside the floor plan.";

/// User-facing rejection for a typed ridge length that is zero-ish (a ridge
/// shorter than a centimetre is a point, not a ridge). Kept stable.
const RIDGE_LENGTH_TOO_SHORT_MSG: &str =
    "The ridge length must be more than a centimetre — enter a longer length.";

/// User-facing rejection for a typed-length ridge whose START sits strictly
/// inside no ceiling region (boundary excluded — the same strict containment
/// the height-stage check applies per end). Kept stable.
const RIDGE_START_OUTSIDE_CEILING_MSG: &str =
    "The ridge's start doesn't sit inside a ceiling area — a ridge can't start on a wall \
     or an area boundary yet.";

/// User-facing rejection for a typed ridge length whose far end would leave
/// the ceiling area its start sits in. Kept stable as the message PREFIX —
/// [`validate_roof_ridge_extent`] appends the permitted run so the user
/// knows what would fit.
const RIDGE_LENGTH_EXCEEDS_AREA_MSG: &str =
    "That length would push the ridge out of its ceiling area — enter a length that fits";

/// A typed ridge length at or below this (m) is rejected — it places the two
/// apexes (near-)coincident.
const MIN_RIDGE_LENGTH_M: f32 = 0.01;

/// The CLEARANCE (m) a typed ridge extent must keep from its ceiling area's
/// boundary: Length-stage acceptance must IMPLY the Height stage's STRICT
/// containment ([`validate_roof_ridge`] — boundary excluded), so the far end
/// stays a millimetre clear rather than touching. (Renamed from
/// `RIDGE_TOUCH_TOL_M`, whose bbox test ALLOWED a wall-to-wall touch — that
/// touch passed Length only to fail at Height; the reversal is deliberate.)
const RIDGE_CLEAR_TOL_M: f64 = 1e-3;

/// An apex within this (m) of its region's base height slopes nothing.
const MIN_APEX_DELTA_M: f64 = 0.01;

/// Index of the resolved region strictly containing `p`, with the
/// outside-ceiling rejection. (Index, not reference — group membership is by
/// region index.)
fn region_idx_strictly_containing(storey: &Storey, p: Point2f64) -> Result<usize, String> {
    let region = storey
        .roofs
        .region_strictly_containing(p)
        .ok_or_else(|| POINT_OUTSIDE_CEILING_MSG.to_string())?;
    Ok(storey
        .roofs
        .regions
        .iter()
        .position(|r| r.id == region.id)
        .expect("region returned from the same list"))
}

/// The ridge-foldover guard for a SECOND point in a one-apex group: run
/// [`ridge_hub_switches`] over the group's merged ring (the whole conceptual
/// area the ridge will form over — the resolved regions are fan/ridge faces
/// by now) and reject > 2. Picks the merged ring containing `p` (the new
/// point); a group degenerate enough to have none (every member warn-excluded
/// from the merge) skips the check with a warning — resolve warns and
/// proceeds on foldover anyway.
fn check_group_ridge(
    group: &RoofApexGroup,
    existing: Point2f64,
    p: Point2f64,
) -> Result<(), String> {
    let ring = group
        .merged_rings
        .iter()
        .find(|ring| {
            let pts: Vec<Point2f32> = ring
                .iter()
                .map(|q| Point2f32::new(q.x as f32, q.y as f32))
                .collect();
            manip_2d_plan::ring_contains_point(&pts, p.x as f32, p.y as f32)
        })
        .or_else(|| group.merged_rings.first());
    match ring {
        Some(ring) if ridge_hub_switches(ring, existing, p) > 2 => {
            Err(RIDGE_TOO_IRREGULAR_MSG.to_string())
        }
        Some(_) => Ok(()),
        None => {
            log::warn!("roof apex group has no merged ring — skipping the ridge hub-switch check");
            Ok(())
        }
    }
}

/// Validate a roof apex against the storey's RESOLVED roofs (the assign-time
/// check, run on the materialized `Storey` before `Ber::add_roof_apex`),
/// GROUP-AWARE:
///
/// - `z_m` must be finite and > 0 (it's ABSOLUTE height above the storey's
///   floor datum, not a delta). Below-base dips are allowed — warn-not-reject
///   lives in the resolve projection, not here.
/// - `p` must be STRICTLY inside one ceiling region
///   ([`region_strictly_containing`] — an apex on a boundary or the outline
///   is rejected up front; re-application uses the boundary-INCLUSIVE
///   predicate instead, two deliberately different predicates).
/// - That region is looked up in the storey's apex groups
///   ([`apex_groups`] against `storey.roof_apexes`):
///   - in NO group → the first-point path: the region must be FLAT
///     (`flat_z()` — a sloped region no apex explains is irregular) and the
///     base for the slope check is its height;
///   - in a ONE-apex group → ALLOWED: the second point forms a RIDGE. The
///     base is the GROUP's `base_z` (the resolved regions are sloped fan
///     faces — none of them is "the" base), and the ridge between the existing
///     apex and `p` must not fold over the group's merged ring
///     (> 2 hub switches → rejected, [`RIDGE_TOO_IRREGULAR_MSG`]);
///   - in a TWO-or-more-apex group → rejected: two high points per ceiling
///     area for now ([`REGION_HAS_RIDGE_MSG`]).
/// - `|z_m − base|` must be ≥ 1 cm — an apex at the base height slopes
///   nothing.
///
/// `Err` is user-facing.
///
/// [`region_strictly_containing`]:
/// datatypes::ber::structural::CeilingRegions::region_strictly_containing
/// [`apex_groups`]: datatypes::ber::structural::CeilingRegions::apex_groups
pub fn validate_roof_apex(storey: &Storey, p: Point2f64, z_m: f64) -> Result<(), String> {
    if !z_m.is_finite() || z_m <= 0.0 {
        return Err(format!(
            "height must be a positive number of metres (got {z_m})"
        ));
    }
    let ri = region_idx_strictly_containing(storey, p)?;
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    let base = match groups.iter().find(|g| g.region_idxs.contains(&ri)) {
        None => storey.roofs.regions[ri]
            .flat_z()
            .ok_or_else(|| REGION_IRREGULAR_SLOPE_MSG.to_string())?,
        Some(g) if g.apex_idxs.len() == 1 => {
            let existing = storey.roof_apexes[g.apex_idxs[0]].point;
            check_group_ridge(g, existing, p)?;
            g.base_z
        }
        Some(_) => return Err(REGION_HAS_RIDGE_MSG.to_string()),
    };
    if (z_m - base).abs() < MIN_APEX_DELTA_M {
        return Err(format!(
            "{z_m} m is within a centimetre of that ceiling area's height ({base} m) — \
             enter a height that actually slopes it."
        ));
    }
    Ok(())
}

/// Validate a roof RIDGE (the line input, which places BOTH apexes at once
/// via `Ber::add_roof_apex` ×2) against the storey's RESOLVED roofs:
///
/// - `z_m` finite and > 0 (one typed height — a level ridge; a sloping ridge
///   is entered via the two-point flow instead).
/// - The endpoints must stay DISTINCT after the f32 quantization
///   `add_roof_apex` applies (a zero-length ridge is a fan, not a ridge).
/// - Both (quantized) endpoints must sit STRICTLY inside the SAME ceiling
///   region.
/// - That region must be in NO apex group — the line needs an empty area
///   (a second point over an existing apex goes through the tap flow,
///   [`validate_roof_apex`]).
/// - The region must be FLAT (its `flat_z()` is the base) and
///   `|z_m − base|` ≥ 1 cm.
/// - The ridge must not fold over the region's boundary
///   ([`ridge_hub_switches`] ≤ 2).
///
/// `Err` is user-facing, all messages stable.
pub fn validate_roof_ridge(
    storey: &Storey,
    a: Point2f64,
    b: Point2f64,
    z_m: f64,
) -> Result<(), String> {
    if !z_m.is_finite() || z_m <= 0.0 {
        return Err(format!(
            "height must be a positive number of metres (got {z_m})"
        ));
    }
    // Work on the endpoints exactly as add_roof_apex will store them.
    let qa = Point2f64::new(a.x as f32 as f64, a.y as f32 as f64);
    let qb = Point2f64::new(b.x as f32 as f64, b.y as f32 as f64);
    if qa.x == qb.x && qa.y == qb.y {
        return Err(RIDGE_ENDS_COINCIDE_MSG.to_string());
    }
    let ra = region_idx_strictly_containing(storey, qa)?;
    let rb = region_idx_strictly_containing(storey, qb)?;
    if ra != rb {
        return Err(RIDGE_ENDS_SAME_REGION_MSG.to_string());
    }
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    if groups.iter().any(|g| g.region_idxs.contains(&ra)) {
        return Err(RIDGE_REGION_OCCUPIED_MSG.to_string());
    }
    let region = &storey.roofs.regions[ra];
    let base = region
        .flat_z()
        .ok_or_else(|| REGION_IRREGULAR_SLOPE_MSG.to_string())?;
    if (z_m - base).abs() < MIN_APEX_DELTA_M {
        return Err(format!(
            "{z_m} m is within a centimetre of that ceiling area's height ({base} m) — \
             enter a height that actually slopes it."
        ));
    }
    let ring: Vec<Point2f64> = region
        .polygon
        .exterior
        .iter()
        .map(|v| Point2f64::new(v.x, v.y))
        .collect();
    if ridge_hub_switches(&ring, qa, qb) > 2 {
        return Err(RIDGE_TOO_IRREGULAR_MSG.to_string());
    }
    Ok(())
}

/// Validate a typed roof-ridge LENGTH against the storey's RESOLVED roofs
/// (the materialized check — the old footprint-bbox test let a length clear
/// the bbox yet cross a ceiling-area boundary, failing only at the Height
/// stage; this rejects honestly at Length):
///
/// - The length itself must exceed a centimetre (shorter places the two
///   apexes coincident).
/// - The ridge's START — `(coord, lo)` for a vertical ridge (`axis` = X),
///   `(lo, coord)` for a horizontal one (the shell's ridge-endpoint axis
///   convention) — must sit STRICTLY inside one ceiling region
///   ([`region_strictly_containing`]; boundary excluded, so a snapped start
///   ON a wall is rejected here — recoverable via the snap tag).
/// - The resolved extent `[lo, lo + len]` must fit that region's contiguous
///   run along the extent axis from the start
///   ([`CeilingRegion::plan_span_through`]'s `t_max`), keeping
///   [`RIDGE_CLEAR_TOL_M`] clear of the boundary so Length-stage acceptance
///   implies the Height stage's strict containment.
///
/// `Err` is user-facing: the over-length rejection carries the permitted run
/// ("under N.NN m from that start") so the user knows what would fit.
///
/// [`region_strictly_containing`]:
/// datatypes::ber::structural::CeilingRegions::region_strictly_containing
/// [`CeilingRegion::plan_span_through`]:
/// datatypes::ber::structural::CeilingRegion::plan_span_through
pub fn validate_roof_ridge_extent(
    storey: &Storey,
    axis: ParameterAxis,
    coord_m: f32,
    lo: f32,
    len_m: f32,
) -> Result<(), String> {
    if !len_m.is_finite() || len_m <= MIN_RIDGE_LENGTH_M {
        return Err(RIDGE_LENGTH_TOO_SHORT_MSG.to_string());
    }
    // The start and the extent direction per the axis convention: `axis` is
    // the CROSS axis; the extent runs along `axis.other()`.
    let (start, dir) = match axis {
        ParameterAxis::X => (
            Point2f64::new(coord_m as f64, lo as f64),
            Point2f64::new(0.0, 1.0),
        ),
        ParameterAxis::Y => (
            Point2f64::new(lo as f64, coord_m as f64),
            Point2f64::new(1.0, 0.0),
        ),
    };
    let region = storey
        .roofs
        .region_strictly_containing(start)
        .ok_or_else(|| RIDGE_START_OUTSIDE_CEILING_MSG.to_string())?;
    let (_, max_len) = region
        .plan_span_through(start, dir)
        // Strictly inside but no span — a degenerate region; the start is
        // unusable as a ridge start all the same.
        .ok_or_else(|| RIDGE_START_OUTSIDE_CEILING_MSG.to_string())?;
    if (len_m as f64) > max_len - RIDGE_CLEAR_TOL_M {
        return Err(format!(
            "{RIDGE_LENGTH_EXCEEDS_AREA_MSG} (under {max_len:.2} m from that start)."
        ));
    }
    Ok(())
}

impl StoreyDoc {
    /// The cheap tap-time validation of a roof-point position against the
    /// OUTLINE (no `Ber` needed — the resolved-roofs check is
    /// [`validate_roof_apex`]): the point must be strictly inside the
    /// footprint bbox on BOTH axes. Reuses the ceiling-step line test per
    /// axis ([`validate_ceiling_step_line`]) with a point-flavoured message.
    /// `Err` is user-facing.
    ///
    /// [`validate_ceiling_step_line`]: StoreyDoc::validate_ceiling_step_line
    pub fn validate_roof_point(&self, p: Point2f32) -> Result<(), String> {
        for (axis, coord) in [(ParameterAxis::X, p.x), (ParameterAxis::Y, p.y)] {
            self.validate_ceiling_step_line(axis, coord)
                .map_err(|_| POINT_OFF_FOOTPRINT_MSG.to_string())?;
        }
        Ok(())
    }

    /// Fit a near-straight `stroke` (world/metre space) to an axis-aligned
    /// RIDGE line: [`manip_2d_plan::fit_axis_segment`] — axis + coordinate +
    /// the stroke's own projected EXTENT (a ridge has endpoints, taken as
    /// drawn). The cross coordinate must cross the footprint (the
    /// ceiling-step line test) and BOTH endpoints must lie strictly inside
    /// the footprint bbox ([`validate_roof_point`] per endpoint). The extent
    /// runs along the OTHER axis from `axis`: a vertical line (`axis` = X)
    /// has its endpoints at `(coord, lo)`/`(coord, hi)`; a horizontal one
    /// (`axis` = Y) at `(lo, coord)`/`(hi, coord)`. `Err` is user-facing.
    ///
    /// [`validate_roof_point`]: StoreyDoc::validate_roof_point
    pub fn fit_roof_ridge_line(
        &self,
        stroke: &[Point2f32],
    ) -> Result<(ParameterAxis, f32, (f32, f32)), String> {
        let (axis, coord, (lo, hi)) = manip_2d_plan::fit_axis_segment(stroke).ok_or_else(|| {
            "That stroke isn't a straight line — draw one straight stroke along the ridge."
                .to_string()
        })?;
        self.validate_ceiling_step_line(axis, coord)
            .map_err(|_| RIDGE_OFF_FOOTPRINT_MSG.to_string())?;
        for t in [lo, hi] {
            let p = match axis {
                // The extent runs along the OTHER axis from `axis`.
                ParameterAxis::X => Point2f32::new(coord, t),
                ParameterAxis::Y => Point2f32::new(t, coord),
            };
            self.validate_roof_point(p)
                .map_err(|_| RIDGE_OFF_FOOTPRINT_MSG.to_string())?;
        }
        Ok((axis, coord, (lo, hi)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionState, ShapeSlot};
    use datatypes::ber::structural::{CeilingRegion, CeilingRegions, Polygon3D, RoofApex, Storey};
    use domain::points::Point3f64;
    use manip_2d_plan::FloorPlan;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    fn apex(x: f64, y: f64, z: f64) -> RoofApex {
        RoofApex {
            point: Point2f64::new(x, y),
            z_m: z,
        }
    }

    /// A Storey whose roofs are one flat region at `height` over `ring`.
    fn storey_with_flat_ring(ring: Vec<Point2f64>, height: f64) -> Storey {
        Storey {
            roofs: CeilingRegions::default_for_footprint(vec![ring], height, Default::default()),
            ceiling_height_m: Some(height),
            ..Storey::default()
        }
    }

    /// A Storey whose roofs are one flat 4×4 region at the given height.
    fn storey_with_flat_roof(height: f64) -> Storey {
        storey_with_flat_ring(
            vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                Point2f64::new(4.0, 4.0),
                Point2f64::new(0.0, 4.0),
            ],
            height,
        )
    }

    /// Apply `apexes` to the storey EXACTLY the way resolve does: stamp the
    /// durable list and re-shape the roofs with the FULL list in one pass.
    fn apply_apexes(storey: &mut Storey, apexes: Vec<RoofApex>) {
        storey.roof_apexes = apexes;
        storey.roofs = std::mem::take(&mut storey.roofs).with_apexes(&storey.roof_apexes);
    }

    /// A 10×4 footprint with a deep SLOT cut from the right (y ∈ [1, 3],
    /// reaching in to x = 3) — a C-shape. A ridge from the left arm to the
    /// top arm wraps past the slot's far edge (4 hub switches).
    fn notched_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 1.0),
            Point2f64::new(3.0, 1.0),
            Point2f64::new(3.0, 3.0),
            Point2f64::new(10.0, 3.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ]
    }

    /// A valid apex passes; a below-base dip is allowed too (warn-not-reject).
    #[test]
    fn apex_strictly_inside_a_flat_region_is_accepted() {
        let storey = storey_with_flat_roof(2.4);
        assert!(validate_roof_apex(&storey, Point2f64::new(2.0, 2.0), 4.2).is_ok());
        // A dip below the base is valid input (rejection would fight the
        // warn-not-reject philosophy); the resolve projection warns at ≤ 0.1.
        assert!(validate_roof_apex(&storey, Point2f64::new(2.0, 2.0), 1.0).is_ok());
    }

    /// Non-finite and non-positive heights are rejected up front.
    #[test]
    fn non_finite_or_non_positive_height_is_rejected() {
        let storey = storey_with_flat_roof(2.4);
        for z in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let err = validate_roof_apex(&storey, Point2f64::new(2.0, 2.0), z).unwrap_err();
            assert!(err.contains("positive"), "z = {z} got: {err}");
        }
    }

    /// Strict containment: a point on a region boundary, outside every region,
    /// or in a storey with no roofs at all gets the stable outside-ceiling
    /// message.
    #[test]
    fn point_outside_or_on_a_boundary_is_rejected() {
        let storey = storey_with_flat_roof(2.4);
        for q in [
            Point2f64::new(0.0, 2.0), // on the outline boundary
            Point2f64::new(5.0, 2.0), // outside
            Point2f64::new(4.0, 4.0), // a corner
        ] {
            assert_eq!(
                validate_roof_apex(&storey, q, 4.0).unwrap_err(),
                POINT_OUTSIDE_CEILING_MSG,
                "{q:?}"
            );
        }
        assert_eq!(
            validate_roof_apex(&Storey::default(), Point2f64::new(2.0, 2.0), 4.0).unwrap_err(),
            POINT_OUTSIDE_CEILING_MSG
        );
    }

    /// THE TWO-POINT FLOW: a second point inside a one-apex group is ALLOWED
    /// (the ridge forms), with the ≥ 1 cm check against the GROUP's base —
    /// not the fan triangle's slope.
    #[test]
    fn second_point_in_a_fanned_area_is_allowed() {
        let mut storey = storey_with_flat_roof(2.4);
        apply_apexes(&mut storey, vec![apex(2.0, 2.0, 4.2)]);
        // Strictly inside one of the fan triangles.
        let q = Point2f64::new(2.0, 0.5);
        assert!(validate_roof_apex(&storey, q, 4.2).is_ok());
        // The base for the slope check is the group's base (2.4): within a
        // centimetre of IT is rejected even though the fan slopes there.
        let err = validate_roof_apex(&storey, q, 2.405).unwrap_err();
        assert!(err.contains("centimetre"), "got: {err}");
        assert!(err.contains("2.4"), "names the group base: {err}");
    }

    /// A third point in a two-apex (ridge) group is rejected with the stable
    /// already-has-a-ridge message.
    #[test]
    fn third_point_in_a_ridge_group_is_rejected() {
        let mut storey = storey_with_flat_roof(2.4);
        apply_apexes(&mut storey, vec![apex(1.0, 2.0, 4.2), apex(3.0, 2.0, 4.2)]);
        // Strictly inside one of the ridge's hub triangles.
        let err = validate_roof_apex(&storey, Point2f64::new(2.0, 0.5), 3.0).unwrap_err();
        assert_eq!(err, REGION_HAS_RIDGE_MSG);
    }

    /// A second point whose ridge would fold over the merged ring (> 2 hub
    /// switches — the slot between the arms) is rejected with the stable
    /// too-irregular message.
    #[test]
    fn second_point_folding_over_a_notch_is_rejected() {
        let mut storey = storey_with_flat_ring(notched_ring(), 2.4);
        apply_apexes(&mut storey, vec![apex(2.0, 2.0, 4.0)]);
        // (8, 3.4) sits strictly inside a fan triangle of the top arm; the
        // ridge (2,2) → (8,3.4) wraps past the slot (4 hub switches).
        let err = validate_roof_apex(&storey, Point2f64::new(8.0, 3.4), 4.0).unwrap_err();
        assert_eq!(err, RIDGE_TOO_IRREGULAR_MSG);
        // A point staying in the LEFT arm keeps a sane ridge (2 switches).
        assert!(validate_roof_apex(&storey, Point2f64::new(1.0, 2.0), 4.0).is_ok());
    }

    /// A sloped region in NO group (nothing durable explains its slope) gets
    /// the stable irregular-slope rejection.
    #[test]
    fn sloped_region_in_no_group_is_rejected_as_irregular() {
        let mut storey = storey_with_flat_roof(2.4);
        storey.roofs.regions.push(CeilingRegion {
            id: Default::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(10.0, 0.0, 2.0),
                Point3f64::new(14.0, 0.0, 2.5),
                Point3f64::new(12.0, 3.0, 3.0),
            ]),
            opes: Default::default(),
            construction_id: Default::default(),
        });
        let err = validate_roof_apex(&storey, Point2f64::new(12.0, 1.0), 4.0).unwrap_err();
        assert_eq!(err, REGION_IRREGULAR_SLOPE_MSG);
    }

    /// An apex within 1 cm of the region's base slopes nothing — rejected.
    #[test]
    fn apex_within_a_centimetre_of_the_base_is_rejected() {
        let storey = storey_with_flat_roof(2.4);
        let err = validate_roof_apex(&storey, Point2f64::new(2.0, 2.0), 2.405).unwrap_err();
        assert!(err.contains("centimetre"), "got: {err}");
        // Exactly 1 cm off qualifies.
        assert!(validate_roof_apex(&storey, Point2f64::new(2.0, 2.0), 2.41).is_ok());
    }

    // ── validate_roof_ridge ─────────────────────────────────────────────────

    /// The happy path: both ends strictly inside the same flat un-apexed
    /// region, distinct, off-base height, sane ring.
    #[test]
    fn ridge_in_an_empty_flat_region_is_accepted() {
        let storey = storey_with_flat_roof(2.4);
        let (a, b) = (Point2f64::new(1.0, 2.0), Point2f64::new(3.0, 2.0));
        assert!(validate_roof_ridge(&storey, a, b, 4.2).is_ok());
        // A level "dip ridge" below the base is allowed (warn-not-reject).
        assert!(validate_roof_ridge(&storey, a, b, 1.0).is_ok());
    }

    /// Non-finite / non-positive heights, near-base heights, and quantized-
    /// coincident endpoints are rejected with their stable messages.
    #[test]
    fn ridge_rejects_bad_heights_and_coincident_ends() {
        let storey = storey_with_flat_roof(2.4);
        let (a, b) = (Point2f64::new(1.0, 2.0), Point2f64::new(3.0, 2.0));
        for z in [f64::NAN, 0.0, -1.0] {
            let err = validate_roof_ridge(&storey, a, b, z).unwrap_err();
            assert!(err.contains("positive"), "z = {z} got: {err}");
        }
        let err = validate_roof_ridge(&storey, a, b, 2.405).unwrap_err();
        assert!(err.contains("centimetre"), "got: {err}");
        // Identical ends — and ends whose difference dies in f32 quantization.
        assert_eq!(
            validate_roof_ridge(&storey, a, a, 4.2).unwrap_err(),
            RIDGE_ENDS_COINCIDE_MSG
        );
        let almost_a = Point2f64::new(1.0 + 1e-12, 2.0);
        assert_eq!(
            validate_roof_ridge(&storey, a, almost_a, 4.2).unwrap_err(),
            RIDGE_ENDS_COINCIDE_MSG
        );
    }

    /// Endpoints outside the ceiling (or on a boundary) get the outside
    /// message; endpoints in two DIFFERENT regions get the same-region one.
    #[test]
    fn ridge_requires_both_ends_strictly_inside_one_region() {
        let storey = storey_with_flat_roof(2.4);
        assert_eq!(
            validate_roof_ridge(
                &storey,
                Point2f64::new(1.0, 2.0),
                Point2f64::new(5.0, 2.0), // outside
                4.2
            )
            .unwrap_err(),
            POINT_OUTSIDE_CEILING_MSG
        );
        assert_eq!(
            validate_roof_ridge(
                &storey,
                Point2f64::new(0.0, 2.0), // on the boundary
                Point2f64::new(3.0, 2.0),
                4.2
            )
            .unwrap_err(),
            POINT_OUTSIDE_CEILING_MSG
        );
        // Two flat regions side by side: one end in each.
        let mut storey = storey_with_flat_roof(2.4);
        storey.roofs.regions.extend(
            CeilingRegions::default_for_footprint(
                vec![vec![
                    Point2f64::new(4.0, 0.0),
                    Point2f64::new(8.0, 0.0),
                    Point2f64::new(8.0, 4.0),
                    Point2f64::new(4.0, 4.0),
                ]],
                2.4,
                Default::default(),
            )
            .regions,
        );
        assert_eq!(
            validate_roof_ridge(
                &storey,
                Point2f64::new(2.0, 2.0),
                Point2f64::new(6.0, 2.0),
                4.2
            )
            .unwrap_err(),
            RIDGE_ENDS_SAME_REGION_MSG
        );
    }

    /// A region already shaped by an apex group rejects the line input (the
    /// second point goes through the tap flow instead).
    #[test]
    fn ridge_over_an_apexed_region_is_rejected() {
        let mut storey = storey_with_flat_roof(2.4);
        apply_apexes(&mut storey, vec![apex(2.0, 2.0, 4.2)]);
        // Both ends strictly inside one fan triangle ((0,0),(4,0),(2,2)).
        let err = validate_roof_ridge(
            &storey,
            Point2f64::new(1.5, 0.3),
            Point2f64::new(2.5, 0.3),
            4.2,
        )
        .unwrap_err();
        assert_eq!(err, RIDGE_REGION_OCCUPIED_MSG);
    }

    /// A ridge wrapping past a deep notch (> 2 hub switches) is rejected
    /// with the stable too-irregular message.
    #[test]
    fn ridge_folding_over_a_notch_is_rejected() {
        let storey = storey_with_flat_ring(notched_ring(), 2.4);
        // Left arm → top arm: the slot's far edge hubs back to the first
        // apex between top-arm edges → 4 switches.
        let err = validate_roof_ridge(
            &storey,
            Point2f64::new(2.0, 2.0),
            Point2f64::new(8.0, 3.4),
            4.2,
        )
        .unwrap_err();
        assert_eq!(err, RIDGE_TOO_IRREGULAR_MSG);
        // A ridge staying within the left arm is fine (2 switches).
        assert!(validate_roof_ridge(
            &storey,
            Point2f64::new(1.5, 1.0),
            Point2f64::new(1.5, 3.0),
            4.2
        )
        .is_ok());
    }

    // ── validate_roof_point / fit_roof_ridge_line (outline-level) ──────────

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

    /// validate_roof_point: strictly inside the outline bbox on BOTH axes;
    /// edges, outside points, and an empty outline get the point-flavoured
    /// footprint message.
    #[test]
    fn roof_point_must_be_strictly_inside_the_outline_bbox() {
        let doc = storey_10x6();
        assert!(doc.validate_roof_point(p(5.0, 3.0)).is_ok());
        for q in [
            p(0.0, 3.0),  // on the left edge
            p(5.0, 6.0),  // on the top edge
            p(-1.0, 3.0), // outside on X
            p(5.0, 9.0),  // outside on Y
        ] {
            assert_eq!(
                doc.validate_roof_point(q).unwrap_err(),
                POINT_OFF_FOOTPRINT_MSG,
                "{q:?}"
            );
        }
        assert!(StoreyDoc::default()
            .validate_roof_point(p(1.0, 1.0))
            .is_err());
    }

    /// A near-vertical stroke fits to (X, x-coordinate, y-extent): the
    /// endpoints are built on the OTHER axis from `axis`.
    #[test]
    fn fit_ridge_line_vertical_stroke_extent_is_the_y_range() {
        let doc = storey_10x6();
        let stroke = vec![p(4.02, 1.5), p(3.97, 2.5), p(4.03, 3.5), p(3.99, 4.5)];
        let (axis, coord, (lo, hi)) = doc.fit_roof_ridge_line(&stroke).expect("fits");
        assert_eq!(axis, ParameterAxis::X);
        assert!((coord - 4.0).abs() < 0.05);
        assert!((lo - 1.5).abs() < 1e-6 && (hi - 4.5).abs() < 1e-6);
    }

    /// A near-horizontal stroke fits to (Y, y-coordinate, x-extent).
    #[test]
    fn fit_ridge_line_horizontal_stroke_extent_is_the_x_range() {
        let doc = storey_10x6();
        let stroke = vec![p(2.0, 3.02), p(4.0, 2.97), p(6.0, 3.03), p(8.0, 2.99)];
        let (axis, coord, (lo, hi)) = doc.fit_roof_ridge_line(&stroke).expect("fits");
        assert_eq!(axis, ParameterAxis::Y);
        assert!((coord - 3.0).abs() < 0.05);
        assert!((lo - 2.0).abs() < 1e-6 && (hi - 8.0).abs() < 1e-6);
    }

    /// Off-footprint strokes are rejected with the stable ridge message: a
    /// cross coordinate outside the bbox, an ENDPOINT on/over the bbox edge
    /// (the extent reaches the boundary), and a non-line blob keeps the
    /// straight-line message.
    #[test]
    fn fit_ridge_line_rejects_off_footprint_position_and_endpoints() {
        let doc = storey_10x6();
        // Position outside the footprint on X.
        let outside = vec![p(14.0, 1.0), p(14.0, 5.0)];
        assert_eq!(
            doc.fit_roof_ridge_line(&outside).unwrap_err(),
            RIDGE_OFF_FOOTPRINT_MSG
        );
        // Endpoint ON the bbox edge (y = 0): the line crosses fine but the
        // endpoint isn't strictly inside.
        let touching = vec![p(4.0, 0.0), p(4.0, 3.0)];
        assert_eq!(
            doc.fit_roof_ridge_line(&touching).unwrap_err(),
            RIDGE_OFF_FOOTPRINT_MSG
        );
        // Not line-like at all → the fit message, not the footprint one.
        let blob = vec![p(2., 2.), p(4., 2.), p(4., 4.), p(2., 4.), p(2., 2.1)];
        assert!(doc
            .fit_roof_ridge_line(&blob)
            .unwrap_err()
            .contains("straight"));
    }

    /// validate_roof_ridge_extent: a sane typed length passes; a zero-ish or
    /// non-finite length gets the too-short message; a start not strictly
    /// inside a ceiling region (boundary, outside, no roofs at all) gets the
    /// stable start-outside message; a length reaching the region boundary —
    /// wall-to-wall INCLUDED, the deliberate flip from the old bbox test —
    /// is rejected WITH the permitted run, because Length-stage acceptance
    /// must imply the Height stage's strict containment.
    #[test]
    fn validate_ridge_extent_checks_length_against_the_region_span() {
        // One flat 4×4 region. A vertical ridge (cross axis X at x = 2) from
        // y = 1: the run to the far wall is 3 m; a length comfortably under
        // it passes.
        let storey = storey_with_flat_roof(2.4);
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, 2.0).is_ok());
        for len in [0.0, 0.01, -2.0, f32::NAN] {
            assert_eq!(
                validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, len).unwrap_err(),
                RIDGE_LENGTH_TOO_SHORT_MSG,
                "len {len}"
            );
        }
        // Wall-to-wall (1 + 3 = 4, the far boundary) is REJECTED now: the
        // far end would land ON the boundary and fail the Height stage's
        // strict containment. So is anything longer — always with the stable
        // prefix and the permitted run from that start.
        for len in [3.0, 3.5, 9.0] {
            let err =
                validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, len).unwrap_err();
            assert!(
                err.starts_with(RIDGE_LENGTH_EXCEEDS_AREA_MSG),
                "len {len}: {err}"
            );
            assert!(
                err.contains("under 3.00 m"),
                "len {len} must carry the permitted run: {err}"
            );
        }
        // Just under the run (clear of the millimetre clearance) passes.
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, 2.99).is_ok());
        // A start ON a region boundary, outside every region, or in a storey
        // with no roofs: the stable start-outside message.
        for (storey, coord, lo) in [
            (&storey, 2.0, 0.0), // (2, 0): on the boundary
            (&storey, 9.0, 1.0), // (9, 1): outside
            (&Storey::default(), 2.0, 1.0),
        ] {
            assert_eq!(
                validate_roof_ridge_extent(storey, ParameterAxis::X, coord, lo, 1.0).unwrap_err(),
                RIDGE_START_OUTSIDE_CEILING_MSG,
                "coord {coord} lo {lo}"
            );
        }
        // Horizontal ridge (cross axis Y at y = 2) from x = 1 in the notched
        // C-shape: the run ends at the slot's near edge (x = 3), NOT the
        // ring's 10 m extent — the limit is the contiguous run through the
        // start, not the bbox.
        let storey = storey_with_flat_ring(notched_ring(), 2.4);
        let err = validate_roof_ridge_extent(&storey, ParameterAxis::Y, 2.0, 1.0, 2.0).unwrap_err();
        assert!(
            err.starts_with(RIDGE_LENGTH_EXCEEDS_AREA_MSG) && err.contains("under 2.00 m"),
            "got: {err}"
        );
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::Y, 2.0, 1.0, 1.9).is_ok());
    }
}
