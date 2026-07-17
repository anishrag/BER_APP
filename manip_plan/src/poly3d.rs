//! 3D planar-polygon primitive shared across the plan/region geometry: a ring
//! of 3D points with plane queries and an in-plane split. Moved here from the
//! BER data model (2026-06) so the geometry crate owns the 3D kernel; the data
//! model re-exports it for serialized region storage.

use geo::{Coord, LineString, Polygon};

use domain::points::{Point3f64, Vector3f64};

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Polygon3D {
    pub exterior: Vec<Point3f64>,
}

/// A line lying in a planar polygon's plane, with the side-deciding `normal` —
/// the input to [`Polygon3D::split_by_line`]. Moved here from the dissolved
/// `gen_3d_vol` crate (2026-06).
pub struct SplitLine3D {
    pub point: Point3f64,      // point on line
    pub direction: Vector3f64, // unit
    pub normal: Vector3f64,    // points to "positive" side
}

impl Polygon3D {
    pub fn new(exterior: Vec<Point3f64>) -> Self {
        Self { exterior }
    }

    /// Unit normal of the polygon's plane (from its first three vertices) and
    /// the plane's distance-to-origin term; `None` when degenerate. Helper for
    /// [`Polygon3D::split_by_line`].
    pub fn get_normal_and_distance_to_origin(&self) -> Option<(Vector3f64, f64)> {
        if self.exterior.len() < 3 {
            return None;
        }
        // NEWELL'S METHOD over the whole ring — a naive first-three-vertices
        // cross product fails whenever the leading vertices are collinear
        // (e.g. a wall whose eave line carries three aligned points), which
        // made such faces vanish from every plane-based consumer (3D
        // picking included). Newell only degenerates for genuinely zero-area
        // rings. Orientation follows the ring winding, same as before.
        let mut normal = Vector3f64::new(0.0, 0.0, 0.0);
        for i in 0..self.exterior.len() {
            let a = &self.exterior[i];
            let b = &self.exterior[(i + 1) % self.exterior.len()];
            normal.x += (a.y - b.y) * (a.z + b.z);
            normal.y += (a.z - b.z) * (a.x + b.x);
            normal.z += (a.x - b.x) * (a.y + b.y);
        }
        let norm_length = (normal.x.powi(2) + normal.y.powi(2) + normal.z.powi(2)).sqrt();
        if norm_length < 1e-10 {
            return None;
        }
        let unit_normal = Vector3f64::new(
            normal.x / norm_length,
            normal.y / norm_length,
            normal.z / norm_length,
        );
        // Signed plane distance to the origin, with the UNIT normal (the old
        // extra ÷ norm_length was a latent bug — every caller ignored the
        // distance, so nothing depended on the wrong scale).
        let p0 = &self.exterior[0];
        let d = -(unit_normal.x * p0.x + unit_normal.y * p0.y + unit_normal.z * p0.z);
        Some((unit_normal, d))
    }

    /// TRUE (in-plane) area of the planar polygon, via Newell's formula —
    /// half the magnitude of the summed edge cross products. Works for any
    /// plane orientation: a vertical wall face gives its face area, a sloped
    /// roof region its sloped (not plan-projected) area. Degenerate rings
    /// (< 3 vertices) are 0.
    pub fn area(&self) -> f64 {
        if self.exterior.len() < 3 {
            return 0.0;
        }
        let (mut sx, mut sy, mut sz) = (0.0f64, 0.0f64, 0.0f64);
        for (a, b) in self
            .exterior
            .iter()
            .zip(self.exterior.iter().cycle().skip(1))
            .take(self.exterior.len())
        {
            sx += a.y * b.z - a.z * b.y;
            sy += a.z * b.x - a.x * b.z;
            sz += a.x * b.y - a.y * b.x;
        }
        0.5 * (sx * sx + sy * sy + sz * sz).sqrt()
    }

    /// The z of this polygon's plane at plan point `(x, y)` — for lifting a
    /// roof window's plan rectangle onto a (possibly sloped) ceiling region. A
    /// flat region returns its constant z. `None` if degenerate or the plane is
    /// vertical (zero z-normal).
    pub fn plane_z(&self, x: f64, y: f64) -> Option<f64> {
        let (n, _) = self.get_normal_and_distance_to_origin()?;
        if n.z.abs() < 1e-9 {
            return None;
        }
        let p0 = self.exterior[0];
        // n · (P − p0) = 0  →  z = p0.z − (n.x·(x−p0.x) + n.y·(y−p0.y)) / n.z
        Some(p0.z - (n.x * (x - p0.x) + n.y * (y - p0.y)) / n.z)
    }

    /// Split this planar polygon in two along `line` (which must lie in its
    /// plane).
    ///
    /// SEAM: currently has no caller. Kept for the deferred wall face/height
    /// DIAGONAL SPLIT of the wall-exposure design — dividing one marked wall's
    /// face along a length/height intercept into separately-attributed
    /// `WallRegion`s for mixed constructions.
    pub fn split_by_line(&self, line: &SplitLine3D) -> Vec<Polygon3D> {
        let (plane_n, _) = self.get_normal_and_distance_to_origin().unwrap();
        let line_n = plane_n.cross(&line.direction).normalize();

        let mut left = Vec::new();
        let mut right = Vec::new();

        let verts = &self.exterior;

        for i in 0..verts.len() {
            let a = verts[i];
            let b = verts[(i + 1) % verts.len()];

            let da = (a.coords - line.point.coords).dot(&line_n);
            let db = (b.coords - line.point.coords).dot(&line_n);

            if da >= 0.0 {
                left.push(a);
            }
            if da <= 0.0 {
                right.push(a);
            }

            // Edge crosses the line → add intersection
            if da * db < 0.0 {
                let t = da / (da - db);
                let p = a + t * (b - a);
                left.push(p);
                right.push(p);
            }
        }

        vec![Polygon3D::new(left), Polygon3D::new(right)]
    }
}

impl From<Polygon3D> for Polygon<f64> {
    fn from(polygon_3d: Polygon3D) -> Self {
        let coords_2d: Vec<Coord<f64>> = polygon_3d
            .exterior
            .iter()
            .map(|v| Coord { x: v.x, y: v.y })
            .collect();
        Polygon::new(LineString::from(coords_2d), vec![])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f64, y: f64, z: f64) -> Point3f64 {
        Point3f64::new(x, y, z)
    }

    /// Newell area: exact on a flat rectangle, a vertical wall face, and a
    /// 45°-sloped roof plane (where the plan projection would under-read).
    #[test]
    fn area_is_true_in_plane_area() {
        let flat = Polygon3D::new(vec![
            p(0., 0., 0.),
            p(4., 0., 0.),
            p(4., 3., 0.),
            p(0., 3., 0.),
        ]);
        assert!((flat.area() - 12.0).abs() < 1e-9);

        let wall = Polygon3D::new(vec![
            p(0., 0., 0.),
            p(5., 0., 0.),
            p(5., 0., 2.4),
            p(0., 0., 2.4),
        ]);
        assert!((wall.area() - 12.0).abs() < 1e-9);

        // 4m (plan) × 3m run rising 3m: slope length 3√2, area 12√2.
        let sloped = Polygon3D::new(vec![
            p(0., 0., 0.),
            p(4., 0., 0.),
            p(4., 3., 3.),
            p(0., 3., 3.),
        ]);
        assert!((sloped.area() - 12.0 * 2.0_f64.sqrt()).abs() < 1e-9);

        assert_eq!(
            Polygon3D::new(vec![p(0., 0., 0.), p(1., 0., 0.)]).area(),
            0.0
        );
    }
}
