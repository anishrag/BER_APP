//! Derived JUNCTION-MISMATCH check: the pure, read-only companion of
//! [`derive_inter_storey`] that reports where a storey's floor does NOT sit
//! on the ceiling below it — the physically impossible configurations the
//! junction derive cannot bury (a floor stepped BELOW the ceiling it rests
//! on mints degenerate wall slivers and churns settled exposure back to
//! `Unset`; field case "56 Elmwood Road").
//!
//! Z-ALIGNMENT CONVENTION (shared with `derive_inter_storey` — do not fork
//! it): absolute z comes from [`Ber::storey_floor_datums`]; a floor region of
//! storey `i` has its walking surface at `datum_i + level_m` and its slab
//! UNDERSIDE at `datum_i + level_m − floor_thickness_m` (the datum law makes
//! `datum_i − slab_i` equal the below storey's lowest covered flat ceiling —
//! exactly `derive_inter_storey`'s wall-band `band_lo`); a ceiling region of
//! storey `i − 1` sits at `datum_{i−1} + flat_z()`. A junction is ALIGNED
//! when the floor underside meets the ceiling surface; the mismatch epsilon
//! is [`RISER_Z_EPS_M`], the same z-continuity epsilon the junction derive
//! (and the [`CeilingRegions::inherited_floor_levels`] seeding) uses.

use geo::bool_ops::BooleanOps;
use geo::{Area, Centroid, Contains, InteriorPoint, Point as GeoPoint, Polygon};

use crate::ber::ber_struct::Ber;
use domain::points::Point2f64;

#[allow(unused_imports)]
use super::*;

/// One plan zone where a storey's floor and the ceiling below it disagree in
/// absolute z by more than [`RISER_Z_EPS_M`]. World metres.
#[derive(Debug, Clone)]
pub struct JunctionMismatch {
    /// The UPPER storey — the one whose floor is misaligned.
    pub storey_idx: usize,
    /// The storey below (`storey_idx − 1`), whose ceiling the floor should
    /// rest on.
    pub below_idx: usize,
    /// Open plan ring of the mismatching zone (floor region ∩ ceiling
    /// region).
    pub ring: Vec<Point2f64>,
    /// A point guaranteed inside the zone (label anchor / claim probe).
    pub label_anchor: Point2f64,
    /// Zone plan area (m²).
    pub area_m2: f64,
    /// Signed gap (m): floor underside − ceiling surface. NEGATIVE = the
    /// floor sits BELOW the ceiling it rests on (the dangerous overlap
    /// case); POSITIVE = the floor floats above it (a void gap).
    pub gap_m: f64,
}

/// Find every junction mismatch across the resolved stack: for each storey
/// `i > 0`, intersect its floor regions with the below storey's FLAT ceiling
/// regions and report the pieces whose absolute-z gap exceeds
/// [`RISER_Z_EPS_M`]. Sloped ceiling regions contribute nothing — the same
/// skip [`CeilingRegions::inherited_floor_levels`] applies (sloped-under-
/// floor is later ceilings work). Degenerate pieces are dropped with the
/// junction derive's own plan-sliver guard ([`plan_piece_is_degenerate`]:
/// sub-[`SLIVER_AREA_M2`] area or a sub-mm minor dimension).
///
/// GATE (`dimensioned`): a pair is checked only when BOTH storeys are
/// dimensioned — the same rule as `derive_inter_storey` (a provisional
/// footprint still moves, so a mismatch against it would be noise). An index
/// past the slice end counts as provisional. Pure — no mutation.
pub fn junction_floor_ceiling_mismatches(ber: &Ber, dimensioned: &[bool]) -> Vec<JunctionMismatch> {
    let mut out = Vec::new();
    let n = ber.storeys.len();
    if n < 2 {
        return out;
    }
    let dim = |i: usize| dimensioned.get(i).copied().unwrap_or(false);
    let datums = ber.storey_floor_datums();
    for i in 1..n {
        if !(dim(i) && dim(i - 1)) {
            continue; // gate: check only settled junctions
        }
        let below = &ber.storeys[i - 1];
        let above = &ber.storeys[i];
        let slab = above.floor_thickness_m.unwrap_or(0.0);
        for fr in &above.floors.regions {
            // The slab UNDERSIDE — what rests on the below ceiling.
            let underside = datums[i] + fr.level_m - slab;
            for cr in &below.ceilings.regions {
                let Some(z) = cr.flat_z() else {
                    continue; // sloped piece — skipped like inherited_floor_levels
                };
                let gap = underside - (datums[i - 1] + z);
                if gap.abs() <= RISER_Z_EPS_M {
                    continue; // aligned junction
                }
                let c_poly: Polygon<f64> = cr.polygon.clone().into();
                for poly in fr
                    .polygon
                    .intersection(&c_poly)
                    .0
                    .iter()
                    .flat_map(split_to_simple)
                {
                    if plan_piece_is_degenerate(&poly) {
                        continue; // graze / boolean-op sliver
                    }
                    let Some(anchor) = interior_anchor(&poly) else {
                        continue;
                    };
                    out.push(JunctionMismatch {
                        storey_idx: i,
                        below_idx: i - 1,
                        ring: open_ring(&poly),
                        label_anchor: anchor,
                        area_m2: poly.unsigned_area(),
                        gap_m: gap,
                    });
                }
            }
        }
    }
    out
}

/// A resolved floor-step zone of storey `idx`, projected for plan DISPLAY:
/// its plan ring, a guaranteed-interior label anchor, the floor level, and —
/// when a [`JunctionMismatch`] falls inside it — the mismatch gap. World
/// metres; the shell formats labels and draws.
#[derive(Debug, Clone)]
pub struct FloorStepZoneDisplay {
    /// Open plan ring of the zone.
    pub ring: Vec<Point2f64>,
    /// A point inside the zone for its label.
    pub label_anchor: Point2f64,
    /// The zone's floor level (m from the storey datum; 0 for a pure
    /// mismatch zone on an unstepped floor region).
    pub level_m: f64,
    /// `Some(gap)` when the junction check flagged this zone — the signed
    /// gap of the worst (largest-magnitude) mismatch inside it (see
    /// [`JunctionMismatch::gap_m`] for the sign convention).
    pub gap_m: Option<f64>,
}

/// Storey `idx`'s floor-step zones for plan display: one zone per RESOLVED
/// floor region with `level_m ≠ 0` (the durable record of the step areas —
/// same source as `step_regions_from_floors` in the document layer), each
/// carrying the gap of any [`JunctionMismatch`] whose anchor falls inside it
/// (the mismatch piece is floor ∩ ceiling, so its interior lies in exactly
/// one floor region). A flagged mismatch on an UNSTEPPED (level-0) region
/// still surfaces — as a zone of its own ring — so a misaligned junction is
/// visible even where no step was drawn. Level-0 regions with no mismatch
/// are skipped. Pure projection — no mutation.
pub fn storey_floor_step_zones(
    ber: &Ber,
    idx: usize,
    mismatches: &[JunctionMismatch],
) -> Vec<FloorStepZoneDisplay> {
    let Some(storey) = ber.storeys.get(idx) else {
        return Vec::new();
    };
    let mine: Vec<&JunctionMismatch> = mismatches.iter().filter(|m| m.storey_idx == idx).collect();
    let mut claimed = vec![false; mine.len()];
    let mut out = Vec::new();
    for fr in &storey.floors.regions {
        // The worst mismatch inside this region claims it.
        let mut gap: Option<f64> = None;
        for (k, m) in mine.iter().enumerate() {
            if claimed[k] {
                continue;
            }
            let p = GeoPoint::new(m.label_anchor.x, m.label_anchor.y);
            if fr.polygon.contains(&p) {
                claimed[k] = true;
                if gap.is_none_or(|g: f64| m.gap_m.abs() > g.abs()) {
                    gap = Some(m.gap_m);
                }
            }
        }
        if (fr.level_m == 0.0 && gap.is_none()) || fr.polygon.unsigned_area() < SLIVER_AREA_M2 {
            continue;
        }
        let Some(anchor) = interior_anchor(&fr.polygon) else {
            continue;
        };
        out.push(FloorStepZoneDisplay {
            ring: open_ring(&fr.polygon),
            label_anchor: anchor,
            level_m: fr.level_m,
            gap_m: gap,
        });
    }
    // Belt: a mismatch whose anchor landed in no region (degenerate ring
    // noise) still surfaces on its own piece rather than vanishing.
    for (k, m) in mine.iter().enumerate() {
        if !claimed[k] {
            out.push(FloorStepZoneDisplay {
                ring: m.ring.clone(),
                label_anchor: m.label_anchor,
                level_m: 0.0,
                gap_m: Some(m.gap_m),
            });
        }
    }
    out
}

/// A guaranteed-interior point of `poly` (geo `InteriorPoint`, centroid
/// fallback — the `storey_phase_zone_polygons` idiom). `None` for a
/// degenerate polygon.
fn interior_anchor(poly: &Polygon<f64>) -> Option<Point2f64> {
    poly.interior_point()
        .or_else(|| poly.centroid())
        .map(|p| Point2f64::new(p.x(), p.y()))
}

/// `poly`'s exterior as an OPEN ring (closing duplicate dropped).
fn open_ring(poly: &Polygon<f64>) -> Vec<Point2f64> {
    let mut pts: Vec<Point2f64> = poly
        .exterior()
        .coords()
        .map(|c| Point2f64::new(c.x, c.y))
        .collect();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    pts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::common::ItemId;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(x0, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0, y1),
        ]
    }

    /// The "56 Elmwood Road" split-level fixture: ground storey 8 × 4 with
    /// its main ceiling at 2.548 and the left-rear zone (x 0.66–3.04,
    /// y 0.9–4.0 → 7.378 m²) stepped down to 1.958; upper storey over the
    /// same footprint, slab 0.2, floors seeded the way
    /// `inherited_floor_levels` does: the remainder (over the MAIN ceiling,
    /// the largest area) is the datum at level 0, and the zone over the low
    /// ceiling drops to −0.59.
    fn elmwood() -> Ber {
        let cid = ItemId::default();
        let footprint = square(0.0, 0.0, 8.0, 4.0);
        let zone = square(0.66, 0.9, 3.04, 4.0);
        // Remainder = footprint minus the zone (an L-ring around it).
        let p = Point2f64::new;
        let remainder = vec![
            p(0.0, 0.0),
            p(8.0, 0.0),
            p(8.0, 4.0),
            p(3.04, 4.0),
            p(3.04, 0.9),
            p(0.66, 0.9),
            p(0.66, 4.0),
            p(0.0, 4.0),
        ];

        let ground = Storey {
            floors: FloorRegions::new(footprint.clone(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![footprint], 2.548, cid)
                .with_z_deltas(&[(zone.clone(), -0.59)]),
            ceiling_height_m: Some(2.548),
            ..Storey::default()
        };
        let mut upper_floors = FloorRegions::from_vertex_lists(vec![zone, remainder], cid);
        upper_floors.regions[0].level_m = -0.59; // over the LOW ceiling — below the datum
        upper_floors.regions[1].level_m = 0.0; // over the main ceiling — the datum
        let upper = Storey {
            floors: upper_floors,
            ceilings: CeilingRegions::default_for_footprint(
                vec![square(0.0, 0.0, 8.0, 4.0)],
                2.4,
                cid,
            ),
            ceiling_height_m: Some(2.4),
            floor_thickness_m: Some(0.2),
            ..Storey::default()
        };
        let mut ber = Ber::default();
        ber.storeys = vec![ground, upper];
        ber
    }

    /// Correctly seeded levels (main 0, low zone −0.59) align both junction
    /// zones exactly — no mismatch. THE negative-step check: a floor stepping
    /// DOWN off its datum lands on its ceiling as exactly as one stepping up
    /// did under the old lowest-anchor convention.
    #[test]
    fn elmwood_seeded_levels_align_no_mismatch() {
        let ber = elmwood();
        let ms = junction_floor_ceiling_mismatches(&ber, &[true, true]);
        assert!(ms.is_empty(), "seeded junction is aligned: {ms:?}");
    }

    /// The field damage: the low zone's floor hand-stepped 0.247 m past its
    /// correct −0.59 puts its underside 0.247 m BELOW the low ceiling it rests
    /// on → exactly one mismatch zone, negative (overlap) gap, on the
    /// stepped-zone side.
    #[test]
    fn elmwood_hand_stepped_floor_flags_the_overlap_zone() {
        let mut ber = elmwood();
        ber.storeys[1].floors.regions[0].level_m = -0.837;
        let ms = junction_floor_ceiling_mismatches(&ber, &[true, true]);
        assert_eq!(ms.len(), 1, "one mismatching zone: {ms:?}");
        let m = &ms[0];
        assert_eq!((m.storey_idx, m.below_idx), (1, 0));
        assert!(
            (m.gap_m + 0.247).abs() < 1e-9,
            "floor 0.247 below the ceiling: {}",
            m.gap_m
        );
        assert!((m.area_m2 - 7.378).abs() < 1e-6, "zone area: {}", m.area_m2);
        // The ring is the stepped zone, not the remainder (f32-quantized
        // ceiling rings put vertices a few 1e-8 m off the exact bounds).
        const Q: f64 = 1e-6;
        for v in &m.ring {
            assert!(
                (0.66 - Q..=3.04 + Q).contains(&v.x) && (0.9 - Q..=4.0 + Q).contains(&v.y),
                "ring stays on the stepped-zone side: {v:?}"
            );
        }
        assert!(
            (0.66..3.04).contains(&m.label_anchor.x),
            "anchor inside the zone"
        );
    }

    /// A floor stepped UP off its ceiling reports a positive (void) gap.
    #[test]
    fn floor_above_ceiling_flags_a_void_gap() {
        let mut ber = elmwood();
        ber.storeys[1].floors.regions[0].level_m = -0.29; // 0.3 above its correct −0.59
        let ms = junction_floor_ceiling_mismatches(&ber, &[true, true]);
        assert_eq!(ms.len(), 1);
        assert!(
            (ms[0].gap_m - 0.3).abs() < 1e-9,
            "void gap: {}",
            ms[0].gap_m
        );
    }

    /// Sub-epsilon misalignment (float noise, not a real step) is clean —
    /// the same [`RISER_Z_EPS_M`] the junction derive treats as continuous.
    #[test]
    fn alignment_within_tolerance_is_clean() {
        let mut ber = elmwood();
        ber.storeys[1].floors.regions[0].level_m = -0.59 + 0.5e-4;
        assert!(junction_floor_ceiling_mismatches(&ber, &[true, true]).is_empty());
    }

    /// The dimensioned gate: an un-dimensioned side skips the pair (same
    /// rule as `derive_inter_storey` — provisional geometry still moves).
    #[test]
    fn undimensioned_pair_is_skipped() {
        let mut ber = elmwood();
        ber.storeys[1].floors.regions[0].level_m = -0.837;
        assert!(junction_floor_ceiling_mismatches(&ber, &[true, false]).is_empty());
        assert!(junction_floor_ceiling_mismatches(&ber, &[false, true]).is_empty());
        assert!(junction_floor_ceiling_mismatches(&ber, &[true]).is_empty());
    }

    /// Display zones: every stepped region shows; the flagged one carries the
    /// gap; the level-0 datum region without a mismatch is skipped.
    #[test]
    fn step_zones_carry_levels_and_the_flagged_gap() {
        // Seeded (aligned): only the −0.59 zone displays, no gap — an UNflagged
        // stepped region still shows.
        let ber = elmwood();
        let ms = junction_floor_ceiling_mismatches(&ber, &[true, true]);
        let zones = storey_floor_step_zones(&ber, 1, &ms);
        assert_eq!(zones.len(), 1, "only the stepped region displays");
        assert!((zones[0].level_m + 0.59).abs() < 1e-9);
        assert!(zones[0].gap_m.is_none());
        // Below storey: no steps, nothing to display.
        assert!(storey_floor_step_zones(&ber, 0, &ms).is_empty());

        // Hand-stepped the DATUM region up 0.3: it now displays too, flagged,
        // alongside the correctly-stepped −0.59 zone.
        let mut ber = elmwood();
        ber.storeys[1].floors.regions[1].level_m = 0.3;
        let ms = junction_floor_ceiling_mismatches(&ber, &[true, true]);
        let zones = storey_floor_step_zones(&ber, 1, &ms);
        assert_eq!(zones.len(), 2, "both stepped regions display");
        let flagged: Vec<_> = zones.iter().filter(|z| z.gap_m.is_some()).collect();
        assert_eq!(flagged.len(), 1, "exactly the mismatching zone is flagged");
        assert!((flagged[0].level_m - 0.3).abs() < 1e-9);
        assert!((flagged[0].gap_m.unwrap() - 0.3).abs() < 1e-9);
    }
}
