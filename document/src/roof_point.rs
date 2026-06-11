//! Roof-point (apex) validation: a declared high point fans the ceiling
//! region containing it up (or down) to an absolute height.
//!
//! The apex itself is DURABLE input (`Storey::roof_apexes`, applied by the
//! resolve projection AFTER the ceiling-step areas — composition order is
//! law: default ceiling → steps → apexes). What lives here is the two
//! assign-time validations the shell runs before `Ber::add_roof_apex`:
//!
//! - [`StoreyDoc::validate_roof_point`] — the cheap tap-time check against
//!   the OUTLINE (no `Ber` needed): the point must be strictly inside the
//!   footprint bbox on both axes, exactly the per-axis test the ceiling-step
//!   line uses.
//! - [`validate_roof_apex`] — the materialized check against the storey's
//!   RESOLVED roofs: strict containment in one ceiling region
//!   (`region_strictly_containing` — deliberately stricter than the
//!   boundary-inclusive predicate `with_apexes` re-applies with), the region
//!   not already sloped (v1: one high point per ceiling area), and a height
//!   that actually slopes it (≥ 1 cm off the region's base).

use crate::StoreyDoc;
use datatypes::ber::structural::Storey;
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

/// User-facing rejection for an apex in a region that already slopes (v1
/// limit). Kept stable.
const REGION_ALREADY_SLOPED_MSG: &str =
    "That ceiling area already slopes — one high point per ceiling area for now.";

/// An apex within this (m) of its region's base height slopes nothing.
const MIN_APEX_DELTA_M: f64 = 0.01;

/// Validate a roof apex against the storey's RESOLVED roofs (the assign-time
/// check, run on the materialized `Storey` before `Ber::add_roof_apex`):
///
/// - `z_m` must be finite and > 0 (it's ABSOLUTE height above the storey's
///   floor datum, not a delta). Below-base dips are allowed — warn-not-reject
///   lives in the resolve projection, not here.
/// - `p` must be STRICTLY inside one ceiling region
///   ([`region_strictly_containing`] — an apex on a boundary or the outline
///   is rejected up front; re-application uses the boundary-INCLUSIVE
///   predicate instead, two deliberately different predicates).
/// - That region must be FLAT (`flat_z()` is `Some`): v1 rejects a second
///   apex in an already-sloped region.
/// - `|z_m − base|` must be ≥ 1 cm — an apex at the ceiling's own height
///   slopes nothing.
///
/// `Err` is user-facing.
///
/// [`region_strictly_containing`]:
/// datatypes::ber::structural::CeilingRegions::region_strictly_containing
pub fn validate_roof_apex(storey: &Storey, p: Point2f64, z_m: f64) -> Result<(), String> {
    if !z_m.is_finite() || z_m <= 0.0 {
        return Err(format!(
            "height must be a positive number of metres (got {z_m})"
        ));
    }
    let region = storey
        .roofs
        .region_strictly_containing(p)
        .ok_or_else(|| POINT_OUTSIDE_CEILING_MSG.to_string())?;
    let base = region
        .flat_z()
        .ok_or_else(|| REGION_ALREADY_SLOPED_MSG.to_string())?;
    if (z_m - base).abs() < MIN_APEX_DELTA_M {
        return Err(format!(
            "{z_m} m is within a centimetre of that ceiling area's height ({base} m) — \
             enter a height that actually slopes it."
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionState, ShapeSlot};
    use datatypes::ber::structural::{CeilingRegions, RoofApex};
    use manip_2d_plan::FloorPlan;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A Storey whose roofs are one flat 4×4 region at the given height.
    fn storey_with_flat_roof(height: f64) -> Storey {
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        Storey {
            roofs: CeilingRegions::default_for_footprint(vec![ring], height, Default::default()),
            ceiling_height_m: Some(height),
            ..Storey::default()
        }
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

    /// v1: a second apex inside an already-sloped (fanned) region is rejected
    /// with the stable one-per-area message.
    #[test]
    fn apex_in_an_already_sloped_region_is_rejected() {
        let mut storey = storey_with_flat_roof(2.4);
        let first = RoofApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.2,
        };
        storey.roofs = storey.roofs.with_apexes(&[first]);
        // Strictly inside one of the fan triangles, not on its boundary.
        let err = validate_roof_apex(&storey, Point2f64::new(2.0, 0.5), 3.0).unwrap_err();
        assert_eq!(err, REGION_ALREADY_SLOPED_MSG);
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

    /// validate_roof_point: strictly inside the outline bbox on BOTH axes;
    /// edges, outside points, and an empty outline get the point-flavoured
    /// footprint message.
    #[test]
    fn roof_point_must_be_strictly_inside_the_outline_bbox() {
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
}
