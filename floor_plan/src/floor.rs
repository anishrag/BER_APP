// standard includes
use geo::algorithm::Contains;
use geo::bool_ops::BooleanOps;
use geo::{Intersects, LineString, MultiPolygon, Polygon};
use itertools::izip;
use nalgebra::Point2;
use std::collections::HashMap;
use uuid::Uuid;

use datatypes::ber::common::ConstructionPhase;
use datatypes::ber::structural::{ConstructionMakeup, FloorConstructionType, PlanarElement};

// local includes
use super::types::{Polygon3D, Polygon3DMultiZ, RegionBase};

/// A planar region of the floor
#[derive(Clone, Debug)]
pub struct FloorRegion {
    pub polygon: Polygon<f64>, // 2D polygon boundary
    pub height: f64,           // height per vertex
    pub construction_type: FloorConstructionType,
    pub base: RegionBase,
}
/// A planar region of the floor
#[derive(Clone, Debug)]
pub struct FloorRegions {
    pub regions: Vec<FloorRegion>, // height per vertex
}

#[derive(Clone, Copy, Debug)]
struct DirectedEdge {
    from: Point2<f64>,
    to: Point2<f64>,
    height: f64,
}
#[derive(Debug)]
pub enum MergeWarning {
    InvalidIds(Vec<Uuid>),
    MultipleResultingRegions {
        source_ids: Vec<Uuid>,
        resulting_regions: usize,
    },
}

impl FloorRegions {
    /// Create a new FloorRegions
    pub fn new(
        vertices: Vec<Point2<f64>>,
        construction_type: Option<FloorConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
    ) -> Self {
        let polygon = Polygon::new(
            LineString::from(vertices.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
            vec![],
        );
        Self {
            regions: vec![FloorRegion {
                polygon,
                height: 0.0f64,
                construction_type: construction_type.unwrap_or(FloorConstructionType::SolidFloor),
                base: RegionBase {
                    id: Uuid::new_v4(),
                    construction_makeup: construction_makeup.unwrap_or_default(),
                    construction_phase: construction_phase.unwrap_or(ConstructionPhase::Original),
                },
            }],
        }
    }

    pub fn from_polygon_3ds(
        polygon_3ds: Vec<&Polygon3D>,
        construction_type: Vec<Option<FloorConstructionType>>,
        construction_makeup: Vec<Option<ConstructionMakeup>>,
        construction_phase: Vec<Option<ConstructionPhase>>,
    ) -> Self {
        let mut regions: Vec<FloorRegion> = Vec::new();

        for (polygon, c_type, c_makeup, c_phase) in izip!(
            &polygon_3ds,
            &construction_type,
            &construction_makeup,
            &construction_phase
        ) {
            let (polygon2d, heights) = polygon.to_2d_polygon_and_heights();
            //assert all heights same
            assert!(
                heights.iter().all(|&h| approx_eq(h, heights[0])),
                "All heights in Polygon3D must be the same to convert to FloorRegion"
            );
            let height = heights[0];

            regions.push(FloorRegion {
                polygon: polygon2d,
                height,
                construction_type: c_type.unwrap_or(FloorConstructionType::SolidFloor),
                base: RegionBase {
                    id: Uuid::new_v4(),
                    construction_makeup: c_makeup.clone().unwrap_or_default(),
                    construction_phase: c_phase.clone().unwrap_or(ConstructionPhase::Original),
                },
            });
        }
        FloorRegions { regions }
    }

    pub fn from_polygons(
        polygons: Vec<&Polygon<f64>>,
        heights: Vec<f64>,
        construction_type: Vec<Option<FloorConstructionType>>,
        construction_makeup: Vec<Option<ConstructionMakeup>>,
        construction_phase: Vec<Option<ConstructionPhase>>,
    ) -> Self {
        let mut regions: Vec<FloorRegion> = Vec::new();
        for (polygon, height, c_type, c_makeup, c_phase) in izip!(
            &polygons,
            &heights,
            &construction_type,
            &construction_makeup,
            &construction_phase
        ) {
            regions.push(FloorRegion {
                polygon: (*polygon).clone(),
                height: *height,
                construction_type: c_type.unwrap_or(FloorConstructionType::SolidFloor),
                base: RegionBase {
                    id: Uuid::new_v4(),
                    construction_makeup: c_makeup.clone().unwrap_or_default(),
                    construction_phase: c_phase.clone().unwrap_or(ConstructionPhase::Original),
                },
            });
        }
        FloorRegions { regions }
    }

    pub fn get_floor_region_by_id(&self, id: &Uuid) -> Option<&FloorRegion> {
        self.regions.iter().find(|r| &r.base.id == id)
    }
    pub fn merge_regions(&self, ids: &[Uuid]) -> (Vec<FloorRegion>, Vec<MergeWarning>) {
        let mut warnings = Vec::new();
        let mut selected: Vec<&FloorRegion> = self
            .regions
            .iter()
            .filter(|r| ids.contains(&r.base.id))
            .collect();

        // warn for invalid IDs
        let invalid_ids: Vec<Uuid> = ids
            .iter()
            .filter(|id| !self.regions.iter().any(|r| &r.base.id == *id))
            .copied()
            .collect();
        if !invalid_ids.is_empty() {
            warnings.push(MergeWarning::InvalidIds(invalid_ids));
        }

        let mut merged = Vec::new();

        while let Some(region) = selected.pop() {
            let mut group = vec![region.clone()];

            selected.retain(|other| {
                if other.height == region.height
                    && other.construction_type == region.construction_type
                    && other.base.construction_makeup == region.base.construction_makeup
                    && other.base.construction_phase == region.base.construction_phase
                {
                    group.push((*other).clone());
                    false
                } else {
                    true
                }
            });

            let merged_multi: MultiPolygon<f64> = group
                .iter()
                .map(|r| r.polygon.clone())
                .fold(MultiPolygon::new(vec![]), |acc, poly| acc.union(&poly));

            let source_ids: Vec<Uuid> = group.iter().map(|r| r.base.id).collect();

            for poly in merged_multi.0.clone() {
                merged.push(FloorRegion {
                    polygon: poly,
                    height: region.height,
                    construction_type: region.construction_type,
                    base: region.base.clone(),
                });
            }

            if merged_multi.0.len() > 1 {
                warnings.push(MergeWarning::MultipleResultingRegions {
                    source_ids,
                    resulting_regions: merged_multi.0.len(),
                });
            }
        }

        (merged, warnings)
    }
    fn modify_polygon<F>(&mut self, polygon: Polygon<f64>, mut modify: F)
    where
        F: FnMut(&mut FloorRegion),
    {
        let mut new_regions = Vec::new();
        let mut regions_to_remove = Vec::new();

        for (idx, region) in self.regions.iter().enumerate() {
            let intersection = region.polygon.intersection(&polygon);
            if intersection.0.is_empty() {
                continue;
            }

            let remainder = region.polygon.difference(&polygon);
            regions_to_remove.push(idx);

            // remainder
            let mut id = region.base.id;
            for poly in remainder.0 {
                new_regions.push(FloorRegion {
                    polygon: poly,
                    height: region.height,
                    construction_type: region.construction_type,
                    base: RegionBase {
                        id,
                        construction_makeup: region.base.construction_makeup.clone(),
                        construction_phase: region.base.construction_phase.clone(),
                    },
                });
                id = Uuid::new_v4(); // ensure new ID for subsequent remainders
            }

            // intersection
            for poly in intersection.0 {
                let mut new_region = region.clone();
                new_region.polygon = poly;
                modify(&mut new_region);
                new_regions.push(new_region);
            }
        }

        for idx in regions_to_remove.into_iter().rev() {
            self.regions.remove(idx);
        }

        self.regions.extend(new_regions);
    }

    pub fn raise_lower_polygon(&mut self, polygon: Polygon<f64>, height_change: f64) {
        self.modify_polygon(polygon, |r| {
            r.height += height_change;
        });
    }

    pub fn modify_polygon_construction_phase(
        &mut self,
        polygon: Polygon<f64>,
        construction_phase: ConstructionPhase,
    ) {
        self.modify_polygon(polygon, |r| {
            r.base.construction_phase = construction_phase.clone();
        });
    }

    pub fn modify_polygon_all_fields(
        &mut self,
        polygon: Polygon<f64>,
        height: Option<f64>,
        construction_type: Option<FloorConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
    ) {
        self.modify_polygon(polygon, |r| {
            if let Some(h) = height {
                r.height = h;
            }
            if let Some(ct) = construction_type {
                r.construction_type = ct;
            }
            if let Some(cl) = construction_makeup.clone() {
                r.base.construction_makeup = cl;
            }
            if let Some(cp) = construction_phase.clone() {
                r.base.construction_phase = cp;
            }
        });
    }

    pub fn by_construction_phase(&self) -> Vec<FloorRegionByConstructionPhase> {
        let mut per_construction_phase: HashMap<ConstructionPhase, Vec<Polygon<f64>>> =
            HashMap::new();

        // 1. Group polygons by year
        for region in &self.regions {
            per_construction_phase
                .entry(region.base.construction_phase.clone())
                .or_default()
                .push(region.polygon.clone());
        }

        let mut result = Vec::new();

        // 2. Union per year
        for (construction_phase, polygons) in per_construction_phase {
            let mut acc: Option<MultiPolygon<f64>> = None;

            for poly in polygons {
                acc = Some(match acc {
                    None => MultiPolygon(vec![poly]),
                    Some(mp) => mp.union(&poly),
                });
            }

            // 3. Emit resulting polygons
            if let Some(mp) = acc {
                for poly in mp.0 {
                    result.push(FloorRegionByConstructionPhase {
                        polygon: poly,
                        construction_phase: construction_phase.clone(),
                    });
                }
            }
        }
        result
    }

    pub fn get_floor_height_at_point(&self, point: &Point2<f64>) -> Vec<f64> {
        let mut heights = Vec::new();
        for region in &self.regions {
            if region.polygon.contains(&geo::Point::new(point.x, point.y)) {
                heights.push(region.height);
            }
        }
        heights
    }

    pub fn get_clockwise_3d_polygon_starting_closest_to_origin(&self) -> Polygon3DMultiZ {
        // ------------------------------------------------------------
        // 1. Collect all directed edges from all regions
        // ------------------------------------------------------------
        let mut all_edges: Vec<DirectedEdge> = Vec::new();

        for region in &self.regions {
            let coords: Vec<Point2<f64>> = region
                .polygon
                .exterior()
                .points()
                .map(|p| Point2::new(p.x(), p.y()))
                .collect();

            if coords.len() < 2 {
                continue;
            }

            // skip duplicate closing point
            for i in 0..coords.len() - 1 {
                all_edges.push(DirectedEdge {
                    from: coords[i],
                    to: coords[i + 1],
                    height: region.height,
                });
            }
            all_edges.push(DirectedEdge {
                from: coords[coords.len() - 1],
                to: coords[0],
                height: region.height,
            });
        }

        if all_edges.is_empty() {
            return Polygon3DMultiZ::new(vec![]);
        }

        // ------------------------------------------------------------
        // 2. Cancel interior edges (A→B cancels B→A)
        // ------------------------------------------------------------
        let mut exterior_edges: Vec<DirectedEdge> = Vec::new();

        'edge_loop: for (i, e) in all_edges.iter().enumerate() {
            if all_edges
                .iter()
                .enumerate()
                .any(|(j, other)| j != i && edges_match(e, other))
            {
                continue 'edge_loop;
            }
            exterior_edges.push(*e);
        }

        if exterior_edges.is_empty() {
            return Polygon3DMultiZ::new(vec![]);
        }

        // ------------------------------------------------------------
        // 3. Reconstruct ordered boundary edge loop
        // ------------------------------------------------------------
        let mut boundary_edges: Vec<DirectedEdge> = Vec::new();

        let start_edge = exterior_edges[0];
        boundary_edges.push(start_edge);

        loop {
            let last = boundary_edges.last().unwrap();

            let mut found = false;

            for e in &exterior_edges {
                if e.from == last.to {
                    // avoid immediately going backwards
                    if boundary_edges.len() > 1 {
                        let prev = boundary_edges[boundary_edges.len() - 2];
                        if e.to == prev.from {
                            continue;
                        }
                    }

                    if e.to == start_edge.from {
                        boundary_edges.push(*e);
                        found = true;
                        break;
                    }

                    boundary_edges.push(*e);
                    found = true;
                    break;
                }
            }

            if !found || boundary_edges.last().unwrap().to == start_edge.from {
                break;
            }
        }
        // ------------------------------------------------------------
        // 4. Ensure clockwise orientation
        // ------------------------------------------------------------
        let mut signed_area = 0.0;

        for e in &boundary_edges {
            let pt1 = e.from;
            let pt2 = e.to;
            signed_area += pt1.x * pt2.y - pt2.x * pt1.y;
        }
        if signed_area > 0.0 {
            boundary_edges = boundary_edges
                .iter()
                .rev()
                .map(|e| {
                    let temp = e.from;
                    DirectedEdge {
                        from: e.to,
                        to: temp,
                        height: e.height,
                    }
                })
                .collect();
        }
        // ------------------------------------------------------------
        // 5. Build traversal-consistent multi-Z vertices
        // ------------------------------------------------------------
        let mut points_with_z: Vec<(f64, f64, Vec<f64>)> = Vec::new();

        for i in 0..boundary_edges.len() {
            let incoming = &boundary_edges[i];
            let outgoing = &boundary_edges[(i + 1) % boundary_edges.len()];

            let pt = incoming.to;

            let mut heights = Vec::new();
            heights.push(incoming.height);

            if outgoing.height != incoming.height {
                heights.push(outgoing.height);
            }

            points_with_z.push((pt.x, pt.y, heights));
        }

        // ------------------------------------------------------------
        // 6. Rotate so closest point to origin is first
        // ------------------------------------------------------------
        let closest_idx = points_with_z
            .iter()
            .enumerate()
            .min_by(|(_, (x1, y1, _)), (_, (x2, y2, _))| {
                let d1 = x1 * x1 + y1 * y1;
                let d2 = x2 * x2 + y2 * y2;
                d1.partial_cmp(&d2).unwrap()
            })
            .map(|(i, _)| i)
            .unwrap_or(0);

        points_with_z.rotate_left(closest_idx);

        Polygon3DMultiZ::new(points_with_z)
    }
}
/// Canonical (undirected) edge key for cancellation
fn edge_key(a: Point2<f64>, b: Point2<f64>) -> (Point2<f64>, Point2<f64>) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn points_match(p1: &Point2<f64>, p2: &Point2<f64>) -> bool {
    approx_eq(p1.x, p2.x) && approx_eq(p1.y, p2.y)
}

fn edges_match(a: &DirectedEdge, b: &DirectedEdge) -> bool {
    (points_match(&a.from, &b.from) && points_match(&a.to, &b.to))
        || (points_match(&a.from, &b.to) && points_match(&a.to, &b.from))
}

#[derive(Debug)]
pub struct FloorRegionByConstructionPhase {
    pub polygon: Polygon<f64>,
    pub construction_phase: ConstructionPhase,
}

impl From<Vec<PlanarElement>> for FloorRegions {
    fn from(elements: Vec<PlanarElement>) -> Self {
        let regions = elements
            .into_iter()
            .map(|el| match el {
                PlanarElement::Floor {
                    common,
                    floor_specifics,
                } => FloorRegion {
                    polygon: Polygon::new(
                        LineString::from(
                            common
                                .vertices
                                .iter()
                                .map(|p| (p.x, p.y))
                                .collect::<Vec<(f64, f64)>>(),
                        ),
                        vec![],
                    ),
                    height: 0.0,
                    construction_type: floor_specifics.construction_type,
                    base: RegionBase {
                        id: common.id,
                        construction_makeup: common.construction.clone(),
                        construction_phase: common.construction_phase.clone(),
                    },
                },
                _ => panic!("Non-floor PlanarElement found when converting to FloorRegions"),
            })
            .collect();
        FloorRegions { regions }
    }
}

impl Into<Vec<PlanarElement>> for FloorRegions {
    fn into(self) -> Vec<PlanarElement> {
        self.regions
            .into_iter()
            .map(|region| {
                let vertices: Vec<nalgebra::Point3<f64>> = region
                    .polygon
                    .exterior()
                    .points()
                    .map(|p| nalgebra::Point3::new(p.x(), p.y(), region.height))
                    .collect();
                PlanarElement::new_floor(
                    vertices,
                    region.base.construction_phase.clone(),
                    region.base.construction_makeup.clone(),
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::types::{Polygon3D, Polygon3DConversionError};
    use datatypes::ber::structural::{ConstructionLayer, ConstructionMaterial};

    use nalgebra::Point3;
    fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon<f64> {
        Polygon::new(
            LineString::from(vec![
                (min_x, min_y),
                (max_x, min_y),
                (max_x, max_y),
                (min_x, max_y),
                (min_x, min_y), // close the loop
            ]),
            vec![], // no holes
        )
    }

    fn base_floor() -> FloorRegions {
        let vertices = vec![
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(10.0, 10.0),
            Point2::new(0.0, 10.0),
        ];

        FloorRegions::new(vertices, None, None, None)
    }

    #[test]
    fn new_creates_single_region() {
        let floor = base_floor();

        assert_eq!(floor.regions.len(), 1);
        assert_eq!(floor.regions[0].height, 0.0);
        assert_eq!(
            floor.regions[0].construction_type,
            FloorConstructionType::SolidFloor
        );
        assert_eq!(
            floor.regions[0].base.construction_makeup,
            ConstructionMakeup::default()
        );
        assert_eq!(
            floor.regions[0].base.construction_phase,
            ConstructionPhase::Original
        );
    }

    #[test]
    fn raise_polygon_splits_region_and_changes_height() {
        let mut floor = base_floor();

        let raise_poly = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.raise_lower_polygon(raise_poly, 2.5);

        assert_eq!(floor.regions.len(), 2);

        let raised = floor.regions.iter().filter(|r| r.height == 2.5).count();
        let flat = floor.regions.iter().filter(|r| r.height == 0.0).count();

        assert_eq!(raised, 1);
        assert_eq!(flat, 1);
    }

    #[test]
    fn change_year_polygon_updates_only_intersection() {
        let mut floor = base_floor();

        let poly = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.modify_polygon_construction_phase(poly, ConstructionPhase::Extension1);

        assert_eq!(floor.regions.len(), 2);

        let phases: Vec<ConstructionPhase> = floor
            .regions
            .iter()
            .map(|r| r.base.construction_phase.clone())
            .collect();

        assert!(phases.contains(&ConstructionPhase::Original));
        assert!(phases.contains(&ConstructionPhase::Extension1));
    }

    #[test]
    fn change_id_polygon_allocates_new_id_same_year() {
        let mut floor = base_floor();

        let poly = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.modify_polygon_all_fields(
            poly,
            None,
            None,
            Some(ConstructionMakeup::Layers(vec![
                ConstructionLayer {
                    thickness_mm: 200,
                    material: ConstructionMaterial::Concrete,
                    bridged: false,
                },
                ConstructionLayer {
                    thickness_mm: 100,
                    material: ConstructionMaterial::InsulationEPS,
                    bridged: false,
                },
            ])),
            None,
        );

        assert_eq!(floor.regions.len(), 2);

        let num_regions = floor.regions.len();
        let construction_phases = floor
            .regions
            .iter()
            .map(|r| r.base.construction_phase.clone())
            .collect::<Vec<_>>();

        assert_eq!(num_regions, 2);
        assert_eq!(construction_phases[1], ConstructionPhase::Original);
        assert_eq!(construction_phases[0], ConstructionPhase::Original);
    }
    //
    #[test]
    fn add_construction_makeup_for_part_of_multiple_polygons_should_create_extra_polys() {
        let mut floor = base_floor();

        let left = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.modify_polygon_construction_phase(left, ConstructionPhase::Extension1);

        let spanning = rectangle(0.0, 0.0, 10.0, 5.0);
        floor.modify_polygon_all_fields(
            spanning,
            None,
            None,
            Some(ConstructionMakeup::Layers(vec![
                ConstructionLayer {
                    thickness_mm: 200,
                    material: ConstructionMaterial::Concrete,
                    bridged: false,
                },
                ConstructionLayer {
                    thickness_mm: 100,
                    material: ConstructionMaterial::InsulationEPS,
                    bridged: false,
                },
            ])),
            None,
        );
        let num_regions = floor.regions.len();
        assert_eq!(num_regions, 4);
    }

    #[test]
    fn construction_phase_unions_regions_per_phase() {
        let mut floor = base_floor();

        let left = rectangle(0.0, 0.0, 5.0, 10.0);
        let right = rectangle(5.0, 0.0, 10.0, 10.0);

        floor.modify_polygon_construction_phase(left, ConstructionPhase::Extension1);
        floor.modify_polygon_construction_phase(right, ConstructionPhase::Extension1);

        let by_construction_phase = floor.by_construction_phase();

        let regions: Vec<_> = by_construction_phase
            .iter()
            .filter(|r| r.construction_phase == ConstructionPhase::Extension1)
            .collect();
        assert_eq!(regions.len(), 1);
    }

    #[test]
    fn height_is_preserved_for_remainder() {
        let mut floor = base_floor();

        floor.raise_lower_polygon(rectangle(0.0, 0.0, 10.0, 10.0), 1.0);
        floor.raise_lower_polygon(rectangle(0.0, 0.0, 5.0, 10.0), 2.0);

        let heights: Vec<f64> = floor.regions.iter().map(|r| r.height).collect();

        assert!(heights.contains(&3.0)); // overlap region
        assert!(heights.contains(&1.0)); // remainder region
    }

    #[test]
    fn test_single_region_3d_polygon() {
        // Create a simple square region at height 1.0
        let vertices = vec![
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 4.0),
            Point2::new(0.0, 4.0),
        ];

        let mut regions = FloorRegions::new(
            vertices,
            Some(FloorConstructionType::SolidFloor),
            None,
            Some(ConstructionPhase::Original),
        );
        regions.regions[0].height = 1.0;

        let polygon_3d = regions.get_clockwise_3d_polygon_starting_closest_to_origin();

        // Should have 4 vertices (+ closing point potentially, depending on geo crate)
        assert!(!polygon_3d.exterior.is_empty());

        // All z values should be single (no discontinuities)
        for (x, y, z_values) in &polygon_3d.exterior {
            assert_eq!(
                z_values.len(),
                1,
                "Point ({}, {}) should have single z value",
                x,
                y
            );
            assert_eq!(z_values[0], 1.0, "Height should be 1.0");
        }

        // First point should be closest to origin (0, 0)
        let first = &polygon_3d.exterior[0];
        let first_dist = first.0 * first.0 + first.1 * first.1;
        for point in &polygon_3d.exterior {
            let dist = point.0 * point.0 + point.1 * point.1;
            assert!(
                first_dist <= dist,
                "First point should be closest to origin"
            );
        }

        // Test conversion to Polygon3D (should succeed)
        let simple_polygon = Polygon3D::try_from(&polygon_3d);
        assert!(simple_polygon.is_ok());
    }

    #[test]
    fn test_two_regions_with_discontinuity() {
        // Create two adjacent regions with different heights
        // Region 1: left square at height 1.0
        let vertices1 = vec![
            Point2::new(0.0, 0.0),
            Point2::new(2.0, 0.0),
            Point2::new(2.0, 2.0),
            Point2::new(0.0, 2.0),
        ];

        // Region 2: right square at height 3.0
        let vertices2 = vec![
            Point2::new(2.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(4.0, 2.0),
            Point2::new(2.0, 2.0),
        ];

        let mut regions = FloorRegions::new(
            vertices1,
            Some(FloorConstructionType::SolidFloor),
            None,
            Some(ConstructionPhase::Original),
        );
        regions.regions[0].height = 1.0;

        // Add second region
        let polygon2 = Polygon::new(
            LineString::from(
                vertices2
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        regions.regions.push(FloorRegion {
            polygon: polygon2,
            height: 3.0,
            construction_type: FloorConstructionType::SolidFloor,
            base: RegionBase {
                id: Uuid::new_v4(),
                construction_makeup: ConstructionMakeup::default(),
                construction_phase: ConstructionPhase::Original,
            },
        });

        let polygon_3d = regions.get_clockwise_3d_polygon_starting_closest_to_origin();

        println!("3D Polygon: {:?}", polygon_3d);
        // Find points on the shared edge (x=2.0)
        let shared_edge_points: Vec<_> = polygon_3d
            .exterior
            .iter()
            .filter(|(x, _, _)| (*x - 2.0).abs() < 1e-6)
            .collect();

        println!("Shared edge points: {:?}", shared_edge_points);
        // There should be at least 2 points on the shared edge
        assert!(
            shared_edge_points.len() >= 2,
            "Should have points on shared edge"
        );

        // Points on the shared edge should have multiple z values (discontinuity)
        for (x, y, z_values) in shared_edge_points {
            assert_eq!(
                z_values.len(),
                2,
                "Point ({}, {}) on shared edge should have 2 z values (discontinuity)",
                x,
                y
            );
            assert!(z_values.contains(&1.0), "Should contain height 1.0");
            assert!(z_values.contains(&3.0), "Should contain height 3.0");
        }

        // Conversion to Polygon3D should fail due to discontinuities
        let simple_polygon = Polygon3D::try_from(&polygon_3d);
        assert!(simple_polygon.is_err());
        if let Err(Polygon3DConversionError::HasDiscontinuity { z_values, .. }) = simple_polygon {
            assert_eq!(z_values.len(), 2);
        } else {
            panic!("Expected HasDiscontinuity error");
        }
    }

    #[test]
    fn test_polygon_3d_conversion_roundtrip() {
        // Create a simple Polygon3D
        let points = vec![
            Point3::new(0.0, 0.0, 1.0),
            Point3::new(1.0, 0.0, 1.5),
            Point3::new(1.0, 1.0, 2.0),
            Point3::new(0.0, 1.0, 1.5),
        ];
        let polygon_3d = Polygon3D::new(points.clone());

        // Convert to MultiZ
        let multi_z = Polygon3DMultiZ::from(polygon_3d.clone());

        // Check that each point has single z value
        assert_eq!(multi_z.exterior.len(), 4);
        for (idx, (x, y, z_values)) in multi_z.exterior.iter().enumerate() {
            assert_eq!(z_values.len(), 1);
            assert_eq!(*x, points[idx].x);
            assert_eq!(*y, points[idx].y);
            assert_eq!(z_values[0], points[idx].z);
        }

        // Convert back to Polygon3D
        let roundtrip = Polygon3D::try_from(&multi_z).unwrap();

        // Should match original
        assert_eq!(roundtrip.exterior.len(), polygon_3d.exterior.len());
        for (idx, point) in roundtrip.exterior.iter().enumerate() {
            assert_eq!(point.x, polygon_3d.exterior[idx].x);
            assert_eq!(point.y, polygon_3d.exterior[idx].y);
            assert_eq!(point.z, polygon_3d.exterior[idx].z);
        }
    }
    #[test]
    fn test_clockwise_ordering() {
        use nalgebra::Point2;

        // Create a simple square
        let vertices = vec![
            Point2::new(0.0, 0.0),
            Point2::new(1.0, 0.0),
            Point2::new(1.0, 1.0),
            Point2::new(0.0, 1.0),
        ];

        let regions = FloorRegions::new(
            vertices,
            Some(FloorConstructionType::SolidFloor),
            None,
            Some(ConstructionPhase::Original),
        );

        let polygon_3d = regions.get_clockwise_3d_polygon_starting_closest_to_origin();

        println!("3D Polygon: {:?}", polygon_3d);
        // Compute signed area using shoelace formula
        let coords = &polygon_3d.exterior; // assuming this is Vec<(f64, f64, Vec<f64>)>
        let n = coords.len();
        let mut signed_area = 0.0;

        for i in 0..n {
            let (x0, y0, _) = coords[i];
            let (x1, y1, _) = coords[(i + 1) % n];
            signed_area += x0 * y1 - x1 * y0;
        }

        // Divide by 2 (not strictly necessary for sign check)
        signed_area *= 0.5;

        // Assert clockwise (signed area < 0)
        assert!(
            signed_area < 0.0,
            "Polygon should be clockwise (negative area), got {}",
            signed_area
        );
    }
}
