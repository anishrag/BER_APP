//! Roof-point (apex) and ceiling-ridge validation: declared high points shape
//! the ceiling region containing them — one apex fans it, two apexes in one
//! group form a ridge.
//!
//! The apexes are DURABLE input (`Storey::ceiling_apexes`, applied by the
//! resolve projection AFTER the ceiling-step areas — composition order is
//! law: default ceiling → steps → apexes). A ridge is NOT a separate model
//! object: it is simply two apexes grouped with the regions they shape
//! ([`CeilingRegions::apex_groups`], THE grouping authority). What lives here
//! are the assign-time validations the shell runs before `Ber::add_ceiling_apex`:
//!
//! - [`StoreyDoc::validate_roof_point`] — the cheap tap-time check against
//!   the OUTLINE (no `Ber` needed): the point must be strictly inside the
//!   footprint bbox on both axes, exactly the per-axis test the ceiling-step
//!   line uses.
//! - [`StoreyDoc::fit_roof_ridge_line`] — the stroke-time fit of a drawn
//!   ridge line (axis-aligned, endpoints = the stroke's own extent), again
//!   against the OUTLINE only.
//! - [`validate_roof_apex`] — the materialized check against the storey's
//!   RESOLVED ceilings, GROUP-AWARE: the region strictly containing the point is
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
//! ber_model::ber::structural::CeilingRegions::apex_groups

use crate::StoreyDoc;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::{
    gable_ring_roles, rect_hip_switches, ridge_hub_switches, strip_collinear, CeilingApex,
    CeilingApexGroup, CeilingRegion, Storey, APEX_BOUNDARY_EPS_M, UNFAN_MATCH_EPS_M,
};
use domain::points::{ring_area, to_f32, Point2f32, Point2f64};
use manip_plan::ParameterAxis;

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

/// User-facing rejection for a dormer footprint that isn't a usable closed
/// patch (fewer than three corners, or a collapsed / self-touching ring with no
/// plan area). Kept stable — tests match on it.
const DORMER_DEGENERATE_MSG: &str =
    "The dormer footprint isn't a closed area — draw a closed patch with three or more \
     corners and real area.";

/// User-facing rejection for a dormer whose footprint is NOT fully covered by
/// the roof — part of it pokes past the union of the storey's ceiling areas.
/// A footprint may SPAN several ceiling areas (a dormer running from a sloped
/// area into an adjacent flat protrusion); only poking OUTSIDE the roof is
/// rejected. Kept stable — tests match on it.
const DORMER_OUTSIDE_CEILING_MSG: &str =
    "The dormer must sit within the roof — keep the whole patch over ceiling area(s).";

/// User-facing rejection for a dormer top that dips BELOW the roof it carves
/// into somewhere over its footprint (an inverted riser). Flush with the roof
/// along the uphill edge is allowed; anywhere lower is not. Kept stable.
const DORMER_TOP_BELOW_ROOF_MSG: &str =
    "The dormer top must clear the roof — raise the height / move it downslope.";

/// A dormer footprint whose plan area is at or below this (m²) is treated as
/// degenerate — a sliver too small to carve a real patch.
const DORMER_MIN_AREA_M2: f64 = 1e-4;

/// Plan-area slack (m²) allowed between the footprint and the roof coverage
/// before a dormer is judged to poke outside — boolean-op noise / boundary
/// grazes, not a real gap. Also the per-region graze threshold below which a
/// region ∩ footprint is treated as no overlap.
const DORMER_COVERAGE_EPS_M2: f64 = 1e-4;

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
        .ceilings
        .region_strictly_containing(p)
        .ok_or_else(|| POINT_OUTSIDE_CEILING_MSG.to_string())?;
    Ok(storey
        .ceilings
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
    match storey.ceilings.claim_regions(p).as_slice() {
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
    group: &CeilingApexGroup,
    existing: Point2f64,
    p: Point2f64,
) -> Result<(), String> {
    let ring = group
        .merged_rings
        .iter()
        .find(|ring| {
            let pts: Vec<Point2f32> = ring.iter().copied().map(to_f32).collect();
            manip_plan::ring_contains_point(&pts, p.x as f32, p.y as f32)
        })
        .or_else(|| group.merged_rings.first());
    match ring {
        Some(ring) if ridge_hub_switches(ring, existing, p) > 2 => {
            Err(RIDGE_TOO_IRREGULAR_MSG.to_string())
        }
        Some(_) => Ok(()),
        None => {
            log::warn!(
                "ceiling apex group has no merged ring — skipping the ridge hub-switch check"
            );
            Ok(())
        }
    }
}

/// Validate a ceiling apex against the storey's RESOLVED ceilings (the assign-time
/// check, run on the materialized `Storey` before `Ber::add_ceiling_apex`),
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
///   ([`apex_groups`] against `storey.ceiling_apexes`):
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
/// ber_model::ber::structural::CeilingRegions::region_strictly_containing
/// [`apex_groups`]: ber_model::ber::structural::CeilingRegions::apex_groups
pub fn validate_roof_apex(storey: &Storey, p: Point2f64, z_m: f64) -> Result<(), String> {
    if !z_m.is_finite() || z_m <= 0.0 {
        return Err(format!(
            "height must be a positive number of metres (got {z_m})"
        ));
    }
    let ri = region_idx_uniquely_claiming(storey, p)?;
    let groups = storey.ceiling_apex_groups();
    let base = match groups.iter().find(|g| g.region_idxs.contains(&ri)) {
        None => storey.ceilings.regions[ri]
            .flat_z()
            .ok_or_else(|| REGION_IRREGULAR_SLOPE_MSG.to_string())?,
        Some(g) if g.apex_idxs.len() == 1 => {
            let existing = storey.ceiling_apexes[g.apex_idxs[0]].point;
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
/// ber_model::ber::structural::CeilingRegions::claim_regions
fn check_end_claims(storey: &Storey, q: Point2f64, ridx: usize) -> Result<(), String> {
    let claims = storey.ceilings.claim_regions(q);
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
/// via `Ber::add_ceiling_apex` ×2) against the storey's RESOLVED ceilings:
///
/// - `z_m` finite and > 0 (one typed height — a level ridge; a sloping ridge
///   is entered via the two-point flow instead).
/// - The endpoints must stay DISTINCT after the f32 quantization
///   `add_ceiling_apex` applies (a zero-length ridge is a fan, not a ridge).
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
    // Work on the endpoints exactly as add_ceiling_apex will store them.
    let qa = CeilingApex::quantize_point(a);
    let qb = CeilingApex::quantize_point(b);
    if qa.x == qb.x && qa.y == qb.y {
        return Err(RIDGE_ENDS_COINCIDE_MSG.to_string());
    }
    let mid = Point2f64::new((qa.x + qb.x) / 2.0, (qa.y + qb.y) / 2.0);
    // Try every area claiming the midpoint (mirrors `validate_roof_ridge_extent`):
    // a junction ridge on a shared boundary is valid if it slopes EITHER
    // neighbour; a strict-interior midpoint has one claimant. Most specific
    // rejection wins.
    let claims = storey.ceilings.claim_regions(mid);
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
    // No single area takes it — a ridge across a CHAIN of flat areas (two
    // raised strips split by a ceiling step) is still valid: the solve fuses
    // the chain and shapes ONE roof over the union (`fuse_ridge_spanned_regions`).
    match ridge_fits_flat_chain(storey, qa, qb, Some(z_m)) {
        Ok(()) => Ok(()),
        // A PLAUSIBLE chain (all members flat and un-apexed) failed on a
        // value the user can fix — its message is the specific one.
        Err(ChainReject::Specific(msg)) => Err(msg),
        Err(ChainReject::NotAChain) => Err(last_err),
    }
}

/// Whether the level ridge `qa→qb` (quantized ends) is acceptable as a
/// CROSS-STEP CHAIN ridge — the assign-time mirror of the solve's
/// `fuse_ridge_spanned_regions` (ber_model), which fuses the flat regions a
/// ridge spans into one and shapes the union; keep the two in step:
///
/// - Walked at 64 interior samples, every sample must be claimed by ≥ 1
///   region ([`claim_regions`] — boundary band included) and every region so
///   met must be FLAT and UN-APEXED; ≥ 2 distinct regions must be met (one
///   region is the normal single-area path, not a chain).
/// - Both ends must be claimed by a chain member (inside or on its edge).
/// - The chain must union into ONE simple ring
///   ([`CeilingRegions::plan_union_ring`]) and the ridge must not fold over
///   it ([`ridge_hub_switches`] ≤ 2).
/// - With `z_m` given (the Height stage): `|z_m − base|` ≥ 1 cm against
///   EVERY member's base — a ridge at one strip's own height would leave
///   that strip unsloped, a silent near-noop.
///
/// The verdict distinguishes "this was never a chain" ([`ChainReject::NotAChain`]
/// — a sample outside every region, a single region, a sloped or apexed
/// member) from a PLAUSIBLE chain failing on something the user can fix
/// ([`ChainReject::Specific`] — the height, an end, the foldover): callers
/// keep their single-area rejection for the former and surface the chain's
/// own message for the latter.
///
/// [`claim_regions`]: ber_model::ber::structural::CeilingRegions::claim_regions
/// [`CeilingRegions::plan_union_ring`]:
/// ber_model::ber::structural::CeilingRegions::plan_union_ring
fn ridge_fits_flat_chain(
    storey: &Storey,
    qa: Point2f64,
    qb: Point2f64,
    z_m: Option<f64>,
) -> Result<(), ChainReject> {
    const SAMPLES: usize = 64;
    let mut members: Vec<usize> = Vec::new();
    for s in 0..SAMPLES {
        let t = (s as f64 + 0.5) / SAMPLES as f64;
        let p = Point2f64::new(qa.x + (qb.x - qa.x) * t, qa.y + (qb.y - qa.y) * t);
        let claims = storey.ceilings.claim_regions(p);
        if claims.is_empty() {
            return Err(ChainReject::NotAChain);
        }
        for c in claims {
            if !members.contains(&c) {
                members.push(c);
            }
        }
    }
    members.sort_unstable();
    if members.len() < 2 {
        return Err(ChainReject::NotAChain);
    }
    let groups = storey.ceiling_apex_groups();
    for &m in &members {
        if groups.iter().any(|g| g.region_idxs.contains(&m)) {
            return Err(ChainReject::NotAChain);
        }
        let Some(base) = storey.ceilings.regions[m].flat_z() else {
            return Err(ChainReject::NotAChain);
        };
        if let Some(z) = z_m {
            if (z - base).abs() < MIN_APEX_DELTA_M {
                return Err(ChainReject::Specific(format!(
                    "{z} m is within a centimetre of a ceiling area's height ({base} m) — \
                     enter a height that actually slopes it."
                )));
            }
        }
    }
    for q in [qa, qb] {
        let claims = storey.ceilings.claim_regions(q);
        if claims.is_empty() {
            return Err(ChainReject::Specific(
                RIDGE_END_OUTSIDE_CEILING_MSG.to_string(),
            ));
        }
        if !claims.iter().any(|c| members.contains(c)) {
            return Err(ChainReject::Specific(
                RIDGE_ENDS_SAME_REGION_MSG.to_string(),
            ));
        }
    }
    let Some(ring) = storey.ceilings.plan_union_ring(&members) else {
        return Err(ChainReject::Specific(RIDGE_TOO_IRREGULAR_MSG.to_string()));
    };
    if ridge_hub_switches(&ring, qa, qb) > 2 {
        return Err(ChainReject::Specific(RIDGE_TOO_IRREGULAR_MSG.to_string()));
    }
    Ok(())
}

/// Why [`ridge_fits_flat_chain`] said no — see its verdict doc.
enum ChainReject {
    /// The geometry was never a cross-area chain (outside samples, one
    /// region, sloped/apexed members): the caller's single-area rejection
    /// stands.
    NotAChain,
    /// A plausible chain failed on a fixable value — surface THIS message.
    Specific(String),
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
    let groups = storey.ceiling_apex_groups();
    if groups.iter().any(|g| g.region_idxs.contains(&ra)) {
        return Err(RIDGE_REGION_OCCUPIED_MSG.to_string());
    }
    let region = &storey.ceilings.regions[ra];
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

/// Validate a typed ceiling-ridge LENGTH against the storey's RESOLVED ceilings —
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
    // A ridge may legally run across a CHAIN of flat areas (two raised
    // strips split by a ceiling step — the chain checks / the solve's
    // fuse), so its bound spans a connected component, not one region.
    flat_area_length_bound(storey, axis, len_m, true)
}

/// The optimistic "could ANY area take it" length bound shared by the ridge
/// and rect Length stages: only flat, un-apexed regions can host new roof
/// input (the same predicates the finalize checks apply), and the typed
/// length must fit the plan-bbox extent along the EXTENT axis
/// (`axis.other()` per the axis convention) of some region — or, with
/// `chains` (the RIDGE, which may span a flat chain the solve fuses), of
/// some plan-ADJACENT COMPONENT of those regions
/// ([`CeilingRegions::plan_adjacent_components`]). The rect keeps the
/// per-region bound: a flat top must sit within ONE area
/// ([`validate_roof_rect_placement`]).
///
/// [`CeilingRegions::plan_adjacent_components`]:
/// ber_model::ber::structural::CeilingRegions::plan_adjacent_components
fn flat_area_length_bound(
    storey: &Storey,
    axis: ParameterAxis,
    len_m: f32,
    chains: bool,
) -> Result<(), String> {
    let groups = storey.ceiling_apex_groups();
    let idxs: Vec<usize> = storey
        .ceilings
        .regions
        .iter()
        .enumerate()
        .filter(|(i, r)| r.flat_z().is_some() && !groups.iter().any(|g| g.region_idxs.contains(i)))
        .map(|(i, _)| i)
        .collect();
    let comps: Vec<Vec<usize>> = if chains {
        storey.ceilings.plan_adjacent_components(&idxs)
    } else {
        idxs.into_iter().map(|i| vec![i]).collect()
    };
    let max_extent = comps
        .iter()
        .map(|comp| {
            let mut min = f64::INFINITY;
            let mut max = f64::NEG_INFINITY;
            for &i in comp {
                for v in &storey.ceilings.regions[i].polygon.exterior {
                    let c = match axis {
                        ParameterAxis::X => v.y,
                        ParameterAxis::Y => v.x,
                    };
                    min = min.min(c);
                    max = max.max(c);
                }
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
/// ceilings — THE FINALIZE CHECK, run at the moment the geometry becomes fully
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
/// ber_model::ber::structural::CeilingRegions::region_strictly_containing
/// [`CeilingRegion::plan_span_through`]:
/// ber_model::ber::structural::CeilingRegion::plan_span_through
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
    let claims = storey.ceilings.claim_regions(mid);
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
    // No single area takes the extent — a ridge across a CHAIN of flat areas
    // (two raised strips split by a ceiling step) is still valid: the solve
    // fuses the chain and shapes ONE roof over the union
    // (`fuse_ridge_spanned_regions`). Same quantization as the Height stage.
    let (a, b) = match axis {
        ParameterAxis::X => (
            Point2f64::new(coord_m as f64, lo as f64),
            Point2f64::new(coord_m as f64, (lo + len_m) as f64),
        ),
        ParameterAxis::Y => (
            Point2f64::new(lo as f64, coord_m as f64),
            Point2f64::new((lo + len_m) as f64, coord_m as f64),
        ),
    };
    match ridge_fits_flat_chain(
        storey,
        CeilingApex::quantize_point(a),
        CeilingApex::quantize_point(b),
        None,
    ) {
        Ok(()) => Ok(()),
        Err(ChainReject::Specific(msg)) => Err(msg),
        Err(ChainReject::NotAChain) => Err(last_err),
    }
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
    let groups = storey.ceiling_apex_groups();
    if groups.iter().any(|g| g.region_idxs.contains(&ridx)) {
        return Err(RIDGE_REGION_OCCUPIED_MSG.to_string());
    }
    let region = &storey.ceilings.regions[ridx];
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
        check_end_claims(storey, CeilingApex::quantize_point(q), ridx)?;
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
        CeilingApex::quantize_point(qa),
        CeilingApex::quantize_point(qb),
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
    let region = storey.ceilings.region_strictly_containing(mid)?;
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
    storey.ceilings.regions[ridx]
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
/// ber_model::ber::structural::CeilingRegion::plan_span_through
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

// ── Gable-end snap + tap-to-unfan fan markers ───────────────────────────────

/// How close (m) a ridge END must be to its ceiling area's boundary — short
/// of it OR past it — to auto-snap ONTO the boundary at finalize (the
/// automatic GABLE-END rule): the same tolerance the ridge drawing's
/// elective end snap uses ([`crate::ceiling_step::CEILING_LINE_SNAP_TOL_M`]).
const RIDGE_GABLE_SNAP_TOL_M: f64 = crate::ceiling_step::CEILING_LINE_SNAP_TOL_M as f64;

/// The automatic GABLE-END rule, applied by the shell at the ridge FINALIZE
/// step (once the geometry is fully determined, before
/// [`validate_roof_ridge_extent`]): each extent end that lies ON, short of,
/// or past its ceiling area's boundary within [`RIDGE_GABLE_SNAP_TOL_M`] is
/// pulled EXACTLY onto the boundary — the wall-to-wall gable the assessor
/// meant. Returns the (possibly) adjusted `(lo, len_m)`.
///
/// Why this makes the gable: with a ridge endpoint exactly ON a boundary
/// edge, the resolve's hub triangles along that edge are VERTICAL (zero plan
/// area) and drop as slivers, and the hip transition triangles merge into
/// the two slope planes — so the end edge becomes a vertical gable end (its
/// wall rises to the ridge via the roof-void wall split) and its vertices
/// are excluded from the fan. A classic gable over a rectangle thus solves
/// to exactly two sloped planes reaching only the 4 corners, with zero taps.
/// Ends further inside than the tolerance keep today's hip/fan behaviour
/// (the conservatory case).
///
/// The span is the region's contiguous run through the ridge MIDPOINT along
/// the extent axis (`axis.other()` — the [`validate_roof_ridge_extent`]
/// convention), so on an L-shape the end snaps to ITS area's wall, not the
/// bbox. A midpoint in no region, or a degenerate span, returns the input
/// unchanged (the finalize validation right after surfaces the real error).
pub fn snap_ridge_extent_to_gables(
    storey: &Storey,
    axis: ParameterAxis,
    coord_m: f32,
    lo: f32,
    len_m: f32,
) -> (f32, f32) {
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
    let Some(region) = storey.ceilings.region_strictly_containing(mid) else {
        return (lo, len_m);
    };
    let Some((t_min, t_max)) = region.plan_span_through(mid, dir) else {
        return (lo, len_m);
    };
    let (wall_lo, wall_hi) = (mid_along + t_min, mid_along + t_max);
    let hi = lo as f64 + len_m as f64;
    let new_lo = if (lo as f64 - wall_lo).abs() <= RIDGE_GABLE_SNAP_TOL_M {
        wall_lo
    } else {
        lo as f64
    };
    let new_hi = if (hi - wall_hi).abs() <= RIDGE_GABLE_SNAP_TOL_M {
        wall_hi
    } else {
        hi
    };
    if new_lo != lo as f64 || new_hi != hi {
        log::debug!(
            "ridge gable-end snap: extent [{:.3}, {:.3}] → [{new_lo:.3}, {new_hi:.3}] \
             (area run [{wall_lo:.3}, {wall_hi:.3}])",
            lo,
            hi
        );
    }
    (new_lo as f32, (new_hi - new_lo) as f32)
}

/// One tappable fan-vertex marker of the ceiling-shape editing context: a
/// ceiling-area corner of a shaped roof. See [`ceiling_fan_markers`] for
/// which corners appear and what tapping each does.
#[derive(Debug, Clone, PartialEq)]
pub struct CeilingFanMarker {
    /// The vertex's plan position (world metres) — also the identity the
    /// toggle mutations store (f32-quantized by the `Ber` setters).
    pub pos: Point2f64,
    /// `true` while the vertex keeps its own z (a hip/fan target, or a gable
    /// anchor); `false` when it is embedded / unfanned.
    pub fanned: bool,
    /// `true` when the vertex is a GABLE side's CURRENT pitch anchor ("this
    /// corner sets the pitch" — render distinctly; tapping it clears the
    /// side's override, back to auto-nearest). Implies `fanned`. Always
    /// `false` on hip/fan groups.
    pub anchor: bool,
}

/// What a tap on a fan marker should DO — resolved by
/// [`ceiling_fan_tap_action`] against the storey's current solve, so the
/// shell stays a thin router:
#[derive(Debug, Clone, PartialEq)]
pub enum CeilingFanTapAction {
    /// Hip/fan group: toggle the vertex in the stored unfanned set
    /// (`Ber::toggle_ceiling_unfanned`) — the pre-gable behaviour, kept
    /// byte-identical.
    ToggleUnfanned(Point2f64),
    /// Gable side: make the vertex the side's pitch anchor — store it in
    /// `ceiling_plane_anchors`, dropping the side's previous override(s) in
    /// `drop` (`Ber::update_ceiling_plane_anchors(drop, Some(anchor))`).
    SetPlaneAnchor {
        anchor: Point2f64,
        drop: Vec<Point2f64>,
    },
    /// Gable side: the tapped vertex IS the current anchor and holds an
    /// override — drop it (back to auto-nearest;
    /// `Ber::update_ceiling_plane_anchors(drop, None)`).
    ClearPlaneAnchor { drop: Vec<Point2f64> },
}

/// The fan-vertex MARKERS for the storey's resolved roofs: every non-apex,
/// non-collinear vertex of each fan/ridge group's merged ring, in the role
/// the solve gives it:
///
/// - a TRUE-GABLE ridge (both endpoints on the merged ring's boundary):
///   [`gable_ring_roles`] — each side's pitch ANCHOR (auto-nearest or the
///   stored `ceiling_plane_anchors` override) is `anchor` (+`fanned`);
///   every other vertex is embedded on the side plane (`fanned = false`).
///   Tap semantics are anchor selection ([`ceiling_fan_tap_action`]) — the
///   stored unfanned set does not apply here;
/// - any other fan/ridge: `fanned` unless in the stored unfanned set
///   (embedding semantics in `ceiling_geom::lift_unfanned_fan` /
///   `lift_unfanned_hip_ridge`); never `anchor`.
///
/// A group contributes markers only when they are actionable: for a gable,
/// some vertex genuinely DEVIATES (its embedded z differs from its base —
/// a plain rectangle gable shows nothing) or an override is stored (it must
/// stay clearable); for hips/fans, MORE THAN 4 vertices fanned or any
/// vertex unfanned (the pre-gable rule, unchanged). Only 1-apex (fan) and
/// 2-apex (ridge) groups participate — the pavilion loft has no per-vertex
/// fan. Collinear ring vertices are stripped ([`strip_collinear`]): they
/// contribute no fan plane. Empty when the storey has no shaped ceiling.
pub fn ceiling_fan_markers(storey: &Storey) -> Vec<CeilingFanMarker> {
    if storey.ceiling_apexes.is_empty() {
        return Vec::new();
    }
    let unfanned = storey.ceiling_unfanned_points();
    let anchors = storey.ceiling_plane_anchor_points();
    let near = |a: &Point2f64, bx: f64, by: f64| {
        (a.x - bx).abs() <= UNFAN_MATCH_EPS_M && (a.y - by).abs() <= UNFAN_MATCH_EPS_M
    };
    let mut out = Vec::new();
    for g in storey.ceiling_apex_groups() {
        if !matches!(g.apex_idxs.len(), 1 | 2) {
            continue; // pavilion / degenerate group — no per-vertex fan
        }
        let ridge = (g.apex_idxs.len() == 2).then(|| {
            (
                storey.ceiling_apexes[g.apex_idxs[0]].point,
                storey.ceiling_apexes[g.apex_idxs[1]].point,
            )
        });
        let mut markers = Vec::new();
        let mut show = false;
        for ring in &g.merged_rings {
            let ring = strip_collinear(ring);
            // The gable roles when this ring IS a true gable —
            // index-parallel to `ring`.
            let roles = ridge.and_then(|(pa, pb)| gable_ring_roles(&ring, pa, pb, &anchors));
            if let Some(r) = &roles {
                show |= r.overridden || r.deviates.iter().any(|d| *d);
            }
            for (i, v) in ring.iter().enumerate() {
                if g.apex_idxs
                    .iter()
                    .any(|&ia| near(&storey.ceiling_apexes[ia].point, v.x, v.y))
                {
                    continue; // a gable-end hub on the boundary — not a fan target
                }
                let (fanned, anchor) = match &roles {
                    Some(r) => (r.anchor[i], r.anchor[i]),
                    None => (!unfanned.iter().any(|u| near(u, v.x, v.y)), false),
                };
                markers.push(CeilingFanMarker {
                    pos: *v,
                    fanned,
                    anchor,
                });
            }
        }
        // Hip/fan visibility: the pre-gable rule.
        if ridge.is_none() || markers.iter().all(|m| !m.anchor) {
            let fanned_count = markers.iter().filter(|m| m.fanned).count();
            show |= fanned_count > 4 || markers.iter().any(|m| !m.fanned);
        }
        if show {
            out.extend(markers);
        }
    }
    out
}

/// The marker within `tol_m` (plan metres) of `p` nearest to it — the
/// document-side guard behind the shell's fan-marker tap: only a CURRENT
/// marker's canonical position may be acted on (a stray tap does nothing).
pub fn ceiling_fan_marker_at(storey: &Storey, p: Point2f64, tol_m: f64) -> Option<Point2f64> {
    ceiling_fan_markers(storey)
        .into_iter()
        .map(|m| {
            let d2 = (m.pos.x - p.x).powi(2) + (m.pos.y - p.y).powi(2);
            (m, d2)
        })
        .filter(|(_, d2)| *d2 <= tol_m * tol_m)
        .min_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(m, _)| m.pos)
}

/// Resolve a fan-marker tap at `p` (within `tol_m`) into the model mutation
/// it means — the document-side brain behind the shell's marker tap:
///
/// - on a HIP/FAN group's marker: [`CeilingFanTapAction::ToggleUnfanned`]
///   (the pre-gable behaviour, byte-identical);
/// - on a GABLE side's EMBEDDED vertex (eligible — it owns an eave edge):
///   [`CeilingFanTapAction::SetPlaneAnchor`] — that corner now sets the
///   side's pitch; the side's previous override(s) ride in `drop` so one
///   `Ber::update_ceiling_plane_anchors` call swaps them;
/// - on a GABLE side's CURRENT anchor: [`CeilingFanTapAction::ClearPlaneAnchor`]
///   when an override is stored for that side (back to auto-nearest);
///   `None` when the anchor is already the auto choice (nothing to clear);
/// - on an INELIGIBLE gable vertex (a gable-wall extra — it can't anchor a
///   pitch), or a tap that hits no marker: `None` (logged by the caller).
pub fn ceiling_fan_tap_action(
    storey: &Storey,
    p: Point2f64,
    tol_m: f64,
) -> Option<CeilingFanTapAction> {
    let anchor_overrides = storey.ceiling_plane_anchor_points();
    let near = |a: &Point2f64, b: &Point2f64| {
        (a.x - b.x).abs() <= UNFAN_MATCH_EPS_M && (a.y - b.y).abs() <= UNFAN_MATCH_EPS_M
    };
    for g in storey.ceiling_apex_groups() {
        if !matches!(g.apex_idxs.len(), 1 | 2) {
            continue;
        }
        let ridge = (g.apex_idxs.len() == 2).then(|| {
            (
                storey.ceiling_apexes[g.apex_idxs[0]].point,
                storey.ceiling_apexes[g.apex_idxs[1]].point,
            )
        });
        for ring in &g.merged_rings {
            let ring = strip_collinear(ring);
            // The tapped ring vertex, if any (apex corners are not markers).
            let Some(vi) = ring
                .iter()
                .enumerate()
                .filter(|(_, v)| {
                    (v.x - p.x).powi(2) + (v.y - p.y).powi(2) <= tol_m * tol_m
                        && !g
                            .apex_idxs
                            .iter()
                            .any(|&ia| near(&storey.ceiling_apexes[ia].point, v))
                })
                .min_by(|(_, u), (_, w)| {
                    let du = (u.x - p.x).powi(2) + (u.y - p.y).powi(2);
                    let dw = (w.x - p.x).powi(2) + (w.y - p.y).powi(2);
                    du.partial_cmp(&dw).unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
            else {
                continue;
            };
            let v = ring[vi];
            let Some((pa, pb)) = ridge
                .filter(|(pa, pb)| gable_ring_roles(&ring, *pa, *pb, &anchor_overrides).is_some())
            else {
                // Hip / fan group — the pre-gable toggle.
                return Some(CeilingFanTapAction::ToggleUnfanned(v));
            };
            let roles = gable_ring_roles(&ring, pa, pb, &anchor_overrides).expect("checked above");
            if !roles.eligible[vi] {
                log::debug!(
                    "fan-marker tap at ({:.3}, {:.3}): a gable-wall vertex can't anchor a pitch",
                    v.x,
                    v.y
                );
                return None;
            }
            // The tapped vertex's SIDE, and that side's stored override(s):
            // every override point matching a same-side ring vertex.
            let side_of = |q: &Point2f64| {
                let (dx, dy) = (pb.x - pa.x, pb.y - pa.y);
                dx * (q.y - pa.y) - dy * (q.x - pa.x) > 0.0
            };
            let drop: Vec<Point2f64> = anchor_overrides
                .iter()
                .filter(|o| {
                    ring.iter()
                        .any(|rv| near(o, rv) && side_of(rv) == side_of(&v))
                })
                .copied()
                .collect();
            if roles.anchor[vi] {
                if drop.is_empty() {
                    log::debug!(
                        "fan-marker tap at ({:.3}, {:.3}): already the auto anchor — nothing to clear",
                        v.x,
                        v.y
                    );
                    return None;
                }
                return Some(CeilingFanTapAction::ClearPlaneAnchor { drop });
            }
            return Some(CeilingFanTapAction::SetPlaneAnchor { anchor: v, drop });
        }
    }
    None
}

// ── The closed-stroke RECT ridge (flat-topped hip / pavilion) ───────────────

/// The four corners of the rect `(lo, hi)`, QUANTIZED exactly as
/// `Ber::add_ceiling_apex` will store them, in the canonical CCW order
/// c00 → c10 → c11 → c01.
fn rect_corners(lo: Point2f64, hi: Point2f64) -> [Point2f64; 4] {
    let qlo = CeilingApex::quantize_point(lo);
    let qhi = CeilingApex::quantize_point(hi);
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
    let claims = storey.ceilings.claim_regions(q);
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
/// RESOLVED ceilings — the rect flow's stage-1 check (per axis, the rect
/// analogue of [`validate_roof_ridge_length`], run progressively as each
/// length is typed): both must exceed a centimetre, and each must fit SOME
/// flat, un-apexed ceiling area's plan-bbox extent along its axis (the
/// OPTIMISTIC bound — the binding check is the finalize placement's,
/// [`validate_roof_rect_placement`]). `Err` is user-facing, messages stable.
///
/// Test-only batch sibling of [`validate_roof_rect_length`]: the shell drives
/// the per-axis singular (one rejection attaches to the typed value); this
/// both-at-once form is kept for the unit tests.
#[cfg(test)]
pub(crate) fn validate_roof_rect_lengths(
    storey: &Storey,
    len_x: f32,
    len_y: f32,
) -> Result<(), String> {
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
    // The per-axis bbox bound, shared with the ridge: an extent along `axis`
    // is a ridge whose CROSS axis is `axis.other()` (the extent runs along
    // the other axis from the cross). PER REGION, no chain components — a
    // flat top must sit within one ceiling area.
    flat_area_length_bound(storey, axis.other(), len, false)
}

/// Validate the flat top's PLACEMENT against the storey's RESOLVED ceilings —
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
/// ber_model::ber::structural::CeilingRegion::plan_span_through
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
    let groups = storey.ceiling_apex_groups();
    if groups.iter().any(|g| g.region_idxs.contains(&ridx)) {
        return Err(RECT_REGION_OCCUPIED_MSG.to_string());
    }
    let region = &storey.ceilings.regions[ridx];
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
    let qlo = CeilingApex::quantize_point(lo);
    let qhi = CeilingApex::quantize_point(hi);
    let centre = Point2f64::new((qlo.x + qhi.x) / 2.0, (qlo.y + qhi.y) / 2.0);
    let ridx = region_idx_strictly_containing(storey, centre)
        .map_err(|_| RECT_OUTSIDE_CEILING_MSG.to_string())?;
    let base = storey.ceilings.regions[ridx]
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
    let (t_min, t_max) = storey.ceilings.regions[ridx]
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

// ── The island DORMER (a flat-topped patch raised into a sloped attic) ───────

/// Validate a flat-topped island DORMER against the storey's RESOLVED ceilings
/// (the assign-time check, run on the materialized `Storey` before
/// [`Ber::add_dormer`] — the caller has already `materialize()`d, so this reads
/// the resolved `storey.ceilings`, exactly like [`validate_roof_apex`] and the
/// rect validators). A patch, flat top, that may SPAN one or more ceiling areas.
///
/// - `top_z_m` finite and > 0 (ABSOLUTE height above the storey's floor datum,
///   the [`Dormer::top_z_m`] convention — not a delta).
/// - `footprint` is a non-degenerate closed ring: three or more corners and a
///   plan area over [`DORMER_MIN_AREA_M2`] ([`ring_area`]).
/// - The footprint may SPAN SEVERAL ceiling areas — which, UNLIKE
///   [`validate_roof_apex`], MAY BE SLOPED (the whole point of a dormer is to
///   carve into an already-pitched roof; flatness is NOT required). The
///   assessor builds the roof slopes on the un-protruded ceiling, then draws a
///   dormer that runs from that sloped area INTO an adjacent FLAT protrusion,
///   so ONE footprint legitimately covers a sloped region + a flat one. Rather
///   than requiring a single region, the footprint must be COVERED by the UNION
///   of the storey's ceiling regions: sum each region's 2D intersection with
///   the footprint; if the covered area falls short of the footprint's own area
///   (beyond [`DORMER_COVERAGE_EPS_M2`]) part pokes outside the roof and is
///   rejected ([`DORMER_OUTSIDE_CEILING_MSG`]); nothing intersecting is also
///   rejected.
/// - `top_z_m` must clear the roof over the WHOLE footprint. Sampling only the
///   footprint's own vertices is INSUFFICIENT: when a ridge/apex passes through
///   the footprint interior it is higher than any footprint corner. The
///   coverage and the roof's maximum height are computed together by
///   [`CeilingRegions::footprint_coverage_and_roof_max`], which evaluates EACH
///   region's OWN plane over its OWN intersection piece — so a DISCONTINUOUS
///   roof step at a region seam is captured from both sides (the higher side
///   wins, order-independently). The top must be `≥` that maximum (within
///   [`APEX_BOUNDARY_EPS_M`]). Flush (`==`) with the highest point passes; a top
///   below the roof anywhere is an inverted riser and is rejected
///   ([`DORMER_TOP_BELOW_ROOF_MSG`]).
///
/// `Err` is user-facing, all messages stable.
///
/// [`Ber::add_dormer`]: ber_model::ber::structural::ber_struct::Ber::add_dormer
/// [`Dormer::top_z_m`]: ber_model::ber::structural::Dormer::top_z_m
/// [`CeilingRegions::footprint_coverage_and_roof_max`]:
/// ber_model::ber::structural::CeilingRegions::footprint_coverage_and_roof_max
pub fn validate_dormer(
    storey: &Storey,
    footprint: &[Point2f64],
    top_z_m: f64,
) -> Result<(), String> {
    if !top_z_m.is_finite() || top_z_m <= 0.0 {
        return Err(format!(
            "height must be a positive number of metres (got {top_z_m})"
        ));
    }
    // 1. A non-degenerate closed ring with real plan area.
    if footprint.len() < 3 || ring_area(footprint) <= DORMER_MIN_AREA_M2 {
        return Err(DORMER_DEGENERATE_MSG.to_string());
    }
    let fp_area = ring_area(footprint).abs();

    // 2. Containment + roof height in one pass. `roof_max` reads each region's
    //    OWN plane over its OWN intersection piece, so a discontinuous step at a
    //    region seam is captured from both sides (no first-region-wins ambiguity
    //    of `top_z_at`, whose sample points would otherwise sit ON the seam).
    let (covered, roof_max) = storey.ceilings.footprint_coverage_and_roof_max(footprint);
    // The footprint must be COVERED by the UNION of the roof's ceiling areas
    // (spanning several is fine; poking outside — or over a hole — is not).
    if covered < fp_area - DORMER_COVERAGE_EPS_M2 {
        return Err(DORMER_OUTSIDE_CEILING_MSG.to_string());
    }
    // Nothing intersecting → no roof to carve into.
    let Some(roof_max) = roof_max else {
        return Err(DORMER_OUTSIDE_CEILING_MSG.to_string());
    };

    // 3. Clearance — the top must clear the roof over the WHOLE footprint. Flush
    //    (≥, within the boundary epsilon) with the highest point passes; below
    //    the roof anywhere is an inverted riser.
    if top_z_m < roof_max - APEX_BOUNDARY_EPS_M {
        return Err(DORMER_TOP_BELOW_ROOF_MSG.to_string());
    }
    Ok(())
}

impl StoreyDoc {
    /// The cheap tap-time validation of a ceiling-point position against the
    /// OUTLINE (no `Ber` needed — the resolved-ceilings check is
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
    /// RIDGE line: [`manip_plan::fit_axis_segment`] — axis + coordinate +
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
        let (axis, coord, (lo, hi)) = manip_plan::fit_axis_segment(stroke).ok_or_else(|| {
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
    /// flat-top RECTANGLE: [`manip_plan::fit_axis_rect`] (the stroke's
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
            manip_plan::fit_axis_rect(stroke).ok_or_else(|| RECT_NOT_RECT_MSG.to_string())?;
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
    /// One area to slope with no real alternative (a SINGLE-claimant ridge):
    /// the interior point to stamp as BOTH apexes' side hint (`None` for an
    /// outline ridge with no usable drawn side — the apexes are stored without
    /// a hint, as a plain mono-pitch).
    Resolved(Option<(f32, f32)>),
    /// A JUNCTION ridge (≥ 2 claiming areas) resolved WITHOUT asking — exactly
    /// one neighbour was slope-able. The payload names both sides so the shell
    /// can SURFACE the silent decision (a notice + a log line) instead of
    /// letting it pass invisibly.
    AutoResolved(RidgeAutoChoice),
    /// The ridge sits on an INTERNAL boundary shared by ≥ 2 areas and the drawn
    /// side doesn't fall clearly inside one — the shell must ASK which slopes.
    /// Each option is `(side label, that area's interior hint point)`.
    Ambiguous(Vec<(String, (f32, f32))>),
}

/// The context of a silently auto-resolved junction ridge (see
/// [`RidgeSlopeSide::AutoResolved`]): who slopes and who was passed over, with
/// the side labels the ChooseArea prompt would have shown plus the region ids
/// and flat heights for the shell's log line.
#[derive(Debug, Clone, PartialEq)]
pub struct RidgeAutoChoice {
    /// The sloped area's interior point — the hint to stamp on both apexes.
    pub hint: (f32, f32),
    /// The sloped area's side label (e.g. "Left of the ridge") — the same
    /// vocabulary as the ChooseArea options.
    pub sloped_label: String,
    /// The sloped region's id and flat height (`None` = already non-flat).
    pub sloped_region: (ItemId, Option<f64>),
    /// The passed-over claimants: `(side label, region id, flat height)`.
    pub passed_over: Vec<(String, ItemId, Option<f64>)>,
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
            // Plan y grows DOWNWARD (screen convention), so a SMALLER y sits
            // higher on the plan ("above" the ridge) and a larger y sits lower
            // ("below"). (These were previously swapped.)
            if inside.1 < coord_m {
                "Above the ridge"
            } else {
                "Below the ridge"
            }
        }
    }
    .to_string()
}

/// Decide which ceiling area a level ridge `a→b` (cross axis `axis`, coordinate
/// `coord_m`) at height `z_m` slopes, given the DRAWN (pre-snap) cross coordinate
/// `drawn_cross` (the side the assessor drew over, `None` when the cross didn't
/// snap).
///
/// The ridge's region is claimed by its midpoint ([`CeilingRegions::claim_regions`]):
/// - claimed by ONE area (a strict-interior GABLE ridge) → [`RidgeSlopeSide::Resolved`]
///   with the drawn side's hint (or `None` — both halves slope);
/// - claimed by ≥ 2 (a junction ridge on an internal boundary): the ridge slopes
///   ONE neighbour and steps the other. It can only slope a neighbour whose flat
///   height DIFFERS from `z_m` — a ridge at height == a region's own base is a
///   no-op for it (ridging to its own base collapses back to flat). So:
///   - exactly ONE such slope-able neighbour → resolve to it deterministically
///     (a mono-pitch drawn on the joint of an already-raised area slopes the
///     LOWER neighbour, regardless of which side the line was drawn on) —
///     returned as [`RidgeSlopeSide::AutoResolved`], naming both sides so the
///     shell can surface the silent decision;
///   - ≥ 2 slope-able neighbours → [`RidgeSlopeSide::Ambiguous`], ALWAYS asking
///     which side rises to the ridge. The drawn side is NOT used to auto-resolve
///     here: "the side the line was drawn over" does not reliably match the
///     assessor's intent ("which side rises to the ridge"), so a genuine two-way
///     choice is always put to the user.
///
/// This keeps the claim/contain walks and the centroid/side math in the model
/// layer; the shell only matches on the result.
///
/// [`CeilingRegions::claim_regions`]:
/// ber_model::ber::structural::CeilingRegions::claim_regions
pub fn resolve_ridge_slope_side(
    storey: &Storey,
    a: Point2f64,
    b: Point2f64,
    drawn_cross: Option<f32>,
    axis: ParameterAxis,
    coord_m: f32,
    z_m: f64,
) -> RidgeSlopeSide {
    let mid = Point2f64::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    // The drawn side as a plan point: the midpoint with its CROSS coordinate
    // replaced by where the line was actually drawn (before it snapped).
    let drawn_side: Option<(f32, f32)> = drawn_cross.map(|dc| match axis {
        ParameterAxis::X => (dc, mid.y as f32),
        ParameterAxis::Y => (mid.x as f32, dc),
    });
    let claims = storey.ceilings.claim_regions(mid);
    let drawn_inside = |candidates: &[usize]| -> Option<usize> {
        let (hx, hy) = drawn_side?;
        let hp = Point2f64::new(hx as f64, hy as f64);
        let r = storey.ceilings.region_strictly_containing(hp)?;
        let ri = storey.ceilings.regions.iter().position(|x| x.id == r.id)?;
        candidates.contains(&ri).then_some(ri)
    };

    // Junction ridge (≥ 2 claimants): choose among the neighbours the ridge can
    // ACTUALLY slope — those whose flat height differs from z_m. A ridge at
    // height == a region's own base does nothing to it, so it must be meant for
    // the neighbour. This slopes the lower area deterministically instead of
    // coin-flipping on the drawn side onto the no-op (already-raised) side.
    if claims.len() >= 2 {
        let sloped: Vec<usize> = claims
            .iter()
            .copied()
            .filter(|&ri| match storey.ceilings.regions[ri].flat_z() {
                Some(fz) => (fz - z_m).abs() >= MIN_APEX_DELTA_M,
                None => true, // already non-flat — the ridge would re-shape it
            })
            .collect();
        // The set to pick among: the real slope targets if any, else all
        // claimants (a degenerate all-no-op ridge keeps the prior behaviour).
        let candidates: &[usize] = if sloped.is_empty() { &claims } else { &sloped };
        if candidates.len() == 1 {
            // A real choice existed (≥ 2 claimants) but the slope-able filter
            // left one — resolve silently, carrying enough context for the
            // shell to NAME the decision (labels + ids + heights).
            let ri = candidates[0];
            let chosen = &storey.ceilings.regions[ri];
            let inside = area_inside_point(chosen, mid);
            return RidgeSlopeSide::AutoResolved(RidgeAutoChoice {
                hint: inside,
                sloped_label: area_side_label(axis, coord_m, inside),
                sloped_region: (chosen.id, chosen.flat_z()),
                passed_over: claims
                    .iter()
                    .copied()
                    .filter(|&other| other != ri)
                    .map(|other| {
                        let r = &storey.ceilings.regions[other];
                        let label = area_side_label(axis, coord_m, area_inside_point(r, mid));
                        (label, r.id, r.flat_z())
                    })
                    .collect(),
            });
        }
        // Two or more slope-able neighbours: a genuine two-way choice. Never
        // guess from the drawn side (it doesn't reliably match "which side rises
        // to the ridge") — ALWAYS ask.
        return RidgeSlopeSide::Ambiguous(
            candidates
                .iter()
                .map(|&ri| {
                    let inside = area_inside_point(&storey.ceilings.regions[ri], mid);
                    (area_side_label(axis, coord_m, inside), inside)
                })
                .collect(),
        );
    }

    // Strict-interior ridge (single claimant): a full gable — the drawn side's
    // hint if any (harmless on an outline wall), else no hint (both halves slope).
    let chosen = drawn_inside(&claims)
        .map(|ri| area_inside_point(&storey.ceilings.regions[ri], mid))
        .or(drawn_side);
    RidgeSlopeSide::Resolved(chosen)
}

/// The height the "meet other edge" shortcut sets a boundary ridge to: the flat
/// height of the RAISED neighbour it abuts, so the sloped (lower) area rises to
/// meet that area's ceiling flush. `None` unless the ridge's midpoint sits on a
/// shared boundary between two flat areas whose heights differ by ≥ 1 cm (i.e.
/// there IS a raised edge to meet).
pub fn ridge_meet_edge_height(storey: &Storey, a: Point2f64, b: Point2f64) -> Option<f64> {
    let mid = Point2f64::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    let claims = storey.ceilings.claim_regions(mid);
    if claims.len() < 2 {
        return None;
    }
    let heights: Vec<f64> = claims
        .iter()
        .filter_map(|&ri| storey.ceilings.regions[ri].flat_z())
        .collect();
    let lo = heights.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = heights.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // A genuine step: the highest neighbour must sit at least a centimetre above
    // the lowest, else there's no raised edge to meet.
    (hi - lo >= MIN_APEX_DELTA_M).then_some(hi)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionState, ShapeSlot};
    use ber_model::ber::structural::{
        CeilingApex, CeilingRegion, CeilingRegions, Polygon3D, Storey,
    };
    use domain::points::Point3f64;
    use manip_plan::FloorPlan;

    /// The ridge slope-side labels match the plan's on-screen orientation: y
    /// grows DOWNWARD, so a smaller y is "above" the ridge and a larger y is
    /// "below" (these were previously inverted). X is left/right as usual.
    #[test]
    fn area_side_label_matches_screen_orientation() {
        assert_eq!(
            area_side_label(ParameterAxis::Y, 5.0, (2.0, 3.0)),
            "Above the ridge"
        );
        assert_eq!(
            area_side_label(ParameterAxis::Y, 5.0, (2.0, 8.0)),
            "Below the ridge"
        );
        assert_eq!(
            area_side_label(ParameterAxis::X, 5.0, (2.0, 3.0)),
            "Left of the ridge"
        );
        assert_eq!(
            area_side_label(ParameterAxis::X, 5.0, (8.0, 3.0)),
            "Right of the ridge"
        );
    }

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    fn apex(x: f64, y: f64, z: f64) -> CeilingApex {
        CeilingApex {
            point: Point2f64::new(x, y),
            z_m: z,
        }
    }

    /// A Storey whose ceilings are one flat region at `height` over `ring`.
    fn storey_with_flat_ring(ring: Vec<Point2f64>, height: f64) -> Storey {
        Storey {
            ceilings: CeilingRegions::default_for_footprint(vec![ring], height, Default::default()),
            ceiling_height_m: Some(height),
            ..Storey::default()
        }
    }

    /// A Storey whose ceilings are one flat 4×4 region at the given height.
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
    /// durable list and re-shape the ceilings with the FULL list in one pass.
    fn apply_apexes(storey: &mut Storey, apexes: Vec<CeilingApex>) {
        storey.ceiling_apexes = apexes;
        storey.ceilings = std::mem::take(&mut storey.ceilings).with_apexes(&storey.ceiling_apexes);
    }

    /// The automatic GABLE-END rule ([`snap_ridge_extent_to_gables`]): an
    /// extent end within the snap tolerance (0.2 m) of its ceiling area's
    /// boundary — short of it OR past it — snaps exactly onto it; a
    /// genuinely interior end (a real hip) is untouched; ends snap
    /// independently.
    #[test]
    fn ridge_gable_snap_pulls_near_wall_ends_onto_the_walls() {
        let storey = storey_with_flat_roof(2.4);
        // Vertical ridge at x = 2 over the 4×4, ends 0.15 m short of the
        // walls → both snap: the wall-to-wall gable.
        let (lo, len) = snap_ridge_extent_to_gables(&storey, ParameterAxis::X, 2.0, 0.15, 3.7);
        assert!(
            lo.abs() < 1e-6 && (len - 4.0).abs() < 1e-6,
            "both ends snap: got ({lo}, {len})"
        );
        // A slight overshoot clamps back onto the walls too.
        let (lo, len) = snap_ridge_extent_to_gables(&storey, ParameterAxis::X, 2.0, -0.1, 4.2);
        assert!(
            lo.abs() < 1e-6 && (len - 4.0).abs() < 1e-6,
            "overshoot clamps: got ({lo}, {len})"
        );
        // Ends 0.5 m inside — a deliberate hip — stay exactly as drawn.
        assert_eq!(
            snap_ridge_extent_to_gables(&storey, ParameterAxis::X, 2.0, 0.5, 3.0),
            (0.5, 3.0)
        );
        // One near end snaps without dragging the far (interior) end along.
        let (lo, len) = snap_ridge_extent_to_gables(&storey, ParameterAxis::X, 2.0, 0.1, 3.0);
        assert!(
            lo.abs() < 1e-6 && (len - 3.1).abs() < 1e-6,
            "only the near end snaps: got ({lo}, {len})"
        );
    }

    /// The hexagon ring behind the fan-marker tests: 6 corners, area 24 m².
    /// The cd36f510 field-report layout: two raised strips at DIFFERENT
    /// heights (2.95 over y ∈ [0.093, 1.462], 2.572 over y ∈ [−1.385, 0.093],
    /// both x ∈ [1.366, 7.367]) plus three default-height (2.409) regions
    /// covering the rest of the footprint.
    fn cd36f510_storey() -> Storey {
        let r = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![
                Point2f64::new(x0, y0),
                Point2f64::new(x1, y0),
                Point2f64::new(x1, y1),
                Point2f64::new(x0, y1),
            ]
        };
        let mut c = CeilingRegions::default_for_footprint(
            vec![r(1.3659, 0.0929, 7.3669, 1.4619)],
            2.95,
            Default::default(),
        );
        for (ring, z) in [
            (r(1.3659, -1.3851, 7.3669, 0.0929), 2.572),
            (r(7.3669, 1.4619, 9.875, 9.234), 2.409),
            (r(1.3659, 1.4619, 7.3669, 9.401), 2.409),
            (r(7.3669, -1.3851, 9.875, 1.4619), 2.409),
        ] {
            c.regions.extend(
                CeilingRegions::default_for_footprint(vec![ring], z, Default::default()).regions,
            );
        }
        Storey {
            ceilings: c,
            ceiling_height_m: Some(2.409),
            ..Storey::default()
        }
    }

    /// THE cd36f510 REGRESSION: a ridge spanning BOTH raised strips (one end
    /// in each, crossing the ceiling step between them) passes the extent
    /// and height checks via the flat-CHAIN acceptance — no single region
    /// contains it, but the chain of flat un-apexed regions it crosses does,
    /// and the solve fuses exactly that chain
    /// (`fuse_ridge_spanned_regions` in ber_model). Before the chain checks
    /// both stages rejected it ("out of its ceiling area" / "both ends must
    /// sit in the same ceiling area").
    #[test]
    fn ridge_across_two_raised_strips_passes_the_chain_checks() {
        let storey = cd36f510_storey();
        // Vertical ridge at x = 4.37 from the outline wall (y = −1.3851) to
        // the strip boundary (y = 1.4619): axis = X (cross), extent on Y.
        assert_eq!(
            validate_roof_ridge_extent(&storey, ParameterAxis::X, 4.37, -1.3851, 2.847),
            Ok(())
        );
        assert_eq!(
            validate_roof_ridge(
                &storey,
                Point2f64::new(4.37, -1.3851),
                Point2f64::new(4.37, 1.4619),
                3.4
            ),
            Ok(())
        );
        // The Length stage's optimistic bound spans the CONNECTED flat
        // component, not one region — 2.847 m fits the pair even though each
        // strip alone runs under 1.5 m.
        assert_eq!(
            validate_roof_ridge_length(&storey, ParameterAxis::X, 2.847),
            Ok(())
        );
    }

    /// The chain acceptance does not swallow real rejections: a chain ridge
    /// at a height within a centimetre of a MEMBER's base is rejected (it
    /// would leave that strip unsloped), and a ridge poking outside every
    /// ceiling region stays rejected.
    #[test]
    fn chain_ridge_still_rejects_member_base_heights_and_outside_ends() {
        let storey = cd36f510_storey();
        let a = Point2f64::new(4.37, -1.3851);
        let b = Point2f64::new(4.37, 1.4619);
        // 2.95 is a member strip's own height — near-noop over it.
        let err = validate_roof_ridge(&storey, a, b, 2.9501).unwrap_err();
        assert!(err.contains("within a centimetre"), "got: {err}");
        // An extent overrunning the whole footprint is still out.
        let err =
            validate_roof_ridge_extent(&storey, ParameterAxis::X, 4.37, -3.0, 4.5).unwrap_err();
        assert!(
            err.starts_with(RIDGE_LENGTH_EXCEEDS_AREA_MSG) || err == RIDGE_OUTSIDE_CEILING_MSG,
            "got: {err}"
        );
    }

    /// A chain member that already carries an apex group blocks the chain —
    /// the single-region occupied rejection stands.
    #[test]
    fn chain_ridge_over_an_apexed_strip_still_rejects() {
        let mut storey = cd36f510_storey();
        apply_apexes(&mut storey, vec![apex(4.0, 0.7, 3.2)]); // fan in the 2.95 strip
        let err = validate_roof_ridge(
            &storey,
            Point2f64::new(4.37, -1.3851),
            Point2f64::new(4.37, 1.4619),
            3.4,
        )
        .unwrap_err();
        assert!(
            err.contains("already has a high point") || err.contains("same ceiling area"),
            "got: {err}"
        );
    }

    fn hex_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(6.0, 2.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
            Point2f64::new(-2.0, 2.0),
        ]
    }

    /// [`ceiling_fan_markers`] shows markers only when ACTIONABLE: a true
    /// gable's 4-corner fan gets none; a hexagon fan (6 > 4 fanned) gets all
    /// six; unfanning one keeps all six visible with the unfanned one
    /// flagged (it must stay tappable to re-fan). [`ceiling_fan_marker_at`]
    /// resolves a near tap to the canonical vertex.
    #[test]
    fn fan_markers_show_only_when_actionable_and_flag_unfanned() {
        // A wall-to-wall gable over the 4×4: the fan reaches only the 4
        // corners — nothing actionable, no markers.
        let mut gable = storey_with_flat_roof(2.4);
        apply_apexes(
            &mut gable,
            vec![
                CeilingApex {
                    point: Point2f64::new(2.0, 0.0),
                    z_m: 4.2,
                },
                CeilingApex {
                    point: Point2f64::new(2.0, 4.0),
                    z_m: 4.2,
                },
            ],
        );
        assert!(
            ceiling_fan_markers(&gable).is_empty(),
            "a 4-corner gable fan shows no markers"
        );

        // A hexagon fan: six fanned vertices → all six markers, all fanned.
        let mut storey = storey_with_flat_ring(hex_ring(), 2.4);
        apply_apexes(
            &mut storey,
            vec![CeilingApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.5,
            }],
        );
        let markers = ceiling_fan_markers(&storey);
        assert_eq!(markers.len(), 6, "every hexagon corner is a fan target");
        assert!(markers.iter().all(|m| m.fanned));
        assert_eq!(
            ceiling_fan_marker_at(&storey, Point2f64::new(6.001, 2.0), 0.01),
            Some(Point2f64::new(6.0, 2.0)),
            "a near tap resolves to the canonical vertex"
        );
        assert_eq!(
            ceiling_fan_marker_at(&storey, Point2f64::new(3.0, 2.0), 0.01),
            None,
            "a stray tap hits nothing"
        );

        // Unfan one vertex and re-derive the way recompose does: the marker
        // set stays six, with exactly that vertex flagged unfanned.
        storey.ceiling_unfanned = vec![(6.0, 2.0)];
        storey.ceilings =
            CeilingRegions::default_for_footprint(vec![hex_ring()], 2.4, Default::default())
                .with_apexes_hinted_unfanned(
                    &storey.ceiling_apexes,
                    &[],
                    &storey.ceiling_unfanned_points(),
                );
        let markers = ceiling_fan_markers(&storey);
        assert_eq!(markers.len(), 6, "the unfanned vertex stays tappable");
        let unf: Vec<_> = markers.iter().filter(|m| !m.fanned).collect();
        assert_eq!(unf.len(), 1);
        assert_eq!(unf[0].pos, Point2f64::new(6.0, 2.0));
    }

    /// The markers on the ee77622f gable-over-a-bay drawing mirror the
    /// AUTOMATIC solve: each side's PITCH ANCHOR (the lex-smallest of the
    /// nearest eave corners — the deterministic tie-break) renders as the
    /// anchor marker, every other corner is embedded (struck, tappable to
    /// re-anchor) — including the two bay corners, whose deviation is what
    /// makes the marker set visible at all.
    #[test]
    fn fan_markers_flag_auto_embedded_bay_corners_on_a_gable() {
        let ring = vec![
            Point2f64::new(1.52, 2.31),
            Point2f64::new(6.52, 2.31),
            Point2f64::new(6.52, 8.31),
            Point2f64::new(5.52, 8.31),
            Point2f64::new(5.52, 7.31),
            Point2f64::new(1.52, 7.31),
        ];
        let mut storey = storey_with_flat_ring(ring, 2.0);
        apply_apexes(
            &mut storey,
            vec![
                CeilingApex {
                    point: Point2f64::new(1.52, 4.66),
                    z_m: 3.0,
                },
                CeilingApex {
                    point: Point2f64::new(6.52, 4.66),
                    z_m: 3.0,
                },
            ],
        );
        let markers = ceiling_fan_markers(&storey);
        assert_eq!(markers.len(), 6, "all six footprint corners are markers");
        let near = |m: &CeilingFanMarker, x: f64, y: f64| {
            (m.pos.x - x).abs() <= 1e-6 && (m.pos.y - y).abs() <= 1e-6
        };
        // One anchor per side, lex-smallest of the 2.65/2.35 m ties.
        let anchors: Vec<_> = markers.iter().filter(|m| m.anchor).collect();
        assert_eq!(anchors.len(), 2, "one pitch anchor per side: {markers:?}");
        assert!(anchors.iter().any(|m| near(m, 1.52, 2.31)), "north anchor");
        assert!(anchors.iter().any(|m| near(m, 1.52, 7.31)), "south anchor");
        assert!(anchors.iter().all(|m| m.fanned), "anchors keep their z");
        // Everything else is embedded — the bay corners among them.
        assert_eq!(markers.iter().filter(|m| !m.fanned).count(), 4);
        for (x, y) in [(5.52, 8.31), (6.52, 8.31)] {
            assert!(
                markers
                    .iter()
                    .any(|m| near(m, x, y) && !m.fanned && !m.anchor),
                "bay corner ({x}, {y}) shows as embedded"
            );
        }
    }

    /// USER-SELECTED PLANE ANCHOR (the tap flow end to end, document side):
    /// on the ee77622f gable, tapping the embedded bay corner yields
    /// `SetPlaneAnchor`; applying it re-pitches the south plane to the bay
    /// wall (the main-wall corners now embed ABOVE base); the markers show
    /// the moved anchor; tapping the new anchor yields `ClearPlaneAnchor`,
    /// and clearing restores the auto solve. A stale override is ignored.
    #[test]
    fn plane_anchor_override_moves_the_pitch_and_clears_back_to_auto() {
        let ring = vec![
            Point2f64::new(1.52, 2.31),
            Point2f64::new(6.52, 2.31),
            Point2f64::new(6.52, 8.31),
            Point2f64::new(5.52, 8.31),
            Point2f64::new(5.52, 7.31),
            Point2f64::new(1.52, 7.31),
        ];
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(1.52, 4.66),
                z_m: 3.0,
            },
            CeilingApex {
                point: Point2f64::new(6.52, 4.66),
                z_m: 3.0,
            },
        ];
        let recompose = |storey: &mut Storey| {
            storey.ceilings =
                CeilingRegions::default_for_footprint(vec![ring.clone()], 2.0, Default::default())
                    .with_apexes_shaped(
                        &storey.ceiling_apexes,
                        &[],
                        &storey.ceiling_unfanned_points(),
                        &storey.ceiling_plane_anchor_points(),
                    );
        };
        let mut storey = storey_with_flat_ring(ring.clone(), 2.0);
        storey.ceiling_apexes = apexes;
        recompose(&mut storey);

        // Tap the embedded bay corner → SetPlaneAnchor (nothing to drop
        // yet). The returned canonical position is the MERGED-ring vertex —
        // boolean-op output a few 1e-10 off the drawn corner, absorbed by
        // the storage quantization + epsilon matching downstream.
        let bay = Point2f64::new(5.52, 8.31);
        let close =
            |p: &Point2f64, q: &Point2f64| (p.x - q.x).abs() <= 1e-6 && (p.y - q.y).abs() <= 1e-6;
        match ceiling_fan_tap_action(&storey, bay, 1e-3) {
            Some(CeilingFanTapAction::SetPlaneAnchor { anchor, drop }) => {
                assert!(close(&anchor, &bay), "canonical bay corner, got {anchor:?}");
                assert!(drop.is_empty(), "no previous override to drop");
            }
            other => panic!("expected SetPlaneAnchor, got {other:?}"),
        }
        // Apply it (the controller's Ber::update_ceiling_plane_anchors) and
        // re-derive: the south plane now meets base at the BAY wall
        // (dist 3.65), so the main-wall corners embed ABOVE base.
        storey.ceiling_plane_anchors = vec![(5.52, 8.31)];
        recompose(&mut storey);
        assert_eq!(storey.ceilings.regions.len(), 2, "still two planes");
        let z_main = 3.0 - 2.65 / 3.65; // ≈ 2.274 — above the 2.0 base
        let south = storey
            .ceilings
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            })
            .expect("south plane");
        let z_at = |x: f64, y: f64| {
            south
                .polygon
                .exterior
                .iter()
                .find(|v| (v.x - x).abs() <= 1e-9 && (v.y - y).abs() <= 1e-9)
                .map(|v| v.z)
                .unwrap_or_else(|| panic!("south plane misses ({x}, {y})"))
        };
        assert!(
            (z_at(5.52, 8.31) - 2.0).abs() <= 1e-9,
            "the override anchors at base"
        );
        assert!(
            (z_at(6.52, 8.31) - 2.0).abs() <= 1e-9,
            "co-distant bay corner on plane"
        );
        assert!(
            (z_at(1.52, 7.31) - z_main).abs() <= 1e-9 && (z_at(5.52, 7.31) - z_main).abs() <= 1e-9,
            "main-wall corners embed above base: got {} / {}",
            z_at(1.52, 7.31),
            z_at(5.52, 7.31)
        );
        // The markers mirror it: the south anchor moved to the bay corner.
        let markers = ceiling_fan_markers(&storey);
        assert!(
            markers.iter().any(|m| m.anchor
                && (m.pos.x - 5.52).abs() <= 1e-6
                && (m.pos.y - 8.31).abs() <= 1e-6),
            "the override renders as the anchor: {markers:?}"
        );
        // Tap the new anchor → ClearPlaneAnchor dropping the stored override.
        match ceiling_fan_tap_action(&storey, bay, 1e-3) {
            Some(CeilingFanTapAction::ClearPlaneAnchor { drop }) => {
                assert_eq!(drop.len(), 1, "exactly the stored override drops");
                assert!(close(&drop[0], &bay), "the stored override, got {drop:?}");
            }
            other => panic!("expected ClearPlaneAnchor, got {other:?}"),
        }
        storey.ceiling_plane_anchors.clear();
        recompose(&mut storey);
        let south = storey
            .ceilings
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            })
            .expect("south plane");
        let bay_auto = south
            .polygon
            .exterior
            .iter()
            .find(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            .unwrap();
        assert!(
            (bay_auto.z - (3.0 - 3.65 / 2.65)).abs() <= 1e-9,
            "clearing restores the auto pitch (bay below base): {}",
            bay_auto.z
        );
        // A STALE override (matching no ring vertex) is ignored entirely.
        storey.ceiling_plane_anchors = vec![(99.0, 99.0)];
        recompose(&mut storey);
        let south = storey
            .ceilings
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            })
            .expect("south plane");
        assert!(
            south
                .polygon
                .exterior
                .iter()
                .any(|v| (v.y - 8.31).abs() <= 1e-9 && (v.z - (3.0 - 3.65 / 2.65)).abs() <= 1e-9),
            "a stale override falls back to auto-nearest"
        );
    }

    /// The USER'S HIP SCENARIO pinned (no code change expected): the ridge's
    /// east endpoint pulled 2 m inside the wall (an interior end — no gable
    /// snap), the bay corners manually unfanned → they embed ON the plane of
    /// the effective solve's containing hub face, so "the plane continues in
    /// those regions": the face through the bay is ONE planar polygon
    /// holding its fanned neighbours at base and the hub at ridge height,
    /// and the face count drops against the un-unfanned baseline.
    #[test]
    fn hip_manual_unfan_continues_the_neighbour_plane_over_the_bay() {
        let ring = vec![
            Point2f64::new(1.52, 2.31),
            Point2f64::new(6.52, 2.31),
            Point2f64::new(6.52, 8.31),
            Point2f64::new(5.52, 8.31),
            Point2f64::new(5.52, 7.31),
            Point2f64::new(1.52, 7.31),
        ];
        // West end on the wall, EAST end 2 m short of x = 6.52 — a hip end.
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(1.52, 4.66),
                z_m: 3.0,
            },
            CeilingApex {
                point: Point2f64::new(4.52, 4.66),
                z_m: 3.0,
            },
        ];
        let cid = Default::default();
        let baseline = CeilingRegions::default_for_footprint(vec![ring.clone()], 2.0, cid)
            .with_apexes(&apexes);
        assert!(
            baseline.regions.len() > 2,
            "interior east end fans/hips: {} faces",
            baseline.regions.len()
        );
        // Unfan the two bay corners.
        let unfanned = vec![Point2f64::new(6.52, 8.31), Point2f64::new(5.52, 8.31)];
        let out = CeilingRegions::default_for_footprint(vec![ring.clone()], 2.0, cid)
            .with_apexes_hinted_unfanned(&apexes, &[], &unfanned);
        assert!(
            out.regions.len() < baseline.regions.len(),
            "the bay facets fold away: {} → {}",
            baseline.regions.len(),
            out.regions.len()
        );
        let total: f64 = out
            .regions
            .iter()
            .map(|r| {
                let e = &r.polygon.exterior;
                let n = e.len();
                (0..n)
                    .map(|i| e[i].x * e[(i + 1) % n].y - e[(i + 1) % n].x * e[i].y)
                    .sum::<f64>()
                    .abs()
                    / 2.0
            })
            .sum();
        assert!((total - 26.0).abs() < 1e-6, "coverage unchanged");
        // The face over the bay is ONE planar polygon — the neighbours'
        // plane continued: it holds the fanned neighbours (6.52, 2.31) and
        // (5.52, 7.31) at base, hub B at ridge height, and both bay corners
        // ON that same plane (below base — the roof keeps falling).
        let bay_face = out
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            })
            .expect("a face spans the bay");
        let has = |x: f64, y: f64, z: f64| {
            bay_face.polygon.exterior.iter().any(|v| {
                (v.x - x).abs() <= 1e-9 && (v.y - y).abs() <= 1e-9 && (v.z - z).abs() <= 1e-9
            })
        };
        assert!(
            has(6.52, 2.31, 2.0) && has(5.52, 7.31, 2.0) && has(4.52, 4.66, 3.0),
            "the containing face runs neighbour-to-neighbour through the hub: {:?}",
            bay_face.polygon
        );
        // Both embedded corners lie ON that face's plane, below base.
        let (nrm, d) = {
            // Newell plane of the face (the merge guarantees planarity).
            let e = &bay_face.polygon.exterior;
            let (mut nx, mut ny, mut nz) = (0.0, 0.0, 0.0);
            for i in 0..e.len() {
                let (p, q) = (e[i], e[(i + 1) % e.len()]);
                nx += (p.y - q.y) * (p.z + q.z);
                ny += (p.z - q.z) * (p.x + q.x);
                nz += (p.x - q.x) * (p.y + q.y);
            }
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            let n = (nx / len, ny / len, nz / len);
            let d = e
                .iter()
                .map(|v| n.0 * v.x + n.1 * v.y + n.2 * v.z)
                .sum::<f64>()
                / e.len() as f64;
            (n, d)
        };
        for (x, y) in [(6.52, 8.31), (5.52, 8.31)] {
            let v = bay_face
                .polygon
                .exterior
                .iter()
                .find(|v| (v.x - x).abs() <= 1e-9 && (v.y - y).abs() <= 1e-9)
                .unwrap_or_else(|| panic!("bay corner ({x}, {y}) missing from the face"));
            assert!(
                (nrm.0 * v.x + nrm.1 * v.y + nrm.2 * v.z - d).abs() <= 1e-9,
                "bay corner ({x}, {y}) sits on the continued plane"
            );
            assert!(v.z < 2.0, "the continued plane keeps falling past the eave");
        }
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

    /// A ridge drawn on the joint between an already-RAISED area (flat height ==
    /// the ridge height) and a LOWER flat area resolves to the LOWER area — the
    /// only one the ridge can actually slope. A ridge at height == a region's own
    /// base is a no-op for it, so the resolver must ignore the drawn side and
    /// pick the lower neighbour. (Regression: the apexes landed on the raised
    /// neighbour, whose re-ridge collapsed back to flat — nothing sloped, and
    /// `apex_groups` was empty.)
    #[test]
    fn boundary_ridge_slopes_the_lower_area_not_the_already_raised_one() {
        let cid = Default::default();
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![
                Point2f64::new(x0, y0),
                Point2f64::new(x1, y0),
                Point2f64::new(x1, y1),
                Point2f64::new(x0, y1),
            ]
        };
        // Lower area y0..2 at 2.4; raised area y2..5 at 3.4; shared joint y = 2.
        let mut ceilings =
            CeilingRegions::default_for_footprint(vec![sq(0.0, 0.0, 6.0, 2.0)], 2.4, cid);
        ceilings.regions.extend(
            CeilingRegions::default_for_footprint(vec![sq(0.0, 2.0, 6.0, 5.0)], 3.4, cid).regions,
        );
        let storey = Storey {
            ceilings,
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };

        let a = Point2f64::new(0.0, 2.0);
        let b = Point2f64::new(6.0, 2.0);
        // Draw on the RAISED side (drawn_cross just above the joint) at the raised
        // height (3.4): it must STILL resolve into the lower area.
        match resolve_ridge_slope_side(&storey, a, b, Some(2.1), ParameterAxis::Y, 2.0, 3.4) {
            RidgeSlopeSide::AutoResolved(auto) => assert!(
                (auto.hint.1 as f64) < 2.0,
                "hint sits inside the lower area (y < 2), got y = {}",
                auto.hint.1
            ),
            RidgeSlopeSide::Resolved(_) => {
                panic!("a junction resolve must be AutoResolved (it had a real choice)")
            }
            RidgeSlopeSide::Ambiguous(_) => {
                panic!("expected a deterministic resolve to the lower area, got Ambiguous")
            }
        }
    }

    /// The silent junction resolve carries the context the shell needs to
    /// surface it: both sides' labels (the ChooseArea vocabulary), the region
    /// ids, and the flat heights.
    #[test]
    fn auto_resolved_junction_ridge_names_both_sides() {
        let cid = Default::default();
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![
                Point2f64::new(x0, y0),
                Point2f64::new(x1, y0),
                Point2f64::new(x1, y1),
                Point2f64::new(x0, y1),
            ]
        };
        // Lower area y 0..2 at 2.4; raised area y 2..5 at 3.4; joint y = 2.
        let mut ceilings =
            CeilingRegions::default_for_footprint(vec![sq(0.0, 0.0, 6.0, 2.0)], 2.4, cid);
        ceilings.regions.extend(
            CeilingRegions::default_for_footprint(vec![sq(0.0, 2.0, 6.0, 5.0)], 3.4, cid).regions,
        );
        let lower_id = ceilings.regions[0].id;
        let raised_id = ceilings.regions[1].id;
        let storey = Storey {
            ceilings,
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };

        let a = Point2f64::new(0.0, 2.0);
        let b = Point2f64::new(6.0, 2.0);
        // Ridge at the RAISED height: only the lower area is slope-able.
        let RidgeSlopeSide::AutoResolved(auto) =
            resolve_ridge_slope_side(&storey, a, b, None, ParameterAxis::Y, 2.0, 3.4)
        else {
            panic!("expected AutoResolved for the one-slope-able junction");
        };
        // Plan y grows downward: the lower area (y < 2) is "Above the ridge".
        assert_eq!(auto.sloped_label, "Above the ridge");
        assert_eq!(auto.sloped_region, (lower_id, Some(2.4)));
        assert_eq!(
            auto.passed_over,
            vec![("Below the ridge".to_string(), raised_id, Some(3.4))]
        );
    }

    /// A ridge on the joint between TWO areas the ridge can actually slope (both
    /// flat heights differ from the ridge height) is a genuine two-way choice:
    /// it must ALWAYS ask, never auto-resolve from the drawn side. (Regression
    /// for the surveyor's report: the ridge silently sloped the side the stroke
    /// was drawn over — the HIGHER area, a barely-visible rise — instead of the
    /// LOWER area the assessor meant to raise up to the ridge.)
    #[test]
    fn boundary_ridge_between_two_slopeable_areas_always_asks() {
        let cid = Default::default();
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![
                Point2f64::new(x0, y0),
                Point2f64::new(x1, y0),
                Point2f64::new(x1, y1),
                Point2f64::new(x0, y1),
            ]
        };
        // Lower area y0..2 at 2.0; upper area y2..5 at 2.4; shared joint y = 2.
        // Ridge height 3.0 is above BOTH, so BOTH are slope-able → two candidates.
        let mut ceilings =
            CeilingRegions::default_for_footprint(vec![sq(0.0, 0.0, 6.0, 2.0)], 2.0, cid);
        ceilings.regions.extend(
            CeilingRegions::default_for_footprint(vec![sq(0.0, 2.0, 6.0, 5.0)], 2.4, cid).regions,
        );
        let storey = Storey {
            ceilings,
            ceiling_height_m: Some(2.0),
            ..Storey::default()
        };

        let a = Point2f64::new(0.0, 2.0);
        let b = Point2f64::new(6.0, 2.0);
        // Drawn clearly on the UPPER side (drawn_cross = 2.1): must NOT auto-pick
        // it — a genuine two-way choice always asks.
        match resolve_ridge_slope_side(&storey, a, b, Some(2.1), ParameterAxis::Y, 2.0, 3.0) {
            RidgeSlopeSide::Ambiguous(opts) => {
                assert_eq!(opts.len(), 2, "both sides offered, got {}", opts.len())
            }
            RidgeSlopeSide::Resolved(_) | RidgeSlopeSide::AutoResolved(_) => {
                panic!("expected Ambiguous (always ask), got a silent resolve")
            }
        }
    }

    /// `ridge_meet_edge_height` returns the RAISED neighbour's height for a
    /// ridge on the joint of a stepped ceiling, and `None` when the two
    /// neighbours are level (no raised edge to meet).
    #[test]
    fn meet_edge_height_is_the_raised_neighbours_ceiling() {
        let cid = Default::default();
        let sq = |x0: f64, y0: f64, x1: f64, y1: f64| {
            vec![
                Point2f64::new(x0, y0),
                Point2f64::new(x1, y0),
                Point2f64::new(x1, y1),
                Point2f64::new(x0, y1),
            ]
        };
        let stepped = |raised: f64| {
            let mut c =
                CeilingRegions::default_for_footprint(vec![sq(0.0, 0.0, 6.0, 2.0)], 2.4, cid);
            c.regions.extend(
                CeilingRegions::default_for_footprint(vec![sq(0.0, 2.0, 6.0, 5.0)], raised, cid)
                    .regions,
            );
            Storey {
                ceilings: c,
                ceiling_height_m: Some(2.4),
                ..Storey::default()
            }
        };
        let a = Point2f64::new(0.0, 2.0);
        let b = Point2f64::new(6.0, 2.0);
        // Raised neighbour at 3.4 → meet it.
        assert_eq!(
            ridge_meet_edge_height(&stepped(3.4), a, b),
            Some(3.4),
            "meet the raised neighbour"
        );
        // Level neighbours (both 2.4) → nothing to meet.
        assert_eq!(
            ridge_meet_edge_height(&stepped(2.4), a, b),
            None,
            "no raised edge → no meet height"
        );
    }

    /// Strict containment: a point on a region boundary, outside every region,
    /// or in a storey with no ceilings at all gets the stable outside-ceiling
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
        storey.ceilings.regions.push(CeilingRegion {
            id: Default::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(10.0, 0.0, 2.0),
                Point3f64::new(14.0, 0.0, 2.5),
                Point3f64::new(12.0, 3.0, 3.0),
            ]),
            opes: Default::default(),
            construction_id: Default::default(),
            exposure: Default::default(),
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
    /// message, ends in two DIFFERENT flat un-apexed regions are legal via
    /// the flat-CHAIN acceptance (cd36f510 — the solve fuses them), and an
    /// end on an INTERNAL shared boundary is a legal mono-pitch junction.
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
        storey.ceilings.regions.extend(
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
        // Midpoint in region 0, far end strictly in region 1: LEGAL since the
        // flat-CHAIN acceptance (cd36f510) — the ridge spans both flat
        // un-apexed regions and the solve fuses them into one gable
        // (`fuse_ridge_spanned_regions`). The same-region rejection now only
        // fires when the chain itself is unusable (sloped/apexed members,
        // disjoint union).
        assert!(validate_roof_ridge(
            &storey,
            Point2f64::new(1.0, 2.0),
            Point2f64::new(6.0, 2.0),
            4.2
        )
        .is_ok());
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
    fn ceiling_point_must_be_strictly_inside_the_outline_bbox() {
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
        // …and a storey with no ceilings at all does too.
        let err =
            validate_roof_ridge_length(&Storey::default(), ParameterAxis::X, 1.0).unwrap_err();
        assert!(err.starts_with(RIDGE_LENGTH_NO_AREA_MSG), "got: {err}");

        // With a SECOND, longer flat region the bound is the MAX over the
        // candidates.
        let mut storey = storey_with_flat_roof(2.4);
        storey.ceilings.regions.extend(
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
        // position outside every region, or a storey with no ceilings at all: the
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
        two.ceilings.regions.extend(
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
        storey.ceilings.regions.extend(
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
        storey.ceilings.regions.extend(
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

    // ── DORMER validation ───────────────────────────────────────────────────

    /// A single SLOPED ceiling region — a `[x0,x1] × [y0,y1]` rectangle on the
    /// plane `z = base + slope_y·y` (rises northward in y). Planar by
    /// construction, so `region_plane` / `top_z_at` evaluate it exactly.
    fn sloped_quad(x0: f64, y0: f64, x1: f64, y1: f64, base: f64, slope_y: f64) -> CeilingRegion {
        let z = |y: f64| base + slope_y * y;
        CeilingRegion {
            id: Default::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(x0, y0, z(y0)),
                Point3f64::new(x1, y0, z(y0)),
                Point3f64::new(x1, y1, z(y1)),
                Point3f64::new(x0, y1, z(y1)),
            ]),
            opes: Default::default(),
            construction_id: Default::default(),
            exposure: Default::default(),
        }
    }

    /// A single FLAT ceiling region — a `[x0,x1] × [y0,y1]` rectangle at `z`.
    fn flat_quad(x0: f64, y0: f64, x1: f64, y1: f64, z: f64) -> CeilingRegion {
        CeilingRegion {
            id: Default::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(x0, y0, z),
                Point3f64::new(x1, y0, z),
                Point3f64::new(x1, y1, z),
                Point3f64::new(x0, y1, z),
            ]),
            opes: Default::default(),
            construction_id: Default::default(),
            exposure: Default::default(),
        }
    }

    /// A four-face PYRAMID fan over 4×4: base corners at `base`, apex (2,2) at
    /// `apex_z`. Each triangular face is planar by construction, so the apex is a
    /// region CORNER strictly inside a footprint drawn around it — higher than
    /// any of that footprint's own corners. Faces meet at the apex (a shared
    /// corner), so `top_z_at(2,2)` evaluates to `apex_z`.
    fn fan_pyramid(base: f64, apex_z: f64) -> Vec<CeilingRegion> {
        let a = Point3f64::new(2.0, 2.0, apex_z);
        let mk = |p0: Point3f64, p1: Point3f64| CeilingRegion {
            id: Default::default(),
            polygon: Polygon3D::new(vec![p0, p1, a]),
            opes: Default::default(),
            construction_id: Default::default(),
            exposure: Default::default(),
        };
        let c00 = Point3f64::new(0.0, 0.0, base);
        let c10 = Point3f64::new(4.0, 0.0, base);
        let c11 = Point3f64::new(4.0, 4.0, base);
        let c01 = Point3f64::new(0.0, 4.0, base);
        vec![mk(c00, c10), mk(c10, c11), mk(c11, c01), mk(c01, c00)]
    }

    fn storey_with_regions(regions: Vec<CeilingRegion>) -> Storey {
        Storey {
            ceilings: CeilingRegions { regions },
            ..Storey::default()
        }
    }

    fn pt(x: f64, y: f64) -> Point2f64 {
        Point2f64::new(x, y)
    }

    /// An island dormer strictly inside ONE SLOPED region is accepted when its
    /// top clears the roof over the whole footprint — INCLUDING the flush case
    /// where the top exactly meets the roof along the uphill edge (the key
    /// relaxation vs `validate_roof_apex`: the region need NOT be flat).
    #[test]
    fn island_dormer_on_a_sloped_region_is_accepted() {
        // 10×6 region on z = 2.0 + 0.2·y: roof rises from 2.0 (y=0) to 3.2 (y=6).
        let storey = storey_with_regions(vec![sloped_quad(0.0, 0.0, 10.0, 6.0, 2.0, 0.2)]);
        // Footprint (3,2)-(6,2)-(6,4)-(3,4): roof over it spans 2.4 (y=2) → 2.8
        // (y=4, the uphill edge).
        let fp = vec![pt(3.0, 2.0), pt(6.0, 2.0), pt(6.0, 4.0), pt(3.0, 4.0)];
        // Well clear of the roof.
        assert!(validate_dormer(&storey, &fp, 3.0).is_ok());
        // FLUSH with the roof along the uphill edge (max roof z = 2.8): ≥ passes.
        assert!(
            validate_dormer(&storey, &fp, 2.8).is_ok(),
            "a top flush with the uphill roof edge is allowed"
        );
    }

    /// The REPORTED case: a dormer whose footprint SPANS a sloped region and an
    /// adjacent FLAT protrusion is ACCEPTED — the whole patch is covered by the
    /// UNION of the two areas, so it no longer has to land in one region.
    #[test]
    fn dormer_spanning_a_sloped_and_a_flat_region_is_accepted() {
        // Sloped region A over x∈[0,5] (z = 2.0 + 0.2·y), flat protrusion B over
        // x∈[5,10] at 2.8 — the two share the internal boundary x = 5.
        let storey = storey_with_regions(vec![
            sloped_quad(0.0, 0.0, 5.0, 6.0, 2.0, 0.2),
            flat_quad(5.0, 0.0, 10.0, 6.0, 2.8),
        ]);
        // Footprint runs from the sloped area (x = 3) INTO the flat one (x = 7).
        let fp = vec![pt(3.0, 2.0), pt(7.0, 2.0), pt(7.0, 4.0), pt(3.0, 4.0)];
        // Roof over it: the sloped side peaks at y = 4 → 2.0 + 0.8 = 2.8; the
        // flat side is 2.8. A top of 3.0 clears the whole footprint.
        assert!(
            validate_dormer(&storey, &fp, 3.0).is_ok(),
            "a dormer spanning a sloped + flat region is accepted"
        );
        // FLUSH with the highest roof point (2.8) passes.
        assert!(validate_dormer(&storey, &fp, 2.8).is_ok());
        // A top below the roof (2.5 < 2.8) is an inverted riser — rejected.
        assert_eq!(
            validate_dormer(&storey, &fp, 2.5).unwrap_err(),
            DORMER_TOP_BELOW_ROOF_MSG
        );
    }

    /// A CORNER/RIDGE dormer spanning TWO SLOPED faces that meet at a ridge (the
    /// dormer runs across the ridge onto both slopes) is ACCEPTED when its top
    /// clears the ridge — and REJECTED when the ridge, running through the
    /// footprint INTERIOR, rises above the top even though every footprint corner
    /// is below it (the same union-coverage + intersection-vertex path as the
    /// flat-span case, exercised over two SLOPED regions).
    #[test]
    fn dormer_spanning_two_slopes_across_a_ridge() {
        // South face rises z = 2.0 + 0.4·y to the ridge at y = 5 (z = 4.0); north
        // face falls z = 6.0 - 0.4·y from it.
        let storey = storey_with_regions(vec![
            sloped_quad(0.0, 0.0, 10.0, 5.0, 2.0, 0.4),
            sloped_quad(0.0, 5.0, 10.0, 10.0, 6.0, -0.4),
        ]);
        // Footprint straddles the ridge y = 5: corners at y = 4/6 sit at z = 3.6,
        // the ridge crosses the interior at z = 4.0.
        let fp = vec![pt(3.0, 4.0), pt(7.0, 4.0), pt(7.0, 6.0), pt(3.0, 6.0)];
        // Clears the ridge → accepted; flush with it (4.0) too.
        assert!(
            validate_dormer(&storey, &fp, 4.2).is_ok(),
            "a dormer spanning two slopes across the ridge is accepted"
        );
        assert!(
            validate_dormer(&storey, &fp, 4.0).is_ok(),
            "flush with the ridge passes"
        );
        // Above every corner (3.6) but below the interior ridge (4.0): the
        // boundary crossings (3,5)/(7,5) sit on the ridge at 4.0, so clearance
        // rejects — the ridge through the interior, not any corner, binds.
        assert_eq!(
            validate_dormer(&storey, &fp, 3.8).unwrap_err(),
            DORMER_TOP_BELOW_ROOF_MSG
        );
    }

    /// A dormer top BELOW a ridge/apex that passes through the footprint
    /// INTERIOR is rejected — even though the top clears every footprint CORNER.
    /// Proves the clearance check samples the intersection vertices (which
    /// include the interior apex), not just the footprint's own corners.
    #[test]
    fn dormer_top_below_an_interior_ridge_apex_is_rejected() {
        // Pyramid fan over 4×4: base 2.4, apex (2,2) at 4.2. A footprint square
        // (1,1)-(3,3) around the apex has all FOUR corners at z = 3.3, but the
        // apex — a region corner INSIDE the footprint — rises to 4.2.
        let storey = storey_with_regions(fan_pyramid(2.4, 4.2));
        let fp = vec![pt(1.0, 1.0), pt(3.0, 1.0), pt(3.0, 3.0), pt(1.0, 3.0)];
        // Above every corner (3.3) yet below the interior apex (4.2): corner-only
        // sampling would WRONGLY accept; the intersection-vertex check rejects.
        assert_eq!(
            validate_dormer(&storey, &fp, 3.5).unwrap_err(),
            DORMER_TOP_BELOW_ROOF_MSG
        );
        // Flush with the apex (4.2) passes.
        assert!(validate_dormer(&storey, &fp, 4.2).is_ok());
    }

    /// A dormer whose top dips below the sloped roof SOMEWHERE over its
    /// footprint (an inverted riser) is rejected, even though it clears the roof
    /// at the downhill edge.
    #[test]
    fn dormer_top_below_the_slope_somewhere_is_rejected() {
        let storey = storey_with_regions(vec![sloped_quad(0.0, 0.0, 10.0, 6.0, 2.0, 0.2)]);
        // Roof over the footprint: 2.4 (y=2) → 2.8 (y=4). A top of 2.6 clears the
        // downhill edge but is BELOW the roof at the uphill edge.
        let fp = vec![pt(3.0, 2.0), pt(6.0, 2.0), pt(6.0, 4.0), pt(3.0, 4.0)];
        assert_eq!(
            validate_dormer(&storey, &fp, 2.6).unwrap_err(),
            DORMER_TOP_BELOW_ROOF_MSG
        );
    }

    /// A dormer poking outside the roof (a footprint vertex past the region /
    /// outline) is rejected with the outside-ceiling message.
    #[test]
    fn dormer_poking_outside_the_outline_is_rejected() {
        let storey = storey_with_regions(vec![sloped_quad(0.0, 0.0, 10.0, 6.0, 2.0, 0.2)]);
        // Centre at x=9.5 is inside, but the right corners at x=11 fall past the
        // region's outer wall (x=10).
        let fp = vec![pt(8.0, 2.0), pt(11.0, 2.0), pt(11.0, 4.0), pt(8.0, 4.0)];
        assert_eq!(
            validate_dormer(&storey, &fp, 4.0).unwrap_err(),
            DORMER_OUTSIDE_CEILING_MSG
        );
        // No ceilings at all → also outside.
        assert_eq!(
            validate_dormer(&Storey::default(), &fp, 4.0).unwrap_err(),
            DORMER_OUTSIDE_CEILING_MSG
        );
    }

    /// The review's DISCONTINUOUS-STEP gap: region A slopes up to 3.0 at the
    /// seam x = 5, region B is flat at 2.6. A dormer spanning both must see the
    /// roof max as A's 3.0 (the seam's higher side), so a top of 2.7 is REJECTED
    /// (inverted riser) and 3.1 accepted — regardless of region order. The old
    /// `top_z_at` seam sampling could return B's 2.6 and wrongly accept 2.7.
    #[test]
    fn dormer_over_a_discontinuous_step_reads_the_higher_side() {
        // A: x∈[0,5], z = 1.0 + 0.4·x → 3.0 at the seam x = 5.
        let mk_a = || CeilingRegion {
            id: Default::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 1.0),
                Point3f64::new(5.0, 0.0, 3.0),
                Point3f64::new(5.0, 6.0, 3.0),
                Point3f64::new(0.0, 6.0, 1.0),
            ]),
            opes: Default::default(),
            construction_id: Default::default(),
            exposure: Default::default(),
        };
        // B: x∈[5,10], flat at 2.6.
        let mk_b = || flat_quad(5.0, 0.0, 10.0, 6.0, 2.6);
        let fp = vec![pt(3.0, 2.0), pt(7.0, 2.0), pt(7.0, 4.0), pt(3.0, 4.0)];
        for regions in [vec![mk_a(), mk_b()], vec![mk_b(), mk_a()]] {
            let storey = storey_with_regions(regions);
            // 2.7 sits below A's 3.0 at the seam — inverted riser, rejected.
            assert_eq!(
                validate_dormer(&storey, &fp, 2.7).unwrap_err(),
                DORMER_TOP_BELOW_ROOF_MSG,
                "the step's higher side (3.0) binds, not B's 2.6"
            );
            // 3.1 clears the whole footprint.
            assert!(validate_dormer(&storey, &fp, 3.1).is_ok());
        }
    }

    /// A degenerate footprint (too few corners, or zero plan area) and a
    /// non-finite / non-positive top height are rejected up front.
    #[test]
    fn degenerate_footprint_and_bad_height_are_rejected() {
        let storey = storey_with_regions(vec![sloped_quad(0.0, 0.0, 10.0, 6.0, 2.0, 0.2)]);
        // Fewer than three corners.
        assert_eq!(
            validate_dormer(&storey, &[pt(3.0, 2.0), pt(4.0, 2.0)], 3.0).unwrap_err(),
            DORMER_DEGENERATE_MSG
        );
        // Three COLLINEAR corners → zero area.
        assert_eq!(
            validate_dormer(&storey, &[pt(3.0, 2.0), pt(4.0, 2.0), pt(5.0, 2.0)], 3.0).unwrap_err(),
            DORMER_DEGENERATE_MSG
        );
        // Non-finite / non-positive top height.
        let fp = vec![pt(3.0, 2.0), pt(6.0, 2.0), pt(6.0, 4.0), pt(3.0, 4.0)];
        for z in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            let err = validate_dormer(&storey, &fp, z).unwrap_err();
            assert!(err.contains("positive"), "z = {z} got: {err}");
        }
    }
}
