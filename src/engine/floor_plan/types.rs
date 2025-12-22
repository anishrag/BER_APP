use geo::BooleanOps;
use geo::{Coord, LineString, MultiPolygon, Polygon};
use nalgebra::{Point3, Vector2};

#[derive(Clone, Debug, PartialEq)]
pub struct Polygon3D {
    pub exterior: Vec<Point3<f64>>,
}

impl Polygon3D {
    pub fn new(exterior: Vec<Point3<f64>>) -> Self {
        Self { exterior }
    }

    /// Returns the overlapping regions shared by both `self` and a 2D `other` polygon.
    /// Projects this 3D polygon to 2D, performs the intersection, then lifts back to 3D.
    pub fn intersection(&self, other: &Polygon<f64>) -> MultiPolygon3D {
        self.boolean_op(other, OpType::Intersection)
    }

    pub fn difference(&self, other: &Polygon<f64>) -> MultiPolygon3D {
        self.boolean_op(other, OpType::Difference)
    }

    /// Returns the regions of `self` that remain after subtracting a 2D `other` polygon.
    /// Projects this 3D polygon to 2D, performs the difference, then lifts back to 3D.
    pub fn remainder(&self, other: &Polygon<f64>) -> MultiPolygon3D {
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
                let z = self.interpolate_z(point.x(), point.y());
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
    fn interpolate_z(&self, x: f64, y: f64) -> f64 {
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
            let w0 = ((v1.y - v2.y) * (x - v2.x) + (v2.x - v1.x) * (y - v2.y)) / denom;
            let w1 = ((v2.y - v0.y) * (x - v2.x) + (v0.x - v2.x) * (y - v2.y)) / denom;
            let w2 = 1.0 - w0 - w1;

            return w0 * v0.z + w1 * v1.z + w2 * v2.z;
        }

        // Fallback: find closest edge and interpolate
        let mut min_dist = f64::INFINITY;
        let mut closest_z = 0.0;

        for i in 0..self.exterior.len() {
            let v_a = &self.exterior[i];
            let v_b = &self.exterior[(i + 1) % self.exterior.len()];

            let t = Self::closest_point_on_segment(x, y, v_a, v_b);
            let interp_z = v_a.z + t * (v_b.z - v_a.z);

            let px = v_a.x + t * (v_b.x - v_a.x);
            let py = v_a.y + t * (v_b.y - v_a.y);
            let dist = ((x - px).powi(2) + (y - py).powi(2)).sqrt();

            if dist < min_dist {
                min_dist = dist;
                closest_z = interp_z;
            }
        }

        closest_z
    }

    /// Finds the parameter t [0, 1] for the closest point on line segment AB to point (x, y).
    fn closest_point_on_segment(x: f64, y: f64, a: &Point3<f64>, b: &Point3<f64>) -> f64 {
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let len_sq = dx * dx + dy * dy;

        if len_sq < 1e-10 {
            return 0.0;
        }

        let t = ((x - a.x) * dx + (y - a.y) * dy) / len_sq;
        t.clamp(0.0, 1.0)
    }
}

/// Type of boolean operation to perform
enum OpType {
    Intersection,
    Difference,
}

#[derive(PartialEq, Clone, Debug)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MultiPolygon3D {
    pub polygons: Vec<Polygon3D>,
}

impl From<Polygon3D> for MultiPolygon3D {
    fn from(polygon: Polygon3D) -> Self {
        Self {
            polygons: vec![polygon],
        }
    }
}

impl From<Vec<Polygon3D>> for MultiPolygon3D {
    fn from(polygons: Vec<Polygon3D>) -> Self {
        Self { polygons }
    }
}

impl MultiPolygon3D {
    /// Creates a new MultiPolygon3D with the given Polygon3Ds as elements
    pub fn new(polygons: Vec<Polygon3D>) -> Self {
        Self { polygons }
    }

    /// Returns an empty MultiPolygon3D
    pub fn empty() -> Self {
        Self {
            polygons: Vec::new(),
        }
    }

    /// Returns an iterator over the polygons
    pub fn iter(&self) -> impl Iterator<Item = &Polygon3D> {
        self.polygons.iter()
    }

    /// Returns a mutable iterator over the polygons
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Polygon3D> {
        self.polygons.iter_mut()
    }

    /// Returns the number of polygons in this MultiPolygon3D
    pub fn len(&self) -> usize {
        self.polygons.len()
    }

    /// Returns true if this MultiPolygon3D contains no polygons
    pub fn is_empty(&self) -> bool {
        self.polygons.is_empty()
    }
}

/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorConstructionType {
    SolidFloor,
    SuspendedFloor,
    NonHeatLossFloor,
    Other,
}

/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeilingConstructionType {
    FlatRoof,
    SlopedInsOnRafter,
    SlopedInsOnCeiling,
    SlopedInsOnCeilingAndRafter,
    NonHeatLossCeiling,
    Other,
}
/// Construction layers (optional multiple layers)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstructionLayer {
    Insulation,
    Concrete,
    Waterproofing,
    Finish,
    // add more as needed
}
