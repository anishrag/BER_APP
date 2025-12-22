// standard includes
use geo::bool_ops::BooleanOps;
use geo::{LineString, MultiPolygon, Polygon};
use nalgebra::Point2;
use std::collections::HashMap;

use crate::datatypes::ber::common::ConstructionPhase;

// local includes
use super::types::{ConstructionLayer, ConstructionType, Vertex2D};

/// A planar region of the floor
#[derive(Clone, Debug)]
pub struct FloorRegion {
    pub polygon: Polygon<f64>, // 2D polygon boundary
    pub height: f64,           // height per vertex
    pub construction_type: ConstructionType,
    pub construction_layers: Option<Vec<ConstructionLayer>>,
    pub construction_phase: ConstructionPhase,
}
/// A planar region of the floor
#[derive(Debug)]
pub struct FloorRegions {
    pub regions: Vec<FloorRegion>, // height per vertex
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
                construction_type: ConstructionType::Concrete,
                construction_layers: None,
                construction_phase: ConstructionPhase::Original,
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

    pub fn change_polygon_construction_phase(
        &mut self,
        polygon: Polygon<f64>,
        construction_phase: ConstructionPhase,
    ) {
        self.modify_polygon(polygon, |r| {
            r.construction_phase = construction_phase.clone();
        });
    }

    // pub fn change_id_polygon(&mut self, polygon: Polygon<f64>) {
    //     let year = self
    //         .year_for_polygon(&polygon)
    //         .expect("change_id_polygon spans multiple years");
    //
    //     let new_year_id = self.year_id_allocator.alloc(year);
    //
    //     self.modify_polygon(polygon, |r| {
    //         r.year_id = new_year_id;
    //     });
    // }
    //
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
}

#[derive(Debug)]
pub struct FloorRegionByConstructionPhase {
    pub polygon: Polygon<f64>,
    pub construction_phase: ConstructionPhase,
}

// #[cfg(test)]
// mod tests {
//     use super::*;
//
//     fn rectangle(min_x: f64, min_y: f64, max_x: f64, max_y: f64) -> Polygon<f64> {
//         Polygon::new(
//             LineString::from(vec![
//                 (min_x, min_y),
//                 (max_x, min_y),
//                 (max_x, max_y),
//                 (min_x, max_y),
//                 (min_x, min_y), // close the loop
//             ]),
//             vec![], // no holes
//         )
//     }
//
//     fn base_floor(year: Year) -> FloorRegions {
//         let vertices = vec![
//             Vertex2D { x: 0.0, y: 0.0 },
//             Vertex2D { x: 10.0, y: 0.0 },
//             Vertex2D { x: 10.0, y: 10.0 },
//             Vertex2D { x: 0.0, y: 10.0 },
//         ];
//
//         FloorRegions::new(vertices, year)
//     }
//
//     #[test]
//     fn new_creates_single_region() {
//         let floor = base_floor(2020);
//
//         assert_eq!(floor.regions.len(), 1);
//         assert_eq!(floor.regions[0].height, 0.0);
//         assert_eq!(floor.regions[0].year_id.year, 2020);
//     }
//
//     #[test]
//     fn raise_polygon_splits_region_and_changes_height() {
//         let mut floor = base_floor(2020);
//
//         let raise_poly = rectangle(0.0, 0.0, 5.0, 10.0);
//         floor.raise_lower_polygon(raise_poly, 2.5);
//
//         assert_eq!(floor.regions.len(), 2);
//
//         let raised = floor.regions.iter().filter(|r| r.height == 2.5).count();
//         let flat = floor.regions.iter().filter(|r| r.height == 0.0).count();
//
//         assert_eq!(raised, 1);
//         assert_eq!(flat, 1);
//     }
//
//     #[test]
//     fn change_year_polygon_updates_only_intersection() {
//         let mut floor = base_floor(2020);
//
//         let poly = rectangle(0.0, 0.0, 5.0, 10.0);
//         floor.change_year_polygon(poly, 2025);
//
//         assert_eq!(floor.regions.len(), 2);
//
//         let years: Vec<u32> = floor.regions.iter().map(|r| r.year_id.year).collect();
//
//         assert!(years.contains(&2020));
//         assert!(years.contains(&2025));
//     }
//
//     #[test]
//     fn change_id_polygon_allocates_new_id_same_year() {
//         let mut floor = base_floor(2020);
//
//         let poly = rectangle(0.0, 0.0, 5.0, 10.0);
//         floor.change_id_polygon(poly);
//
//         assert_eq!(floor.regions.len(), 2);
//
//         let ids: Vec<_> = floor.regions.iter().map(|r| r.year_id.id).collect();
//         let years: Vec<_> = floor.regions.iter().map(|r| r.year_id.year).collect();
//
//         assert_eq!(years[0], 2020);
//         assert_eq!(years[1], 2020);
//         assert_ne!(ids[0], ids[1]);
//     }
//
//     #[test]
//     #[should_panic(expected = "change_id_polygon spans multiple years")]
//     fn change_id_polygon_panics_on_mixed_years() {
//         let mut floor = base_floor(2020);
//
//         let left = rectangle(0.0, 0.0, 5.0, 10.0);
//         floor.change_year_polygon(left, 2025);
//
//         let spanning = rectangle(0.0, 0.0, 10.0, 10.0);
//         floor.change_id_polygon(spanning);
//     }
//
//     #[test]
//     fn by_year_unions_regions_per_year() {
//         let mut floor = base_floor(2020);
//
//         let left = rectangle(0.0, 0.0, 5.0, 10.0);
//         let right = rectangle(5.0, 0.0, 10.0, 10.0);
//
//         floor.change_year_polygon(left, 2021);
//         floor.change_year_polygon(right, 2021);
//
//         let by_year = floor.by_year();
//
//         let regions_2021: Vec<_> = by_year.iter().filter(|r| r.year == 2021).collect();
//         assert_eq!(regions_2021.len(), 1);
//     }
//
//     #[test]
//     fn height_is_preserved_for_remainder() {
//         let mut floor = base_floor(2020);
//
//         floor.raise_lower_polygon(rectangle(0.0, 0.0, 10.0, 10.0), 1.0);
//         floor.raise_lower_polygon(rectangle(0.0, 0.0, 5.0, 10.0), 2.0);
//
//         let heights: Vec<f64> = floor.regions.iter().map(|r| r.height).collect();
//
//         assert!(heights.contains(&3.0)); // overlap region
//         assert!(heights.contains(&1.0)); // remainder region
//     }
// }
