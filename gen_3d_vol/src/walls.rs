//! Wall split-by-line: divide one `WallRegion`'s face along a length/height
//! intercept line into separately-attributed regions.
//!
//! Kept because the canonical wall pipeline (`WallRegions::regenerate` in
//! `datatypes`, driven by `document::resolve_storey`) deliberately defers
//! face splits (the wall-exposure design's "face/height diagonal split");
//! this is the geometric core a future split feature would re-apply. Wall
//! *generation* (perimeter + discontinuity walls) is owned by `regenerate`
//! and the resolve-time riser seam now.

// standard includes
use datatypes::ber::common::ItemId;
use datatypes::ber::structural::{WallRegion, WallRegions};
use domain::points::{Point3f64, Vector2f64, Vector3f64};

// local includes
use super::geometry::{Geometry, SplitLine3D};

trait WallRegionManip {
    fn get_leftmost_floor_vertex(&self) -> Point3f64;
    fn wall_horizontal_dir(&self) -> Vector3f64;
    fn wall_plane_normal(&self) -> Vector3f64;
    fn get_dividing_line_equation(&self, split_along_xy: f64, split_along_z: f64) -> SplitLine3D;
}

impl WallRegionManip for WallRegion {
    /// Get the length of the wall region (projected to XY plane)
    fn get_leftmost_floor_vertex(&self) -> Point3f64 {
        self.polygon.exterior[0]
    }

    fn wall_horizontal_dir(&self) -> Vector3f64 {
        let p0 = self.polygon.exterior[0];
        let p1 = self.polygon.exterior[1];
        Vector3f64::new(p1.x - p0.x, p1.y - p0.y, 0.0)
    }

    fn wall_plane_normal(&self) -> Vector3f64 {
        let p0 = self.polygon.exterior[0];
        let p1 = self.polygon.exterior[1];
        let p2 = self.polygon.exterior[2];
        let u = Vector3f64::new(p1.x - p0.x, p1.y - p0.y, p1.z - p0.z);
        let v = Vector3f64::new(p2.x - p0.x, p2.y - p0.y, p2.z - p0.z);
        u.cross(&v).normalize()
    }

    fn get_dividing_line_equation(&self, split_along_xy: f64, split_along_z: f64) -> SplitLine3D {
        let origin = self.get_leftmost_floor_vertex();

        let u = self.wall_horizontal_dir().normalize(); // in XY
        let v = Vector3f64::new(0.0, 0.0, 1.0);

        // 2D normal in wall coordinates
        let mut n2 = Vector2f64::new(0.0, 0.0);

        if split_along_xy != 0.0 {
            n2.x = 1.0 / split_along_xy;
        }
        if split_along_z != 0.0 {
            n2.y = 1.0 / split_along_z;
        }

        // Convert to 3D normal
        let normal = (u * n2.x + v * n2.y).normalize();

        // Find a point on the line
        // u/s_xy + v/s_z = 1
        let point = origin + u * split_along_xy.max(0.0) + v * split_along_z.max(0.0);

        // Line direction is perpendicular to normal *in wall plane*
        let direction = normal.cross(&self.wall_plane_normal()).normalize();

        SplitLine3D {
            point,
            direction,
            normal,
        }
    }
}

pub trait WallRegionsManip {
    fn get_wall_region_by_id_mut(&mut self, id: &ItemId) -> Option<&mut WallRegion>;
    fn split_wall_region<F>(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        update_new_region: F,
    ) where
        F: FnMut(&mut WallRegion);
}

impl WallRegionsManip for WallRegions {
    fn get_wall_region_by_id_mut(&mut self, id: &ItemId) -> Option<&mut WallRegion> {
        self.regions.iter_mut().find(|region| &region.id == id)
    }

    fn split_wall_region<F>(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        mut update_new_region: F,
    ) where
        F: FnMut(&mut WallRegion),
    {
        let Some(wall_region) = self.get_wall_region_by_id_mut(&wall_id) else {
            println!("Wall with id {:?} not found.", wall_id);
            return;
        };

        let split_line = wall_region.get_dividing_line_equation(split_along_xy, split_along_z);
        let split_polygons = wall_region.polygon.split_by_line(&split_line);

        if split_polygons.is_empty() {
            println!("Split resulted in no polygons for wall {:?}", wall_id);
            return;
        }

        // First polygon stays on the original region
        wall_region.polygon = split_polygons[0].clone();

        let wall_region_clone = wall_region.clone();

        for split_polygon in split_polygons.iter().skip(1) {
            let mut new_wall_region = wall_region_clone.clone();
            new_wall_region.id = ItemId::default();
            new_wall_region.polygon = split_polygon.clone();

            update_new_region(&mut new_wall_region);

            self.regions.push(new_wall_region);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use datatypes::ber::structural::{Opes, Polygon3D, WallExposure};

    const TOL: f64 = 1e-9;

    /* ---------------------------------------------------------------------
     * Test helpers
     * ------------------------------------------------------------------ */

    fn simple_wall() -> WallRegion {
        WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 3.0),
                Point3f64::new(0.0, 0.0, 3.0),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        }
    }

    fn simple_wall_regions() -> WallRegions {
        WallRegions {
            regions: vec![simple_wall()],
        }
    }

    fn assert_unit(v: Vector3f64) {
        assert!((v.norm() - 1.0).abs() < TOL);
    }

    /* ---------------------------------------------------------------------
     * WallRegionManip
     * ------------------------------------------------------------------ */

    #[test]
    fn leftmost_floor_vertex_is_first_exterior_vertex() {
        let wall = simple_wall();
        assert_eq!(
            wall.get_leftmost_floor_vertex(),
            Point3f64::new(0.0, 0.0, 0.0)
        );
    }

    #[test]
    fn wall_horizontal_dir_is_xy_only() {
        let wall = simple_wall();
        let d = wall.wall_horizontal_dir();
        assert_eq!(d, Vector3f64::new(4.0, 0.0, 0.0));
    }

    #[test]
    fn wall_plane_normal_is_unit_and_perpendicular() {
        let wall = simple_wall();
        let n = wall.wall_plane_normal();

        assert_unit(n);

        let horiz = wall.wall_horizontal_dir();
        assert!((n.dot(&horiz)).abs() < TOL);
    }

    #[test]
    fn dividing_line_geometry_is_consistent() {
        let wall = simple_wall();
        let split = wall.get_dividing_line_equation(2.0, 1.5);

        assert_unit(split.normal);
        assert_unit(split.direction);

        // Direction must lie in the wall plane
        let plane_n = wall.wall_plane_normal();
        assert!((split.direction.dot(&plane_n)).abs() < TOL);

        // Normal must be orthogonal to direction
        assert!((split.direction.dot(&split.normal)).abs() < TOL);
    }

    /* ---------------------------------------------------------------------
     * WallRegionsManip – lookup & split
     * ------------------------------------------------------------------ */

    #[test]
    fn get_wall_region_by_id_mut_returns_correct_region() {
        let mut walls = simple_wall_regions();
        let id = walls.regions[0].id;

        let r = walls.get_wall_region_by_id_mut(&id).unwrap();
        r.exposure = WallExposure::NonHeatLoss;

        assert_eq!(walls.regions[0].exposure, WallExposure::NonHeatLoss);
    }

    #[test]
    fn split_wall_region_creates_multiple_regions() {
        let mut walls = simple_wall_regions();
        let id = walls.regions[0].id;

        walls.split_wall_region(id, 2.0, 0.0, |_| {});

        assert!(walls.regions.len() >= 2);
    }

    #[test]
    fn split_wall_region_noop_if_id_missing() {
        let mut walls = simple_wall_regions();
        walls.split_wall_region(ItemId::default(), 1.0, 0.0, |_| {});
        assert_eq!(walls.regions.len(), 1);
    }
}
