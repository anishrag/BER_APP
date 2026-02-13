// standard includes
use datatypes::ber::common::{ConstructionPhase, ItemId};
use datatypes::ber::structural::{
    CeilingRegions, ConstructionMakeup, FloorRegions, WallConstructionType, WallExposure,
    WallRegion, WallRegions,
};
use domain::points::{Point2f64, Point3f64, Vector2f64, Vector3f64};
use geo::Polygon;

// local includes
use super::ceiling::CeilingRegionsManip;
use super::floor::FloorRegionsManip;
use super::geometry::{Geometry, MergeableRegion};
use super::types::{HasBaseRegion, Polygon3D, Polygon3DMultiZ, RegionBase, SplitLine3D, EPSILON};

trait WallRegionManip {
    fn get_leftmost_floor_vertex(&self) -> Point3f64;
    fn wall_horizontal_dir(&self) -> Vector3f64<f64>;
    fn wall_plane_normal(&self) -> Vector3f64<f64>;
    fn get_dividing_line_equation(&self, split_along_xy: f64, split_along_z: f64) -> SplitLine3D;
}

impl WallRegionManip for WallRegion {
    /// Get the length of the wall region (projected to XY plane)
    fn get_leftmost_floor_vertex(&self) -> Point3f64 {
        self.polygon.exterior[0]
    }

    fn wall_horizontal_dir(&self) -> Vector3f64<f64> {
        let p0 = self.polygon.exterior[0];
        let p1 = self.polygon.exterior[1];
        Vector3f64::new(p1.x - p0.x, p1.y - p0.y, 0.0)
    }

    fn wall_plane_normal(&self) -> Vector3f64<f64> {
        let p0 = self.polygon.exterior[0];
        let p1 = self.polygon.exterior[1];
        let p2 = self.polygon.exterior[2];
        let u = Vector3f64::new(p1.x - p0.x, p1.y - p0.y, p1.z - p0.z);
        let v = Vector3f64::new(p2.x - p0.x, p2.y - p0.y, p2.z - p0.z);
        u.cross(&v).normalize()
    }

    fn get_dividing_line_equation(&self, split_along_xy: f64, split_along_z: f64) -> SplitLine3D {
        let origin = self.get_leftmost_floor_vertex();

        let u = self.wall_horizontal_dir().normalize(); // in XY
        let v = Vector3f64::new(0.0, 0.0, 1.0);

        // 2D normal in wall coordinates
        let mut n2 = Vector2f64::new(0.0, 0.0);

        if split_along_xy != 0.0 {
            n2.x = 1.0 / split_along_xy;
        }
        if split_along_z != 0.0 {
            n2.y = 1.0 / split_along_z;
        }

        // Convert to 3D normal
        let normal = (u * n2.x + v * n2.y).normalize();

        // Find a point on the line
        // u/s_xy + v/s_z = 1
        let point = origin + u * split_along_xy.max(0.0) + v * split_along_z.max(0.0);

        // Line direction is perpendicular to normal *in wall plane*
        let direction = normal.cross(&self.wall_plane_normal()).normalize();

        SplitLine3D {
            point,
            direction,
            normal,
        }
    }
}
impl MergeableRegion for WallRegion {
    type Geometry = Polygon3D; // or Vec<Vertex> if you use vertex list

    fn id(&self) -> ItemId {
        self.base.id
    }

    fn geometry(&self) -> &Self::Geometry {
        &self.polygon
    }

    fn is_merge_compatible(&self, other: &Self) -> bool {
        self.construction_type == other.construction_type
            && self.base.construction_makeup == other.base.construction_makeup
            && self.base.construction_phase == other.base.construction_phase
            && self.exposure == other.exposure
    }

    fn with_geometry(&self, geometry: Self::Geometry) -> Self {
        WallRegion {
            polygon: geometry,
            construction_type: self.construction_type,
            base: self.base.clone(),
            exposure: self.exposure,
        }
    }
}

pub trait WallRegionsManip {
    fn get_wall_region_by_id_mut(&mut self, id: &ItemId) -> Option<&mut WallRegion>;
    fn split_wall_region<F>(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        update_new_region: F,
    ) where
        F: FnMut(&mut WallRegion);
    fn add_from_floor_discontinuities(
        &mut self,
        floor_regions: &FloorRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_exposure: Option<WallExposure>,
    );
    fn add_from_ceiling_discontinuities(
        &mut self,
        ceiling_regions: &CeilingRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_exposure: Option<WallExposure>,
    );
    fn add_from_exterior_perimeter(
        &mut self,
        floor_regions: &FloorRegions,
        ceiling_regions: &CeilingRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_construction_phase: Option<ConstructionPhase>,
        default_exposure: Option<WallExposure>,
    );
    fn merge(&mut self, other: WallRegions);
    fn modify_walls_in_region(
        &mut self,
        polygon_2d: &Polygon<f64>,
        construction_type: Option<WallConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
        exposure: Option<WallExposure>,
    );
    fn check_floor_ceiling_aligned(
        floor_boundary: &Polygon3DMultiZ,
        ceiling_boundary: &Polygon3DMultiZ,
    ) -> bool;
    fn merge_floor_ceiling_by_line(
        floor_boundary: &Polygon3DMultiZ,
        ceiling_boundary: &Polygon3DMultiZ,
    ) -> Option<Vec<BoundaryVertex>>;
}

pub struct BoundaryVertex {
    point: Point2f64,
    floor_height: Vec<f64>,
    ceiling_height: Vec<f64>,
}
impl WallRegionsManip for WallRegions {
    /// Create a new empty WallRegions
    fn get_wall_region_by_id_mut(&mut self, id: &ItemId) -> Option<&mut WallRegion> {
        self.regions.iter_mut().find(|region| &region.base.id == id)
    }

    fn split_wall_region<F>(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        mut update_new_region: F,
    ) where
        F: FnMut(&mut WallRegion),
    {
        let Some(wall_region) = self.get_wall_region_by_id_mut(&wall_id) else {
            println!("Wall with id {:?} not found.", wall_id);
            return;
        };

        let split_line = wall_region.get_dividing_line_equation(split_along_xy, split_along_z);
        let split_polygons = wall_region.polygon.split_by_line(&split_line);

        if split_polygons.is_empty() {
            println!("Split resulted in no polygons for wall {:?}", wall_id);
            return;
        }

        // First polygon stays on the original region
        wall_region.polygon = split_polygons[0].clone();

        let wall_region_clone = wall_region.clone();

        for split_polygon in split_polygons.iter().skip(1) {
            let mut new_wall_region = wall_region_clone.clone();
            new_wall_region.base.id = ItemId::default();
            new_wall_region.polygon = split_polygon.clone();

            update_new_region(&mut new_wall_region);

            self.regions.push(new_wall_region);
        }
    }

    /// Generate walls from floor region discontinuities (internal walls where floor height changes)
    fn add_from_floor_discontinuities(
        &mut self,
        floor_regions: &FloorRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_exposure: Option<WallExposure>,
    ) {
        // Find all edges between different floor regions
        for i in 0..floor_regions.regions.len() {
            for j in (i + 1)..floor_regions.regions.len() {
                let region_a = &floor_regions.regions[i];
                let region_b = &floor_regions.regions[j];

                // Find shared edges between the two regions
                let shared_edges = find_shared_edges_2d(&region_a.polygon, &region_b.polygon);

                for (p1, p2) in shared_edges {
                    // Create wall from floor height difference
                    let z1_a = region_a.height;
                    let z1_b = region_b.height;

                    // Only create wall if there's a height discontinuity
                    if (z1_a - z1_b).abs() < EPSILON {
                        continue;
                    }

                    let vertices = vec![
                        Point3f64::new(p1.0, p1.1, z1_a),
                        Point3f64::new(p2.0, p2.1, z1_a),
                        Point3f64::new(p2.0, p2.1, z1_b),
                        Point3f64::new(p1.0, p1.1, z1_b),
                    ];

                    // Determine construction phase (use the later one or the first region's)
                    let construction_phase = match (
                        &region_a.base.construction_phase,
                        &region_b.base.construction_phase,
                    ) {
                        (a, b) if a == b => a.clone(),
                        _ => ConstructionPhase::Original,
                    };

                    self.regions.push(WallRegion {
                        polygon: Polygon3D::new(vertices),
                        construction_type: default_construction_type
                            .unwrap_or(WallConstructionType::Uninitialised),
                        base: RegionBase {
                            id: ItemId::default(),
                            construction_phase,
                            construction_makeup: default_construction_makeup
                                .clone()
                                .unwrap_or_default(),
                            ..Default::default()
                        },
                        exposure: default_exposure.unwrap_or(WallExposure::Exposed),
                    });
                }
            }
        }
    }

    /// Generate walls from ceiling region discontinuities (internal walls where ceiling height jumps)
    fn add_from_ceiling_discontinuities(
        &mut self,
        ceiling_regions: &CeilingRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_exposure: Option<WallExposure>,
    ) {
        // Find all edges between different ceiling regions
        for i in 0..ceiling_regions.regions.len() {
            for j in (i + 1)..ceiling_regions.regions.len() {
                let region_a = &ceiling_regions.regions[i];
                let region_b = &ceiling_regions.regions[j];

                // Find shared edges between the two ceiling regions
                let shared_edges = find_shared_edges_3d(&region_a.polygon, &region_b.polygon);

                for edge in shared_edges {
                    // Edge represents points on two different ceiling levels
                    let (p1_a, p2_a) = edge.points_a;
                    let (p1_b, p2_b) = edge.points_b;

                    if p1_a.z != p1_b.z || p2_a.z != p2_b.z {
                        continue; // Ensure edges correspond correctly
                    }

                    // Create vertical wall between the two ceiling heights
                    let vertices = vec![p1_a, p2_a, p2_b, p1_b];

                    // Determine construction phase
                    let construction_phase = match (
                        &region_a.base.construction_phase,
                        &region_b.base.construction_phase,
                    ) {
                        (a, b) if a == b => a.clone(),
                        _ => ConstructionPhase::Original,
                    };

                    self.regions.push(WallRegion {
                        polygon: Polygon3D::new(vertices),
                        construction_type: default_construction_type
                            .unwrap_or(WallConstructionType::Uninitialised),
                        base: RegionBase {
                            id: ItemId::default(),
                            construction_makeup: default_construction_makeup
                                .clone()
                                .unwrap_or_default(),
                            construction_phase,
                            ..Default::default()
                        },
                        exposure: default_exposure.unwrap_or(WallExposure::NonHeatLoss),
                    });
                }
            }
        }
    }

    fn check_floor_ceiling_aligned(
        floor_boundary: &Polygon3DMultiZ,
        ceiling_boundary: &Polygon3DMultiZ,
    ) -> bool {
        let floor_simple = floor_boundary.simplify(EPSILON);
        let ceiling_simple = ceiling_boundary.simplify(EPSILON);

        floor_simple.xy_approx_eq(&ceiling_simple, EPSILON)
    }

    fn merge_floor_ceiling_by_line(
        floor_boundary: &Polygon3DMultiZ,
        ceiling_boundary: &Polygon3DMultiZ,
    ) -> Option<Vec<BoundaryVertex>> {
        assert!(Self::check_floor_ceiling_aligned(
            floor_boundary,
            ceiling_boundary
        ));
        let approx_eq = |p1: &(f64, f64), p2: &(f64, f64)| {
            (p1.0 - p2.0).abs() < EPSILON && (p1.1 - p2.1).abs() < EPSILON
        };

        let mut merged = Vec::new();
        let mut fi = 0;
        let mut ci = 0;
        let (fx, fy, fheight) = &floor_boundary.exterior[fi];
        let (_, _, cheight) = &ceiling_boundary.exterior[ci];

        merged.push(BoundaryVertex {
            point: Point2f64::new(*fx, *fy),
            floor_height: fheight.clone(),
            ceiling_height: cheight.clone(),
        });
        fi += 1;
        ci += 1;
        let mut segment_start: Option<(f64, f64)> = Some((*fx, *fy));

        // Helper for linear interpolation of heights along a line
        let interpolate_ceiling_height = |p: (f64, f64),
                                          start: (f64, f64, Vec<f64>),
                                          end: (f64, f64, Vec<f64>)|
         -> Option<Vec<f64>> {
            let start_z = start.2.last()?;
            let end_z = end.2.first()?;
            let dx = end.0 - start.0;
            let dy = end.1 - start.1;
            let length_sq = dx * dx + dy * dy;
            if length_sq < EPSILON {
                return Some(start.2.clone()); // avoid division by zero
            }
            let t = ((p.0 - start.0) * dx + (p.1 - start.1) * dy) / length_sq;
            Some(vec![start_z + t * (end_z - start_z)])
        };

        while fi < floor_boundary.exterior.len() || ci < ceiling_boundary.exterior.len() {
            let (fx, fy, fheight) = &floor_boundary.exterior[fi % floor_boundary.exterior.len()];
            let (cx, cy, cheight) =
                &ceiling_boundary.exterior[ci % ceiling_boundary.exterior.len()];

            if approx_eq(&(*fx, *fy), &(*cx, *cy)) {
                merged.push(BoundaryVertex {
                    point: Point2f64::new(*fx, *fy),
                    floor_height: fheight.clone(),
                    ceiling_height: cheight.clone(),
                });
                segment_start = Some((*fx, *fy));
                fi += 1;
                ci += 1;
                // Points coincide → merge
            } else {
                // Points are different but on the same line → choose closest to segment_start
                let start = segment_start.unwrap();
                let dist_floor = ((*fx - start.0).powi(2) + (*fy - start.1).powi(2)).sqrt();
                let dist_ceiling = ((*cx - start.0).powi(2) + (*cy - start.1).powi(2)).sqrt();

                if dist_floor <= dist_ceiling {
                    let interpolated_ceiling = interpolate_ceiling_height(
                        (*fx, *fy),
                        ceiling_boundary.exterior[ci - 1].clone(),
                        ceiling_boundary.exterior[ci].clone(),
                    )?;
                    merged.push(BoundaryVertex {
                        point: Point2f64::new(*fx, *fy),
                        floor_height: fheight.clone(),
                        ceiling_height: interpolated_ceiling,
                    });
                    segment_start = Some((*fx, *fy));
                    fi += 1;
                } else {
                    let floor_height = floor_boundary.exterior[fi - 1].2.last()?;
                    merged.push(BoundaryVertex {
                        point: Point2f64::new(*cx, *cy),
                        floor_height: vec![*floor_height],
                        ceiling_height: cheight.clone(),
                    });
                    segment_start = Some((*cx, *cy));
                    ci += 1;
                }
            }
        }

        Some(merged)
    }
    /// Generate exterior walls from floor and ceiling perimeters
    fn add_from_exterior_perimeter(
        &mut self,
        floor_regions: &FloorRegions,
        ceiling_regions: &CeilingRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_construction_phase: Option<ConstructionPhase>,
        default_exposure: Option<WallExposure>,
    ) {
        let floor_boundary = floor_regions.get_clockwise_3d_polygon_starting_closest_to_origin();
        let ceiling_boundary =
            ceiling_regions.get_clockwise_3d_polygon_starting_closest_to_origin();
        let merged_boundary_vertices =
            Self::merge_floor_ceiling_by_line(&floor_boundary, &ceiling_boundary).expect(
                "Failed to merge floor and ceiling boundaries; ensure they are aligned in XY",
            );
        merged_boundary_vertices
            .iter()
            .enumerate()
            .for_each(|(i, bv)| {
                let next_bv = &merged_boundary_vertices[(i + 1) % merged_boundary_vertices.len()];
                let floor_heights = &bv.floor_height;
                let ceiling_heights = &bv.ceiling_height;
                for &fh in floor_heights {
                    for &ch in ceiling_heights {
                        if (fh - ch).abs() < EPSILON {
                            continue; // No wall if floor and ceiling heights are equal
                        }
                        let vertices = vec![
                            Point3f64::new(bv.point.x, bv.point.y, fh),
                            Point3f64::new(next_bv.point.x, next_bv.point.y, fh),
                            Point3f64::new(next_bv.point.x, next_bv.point.y, ch),
                            Point3f64::new(bv.point.x, bv.point.y, ch),
                        ];
                        // Determine construction phase
                        let construction_phase =
                            default_construction_phase.clone().unwrap_or_else(|| {
                                determine_construction_phase_for_exterior_wall(
                                    floor_regions,
                                    ceiling_regions,
                                    &vertices[0],
                                    &vertices[1],
                                )
                                .unwrap_or(ConstructionPhase::Original)
                            });
                        self.regions.push(WallRegion {
                            polygon: Polygon3D::new(vertices),
                            construction_type: default_construction_type
                                .unwrap_or(WallConstructionType::Uninitialised),
                            base: RegionBase {
                                id: ItemId::default(),
                                construction_makeup: default_construction_makeup
                                    .clone()
                                    .unwrap_or_default(),
                                construction_phase,
                                ..Default::default()
                            },
                            exposure: default_exposure.unwrap_or(WallExposure::Exposed),
                        });
                    }
                }
            });
    }

    /// Merge this WallRegions with another
    fn merge(&mut self, other: WallRegions) {
        self.regions.extend(other.regions);
    }

    /// Modify wall properties for walls that match a 2D polygon (e.g., for user editing)
    fn modify_walls_in_region(
        &mut self,
        polygon_2d: &Polygon<f64>,
        construction_type: Option<WallConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
        exposure: Option<WallExposure>,
    ) {
        for region in &mut self.regions {
            // Check if wall intersects with the 2D polygon
            if wall_intersects_polygon_2d(&region.polygon, polygon_2d) {
                if let Some(ct) = construction_type {
                    region.construction_type = ct;
                }
                if let Some(cl) = &construction_makeup {
                    region.base.construction_makeup = cl.clone();
                }
                if let Some(cp) = &construction_phase {
                    region.base.construction_phase = cp.clone();
                }
                if let Some(exp) = exposure {
                    region.exposure = exp;
                }
            }
        }
    }
}
// Helper structures
struct SharedEdge3D {
    points_a: (Point3f64, Point3f64),
    points_b: (Point3f64, Point3f64),
}

// Helper functions
fn find_shared_edges_2d(
    poly_a: &Polygon<f64>,
    poly_b: &Polygon<f64>,
) -> Vec<((f64, f64), (f64, f64))> {
    let mut shared_edges = Vec::new();

    let coords_a = poly_a.exterior().coords().collect::<Vec<_>>();
    let coords_b = poly_b.exterior().coords().collect::<Vec<_>>();

    for i in 0..coords_a.len() - 1 {
        let edge_a = (coords_a[i], coords_a[i + 1]);

        for j in 0..coords_b.len() - 1 {
            let edge_b = (coords_b[j], coords_b[j + 1]);

            // Check if edges are the same (in either direction)
            if edges_equal_2d(edge_a, edge_b) {
                shared_edges.push(((edge_a.0.x, edge_a.0.y), (edge_a.1.x, edge_a.1.y)));
            }
        }
    }

    shared_edges
}

fn edges_equal_2d(
    edge_a: (&geo::Coord<f64>, &geo::Coord<f64>),
    edge_b: (&geo::Coord<f64>, &geo::Coord<f64>),
) -> bool {
    let same_dir = points_equal_2d(edge_a.0, edge_b.0) && points_equal_2d(edge_a.1, edge_b.1);
    let reverse_dir = points_equal_2d(edge_a.0, edge_b.1) && points_equal_2d(edge_a.1, edge_b.0);
    same_dir || reverse_dir
}

fn points_equal_2d(a: &geo::Coord<f64>, b: &geo::Coord<f64>) -> bool {
    (a.x - b.x).abs() < EPSILON && (a.y - b.y).abs() < EPSILON
}

fn find_shared_edges_3d(poly_a: &Polygon3D, poly_b: &Polygon3D) -> Vec<SharedEdge3D> {
    let mut shared_edges = Vec::new();

    let verts_a = &poly_a.exterior;
    let verts_b = &poly_b.exterior;

    for i in 0..verts_a.len() {
        let j = (i + 1) % verts_a.len();
        let edge_a = (&verts_a[i], &verts_a[j]);

        for k in 0..verts_b.len() {
            let l = (k + 1) % verts_b.len();
            let edge_b = (&verts_b[k], &verts_b[l]);

            // Check if edges share the same 2D projection
            if edges_equal_3d_projection(edge_a, edge_b) {
                shared_edges.push(SharedEdge3D {
                    points_a: (*edge_a.0, *edge_a.1),
                    points_b: (*edge_b.0, *edge_b.1),
                });
            }
        }
    }

    shared_edges
}

fn edges_equal_3d_projection(
    edge_a: (&Point3f64, &Point3f64),
    edge_b: (&Point3f64, &Point3f64),
) -> bool {
    let same_dir = points_equal_3d_xy(edge_a.0, edge_b.0) && points_equal_3d_xy(edge_a.1, edge_b.1);
    let reverse_dir =
        points_equal_3d_xy(edge_a.0, edge_b.1) && points_equal_3d_xy(edge_a.1, edge_b.0);
    same_dir || reverse_dir
}

fn points_equal_3d_xy(a: &Point3f64, b: &Point3f64) -> bool {
    (a.x - b.x).abs() < EPSILON && (a.y - b.y).abs() < EPSILON
}

fn point_in_polygon_3d(polygon: &Polygon3D, x: f64, y: f64) -> bool {
    // Ray casting algorithm in 2D
    let mut inside = false;
    let verts = &polygon.exterior;

    for i in 0..verts.len() {
        let j = (i + 1) % verts.len();
        let vi = &verts[i];
        let vj = &verts[j];

        if ((vi.y > y) != (vj.y > y)) && (x < (vj.x - vi.x) * (y - vi.y) / (vj.y - vi.y) + vi.x) {
            inside = !inside;
        }
    }

    inside
}

fn determine_construction_phase_for_exterior_wall(
    floor_regions: &FloorRegions,
    ceiling_regions: &CeilingRegions,
    v1: &Point3f64,
    v2: &Point3f64,
) -> Option<ConstructionPhase> {
    // Find floor and ceiling regions that contain the edge midpoint
    let mid_x = (v1.x + v2.x) / 2.0;
    let mid_y = (v1.y + v2.y) / 2.0;

    let floor_phase = floor_regions
        .regions
        .iter()
        .find(|r| point_in_polygon_2d(&r.polygon, mid_x, mid_y))
        .map(|r| r.base.construction_phase.clone());

    let ceiling_phase = ceiling_regions
        .regions
        .iter()
        .find(|r| point_in_polygon_3d(&r.polygon, mid_x, mid_y))
        .map(|r| r.base.construction_phase.clone());

    // If both phases match, use that; otherwise return floor phase
    match (floor_phase, ceiling_phase) {
        (Some(fp), Some(cp)) if fp == cp => Some(fp),
        (Some(fp), _) => Some(fp),
        (_, Some(cp)) => Some(cp),
        _ => None,
    }
}

fn point_in_polygon_2d(polygon: &Polygon<f64>, x: f64, y: f64) -> bool {
    let point = geo::Point::new(x, y);
    use geo::Contains;
    polygon.contains(&point)
}

fn wall_intersects_polygon_2d(wall: &Polygon3D, polygon_2d: &Polygon<f64>) -> bool {
    // Check if any wall vertex (projected to 2D) is inside the polygon
    for vertex in &wall.exterior {
        if point_in_polygon_2d(polygon_2d, vertex.x, vertex.y) {
            return true;
        }
    }
    false
}

impl HasBaseRegion for WallRegion {
    fn base_mut(&mut self) -> &mut RegionBase {
        &mut self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use geo::{polygon, Polygon};

    const TOL: f64 = 1e-9;

    /* ---------------------------------------------------------------------
     * Test helpers
     * ------------------------------------------------------------------ */

    fn simple_wall() -> WallRegion {
        WallRegion {
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 3.0),
                Point3f64::new(0.0, 0.0, 3.0),
            ]),
            construction_type: WallConstructionType::Uninitialised,
            base: RegionBase {
                id: ItemId::default(),
                construction_phase: ConstructionPhase::Original,
                ..Default::default()
            },
            exposure: WallExposure::Exposed,
        }
    }

    fn simple_wall_regions() -> WallRegions {
        WallRegions {
            regions: vec![simple_wall()],
        }
    }

    fn simple_floor_boundary(z: f64) -> Polygon3DMultiZ {
        Polygon3DMultiZ {
            exterior: vec![
                (0.0, 0.0, vec![z]),
                (4.0, 0.0, vec![z]),
                (4.0, 4.0, vec![z]),
                (0.0, 4.0, vec![z]),
            ],
        }
    }

    fn assert_unit(v: Vector3f64<f64>) {
        assert!((v.norm() - 1.0).abs() < TOL);
    }

    /* ---------------------------------------------------------------------
     * WallRegionManip
     * ------------------------------------------------------------------ */

    #[test]
    fn leftmost_floor_vertex_is_first_exterior_vertex() {
        let wall = simple_wall();
        assert_eq!(
            wall.get_leftmost_floor_vertex(),
            Point3f64::new(0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn wall_horizontal_dir_is_xy_only() {
        let wall = simple_wall();
        let d = wall.wall_horizontal_dir();
        assert_eq!(d, Vector3f64::new(4.0, 0.0, 0.0));
    }

    #[test]
    fn wall_plane_normal_is_unit_and_perpendicular() {
        let wall = simple_wall();
        let n = wall.wall_plane_normal();

        assert_unit(n);

        let horiz = wall.wall_horizontal_dir();
        assert!((n.dot(&horiz)).abs() < TOL);
    }

    #[test]
    fn dividing_line_geometry_is_consistent() {
        let wall = simple_wall();
        let split = wall.get_dividing_line_equation(2.0, 1.5);

        assert_unit(split.normal);
        assert_unit(split.direction);

        // Direction must lie in the wall plane
        let plane_n = wall.wall_plane_normal();
        assert!((split.direction.dot(&plane_n)).abs() < TOL);

        // Normal must be orthogonal to direction
        assert!((split.direction.dot(&split.normal)).abs() < TOL);
    }

    /* ---------------------------------------------------------------------
     * MergeableRegion
     * ------------------------------------------------------------------ */

    #[test]
    fn merge_compatible_when_all_fields_match() {
        let a = simple_wall();
        let b = simple_wall();
        assert!(a.is_merge_compatible(&b));
    }

    #[test]
    fn merge_incompatible_if_any_field_differs() {
        let mut a = simple_wall();
        let mut b = simple_wall();

        b.exposure = WallExposure::NonHeatLoss;
        assert!(!a.is_merge_compatible(&b));

        b.exposure = a.exposure;
        b.construction_type = WallConstructionType::TimberConstruction;
        assert!(!a.is_merge_compatible(&b));

        b.construction_type = a.construction_type;
        b.base.construction_phase = ConstructionPhase::Extension1;
        assert!(!a.is_merge_compatible(&b));
    }

    #[test]
    fn with_geometry_preserves_metadata() {
        let wall = simple_wall();
        let new_poly = Polygon3D::new(vec![
            Point3f64::new(0.0, 0.0, 0.0),
            Point3f64::new(2.0, 0.0, 0.0),
            Point3f64::new(2.0, 0.0, 3.0),
            Point3f64::new(0.0, 0.0, 3.0),
        ]);

        let w2 = wall.with_geometry(new_poly.clone());

        assert_eq!(w2.polygon.exterior, new_poly.exterior);
        assert_eq!(w2.exposure, wall.exposure);
        assert_eq!(w2.construction_type, wall.construction_type);
        assert_eq!(w2.base.construction_phase, wall.base.construction_phase);
    }

    /* ---------------------------------------------------------------------
     * WallRegionsManip – lookup & split
     * ------------------------------------------------------------------ */

    #[test]
    fn get_wall_region_by_id_mut_returns_correct_region() {
        let mut walls = simple_wall_regions();
        let id = walls.regions[0].base.id;

        let r = walls.get_wall_region_by_id_mut(&id).unwrap();
        r.exposure = WallExposure::NonHeatLoss;

        assert_eq!(walls.regions[0].exposure, WallExposure::NonHeatLoss);
    }

    #[test]
    fn split_wall_region_creates_multiple_regions() {
        let mut walls = simple_wall_regions();
        let id = walls.regions[0].base.id;

        walls.split_wall_region(id, 2.0, 0.0, |_| {});

        assert!(walls.regions.len() >= 2);
    }

    #[test]
    fn split_wall_region_noop_if_id_missing() {
        let mut walls = simple_wall_regions();
        walls.split_wall_region(ItemId::default(), 1.0, 0.0, |_| {});
        assert_eq!(walls.regions.len(), 1);
    }

    /* ---------------------------------------------------------------------
     * Floor / ceiling alignment
     * ------------------------------------------------------------------ */

    #[test]
    fn floor_ceiling_aligned_when_xy_matches() {
        let floor = simple_floor_boundary(0.0);
        let ceiling = simple_floor_boundary(3.0);

        assert!(WallRegions::check_floor_ceiling_aligned(&floor, &ceiling));
    }

    #[test]
    fn floor_ceiling_not_aligned_when_xy_differs() {
        let floor = simple_floor_boundary(0.0);

        let ceiling = Polygon3DMultiZ {
            exterior: vec![
                (1.0, 0.0, vec![3.0]),
                (5.0, 0.0, vec![3.0]),
                (5.0, 4.0, vec![3.0]),
                (1.0, 4.0, vec![3.0]),
            ],
        };

        assert!(!WallRegions::check_floor_ceiling_aligned(&floor, &ceiling));
    }

    /* ---------------------------------------------------------------------
     * merge_floor_ceiling_by_line
     * ------------------------------------------------------------------ */

    #[test]
    fn merge_floor_ceiling_produces_boundary_vertices() {
        let floor = simple_floor_boundary(0.0);
        let ceiling = simple_floor_boundary(3.0);

        let merged = WallRegions::merge_floor_ceiling_by_line(&floor, &ceiling).unwrap();

        assert_eq!(merged.len(), floor.exterior.len());

        for bv in merged {
            assert_eq!(bv.floor_height, vec![0.0]);
            assert_eq!(bv.ceiling_height, vec![3.0]);
        }
    }

    #[test]
    fn merge_floor_ceiling_handles_linear_interpolation() {
        let floor = Polygon3DMultiZ {
            exterior: vec![(0.0, 0.0, vec![0.0]), (2.0, 0.0, vec![0.0])],
        };

        let ceiling = Polygon3DMultiZ {
            exterior: vec![(0.0, 0.0, vec![2.0]), (2.0, 0.0, vec![4.0])],
        };

        let merged = WallRegions::merge_floor_ceiling_by_line(&floor, &ceiling).unwrap();

        assert_eq!(merged.len(), 2);
        assert!((merged[1].ceiling_height[0] - 4.0).abs() < TOL);
    }

    /* ---------------------------------------------------------------------
     * modify_walls_in_region
     * ------------------------------------------------------------------ */

    #[test]
    fn modify_walls_only_affects_intersecting_walls() {
        let mut walls = simple_wall_regions();

        let selection = polygon![
            (x: -1.0, y: -1.0),
            (x:  5.0, y: -1.0),
            (x:  5.0, y:  1.0),
            (x: -1.0, y:  1.0),
        ];

        walls.modify_walls_in_region(
            &selection,
            Some(WallConstructionType::TimberConstruction),
            None,
            Some(ConstructionPhase::Extension1),
            Some(WallExposure::NonHeatLoss),
        );

        let w = &walls.regions[0];
        assert_eq!(
            w.construction_type,
            WallConstructionType::TimberConstruction
        );
        assert_eq!(w.base.construction_phase, ConstructionPhase::Extension1);
        assert_eq!(w.exposure, WallExposure::NonHeatLoss);
    }

    #[test]
    fn modify_walls_noop_when_no_intersection() {
        let mut walls = simple_wall_regions();

        let selection = polygon![
            (x: 10.0, y: 10.0),
            (x: 12.0, y: 10.0),
            (x: 12.0, y: 12.0),
            (x: 10.0, y: 12.0),
        ];

        walls.modify_walls_in_region(
            &selection,
            Some(WallConstructionType::TimberConstruction),
            None,
            None,
            None,
        );

        assert_eq!(
            walls.regions[0].construction_type,
            WallConstructionType::Uninitialised
        );
    }

    /* ---------------------------------------------------------------------
     * merge()
     * ------------------------------------------------------------------ */

    #[test]
    fn merge_extends_regions() {
        let mut a = simple_wall_regions();
        let b = simple_wall_regions();

        a.merge(b);
        assert_eq!(a.regions.len(), 2);
    }
}
