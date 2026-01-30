// 1. x take floor polygon and split by discontinuitys
// 2. x Triangulate each region separately
// 3. x Create extra vertices if needed and set heights
// 4. repolygon and split by planes
// 5. x further split by year
// 6. x futher user splitting if required
// external crates
use earcutr::earcut;
use geo::Polygon;
use nalgebra::{Point2, Point3, Vector3};
use std::collections::HashMap;
use std::convert::TryFrom;
// crate includes
use datatypes::ber::common::{ConstructionPhase, ItemId};
use datatypes::ber::structural::{
    CeilingConstructionType, CeilingRegion, CeilingRegions, ConstructionMakeup,
};

// local includes
use super::geometry::{point_in_triangle_2d, point_on_segment_2d, Geometry, MergeableRegion};
use super::types::{
    DirectedEdge3D, HasBaseRegion, Point3Ext, Polygon3D, Polygon3DMultiZ, RegionBase, EPSILON,
};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Edge2D {
    a: (i64, i64),
    b: (i64, i64),
}

#[derive(Debug, Clone)]
struct Edge3DF {
    a: (f64, f64, f64),
    b: (f64, f64, f64),
}

impl PartialEq for Edge3DF {
    fn eq(&self, other: &Self) -> bool {
        fn feq(a: f64, b: f64) -> bool {
            (a - b).abs() <= EPSILON
        }

        let same_dir = feq(self.a.0, other.a.0)
            && feq(self.a.1, other.a.1)
            && feq(self.a.2, other.a.2)
            && feq(self.b.0, other.b.0)
            && feq(self.b.1, other.b.1)
            && feq(self.b.2, other.b.2);

        let swapped_dir = feq(self.a.0, other.b.0)
            && feq(self.a.1, other.b.1)
            && feq(self.a.2, other.b.2)
            && feq(self.b.0, other.a.0)
            && feq(self.b.1, other.a.1)
            && feq(self.b.2, other.a.2);

        same_dir || swapped_dir
    }
}

fn quantize(p: &Point3<f64>) -> (i64, i64) {
    ((p.x * 1e9) as i64, (p.y * 1e9) as i64)
}

fn undirected_edge_2d(a: &Point3<f64>, b: &Point3<f64>) -> Edge2D {
    let qa = quantize(a);
    let qb = quantize(b);
    if qa <= qb {
        Edge2D { a: qa, b: qb }
    } else {
        Edge2D { a: qb, b: qa }
    }
}

fn undirected_edge_3df(a: &Point3<f64>, b: &Point3<f64>) -> Edge3DF {
    let pa = (a.x, a.y, a.z);
    let pb = (b.x, b.y, b.z);
    if pa <= pb {
        Edge3DF { a: pa, b: pb }
    } else {
        Edge3DF { a: pb, b: pa }
    }
}

pub enum CeilingRegionError {
    RegionVertexInsertionError(VertexInsertionError),
    ConversionFailed,
}

impl MergeableRegion for CeilingRegion {
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
    }

    fn with_geometry(&self, geometry: Self::Geometry) -> Self {
        CeilingRegion {
            polygon: geometry,
            construction_type: self.construction_type,
            base: self.base.clone(),
        }
    }
}

pub trait CeilingRegionsManip {
    fn insert_vertex(&mut self, p: Point2<f64>, height: f64) -> Result<(), CeilingRegionError>;
    fn modify_polygon_construction_phase(
        &mut self,
        polygon: Polygon<f64>,
        construction_phase: ConstructionPhase,
    );
    fn modify_polygon_all_fields(
        &mut self,
        polygon: Polygon<f64>,
        construction_type: Option<CeilingConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
    );
    fn modify_polygon_increase_height(&mut self, polygon: Polygon<f64>, delta_height: f64);
    fn get_clockwise_3d_polygon_starting_closest_to_origin(&self) -> Polygon3DMultiZ;
}

impl CeilingRegionsManip for CeilingRegions {
    fn insert_vertex(&mut self, p: Point2<f64>, height: f64) -> Result<(), CeilingRegionError> {
        let old_regions = std::mem::take(&mut self.regions);

        self.regions = old_regions
            .into_iter()
            .try_fold(Vec::new(), |mut acc, reg| {
                if reg.polygon.contains_point(&p) {
                    // Convert self into a mesh
                    let mut mesh = CeilingMesh::try_from(reg).map_err(|e| {
                        CeilingRegionError::RegionVertexInsertionError(
                            VertexInsertionError::EarcutError(e),
                        )
                    })?;

                    // Add the vertex
                    mesh.add_new_vertex_with_height(p.x, p.y, height)
                        .map_err(CeilingRegionError::RegionVertexInsertionError)?;

                    // Convert back to regions
                    let conversion: CeilingRegionConversion = mesh.into();

                    if conversion.failed_components > 0 || conversion.regions.is_empty() {
                        return Err(CeilingRegionError::ConversionFailed);
                    }

                    acc.extend(conversion.regions) // return all resulting regions
                } else {
                    acc.push(reg); // keep the original region
                }
                Ok(acc)
            })?;
        Ok(())
    }

    fn modify_polygon_construction_phase(
        &mut self,
        polygon: Polygon<f64>,
        construction_phase: ConstructionPhase,
    ) {
        self.modify_polygon(polygon, |r| {
            r.base.construction_phase = construction_phase.clone();
        });
    }

    fn modify_polygon_all_fields(
        &mut self,
        polygon: Polygon<f64>,
        construction_type: Option<CeilingConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
    ) {
        self.modify_polygon(polygon, |r| {
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

    fn modify_polygon_increase_height(&mut self, polygon: Polygon<f64>, delta_height: f64) {
        self.modify_polygon(polygon, |r| {
            for vertex in r.polygon.exterior.iter_mut() {
                vertex.z += delta_height;
            }
        });
    }

    fn get_clockwise_3d_polygon_starting_closest_to_origin(&self) -> Polygon3DMultiZ {
        // ------------------------------------------------------------
        // 1. Collect all directed edges from all regions
        // ------------------------------------------------------------
        let mut all_edges: Vec<DirectedEdge3D> = Vec::new();

        for region in &self.regions {
            let coords = &region.polygon.exterior;

            if coords.len() < 2 {
                continue;
            }

            for i in 0..coords.len() - 1 {
                all_edges.push(DirectedEdge3D::new(coords[i], coords[i + 1]));
            }

            // Closing edge
            all_edges.push(DirectedEdge3D::new(coords[coords.len() - 1], coords[0]));
        }

        if all_edges.is_empty() {
            return Polygon3DMultiZ::new(vec![]);
        }

        // ------------------------------------------------------------
        // 2. Cancel interior edges (A→B cancels B→A)
        // ------------------------------------------------------------
        let mut exterior_edges: Vec<DirectedEdge3D> = Vec::new();

        'edge_loop: for (i, e) in all_edges.iter().enumerate() {
            if all_edges
                .iter()
                .enumerate()
                .any(|(j, other)| j != i && e.matches_xy_undirected(other))
            {
                continue 'edge_loop;
            }
            exterior_edges.push(*e);
        }
        println!("Exterior edges: {:?}", exterior_edges);
        if exterior_edges.is_empty() {
            return Polygon3DMultiZ::new(vec![]);
        }

        // ------------------------------------------------------------
        // 3. Reconstruct ordered boundary edge loop
        // ------------------------------------------------------------
        let mut boundary_edges: Vec<DirectedEdge3D> = Vec::new();
        let start_edge = exterior_edges[0];
        boundary_edges.push(start_edge);

        loop {
            let last = boundary_edges.last().unwrap();
            let mut found = false;

            for e in &exterior_edges {
                if e.from.eq_xy(&last.to, EPSILON) {
                    if boundary_edges.len() > 1 {
                        let prev = boundary_edges[boundary_edges.len() - 2];
                        if e.to.eq_xy(&prev.from, EPSILON) {
                            continue;
                        }
                    }

                    if e.to.eq_xy(&start_edge.from, EPSILON) {
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

        println!("Reconstructed boundary edges: {:?}", boundary_edges);
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
            boundary_edges.reverse();
            for e in &mut boundary_edges {
                std::mem::swap(&mut e.from, &mut e.to);
            }
        }

        // ------------------------------------------------------------
        // 5. Build traversal-consistent multi-Z vertices
        // ------------------------------------------------------------
        let mut points_with_z: Vec<(f64, f64, Vec<f64>)> = Vec::new();

        for i in 0..boundary_edges.len() {
            let incoming = &boundary_edges[i];
            let outgoing = &boundary_edges[(i + 1) % boundary_edges.len()];

            let pt = incoming.to; // current vertex

            // Collect Zs from the incoming edge's end and outgoing edge's start
            let mut heights = vec![incoming.to.z, outgoing.from.z];
            heights.sort_by(|a, b| a.partial_cmp(b).unwrap());
            heights.dedup();

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

trait CeilingRegionsManipPrivate {
    fn modify_polygon<F>(&mut self, polygon: Polygon<f64>, modify: F)
    where
        F: FnMut(&mut CeilingRegion);
}

impl CeilingRegionsManipPrivate for CeilingRegions {
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

            let mut id = region.base.id;
            // remainder
            for poly in remainder.polygons {
                new_regions.push(CeilingRegion {
                    polygon: poly,
                    construction_type: region.construction_type,
                    base: RegionBase {
                        id,
                        construction_makeup: region.base.construction_makeup.clone(),
                        construction_phase: region.base.construction_phase.clone(),
                        opes: region.base.opes.clone(),
                        photos: region.base.photos.clone(),
                        description: region.base.description.clone(),
                    },
                });
                id = ItemId::default();
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
}
#[derive(Debug)]
pub enum VertexInsertionError {
    VertexNotInAnyTriangle { x: f64, y: f64 },
    DegenerateTriangle { triangle_idx: usize },
    EmptyMesh,
    EarcutError(earcutr::Error),
}

#[derive(Debug)]
pub struct CeilingMesh {
    pub triangles: Vec<CeilingTriangle>,
    pub construction_phase: ConstructionPhase,
}
impl CeilingMesh {
    pub fn add_new_vertex_with_height(
        &mut self,
        x: f64,
        y: f64,
        height: f64,
    ) -> Result<(), VertexInsertionError> {
        if self.triangles.is_empty() {
            return Err(VertexInsertionError::EmptyMesh);
        }

        let new_vertex = Point3::new(x, y, height);
        let mut triangles_to_remove = Vec::new();
        let mut triangles_to_add = Vec::new();
        let mut vertex_was_processed = false;

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
                let mut new_triangle = triangle.clone();
                new_triangle.vertices[v_idx].z = height;
                new_triangle.plane = Plane::from_triangle(&new_triangle.vertices);

                triangles_to_remove.push(idx);
                triangles_to_add.push(new_triangle);
                vertex_was_processed = true;
                continue;
            }

            // Case 2: Check if the point lies on an edge
            let edges = [(0, 1), (1, 2), (2, 0)];
            let mut on_edge = None;
            for &(i, j) in &edges {
                if point_on_segment_2d(
                    &Point2::new(x, y),
                    &triangle.vertices[i],
                    &triangle.vertices[j],
                    EPSILON,
                ) {
                    on_edge = Some((i, j));
                    break;
                }
            }

            if let Some((i, j)) = on_edge {
                let k = 3 - i - j;

                let tri1_verts = [triangle.vertices[i], new_vertex, triangle.vertices[k]];
                let tri2_verts = [new_vertex, triangle.vertices[j], triangle.vertices[k]];

                // Validate non-degenerate
                if Self::is_degenerate(&tri1_verts) || Self::is_degenerate(&tri2_verts) {
                    return Err(VertexInsertionError::DegenerateTriangle { triangle_idx: idx });
                }

                let tri1 = CeilingTriangle {
                    vertices: tri1_verts,
                    plane: Plane::from_triangle(&tri1_verts),
                };
                let tri2 = CeilingTriangle {
                    vertices: tri2_verts,
                    plane: Plane::from_triangle(&tri2_verts),
                };

                triangles_to_remove.push(idx);
                triangles_to_add.push(tri1);
                triangles_to_add.push(tri2);
                vertex_was_processed = true;
                continue;
            }

            // Case 3: Check if the point is inside the triangle
            if point_in_triangle_2d(
                &Point2::new(x, y),
                &triangle.vertices[0].xy(),
                &triangle.vertices[1].xy(),
                &triangle.vertices[2].xy(),
                EPSILON,
            ) {
                for i in 0..3 {
                    let j = (i + 1) % 3;
                    let tri_verts = [triangle.vertices[i], triangle.vertices[j], new_vertex];

                    if Self::is_degenerate(&tri_verts) {
                        return Err(VertexInsertionError::DegenerateTriangle { triangle_idx: idx });
                    }

                    let tri = CeilingTriangle {
                        vertices: tri_verts,
                        plane: Plane::from_triangle(&tri_verts),
                    };
                    triangles_to_add.push(tri);
                }

                triangles_to_remove.push(idx);
                vertex_was_processed = true;
                break;
            }
        }

        if !vertex_was_processed {
            return Err(VertexInsertionError::VertexNotInAnyTriangle { x, y });
        }

        // Apply changes
        triangles_to_remove.sort_unstable();
        for &idx in triangles_to_remove.iter().rev() {
            self.triangles.remove(idx);
        }
        self.triangles.extend(triangles_to_add);

        Ok(())
    }

    fn is_degenerate(verts: &[Point3<f64>; 3]) -> bool {
        let v0 = verts[1] - verts[0];
        let v1 = verts[2] - verts[0];
        let cross = v0.cross(&v1);
        cross.norm() < EPSILON
    }
    fn split_by_plane(&self) -> Vec<Vec<&CeilingTriangle>> {
        let mut groups: Vec<Vec<&CeilingTriangle>> = Vec::new();

        'outer: for tri in &self.triangles {
            for group in &mut groups {
                let p = &group[0].plane;
                if p.approx_eq(&tri.plane, EPSILON) {
                    group.push(tri);
                    continue 'outer;
                }
            }
            groups.push(vec![tri]);
        }

        groups
    }

    fn split_by_connectivity<'a>(tris: &'a [&'a CeilingTriangle]) -> Vec<Vec<&'a CeilingTriangle>> {
        // ============================================================
        // 2. For each plane group → split by connectivity
        // ============================================================
        let n = tris.len();

        // ---- build edge → triangles map ----
        let mut edge_map: HashMap<Edge2D, Vec<usize>> = HashMap::new();

        for (i, &tri) in tris.iter().enumerate() {
            let v = tri.vertices;
            for &(a, b) in &[(0, 1), (1, 2), (2, 0)] {
                edge_map
                    .entry(undirected_edge_2d(&v[a], &v[b]))
                    .or_default()
                    .push(i);
            }
        }

        // ---- build adjacency list ----
        let mut adj = vec![Vec::<usize>::new(); n];

        for tri_indices in edge_map.values() {
            if tri_indices.len() == 2 {
                let a = tri_indices[0];
                let b = tri_indices[1];
                adj[a].push(b);
                adj[b].push(a);
            }
        }

        // ---- connected components (DFS) ----
        let mut visited = vec![false; n];
        let mut components: Vec<Vec<&CeilingTriangle>> = Vec::new(); // <-- collect all components

        for start in 0..n {
            if visited[start] {
                continue;
            }

            let mut stack = vec![start];
            let mut component = Vec::new();
            visited[start] = true;

            while let Some(i) = stack.pop() {
                component.push(tris[i]);
                for &j in &adj[i] {
                    if !visited[j] {
                        visited[j] = true;
                        stack.push(j);
                    }
                }
            }
            components.push(component); // <-- store finished component
        }
        components
    }

    fn boundary_polygon(tris: &[&CeilingTriangle]) -> Option<Polygon3D> {
        // EXISTING boundary reconstruction logic MOVED here unchanged
        // returns None if non-manifold or degenerate
        // ====================================================
        // 3. Reconstruct boundary polygon for this component
        // ====================================================
        let mut edges = Vec::new();

        for tri in tris.iter() {
            let v = &tri.vertices;
            edges.push(undirected_edge_3df(&v[0], &v[1]));
            edges.push(undirected_edge_3df(&v[1], &v[2]));
            edges.push(undirected_edge_3df(&v[2], &v[0]));
        }

        let mut boundary_edges = Vec::new();

        for i in 0..edges.len() {
            let curr_edge = &edges[i];
            let mut count = 0;

            for test_edge in &edges {
                if test_edge == curr_edge {
                    count += 1;
                }
            }

            if count == 1 {
                boundary_edges.push(curr_edge.clone());
            }
        }

        if boundary_edges.is_empty() {
            return None;
        }

        // ---- stitch boundary ----
        let mut polygon_vertices = Vec::new();
        polygon_vertices.push(boundary_edges[0].a);
        polygon_vertices.push(boundary_edges[0].b);
        boundary_edges.remove(0);

        while !boundary_edges.is_empty() {
            let last = *polygon_vertices.last().unwrap();
            let mut found = false;

            for i in 0..boundary_edges.len() {
                let edge = &boundary_edges[i];
                let (a, b) = (edge.a, edge.b);
                if last == a {
                    polygon_vertices.push(b);
                    boundary_edges.remove(i);
                    found = true;
                    break;
                } else if last == b {
                    polygon_vertices.push(a);
                    boundary_edges.remove(i);
                    found = true;
                    break;
                }
            }

            if !found {
                return None; // malformed / non-manifold
            }
        }
        if polygon_vertices.first() != polygon_vertices.last() {
            return None; // malformed / not closed
        }
        polygon_vertices.pop(); // remove the duplicate last vertex

        Some(Polygon3D::new(
            polygon_vertices
                .iter()
                .map(|&(x, y, z)| Point3::new(x, y, z))
                .collect::<Vec<Point3<f64>>>(),
        ))
    }
}

impl TryFrom<&CeilingRegion> for CeilingMesh {
    type Error = earcutr::Error; // or the actual error type from earcutr

    fn try_from(ceiling_region: &CeilingRegion) -> Result<Self, Self::Error> {
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
            construction_phase: ceiling_region.base.construction_phase.clone(),
        })
    }
}

impl TryFrom<CeilingRegion> for CeilingMesh {
    type Error = earcutr::Error; // or the actual error type from earcutr

    fn try_from(ceiling_region: CeilingRegion) -> Result<Self, Self::Error> {
        let vertices_3d = ceiling_region.polygon.exterior;

        if vertices_3d.len() < 3 {
            return Err(earcutr::Error::Unknown);
        }

        // Flatten into 2D
        let mut coords_2d = Vec::with_capacity(vertices_3d.len() * 2);
        for v in &vertices_3d {
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
            construction_phase: ceiling_region.base.construction_phase.clone(),
        })
    }
}
impl Plane {
    fn new(normal: Vector3<f64>, d: f64) -> Self {
        let norm = normal.norm();
        debug_assert!(norm.is_finite() && norm > 0.0);

        let n = normal / norm;
        Plane {
            normal: n,
            d: d / norm,
        }
    }
    pub fn approx_eq(&self, other: &Plane, eps: f64) -> bool {
        let dot = self.normal.dot(&other.normal);

        if dot > 1.0 - eps {
            // same orientation
            (self.d - other.d).abs() < eps
        } else if dot < -1.0 + eps {
            // opposite orientation
            (self.d + other.d).abs() < eps
        } else {
            false
        }
    }

    // Solve for z = (-d - nx*x - ny*y) / nz
    pub fn z_at(&self, x: f64, y: f64) -> f64 {
        (-self.d - self.normal.x * x - self.normal.y * y) / self.normal.z
    }

    fn from_triangle(verts: &[Point3<f64>; 3]) -> Self {
        let v0 = verts[1] - verts[0];
        let v1 = verts[2] - verts[0];
        let normal = v0.cross(&v1);
        let d = -normal.dot(&verts[0].coords);
        Plane::new(normal, d)
    }
}

#[derive(Clone, Debug)]
pub struct Plane {
    normal: Vector3<f64>,
    d: f64,
}

#[derive(Clone, Debug)]
pub struct CeilingTriangle {
    pub vertices: [Point3<f64>; 3],
    pub plane: Plane,
}
pub struct CeilingRegionConversion {
    pub regions: Vec<CeilingRegion>,
    pub failed_components: usize,
}

impl From<CeilingMesh> for CeilingRegionConversion {
    fn from(mesh: CeilingMesh) -> Self {
        let mut regions = Vec::new();
        let mut failed_components = 0;

        for coplanar in mesh.split_by_plane() {
            for connected in CeilingMesh::split_by_connectivity(&coplanar) {
                match CeilingMesh::boundary_polygon(&connected) {
                    Some(polygon) => {
                        regions.push(CeilingRegion {
                            polygon,
                            construction_type: CeilingConstructionType::Uninitialised,
                            base: RegionBase {
                                id: ItemId::default(),
                                construction_phase: mesh.construction_phase.clone(),
                                construction_makeup: ConstructionMakeup::default(),
                                ..Default::default()
                            },
                        });
                    }
                    None => {
                        failed_components += 1;
                    }
                }
            }
        }

        CeilingRegionConversion {
            regions,
            failed_components,
        }
    }
}

impl HasBaseRegion for CeilingRegion {
    fn base_mut(&mut self) -> &mut RegionBase {
        &mut self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nalgebra::Point3;

    // Helper function to create a square region
    fn create_square_region(z: f64) -> CeilingRegion {
        CeilingRegion {
            polygon: Polygon3D::new(vec![
                Point3::new(0.0, 0.0, z),
                Point3::new(1.0, 0.0, z),
                Point3::new(1.0, 1.0, z),
                Point3::new(0.0, 1.0, z),
            ]),
            construction_type: CeilingConstructionType::Uninitialised,
            base: RegionBase {
                id: ItemId::default(),
                construction_makeup: ConstructionMakeup::default(),
                construction_phase: ConstructionPhase::Original,
                ..Default::default()
            },
        }
    }
    #[test]
    fn test_quantize() {
        let p = Point3::new(1.23456789, 9.87654321, 0.0);
        let (x, y) = quantize(&p);

        assert_eq!(x, 1234567890);
        assert_eq!(y, 9876543210);
    }

    #[test]
    fn test_undirected_edge_2d() {
        let p1 = Point3::new(0.0, 0.0, 0.0);
        let p2 = Point3::new(1.0, 1.0, 0.0);

        let edge1 = undirected_edge_2d(&p1, &p2);
        let edge2 = undirected_edge_2d(&p2, &p1);

        assert_eq!(edge1, edge2); // Should be the same regardless of order
    }

    #[test]
    fn test_undirected_edge_3df_equality() {
        let p1 = Point3::new(0.0, 0.0, 0.0);
        let p2 = Point3::new(1.0, 1.0, 1.0);

        let edge1 = undirected_edge_3df(&p1, &p2);
        let edge2 = undirected_edge_3df(&p2, &p1);

        assert_eq!(edge1, edge2);
    }

    #[test]
    fn test_edge3df_equality_with_epsilon() {
        let p1 = Point3::new(0.0, 0.0, 0.0);
        let p2 = Point3::new(1.0, 1.0, 1.0);
        let p2_close = Point3::new(1.0 + EPSILON / 2.0, 1.0, 1.0);

        let edge1 = undirected_edge_3df(&p1, &p2);
        let edge2 = undirected_edge_3df(&p1, &p2_close);

        assert_eq!(edge1, edge2);
    }

    #[test]
    fn test_ceiling_regions_new() {
        let vertices = vec![
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(0.5, 1.0, 0.0),
        ];

        let regions = CeilingRegions::new(vertices, None, None, None);

        assert_eq!(regions.regions.len(), 1);
        assert_eq!(
            regions.regions[0].construction_type,
            CeilingConstructionType::Uninitialised
        );
        assert_eq!(
            regions.regions[0].base.construction_phase,
            ConstructionPhase::Original
        );
    }

    #[test]
    fn test_ceiling_regions_modify_polygon_increase_height() {
        let vertices = vec![
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(1.0, 1.0, 0.0),
            Point3::new(0.0, 1.0, 0.0),
        ];

        let mut regions = CeilingRegions::new(vertices, None, None, None);

        // Create a polygon that overlaps
        let poly = geo::Polygon::new(
            geo::LineString::from(vec![
                (0.0, 0.0),
                (1.0, 0.0),
                (1.0, 1.0),
                (0.0, 1.0),
                (0.0, 0.0),
            ]),
            vec![],
        );

        regions.modify_polygon_increase_height(poly, 2.0);

        // Check that heights were increased
        for vertex in &regions.regions[0].polygon.exterior {
            assert_eq!(vertex.z, 2.0);
        }
    }

    #[test]
    fn test_plane_from_triangle() {
        let verts = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(0.0, 1.0, 0.0),
        ];

        let plane = Plane::from_triangle(&verts);

        // This should be a plane with normal pointing up (0, 0, 1)
        assert!((plane.normal.z.abs() - 1.0).abs() < EPSILON);
    }

    #[test]
    fn test_plane_approx_eq() {
        let verts1 = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(0.0, 1.0, 0.0),
        ];
        let verts2 = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(2.0, 0.0, 0.0),
            Point3::new(0.0, 2.0, 0.0),
        ];

        let plane1 = Plane::from_triangle(&verts1);
        let plane2 = Plane::from_triangle(&verts2);

        assert!(plane1.approx_eq(&plane2, EPSILON));
    }

    #[test]
    fn test_plane_z_at() {
        // Create a sloped plane
        let verts = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 1.0),
            Point3::new(0.0, 1.0, 0.0),
        ];

        let plane = Plane::from_triangle(&verts);
        let z = plane.z_at(0.5, 0.0);

        // Should be approximately 0.5 for this plane
        assert!((z - 0.5).abs() < 0.1);
    }

    #[test]
    fn test_is_degenerate_collinear() {
        let verts = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(2.0, 0.0, 0.0), // Collinear
        ];

        assert!(CeilingMesh::is_degenerate(&verts));
    }

    #[test]
    fn test_is_degenerate_valid() {
        let verts = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(0.0, 1.0, 0.0),
        ];

        assert!(!CeilingMesh::is_degenerate(&verts));
    }

    #[test]
    fn test_ceiling_mesh_try_from_region() {
        let region = create_square_region(0.0);
        let mesh = CeilingMesh::try_from(region);

        assert!(mesh.is_ok());
        let mesh = mesh.unwrap();
        assert_eq!(mesh.triangles.len(), 2); // Square should be 2 triangles
    }

    #[test]
    fn test_ceiling_mesh_try_from_invalid_region() {
        let region = CeilingRegion {
            polygon: Polygon3D::new(vec![Point3::new(0.0, 0.0, 0.0), Point3::new(1.0, 0.0, 0.0)]), // Only 2 vertices
            construction_type: CeilingConstructionType::Uninitialised,
            base: RegionBase {
                id: ItemId::default(),
                construction_makeup: ConstructionMakeup::default(),
                construction_phase: ConstructionPhase::Original,
                ..Default::default()
            },
        };

        let mesh = CeilingMesh::try_from(region);
        assert!(mesh.is_err());
    }

    #[test]
    fn test_add_vertex_empty_mesh() {
        let mut mesh = CeilingMesh {
            triangles: vec![],
            construction_phase: ConstructionPhase::Original,
        };

        let result = mesh.add_new_vertex_with_height(0.5, 0.5, 1.0);
        assert!(matches!(result, Err(VertexInsertionError::EmptyMesh)));
    }

    #[test]
    fn test_add_vertex_inside_triangle() {
        let region = create_square_region(0.0);
        let mut mesh = CeilingMesh::try_from(region).unwrap();
        let initial_count = mesh.triangles.len();

        // Add vertex in the middle of the square
        let result = mesh.add_new_vertex_with_height(0.5, 0.5, 1.0);
        assert!(result.is_ok());

        // Should have split one triangle into 3, so net +2 triangles
        assert_eq!(mesh.triangles.len(), initial_count + 2);
    }

    #[test]
    fn test_add_vertex_on_edge() {
        let region = create_square_region(0.0);
        let mut mesh = CeilingMesh::try_from(region).unwrap();
        let initial_count = mesh.triangles.len();

        // Add vertex on an edge
        let result = mesh.add_new_vertex_with_height(0.5, 0.0, 1.0);
        assert!(result.is_ok());

        // Should have split one triangle into 2, so net +1 triangle
        assert_eq!(mesh.triangles.len(), initial_count + 1);
    }

    #[test]
    fn test_add_vertex_at_existing_vertex() {
        let region = create_square_region(0.0);
        let mut mesh = CeilingMesh::try_from(region).unwrap();
        let initial_count = mesh.triangles.len();

        // Add vertex at existing location with different height
        let result = mesh.add_new_vertex_with_height(0.0, 0.0, 2.0);
        assert!(result.is_ok());

        // Should have same number of triangles but updated height
        assert_eq!(mesh.triangles.len(), initial_count);
    }

    #[test]
    fn test_add_vertex_outside_mesh() {
        let region = create_square_region(0.0);
        let mut mesh = CeilingMesh::try_from(region).unwrap();

        // Add vertex outside the mesh
        let result = mesh.add_new_vertex_with_height(5.0, 5.0, 1.0);
        assert!(matches!(
            result,
            Err(VertexInsertionError::VertexNotInAnyTriangle { .. })
        ));
    }

    #[test]
    fn test_split_by_plane_single_plane() {
        let region = create_square_region(0.0);
        let mesh = CeilingMesh::try_from(region).unwrap();

        let groups = mesh.split_by_plane();

        // All triangles should be in one group (same plane)
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), mesh.triangles.len());
    }

    #[test]
    fn test_split_by_connectivity_single_component() {
        let region = create_square_region(0.0);
        let mesh = CeilingMesh::try_from(region).unwrap();

        let plane_groups = mesh.split_by_plane();
        let components = CeilingMesh::split_by_connectivity(&plane_groups[0]);

        // Should be one connected component
        assert_eq!(components.len(), 1);
    }

    #[test]
    fn test_boundary_polygon_simple_square() {
        let region = create_square_region(0.0);
        let mesh = CeilingMesh::try_from(region).unwrap();

        let plane_groups = mesh.split_by_plane();
        let components = CeilingMesh::split_by_connectivity(&plane_groups[0]);
        let boundary = CeilingMesh::boundary_polygon(&components[0]);

        assert!(boundary.is_some());
        let polygon = boundary.unwrap();
        assert_eq!(polygon.exterior.len(), 4); // Square has 4 vertices
    }

    #[test]
    fn test_ceiling_mesh_into_conversion() {
        let region = create_square_region(0.0);
        let mesh = CeilingMesh::try_from(region).unwrap();

        let conversion: CeilingRegionConversion = mesh.into();

        assert_eq!(conversion.regions.len(), 1);
        assert_eq!(conversion.failed_components, 0);
    }

    #[test]
    fn test_modify_polygon_construction_phase() {
        let vertices = vec![
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(1.0, 1.0, 0.0),
            Point3::new(0.0, 1.0, 0.0),
        ];

        let mut regions = CeilingRegions::new(vertices, None, None, None);

        let poly = geo::Polygon::new(
            geo::LineString::from(vec![
                (0.0, 0.0),
                (1.0, 0.0),
                (1.0, 1.0),
                (0.0, 1.0),
                (0.0, 0.0),
            ]),
            vec![],
        );

        regions.modify_polygon_construction_phase(
            poly,
            ConstructionPhase::Other("NewPhase".to_string()),
        );

        assert_eq!(
            regions.regions[0].base.construction_phase,
            ConstructionPhase::Other("NewPhase".to_string())
        );
    }

    #[test]
    fn test_ceiling_triangle_plane_consistency() {
        let verts = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(1.0, 0.0, 0.0),
            Point3::new(0.0, 1.0, 0.0),
        ];

        let triangle = CeilingTriangle {
            vertices: verts,
            plane: Plane::from_triangle(&verts),
        };

        // All vertices should lie on the plane
        for v in &triangle.vertices {
            let z_on_plane = triangle.plane.z_at(v.x, v.y);
            assert!((z_on_plane - v.z).abs() < EPSILON);
        }
    }

    #[test]
    fn test_degenerate_detection_zero_area() {
        let verts = [
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(0.0, 0.0, 0.0),
            Point3::new(0.0, 0.0, 0.0),
        ];

        assert!(CeilingMesh::is_degenerate(&verts));
    }

    #[test]
    fn test_conversion_preserves_construction_phase() {
        let region = CeilingRegion {
            polygon: Polygon3D::new(vec![
                Point3::new(0.0, 0.0, 0.0),
                Point3::new(1.0, 0.0, 0.0),
                Point3::new(1.0, 1.0, 0.0),
                Point3::new(0.0, 1.0, 0.0),
            ]),
            construction_type: CeilingConstructionType::Uninitialised,
            base: RegionBase {
                id: ItemId::default(),
                construction_makeup: ConstructionMakeup::default(),
                construction_phase: ConstructionPhase::Other("TestPhase".to_string()),
                ..Default::default()
            },
        };

        let mesh = CeilingMesh::try_from(region).unwrap();
        let conversion: CeilingRegionConversion = mesh.into();

        assert_eq!(
            conversion.regions[0].base.construction_phase,
            ConstructionPhase::Other("TestPhase".to_string())
        );
    }

    #[test]
    fn test_simple_flat_square_ceiling() {
        let vertices = vec![
            Point3::new(0.0, 0.0, 2.5),
            Point3::new(1.0, 0.0, 2.5),
            Point3::new(1.0, 1.0, 2.5),
            Point3::new(0.0, 1.0, 2.5),
        ];

        let ceilings = CeilingRegions::new(vertices, None, None, None);
        let polygon_3d = ceilings.get_clockwise_3d_polygon_starting_closest_to_origin();

        // Should have 4 vertices
        assert_eq!(polygon_3d.exterior.len(), 4);

        // Check all Z values
        for (_, _, zs) in &polygon_3d.exterior {
            assert_eq!(zs.len(), 1);
            assert!((zs[0] - 2.5).abs() < 1e-9);
        }
    }

    #[test]
    fn test_sloped_ceiling_edge() {
        let vertices = vec![
            Point3::new(0.0, 0.0, 2.0),
            Point3::new(1.0, 0.0, 2.5),
            Point3::new(1.0, 1.0, 3.0),
            Point3::new(0.0, 1.0, 2.5),
        ];

        let ceilings = CeilingRegions::new(vertices, None, None, None);
        let polygon_3d = ceilings.get_clockwise_3d_polygon_starting_closest_to_origin();

        assert_eq!(polygon_3d.exterior.len(), 4);

        // Each vertex should have only 1 1 z value
        for (_, _, zs) in &polygon_3d.exterior {
            assert!(!zs.is_empty());
            assert!(zs.len() == 1);
        }

        // The lowest Z should be 2.0 and the highest 3.0
        let mut min_z = f64::MAX;
        let mut max_z = f64::MIN;
        for (_, _, zs) in &polygon_3d.exterior {
            for z in zs {
                min_z = min_z.min(*z);
                max_z = max_z.max(*z);
            }
        }
        assert!((min_z - 2.0).abs() < 1e-9);
        assert!((max_z - 3.0).abs() < 1e-9);
    }

    #[test]
    fn test_multiple_ceiling_regions() {
        let region1 = vec![
            Point3::new(0.0, 0.0, 2.0),
            Point3::new(1.0, 0.0, 2.0),
            Point3::new(1.0, 1.0, 2.0),
            Point3::new(0.0, 1.0, 2.0),
        ];

        let region2 = vec![
            Point3::new(1.0, 0.0, 2.5),
            Point3::new(2.0, 0.0, 2.5),
            Point3::new(2.0, 1.0, 2.5),
            Point3::new(1.0, 1.0, 2.5),
        ];

        let mut ceilings = CeilingRegions::new(region1, None, None, None);
        ceilings.regions.push(CeilingRegion {
            polygon: Polygon3D::new(region2),
            construction_type: CeilingConstructionType::Uninitialised,
            base: RegionBase {
                id: ItemId::default(),
                construction_makeup: ConstructionMakeup::default(),
                construction_phase: ConstructionPhase::Original,
                ..Default::default()
            },
        });

        println!("{:#?}", ceilings);
        let polygon_3d = ceilings.get_clockwise_3d_polygon_starting_closest_to_origin();

        println!("{:?}", polygon_3d);
        // There should be 6 vertices
        assert!(polygon_3d.exterior.len() == 6);

        // Check that all Z values are correctly present
        for (_, _, zs) in &polygon_3d.exterior {
            assert!(!zs.is_empty());
        }
    }
}
