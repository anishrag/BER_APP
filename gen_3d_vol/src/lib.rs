mod ceiling;
mod floor;
mod geometry;
mod types;
mod walls;

use domain::points::{Point2f64, Point3f64};
use geo::{LineString, Polygon};

use datatypes::ber::common::ItemId;

pub use datatypes::ber::structural::FloorConstructionType;
use datatypes::ber::structural::{
    CeilingConstructionType, CeilingRegion, CeilingRegions, ConstructionMakeup, FloorRegion,
    FloorRegions, Storey, WallRegion, WallRegions,
};

use crate::ceiling::CeilingRegionsManip;
use crate::floor::FloorRegionsManip;
use crate::geometry::{Geometry, MergeableGeometry, MergeableRegion};
use crate::types::{MergeWarning, RegionContainer, RegionType, EPSILON};
use crate::walls::WallRegionsManip;

pub use datatypes::ber::common::ConstructionPhase;

pub enum Gen3DError {
    PointNotOnEdge,
    HeightConflict,
    CeilingError,
}

pub struct Gen3D {
    pub storey: u32,
    pub floors: FloorRegions,
    pub walls: WallRegions,
    pub ceilings: CeilingRegions,
}

//TODO: ddecide where storey lives
impl Gen3D {
    // costructors
    pub fn new_floor(
        vertices: Vec<Point2f64>,
        height: f64,
        construction_type: FloorConstructionType,
        floor_plan_below: Option<&Gen3D>,
    ) -> Self {
        let floor_regions;
        let mut storey = 0;
        match floor_plan_below {
            Some(floor_plan) => {
                let floor_polygon_2d = Polygon::new(
                    LineString::from(
                        vertices
                            .iter()
                            .map(|p| (p.x, p.y))
                            .collect::<Vec<(f64, f64)>>(),
                    ),
                    vec![],
                );
                let ceiling_regions_below = &floor_plan.ceilings;
                let floor_polygons: Vec<(Polygon<f64>, f64)> = ceiling_regions_below
                    .regions
                    .iter()
                    .flat_map(|region| region.polygon.clone().intersection(&floor_polygon_2d))
                    .map(|poly3d| {
                        let (poly2d, heights) = poly3d.to_2d_polygon_and_heights();
                        // Assert all heights are the same
                        assert!(heights.iter().all(|&h| (h - heights[0]).abs() < EPSILON));
                        (poly2d, heights[0])
                    })
                    .collect();
                let (polygons, heights): (Vec<&Polygon>, Vec<f64>) =
                    floor_polygons.iter().map(|(p, h)| (p, *h)).unzip();
                let n = polygons.len();
                floor_regions = FloorRegions::from_polygons(
                    polygons,
                    heights,
                    vec![None; n],
                    vec![None; n],
                    vec![None; n],
                );
                storey = floor_plan.storey + 1;
            }
            None => {
                floor_regions =
                    FloorRegions::new(vertices, height, Some(construction_type), None, None);
            }
        }

        Self {
            storey,
            floors: floor_regions,
            walls: WallRegions { regions: vec![] },
            ceilings: CeilingRegions { regions: vec![] },
        }
    }

    // ----------------------------------------------------------------------------------------------
    // --- Floor modification methods ---
    // ----------------------------------------------------------------------------------------------
    pub fn raise_lower_floor_split(
        &mut self,
        vertices: Vec<Point2f64>,
        split_height_change: f64,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.floors
            .raise_lower_polygon(polygon, split_height_change);
    }

    pub fn change_floor_construction_phase_split(
        &mut self,
        vertices: Vec<Point2f64>,
        phase: ConstructionPhase,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.floors
            .modify_polygon_construction_phase(polygon, phase);
    }

    pub fn change_floor_construction_type_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_type: FloorConstructionType,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.floors
            .modify_polygon_all_fields(polygon, None, Some(construction_type), None, None);
    }

    pub fn change_floor_construction_makeup_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_makeup: ConstructionMakeup,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.floors
            .modify_polygon_all_fields(polygon, None, None, Some(construction_makeup), None);
    }

    // ----------------------------------------------------------------------------------------------
    // --- Ceiling modification methods ---
    // ----------------------------------------------------------------------------------------------
    pub fn add_ceiling_with_height(&mut self, height: f64) {
        let first_floor_height = self.floors.regions[0].height;
        let height = first_floor_height + height;
        let exterior_polygon = self
            .floors
            .get_clockwise_3d_polygon_starting_closest_to_origin();
        let vertices_2d: Vec<Point2f64> = exterior_polygon.into();
        let vertices_3d: Vec<Point3f64> = vertices_2d
            .into_iter()
            .map(|p| Point3f64::new(p.x, p.y, height))
            .collect();
        self.ceilings = CeilingRegions::new(vertices_3d, None, None, None);
    }

    pub fn raise_lower_ceiling_split(
        &mut self,
        vertices: Vec<Point2f64>,
        split_height_change: f64,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.ceilings
            .modify_polygon_increase_height(polygon, split_height_change);
    }

    pub fn set_point_height_ceiling(
        &mut self,
        point: Point2f64,
        new_height: f64,
        from_highest_region: bool,
    ) -> Result<(), Gen3DError> {
        let floor_heights = self.floors.get_floor_height_at_point(&point);
        let floor_z = if from_highest_region {
            floor_heights
                .iter()
                .copied()
                .max_by(|a, b| a.partial_cmp(b).unwrap())
                .unwrap()
        } else {
            floor_heights
                .iter()
                .copied()
                .min_by(|a, b| a.partial_cmp(b).unwrap())
                .unwrap()
        };

        self.ceilings.insert_vertex(point, new_height + floor_z);

        Ok(())
    }

    pub fn change_ceiling_construction_makeup_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_makeup: ConstructionMakeup,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.ceilings
            .modify_polygon_all_fields(polygon, None, Some(construction_makeup), None);
    }

    pub fn change_ceiling_construction_type_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_type: CeilingConstructionType,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.ceilings
            .modify_polygon_all_fields(polygon, Some(construction_type), None, None);
    }

    pub fn change_ceiling_construction_phase_split(
        &mut self,
        vertices: Vec<Point2f64>,
        phase: ConstructionPhase,
    ) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.ceilings
            .modify_polygon_construction_phase(polygon, phase);
    }

    // ----------------------------------------------------------------------------------------------
    // --- Wall modification methods ---
    // ----------------------------------------------------------------------------------------------
    //
    pub fn create_walls_betweeen_floor_edges(&mut self) {
        self.walls
            .add_from_floor_discontinuities(&self.floors, None, None, None);
    }

    pub fn create_walls_between_ceiling_edges(&mut self) {
        self.walls
            .add_from_ceiling_discontinuities(&self.ceilings, None, None, None);
    }

    pub fn create_exterior_walls(&mut self) {
        self.walls.add_from_exterior_perimeter(
            &self.floors,
            &self.ceilings,
            None,
            None,
            None,
            None,
        );
    }

    pub fn change_wall_construction_makeup_split(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        construction_makeup: ConstructionMakeup,
    ) {
        self.walls
            .split_wall_region(wall_id, split_along_xy, split_along_z, |region| {
                region.base.construction_makeup = construction_makeup.clone();
            });
    }

    pub fn change_wall_construction_phase_split(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        phase: ConstructionPhase,
    ) {
        self.walls
            .split_wall_region(wall_id, split_along_xy, split_along_z, |region| {
                region.base.construction_phase = phase.clone();
            });
    }

    pub fn change_wall_construction_type_split(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        construction_type: datatypes::ber::structural::WallConstructionType,
    ) {
        self.walls
            .split_wall_region(wall_id, split_along_xy, split_along_z, |region| {
                region.construction_type = construction_type;
            });
    }

    // ----------------------------------------------------------------------------------------------
    // --- General modification methods ---
    // ----------------------------------------------------------------------------------------------

    pub fn combine_regions(
        &mut self,
        region_ids: &mut Vec<ItemId>,
        region_type: RegionType,
    ) -> Vec<MergeWarning> {
        match region_type {
            RegionType::Floor => self.combine_regions_generic::<FloorRegion>(region_ids),
            RegionType::Wall => self.combine_regions_generic::<WallRegion>(region_ids),
            RegionType::Ceiling => self.combine_regions_generic::<CeilingRegion>(region_ids),
        }
    }

    fn combine_regions_generic<T>(&mut self, region_ids: &mut Vec<ItemId>) -> Vec<MergeWarning>
    where
        T: MergeableRegion,
        Gen3D: RegionContainer<T>,
    {
        let mut warnings = Vec::new();
        // Select only the regions we want to merge
        let regions = self.regions();
        let mut selected: Vec<&T> = regions
            .iter()
            .filter(|r: &&T| region_ids.contains(&r.id()))
            .collect();

        // Warn for invalid IDs
        let invalid_ids: Vec<ItemId> = region_ids
            .iter()
            .filter(|id| !regions.iter().any(|r| &r.id() == *id))
            .copied()
            .collect();

        if !invalid_ids.is_empty() {
            warnings.push(MergeWarning::InvalidIds(invalid_ids));
        }

        // Process each selected region
        while let Some(region) = selected.pop() {
            let mut group = vec![region.clone()];

            // Find all merge-compatible regions
            selected.retain(|other| {
                if region.is_merge_compatible(other) {
                    group.push((*other).clone());
                    false
                } else {
                    true
                }
            });

            // --- Iterative / convergence merge ---
            // Start with all polygons in the group as separate geometries
            let mut geometries: Vec<T::Geometry> =
                group.iter().map(|r| r.geometry().clone()).collect();

            let mut changed = true;
            while changed {
                changed = false;
                let mut new_geometries = Vec::new();
                let mut skip = vec![false; geometries.len()];

                for i in 0..geometries.len() {
                    if skip[i] {
                        continue;
                    }
                    let mut merged_geom = geometries[i].clone();
                    for j in (i + 1)..geometries.len() {
                        if skip[j] {
                            continue;
                        }
                        // Attempt merge
                        let union_result = merged_geom.merge(&geometries[j]);
                        if union_result.len() == 1 {
                            // Successfully merged
                            merged_geom = union_result.into_iter().next().unwrap();
                            skip[j] = true;
                            changed = true;
                        }
                    }
                    new_geometries.push(merged_geom);
                }
                geometries = new_geometries;
            }
            // --- end convergence merge ---

            let source_ids: Vec<ItemId> = group.iter().map(|r| r.id()).collect();

            // Build new merged regions
            for geom in geometries.iter().cloned() {
                region.with_geometry(geom);
                region_ids.retain(|id| id != &region.id());
            }

            if geometries.len() > 1 {
                warnings.push(MergeWarning::MultipleResultingRegions {
                    source_ids,
                    resulting_regions: geometries.len(),
                });
            }
        }

        warnings
    }
}

impl RegionContainer<FloorRegion> for Gen3D {
    fn regions(&self) -> &Vec<FloorRegion> {
        &self.floors.regions
    }
}
impl RegionContainer<CeilingRegion> for Gen3D {
    fn regions(&self) -> &Vec<CeilingRegion> {
        &self.ceilings.regions
    }
}
impl RegionContainer<WallRegion> for Gen3D {
    fn regions(&self) -> &Vec<WallRegion> {
        &self.walls.regions
    }
}

impl From<Storey> for Gen3D {
    fn from(storey: Storey) -> Self {
        Self {
            storey: storey.storey_number as u32,
            floors: FloorRegions::from(storey.floors),
            walls: WallRegions::from(storey.walls),
            ceilings: CeilingRegions::from(storey.roofs),
        }
    }
}
