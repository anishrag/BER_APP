//! Pure 3D-polygon math kept for the crate's surviving capabilities (mesh
//! triangulation and wall split-by-line). Region *derivation* math lives in
//! `datatypes` (`region_plane`, `with_apexes`, …) — nothing here overlaps it.

use datatypes::ber::structural::Polygon3D;
use domain::points::{Point2f64, Point3f64, Vector3f64};

pub const EPSILON: f64 = 1e-9;

/// A line lying in a wall's plane, with the side-deciding `normal` — the input
/// to `Polygon3D::split_by_line`. Kept for the deferred wall diagonal split
/// (splitting a marked wall's face by height/length for mixed constructions).
pub struct SplitLine3D {
    pub point: Point3f64,      // point on line
    pub direction: Vector3f64, // unit
    pub normal: Vector3f64,    // points to "positive" side
}

/// True if the 2D projection of `p` lies on segment `a`→`b` (within `epsilon`).
pub fn point_on_segment_2d(p: &Point2f64, a: &Point3f64, b: &Point3f64, epsilon: f64) -> bool {
    // Check if point is collinear with the segment
    let cross = (p.y - a.y) * (b.x - a.x) - (p.x - a.x) * (b.y - a.y);
    if cross.abs() > epsilon {
        return false;
    }

    // Check if point is between a and b
    let dot = (p.x - a.x) * (b.x - a.x) + (p.y - a.y) * (b.y - a.y);
    let len_sq = (b.x - a.x) * (b.x - a.x) + (b.y - a.y) * (b.y - a.y);

    dot >= -epsilon && dot <= len_sq + epsilon
}

/// True if `p` is inside triangle `abc` (barycentric test, edge-inclusive).
pub fn point_in_triangle_2d(
    p: &Point2f64,
    a: &Point2f64,
    b: &Point2f64,
    c: &Point2f64,
    epsilon: f64,
) -> bool {
    let v0x = c.x - a.x;
    let v0y = c.y - a.y;
    let v1x = b.x - a.x;
    let v1y = b.y - a.y;
    let v2x = p.x - a.x;
    let v2y = p.y - a.y;

    let dot00 = v0x * v0x + v0y * v0y;
    let dot01 = v0x * v1x + v0y * v1y;
    let dot02 = v0x * v2x + v0y * v2y;
    let dot11 = v1x * v1x + v1y * v1y;
    let dot12 = v1x * v2x + v1y * v2y;

    let inv_denom = 1.0 / (dot00 * dot11 - dot01 * dot01);
    let u = (dot11 * dot02 - dot01 * dot12) * inv_denom;
    let v = (dot00 * dot12 - dot01 * dot02) * inv_denom;

    (u >= -epsilon) && (v >= -epsilon) && (u + v <= 1.0 + epsilon)
}

/// The 3D-polygon operations the kept machinery needs (the old boolean-op /
/// lift surface duplicated `datatypes`' region splitting and was deleted).
pub trait Geometry {
    fn get_normal_and_distance_to_origin(&self) -> Option<(Vector3f64, f64)>;
    fn split_by_line(&self, line: &SplitLine3D) -> Vec<Polygon3D>;
    fn contains_point(&self, p: &Point2f64) -> bool;
}

impl Geometry for Polygon3D {
    fn get_normal_and_distance_to_origin(&self) -> Option<(Vector3f64, f64)> {
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

    /// Split this planar polygon in two along `line` (which must lie in its
    /// plane). Kept for the deferred wall diagonal split.
    fn split_by_line(&self, line: &SplitLine3D) -> Vec<Polygon3D> {
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

    /// Returns true if the (x, y) point lies inside the polygon exterior.
    /// Points on edges are considered inside.
    fn contains_point(&self, p: &Point2f64) -> bool {
        let n = self.exterior.len();
        if n < 3 {
            return false;
        }

        let mut inside = false;

        for i in 0..n {
            let a = &self.exterior[i];
            let b = &self.exterior[(i + 1) % n];

            // Check if point is exactly on the edge
            if point_on_segment_2d(p, a, b, EPSILON) {
                return true;
            }

            // Ray casting: does the horizontal ray cross this edge?
            let intersects = ((a.y > p.y) != (b.y > p.y))
                && (p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x);

            if intersects {
                inside = !inside;
            }
        }

        inside
    }
}
