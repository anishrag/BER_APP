use geo::algorithm::coords_iter::CoordsIter;
use geo::algorithm::intersects::Intersects;
use geo::{Coord, LineString, Polygon};

/// A vertical wall polygon
#[derive(Clone, Debug)]
pub struct WallRegion {
    pub polygon: Polygon3D,
}

/// Collection of wall polygons
#[derive(Debug)]
pub struct WallRegions {
    pub regions: Vec<WallRegion>,
}

impl WallRegions {
    pub fn new() -> Self {
        WallRegions {
            regions: Vec::new(),
        }
    }

    /// Generate walls connecting exposed floor edges to ceiling polygons
    pub fn generate_exposed_floor_walls(
        &mut self,
        floors: &FloorRegions,
        ceilings: &CeilingRegions,
    ) {
        // Collect all floor edges
        let mut edges = Vec::new();
        for floor in &floors.regions {
            let coords: Vec<Coord<f64>> = floor.polygon.exterior().coords_iter().collect();
            for i in 0..coords.len() - 1 {
                edges.push((
                    (coords[i].x, coords[i].y),
                    (coords[i + 1].x, coords[i + 1].y),
                ));
            }
        }

        // Helper to check if edge is shared
        let is_exposed = |edge: &((f64, f64), (f64, f64))| {
            edges
                .iter()
                .filter(|e| WallRegions::edges_equal(e, edge))
                .count()
                == 1
        };

        // For each floor polygon
        for floor in &floors.regions {
            let coords: Vec<Coord<f64>> = floor.polygon.exterior().coords_iter().collect();
            for i in 0..coords.len() - 1 {
                let edge = (
                    (coords[i].x, coords[i].y),
                    (coords[i + 1].x, coords[i + 1].y),
                );
                if !is_exposed(&edge) {
                    continue;
                }

                // Find overlapping ceiling edges
                for ceiling in &ceilings.regions {
                    if !floor.polygon.intersects(&ceiling.polygon) {
                        continue;
                    }

                    // Split floor edge proportionally if multiple ceilings overlap
                    // Here we just connect the full edge to ceiling's closest points
                    let floor_start = Point3D {
                        x: edge.0 .0,
                        y: edge.0 .1,
                        z: floor.height,
                    };
                    let floor_end = Point3D {
                        x: edge.1 .0,
                        y: edge.1 .1,
                        z: floor.height,
                    };

                    let ceiling_coords: Vec<Coord<f64>> =
                        ceiling.polygon.exterior().coords_iter().collect();
                    let ceiling_start = Point3D {
                        x: ceiling_coords[0].x,
                        y: ceiling_coords[0].y,
                        z: ceiling.height,
                    };
                    let ceiling_end = Point3D {
                        x: ceiling_coords[1].x,
                        y: ceiling_coords[1].y,
                        z: ceiling.height,
                    };

                    let wall = Polygon3D {
                        vertices: vec![floor_start, floor_end, ceiling_end, ceiling_start],
                    };
                    self.regions.push(WallRegion { polygon: wall });
                }
            }
        }
    }

    /// Generate walls for floor steps
    pub fn generate_floor_step_walls(&mut self, floors: &FloorRegions) {
        for i in 0..floors.regions.len() {
            for j in i + 1..floors.regions.len() {
                let a = &floors.regions[i];
                let b = &floors.regions[j];

                if !a.polygon.intersects(&b.polygon) {
                    continue;
                }
                if (a.height - b.height).abs() < 1e-6 {
                    continue;
                } // skip if same height

                // Compute overlapping area (simplified: take bounding intersection)
                let coords_a: Vec<Coord<f64>> = a.polygon.exterior().coords_iter().collect();
                let coords_b: Vec<Coord<f64>> = b.polygon.exterior().coords_iter().collect();

                let wall = Polygon3D {
                    vertices: vec![
                        Point3D {
                            x: coords_a[0].x,
                            y: coords_a[0].y,
                            z: a.height,
                        },
                        Point3D {
                            x: coords_a[1].x,
                            y: coords_a[1].y,
                            z: a.height,
                        },
                        Point3D {
                            x: coords_b[1].x,
                            y: coords_b[1].y,
                            z: b.height,
                        },
                        Point3D {
                            x: coords_b[0].x,
                            y: coords_b[0].y,
                            z: b.height,
                        },
                    ],
                };
                self.regions.push(WallRegion { polygon: wall });
            }
        }
    }

    /// Generate walls for ceiling steps
    pub fn generate_ceiling_step_walls(&mut self, ceilings: &CeilingRegions) {
        for i in 0..ceilings.regions.len() {
            for j in i + 1..ceilings.regions.len() {
                let a = &ceilings.regions[i];
                let b = &ceilings.regions[j];

                if !a.polygon.intersects(&b.polygon) {
                    continue;
                }
                if (a.height - b.height).abs() < 1e-6 {
                    continue;
                }

                let coords_a: Vec<Coord<f64>> = a.polygon.exterior().coords_iter().collect();
                let coords_b: Vec<Coord<f64>> = b.polygon.exterior().coords_iter().collect();

                let wall = Polygon3D {
                    vertices: vec![
                        Point3D {
                            x: coords_a[0].x,
                            y: coords_a[0].y,
                            z: a.height,
                        },
                        Point3D {
                            x: coords_a[1].x,
                            y: coords_a[1].y,
                            z: a.height,
                        },
                        Point3D {
                            x: coords_b[1].x,
                            y: coords_b[1].y,
                            z: b.height,
                        },
                        Point3D {
                            x: coords_b[0].x,
                            y: coords_b[0].y,
                            z: b.height,
                        },
                    ],
                };
                self.regions.push(WallRegion { polygon: wall });
            }
        }
    }

    fn edges_equal(a: &((f64, f64), (f64, f64)), b: &((f64, f64), (f64, f64))) -> bool {
        (a.0 == b.0 && a.1 == b.1) || (a.0 == b.1 && a.1 == b.0)
    }
}
