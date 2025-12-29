// standard includes
use geo::bool_ops::BooleanOps;
use geo::{Intersects, LineString, MultiPolygon, Polygon};
use nalgebra::Point2;
use std::collections::HashMap;

use crate::datatypes::ber::common::ConstructionPhase;

// local includes
use super::types::{ConstructionLayer, FloorConstructionType, Polygon3DMultiZ};

/// A planar region of the floor
#[derive(Clone, Debug)]
pub struct FloorRegion {
    pub polygon: Polygon<f64>, // 2D polygon boundary
    pub height: f64,           // height per vertex
    pub construction_type: FloorConstructionType,
    pub construction_layers: Vec<(ConstructionLayer, u32)>,
    pub construction_phase: ConstructionPhase,
}
/// A planar region of the floor
#[derive(Debug)]
pub struct FloorRegions {
    pub regions: Vec<FloorRegion>, // height per vertex
}

#[derive(Clone, Copy, Debug)]
struct DirectedEdge {
    from: Point2<f64>,
    to: Point2<f64>,
    height: f64,
}

impl FloorRegions {
    /// Create a new FloorRegions
    pub fn new(
        vertices: Vec<Point2<f64>>,
        construction_type: Option<FloorConstructionType>,
        construction_layers: Option<Vec<(ConstructionLayer, u32)>>,
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
                construction_layers: construction_layers.unwrap_or_default(),
                construction_phase: construction_phase.unwrap_or(ConstructionPhase::Original),
            }],
        }
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
            for poly in remainder.0 {
                new_regions.push(FloorRegion {
                    polygon: poly,
                    height: region.height,
                    construction_type: region.construction_type,
                    construction_layers: region.construction_layers.clone(),
                    construction_phase: region.construction_phase.clone(),
                });
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
            r.construction_phase = construction_phase.clone();
        });
    }

    pub fn modify_polygon_all_fields(
        &mut self,
        polygon: Polygon<f64>,
        height: Option<f64>,
        construction_type: Option<FloorConstructionType>,
        construction_layers: Option<Vec<(ConstructionLayer, u32)>>,
        construction_phase: Option<ConstructionPhase>,
    ) {
        self.modify_polygon(polygon, |r| {
            if let Some(h) = height {
                r.height = h;
            }
            if let Some(ct) = construction_type {
                r.construction_type = ct;
            }
            if let Some(cl) = construction_layers.clone() {
                r.construction_layers = cl;
            }
            if let Some(cp) = construction_phase.clone() {
                r.construction_phase = cp;
            }
        });
    }

    pub fn by_construction_phase(&self) -> Vec<FloorRegionByConstructionPhase> {
        let mut per_construction_phase: HashMap<ConstructionPhase, Vec<Polygon<f64>>> =
            HashMap::new();

        // 1. Group polygons by year
        for region in &self.regions {
            per_construction_phase
                .entry(region.construction_phase.clone())
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

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(floor.regions[0].construction_layers.len(), 0);
        assert_eq!(
            floor.regions[0].construction_phase,
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
            .map(|r| r.construction_phase.clone())
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
            Some(vec![
                (ConstructionLayer::Insulation, 100),
                (ConstructionLayer::Concrete, 100),
            ]),
            None,
        );

        assert_eq!(floor.regions.len(), 2);

        let num_regions = floor.regions.len();
        let construction_phases = floor
            .regions
            .iter()
            .map(|r| r.construction_phase.clone())
            .collect::<Vec<_>>();

        assert_eq!(num_regions, 2);
        assert_eq!(construction_phases[1], ConstructionPhase::Original);
        assert_eq!(construction_phases[0], ConstructionPhase::Original);
    }
    //
    #[test]
    fn add_construction_layers_for_part_of_multiple_polygons_should_create_extra_polys() {
        let mut floor = base_floor();

        let left = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.modify_polygon_construction_phase(left, ConstructionPhase::Extension1);

        let spanning = rectangle(0.0, 0.0, 10.0, 5.0);
        floor.modify_polygon_all_fields(
            spanning,
            None,
            None,
            Some(vec![
                (ConstructionLayer::Insulation, 100),
                (ConstructionLayer::Concrete, 100),
            ]),
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
}
