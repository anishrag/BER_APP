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
//!   which places both apexes at once: the region is claimed by the ridge
//!   MIDPOINT (strictly inside), and each end by exactly that region —
//!   strictly inside it or ON its outer boundary (a wall-to-wall GABLE).
//! - [`validate_roof_ridge_length`] — the LENGTH-stage check (asked FIRST,
//!   before any location is known): an OPTIMISTIC bound — the length must
//!   fit SOME flat, un-apexed ceiling area's plan bbox.
//! - [`validate_roof_ridge_extent`] — the FINALIZE placement check, run at
//!   the last open location stage once the geometry is fully determined.
//! - [`centred_ridge_cross`] / [`centred_ridge_start`] — the 'Centre'
//!   button's COMPUTED SUGGESTIONS: the value that centres the pending ridge
//!   within its ceiling area, routed by the shell through the SAME assign
//!   path as a typed value.
//! - The closed-stroke RECT ridge (flat-topped hip / pavilion, durably FOUR
//!   corner apexes in one group): [`StoreyDoc::fit_roof_rect`],
//!   [`validate_roof_rect_lengths`] (stage-1 optimistic bound),
//!   [`validate_roof_rect_placement`] (the finalize check),
//!   [`validate_roof_rect_ridge`] (the height stage), and
//!   [`centred_rect_lo`] (the rect's Centre suggestion).
//!
//! [`CeilingRegions::apex_groups`]:
//! datatypes::ber::structural::CeilingRegions::apex_groups

use crate::StoreyDoc;
use datatypes::ber::structural::{
    rect_hip_switches, ridge_hub_switches, CeilingRegion, RoofApex, RoofApexGroup, Storey,
    APEX_BOUNDARY_EPS_M,
};
use domain::points::{to_f32, Point2f32, Point2f64};
use manip_2d_plan::ParameterAxis;

/// User-facing rejection for a tapped point outside the storey footprint.
/// Kept stable — tests (and the shell) match on it.
const POINT_OFF_FOOTPRINT_MSG: &str =
    "The high point must be inside the storey footprint — tap a point inside the floor plan.";

/// User-facing rejection for an apex that lands in no ceiling region.
const POINT_OUTSIDE_CEILING_MSG: &str =
    "Tap a point inside the ceiling — the high point must sit inside a ceiling area.";

/// User-facing rejection for an apex on an INTERNAL boundary shared by two
/// ceiling areas (ambiguous which one it would slope). A point on a single
/// region's edge/corner (the footprint perimeter) is allowed.
const POINT_ON_SHARED_BOUNDARY_MSG: &str =
    "That point sits on the line between two ceiling areas — tap inside the area \
     you want to slope, or on its outer wall/corner.";

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
/// Kept stable. (Reworded with the gable feature: ends may now sit ON the
/// shared region's outer boundary, so "strictly inside" came out.)
const RIDGE_ENDS_SAME_REGION_MSG: &str =
    "Both ends of the ridge must sit in the same ceiling area.";

/// User-facing rejection for a ridge end ON the line SHARED by two ceiling
/// areas: an outline-boundary end is a gable (one claimant), but an internal
/// shared boundary has no single owning area — both neighbours would need
/// coordinated face generation. Kept stable.
const RIDGE_END_ON_SHARED_BOUNDARY_MSG: &str =
    "A high point can't sit on the line between two ceiling areas — move it to one side.";

/// User-facing rejection for a ridge end claimed by NO ceiling area (outside
/// every region and beyond the boundary band). Kept stable.
const RIDGE_END_OUTSIDE_CEILING_MSG: &str =
    "The ridge's ends must stay inside (or on the edge of) one ceiling area.";

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

/// How far a hand-drawn ridge END may overshoot a perimeter wall and still be
/// accepted (clamped onto the wall) — a ridge meant to reach a wall is usually
/// drawn a little past it. Generous, since the finalize region check guards the
/// real geometry.
const RIDGE_END_REACH_M: f32 = 0.6;

/// User-facing rejection for a typed ridge length that is zero-ish (a ridge
/// shorter than a centimetre is a point, not a ridge). Kept stable.
const RIDGE_LENGTH_TOO_SHORT_MSG: &str =
    "The ridge length must be more than a centimetre — enter a longer length.";

/// User-facing rejection for a ridge whose MIDPOINT sits strictly inside no
/// ceiling region — the ridge's region is claimed by its midpoint (never the
/// ends, which may legally sit ON the boundary: a gable). A midpoint on a
/// boundary means the ridge runs ALONG a wall or a step line, which makes no
/// roof. Kept stable. (Replaces `RIDGE_START_OUTSIDE_CEILING_MSG`, whose
/// "can't start on a wall … yet" promise the gable feature delivered.)
const RIDGE_OUTSIDE_CEILING_MSG: &str =
    "The ridge doesn't sit inside a ceiling area — draw it across one ceiling area, \
     not along a wall or an area boundary.";

/// User-facing rejection for a typed ridge length whose far end would leave
/// the ceiling area its start sits in. Kept stable as the message PREFIX —
/// [`validate_roof_ridge_extent`] appends the permitted run so the user
/// knows what would fit.
const RIDGE_LENGTH_EXCEEDS_AREA_MSG: &str =
    "That length would push the ridge out of its ceiling area — enter a length that fits";

/// User-facing rejection for a typed ridge length longer than EVERY flat,
/// un-apexed ceiling area's plan-bbox extent along the ridge axis — the
/// Length stage's optimistic bound ([`validate_roof_ridge_length`]). Kept
/// stable as the message PREFIX; the permitted bound is appended.
const RIDGE_LENGTH_NO_AREA_MSG: &str = "No ceiling area is that long — enter a length under";

/// User-facing rejection for a closed roof-mode stroke that isn't
/// rectangle-like. Kept stable.
const RECT_NOT_RECT_MSG: &str =
    "That stroke isn't a rectangle — draw one closed rectangular stroke over the flat top.";

/// User-facing rejection for a drawn flat-top rectangle outside the storey
/// footprint. Kept stable.
const RECT_OFF_FOOTPRINT_MSG: &str =
    "The flat top must lie inside the storey footprint — draw it inside the floor plan.";

/// User-facing rejection for a flat top whose CENTRE sits strictly inside no
/// ceiling region — the rect's region is claimed by its centre (never the
/// corners, which may legally sit ON the boundary). Kept stable.
const RECT_OUTSIDE_CEILING_MSG: &str =
    "The flat top doesn't sit inside a ceiling area — draw it within one ceiling area.";

/// User-facing rejection for a flat-top side at or below a centimetre (the
/// corners would collapse). Kept stable.
const RECT_SIDE_TOO_SHORT_MSG: &str =
    "Each side of the flat top must be more than a centimetre — enter a longer length.";

/// User-facing rejection for a flat top over a ceiling area an apex group
/// already shapes. Kept stable.
const RECT_REGION_OCCUPIED_MSG: &str =
    "That ceiling area already has a high point — draw the flat top over a ceiling area \
     without one.";

/// User-facing rejection when the ceiling area's boundary wraps past one of
/// the flat top's hip lines ([`rect_hip_switches`] ≠ 4 — the loft would
/// fold). Kept stable.
const RECT_TOO_IRREGULAR_MSG: &str =
    "That ceiling area is too irregular for a flat-topped roof there.";

/// User-facing rejection for a typed flat-top side length whose extent would
/// leave the ceiling area. Kept stable as the message PREFIX — the permitted
/// run is appended (the ridge convention).
const RECT_LENGTH_EXCEEDS_AREA_MSG: &str =
    "That length would push the flat top out of its ceiling area — enter a length that fits";

/// User-facing rejection for a flat-top corner claimed by NO ceiling area.
/// Kept stable.
const RECT_CORNER_OUTSIDE_CEILING_MSG: &str =
    "The flat top's corners must stay inside (or on the edge of) one ceiling area.";

/// User-facing rejection for flat-top corners spilling into a DIFFERENT
/// ceiling area than the centre's. Kept stable.
const RECT_CORNERS_SAME_REGION_MSG: &str =
    "All four corners of the flat top must sit in the same ceiling area.";

/// A typed ridge length at or below this (m) is rejected — it places the two
/// apexes (near-)coincident.
const MIN_RIDGE_LENGTH_M: f32 = 0.01;

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

/// Index of the region UNIQUELY claiming `p`: strictly inside, or on the
/// boundary of EXACTLY ONE region (a footprint edge/corner — a valid anchor for
/// a corner-raised slope). `Err` if `p` is outside every region, or on an
/// INTERNAL boundary shared by several (ambiguous which it slopes). Matches the
/// boundary-inclusive predicate the fan (`with_apexes`) re-applies with.
fn region_idx_uniquely_claiming(storey: &Storey, p: Point2f64) -> Result<usize, String> {
    match storey.roofs.claim_regions(p).as_slice() {
        [] => Err(POINT_OUTSIDE_CEILING_MSG.to_string()),
        [i] => Ok(*i),
        _ => Err(POINT_ON_SHARED_BOUNDARY_MSG.to_string()),
    }
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
            let pts: Vec<Point2f32> = ring.iter().copied().map(to_f32).collect();
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
    let ri = region_idx_uniquely_claiming(storey, p)?;
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

/// Require ridge end `q` (already quantized) to be claimed by EXACTLY the
/// region `ridx` ([`CeilingRegions::claim_regions`]): an end strictly inside
/// it, or on its OUTER boundary (one claimant — a gable end), passes; an end
/// on an INTERNAL shared boundary, in a different region, or claimed by
/// nothing is rejected with its stable message.
///
/// [`CeilingRegions::claim_regions`]:
/// datatypes::ber::structural::CeilingRegions::claim_regions
fn check_end_claims(storey: &Storey, q: Point2f64, ridx: usize) -> Result<(), String> {
    let claims = storey.roofs.claim_regions(q);
    // The end may be strictly inside the region, on its OUTER boundary (gable),
    // OR on an INTERNAL boundary that includes it (a mono-pitch ridge along the
    // junction — the area it slopes is fixed by the apex side hint at resolve).
    if claims.contains(&ridx) {
        Ok(())
    } else if claims.is_empty() {
        Err(RIDGE_END_OUTSIDE_CEILING_MSG.to_string())
    } else {
        Err(RIDGE_ENDS_SAME_REGION_MSG.to_string())
    }
}

/// Validate a roof RIDGE (the line input, which places BOTH apexes at once
/// via `Ber::add_roof_apex` ×2) against the storey's RESOLVED roofs:
///
/// - `z_m` finite and > 0 (one typed height — a level ridge; a sloping ridge
///   is entered via the two-point flow instead).
/// - The endpoints must stay DISTINCT after the f32 quantization
///   `add_roof_apex` applies (a zero-length ridge is a fan, not a ridge).
/// - The ridge's region is claimed by its MIDPOINT, which must sit STRICTLY
///   inside one ceiling region — the midpoint, never the ends, picks the
///   region (the determinism dodge: the ends may legally sit ON the
///   boundary — a wall-to-wall GABLE ridge).
/// - Each (quantized) endpoint must be claimed by EXACTLY that region
///   ([`check_end_claims`]): strictly inside it or on its OUTER boundary;
///   an end on an internal shared boundary is rejected
///   ([`RIDGE_END_ON_SHARED_BOUNDARY_MSG`]).
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
    let qa = RoofApex::quantize_point(a);
    let qb = RoofApex::quantize_point(b);
    if qa.x == qb.x && qa.y == qb.y {
        return Err(RIDGE_ENDS_COINCIDE_MSG.to_string());
    }
    let mid = Point2f64::new((qa.x + qb.x) / 2.0, (qa.y + qb.y) / 2.0);
    // Try every area claiming the midpoint (mirrors `validate_roof_ridge_extent`):
    // a junction ridge on a shared boundary is valid if it slopes EITHER
    // neighbour; a strict-interior midpoint has one claimant. Most specific
    // rejection wins.
    let claims = storey.roofs.claim_regions(mid);
    if claims.is_empty() {
        return Err(RIDGE_OUTSIDE_CEILING_MSG.to_string());
    }
    let mut last_err = RIDGE_OUTSIDE_CEILING_MSG.to_string();
    for ra in claims {
        match ridge_fits_region_height(storey, ra, qa, qb, z_m) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Whether a level ridge `qa→qb` at height `z_m` fits ceiling region `ra`: both
/// ends claimed by it, the region un-apexed, flat, sloped by `z_m` (≥ 1 cm off
/// its base), and the ridge not folding over its boundary. The per-region body
/// of [`validate_roof_ridge`], factored out so a boundary ridge can be tried
/// against each claiming area.
fn ridge_fits_region_height(
    storey: &Storey,
    ra: usize,
    qa: Point2f64,
    qb: Point2f64,
    z_m: f64,
) -> Result<(), String> {
    for q in [qa, qb] {
        check_end_claims(storey, q, ra)?;
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

/// Validate a typed roof-ridge LENGTH against the storey's RESOLVED roofs —
/// the LENGTH stage's check, asked FIRST in the flow, before any location
/// question, so only an OPTIMISTIC bound is knowable:
///
/// - The length itself must be finite and exceed a centimetre (shorter
///   places the two apexes coincident).
/// - The length must fit in SOME ceiling area: `len ≤` the max over flat,
///   un-apexed regions of the plan-bbox extent along the ridge (extent)
///   axis `axis.other()`. A cheap over-approximation — a length that fits
///   the bbox but not the actual run is rejected later, at the location
///   stage that fully determines the geometry (the finalize check,
///   [`validate_roof_ridge_extent`], whose rejection then attaches to the
///   position the user just entered).
///
/// `Err` is user-facing, messages stable: the over-length rejection carries
/// the permitted bound ([`RIDGE_LENGTH_NO_AREA_MSG`] prefix).
pub fn validate_roof_ridge_length(
    storey: &Storey,
    axis: ParameterAxis,
    len_m: f32,
) -> Result<(), String> {
    if !len_m.is_finite() || len_m <= MIN_RIDGE_LENGTH_M {
        return Err(RIDGE_LENGTH_TOO_SHORT_MSG.to_string());
    }
    // Only flat, un-apexed regions can host a new ridge — the same region
    // predicates the finalize check applies, lifted to the optimistic
    // "could ANY area take it" form.
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    let max_extent = storey
        .roofs
        .regions
        .iter()
        .enumerate()
        .filter(|(i, r)| r.flat_z().is_some() && !groups.iter().any(|g| g.region_idxs.contains(i)))
        .map(|(_, r)| {
            // The region's plan-bbox extent along the EXTENT axis (the
            // ridge runs along `axis.other()` per the axis convention).
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for v in &r.polygon.exterior {
                let c = match axis {
                    ParameterAxis::X => v.y,
                    ParameterAxis::Y => v.x,
                };
                min = min.min(c);
                max = max.max(c);
            }
            max - min
        })
        .fold(0.0_f64, f64::max);
    if (len_m as f64) > max_extent {
        return Err(format!("{RIDGE_LENGTH_NO_AREA_MSG} {max_extent:.2} m."));
    }
    Ok(())
}

/// Validate the pending ridge's PLACEMENT against the storey's RESOLVED
/// roofs — THE FINALIZE CHECK, run at the moment the geometry becomes fully
/// determined (the last open location stage, or straight after the typed
/// length when every location was snapped), so its rejection always attaches
/// to a value the user just entered:
///
/// - The ridge's MIDPOINT — `(coord, lo + len/2)` for a vertical ridge
///   (`axis` = X; the shell's ridge-endpoint axis convention) — must sit
///   STRICTLY inside one ceiling region
///   ([`region_strictly_containing`]). The midpoint, never the ends, picks
///   the region: the ends may legally lie ON the boundary (a wall-to-wall
///   GABLE ridge — the old millimetre clearance `RIDGE_CLEAR_TOL_M` is
///   retired; this is its deliberate reversal).
/// - That region must be flat and un-apexed (the same predicates the Height
///   stage re-checks — rejecting here keeps the Height stage's only
///   rejectable input the height value itself).
/// - The resolved extent `[lo, lo + len]` must fit that region's contiguous
///   run THROUGH THE MIDPOINT along the extent axis
///   ([`CeilingRegion::plan_span_through`]), boundary touch allowed within
///   [`APEX_BOUNDARY_EPS_M`].
/// - Each end must be claimed by exactly that region
///   ([`check_end_claims`] — an end on an INTERNAL shared boundary is
///   rejected here, not at the Height stage).
/// - The ridge must not fold over the region's boundary
///   ([`ridge_hub_switches`] ≤ 2 over the quantized prospective ends — the
///   Height stage re-checks, but rejecting a notched region HERE keeps the
///   no-late-rejection invariant: no height could fix it).
/// - The length is re-guarded cheaply (> 1 cm): the SKIP path accepts the
///   drawn length without the Length stage's validation.
///
/// `Err` is user-facing: the over-length rejection keeps the stable
/// [`RIDGE_LENGTH_EXCEEDS_AREA_MSG`] prefix and carries the permitted run —
/// now the FULL wall-to-wall span through the midpoint.
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
    // The midpoint and the extent direction per the axis convention: `axis`
    // is the CROSS axis; the extent runs along `axis.other()`.
    let half = len_m as f64 / 2.0;
    let mid_along = lo as f64 + half;
    let (mid, dir) = match axis {
        ParameterAxis::X => (
            Point2f64::new(coord_m as f64, mid_along),
            Point2f64::new(0.0, 1.0),
        ),
        ParameterAxis::Y => (
            Point2f64::new(mid_along, coord_m as f64),
            Point2f64::new(1.0, 0.0),
        ),
    };
    // Try EVERY area that claims the midpoint, not just the first: a ridge on
    // an internal shared boundary (a mono-pitch junction) is claimed by both
    // neighbours, and is valid if it fits EITHER — the user picks which slopes
    // (the ChooseArea prompt) and the other gets a step. A strict-interior
    // midpoint has exactly one claimant, so this preserves the prior single-
    // region behaviour. The most specific rejection is surfaced.
    let claims = storey.roofs.claim_regions(mid);
    if claims.is_empty() {
        return Err(RIDGE_OUTSIDE_CEILING_MSG.to_string());
    }
    let mut last_err = RIDGE_OUTSIDE_CEILING_MSG.to_string();
    for ridx in claims {
        match ridge_fits_region_extent(storey, ridx, mid, dir, half, axis, coord_m, lo, len_m) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Whether the prospective ridge `(axis, coord_m, lo, len_m)` (midpoint `mid`,
/// extent direction `dir`, half-length `half`) fits ceiling region `ridx`: the
/// region must be un-apexed, flat, span the full ridge along `dir`
/// (boundary-tolerant — a junction ridge runs along the region's edge), have
/// both ends claimed by it, and not fold over its boundary. The per-region body
/// of [`validate_roof_ridge_extent`], factored out so a boundary ridge can be
/// tried against each claiming area.
#[allow(clippy::too_many_arguments)]
fn ridge_fits_region_extent(
    storey: &Storey,
    ridx: usize,
    mid: Point2f64,
    dir: Point2f64,
    half: f64,
    axis: ParameterAxis,
    coord_m: f32,
    lo: f32,
    len_m: f32,
) -> Result<(), String> {
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    if groups.iter().any(|g| g.region_idxs.contains(&ridx)) {
        return Err(RIDGE_REGION_OCCUPIED_MSG.to_string());
    }
    let region = &storey.roofs.regions[ridx];
    if region.flat_z().is_none() {
        return Err(REGION_IRREGULAR_SLOPE_MSG.to_string());
    }
    let (t_min, t_max) = region
        .plan_span_through_inclusive(mid, dir)
        // Claims the midpoint but yields no span — a degenerate region; the
        // midpoint is unusable as a ridge anchor all the same.
        .ok_or_else(|| RIDGE_OUTSIDE_CEILING_MSG.to_string())?;
    // Boundary touch ALLOWED: lo ≥ mid + t_min − EPS and
    // lo + len ≤ mid + t_max + EPS, i.e. each half-extent fits its side of
    // the span within the boundary band.
    if t_min > -half + APEX_BOUNDARY_EPS_M || t_max < half - APEX_BOUNDARY_EPS_M {
        let span = t_max - t_min;
        return Err(format!(
            "{RIDGE_LENGTH_EXCEEDS_AREA_MSG} (the ceiling area runs {span:.2} m \
             wall to wall there)."
        ));
    }
    // Both ends claimed by the candidate region: an end on an internal shared
    // boundary that this region also touches is fine (it's the junction ridge);
    // an end that leaves it entirely is rejected here, not at the Height stage.
    let (qa, qb) = match axis {
        ParameterAxis::X => (
            Point2f64::new(coord_m as f64, lo as f64),
            Point2f64::new(coord_m as f64, (lo + len_m) as f64),
        ),
        ParameterAxis::Y => (
            Point2f64::new(lo as f64, coord_m as f64),
            Point2f64::new((lo + len_m) as f64, coord_m as f64),
        ),
    };
    for q in [qa, qb] {
        check_end_claims(storey, RoofApex::quantize_point(q), ridx)?;
    }
    // The foldover fence, on the quantized prospective ends — the SAME check
    // the Height stage re-runs ([`validate_roof_ridge`]'s hub-switch guard).
    // Without it a notched region passes finalize and is rejected LATE, at
    // the Height stage, where no typed height can fix it.
    let ring: Vec<Point2f64> = region
        .polygon
        .exterior
        .iter()
        .map(|v| Point2f64::new(v.x, v.y))
        .collect();
    if ridge_hub_switches(
        &ring,
        RoofApex::quantize_point(qa),
        RoofApex::quantize_point(qb),
    ) > 2
    {
        return Err(RIDGE_TOO_IRREGULAR_MSG.to_string());
    }
    Ok(())
}

/// The plan ring (world metres) of the resolved ceiling region STRICTLY
/// CONTAINING the ridge midpoint for `(axis, coord_m, lo, len_m)`, or `None`
/// when the midpoint is in no region. Reuses the same midpoint formula and
/// `region_strictly_containing` machinery as
/// [`validate_roof_ridge_extent`] (`axis` is the CROSS axis; the extent runs
/// along `axis.other()`, midpoint at `lo + len/2`).
///
/// This is the priority ring for
/// [`StoreyDoc::ceiling_line_position_prompt`](crate::StoreyDoc::ceiling_line_position_prompt):
/// passing it makes a ridge's DEFAULT reference wall bound its OWN ceiling
/// area rather than the globally nearest wall (which on an L-shape is often a
/// wall in the other arm). Owned `&Storey` in, owned ring out — no shell, UI,
/// or DB dependency crosses into the document crate.
pub fn ridge_region_ring(
    storey: &Storey,
    axis: ParameterAxis,
    coord_m: f32,
    lo: f32,
    len_m: f32,
) -> Option<Vec<Point2f64>> {
    let mid_along = lo as f64 + len_m as f64 / 2.0;
    let mid = match axis {
        ParameterAxis::X => Point2f64::new(coord_m as f64, mid_along),
        ParameterAxis::Y => Point2f64::new(mid_along, coord_m as f64),
    };
    let region = storey.roofs.region_strictly_containing(mid)?;
    Some(
        region
            .polygon
            .exterior
            .iter()
            .map(|v| Point2f64::new(v.x, v.y))
            .collect(),
    )
}

/// The pending ridge's MIDPOINT (per the shell's axis convention — `axis` is
/// the CROSS axis, the extent runs along `axis.other()`), the resolved
/// region strictly containing it, and that region's contiguous run through
/// the midpoint along `dir` — the shared lookup behind the two Centre
/// suggestions. Both Err arms (midpoint in no region; strictly inside but no
/// span — degenerate) collapse to the stable ridge-outside message.
fn ridge_span_through_midpoint(
    storey: &Storey,
    axis: ParameterAxis,
    coord_m: f32,
    lo: f32,
    len_m: f32,
    dir: Point2f64,
) -> Result<(f64, f64), String> {
    let mid_along = lo as f64 + len_m as f64 / 2.0;
    let mid = match axis {
        ParameterAxis::X => Point2f64::new(coord_m as f64, mid_along),
        ParameterAxis::Y => Point2f64::new(mid_along, coord_m as f64),
    };
    let ridx = region_idx_strictly_containing(storey, mid)
        .map_err(|_| RIDGE_OUTSIDE_CEILING_MSG.to_string())?;
    storey.roofs.regions[ridx]
        .plan_span_through(mid, dir)
        .ok_or_else(|| RIDGE_OUTSIDE_CEILING_MSG.to_string())
}

/// The cross coordinate that CENTRES the pending ridge within its ceiling
/// area — the 'Centre' button's computed suggestion for the CROSS location
/// stage: the midpoint of the region's span through the ridge's current
/// (drawn / resolved-so-far) midpoint along the CROSS axis
/// ([`CeilingRegion::plan_span_through`] — the contiguous run, not the
/// bbox, so a notched region centres within the run the midpoint actually
/// sits in). The region is the one STRICTLY containing the midpoint (the
/// midpoint, never the ends, picks the region — the finalize convention).
/// The centred coordinate lies on the span through the midpoint, which
/// stays inside the region by construction — no separate footprint check.
/// `Err` (user-facing, stable) when the midpoint sits in no region.
///
/// The shell routes the returned value through the SAME advance path as a
/// typed cross distance, so validation/finalize behaviour is identical by
/// construction.
///
/// [`CeilingRegion::plan_span_through`]:
/// datatypes::ber::structural::CeilingRegion::plan_span_through
pub fn centred_ridge_cross(
    storey: &Storey,
    axis: ParameterAxis,
    coord_m: f32,
    lo: f32,
    len_m: f32,
) -> Result<f32, String> {
    // The span runs along the CROSS axis: `axis` itself.
    let dir = match axis {
        ParameterAxis::X => Point2f64::new(1.0, 0.0),
        ParameterAxis::Y => Point2f64::new(0.0, 1.0),
    };
    let (t_min, t_max) = ridge_span_through_midpoint(storey, axis, coord_m, lo, len_m, dir)?;
    Ok((coord_m as f64 + (t_min + t_max) / 2.0) as f32)
}

/// The extent START that CENTRES the pending ridge along its ceiling area —
/// the 'Centre' button's computed suggestion for the START location stage:
/// the region's span through the ridge's current midpoint along the ALONG
/// axis (`axis.other()`), returning `span_mid − len/2`. `Err` (user-facing,
/// stable) when the midpoint sits in no region
/// ([`RIDGE_OUTSIDE_CEILING_MSG`]) or the length exceeds the span — beyond
/// the boundary-touch band ([`APEX_BOUNDARY_EPS_M`] per side, the finalize
/// check's allowance) — carrying the permitted run with the stable
/// [`RIDGE_LENGTH_EXCEEDS_AREA_MSG`] prefix. A full-span length centres to
/// the exact wall-to-wall gable.
pub fn centred_ridge_start(
    storey: &Storey,
    axis: ParameterAxis,
    coord_m: f32,
    lo: f32,
    len_m: f32,
) -> Result<f32, String> {
    // The span runs along the ALONG axis: `axis.other()`.
    let dir = match axis {
        ParameterAxis::X => Point2f64::new(0.0, 1.0),
        ParameterAxis::Y => Point2f64::new(1.0, 0.0),
    };
    let (t_min, t_max) = ridge_span_through_midpoint(storey, axis, coord_m, lo, len_m, dir)?;
    let span = t_max - t_min;
    if len_m as f64 > span + 2.0 * APEX_BOUNDARY_EPS_M {
        return Err(format!(
            "{RIDGE_LENGTH_EXCEEDS_AREA_MSG} (the ceiling area runs {span:.2} m \
             wall to wall there)."
        ));
    }
    let mid_along = lo as f64 + len_m as f64 / 2.0;
    let span_mid = mid_along + (t_min + t_max) / 2.0;
    Ok((span_mid - len_m as f64 / 2.0) as f32)
}

// ── The closed-stroke RECT ridge (flat-topped hip / pavilion) ───────────────

/// The four corners of the rect `(lo, hi)`, QUANTIZED exactly as
/// `Ber::add_roof_apex` will store them, in the canonical CCW order
/// c00 → c10 → c11 → c01.
fn rect_corners(lo: Point2f64, hi: Point2f64) -> [Point2f64; 4] {
    let qlo = RoofApex::quantize_point(lo);
    let qhi = RoofApex::quantize_point(hi);
    [
        qlo,
        Point2f64::new(qhi.x, qlo.y),
        qhi,
        Point2f64::new(qlo.x, qhi.y),
    ]
}

/// Require flat-top corner `q` (already quantized) to be claimed by EXACTLY
/// the region `ridx` — the rect flavour of [`check_end_claims`], with the
/// rect's stable messages: strictly inside or on the OUTER boundary passes
/// (a boundary-touching side is the mono-gable case); an internal shared
/// boundary, a different region, or no claimant rejects.
fn check_rect_corner_claims(storey: &Storey, q: Point2f64, ridx: usize) -> Result<(), String> {
    let claims = storey.roofs.claim_regions(q);
    if claims == [ridx] {
        return Ok(());
    }
    if claims.is_empty() {
        Err(RECT_CORNER_OUTSIDE_CEILING_MSG.to_string())
    } else if claims.contains(&ridx) {
        Err(RIDGE_END_ON_SHARED_BOUNDARY_MSG.to_string())
    } else {
        Err(RECT_CORNERS_SAME_REGION_MSG.to_string())
    }
}

/// Validate the two typed flat-top SIDE LENGTHS against the storey's
/// RESOLVED roofs — the rect flow's stage-1 check (per axis, the rect
/// analogue of [`validate_roof_ridge_length`], run progressively as each
/// length is typed): both must exceed a centimetre, and each must fit SOME
/// flat, un-apexed ceiling area's plan-bbox extent along its axis (the
/// OPTIMISTIC bound — the binding check is the finalize placement's,
/// [`validate_roof_rect_placement`]). `Err` is user-facing, messages stable.
pub fn validate_roof_rect_lengths(storey: &Storey, len_x: f32, len_y: f32) -> Result<(), String> {
    validate_roof_rect_length(storey, ParameterAxis::X, len_x)?;
    validate_roof_rect_length(storey, ParameterAxis::Y, len_y)
}

/// Validate ONE typed flat-top side length, on the open stage's axis only —
/// what the shell's LenX/LenY stages call, so a rejection always attaches to
/// the value the user just typed (a bad DRAWN length on the OTHER axis is
/// that axis's own stage's to reject, or finalize's when skipped): the side
/// must exceed a centimetre and fit SOME flat, un-apexed ceiling area's
/// plan-bbox extent along `axis`. `Err` is user-facing, messages stable.
pub fn validate_roof_rect_length(
    storey: &Storey,
    axis: ParameterAxis,
    len: f32,
) -> Result<(), String> {
    if !len.is_finite() || len <= MIN_RIDGE_LENGTH_M {
        return Err(RECT_SIDE_TOO_SHORT_MSG.to_string());
    }
    // The per-axis bbox bound, reused from the ridge: an extent along `axis`
    // is a ridge whose CROSS axis is `axis.other()` (the extent runs along
    // the other axis from the cross).
    validate_roof_ridge_length(storey, axis.other(), len)
}

/// Validate the flat top's PLACEMENT against the storey's RESOLVED roofs —
/// the rect flow's FINALIZE check, run at the last open location stage
/// (the rect analogue of [`validate_roof_ridge_extent`]):
///
/// - Both quantized sides exceed a centimetre.
/// - The rect's CENTRE sits STRICTLY inside one flat, un-apexed ceiling
///   region (the centre, never the corners, picks the region).
/// - The region's runs through the centre fit BOTH extents
///   ([`CeilingRegion::plan_span_through`] per axis), boundary touch
///   allowed within [`APEX_BOUNDARY_EPS_M`] per side.
/// - Each quantized corner is claimed by exactly that region
///   ([`check_rect_corner_claims`]).
/// - The region boundary's sector walk is clean (every edge and vertex
///   faces the rect side nearest it): [`rect_hip_switches`] == 4.
///
/// `Err` is user-facing, all messages stable; the over-length rejection
/// carries the permitted run ([`RECT_LENGTH_EXCEEDS_AREA_MSG`] prefix).
///
/// [`CeilingRegion::plan_span_through`]:
/// datatypes::ber::structural::CeilingRegion::plan_span_through
pub fn validate_roof_rect_placement(
    storey: &Storey,
    lo: Point2f64,
    hi: Point2f64,
) -> Result<(), String> {
    let corners = rect_corners(lo, hi);
    let (qlo, qhi) = (corners[0], corners[2]);
    if qhi.x - qlo.x <= MIN_RIDGE_LENGTH_M as f64 || qhi.y - qlo.y <= MIN_RIDGE_LENGTH_M as f64 {
        return Err(RECT_SIDE_TOO_SHORT_MSG.to_string());
    }
    let centre = Point2f64::new((qlo.x + qhi.x) / 2.0, (qlo.y + qhi.y) / 2.0);
    let ridx = region_idx_strictly_containing(storey, centre)
        .map_err(|_| RECT_OUTSIDE_CEILING_MSG.to_string())?;
    let groups = storey.roofs.apex_groups(&storey.roof_apexes);
    if groups.iter().any(|g| g.region_idxs.contains(&ridx)) {
        return Err(RECT_REGION_OCCUPIED_MSG.to_string());
    }
    let region = &storey.roofs.regions[ridx];
    if region.flat_z().is_none() {
        return Err(REGION_IRREGULAR_SLOPE_MSG.to_string());
    }
    // Both extents fit the region's run through the centre, boundary touch
    // allowed within the EPS band per side.
    for (dir, half) in [
        (Point2f64::new(1.0, 0.0), (qhi.x - qlo.x) / 2.0),
        (Point2f64::new(0.0, 1.0), (qhi.y - qlo.y) / 2.0),
    ] {
        let (t_min, t_max) = region
            .plan_span_through(centre, dir)
            .ok_or_else(|| RECT_OUTSIDE_CEILING_MSG.to_string())?;
        if t_min > -half + APEX_BOUNDARY_EPS_M || t_max < half - APEX_BOUNDARY_EPS_M {
            let span = t_max - t_min;
            return Err(format!(
                "{RECT_LENGTH_EXCEEDS_AREA_MSG} (the ceiling area runs {span:.2} m \
                 wall to wall there)."
            ));
        }
    }
    for q in corners {
        check_rect_corner_claims(storey, q, ridx)?;
    }
    let ring: Vec<Point2f64> = region
        .polygon
        .exterior
        .iter()
        .map(|v| Point2f64::new(v.x, v.y))
        .collect();
    if rect_hip_switches(&ring, qlo, qhi) != 4 {
        return Err(RECT_TOO_IRREGULAR_MSG.to_string());
    }
    Ok(())
}

/// Validate the flat-top RECT ridge at its HEIGHT stage (the rect analogue
/// of [`validate_roof_ridge`], which places FOUR apexes at once): `z_m`
/// finite and > 0, the placement re-checked (cheap, and `materialize` may
/// have shifted derived rings), and `|z_m − base|` ≥ 1 cm against the
/// claimed region's flat base. `Err` is user-facing, messages stable.
pub fn validate_roof_rect_ridge(
    storey: &Storey,
    lo: Point2f64,
    hi: Point2f64,
    z_m: f64,
) -> Result<(), String> {
    if !z_m.is_finite() || z_m <= 0.0 {
        return Err(format!(
            "height must be a positive number of metres (got {z_m})"
        ));
    }
    validate_roof_rect_placement(storey, lo, hi)?;
    let qlo = RoofApex::quantize_point(lo);
    let qhi = RoofApex::quantize_point(hi);
    let centre = Point2f64::new((qlo.x + qhi.x) / 2.0, (qlo.y + qhi.y) / 2.0);
    let ridx = region_idx_strictly_containing(storey, centre)
        .map_err(|_| RECT_OUTSIDE_CEILING_MSG.to_string())?;
    let base = storey.roofs.regions[ridx]
        .flat_z()
        .ok_or_else(|| REGION_IRREGULAR_SLOPE_MSG.to_string())?;
    if (z_m - base).abs() < MIN_APEX_DELTA_M {
        return Err(format!(
            "{z_m} m is within a centimetre of that ceiling area's height ({base} m) — \
             enter a height that actually slopes it."
        ));
    }
    Ok(())
}

/// The LO coordinate on `axis` that CENTRES the pending flat top within its
/// ceiling area — the 'Centre' button's computed suggestion for the rect's
/// PosX / PosY stages (mirrors [`centred_ridge_start`]): the region is
/// claimed by the rect's CENTRE (strictly inside), the span runs through it
/// along `axis`, and the centred lo is `span_mid − len/2`. `Err`
/// (user-facing, stable) when the centre sits in no region or the side
/// length exceeds the span beyond the boundary-touch band.
pub fn centred_rect_lo(
    storey: &Storey,
    axis: ParameterAxis,
    lo: Point2f64,
    hi: Point2f64,
) -> Result<f32, String> {
    let centre = Point2f64::new((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
    let ridx = region_idx_strictly_containing(storey, centre)
        .map_err(|_| RECT_OUTSIDE_CEILING_MSG.to_string())?;
    let (dir, len, centre_c) = match axis {
        ParameterAxis::X => (Point2f64::new(1.0, 0.0), hi.x - lo.x, centre.x),
        ParameterAxis::Y => (Point2f64::new(0.0, 1.0), hi.y - lo.y, centre.y),
    };
    let (t_min, t_max) = storey.roofs.regions[ridx]
        .plan_span_through(centre, dir)
        .ok_or_else(|| RECT_OUTSIDE_CEILING_MSG.to_string())?;
    let span = t_max - t_min;
    if len > span + 2.0 * APEX_BOUNDARY_EPS_M {
        return Err(format!(
            "{RECT_LENGTH_EXCEEDS_AREA_MSG} (the ceiling area runs {span:.2} m \
             wall to wall there)."
        ));
    }
    let span_mid = centre_c + (t_min + t_max) / 2.0;
    Ok((span_mid - len / 2.0) as f32)
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
    /// ceiling-step line test — STRICT: a ridge ON a parallel wall makes no
    /// roof), while the endpoints need only lie inside the footprint bbox
    /// EXPANDED by the end-snap tolerance
    /// ([`CEILING_LINE_SNAP_TOL_M`](crate::ceiling_step)) — a stroke ending
    /// on/just past a perpendicular wall must survive to the snap, which
    /// pulls it exactly onto the wall (a GABLE end). The extent runs along
    /// the OTHER axis from `axis`: a vertical line (`axis` = X) has its
    /// endpoints at `(coord, lo)`/`(coord, hi)`; a horizontal one (`axis` =
    /// Y) at `(lo, coord)`/`(hi, coord)`. `Err` is user-facing.
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
        let ((min_x, min_y), (max_x, max_y)) = self
            .outline
            .floor_plan
            .bounding_box()
            .ok_or_else(|| RIDGE_OFF_FOOTPRINT_MSG.to_string())?;
        // The extent's bbox interval on the OTHER axis from `axis`.
        let (ext_lo, ext_hi) = match axis {
            ParameterAxis::X => (min_y, max_y),
            ParameterAxis::Y => (min_x, max_x),
        };
        // A hand-drawn ridge meant to span wall-to-wall typically OVERSHOOTS the
        // outer walls by a few cm. Reject only a gross miss (beyond
        // `RIDGE_END_REACH_M` past the footprint); otherwise CLAMP each end onto
        // the footprint bbox so the ridge ends exactly at the walls (the
        // assessor's "let it snap from the top to bottom wall"). The actual
        // region containment is still checked at finalize (`validate_roof_ridge`).
        for t in [lo, hi] {
            if t < ext_lo - RIDGE_END_REACH_M || t > ext_hi + RIDGE_END_REACH_M {
                return Err(RIDGE_OFF_FOOTPRINT_MSG.to_string());
            }
        }
        let lo = lo.clamp(ext_lo, ext_hi);
        let hi = hi.clamp(ext_lo, ext_hi);
        Ok((axis, coord, (lo, hi)))
    }

    /// Fit a CLOSED `stroke` (world/metre space) to an axis-aligned
    /// flat-top RECTANGLE: [`manip_2d_plan::fit_axis_rect`] (the stroke's
    /// bbox, accepted only when every point hugs its boundary), then the
    /// footprint test — all four sides within the outline bbox expanded by
    /// the snap tolerance
    /// ([`CEILING_LINE_SNAP_TOL_M`](crate::ceiling_step)), so a side drawn
    /// on/just past a wall survives (the mono-gable case; finalize pins the
    /// binding placement). Returns the rect as `(lo, hi)` corners. `Err` is
    /// user-facing, messages stable. CLOSURE is the caller's test — the
    /// shell routes a closed roof-mode stroke here, an open one to
    /// [`fit_roof_ridge_line`](Self::fit_roof_ridge_line).
    pub fn fit_roof_rect(&self, stroke: &[Point2f32]) -> Result<(Point2f32, Point2f32), String> {
        let ((x0, y0), (x1, y1)) =
            manip_2d_plan::fit_axis_rect(stroke).ok_or_else(|| RECT_NOT_RECT_MSG.to_string())?;
        let ((min_x, min_y), (max_x, max_y)) = self
            .outline
            .floor_plan
            .bounding_box()
            .ok_or_else(|| RECT_OFF_FOOTPRINT_MSG.to_string())?;
        let tol = crate::ceiling_step::CEILING_LINE_SNAP_TOL_M;
        if x0 < min_x - tol || y0 < min_y - tol || x1 > max_x + tol || y1 > max_y + tol {
            return Err(RECT_OFF_FOOTPRINT_MSG.to_string());
        }
        Ok((Point2f32::new(x0, y0), Point2f32::new(x1, y1)))
    }
}

/// The outcome of deciding WHICH ceiling area a level ridge slopes — the shell
/// matches on this to either stamp the apexes directly or open its area-choice
/// prompt. (See [`resolve_ridge_slope_side`].)
pub enum RidgeSlopeSide {
    /// One area to slope: the interior point to stamp as BOTH apexes' side hint
    /// (`None` for a single-claimant outline ridge with no usable drawn side —
    /// the apexes are stored without a hint, as a plain mono-pitch).
    Resolved(Option<(f32, f32)>),
    /// The ridge sits on an INTERNAL boundary shared by ≥ 2 areas and the drawn
    /// side doesn't fall clearly inside one — the shell must ASK which slopes.
    /// Each option is `(side label, that area's interior hint point)`.
    Ambiguous(Vec<(String, (f32, f32))>),
}

/// A point ~30 cm inside `region` from the ridge midpoint `mid` (toward the
/// region's centroid) — a robust interior point to use as the apex side hint.
fn area_inside_point(region: &CeilingRegion, mid: Point2f64) -> (f32, f32) {
    let ext = &region.polygon.exterior;
    let n = ext.len().max(1) as f64;
    let cx = ext.iter().map(|v| v.x).sum::<f64>() / n;
    let cy = ext.iter().map(|v| v.y).sum::<f64>() / n;
    let (dx, dy) = (cx - mid.x, cy - mid.y);
    let d = (dx * dx + dy * dy).sqrt().max(1e-6);
    let step = d.min(0.3);
    (
        (mid.x + dx / d * step) as f32,
        (mid.y + dy / d * step) as f32,
    )
}

/// A human label for which side of the ridge an area sits on.
fn area_side_label(axis: ParameterAxis, coord_m: f32, inside: (f32, f32)) -> String {
    match axis {
        ParameterAxis::X => {
            if inside.0 < coord_m {
                "Left of the ridge"
            } else {
                "Right of the ridge"
            }
        }
        ParameterAxis::Y => {
            if inside.1 < coord_m {
                "Below the ridge"
            } else {
                "Above the ridge"
            }
        }
    }
    .to_string()
}

/// Decide which ceiling area a level ridge `a→b` (cross axis `axis`, coordinate
/// `coord_m`) slopes, given the DRAWN (pre-snap) cross coordinate `drawn_cross`
/// (the side the assessor drew over, `None` when the cross didn't snap).
///
/// The ridge's region is claimed by its midpoint ([`CeilingRegions::claim_regions`]):
/// - claimed by ONE area, or the drawn side falls clearly inside one of several
///   → [`RidgeSlopeSide::Resolved`] with that area's interior hint (or the drawn
///   side itself / `None`);
/// - claimed by ≥ 2 with no clear drawn side (a junction ridge on an internal
///   boundary) → [`RidgeSlopeSide::Ambiguous`] with one option per claiming area.
///
/// This keeps the claim/contain walks and the centroid/side math in the model
/// layer; the shell only matches on the result.
///
/// [`CeilingRegions::claim_regions`]:
/// datatypes::ber::structural::CeilingRegions::claim_regions
pub fn resolve_ridge_slope_side(
    storey: &Storey,
    a: Point2f64,
    b: Point2f64,
    drawn_cross: Option<f32>,
    axis: ParameterAxis,
    coord_m: f32,
) -> RidgeSlopeSide {
    let mid = Point2f64::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    // The drawn side as a plan point: the midpoint with its CROSS coordinate
    // replaced by where the line was actually drawn (before it snapped).
    let drawn_side: Option<(f32, f32)> = drawn_cross.map(|dc| match axis {
        ParameterAxis::X => (dc, mid.y as f32),
        ParameterAxis::Y => (mid.x as f32, dc),
    });
    let claims = storey.roofs.claim_regions(mid);
    let drawn_inside = drawn_side.and_then(|(hx, hy)| {
        let hp = Point2f64::new(hx as f64, hy as f64);
        let r = storey.roofs.region_strictly_containing(hp)?;
        let ri = storey.roofs.regions.iter().position(|x| x.id == r.id)?;
        claims.contains(&ri).then_some(ri)
    });
    if claims.len() >= 2 && drawn_inside.is_none() {
        let options = claims
            .iter()
            .map(|&ri| {
                let inside = area_inside_point(&storey.roofs.regions[ri], mid);
                (area_side_label(axis, coord_m, inside), inside)
            })
            .collect();
        return RidgeSlopeSide::Ambiguous(options);
    }
    // Unambiguous: the clearly-drawn area's interior, else the drawn side
    // (harmless on an outline wall — single claimant), else no hint.
    let chosen = drawn_inside
        .map(|ri| area_inside_point(&storey.roofs.regions[ri], mid))
        .or(drawn_side);
    RidgeSlopeSide::Resolved(chosen)
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
    fn apex_outside_rejected_but_single_region_edge_and_corner_allowed() {
        let storey = storey_with_flat_roof(2.4);
        // OUTSIDE every region → rejected.
        assert_eq!(
            validate_roof_apex(&storey, Point2f64::new(5.0, 2.0), 4.0).unwrap_err(),
            POINT_OUTSIDE_CEILING_MSG
        );
        assert_eq!(
            validate_roof_apex(&Storey::default(), Point2f64::new(2.0, 2.0), 4.0).unwrap_err(),
            POINT_OUTSIDE_CEILING_MSG
        );
        // On the SINGLE region's outline edge / corner → now ALLOWED (a
        // corner-raised slope). The fan handles the boundary apex.
        assert!(
            validate_roof_apex(&storey, Point2f64::new(0.0, 2.0), 4.0).is_ok(),
            "an apex on the outer wall is allowed"
        );
        assert!(
            validate_roof_apex(&storey, Point2f64::new(4.0, 4.0), 4.0).is_ok(),
            "an apex on a footprint corner is allowed"
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

    /// THE GABLE FLIP: an end ON the region's OUTER boundary is legal now
    /// (one claimant — the wall-to-wall ridge), an end claimed by NOTHING
    /// gets the end-outside message, a ridge whose MIDPOINT sits in no
    /// region (e.g. it runs along a boundary) gets the ridge-outside
    /// message, ends in two DIFFERENT regions keep the same-region message,
    /// and an end on an INTERNAL shared boundary gets the new
    /// shared-boundary rejection.
    #[test]
    fn ridge_end_claims_gable_legal_shared_boundary_rejected() {
        let storey = storey_with_flat_roof(2.4);
        // One end ON the outline boundary: a gable end — accepted.
        assert!(validate_roof_ridge(
            &storey,
            Point2f64::new(0.0, 2.0),
            Point2f64::new(3.0, 2.0),
            4.2
        )
        .is_ok());
        // BOTH ends on opposite walls: the full wall-to-wall gable ridge.
        assert!(validate_roof_ridge(
            &storey,
            Point2f64::new(0.0, 2.0),
            Point2f64::new(4.0, 2.0),
            4.2
        )
        .is_ok());
        // An end beyond the ceiling entirely (claimed by nothing).
        assert_eq!(
            validate_roof_ridge(
                &storey,
                Point2f64::new(1.0, 2.0),
                Point2f64::new(5.0, 2.0), // outside
                4.2
            )
            .unwrap_err(),
            RIDGE_END_OUTSIDE_CEILING_MSG
        );
        // A ridge ALONG the outline wall (a mono-pitch high edge) is now LEGAL —
        // it slopes the region down to the opposite wall.
        assert!(validate_roof_ridge(
            &storey,
            Point2f64::new(1.0, 0.0),
            Point2f64::new(3.0, 0.0),
            4.2
        )
        .is_ok());
        // Two flat regions side by side.
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
        // Midpoint in region 0, far end strictly in region 1: different
        // regions (the midpoint picks the region, never the ends).
        assert_eq!(
            validate_roof_ridge(
                &storey,
                Point2f64::new(1.0, 2.0),
                Point2f64::new(6.0, 2.0),
                4.2
            )
            .unwrap_err(),
            RIDGE_ENDS_SAME_REGION_MSG
        );
        // An end ON the INTERNAL boundary shared by the two regions is now
        // LEGAL — a mono-pitch ridge along the junction (the side hint fixes
        // which area slopes at resolve).
        assert!(validate_roof_ridge(
            &storey,
            Point2f64::new(1.0, 2.0),
            Point2f64::new(4.0, 2.0),
            4.2
        )
        .is_ok());
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
        // Along the top arm, spanning the slot: the midline (x = 5) crosses
        // the slot's edges → 4 switches. (The midpoint (5, 3.5) sits inside
        // the top arm — the midpoint-claims rework needs that; the old
        // diagonal test ridge had its midpoint over the slot, which now
        // rejects earlier as outside.)
        let err = validate_roof_ridge(
            &storey,
            Point2f64::new(1.0, 3.5),
            Point2f64::new(9.0, 3.5),
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
    /// cross coordinate outside the bbox (STRICT — a ridge on a parallel
    /// wall makes no roof) and an endpoint beyond the bbox + end-snap
    /// tolerance; an endpoint ON (or just past) the bbox edge SURVIVES the
    /// fit now — it must reach the end snap, which pulls it onto the wall (a
    /// gable end). A non-line blob keeps the straight-line message.
    #[test]
    fn fit_ridge_line_rejects_off_footprint_position_and_endpoints() {
        let doc = storey_10x6();
        // Position outside the footprint on X.
        let outside = vec![p(14.0, 1.0), p(14.0, 5.0)];
        assert_eq!(
            doc.fit_roof_ridge_line(&outside).unwrap_err(),
            RIDGE_OFF_FOOTPRINT_MSG
        );
        // Cross coordinate ON the bbox edge: still strictly rejected.
        let along_wall = vec![p(0.0, 1.0), p(0.0, 5.0)];
        assert_eq!(
            doc.fit_roof_ridge_line(&along_wall).unwrap_err(),
            RIDGE_OFF_FOOTPRINT_MSG
        );
        // Endpoint ON the bbox edge (y = 0): fits, extent kept.
        let touching = vec![p(4.0, 0.0), p(4.0, 3.0)];
        let (axis, _, (lo, hi)) = doc.fit_roof_ridge_line(&touching).expect("gable end fits");
        assert_eq!(axis, ParameterAxis::X);
        assert!(lo.abs() < 1e-6 && (hi - 3.0).abs() < 1e-6);
        // An end OVERSHOOTING the wall (y = −0.4, within the reach band) is
        // CLAMPED onto the wall (y = 0), not rejected — "let it snap to the wall".
        let overshoot = vec![p(4.0, -0.4), p(4.0, 3.0)];
        let (_, _, (lo, hi)) = doc
            .fit_roof_ridge_line(&overshoot)
            .expect("overshoot clamps");
        assert!(lo.abs() < 1e-6, "overshoot clamped to the wall: {lo}");
        assert!((hi - 3.0).abs() < 1e-6);
        // A GROSS miss (y = −0.8, beyond the reach band): still rejected.
        let too_far = vec![p(4.0, -0.8), p(4.0, 3.0)];
        assert_eq!(
            doc.fit_roof_ridge_line(&too_far).unwrap_err(),
            RIDGE_OFF_FOOTPRINT_MSG
        );
        // Not line-like at all → the fit message, not the footprint one.
        let blob = vec![p(2., 2.), p(4., 2.), p(4., 4.), p(2., 4.), p(2., 2.1)];
        assert!(doc
            .fit_roof_ridge_line(&blob)
            .unwrap_err()
            .contains("straight"));
    }

    /// validate_roof_ridge_length (the LENGTH stage's optimistic bound): a
    /// zero-ish or non-finite length gets the too-short message; a length
    /// within SOME flat un-apexed region's plan-bbox extent along the ridge
    /// axis passes — even when no actual run that long exists through any
    /// particular start (the binding check is finalize's); a length beyond
    /// EVERY area's bbox is rejected with the stable no-area prefix AND the
    /// permitted bound.
    #[test]
    fn validate_ridge_length_is_an_optimistic_bbox_bound() {
        let storey = storey_with_flat_roof(2.4);
        for len in [0.0, 0.01, -2.0, f32::NAN] {
            assert_eq!(
                validate_roof_ridge_length(&storey, ParameterAxis::X, len).unwrap_err(),
                RIDGE_LENGTH_TOO_SHORT_MSG,
                "len {len}"
            );
        }
        // The 4×4 region's bbox extent along either axis is 4: the full
        // wall-to-wall bound passes at THIS stage (the location stage may
        // still reject the actual run), anything beyond is rejected with
        // the bound in the message.
        assert!(validate_roof_ridge_length(&storey, ParameterAxis::X, 3.0).is_ok());
        assert!(validate_roof_ridge_length(&storey, ParameterAxis::X, 4.0).is_ok());
        let err = validate_roof_ridge_length(&storey, ParameterAxis::X, 4.5).unwrap_err();
        assert!(
            err.starts_with(RIDGE_LENGTH_NO_AREA_MSG) && err.contains("under 4.00 m"),
            "got: {err}"
        );

        // OPTIMISM, demonstrated: the notched C-shape's bbox runs 10 m
        // along X even though no contiguous run through most starts does —
        // a 9.5 m horizontal ridge passes Length and is only rejected at
        // the location stage.
        let storey = storey_with_flat_ring(notched_ring(), 2.4);
        assert!(validate_roof_ridge_length(&storey, ParameterAxis::Y, 9.5).is_ok());
        let err = validate_roof_ridge_length(&storey, ParameterAxis::Y, 10.5).unwrap_err();
        assert!(err.contains("under 10.00 m"), "got: {err}");

        // An APEXED region can't host a new ridge, so it contributes no
        // bound: a storey whose only region group is fanned rejects every
        // length (bound 0.00).
        let mut storey = storey_with_flat_roof(2.4);
        apply_apexes(&mut storey, vec![apex(2.0, 2.0, 4.2)]);
        let err = validate_roof_ridge_length(&storey, ParameterAxis::X, 1.0).unwrap_err();
        assert!(
            err.starts_with(RIDGE_LENGTH_NO_AREA_MSG) && err.contains("under 0.00 m"),
            "got: {err}"
        );
        // …and a storey with no roofs at all does too.
        let err =
            validate_roof_ridge_length(&Storey::default(), ParameterAxis::X, 1.0).unwrap_err();
        assert!(err.starts_with(RIDGE_LENGTH_NO_AREA_MSG), "got: {err}");

        // With a SECOND, longer flat region the bound is the MAX over the
        // candidates.
        let mut storey = storey_with_flat_roof(2.4);
        storey.roofs.regions.extend(
            CeilingRegions::default_for_footprint(
                vec![vec![
                    Point2f64::new(4.0, 0.0),
                    Point2f64::new(4.0, 9.0),
                    Point2f64::new(7.0, 9.0),
                    Point2f64::new(7.0, 0.0),
                ]],
                2.4,
                Default::default(),
            )
            .regions,
        );
        assert!(validate_roof_ridge_length(&storey, ParameterAxis::X, 8.5).is_ok());
        let err = validate_roof_ridge_length(&storey, ParameterAxis::X, 9.5).unwrap_err();
        assert!(err.contains("under 9.00 m"), "got: {err}");
    }

    /// validate_roof_ridge_extent (the FINALIZE placement check, run at the
    /// last open location stage): a sane extent passes; a zero-ish or
    /// non-finite length gets the too-short message (the SKIP path accepts
    /// drawn lengths without the Length stage's validation); BOUNDARY TOUCH
    /// IS LEGAL now — a start ON the wall and a wall-to-wall extent both
    /// pass (the gable flip; the old millimetre clearance is retired); the
    /// span check anchors at the MIDPOINT, so a ridge whose middle sits in
    /// no region gets the ridge-outside message; an extent overrunning the
    /// region is rejected with the stable prefix AND the full wall-to-wall
    /// run in the message.
    #[test]
    fn validate_ridge_extent_checks_length_against_the_region_span() {
        // One flat 4×4 region. A vertical ridge (cross axis X at x = 2) from
        // y = 1: the run to the far wall is 3 m.
        let storey = storey_with_flat_roof(2.4);
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, 2.0).is_ok());
        for len in [0.0, 0.01, -2.0, f32::NAN] {
            assert_eq!(
                validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, len).unwrap_err(),
                RIDGE_LENGTH_TOO_SHORT_MSG,
                "len {len}"
            );
        }
        // THE FLIP: touching the far wall (1 + 3 = 4), starting ON the near
        // wall (lo = 0), and the full wall-to-wall extent all PASS now —
        // the ends become gable apexes.
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, 3.0).is_ok());
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 0.0, 1.0).is_ok());
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 0.0, 4.0).is_ok());
        // Overrunning the region (while the midpoint stays inside): the
        // stable prefix + the full wall-to-wall run.
        for len in [3.5, 5.0] {
            let err =
                validate_roof_ridge_extent(&storey, ParameterAxis::X, 2.0, 1.0, len).unwrap_err();
            assert!(
                err.starts_with(RIDGE_LENGTH_EXCEEDS_AREA_MSG),
                "len {len}: {err}"
            );
            assert!(
                err.contains("4.00 m"),
                "len {len} must carry the full run: {err}"
            );
        }
        // A ridge along the left OUTLINE wall (x = 0) is now a valid mono-pitch
        // — its midpoint sits ON the outline boundary, claimed by the one area
        // it bounds, and the boundary-tolerant span fits it (it raises that
        // wall and slopes the area).
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 0.0, 1.0, 2.0).is_ok());
        // A ridge whose MIDPOINT leaves every ceiling area — far overrun, a
        // position outside every region, or a storey with no roofs at all: the
        // stable ridge-outside message.
        for (storey, coord, lo, len) in [
            (&storey, 2.0, 1.0, 9.0), // midpoint (2, 5.5): overrun
            (&storey, 9.0, 1.0, 1.0), // outside
            (&Storey::default(), 2.0, 1.0, 1.0),
        ] {
            assert_eq!(
                validate_roof_ridge_extent(storey, ParameterAxis::X, coord, lo, len).unwrap_err(),
                RIDGE_OUTSIDE_CEILING_MSG,
                "coord {coord} lo {lo} len {len}"
            );
        }
        // Horizontal ridge (cross axis Y at y = 2) from x = 1 in the notched
        // C-shape: the run ends at the slot's near edge (x = 3), NOT the
        // ring's 10 m extent — the limit is the contiguous run through the
        // midpoint, not the bbox. Touching the slot edge (len 2) is legal;
        // past it is rejected with the run (3 m: wall to slot edge).
        let storey = storey_with_flat_ring(notched_ring(), 2.4);
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::Y, 2.0, 1.0, 2.0).is_ok());
        let err = validate_roof_ridge_extent(&storey, ParameterAxis::Y, 2.0, 1.0, 2.5).unwrap_err();
        assert!(
            err.starts_with(RIDGE_LENGTH_EXCEEDS_AREA_MSG) && err.contains("3.00 m"),
            "got: {err}"
        );
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::Y, 2.0, 1.0, 1.9).is_ok());
    }

    /// THE HUB-SWITCH FENCE AT FINALIZE (the accepted-early/rejected-late
    /// regression): a ridge along the notched C's TOP ARM (cross axis Y at
    /// y = 3.5, extent x ∈ [1, 9]) passes every other finalize predicate —
    /// midpoint (5, 3.5) strictly inside, flat, un-apexed, the wall-to-wall
    /// run at y = 3.5 spans the full 10 m, both ends claimed — but the
    /// boundary walk between its ends wraps past the slot (4 hub switches),
    /// which the Height stage would reject with no typed height able to fix
    /// it. Finalize must reject it FIRST, with the same stable message. A
    /// ridge staying within the left arm still passes.
    #[test]
    fn validate_ridge_extent_rejects_a_foldover_the_height_stage_would() {
        let storey = storey_with_flat_ring(notched_ring(), 2.4);
        assert_eq!(
            validate_roof_ridge_extent(&storey, ParameterAxis::Y, 3.5, 1.0, 8.0).unwrap_err(),
            RIDGE_TOO_IRREGULAR_MSG
        );
        // The same geometry is exactly what the Height stage checks: its
        // rejection must agree (finalize is never LOOSER than Height).
        assert_eq!(
            validate_roof_ridge(
                &storey,
                Point2f64::new(1.0, 3.5),
                Point2f64::new(9.0, 3.5),
                4.0
            )
            .unwrap_err(),
            RIDGE_TOO_IRREGULAR_MSG
        );
        // A vertical ridge confined to the LEFT arm keeps a sane walk
        // (2 switches) and still finalizes.
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::X, 1.5, 0.5, 3.0).is_ok());
    }

    /// The CENTRE suggestions (the 'Centre' button's computed values): the
    /// cross centre is the midpoint of the region's run through the ridge
    /// midpoint along the cross axis, the start centre is `span_mid − len/2`
    /// along the extent axis — the contiguous RUN through the midpoint, not
    /// the bbox (the notched C centres within the arm the midpoint sits in).
    /// A full-span length centres to the exact wall-to-wall gable; a
    /// midpoint in no region gets the stable ridge-outside message; a length
    /// exceeding the run gets the stable exceeds-area prefix with the
    /// permitted run.
    #[test]
    fn centred_ridge_cross_and_start_centre_within_the_run() {
        let storey = storey_with_flat_roof(2.4);
        // Vertical ridge (cross axis X) drawn at x = 1, extent y ∈ [1, 3]:
        // the 4×4 region centres the cross at x = 2 and the start at y = 1.
        let cross = centred_ridge_cross(&storey, ParameterAxis::X, 1.0, 1.0, 2.0).unwrap();
        assert!((cross - 2.0).abs() < 1e-6);
        let start = centred_ridge_start(&storey, ParameterAxis::X, 1.0, 1.0, 2.0).unwrap();
        assert!((start - 1.0).abs() < 1e-6);
        // Horizontal ridge (cross axis Y) at y = 3, extent x ∈ [0.5, 1.5].
        let cross = centred_ridge_cross(&storey, ParameterAxis::Y, 3.0, 0.5, 1.0).unwrap();
        assert!((cross - 2.0).abs() < 1e-6);
        let start = centred_ridge_start(&storey, ParameterAxis::Y, 3.0, 0.5, 1.0).unwrap();
        assert!((start - 1.5).abs() < 1e-6);
        // A FULL-SPAN length centres to the wall-to-wall gable (lo = 0).
        let start = centred_ridge_start(&storey, ParameterAxis::X, 2.0, 0.5, 4.0).unwrap();
        assert!(start.abs() < 1e-6);
        // Midpoint in no region → the stable ridge-outside message, both fns.
        assert_eq!(
            centred_ridge_cross(&storey, ParameterAxis::X, 9.0, 1.0, 2.0).unwrap_err(),
            RIDGE_OUTSIDE_CEILING_MSG
        );
        assert_eq!(
            centred_ridge_start(&storey, ParameterAxis::X, 9.0, 1.0, 2.0).unwrap_err(),
            RIDGE_OUTSIDE_CEILING_MSG
        );
        // A length exceeding the run (midpoint still inside): the stable
        // prefix + the permitted run.
        let err = centred_ridge_start(&storey, ParameterAxis::X, 2.0, 0.5, 5.0).unwrap_err();
        assert!(
            err.starts_with(RIDGE_LENGTH_EXCEEDS_AREA_MSG) && err.contains("4.00 m"),
            "got: {err}"
        );

        // The notched C-shape: a horizontal ridge in the LEFT arm (midpoint
        // (1.5, 2)) centres within the arm's 3 m run to the slot edge — not
        // the ring's 10 m bbox; the cross centres within the arm's height.
        let storey = storey_with_flat_ring(notched_ring(), 2.4);
        let start = centred_ridge_start(&storey, ParameterAxis::Y, 2.0, 1.0, 1.0).unwrap();
        assert!(
            (start - 1.0).abs() < 1e-6,
            "run 0..3, centred lo (got {start})"
        );
        let cross = centred_ridge_cross(&storey, ParameterAxis::Y, 2.0, 1.0, 1.0).unwrap();
        assert!((cross - 2.0).abs() < 1e-6, "y-run 0..4 (got {cross})");
    }

    // ── the rect (flat-topped hip) validators ───────────────────────────────

    /// `fit_roof_rect`: a closed rectangular stroke fits to its bbox; a side
    /// drawn on/just past a wall survives within the snap tolerance (the
    /// mono-gable case); a rect beyond the tolerance and a non-rect stroke
    /// are rejected with their stable messages.
    #[test]
    fn fit_roof_rect_accepts_rects_within_the_expanded_footprint() {
        let doc = storey_10x6();
        let stroke = vec![
            p(2.0, 1.05),
            p(4.0, 0.95),
            p(6.0, 1.0),
            p(6.05, 2.5),
            p(5.95, 4.0),
            p(4.0, 4.05),
            p(2.0, 3.95),
            p(1.95, 2.5),
        ];
        let (lo, hi) = doc.fit_roof_rect(&stroke).expect("rect fits");
        assert!((lo.x - 1.95).abs() < 1e-6 && (hi.y - 4.05).abs() < 1e-6);
        // A side ON the wall (y = 0) and just past it (−0.1, inside the
        // 0.2 m tolerance) both survive to the finalize placement.
        let touching = vec![
            p(2.0, 0.0),
            p(6.0, 0.0),
            p(6.0, 2.0),
            p(2.0, 2.0),
            p(2.0, 0.05),
        ];
        assert!(doc.fit_roof_rect(&touching).is_ok());
        let over = vec![p(2.0, -0.1), p(6.0, -0.1), p(6.0, 2.0), p(2.0, 2.0)];
        assert!(doc.fit_roof_rect(&over).is_ok());
        // Beyond the tolerance: the stable footprint message.
        let out = vec![p(2.0, -0.5), p(6.0, -0.5), p(6.0, 2.0), p(2.0, 2.0)];
        assert_eq!(doc.fit_roof_rect(&out).unwrap_err(), RECT_OFF_FOOTPRINT_MSG);
        // A diagonal (non-rect) stroke: the stable not-a-rectangle message.
        let diag = vec![p(1.0, 1.0), p(2.0, 2.0), p(3.0, 3.0), p(4.0, 4.0)];
        assert_eq!(doc.fit_roof_rect(&diag).unwrap_err(), RECT_NOT_RECT_MSG);
    }

    /// `validate_roof_rect_lengths` is the per-axis optimistic bbox bound:
    /// zero-ish sides get the stable side-too-short message; a pair fitting
    /// some area passes; a side beyond every area's bbox extent on its axis
    /// is rejected with the ridge's stable no-area prefix.
    #[test]
    fn validate_rect_lengths_is_a_per_axis_optimistic_bound() {
        let storey = storey_with_flat_roof(2.4); // one flat 4×4 region
        for (lx, ly) in [(0.0, 2.0), (2.0, 0.01), (f32::NAN, 2.0), (-1.0, 2.0)] {
            assert_eq!(
                validate_roof_rect_lengths(&storey, lx, ly).unwrap_err(),
                RECT_SIDE_TOO_SHORT_MSG,
                "({lx}, {ly})"
            );
        }
        assert!(validate_roof_rect_lengths(&storey, 3.0, 2.0).is_ok());
        assert!(validate_roof_rect_lengths(&storey, 4.0, 4.0).is_ok());
        let err = validate_roof_rect_lengths(&storey, 4.5, 2.0).unwrap_err();
        assert!(
            err.starts_with(RIDGE_LENGTH_NO_AREA_MSG) && err.contains("under 4.00 m"),
            "got: {err}"
        );
        let err = validate_roof_rect_lengths(&storey, 2.0, 4.5).unwrap_err();
        assert!(err.starts_with(RIDGE_LENGTH_NO_AREA_MSG), "got: {err}");
    }

    /// The rect FINALIZE check: an interior rect passes; BOUNDARY TOUCH is
    /// legal (a side, or the whole wall-to-wall rect); a centre in no
    /// region, an overrunning extent (with the permitted run), an occupied
    /// region, and corners on an internal shared boundary are rejected with
    /// their stable messages.
    #[test]
    fn validate_rect_placement_centre_claims_spans_corners_and_fence() {
        let storey = storey_with_flat_roof(2.4); // 4×4
        let rect =
            |x0: f64, y0: f64, x1: f64, y1: f64| (Point2f64::new(x0, y0), Point2f64::new(x1, y1));
        let (lo, hi) = rect(1.0, 1.0, 3.0, 3.0);
        assert!(validate_roof_rect_placement(&storey, lo, hi).is_ok());
        // Boundary touch: one side ON the wall (mono-gable) and the full
        // wall-to-wall rect both pass.
        let (lo, hi) = rect(1.0, 0.0, 3.0, 2.0);
        assert!(validate_roof_rect_placement(&storey, lo, hi).is_ok());
        let (lo, hi) = rect(0.0, 0.0, 4.0, 4.0);
        assert!(validate_roof_rect_placement(&storey, lo, hi).is_ok());
        // Degenerate sides.
        let (lo, hi) = rect(1.0, 1.0, 1.005, 3.0);
        assert_eq!(
            validate_roof_rect_placement(&storey, lo, hi).unwrap_err(),
            RECT_SIDE_TOO_SHORT_MSG
        );
        // Centre outside every region.
        let (lo, hi) = rect(5.0, 1.0, 7.0, 3.0);
        assert_eq!(
            validate_roof_rect_placement(&storey, lo, hi).unwrap_err(),
            RECT_OUTSIDE_CEILING_MSG
        );
        // Overrunning the region while the centre stays inside: the stable
        // prefix + the permitted run.
        let (lo, hi) = rect(-0.5, 1.0, 4.5, 3.0);
        let err = validate_roof_rect_placement(&storey, lo, hi).unwrap_err();
        assert!(
            err.starts_with(RECT_LENGTH_EXCEEDS_AREA_MSG) && err.contains("4.00 m"),
            "got: {err}"
        );
        // An occupied region rejects the rect.
        let mut occupied = storey_with_flat_roof(2.4);
        apply_apexes(&mut occupied, vec![apex(2.0, 2.0, 4.2)]);
        let (lo, hi) = rect(1.5, 0.2, 2.5, 0.7); // centre inside a fan face
        assert_eq!(
            validate_roof_rect_placement(&occupied, lo, hi).unwrap_err(),
            RECT_REGION_OCCUPIED_MSG
        );
        // A corner ON an internal shared boundary: two flat regions side by
        // side, rect in region 0 reaching the shared line x = 4.
        let mut two = storey_with_flat_roof(2.4);
        two.roofs.regions.extend(
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
        let (lo, hi) = rect(2.0, 1.0, 4.0, 3.0);
        assert_eq!(
            validate_roof_rect_placement(&two, lo, hi).unwrap_err(),
            RIDGE_END_ON_SHARED_BOUNDARY_MSG
        );
    }

    /// The sector-walk fence (risk #4): a NON-convex region passes as long
    /// as every boundary edge faces the rect side nearest it (a clean
    /// 4-switch walk — the L-shaped region with the rect in its corner); a
    /// rect tucked into the notched C's arm leaves the far edges facing the
    /// rect's RIGHT side (the run loft would fold across the rect — 2
    /// switches), as does one along its bottom arm (3 switches), and a
    /// needle notch dipping toward a rect corner (a vertex whose own sector
    /// matches neither adjacent edge — the vertex-break arm of the count):
    /// all rejected with the stable too-irregular message.
    #[test]
    fn validate_rect_placement_fences_unclean_sector_walks() {
        // L-shaped region, rect in its inner corner: every edge faces its
        // nearest rect side (the two-edge runs included) — passes.
        let l_ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(6.0, 0.0),
            Point2f64::new(6.0, 4.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(4.0, 6.0),
            Point2f64::new(0.0, 6.0),
        ];
        let storey = storey_with_flat_ring(l_ring, 2.4);
        assert!(validate_roof_rect_placement(
            &storey,
            Point2f64::new(1.0, 1.0),
            Point2f64::new(3.0, 3.0)
        )
        .is_ok());
        // Tucked in the C's left arm (x ≤ 3): the bottom and top edges'
        // midpoints sit far right of the rect and face its RIGHT side —
        // the loft would stretch their run across the rect.
        let storey = storey_with_flat_ring(notched_ring(), 2.4);
        let err = validate_roof_rect_placement(
            &storey,
            Point2f64::new(0.5, 0.5),
            Point2f64::new(2.5, 3.5),
        )
        .unwrap_err();
        assert_eq!(err, RECT_TOO_IRREGULAR_MSG);
        // Along the bottom arm under the slot: the left edge faces the top
        // sector's run from the wrong side (3 switches).
        let err = validate_roof_rect_placement(
            &storey,
            Point2f64::new(0.5, 0.2),
            Point2f64::new(9.0, 0.8),
        )
        .unwrap_err();
        assert_eq!(err, RECT_TOO_IRREGULAR_MSG);
        // A needle notch dipping from the top edge toward the rect's
        // top-right corner: its tip vertex faces the RIGHT side while both
        // its edges face the TOP — it would hip to a corner neither run
        // uses (the vertex-break arm) — the fence.
        let needle = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(9.5, 2.05),
            Point2f64::new(9.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let storey = storey_with_flat_ring(needle, 2.4);
        let err = validate_roof_rect_placement(
            &storey,
            Point2f64::new(1.0, 1.0),
            Point2f64::new(9.0, 2.0),
        )
        .unwrap_err();
        assert_eq!(err, RECT_TOO_IRREGULAR_MSG);
    }

    /// `validate_roof_rect_ridge` (the height stage): a sane height passes,
    /// non-finite / non-positive and near-base heights are rejected, and the
    /// placement is re-checked.
    #[test]
    fn validate_rect_ridge_checks_height_and_replays_placement() {
        let storey = storey_with_flat_roof(2.4);
        let (lo, hi) = (Point2f64::new(1.0, 1.0), Point2f64::new(3.0, 3.0));
        assert!(validate_roof_rect_ridge(&storey, lo, hi, 4.0).is_ok());
        for z in [f64::NAN, 0.0, -1.0] {
            let err = validate_roof_rect_ridge(&storey, lo, hi, z).unwrap_err();
            assert!(err.contains("positive"), "z = {z} got: {err}");
        }
        let err = validate_roof_rect_ridge(&storey, lo, hi, 2.405).unwrap_err();
        assert!(err.contains("centimetre"), "got: {err}");
        // Placement re-checked at the height stage.
        let off = (Point2f64::new(5.0, 1.0), Point2f64::new(7.0, 3.0));
        assert_eq!(
            validate_roof_rect_ridge(&storey, off.0, off.1, 4.0).unwrap_err(),
            RECT_OUTSIDE_CEILING_MSG
        );
    }

    /// `centred_rect_lo` mirrors `centred_ridge_start` per axis: the rect
    /// centres within the run through its centre; a full-span side centres
    /// to the boundary; a centre in no region and an over-long side reject
    /// with their stable messages.
    #[test]
    fn centred_rect_lo_centres_each_axis_within_the_run() {
        let storey = storey_with_flat_roof(2.4); // 4×4
        let (lo, hi) = (Point2f64::new(0.5, 1.0), Point2f64::new(2.5, 2.0));
        let cx = centred_rect_lo(&storey, ParameterAxis::X, lo, hi).unwrap();
        assert!(
            (cx - 1.0).abs() < 1e-6,
            "2 m side centres at x = 1 (got {cx})"
        );
        let cy = centred_rect_lo(&storey, ParameterAxis::Y, lo, hi).unwrap();
        assert!(
            (cy - 1.5).abs() < 1e-6,
            "1 m side centres at y = 1.5 (got {cy})"
        );
        // A full-span side centres to the exact wall-to-wall extent.
        let (lo, hi) = (Point2f64::new(0.5, 1.0), Point2f64::new(4.5, 2.0));
        let cx = centred_rect_lo(&storey, ParameterAxis::X, lo, hi).unwrap();
        assert!(cx.abs() < 1e-6, "full span → lo = 0 (got {cx})");
        // Centre outside.
        let (lo, hi) = (Point2f64::new(5.0, 1.0), Point2f64::new(7.0, 2.0));
        assert_eq!(
            centred_rect_lo(&storey, ParameterAxis::X, lo, hi).unwrap_err(),
            RECT_OUTSIDE_CEILING_MSG
        );
        // A side longer than the run: the stable prefix + the permitted run.
        let (lo, hi) = (Point2f64::new(0.0, 1.0), Point2f64::new(5.0, 2.0));
        let err = centred_rect_lo(&storey, ParameterAxis::X, lo, hi).unwrap_err();
        assert!(
            err.starts_with(RECT_LENGTH_EXCEEDS_AREA_MSG) && err.contains("4.00 m"),
            "got: {err}"
        );
    }

    /// The finalize check's END-CLAIM arm: an extent ending ON an INTERNAL
    /// shared boundary is rejected at finalize (not left for the Height
    /// stage), with the stable shared-boundary message.
    #[test]
    fn validate_ridge_extent_rejects_an_end_on_an_internal_shared_boundary() {
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
        // A ridge ending exactly ON the shared line x = 4 (a mono-pitch ridge
        // along the junction) is now LEGAL — the side hint fixes which area
        // slopes at resolve.
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::Y, 2.0, 1.0, 3.0).is_ok());
        // Ending ON the OUTER boundary (x = 0, one claimant) stays legal.
        assert!(validate_roof_ridge_extent(&storey, ParameterAxis::Y, 2.0, 0.0, 3.0).is_ok());
    }

    /// A ridge running ALONG the INTERNAL boundary x = 4 shared by two ceiling
    /// areas (the user's mono-pitch-on-a-junction case): both the finalize
    /// extent check and the height check accept it — its midpoint sits on the
    /// shared edge, claimed by both areas, and the boundary-tolerant span fits
    /// it in EITHER. (Which one slopes is the controller's ChooseArea prompt;
    /// validation only needs SOME claiming area to accept.)
    #[test]
    fn ridge_along_an_internal_shared_boundary_is_accepted_by_both_validators() {
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
        // The ridge runs UP the shared line x = 4 (cross axis X at x = 4), from
        // y = 1 to y = 3 — its midpoint (4, 2) is ON the junction.
        assert!(
            validate_roof_ridge_extent(&storey, ParameterAxis::X, 4.0, 1.0, 2.0).is_ok(),
            "finalize accepts a ridge along the shared boundary"
        );
        let a = Point2f64::new(4.0, 1.0);
        let b = Point2f64::new(4.0, 3.0);
        assert!(
            validate_roof_ridge(&storey, a, b, 3.0).is_ok(),
            "the height stage also accepts it"
        );
    }
}
