use geo::{BooleanOps, Coord, LineString, MultiPolygon, Polygon};
use nalgebra::{Point2, Point3, Vector3};

use datatypes::ber::common::ItemId;
use datatypes::ber::structural::Polygon3D;

use crate::types::{MultiPolygon3D, SplitLine3D, EPSILON};

// Helper function to check if a point lies on a line segment in 2D
pub fn point_on_segment_2d(
    p: &Point2<f64>,
    a: &Point3<f64>,
    b: &Point3<f64>,
    epsilon: f64,
) -> bool {
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

// Helper function to check if a point is inside a triangle in 2D using barycentric coordinates
pub fn point_in_triangle_2d(
    p: &Point2<f64>,
    a: &Point2<f64>,
    b: &Point2<f64>,
    c: &Point2<f64>,
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
/// Finds the parameter t [0, 1] for the closest point on line segment AB to point (x, y).
pub fn closest_point_on_segment(
    p: &Point2<f64>,
    a: &Point3<f64>,
    b: &Point3<f64>,
    epsilon: f64,
) -> f64 {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;

    if len_sq < epsilon {
        return 0.0;
    }

    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / len_sq;
    t.clamp(0.0, 1.0)
}
/// Type of boolean operation to perform
pub enum OpType {
    Intersection,
    Difference,
}

pub trait Geometry {
    fn intersection(&self, other: &Polygon<f64>) -> MultiPolygon3D;
    fn difference(&self, other: &Polygon<f64>) -> MultiPolygon3D;
    fn remainder(&self, other: &Polygon<f64>) -> MultiPolygon3D;
    fn boolean_op(&self, other: &Polygon<f64>, op_type: OpType) -> MultiPolygon3D;
    fn to_2d_polygon(&self) -> Polygon<f64>;
    fn to_2d_polygon_and_heights(&self) -> (Polygon<f64>, Vec<f64>);
    fn lift_multipolygon_to_3d(&self, multi_2d: &MultiPolygon<f64>) -> MultiPolygon3D;
    fn lift_polygon_to_3d(&self, poly_2d: &Polygon<f64>) -> Polygon3D;
    fn interpolate_z(&self, p: &Point2<f64>) -> f64;

    fn get_normal_and_distance_to_origin(&self) -> Option<(Vector3<f64>, f64)>;
    fn split_by_line(&self, line: &SplitLine3D) -> Vec<Polygon3D>;
    fn contains_point(&self, p: &Point2<f64>) -> bool;
    fn get_heights_at_edge_point(&self, p: &Point2<f64>) -> Result<Vec<f64>>;
}
impl Geometry for Polygon3D {
    /// Returns the overlapping regions shared by both `self` and a 2D `other` polygon.
    /// Projects this 3D polygon to 2D, performs the intersection, then lifts back to 3D.
    fn intersection(&self, other: &Polygon<f64>) -> MultiPolygon3D {
        self.boolean_op(other, OpType::Intersection)
    }

    fn difference(&self, other: &Polygon<f64>) -> MultiPolygon3D {
        self.boolean_op(other, OpType::Difference)
    }

    /// Returns the regions of `self` that remain after subtracting a 2D `other` polygon.
    /// Projects this 3D polygon to 2D, performs the difference, then lifts back to 3D.
    fn remainder(&self, other: &Polygon<f64>) -> MultiPolygon3D {
        self.boolean_op(other, OpType::Difference)
    }

    /// Performs a boolean operation between this 3D polygon and a 2D polygon.
    /// The 3D polygon is projected to 2D (XY plane), the operation is performed,
    /// and the result is lifted back to 3D with interpolated Z coordinates.
    fn boolean_op(&self, other: &Polygon<f64>, op_type: OpType) -> MultiPolygon3D {
        // Project this 3D polygon to 2D
        let poly_2d = self.to_2d_polygon();

        // Perform the 2D boolean operation using geo_types
        let result_2d: MultiPolygon<f64> = match op_type {
            OpType::Intersection => poly_2d.intersection(other),
            OpType::Difference => poly_2d.difference(other),
        };

        // Lift the 2D result back to 3D
        self.lift_multipolygon_to_3d(&result_2d)
    }

    /// Projects this 3D polygon to a 2D polygon by dropping the Z coordinate.
    fn to_2d_polygon(&self) -> Polygon<f64> {
        let coords_2d: Vec<Coord<f64>> = self
            .exterior
            .iter()
            .map(|v| Coord { x: v.x, y: v.y })
            .collect();

        Polygon::new(LineString::from(coords_2d), vec![])
    }

    fn to_2d_polygon_and_heights(&self) -> (Polygon<f64>, Vec<f64>) {
        let mut heights = Vec::with_capacity(self.exterior.len());
        let coords_2d: Vec<Coord<f64>> = self
            .exterior
            .iter()
            .map(|v| {
                heights.push(v.z);
                Coord { x: v.x, y: v.y }
            })
            .collect();
        (Polygon::new(LineString::from(coords_2d), vec![]), heights)
    }

    /// Lifts a 2D MultiPolygon back to 3D by interpolating Z coordinates.
    fn lift_multipolygon_to_3d(&self, multi_2d: &MultiPolygon<f64>) -> MultiPolygon3D {
        let polygons_3d: Vec<Polygon3D> = multi_2d
            .iter()
            .map(|poly_2d| self.lift_polygon_to_3d(poly_2d))
            .collect();

        MultiPolygon3D::new(polygons_3d)
    }

    /// Lifts a single 2D polygon to 3D by interpolating Z coordinates from the original 3D polygon.
    fn lift_polygon_to_3d(&self, poly_2d: &Polygon<f64>) -> Polygon3D {
        let exterior_3d: Vec<Point3<f64>> = poly_2d
            .exterior()
            .points()
            .map(|point| {
                let p2: Point2<f64> = Point2::new(point.x(), point.y());
                let z = self.interpolate_z(&p2);
                Point3::new(point.x(), point.y(), z)
            })
            .collect();

        Polygon3D {
            exterior: exterior_3d,
        }
    }

    /// Interpolates the Z coordinate for a given (x, y) point based on the original 3D polygon.
    /// Uses barycentric interpolation if the point is inside the polygon,
    /// or finds the closest edge and interpolates along it.
    fn interpolate_z(&self, p: &Point2<f64>) -> f64 {
        if self.exterior.len() < 3 {
            return self.exterior.first().map_or(0.0, |v| v.z);
        }

        // Try barycentric interpolation using the first triangle
        let v0 = &self.exterior[0];
        let v1 = &self.exterior[1];
        let v2 = &self.exterior[2];

        // Compute barycentric coordinates
        let denom = (v1.y - v2.y) * (v0.x - v2.x) + (v2.x - v1.x) * (v0.y - v2.y);

        if denom.abs() > 1e-10 {
            let w0 = ((v1.y - v2.y) * (p.x - v2.x) + (v2.x - v1.x) * (p.y - v2.y)) / denom;
            let w1 = ((v2.y - v0.y) * (p.x - v2.x) + (v0.x - v2.x) * (p.y - v2.y)) / denom;
            let w2 = 1.0 - w0 - w1;

            return w0 * v0.z + w1 * v1.z + w2 * v2.z;
        }

        // Fallback: find closest edge and interpolate
        let mut min_dist = f64::INFINITY;
        let mut closest_z = 0.0;

        for i in 0..self.exterior.len() {
            let v_a = &self.exterior[i];
            let v_b = &self.exterior[(i + 1) % self.exterior.len()];

            let t = closest_point_on_segment(p, v_a, v_b, EPSILON);
            let interp_z = v_a.z + t * (v_b.z - v_a.z);

            let px = v_a.x + t * (v_b.x - v_a.x);
            let py = v_a.y + t * (v_b.y - v_a.y);
            let dist = ((p.x - px).powi(2) + (p.y - py).powi(2)).sqrt();

            if dist < min_dist {
                min_dist = dist;
                closest_z = interp_z;
            }
        }

        closest_z
    }

    fn get_normal_and_distance_to_origin(&self) -> Option<(Vector3<f64>, f64)> {
        if self.exterior.len() < 3 {
            return None;
        }
        let p0 = &self.exterior[0];
        let p1 = &self.exterior[1];
        let p2 = &self.exterior[2];
        let u = Vector3::new(p1.x - p0.x, p1.y - p0.y, p1.z - p0.z);
        let v = Vector3::new(p2.x - p0.x, p2.y - p0.y, p2.z - p0.z);
        let normal = Vector3::new(
            u.y * v.z - u.z * v.y,
            u.z * v.x - u.x * v.z,
            u.x * v.y - u.y * v.x,
        );
        let norm_length = (normal.x.powi(2) + normal.y.powi(2) + normal.z.powi(2)).sqrt();
        if norm_length < 1e-10 {
            return None;
        }
        let unit_normal = Vector3::new(
            normal.x / norm_length,
            normal.y / norm_length,
            normal.z / norm_length,
        );
        let d = -(unit_normal.x * p0.x + unit_normal.y * p0.y + unit_normal.z * p0.z);
        Some((unit_normal, d / norm_length))
    }

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
    fn contains_point(&self, p: &Point2<f64>) -> bool {
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
    // TODO: known failure point: if there are 3 regions fr the point and the discontinuities aren't
    // on the up/down direction
    fn get_heights_at_edge_point(&self, p: &Point2<f64>) -> Result<Vec<f64>, Polygon3DError> {
        let n = self.exterior.len();
        let mut heights = Vec::new();
        for i in 0..n {
            let a = &self.exterior[i];
            let b = &self.exterior[(i + 1) % n];
            if point_on_segment_2d(p, a, b, EPSILON) {
                let z = self.interpolate_z(p);
                heights.push(z);
            }
        }
        Ok(heights)
    }
}

enum Polygon3DError {
    PointNotOnEdge,
}
pub trait MergeableGeometry: Clone {
    /// Merge two geometries into zero or more geometries
    fn merge(&self, other: &Self) -> Vec<Self>;
}

impl MergeableGeometry for geo::Polygon<f64> {
    fn merge(&self, other: &Self) -> Vec<Self> {
        let multi = self.union(other);
        multi.0
    }
}

impl MergeableGeometry for Polygon3D {
    fn merge(&self, other: &Self) -> Vec<Self> {
        let poly1_2d: Polygon<f64> = self.clone().into();
        let poly2_2d: Polygon<f64> = other.clone().into();
        let union_2d = poly1_2d.union(&poly2_2d);
        // Lift back to 3D
        let mut result_3d = Vec::new();
        for poly_2d in union_2d.0 {
            let lifted = self.lift_polygon_to_3d(&poly_2d);
            result_3d.push(lifted);
        }
        result_3d
    }
}

pub trait MergeableRegion: Clone {
    type Geometry: MergeableGeometry;

    fn id(&self) -> ItemId;
    fn geometry(&self) -> &Self::Geometry;

    fn is_merge_compatible(&self, other: &Self) -> bool;

    /// Rebuild region from merged geometry
    fn with_geometry(&self, geometry: Self::Geometry) -> Self;
}
