// 1. take floor polygon and split by discontinuitys
// 2. Triangulate each region separately
// 3. Create extra vertices if needed and set heights
// 4. repolygon and split by planes
// 5. further split by year
// 6. futher user splitting if required
use geo::algorithm::bool_ops::BooleanOps;
use geo_types::{LineString, MultiPolygon, Polygon};

use super::geometry::{Polygon3D, Vertex2D, Vertex3D};
use super::types::{Year, YearId, YearIdAllocator};

#[derive(Clone, Debug)]
pub struct CeilingRegion {
    pub polygon: Polygon3D,
    pub year_id: YearId,
}

#[derive(Debug)]
pub struct CeilingRegions {
    pub regions: Vec<CeilingRegion>,

    pub allocator: YearIdAllocator,
}

impl CeilingRegions {
    pub fn new(storey_footprint: Polygon<f64>, height: f64, year: Year) -> Self {
        let mut allocator = YearIdAllocator::new();

        let exterior = storey_footprint
            .exterior()
            .points_iter()
            .map(|p| Vertex3D {
                x: p.x(),
                y: p.y(),
                z: height,
            })
            .collect();

        let regions = vec![CeilingRegion {
            polygon: Polygon3D { exterior },
            year_id: allocator.alloc(year),
        }];

        Self { regions, allocator }
    }

    /// Split regions by polyline, inheriting the original year_id
    pub fn split_by_polyline(&mut self, split: &[Vertex2D]) -> Result<(), &'static str> {
        self.split_by_polyline_with(split, |year_id, _idx, _allocator| year_id)
    }

    /// Split regions by polyline, allocating a new year_id for split regions
    pub fn split_by_polyline_new_year(
        &mut self,
        split: &[Vertex2D],
        year: Year,
    ) -> Result<(), &'static str> {
        self.split_by_polyline_with(split, |_year_id, _idx, allocator| allocator.alloc(year))
    }

    /// General split method with custom year_id assignment logic
    ///
    /// The function receives:
    /// - original_year_id: the year_id of the region being split
    /// - region_index: index of the resulting region (0 or 1 for two-way splits)
    /// - allocator: mutable reference to allocate new year_ids if needed
    pub fn split_by_polyline_with<F>(
        &mut self,
        split: &[Vertex2D],
        mut year_id_fn: F,
    ) -> Result<(), &'static str>
    where
        F: FnMut(YearId, usize, &mut YearIdAllocator) -> YearId,
    {
        if split.len() < 2 {
            return Err("Split polyline must have at least two points");
        }

        let split_ls = LineString::from(split.iter().map(|v| (v.x, v.y)).collect::<Vec<_>>());
        let mut new_regions = Vec::new();
        let mut any_split = false;

        for region in self.regions.drain(..) {
            let z = region.polygon.exterior[0].z;
            let geo_poly = polygon3d_to_geo(&region.polygon);
            let result: MultiPolygon<f64> = geo_poly.difference(&split_ls);

            match result.0.len() {
                // Region not affected → keep as-is
                1 => {
                    new_regions.push(region);
                }
                // Region split into exactly two → replace
                2 => {
                    any_split = true;
                    for (idx, poly) in result.0.into_iter().enumerate() {
                        let year_id = year_id_fn(region.year_id, idx, &mut self.allocator);
                        new_regions.push(CeilingRegion {
                            polygon: geo_to_polygon3d(&poly, z),
                            year_id,
                        });
                    }
                }
                // Invalid / ambiguous split
                _ => {
                    return Err("Split produced invalid number of regions");
                }
            }
        }

        if !any_split {
            return Err("Split did not affect any ceiling regions");
        }

        self.regions = new_regions;
        Ok(())
    }
}

#[derive(Debug)]
struct CeilingMesh {
    pub triangles: Vec<CeilingTriangle>,
    pub year_id: YearId,
}

impl CeilingMesh {
    pub fn add_new_vertex_with_height(&mut self, x: f64, y: f64, height: f64) {
        const EPSILON: f64 = 1e-9;

        let new_vertex = Vertex3D { x, y, z: height };

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
                    year: triangle.year,
                };

                // Triangle 2: new_vertex, vertices[j], vertices[k]
                let tri2_verts = [new_vertex, triangle.vertices[j], triangle.vertices[k]];
                let tri2 = CeilingTriangle {
                    vertices: tri2_verts,
                    plane: Plane::from_triangle(&tri2_verts),
                    year: triangle.year,
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
                        year: triangle.year,
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

#[derive(Clone, Debug)]
pub struct CeilingRegionsTriangulated {
    pub meshes: HashMap<u32, CeilingMesh>,
}

impl From<&CeilingRegions> for CeilingRegionsTriangulated {
    fn from(src: &CeilingRegions) -> Self {
        let mut meshes = HashMap::new();

        for (mesh_id, region) in src.regions.iter().enumerate() {
            let z = region.polygon.exterior[0].z;

            let exterior_2d: Vec<Vertex2D> = region
                .polygon
                .exterior
                .iter()
                .map(|v| Vertex2D { x: v.x, y: v.y })
                .collect();

            let tris_2d = triangulate_polygon_exterior(&exterior_2d);

            let triangles = tris_2d
                .into_iter()
                .map(|tri| {
                    let verts = [
                        Vertex3D {
                            x: tri[0].x,
                            y: tri[0].y,
                            z,
                        },
                        Vertex3D {
                            x: tri[1].x,
                            y: tri[1].y,
                            z,
                        },
                        Vertex3D {
                            x: tri[2].x,
                            y: tri[2].y,
                            z,
                        },
                    ];

                    CeilingTriangle {
                        plane: Plane::from_triangle(&verts),
                        vertices: verts,
                        year_id: region.year_id,
                    }
                })
                .collect();

            meshes.insert(
                mesh_id as u32,
                CeilingMesh {
                    triangles,
                    year_id: region.year_id,
                },
            );
        }

        CeilingRegionsTriangulated { meshes }
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

    pub fn from_triangle(v: &[Vertex3D; 3]) -> Self {
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
    pub vertices: [Vertex3D; 3],
    pub plane: Plane,
    pub year: u32,
}

// -----------------------------
// Conversion from Triangulated → Polygonal
// -----------------------------

impl From<&CeilingRegionsTriangulated> for CeilingRegions {
    fn from(src: &CeilingRegionsTriangulated) -> Self {
        let mut allocator = YearIdAllocator::new();
        let mut regions = Vec::new();

        for mesh in src.meshes.values() {
            // Step 1: Group triangles by plane
            let mut plane_groups: Vec<Vec<&CeilingTriangle>> = Vec::new();

            'outer: for tri in &mesh.triangles {
                for group in plane_groups.iter_mut() {
                    if tri.plane.approx_eq(&group[0].plane, EPSILON) {
                        group.push(tri);
                        continue 'outer;
                    }
                }
                plane_groups.push(vec![tri]);
            }

            // Step 2: For each plane group, extract polygons
            for group in plane_groups {
                let polygons = extract_polygons_from_triangles(&group);
                for poly in polygons {
                    regions.push(CeilingRegion {
                        polygon: poly,
                        year_id: mesh.year_id,
                    });
                }
            }
        }

        CeilingRegions { regions, allocator }
    }
}

// -----------------------------
// Extract polygons from triangles
// -----------------------------

fn extract_polygons_from_triangles(tris: &[&CeilingTriangle]) -> Vec<Polygon3D> {
    // 1. Collect edges and count occurrences
    let mut edge_count: HashMap<((f64, f64), (f64, f64)), usize> = HashMap::new();

    for tri in tris {
        let verts = &tri.vertices;
        let edges = [
            (verts[0], verts[1]),
            (verts[1], verts[2]),
            (verts[2], verts[0]),
        ];
        for &(a, b) in &edges {
            // normalize edge direction
            let key = if (a.x < b.x) || (a.x == b.x && a.y < b.y) {
                ((a.x, a.y), (b.x, b.y))
            } else {
                ((b.x, b.y), (a.x, a.y))
            };
            *edge_count.entry(key).or_insert(0) += 1;
        }
    }

    // 2. Keep only boundary edges (edges seen once)
    let boundary_edges: Vec<((f64, f64), (f64, f64))> = edge_count
        .iter()
        .filter_map(|(k, &v)| if v == 1 { Some(*k) } else { None })
        .collect();

    // 3. Walk edges to form polygons
    let mut polygons = Vec::new();
    let mut edges_set: HashSet<_> = boundary_edges.iter().cloned().collect();

    while !edges_set.is_empty() {
        let mut poly_coords = Vec::new();
        let mut start_edge = *edges_set.iter().next().unwrap();
        edges_set.remove(&start_edge);

        poly_coords.push(Vertex3D {
            x: start_edge.0 .0,
            y: start_edge.0 .1,
            z: tris[0].vertices[0].z,
        });
        poly_coords.push(Vertex3D {
            x: start_edge.1 .0,
            y: start_edge.1 .1,
            z: tris[0].vertices[0].z,
        });

        let mut current = start_edge.1;
        loop {
            let mut found_next = false;
            let mut remove_edge = None;
            for e in &edges_set {
                if (e.0 == current) || (e.1 == current) {
                    let next = if e.0 == current { e.1 } else { e.0 };
                    poly_coords.push(Vertex3D {
                        x: next.0,
                        y: next.1,
                        z: tris[0].vertices[0].z,
                    });
                    remove_edge = Some(*e);
                    current = next;
                    found_next = true;
                    break;
                }
            }
            if let Some(e) = remove_edge {
                edges_set.remove(&e);
            }
            if !found_next {
                break;
            }
        }

        polygons.push(Polygon3D {
            exterior: poly_coords,
        });
    }

    polygons
}
