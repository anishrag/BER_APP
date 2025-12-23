// 1. x take floor polygon and split by discontinuitys
// 2. Triangulate each region separately
// 3. Create extra vertices if needed and set heights
// 4. repolygon and split by planes
// 5. x further split by year
// 6. x futher user splitting if required
// external crates
use earcutr::earcut;
use geo::Polygon;
use nalgebra::{Point3, Vector3};
use std::convert::TryFrom;
// crate includes
use crate::datatypes::ber::common::ConstructionPhase;

// local includes
use super::geometry::{point_in_triangle_2d, point_on_segment_2d};
use super::types::{CeilingConstructionType, ConstructionLayer, Polygon3D};

/// A planar region of the floor
#[derive(Clone, Debug)]
pub struct CeilingRegion {
    pub polygon: Polygon3D, // 2D polygon boundary
    pub construction_type: CeilingConstructionType,
    pub construction_layers: Vec<(ConstructionLayer, u32)>,
    pub construction_phase: ConstructionPhase,
}
/// A planar region of the floor
#[derive(Debug)]
pub struct CeilingRegions {
    pub regions: Vec<CeilingRegion>, // height per vertex
}
impl CeilingRegions {
    /// Create a new CeilingRegions
    pub fn new(
        vertices: Vec<Point3<f64>>,
        construction_type: Option<CeilingConstructionType>,
        construction_layers: Option<Vec<(ConstructionLayer, u32)>>,
        construction_phase: Option<ConstructionPhase>,
    ) -> Self {
        Self {
            regions: vec![CeilingRegion {
                polygon: Polygon3D::new(vertices),
                construction_type: construction_type
                    .unwrap_or(CeilingConstructionType::Uninitialised),
                construction_layers: construction_layers.unwrap_or_default(),
                construction_phase: construction_phase.unwrap_or(ConstructionPhase::Original),
            }],
        }
    }

    fn modify_polygon<F>(&mut self, polygon: Polygon<f64>, mut modify: F)
    where
        F: FnMut(&mut CeilingRegion),
    {
        let mut new_regions = Vec::new();
        let mut regions_to_remove = Vec::new();

        for (idx, region) in self.regions.iter().enumerate() {
            let intersection = region.polygon.intersection(&polygon);
            if intersection.polygons.is_empty() {
                continue;
            }

            let remainder = region.polygon.difference(&polygon);
            regions_to_remove.push(idx);

            // remainder
            for poly in remainder.polygons {
                new_regions.push(CeilingRegion {
                    polygon: poly,
                    construction_type: region.construction_type,
                    construction_layers: region.construction_layers.clone(),
                    construction_phase: region.construction_phase.clone(),
                });
            }

            // intersection
            for poly in intersection.polygons {
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
        construction_type: Option<CeilingConstructionType>,
        construction_layers: Option<Vec<(ConstructionLayer, u32)>>,
        construction_phase: Option<ConstructionPhase>,
    ) {
        self.modify_polygon(polygon, |r| {
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

    pub fn modify_polygon_increase_height(&mut self, polygon: Polygon<f64>, delta_height: f64) {
        self.modify_polygon(polygon, |r| {
            for vertex in r.polygon.exterior.iter_mut() {
                vertex.z += delta_height;
            }
        });
    }
}

#[derive(Debug)]
struct CeilingMesh {
    pub triangles: Vec<CeilingTriangle>,
    pub construction_phase: ConstructionPhase,
}

impl CeilingMesh {
    pub fn add_new_vertex_with_height(&mut self, x: f64, y: f64, height: f64) {
        const EPSILON: f64 = 1e-9;

        let new_vertex = Point3::new(x, y, height);

        // Find all triangles that need modification
        let mut triangles_to_remove = Vec::new();
        let mut triangles_to_add = Vec::new();

        for (idx, triangle) in self.triangles.iter().enumerate() {
            // Case 1: Check if the point matches an existing vertex
            let mut vertex_match_idx = None;
            for (v_idx, vertex) in triangle.vertices.iter().enumerate() {
                if (vertex.x - x).abs() < EPSILON && (vertex.y - y).abs() < EPSILON {
                    vertex_match_idx = Some(v_idx);
                    break;
                }
            }

            if let Some(v_idx) = vertex_match_idx {
                // Update the height of this vertex
                let mut new_triangle = triangle.clone();
                new_triangle.vertices[v_idx].z = height;
                new_triangle.plane = Plane::from_triangle(&new_triangle.vertices);

                triangles_to_remove.push(idx);
                triangles_to_add.push(new_triangle);
                continue;
            }

            // Case 2: Check if the point lies on an edge
            let edges = [(0, 1), (1, 2), (2, 0)];

            let mut on_edge = None;
            for &(i, j) in &edges {
                if point_on_segment_2d(
                    x,
                    y,
                    triangle.vertices[i].x,
                    triangle.vertices[i].y,
                    triangle.vertices[j].x,
                    triangle.vertices[j].y,
                    EPSILON,
                ) {
                    on_edge = Some((i, j));
                    break;
                }
            }

            if let Some((i, j)) = on_edge {
                // Split triangle into 2 triangles
                let k = 3 - i - j; // The third vertex

                // Triangle 1: vertices[i], new_vertex, vertices[k]
                let tri1_verts = [triangle.vertices[i], new_vertex, triangle.vertices[k]];
                let tri1 = CeilingTriangle {
                    vertices: tri1_verts,
                    plane: Plane::from_triangle(&tri1_verts),
                };

                // Triangle 2: new_vertex, vertices[j], vertices[k]
                let tri2_verts = [new_vertex, triangle.vertices[j], triangle.vertices[k]];
                let tri2 = CeilingTriangle {
                    vertices: tri2_verts,
                    plane: Plane::from_triangle(&tri2_verts),
                };

                triangles_to_remove.push(idx);
                triangles_to_add.push(tri1);
                triangles_to_add.push(tri2);
                continue;
            }

            // Case 3: Check if the point is inside the triangle
            if point_in_triangle_2d(
                x,
                y,
                triangle.vertices[0].x,
                triangle.vertices[0].y,
                triangle.vertices[1].x,
                triangle.vertices[1].y,
                triangle.vertices[2].x,
                triangle.vertices[2].y,
                EPSILON,
            ) {
                // Split triangle into 3 triangles
                for i in 0..3 {
                    let j = (i + 1) % 3;
                    let tri_verts = [triangle.vertices[i], triangle.vertices[j], new_vertex];
                    let tri = CeilingTriangle {
                        vertices: tri_verts,
                        plane: Plane::from_triangle(&tri_verts),
                    };
                    triangles_to_add.push(tri);
                }

                triangles_to_remove.push(idx);
                break; // Point can only be inside one triangle
            }
        }

        // Remove triangles in reverse order to maintain indices
        triangles_to_remove.sort_unstable();
        for &idx in triangles_to_remove.iter().rev() {
            self.triangles.remove(idx);
        }

        // Add new triangles
        self.triangles.extend(triangles_to_add);
    }
}

impl TryFrom<CeilingRegion> for CeilingMesh {
    type Error = earcutr::Error; // or the actual error type from earcutr

    fn try_from(ceiling_region: CeilingRegion) -> Result<Self, Self::Error> {
        let vertices_3d = &ceiling_region.polygon.exterior;

        if vertices_3d.len() < 3 {
            return Err(earcutr::Error::Unknown);
        }

        // Flatten into 2D
        let mut coords_2d = Vec::with_capacity(vertices_3d.len() * 2);
        for v in vertices_3d {
            coords_2d.push(v.x);
            coords_2d.push(v.y);
        }

        let holes: Vec<usize> = Vec::new();

        // Triangulate — propagate error with ?
        let indices = earcut(&coords_2d, &holes, 2)?;

        let mut triangles = Vec::with_capacity(indices.len() / 3);
        for chunk in indices.chunks_exact(3) {
            let a = vertices_3d[chunk[0]];
            let b = vertices_3d[chunk[1]];
            let c = vertices_3d[chunk[2]];

            triangles.push(CeilingTriangle {
                vertices: [a, b, c],
                plane: Plane::from_triangle(&[a, b, c]),
            });
        }

        Ok(CeilingMesh {
            triangles,
            construction_phase: ceiling_region.construction_phase,
        })
    }
}

impl Plane {
    pub fn approx_eq(&self, other: &Plane, eps: f64) -> bool {
        self.normal.dot(&other.normal).abs() > 1.0 - eps && (self.d - other.d).abs() < eps
    }

    // Solve for z = (-d - nx*x - ny*y) / nz
    pub fn z_at(&self, x: f64, y: f64) -> f64 {
        (-self.d - self.normal.x * x - self.normal.y * y) / self.normal.z
    }

    pub fn from_triangle(v: &[Point3<f64>; 3]) -> Self {
        let a = nalgebra::Point3::new(v[0].x, v[0].y, v[0].z);
        let b = nalgebra::Point3::new(v[1].x, v[1].y, v[1].z);
        let c = nalgebra::Point3::new(v[2].x, v[2].y, v[2].z);
        let normal = (b - a).cross(&(c - a)).normalize();
        let d = -normal.dot(&a.coords);
        Self { normal, d }
    }
}

#[derive(Clone, Debug)]
pub struct Plane {
    pub normal: Vector3<f64>,
    pub d: f64,
}

#[derive(Clone, Debug)]
pub struct CeilingTriangle {
    pub vertices: [Point3<f64>; 3],
    pub plane: Plane,
}
