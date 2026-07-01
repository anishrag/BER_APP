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
use crate::ber::structural::{CeilingApex, CeilingRegions};
use domain::points::Point2f64;

/// Default eave overhang in metres — the roof oversails the walls by this much
/// (the sloped faces continue past the wall, so the overhang eave dips BELOW the
/// wall-top). Drawing-only; ceiling insulation is still assumed full-coverage.
pub const DEFAULT_OVERHANG_M: f64 = 0.3;

/// Offset a plan ring OUTWARD by `d` metres (a miter offset along each vertex's
/// outward edge-normal bisector). Approximate — for the drawing only. `d <= 0` or
/// a degenerate ring returns the ring unchanged.
fn offset_ring_outward(ring: &[Point2f64], d: f64) -> Vec<Point2f64> {
    let n = ring.len();
    if n < 3 || d <= 0.0 {
        return ring.to_vec();
    }
    // Signed area → orientation (outward normal side depends on it).
    let area2: f64 = (0..n)
        .map(|i| {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            a.x * b.y - b.x * a.y
        })
        .sum();
    let ccw = area2 > 0.0;
    let unit = |dx: f64, dy: f64| {
        let l = (dx * dx + dy * dy).sqrt();
        if l < 1e-12 {
            (0.0, 0.0)
        } else {
            (dx / l, dy / l)
        }
    };
    // Outward normal of edge direction (dx,dy): right of travel for CCW rings.
    let outward = |dx: f64, dy: f64| {
        let (ux, uy) = unit(dx, dy);
        if ccw {
            (uy, -ux)
        } else {
            (-uy, ux)
        }
    };
    (0..n)
        .map(|i| {
            let prev = ring[(i + n - 1) % n];
            let cur = ring[i];
            let next = ring[(i + 1) % n];
            let n_in = outward(cur.x - prev.x, cur.y - prev.y);
            let n_out = outward(next.x - cur.x, next.y - cur.y);
            let (mx, my) = unit(n_in.0 + n_out.0, n_in.1 + n_out.1);
            // Miter length = 1/cos(half-angle); clamp so sharp corners don't spike.
            let cos = (mx * n_out.0 + my * n_out.1).max(0.25);
            let f = (1.0 / cos).min(3.0);
            Point2f64::new(cur.x + d * f * mx, cur.y + d * f * my)
        })
        .collect()
}

/// The shape of one roof area's planes. `Flat` keeps the roof level at the eave
/// (no ridge); `Gable` rises to a ridge that reaches the end walls (vertical
/// gable ends); `Hip` rises to a ridge inset from the ends (the ends slope down
/// too); `FollowCeiling` adopts the covered storey's already-sloped CEILING (a
/// converted attic / room-in-roof: the rafters ARE the ceiling, one surface —
/// so the roof reuses the ceiling's apexes instead of a freshly-drawn pitch).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoofShape {
    Flat,
    Gable,
    Hip,
    FollowCeiling,
}

/// One roof area over a group of top-storey ceiling areas: the durable INPUT the
/// assessor drew (shape + the covered footprint + the apexes that shape it), from
/// which the 3-D roof planes are derived. The eave sits at `eave_z` (the covered
/// storey's ceiling height, absolute above its floor datum). The `apexes` are the
/// geometry driver — the same apex/ridge fan the ceiling uses — populated at
/// commit from the drawn ridge + pitch (`Gable`/`Hip`), the covered storey's
/// ceiling apexes (`FollowCeiling`), or empty (`Flat`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoofArea {
    pub id: ItemId,
    /// The storey this roof sits on (its top ceiling) — for the eave height and
    /// the dual-insulation pairing with that storey's ceiling.
    pub storey_idx: usize,
    pub shape: RoofShape,
    /// The drawn pitch in degrees (`Gable`/`Hip`); 0 for `Flat`/`FollowCeiling`.
    /// Descriptive — the geometry is driven by `apexes`.
    pub pitch_deg: f32,
    /// Absolute eave height in metres above the covered storey's floor datum
    /// (= that storey's ceiling height).
    pub eave_z: f64,
    /// The ridge endpoints (plan, world metres); `None` for flat / follow-ceiling.
    /// Descriptive — the geometry is driven by `apexes`.
    pub ridge: Option<(Point2f64, Point2f64)>,
    /// The grouped ceiling-area rings this roof covers (plan, world metres, no
    /// closing duplicate). The derived planes span their union (plus overhang).
    pub footprint: Vec<Vec<Point2f64>>,
    /// The apexes that shape the roof over `footprint` — the SAME apex/ridge fan
    /// the ceiling uses. Empty = flat at the eave.
    #[serde(default)]
    pub apexes: Vec<CeilingApex>,
}

impl RoofArea {
    /// The bounding box (plan, world metres) of the given footprint rings, or
    /// `None` if degenerate.
    fn footprint_bbox(footprint: &[Vec<Point2f64>]) -> Option<(f64, f64, f64, f64)> {
        let mut bb: Option<(f64, f64, f64, f64)> = None;
        for ring in footprint {
            for p in ring {
                bb = Some(match bb {
                    None => (p.x, p.y, p.x, p.y),
                    Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
                });
            }
        }
        bb.filter(|(x0, y0, x1, y1)| x1 - x0 > 1e-6 && y1 - y0 > 1e-6)
    }

    /// The two apexes a drawn ridge + pitch imply — the ridge endpoints raised to
    /// `eave_z + perp_half · tan(pitch)` (`perp_half` = half the footprint span
    /// perpendicular to the ridge). Feeds the ceiling's [`with_apexes`] fan. Empty
    /// if the footprint is degenerate. Used at commit to populate a Gable/Hip roof
    /// area's [`apexes`](Self::apexes).
    pub fn ridge_apexes(
        ridge: (Point2f64, Point2f64),
        pitch_deg: f32,
        eave_z: f64,
        footprint: &[Vec<Point2f64>],
    ) -> Vec<CeilingApex> {
        let (a, b) = ridge;
        let Some((x0, y0, x1, y1)) = Self::footprint_bbox(footprint) else {
            return Vec::new();
        };
        let ridge_along_y = (a.x - b.x).abs() <= (a.y - b.y).abs();
        let perp_half = if ridge_along_y {
            (x1 - x0) / 2.0
        } else {
            (y1 - y0) / 2.0
        };
        let z = eave_z + perp_half * (pitch_deg as f64).to_radians().tan();
        vec![
            CeilingApex { point: a, z_m: z },
            CeilingApex { point: b, z_m: z },
        ]
    }

    /// The effective slope (rise/run) of the roof faces — the apex rise over half
    /// the footprint's shorter span (the cross-slope run). 0 when flat / no apexes.
    fn effective_slope(&self) -> f64 {
        let apex_max = self
            .apexes
            .iter()
            .map(|a| a.z_m)
            .fold(self.eave_z, f64::max);
        match Self::footprint_bbox(&self.footprint) {
            Some((x0, y0, x1, y1)) => {
                let perp_half = ((x1 - x0).min(y1 - y0)) / 2.0;
                if perp_half > 1e-6 {
                    (apex_max - self.eave_z) / perp_half
                } else {
                    0.0
                }
            }
            None => 0.0,
        }
    }

    /// The roof as `CeilingRegions` — built with the SAME machinery as the ceiling
    /// ([`CeilingRegions::default_for_footprint`] over the grouped footprint, then
    /// [`with_apexes`](CeilingRegions::with_apexes) with this area's apexes). The
    /// footprint rings ARE the covered ceiling regions' rings, so the roof shares
    /// the ceiling's plan vertices — and a `FollowCeiling` roof reproduces the
    /// sloped ceiling exactly (rafters = ceiling, one surface). The z is LOCAL to
    /// the covered storey's floor datum.
    ///
    /// `overhang_m` oversails the walls: the footprint is offset outward by that
    /// much and the fan's base is LOWERED by `overhang · slope`, so each sloped
    /// face passes through the wall-top at the wall and CONTINUES down past it —
    /// the overhang eave sits below the wall-top (it may fall within the storey
    /// for a thick-insulation attic; that's fine, it's the drawing only). A flat
    /// roof (no slope) just gets a level lip at the eave.
    pub fn regions(&self, overhang_m: f64, construction_id: ItemId) -> CeilingRegions {
        if overhang_m <= 0.0 {
            return CeilingRegions::default_for_footprint(
                self.footprint.clone(),
                self.eave_z,
                construction_id,
            )
            .with_apexes(&self.apexes);
        }
        let footprint: Vec<Vec<Point2f64>> = self
            .footprint
            .iter()
            .map(|ring| offset_ring_outward(ring, overhang_m))
            .collect();
        // Lower the offset base so the plane continues the slope through the wall.
        let base_z = self.eave_z - overhang_m * self.effective_slope();
        CeilingRegions::default_for_footprint(footprint, base_z, construction_id)
            .with_apexes(&self.apexes)
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
        let ridge = (Point2f64::new(5.0, 0.0), Point2f64::new(5.0, 10.0));
        // Apex height: eave 2.4 + half-span 5 · tan(45°) = 7.4.
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &square_10());
        assert_eq!(apexes.len(), 2, "a ridge is two apexes");
        assert!(apexes.iter().all(|a| (a.z_m - 7.4).abs() < 1e-6));
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint: square_10(),
            apexes,
        };
        // Regions are built by the SHARED ceiling fan; z runs eave→ridge.
        let regions = area.regions(0.0, ItemId::default());
        assert!(!regions.regions.is_empty(), "the fan produced roof faces");
        let (zmin, zmax) = z_extent(&regions);
        assert!((zmin - 2.4).abs() < 1e-6, "eave at 2.4, got {zmin}");
        assert!((zmax - 7.4).abs() < 1e-6, "ridge at 7.4, got {zmax}");
    }

    /// A FollowCeiling roof adopts the covered storey's ceiling apexes (converted
    /// attic): its regions REPRODUCE the sloped ceiling — same eave→ridge z range,
    /// same footprint vertices — so the roof coincides with the ceiling.
    #[test]
    fn follow_ceiling_reproduces_the_sloped_ceiling() {
        // The ceiling's own apexes (a ridge at x = 5, height 6.9).
        let ceiling_apexes = vec![
            CeilingApex {
                point: Point2f64::new(5.0, 0.0),
                z_m: 6.9,
            },
            CeilingApex {
                point: Point2f64::new(5.0, 10.0),
                z_m: 6.9,
            },
        ];
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::FollowCeiling,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: ceiling_apexes.clone(),
        };
        let roof = area.regions(0.0, ItemId::default());
        // Same as the ceiling built the same way.
        let ceiling = CeilingRegions::default_for_footprint(square_10(), 2.4, ItemId::default())
            .with_apexes(&ceiling_apexes);
        assert_eq!(
            z_extent(&roof),
            z_extent(&ceiling),
            "roof coincides with ceiling"
        );
        let (zmin, zmax) = z_extent(&roof);
        assert!((zmin - 2.4).abs() < 1e-6 && (zmax - 6.9).abs() < 1e-6);
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
            apexes: Vec::new(),
        };
        let regions = area.regions(0.0, ItemId::default());
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

    fn min_x(regions: &CeilingRegions) -> f64 {
        regions
            .regions
            .iter()
            .flat_map(|r| r.polygon.exterior.iter().map(|p| p.x))
            .fold(f64::MAX, f64::min)
    }

    /// A 0.3 m overhang on a 45° gable oversails the wall (footprint offset out)
    /// and CONTINUES the slope past it — the overhang eave dips below the wall-top
    /// eave (2.4 − 0.3·tan45 ≈ 2.1), while the ridge height is unchanged.
    #[test]
    fn overhang_continues_the_slope_below_the_wall_top() {
        let ridge = (Point2f64::new(5.0, 0.0), Point2f64::new(5.0, 10.0));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &square_10());
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint: square_10(),
            apexes,
        };
        let over = area.regions(0.3, ItemId::default());
        assert!(
            min_x(&over) < -0.2,
            "footprint offset outward past x=0: {}",
            min_x(&over)
        );
        let (zmin, zmax) = z_extent(&over);
        assert!(
            (zmin - 2.1).abs() < 0.05,
            "overhang eave dips below the 2.4 wall-top to ~2.1: {zmin}"
        );
        assert!((zmax - 7.4).abs() < 0.05, "ridge height unchanged: {zmax}");
    }

    /// A flat roof's overhang is a LEVEL lip at the eave (no slope to continue).
    #[test]
    fn flat_overhang_is_a_level_lip() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.6,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        let over = area.regions(0.3, ItemId::default());
        assert!(min_x(&over) < -0.2, "footprint offset outward");
        let (zmin, zmax) = z_extent(&over);
        assert!(
            (zmin - 2.6).abs() < 1e-9 && (zmax - 2.6).abs() < 1e-9,
            "flat lip stays level at the eave"
        );
    }
}
