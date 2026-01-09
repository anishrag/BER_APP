use geo::BooleanOps;
use geo::{Coord, LineString, MultiPolygon, Polygon};
use nalgebra::{Point2, Point3, Vector3};
use uuid::Uuid;

use datatypes::ber::common::ConstructionPhase;
use datatypes::ber::structural::ConstructionMakeup;

const EPSILON: f64 = 1e-9;

#[derive(Clone, Debug)]
pub struct RegionBase {
    pub id: Uuid,
    pub construction_makeup: ConstructionMakeup,
    pub construction_phase: ConstructionPhase,
}

pub struct SplitLine3D {
    pub point: Point3<f64>,      // point on line
    pub direction: Vector3<f64>, // unit
    pub normal: Vector3<f64>,    // points to "positive" side
}

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

    pub fn to_2d_polygon_and_heights(&self) -> (Polygon<f64>, Vec<f64>) {
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
    /// Convert from Polygon3DMultiZ, failing if any point has multiple z values
    pub fn try_from_multi_z(multi_z: &Polygon3DMultiZ) -> Result<Self, Polygon3DConversionError> {
        let mut points = Vec::new();

        for (idx, (x, y, z_values)) in multi_z.exterior.iter().enumerate() {
            match z_values.len() {
                0 => return Err(Polygon3DConversionError::NoZValue { point_index: idx }),
                1 => points.push(Point3::new(*x, *y, z_values[0])),
                _ => {
                    return Err(Polygon3DConversionError::HasDiscontinuity {
                        point_index: idx,
                        z_values: z_values.clone(),
                    })
                }
            }
        }

        Ok(Self::new(points))
    }

    pub fn get_normal_and_distance_to_origin(&self) -> Option<(Vector3<f64>, f64)> {
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

impl Polygon3D {
    /// Returns true if the (x, y) point lies inside the polygon exterior.
    /// Points on edges are considered inside.
    pub fn contains_point(&self, x: f64, y: f64) -> bool {
        let n = self.exterior.len();
        if n < 3 {
            return false;
        }

        let mut inside = false;

        for i in 0..n {
            let a = &self.exterior[i];
            let b = &self.exterior[(i + 1) % n];

            // Check if point is exactly on the edge
            if Self::point_on_segment_2d(x, y, a, b) {
                return true;
            }

            // Ray casting: does the horizontal ray cross this edge?
            let intersects =
                ((a.y > y) != (b.y > y)) && (x < (b.x - a.x) * (y - a.y) / (b.y - a.y) + a.x);

            if intersects {
                inside = !inside;
            }
        }

        inside
    }

    /// Checks whether point (x, y) lies on the segment AB (2D only)
    fn point_on_segment_2d(x: f64, y: f64, a: &Point3<f64>, b: &Point3<f64>) -> bool {
        const EPS: f64 = 1e-10;

        // Cross product must be ~0 for collinearity
        let cross = (x - a.x) * (b.y - a.y) - (y - a.y) * (b.x - a.x);
        if cross.abs() > EPS {
            return false;
        }

        // Dot product must be within segment bounds
        let dot = (x - a.x) * (x - b.x) + (y - a.y) * (y - b.y);
        dot <= EPS
    }
}
impl Into<Polygon<f64>> for Polygon3D {
    fn into(self) -> Polygon<f64> {
        let coords_2d: Vec<Coord<f64>> = self
            .exterior
            .iter()
            .map(|v| Coord { x: v.x, y: v.y })
            .collect();
        Polygon::new(LineString::from(coords_2d), vec![])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Polygon3DConversionError {
    /// A point has multiple z values (discontinuity)
    HasDiscontinuity {
        point_index: usize,
        z_values: Vec<f64>,
    },
    /// A point has no z values
    NoZValue { point_index: usize },
}

impl TryFrom<&Polygon3DMultiZ> for Polygon3D {
    type Error = Polygon3DConversionError;

    fn try_from(multi_z: &Polygon3DMultiZ) -> Result<Self, Self::Error> {
        Self::try_from_multi_z(multi_z)
    }
}

impl TryFrom<Polygon3DMultiZ> for Polygon3D {
    type Error = Polygon3DConversionError;

    fn try_from(multi_z: Polygon3DMultiZ) -> Result<Self, Self::Error> {
        Self::try_from_multi_z(&multi_z)
    }
}

/// A 3D polygon where each vertex can have multiple Z values (for discontinuities)
#[derive(Clone, Debug, PartialEq)]
pub struct Polygon3DMultiZ {
    /// Each point has an (x, y) coordinate and a vector of z values
    /// Multiple z values represent discontinuities at that point
    pub exterior: Vec<(f64, f64, Vec<f64>)>,
}

impl Polygon3DMultiZ {
    pub fn new(exterior: Vec<(f64, f64, Vec<f64>)>) -> Self {
        Self { exterior }
    }

    /// Remove collinear interior points (based on XY coordinates)
    pub fn simplify(&self, eps: f64) -> Self {
        if self.exterior.len() < 3 {
            return self.clone();
        }

        let mut out = Vec::with_capacity(self.exterior.len());
        out.push(self.exterior[0].clone());

        for i in 1..self.exterior.len() - 1 {
            let a = out[out.len() - 1].clone();
            let b = self.exterior[i].clone();
            let c = self.exterior[i + 1].clone();

            // compute cross product in XY plane
            let cross = (b.0 - a.0) * (c.1 - b.1) - (b.1 - a.1) * (c.0 - b.0);
            if cross.abs() > eps {
                out.push(b);
            }
            // else skip b (it is collinear)
        }

        out.push(self.exterior.last().unwrap().clone());
        Self::new(out)
    }

    pub fn xy_approx_eq(&self, other: &Self, eps: f64) -> bool {
        if self.exterior.len() != other.exterior.len() {
            return false;
        }

        self.exterior
            .iter()
            .zip(&other.exterior)
            .all(|((x1, y1, _), (x2, y2, _))| (x1 - x2).abs() < eps && (y1 - y2).abs() < eps)
    }
}

pub fn approx_equal_xy(a: (f64, f64, Vec<f64>), b: (f64, f64, Vec<f64>), eps: f64) -> bool {
    (a.0 - b.0).abs() < eps && (a.1 - b.1).abs() < eps
}

impl From<Polygon3D> for Polygon3DMultiZ {
    fn from(polygon: Polygon3D) -> Self {
        let exterior = polygon
            .exterior
            .iter()
            .map(|point| (point.x, point.y, vec![point.z]))
            .collect();
        Self::new(exterior)
    }
}

impl Into<Vec<Point2<f64>>> for Polygon3DMultiZ {
    fn into(self) -> Vec<Point2<f64>> {
        self.exterior
            .iter()
            .map(|(x, y, _)| Point2::new(*x, *y))
            .collect()
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

impl IntoIterator for MultiPolygon3D {
    type Item = Polygon3D;
    type IntoIter = std::vec::IntoIter<Polygon3D>;

    fn into_iter(self) -> Self::IntoIter {
        self.polygons.into_iter()
    }
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

#[derive(Clone, Copy, Debug)]
pub struct DirectedEdge3D {
    pub from: Point3<f64>,
    pub to: Point3<f64>,
}

impl DirectedEdge3D {
    pub fn new(from: Point3<f64>, to: Point3<f64>) -> Self {
        Self { from, to }
    }

    pub fn matches(&self, other: &DirectedEdge3D) -> bool {
        (self.from == other.from && self.to == other.to)
            || (self.from == other.to && self.to == other.from)
    }
    /// Compare only x/y coordinates, ignoring z
    pub fn matches_xy(&self, other: &DirectedEdge3D) -> bool {
        self.from.eq_xy(&other.from, 1e-9) && self.to.eq_xy(&other.to, 1e-9)
    }

    /// Compare x/y ignoring order (undirected)
    pub fn matches_xy_undirected(&self, other: &DirectedEdge3D) -> bool {
        self.matches_xy(other)
            || self.from.eq_xy(&other.to, 1e-9) && self.to.eq_xy(&other.from, 1e-9)
    }
}

pub trait Point3Ext {
    fn eq_xy(&self, other: &Self, eps: f64) -> bool;
}

impl Point3Ext for Point3<f64> {
    fn eq_xy(&self, other: &Self, eps: f64) -> bool {
        (self.x - other.x).abs() < eps && (self.y - other.y).abs() < eps
    }
}

pub trait MergeableGeometry: Clone {
    /// Merge two geometries into zero or more geometries
    fn merge(&self, other: &Self) -> Vec<Self>;
    fn same_orientation(&self, other: &Self) -> bool;
}

impl MergeableGeometry for geo::Polygon<f64> {
    fn merge(&self, other: &Self) -> Vec<Self> {
        let multi = self.union(other);
        multi.0
    }
    fn same_orientation(&self, other: &Self) -> bool {
        true
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
    fn same_orientation(&self, other: &Self) -> bool {
        let Some((n1, _)) = self.get_normal_and_distance_to_origin() else {
            return false;
        };
        let Some((n2, _)) = other.get_normal_and_distance_to_origin() else {
            return false;
        };

        n1.dot(&n2) - 1.0 < EPSILON
    }
}

pub trait MergeableRegion: Clone {
    type Geometry: MergeableGeometry;

    fn id(&self) -> Uuid;
    fn geometry(&self) -> &Self::Geometry;

    fn is_merge_compatible(&self, other: &Self) -> bool;

    /// Rebuild region from merged geometry
    fn with_geometry(&self, geometry: Self::Geometry) -> Self;
}

pub enum MergeWarning {
    InvalidIds(Vec<Uuid>),
    IncompatibleRegions(Uuid, Uuid),
    DifferentOrientations(Uuid, Uuid),
    MultipleResultingRegions {
        source_ids: Vec<Uuid>,    // the IDs of the regions that were merged
        resulting_regions: usize, // how many regions came out after merging
    },
}

pub enum RegionType {
    Wall,
    Floor,
    Ceiling,
}

pub trait HasBaseRegion {
    fn base_mut(&mut self) -> &mut RegionBase;
}

pub trait CloneWithNewId {
    fn clone_with_new_id(&self) -> Self;
}

impl<T> CloneWithNewId for T
where
    T: Clone + HasBaseRegion,
{
    fn clone_with_new_id(&self) -> Self {
        let mut new_region = self.clone();
        new_region.base_mut().id = Uuid::new_v4();
        new_region
    }
}

pub trait RegionContainer<T> {
    fn regions(&self) -> &Vec<T>;
    fn regions_mut(&mut self) -> &mut Vec<T>;
}
