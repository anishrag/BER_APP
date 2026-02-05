use nalgebra::{Point2, Point3, Vector3};

use datatypes::ber::common::ItemId;
pub use datatypes::ber::structural::{Polygon3D, RegionBase};

pub const EPSILON: f64 = 1e-9;

pub struct SplitLine3D {
    pub point: Point3<f64>,      // point on line
    pub direction: Vector3<f64>, // unit
    pub normal: Vector3<f64>,    // points to "positive" side
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

// geometry crate
pub trait FromMultiZ {
    fn try_from_multi_z(multi_z: &Polygon3DMultiZ) -> Result<Self, Polygon3DConversionError>
    where
        Self: Sized;
    fn try_from_multi_z_owned(multi_z: Polygon3DMultiZ) -> Result<Self, Polygon3DConversionError>
    where
        Self: Sized,
    {
        // Default implementation delegates to the borrowed version
        Self::try_from_multi_z(&multi_z)
    }
}

// implement for Polygon3D (foreign type) ✅ legal because trait is local
impl FromMultiZ for Polygon3D {
    fn try_from_multi_z(multi_z: &Polygon3DMultiZ) -> Result<Self, Polygon3DConversionError> {
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

        Ok(Polygon3D::new(points))
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
impl From<Polygon3DMultiZ> for Vec<Point2<f64>> {
    fn from(multi_z: Polygon3DMultiZ) -> Self {
        multi_z
            .exterior
            .iter()
            .map(|(x, y, _)| Point2::new(*x, *y))
            .collect()
    }
}

#[derive(PartialEq, Clone, Debug)]
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

pub enum MergeWarning {
    InvalidIds(Vec<ItemId>),
    IncompatibleRegions(ItemId, ItemId),
    DifferentOrientations(ItemId, ItemId),
    MultipleResultingRegions {
        source_ids: Vec<ItemId>,  // the IDs of the regions that were merged
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
        new_region.base_mut().id = ItemId::default();
        new_region
    }
}

pub trait RegionContainer<T> {
    fn regions(&self) -> &Vec<T>;
}
