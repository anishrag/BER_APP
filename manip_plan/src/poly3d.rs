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
        let p0 = &self.exterior[0];
        let p1 = &self.exterior[1];
        let p2 = &self.exterior[2];
        let u = Vector3f64::new(p1.x - p0.x, p1.y - p0.y, p1.z - p0.z);
        let v = Vector3f64::new(p2.x - p0.x, p2.y - p0.y, p2.z - p0.z);
        let normal = Vector3f64::new(
            u.y * v.z - u.z * v.y,
            u.z * v.x - u.x * v.z,
            u.x * v.y - u.y * v.x,
        );
        let norm_length = (normal.x.powi(2) + normal.y.powi(2) + normal.z.powi(2)).sqrt();
        if norm_length < 1e-10 {
            return None;
        }
        let unit_normal = Vector3f64::new(
            normal.x / norm_length,
            normal.y / norm_length,
            normal.z / norm_length,
        );
        let d = -(unit_normal.x * p0.x + unit_normal.y * p0.y + unit_normal.z * p0.z);
        Some((unit_normal, d / norm_length))
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
