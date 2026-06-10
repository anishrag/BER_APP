// standard includes
use domain::points::Point2f64;
use geo::algorithm::Contains;
use geo::bool_ops::BooleanOps;
use geo::{LineString, MultiPolygon, Polygon};
use itertools::izip;
use std::collections::HashMap;

use datatypes::ber::common::ItemId;
use datatypes::ber::structural::{FloorRegion, FloorRegions};

// local includes
use super::geometry::{Geometry, MergeableRegion};
use super::types::{CloneWithNewId, HasRegionId, Polygon3D, Polygon3DMultiZ};

#[derive(Clone, Copy, Debug)]
struct DirectedEdge {
    from: Point2f64,
    to: Point2f64,
    height: f64,
}
#[derive(Debug)]
pub enum MergeWarning {
    InvalidIds(Vec<ItemId>),
    MultipleResultingRegions {
        source_ids: Vec<ItemId>,
        resulting_regions: usize,
    },
}

pub trait FloorRegionsManip {
    fn from_polygons(
        polygons: Vec<&Polygon<f64>>,
        heights: Vec<f64>,
        construction_id: ItemId,
    ) -> Self;

    fn raise_lower_polygon(&mut self, polygon: Polygon<f64>, height_change: f64);
    /// Re-point the floor regions overlapping `polygon` at `construction_id`,
    /// splitting geometry along the polygon boundary. The caller (the `Gen3D`
    /// wrapper) resolves the desired construction (phase / type / make-up) to an
    /// id via `ensure_floor_construction` first, so this layer only deals in ids.
    fn set_polygon_construction(&mut self, polygon: Polygon<f64>, construction_id: ItemId);

    fn get_floor_height_at_point(&self, point: &Point2f64) -> Vec<f64>;
    fn get_clockwise_3d_polygon_starting_closest_to_origin(&self) -> Polygon3DMultiZ;
}
impl FloorRegionsManip for FloorRegions {
    fn from_polygons(
        polygons: Vec<&Polygon<f64>>,
        heights: Vec<f64>,
        construction_id: ItemId,
    ) -> Self {
        let mut regions: Vec<FloorRegion> = Vec::new();
        for (polygon, height) in izip!(&polygons, &heights) {
            regions.push(FloorRegion {
                id: ItemId::default(),
                polygon: (*polygon).clone(),
                construction_id,
                level_m: *height,
            });
        }
        FloorRegions { regions }
    }

    fn raise_lower_polygon(&mut self, polygon: Polygon<f64>, height_change: f64) {
        self.modify_polygon(polygon, |r| {
            r.level_m += height_change;
        });
    }

    fn set_polygon_construction(&mut self, polygon: Polygon<f64>, construction_id: ItemId) {
        self.modify_polygon(polygon, |r| {
            r.construction_id = construction_id;
        });
    }

    fn get_floor_height_at_point(&self, point: &Point2f64) -> Vec<f64> {
        let mut heights = Vec::new();
        for region in &self.regions {
            if region.polygon.contains(&geo::Point::new(point.x, point.y)) {
                heights.push(region.level_m);
            }
        }
        heights
    }

    fn get_clockwise_3d_polygon_starting_closest_to_origin(&self) -> Polygon3DMultiZ {
        // ------------------------------------------------------------
        // 1. Collect all directed edges from all regions
        // ------------------------------------------------------------
        let mut all_edges: Vec<DirectedEdge> = Vec::new();

        for region in &self.regions {
            let coords: Vec<Point2f64> = region
                .polygon
                .exterior()
                .points()
                .map(|p| Point2f64::new(p.x(), p.y()))
                .collect();

            if coords.len() < 2 {
                continue;
            }

            // skip duplicate closing point
            for i in 0..coords.len() - 1 {
                all_edges.push(DirectedEdge {
                    from: coords[i],
                    to: coords[i + 1],
                    height: region.level_m,
                });
            }
            all_edges.push(DirectedEdge {
                from: coords[coords.len() - 1],
                to: coords[0],
                height: region.level_m,
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

trait FloorRegionsManipPrivate {
    fn from_polygon_3ds(polygon_3ds: Vec<&Polygon3D>, construction_id: ItemId) -> Self;
    fn get_floor_region_by_id(&self, id: &ItemId) -> Option<&FloorRegion>;
    fn merge_regions(&self, ids: &[ItemId]) -> (Vec<FloorRegion>, Vec<MergeWarning>);
    fn modify_polygon<F>(&mut self, polygon: Polygon<f64>, modify: F)
    where
        F: FnMut(&mut FloorRegion);
    fn by_construction_id(&self) -> Vec<FloorRegionByConstructionId>;
}

impl FloorRegionsManipPrivate for FloorRegions {
    fn from_polygon_3ds(polygon_3ds: Vec<&Polygon3D>, construction_id: ItemId) -> Self {
        let mut regions: Vec<FloorRegion> = Vec::new();

        for polygon in &polygon_3ds {
            let (polygon2d, heights) = polygon.to_2d_polygon_and_heights();
            //assert all heights same
            assert!(
                heights.iter().all(|&h| approx_eq(h, heights[0])),
                "All heights in Polygon3D must be the same to convert to FloorRegion"
            );
            let height = heights[0];

            regions.push(FloorRegion {
                id: ItemId::default(),
                polygon: polygon2d,
                construction_id,
                level_m: height,
            });
        }
        FloorRegions { regions }
    }

    fn get_floor_region_by_id(&self, id: &ItemId) -> Option<&FloorRegion> {
        self.regions.iter().find(|r| &r.id == id)
    }
    fn merge_regions(&self, ids: &[ItemId]) -> (Vec<FloorRegion>, Vec<MergeWarning>) {
        let mut warnings = Vec::new();
        let mut selected: Vec<&FloorRegion> = self
            .regions
            .iter()
            .filter(|r| ids.contains(&r.id))
            .collect();

        // warn for invalid IDs
        let invalid_ids: Vec<ItemId> = ids
            .iter()
            .filter(|id| !self.regions.iter().any(|r| &r.id == *id))
            .copied()
            .collect();
        if !invalid_ids.is_empty() {
            warnings.push(MergeWarning::InvalidIds(invalid_ids));
        }

        let mut merged = Vec::new();

        while let Some(region) = selected.pop() {
            let mut group = vec![region.clone()];

            // Regions are merge-compatible iff they share the same floor level
            // and construction (fabric now lives behind `construction_id`).
            selected.retain(|other| {
                if other.level_m == region.level_m
                    && other.construction_id == region.construction_id
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

            let source_ids: Vec<ItemId> = group.iter().map(|r| r.id).collect();

            for poly in merged_multi.0.clone() {
                merged.push(FloorRegion {
                    id: ItemId::default(),
                    polygon: poly,
                    construction_id: region.construction_id,
                    level_m: region.level_m,
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
            let mut id = region.id;
            for poly in remainder.0 {
                new_regions.push(FloorRegion {
                    id,
                    polygon: poly,
                    construction_id: region.construction_id,
                    level_m: region.level_m,
                });
                id = ItemId::default(); // ensure new ID for subsequent remainders
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
    fn by_construction_id(&self) -> Vec<FloorRegionByConstructionId> {
        let mut per_construction_id: HashMap<ItemId, Vec<Polygon<f64>>> = HashMap::new();

        // 1. Group polygons by construction
        for region in &self.regions {
            per_construction_id
                .entry(region.construction_id)
                .or_default()
                .push(region.polygon.clone());
        }

        let mut result = Vec::new();

        // 2. Union per construction
        for (construction_id, polygons) in per_construction_id {
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
                    result.push(FloorRegionByConstructionId {
                        polygon: poly,
                        construction_id,
                    });
                }
            }
        }
        result
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

fn points_match(p1: &Point2f64, p2: &Point2f64) -> bool {
    approx_eq(p1.x, p2.x) && approx_eq(p1.y, p2.y)
}

fn edges_match(a: &DirectedEdge, b: &DirectedEdge) -> bool {
    (points_match(&a.from, &b.from) && points_match(&a.to, &b.to))
        || (points_match(&a.from, &b.to) && points_match(&a.to, &b.from))
}

#[derive(Debug)]
pub struct FloorRegionByConstructionId {
    pub polygon: Polygon<f64>,
    pub construction_id: ItemId,
}

impl MergeableRegion for FloorRegion {
    type Geometry = geo::Polygon<f64>; // or Vec<Vertex> if you use vertex list

    fn id(&self) -> ItemId {
        self.id
    }

    fn geometry(&self) -> &Self::Geometry {
        &self.polygon
    }

    fn is_merge_compatible(&self, other: &Self) -> bool {
        // Same floor level + same construction (fabric is behind `construction_id`).
        self.level_m == other.level_m && self.construction_id == other.construction_id
    }

    fn with_geometry(&self, geometry: Self::Geometry) -> Self {
        let mut new_region = self.clone_with_new_id();
        new_region.polygon = geometry;
        new_region
    }
}

impl HasRegionId for FloorRegion {
    fn id_mut(&mut self) -> &mut ItemId {
        &mut self.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::types::{FromMultiZ, Polygon3D, Polygon3DConversionError};

    use domain::points::Point3f64;
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
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ];

        FloorRegions::new(vertices, ItemId::default())
    }

    #[test]
    fn new_creates_single_region() {
        let cid = ItemId::default();
        let vertices = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ];
        let floor = FloorRegions::new(vertices, cid);

        assert_eq!(floor.regions.len(), 1);
        assert_eq!(floor.regions[0].level_m, 0.0);
        // Fabric now lives behind `construction_id`; the region just references it.
        assert_eq!(floor.regions[0].construction_id, cid);
    }

    #[test]
    fn raise_polygon_splits_region_and_changes_height() {
        let mut floor = base_floor();

        let raise_poly = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.raise_lower_polygon(raise_poly, 2.5);

        assert_eq!(floor.regions.len(), 2);

        let raised = floor.regions.iter().filter(|r| r.level_m == 2.5).count();
        let flat = floor.regions.iter().filter(|r| r.level_m == 0.0).count();

        assert_eq!(raised, 1);
        assert_eq!(flat, 1);
    }

    #[test]
    fn change_construction_polygon_updates_only_intersection() {
        let mut floor = base_floor();
        let original_cid = floor.regions[0].construction_id;
        let new_cid = ItemId::default();

        let poly = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.set_polygon_construction(poly, new_cid);

        assert_eq!(floor.regions.len(), 2);

        let cids: Vec<ItemId> = floor.regions.iter().map(|r| r.construction_id).collect();

        // Only the intersected half is re-pointed; the remainder keeps the original.
        assert!(cids.contains(&original_cid));
        assert!(cids.contains(&new_cid));
    }

    #[test]
    fn change_construction_polygon_splits_region() {
        let mut floor = base_floor();
        let new_cid = ItemId::default();

        let poly = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.set_polygon_construction(poly, new_cid);

        assert_eq!(floor.regions.len(), 2);

        // The re-pointed half references the new construction; the other keeps base.
        let changed = floor
            .regions
            .iter()
            .filter(|r| r.construction_id == new_cid)
            .count();
        assert_eq!(changed, 1);
    }
    //
    #[test]
    fn change_construction_for_part_of_multiple_polygons_should_create_extra_polys() {
        let mut floor = base_floor();

        let left = rectangle(0.0, 0.0, 5.0, 10.0);
        floor.set_polygon_construction(left, ItemId::default());

        let spanning = rectangle(0.0, 0.0, 10.0, 5.0);
        floor.set_polygon_construction(spanning, ItemId::default());
        let num_regions = floor.regions.len();
        assert_eq!(num_regions, 4);
    }

    #[test]
    fn by_construction_id_unions_regions_per_construction() {
        let mut floor = base_floor();
        let shared_cid = ItemId::default();

        let left = rectangle(0.0, 0.0, 5.0, 10.0);
        let right = rectangle(5.0, 0.0, 10.0, 10.0);

        floor.set_polygon_construction(left, shared_cid);
        floor.set_polygon_construction(right, shared_cid);

        let by_construction_id = floor.by_construction_id();

        let regions: Vec<_> = by_construction_id
            .iter()
            .filter(|r| r.construction_id == shared_cid)
            .collect();
        assert_eq!(regions.len(), 1);
    }

    #[test]
    fn height_is_preserved_for_remainder() {
        let mut floor = base_floor();

        floor.raise_lower_polygon(rectangle(0.0, 0.0, 10.0, 10.0), 1.0);
        floor.raise_lower_polygon(rectangle(0.0, 0.0, 5.0, 10.0), 2.0);

        let heights: Vec<f64> = floor.regions.iter().map(|r| r.level_m).collect();

        assert!(heights.contains(&3.0)); // overlap region
        assert!(heights.contains(&1.0)); // remainder region
    }

    #[test]
    fn test_single_region_3d_polygon() {
        // Create a simple square region at height 1.0
        let vertices = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];

        let mut regions = FloorRegions::new(vertices, ItemId::default());
        regions.regions[0].level_m = 1.0;

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
        let simple_polygon = Polygon3D::try_from_multi_z(&polygon_3d);
        assert!(simple_polygon.is_ok());
    }

    #[test]
    fn test_two_regions_with_discontinuity() {
        // Create two adjacent regions with different heights
        // Region 1: left square at height 1.0
        let vertices1 = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 2.0),
            Point2f64::new(0.0, 2.0),
        ];

        // Region 2: right square at height 3.0
        let vertices2 = [
            Point2f64::new(2.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 2.0),
            Point2f64::new(2.0, 2.0),
        ];

        let mut regions = FloorRegions::new(vertices1, ItemId::default());
        regions.regions[0].level_m = 1.0;

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
            id: ItemId::default(),
            polygon: polygon2,
            construction_id: ItemId::default(),
            level_m: 3.0,
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
        let simple_polygon = Polygon3D::try_from_multi_z(&polygon_3d);
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
            Point3f64::new(0.0, 0.0, 1.0),
            Point3f64::new(1.0, 0.0, 1.5),
            Point3f64::new(1.0, 1.0, 2.0),
            Point3f64::new(0.0, 1.0, 1.5),
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
        let roundtrip = Polygon3D::try_from_multi_z(&multi_z).unwrap();

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
        // Create a simple square
        let vertices = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(1.0, 0.0),
            Point2f64::new(1.0, 1.0),
            Point2f64::new(0.0, 1.0),
        ];

        let regions = FloorRegions::new(vertices, ItemId::default());

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
