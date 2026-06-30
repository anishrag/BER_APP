//! The dwelling ROOF — a structure SEPARATE from the habitable-area ceiling.
//!
//! Distinct from the ceiling (`Storey::roofs` / the roof-point apexes, which
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
use domain::points::{Point2f64, Point3f64};

/// Default eave overhang (metres) beyond the footprint when deriving roof planes
/// — the roof oversails the walls. (Attic-converted continuation + a per-roof
/// override are follow-ups.)
pub const DEFAULT_OVERHANG_M: f64 = 0.3;

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

    /// The 3-D roof planes — each a closed ring of points whose z is LOCAL to the
    /// covered storey's floor datum (the eave sits at [`eave_z`](Self::eave_z), the
    /// ridge rises from there by the pitch). The caller adds the storey's datum to
    /// place them in world z. Built over the footprint's axis-aligned bounding box
    /// expanded by `overhang_m` (a single rectangular roof per area — a smarter
    /// per-outline build is a follow-up). Empty if the footprint is degenerate.
    pub fn planes(&self, overhang_m: f64) -> Vec<Vec<Point3f64>> {
        let Some((bx0, by0, bx1, by1)) = self.footprint_bbox() else {
            return Vec::new();
        };
        let (x0, y0, x1, y1) = (
            bx0 - overhang_m,
            by0 - overhang_m,
            bx1 + overhang_m,
            by1 + overhang_m,
        );
        let e = self.eave_z;
        let p = |x: f64, y: f64, z: f64| Point3f64::new(x, y, z);

        // Flat (or a sloped roof with no ridge recorded): a level slab at the eave.
        let Some((a, b)) = self.ridge.filter(|_| self.shape != RoofShape::Flat) else {
            return vec![vec![p(x0, y0, e), p(x1, y0, e), p(x1, y1, e), p(x0, y1, e)]];
        };

        let tan = (self.pitch_deg as f64).to_radians().tan();
        let ridge_along_y = (a.x - b.x).abs() <= (a.y - b.y).abs();
        let gable = self.shape == RoofShape::Gable;
        if ridge_along_y {
            // Ridge runs along Y at x = rx; slopes fall in X.
            let rx = (a.x + b.x) / 2.0;
            let r = e + ((x1 - x0) / 2.0) * tan;
            let (lo, hi) = if gable {
                (y0, y1)
            } else {
                (a.y.min(b.y), a.y.max(b.y))
            };
            let mut planes = vec![
                vec![p(x0, y0, e), p(x0, y1, e), p(rx, hi, r), p(rx, lo, r)],
                vec![p(x1, y0, e), p(x1, y1, e), p(rx, hi, r), p(rx, lo, r)],
            ];
            if !gable {
                planes.push(vec![p(x0, y0, e), p(x1, y0, e), p(rx, lo, r)]);
                planes.push(vec![p(x0, y1, e), p(x1, y1, e), p(rx, hi, r)]);
            }
            planes
        } else {
            // Ridge runs along X at y = ry; slopes fall in Y.
            let ry = (a.y + b.y) / 2.0;
            let r = e + ((y1 - y0) / 2.0) * tan;
            let (lo, hi) = if gable {
                (x0, x1)
            } else {
                (a.x.min(b.x), a.x.max(b.x))
            };
            let mut planes = vec![
                vec![p(x0, y0, e), p(x1, y0, e), p(hi, ry, r), p(lo, ry, r)],
                vec![p(x0, y1, e), p(x1, y1, e), p(hi, ry, r), p(lo, ry, r)],
            ];
            if !gable {
                planes.push(vec![p(x0, y0, e), p(x0, y1, e), p(lo, ry, r)]);
                planes.push(vec![p(x1, y0, e), p(x1, y1, e), p(hi, ry, r)]);
            }
            planes
        }
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

    #[test]
    fn gable_planes_two_slopes_to_a_ridge() {
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
        let planes = area.planes(0.0); // no overhang for exact maths
        assert_eq!(planes.len(), 2, "a gable has two slope planes");
        // Ridge height: eave 2.4 + half-span 5 · tan(45°) = 7.4.
        let zmax = planes
            .iter()
            .flatten()
            .map(|p| p.z)
            .fold(f64::MIN, f64::max);
        let zmin = planes
            .iter()
            .flatten()
            .map(|p| p.z)
            .fold(f64::MAX, f64::min);
        assert!((zmax - 7.4).abs() < 1e-6, "ridge at 7.4, got {zmax}");
        assert!((zmin - 2.4).abs() < 1e-6, "eave at 2.4, got {zmin}");
    }

    #[test]
    fn hip_planes_add_two_end_triangles() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::Hip,
            pitch_deg: 30.0,
            eave_z: 2.5,
            ridge: Some((Point2f64::new(5.0, 3.0), Point2f64::new(5.0, 7.0))),
            footprint: square_10(),
        };
        let planes = area.planes(0.0);
        assert_eq!(planes.len(), 4, "a hip has two slopes + two end triangles");
        assert_eq!(
            planes.iter().filter(|p| p.len() == 3).count(),
            2,
            "the two hip ends are triangles"
        );
    }

    #[test]
    fn flat_roof_is_one_level_plane_at_the_eave() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.6,
            ridge: None,
            footprint: square_10(),
        };
        let planes = area.planes(0.0);
        assert_eq!(planes.len(), 1);
        assert!(planes[0].iter().all(|p| (p.z - 2.6).abs() < 1e-9), "level");
    }
}
