// standard includes
use geo::{LineString, Polygon};
use nalgebra::{Point2, Point3};
use std::collections::HashMap;

use crate::datatypes::ber::common::ConstructionPhase;
use crate::datatypes::ber::structural::{
    ConstructionMakeup, PlanarElement, WallConstructionType, WallExposure,
};

// local includes
use super::ceiling::CeilingRegions;
use super::floor::FloorRegions;
use super::types::{approx_equal_xy, Polygon3D, Polygon3DMultiZ};

const EPSILON: f64 = 1e-9;

/// A vertical planar wall region
#[derive(Clone, Debug)]
pub struct WallRegion {
    pub polygon: Polygon3D, // 3D polygon (vertical plane, typically 4 vertices)
    pub construction_type: WallConstructionType,
    pub construction_makeup: ConstructionMakeup,
    pub construction_phase: ConstructionPhase,
    pub exposure: WallExposure,
}

/// Collection of wall regions
#[derive(Debug)]
pub struct WallRegions {
    pub regions: Vec<WallRegion>,
}

pub struct BoundaryVertex {
    point: Point2<f64>,
    floor_height: Vec<f64>,
    ceiling_height: Vec<f64>,
}

impl WallRegions {
    /// Create a new empty WallRegions
    pub fn new() -> Self {
        Self {
            regions: Vec::new(),
        }
    }

    /// Generate walls from floor region discontinuities (internal walls where floor height changes)
    pub fn from_floor_discontinuities(
        floor_regions: &FloorRegions,
        default_ceiling_height: f64,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_exposure: Option<WallExposure>,
    ) -> Self {
        let mut regions = Vec::new();

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
                        Point3::new(p1.0, p1.1, z1_a),
                        Point3::new(p2.0, p2.1, z1_a),
                        Point3::new(p2.0, p2.1, z1_b),
                        Point3::new(p1.0, p1.1, z1_b),
                    ];

                    // Determine construction phase (use the later one or the first region's)
                    let construction_phase =
                        match (&region_a.construction_phase, &region_b.construction_phase) {
                            (a, b) if a == b => a.clone(),
                            _ => ConstructionPhase::Original,
                        };

                    regions.push(WallRegion {
                        polygon: Polygon3D::new(vertices),
                        construction_type: default_construction_type
                            .unwrap_or(WallConstructionType::Uninitialised),
                        construction_makeup: default_construction_makeup
                            .clone()
                            .unwrap_or_default(),
                        construction_phase,
                        exposure: default_exposure.unwrap_or(WallExposure::Exposed),
                    });
                }
            }
        }

        Self { regions }
    }

    /// Generate walls from ceiling region discontinuities (internal walls where ceiling height jumps)
    pub fn from_ceiling_discontinuities(
        ceiling_regions: &CeilingRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_exposure: Option<WallExposure>,
    ) -> Self {
        let mut regions = Vec::new();

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
                    let construction_phase =
                        match (&region_a.construction_phase, &region_b.construction_phase) {
                            (a, b) if a == b => a.clone(),
                            _ => ConstructionPhase::Original,
                        };

                    regions.push(WallRegion {
                        polygon: Polygon3D::new(vertices),
                        construction_type: default_construction_type
                            .unwrap_or(WallConstructionType::Uninitialised),
                        construction_makeup: default_construction_makeup
                            .clone()
                            .unwrap_or_default(),
                        construction_phase,
                        exposure: default_exposure.unwrap_or(WallExposure::NonHeatLoss),
                    });
                }
            }
        }

        Self { regions }
    }

    fn check_floor_ceiling_aligned(
        floor_boundary: &Polygon3DMultiZ,
        ceiling_boundary: &Polygon3DMultiZ,
    ) -> bool {
        const EPSILON: f64 = 1e-9;

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
        let (cx, cy, cheight) = &ceiling_boundary.exterior[ci];

        merged.push(BoundaryVertex {
            point: Point2::new(*fx, *fy),
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
                    point: Point2::new(*fx, *fy),
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
                    let mut interpolated_ceiling = Vec::new();
                    interpolated_ceiling = interpolate_ceiling_height(
                        (*fx, *fy),
                        ceiling_boundary.exterior[ci - 1].clone(),
                        ceiling_boundary.exterior[ci].clone(),
                    )?;
                    merged.push(BoundaryVertex {
                        point: Point2::new(*fx, *fy),
                        floor_height: fheight.clone(),
                        ceiling_height: interpolated_ceiling,
                    });
                    segment_start = Some((*fx, *fy));
                    fi += 1;
                } else {
                    let floor_height = floor_boundary.exterior[fi - 1].2.last()?;
                    merged.push(BoundaryVertex {
                        point: Point2::new(*cx, *cy),
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
    pub fn from_exterior_perimeter(
        floor_regions: &FloorRegions,
        ceiling_regions: &CeilingRegions,
        default_construction_type: Option<WallConstructionType>,
        default_construction_makeup: Option<ConstructionMakeup>,
        default_construction_phase: Option<ConstructionPhase>,
        default_exposure: Option<WallExposure>,
    ) -> Self {
        let mut regions = Vec::new();

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
                            Point3::new(bv.point.x, bv.point.y, fh),
                            Point3::new(next_bv.point.x, next_bv.point.y, fh),
                            Point3::new(next_bv.point.x, next_bv.point.y, ch),
                            Point3::new(bv.point.x, bv.point.y, ch),
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
                        regions.push(WallRegion {
                            polygon: Polygon3D::new(vertices),
                            construction_type: default_construction_type
                                .unwrap_or(WallConstructionType::Uninitialised),
                            construction_makeup: default_construction_makeup
                                .clone()
                                .unwrap_or_default(),
                            construction_phase,
                            exposure: default_exposure.unwrap_or(WallExposure::Exposed),
                        });
                    }
                }
            });

        Self { regions }
    }

    /// Merge this WallRegions with another
    pub fn merge(&mut self, other: WallRegions) {
        self.regions.extend(other.regions);
    }

    /// Modify wall properties for walls that match a 2D polygon (e.g., for user editing)
    pub fn modify_walls_in_region(
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
                    region.construction_makeup = cl.clone();
                }
                if let Some(cp) = &construction_phase {
                    region.construction_phase = cp.clone();
                }
                if let Some(exp) = exposure {
                    region.exposure = exp;
                }
            }
        }
    }
}

impl From<Vec<PlanarElement>> for WallRegions {
    fn from(elements: Vec<PlanarElement>) -> Self {
        let regions = elements
            .into_iter()
            .map(|el| match el {
                PlanarElement::Wall {
                    common,
                    wall_specifics,
                } => WallRegion {
                    polygon: Polygon3D::new(common.vertices),
                    construction_type: wall_specifics.construction_type,
                    construction_makeup: common.construction,
                    construction_phase: common.construction_phase,
                    exposure: wall_specifics.exposure,
                },
                _ => panic!("Expected PlanarElement::Wall"),
            })
            .collect();
        Self { regions }
    }
}
// Helper structures
struct SharedEdge3D {
    points_a: (Point3<f64>, Point3<f64>),
    points_b: (Point3<f64>, Point3<f64>),
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
    edge_a: (&Point3<f64>, &Point3<f64>),
    edge_b: (&Point3<f64>, &Point3<f64>),
) -> bool {
    let same_dir = points_equal_3d_xy(edge_a.0, edge_b.0) && points_equal_3d_xy(edge_a.1, edge_b.1);
    let reverse_dir =
        points_equal_3d_xy(edge_a.0, edge_b.1) && points_equal_3d_xy(edge_a.1, edge_b.0);
    same_dir || reverse_dir
}

fn points_equal_3d_xy(a: &Point3<f64>, b: &Point3<f64>) -> bool {
    (a.x - b.x).abs() < EPSILON && (a.y - b.y).abs() < EPSILON
}

fn extract_floor_boundary_with_all_vertices(floor_regions: &FloorRegions) -> Vec<Point3<f64>> {
    // Get the outer boundary of the union of all floor regions
    // This is simplified - you may need to use the geo library's union operation

    let mut all_vertices = Vec::new();
    let mut vertex_map: HashMap<(i64, i64), Point3<f64>> = HashMap::new();

    for region in &floor_regions.regions {
        for coord in region.polygon.exterior().coords() {
            let key = ((coord.x * 1e9) as i64, (coord.y * 1e9) as i64);
            vertex_map
                .entry(key)
                .or_insert(Point3::new(coord.x, coord.y, region.height));
        }
    }

    // Extract boundary (simplified - assumes convex hull or proper ordering)
    all_vertices.extend(vertex_map.values().cloned());
    all_vertices
}

fn collect_ceiling_boundary_vertices(
    ceiling_regions: &CeilingRegions,
    floor_boundary: &[Point3<f64>],
) -> Vec<Point3<f64>> {
    let mut vertices = Vec::new();

    for region in &ceiling_regions.regions {
        for vertex in &region.polygon.exterior {
            // Check if this vertex lies on the floor boundary (2D projection)
            if is_on_boundary_2d(vertex, floor_boundary) {
                vertices.push(*vertex);
            }
        }
    }

    vertices
}

fn is_on_boundary_2d(point: &Point3<f64>, boundary: &[Point3<f64>]) -> bool {
    for i in 0..boundary.len() {
        let j = (i + 1) % boundary.len();
        if point_on_segment_2d(point, &boundary[i], &boundary[j]) {
            return true;
        }
    }
    false
}

fn point_on_segment_2d(p: &Point3<f64>, a: &Point3<f64>, b: &Point3<f64>) -> bool {
    let cross = (p.y - a.y) * (b.x - a.x) - (p.x - a.x) * (b.y - a.y);
    if cross.abs() > EPSILON {
        return false;
    }

    let dot = (p.x - a.x) * (b.x - a.x) + (p.y - a.y) * (b.y - a.y);
    let len_sq = (b.x - a.x).powi(2) + (b.y - a.y).powi(2);

    dot >= -EPSILON && dot <= len_sq + EPSILON
}

fn merge_boundary_vertices(
    floor_boundary: &[Point3<f64>],
    ceiling_vertices: &[Point3<f64>],
) -> Vec<Point3<f64>> {
    let mut merged = floor_boundary.to_vec();

    // Insert ceiling vertices into the floor boundary sequence
    for cv in ceiling_vertices {
        let mut inserted = false;

        for i in 0..merged.len() {
            let j = (i + 1) % merged.len();

            if point_on_segment_2d(cv, &merged[i], &merged[j]) {
                // Check if not already present
                let already_exists =
                    points_equal_3d_xy(cv, &merged[i]) || points_equal_3d_xy(cv, &merged[j]);

                if !already_exists {
                    merged.insert(j, *cv);
                    inserted = true;
                    break;
                }
            }
        }
    }

    merged
}

fn find_ceiling_height_at_point(ceiling_regions: &CeilingRegions, x: f64, y: f64) -> f64 {
    // Find which ceiling region contains this point
    for region in &ceiling_regions.regions {
        if point_in_polygon_3d(&region.polygon, x, y) {
            // Interpolate or use the ceiling height at this point
            return interpolate_ceiling_height(&region.polygon, x, y);
        }
    }

    // Default fallback
    0.0
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

fn interpolate_ceiling_height(polygon: &Polygon3D, x: f64, y: f64) -> f64 {
    // Simple average of all vertices (could be improved with proper interpolation)
    let sum: f64 = polygon.exterior.iter().map(|v| v.z).sum();
    sum / polygon.exterior.len() as f64
}

fn determine_construction_phase_for_exterior_wall(
    floor_regions: &FloorRegions,
    ceiling_regions: &CeilingRegions,
    v1: &Point3<f64>,
    v2: &Point3<f64>,
) -> Option<ConstructionPhase> {
    // Find floor and ceiling regions that contain the edge midpoint
    let mid_x = (v1.x + v2.x) / 2.0;
    let mid_y = (v1.y + v2.y) / 2.0;

    let floor_phase = floor_regions
        .regions
        .iter()
        .find(|r| point_in_polygon_2d(&r.polygon, mid_x, mid_y))
        .map(|r| r.construction_phase.clone());

    let ceiling_phase = ceiling_regions
        .regions
        .iter()
        .find(|r| point_in_polygon_3d(&r.polygon, mid_x, mid_y))
        .map(|r| r.construction_phase.clone());

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_wall_creation() {
        // Add tests here
    }
}
