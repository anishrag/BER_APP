use earcutr::earcut;

use super::types::{Vertex2D, Year, YearId, YearIdAllocator};
use geo::algorithm::union::Union;
use geo::{LineString, MultiPolygon, Polygon};
use nalgebra::{Point3, Vector3};
use spade::constrained_delaunay::{CDTVertex, ConstrainedDelaunayTriangulation};
use spade::HasPosition;
use std::collections::HashMap;

/// A planar region of the floor
#[derive(Clone, Debug)]
pub struct FloorRegion {
    pub polygon: Polygon<f64>, // 2D polygon boundary
    pub height: f64,           // height per vertex
    pub year_id: YearId,
}
/// A planar region of the floor
#[derive(Debug)]
pub struct FloorRegions {
    pub regions: Vec<FloorRegion>, // height per vertex

    pub year_id_allocator: YearIdAllocator,
}

impl FloorRegions {
    /// Create a new FloorRegions
    pub fn new(vertices: Vec<Vertex2D>, year: u32) -> Self {
        let polygon = Polygon::new(
            LineString::from(vertices.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
            vec![],
        );
        let mut allocator = YearIdAllocator::new();
        let year_id = allocator.alloc(year);

        Self {
            year_id_allocator: allocator,
            regions: vec![FloorRegion {
                polygon,
                height: 0.0f64,
                year_id,
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
                    year_id: region.year_id,
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

    pub fn change_year_polygon(&mut self, polygon: Polygon<f64>, year: u32) {
        let year_id = self.year_id_allocator.alloc(year);
        self.modify_polygon(polygon, |r| {
            r.year_id = year_id;
        });
    }

    fn year_for_polygon(&self, polygon: &Polygon<f64>) -> Option<Year> {
        let mut found_year: Option<Year> = None;

        for region in &self.regions {
            if !region.polygon.intersection(polygon).0.is_empty() {
                match found_year {
                    None => found_year = Some(region.year_id.year),
                    Some(y) if y == region.year_id.year => {}
                    Some(_) => return None, // mixed years
                }
            }
        }

        found_year
    }

    pub fn change_id_polygon(&mut self, polygon: Polygon<f64>) {
        let year = self
            .year_for_polygon(&polygon)
            .expect("change_id_polygon spans multiple years");

        let new_year_id = self.year_id_allocator.alloc(year);

        self.modify_polygon(polygon, |r| {
            r.year_id = new_year_id;
        });
    }

    pub fn by_year(&self) -> Vec<FloorRegionByYear> {
        let mut per_year: HashMap<u32, Vec<Polygon<f64>>> = HashMap::new();

        // 1. Group polygons by year
        for region in &self.regions {
            per_year
                .entry(region.year_id.year)
                .or_default()
                .push(region.polygon.clone());
        }

        let mut result = Vec::new();

        // 2. Union per year
        for (year, polygons) in per_year {
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
                    result.push(FloorRegionByYear {
                        polygon: poly,
                        year,
                    });
                }
            }
        }
        result
    }
}

#[derive(Debug)]
pub struct FloorRegionByYear {
    pub polygon: Polygon<f64>,
    pub year: Year,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_flat_floor_with_vertex_heights() {
        // Given a rectangular floor plan
        let boundary = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 5.0)];

        // All vertices at the same height
        let heights = vec![1.0, 1.0, 1.0, 1.0];

        // When we create the floor plan
        let plan = FloorPlan3D::new(boundary, heights.clone());

        // Then there is exactly one region
        assert_eq!(plan.region_count(), 1);

        // And all vertices have the expected heights
        let verts = plan.vertices();
        assert_eq!(verts.len(), 4);

        for v in verts {
            assert_eq!(v.z, 1.0);
        }
    }

    #[test]
    fn create_sloped_floor_with_per_vertex_heights() {
        // Rectangle again
        let boundary = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 5.0)];

        // Sloped floor: left side low, right side high
        let heights = vec![0.0, 2.0, 2.0, 0.0];

        let plan = FloorPlan3D::new(boundary, heights.clone());

        let verts = plan.vertices();

        // Heights match exactly
        for (v, expected) in verts.iter().zip(heights.iter()) {
            assert!((v.z - expected).abs() < 1e-9);
        }
    }
}
