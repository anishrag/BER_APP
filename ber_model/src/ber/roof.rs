//! The dwelling ROOF — a structure SEPARATE from the habitable-area ceiling.
//!
//! Distinct from the ceiling (`Storey::ceilings` / the roof-point apexes, which
//! shape the heat-loss ceiling at the top of a storey): the roof here is the
//! external structure that sits ABOVE the top storey, on its own plane(s). It is
//! only modelled when it matters for the assessment:
//!
//! - an attic insulated at BOTH the ceiling/attic-floor AND the rafters (so the
//!   roof is a second thermal surface, not the same one as the ceiling), and
//! - solar panels / solar water heaters mounted on the roof (the plane's area,
//!   pitch and orientation drive their contribution).
//!
//! The roof does NOT change the ceiling geometry. It is built area-by-area: the
//! assessor groups top-storey ceiling areas, picks a shape, draws a ridge and
//! gives a pitch; each becomes a [`RoofArea`] here, from which the 3-D roof
//! planes are derived (resolve), eaves at the covered storey's ceiling height.
use serde::{Deserialize, Serialize};

use crate::ber::common::ItemId;
use crate::ber::structural::{CeilingRegions, CeilingApex};
use domain::points::Point2f64;

/// The shape of one roof area's planes. `Flat` keeps the roof level at the eave
/// (no ridge); `Gable` rises to a ridge that reaches the end walls (vertical
/// gable ends); `Hip` rises to a ridge inset from the ends (the ends slope down
/// too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoofShape {
    Flat,
    Gable,
    Hip,
}

/// One roof area over a group of top-storey ceiling areas: the durable INPUT the
/// assessor drew (shape + pitch + ridge + the covered footprint), from which the
/// 3-D roof planes are derived on resolve. The eave sits at `eave_z` (the covered
/// storey's ceiling height, absolute above its floor datum); the ridge rises from
/// there per `pitch_deg`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoofArea {
    pub id: ItemId,
    /// The storey this roof sits on (its top ceiling) — for the eave height and
    /// the dual-insulation pairing with that storey's ceiling.
    pub storey_idx: usize,
    pub shape: RoofShape,
    pub pitch_deg: f32,
    /// Absolute eave height in metres above the covered storey's floor datum
    /// (= that storey's ceiling height).
    pub eave_z: f64,
    /// The ridge endpoints (plan, world metres); `None` for a flat roof.
    pub ridge: Option<(Point2f64, Point2f64)>,
    /// The grouped ceiling-area rings this roof covers (plan, world metres, no
    /// closing duplicate). The derived planes span their union (plus overhang).
    pub footprint: Vec<Vec<Point2f64>>,
}

impl RoofArea {
    /// The footprint's axis-aligned bounding box (plan, world metres), or `None`
    /// if degenerate.
    fn footprint_bbox(&self) -> Option<(f64, f64, f64, f64)> {
        let mut bb: Option<(f64, f64, f64, f64)> = None;
        for ring in &self.footprint {
            for p in ring {
                bb = Some(match bb {
                    None => (p.x, p.y, p.x, p.y),
                    Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
                });
            }
        }
        bb.filter(|(x0, y0, x1, y1)| x1 - x0 > 1e-6 && y1 - y0 > 1e-6)
    }

    /// The roof apexes derived from the ridge + pitch — the ridge endpoints, each
    /// raised to `eave_z + perp_half · tan(pitch)` (`perp_half` = half the
    /// footprint span perpendicular to the ridge). Empty for a flat roof (or no
    /// ridge): the roof stays level at the eave. These feed the SAME apex/ridge
    /// fan the ceiling uses ([`CeilingRegions::with_apexes`]).
    pub fn apexes(&self) -> Vec<CeilingApex> {
        let Some((a, b)) = self.ridge.filter(|_| self.shape != RoofShape::Flat) else {
            return Vec::new();
        };
        let Some((x0, y0, x1, y1)) = self.footprint_bbox() else {
            return Vec::new();
        };
        let ridge_along_y = (a.x - b.x).abs() <= (a.y - b.y).abs();
        let perp_half = if ridge_along_y {
            (x1 - x0) / 2.0
        } else {
            (y1 - y0) / 2.0
        };
        let z = self.eave_z + perp_half * (self.pitch_deg as f64).to_radians().tan();
        vec![CeilingApex { point: a, z_m: z }, CeilingApex { point: b, z_m: z }]
    }

    /// The roof as `CeilingRegions` — built with the SAME machinery as the ceiling
    /// ([`CeilingRegions::default_for_footprint`] over the grouped footprint at the
    /// eave, then [`with_apexes`](CeilingRegions::with_apexes) for the ridge). The
    /// footprint rings ARE the covered ceiling regions' rings, so the roof shares
    /// the ceiling's plan vertices — and when the roof is flat at the ceiling level
    /// it coincides with the ceiling exactly. The z is LOCAL to the covered
    /// storey's floor datum (the caller adds the datum for world z). `Flat` →
    /// level regions at the eave; `Gable`/`Hip` → fanned to the ridge.
    pub fn regions(&self, construction_id: ItemId) -> CeilingRegions {
        CeilingRegions::default_for_footprint(self.footprint.clone(), self.eave_z, construction_id)
            .with_apexes(&self.apexes())
    }
}

/// The dwelling roof: the set of roof areas the assessor drew. Lives on
/// [`Ber`](crate::ber::ber_struct::Ber) — one roof per dwelling, separate from
/// every storey's ceiling.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Roof {
    pub areas: Vec<RoofArea>,
}

impl Roof {
    /// True when no roof area has been drawn yet.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_10() -> Vec<Vec<Point2f64>> {
        vec![vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ]]
    }

    fn z_extent(regions: &CeilingRegions) -> (f64, f64) {
        let zs: Vec<f64> = regions
            .regions
            .iter()
            .flat_map(|r| r.polygon.exterior.iter().map(|p| p.z))
            .collect();
        (
            zs.iter().cloned().fold(f64::MAX, f64::min),
            zs.iter().cloned().fold(f64::MIN, f64::max),
        )
    }

    #[test]
    fn gable_regions_rise_from_eave_to_ridge_via_the_ceiling_fan() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            // Ridge along Y at x = 5 (slopes fall in X).
            ridge: Some((Point2f64::new(5.0, 0.0), Point2f64::new(5.0, 10.0))),
            footprint: square_10(),
        };
        // Apex height: eave 2.4 + half-span 5 · tan(45°) = 7.4.
        let apexes = area.apexes();
        assert_eq!(apexes.len(), 2, "a ridge is two apexes");
        assert!(apexes.iter().all(|a| (a.z_m - 7.4).abs() < 1e-6));
        // Regions are built by the SHARED ceiling fan; z runs eave→ridge.
        let regions = area.regions(ItemId::default());
        assert!(!regions.regions.is_empty(), "the fan produced roof faces");
        let (zmin, zmax) = z_extent(&regions);
        assert!((zmin - 2.4).abs() < 1e-6, "eave at 2.4, got {zmin}");
        assert!((zmax - 7.4).abs() < 1e-6, "ridge at 7.4, got {zmax}");
    }

    #[test]
    fn flat_regions_are_level_at_the_eave_and_share_the_footprint() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.6,
            ridge: None,
            footprint: square_10(),
        };
        assert!(area.apexes().is_empty(), "a flat roof has no apexes");
        let regions = area.regions(ItemId::default());
        // Flat = the footprint regions at the eave (shares the ceiling's vertices).
        assert_eq!(regions.regions.len(), 1);
        let (zmin, zmax) = z_extent(&regions);
        assert!(
            (zmin - 2.6).abs() < 1e-9 && (zmax - 2.6).abs() < 1e-9,
            "level"
        );
        // The plan ring matches the footprint exactly (shared vertices).
        let xy: Vec<(f64, f64)> = regions.regions[0]
            .polygon
            .exterior
            .iter()
            .map(|p| (p.x, p.y))
            .collect();
        for v in &square_10()[0] {
            assert!(
                xy.iter()
                    .any(|(x, y)| (x - v.x).abs() < 1e-9 && (y - v.y).abs() < 1e-9),
                "footprint vertex {v:?} is shared by the flat roof"
            );
        }
    }
}
