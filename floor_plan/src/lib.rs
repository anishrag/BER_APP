pub mod ceiling;
pub mod geometry;
pub mod walls;

mod floor;
mod types;
use geo::{LineString, Polygon};
use nalgebra::{Point2, Point3};

use ceiling::{CeilingMesh, CeilingRegion, CeilingRegionConversion, CeilingRegions};
use datatypes::ber::diffable::AddRemoveTrait;
use datatypes::ber::structural::{
    CeilingConstructionType, ConstructionMakeup, FloorConstructionType, Storey,
};
use floor::FloorRegions;
use types::{MultiPolygon3D, Polygon3D};
use walls::WallRegions;

pub use datatypes::ber::common::ConstructionPhase;

use uuid::Uuid;

pub struct FloorPlan {
    pub storey: u32,
    pub floors: FloorRegions,
    pub walls: WallRegions,
    pub ceilings: CeilingRegions,
}

//TODO: ddecide where storey lives
impl FloorPlan {
    // costructors
    pub fn new_floor(
        vertices: Vec<Point2<f64>>,
        construction_type: FloorConstructionType,
        floor_plan_below: Option<&FloorPlan>,
    ) -> (Self, Storey) {
        let mut floor_regions;
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
                        assert!(heights
                            .iter()
                            .all(|&h| (h - heights[0]).abs() < std::f64::EPSILON));
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
                floor_regions = FloorRegions::new(vertices, Some(construction_type), None, None);
            }
        }

        (
            Self {
                storey,
                floors: floor_regions.clone(),
                walls: WallRegions { regions: vec![] },
                ceilings: CeilingRegions { regions: vec![] },
            },
            Storey {
                id: Uuid::new_v4(),
                storey_number: storey as i32,
                name: "Floor ".to_string() + &storey.to_string(),
                floors: floor_regions.into(),
                walls: vec![],
                roofs: vec![],
                windows: vec![],
                doors: vec![],
                rooms: vec![],
            },
        )
    }

    // ----------------------------------------------------------------------------------------------
    // --- Floor modification methods ---
    // ----------------------------------------------------------------------------------------------
    pub fn raise_lower_floor_split(
        &mut self,
        vertices: Vec<Point2<f64>>,
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
        vertices: Vec<Point2<f64>>,
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
        vertices: Vec<Point2<f64>>,
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
        vertices: Vec<Point2<f64>>,
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
        let vertices_2d: Vec<Point2<f64>> = exterior_polygon.into();
        let vertices_3d: Vec<Point3<f64>> = vertices_2d
            .into_iter()
            .map(|p| Point3::new(p.x, p.y, height))
            .collect();
        self.ceilings = CeilingRegions::new(vertices_3d, None, None, None);
    }

    pub fn raise_lower_ceiling_split(
        &mut self,
        vertices: Vec<Point2<f64>>,
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
        point: Point2<f64>,
        new_height: f64,
        from_highest_region: bool,
    ) {
        let regions_containing_point: Vec<&CeilingRegion> = self
            .ceilings
            .regions
            .iter()
            .filter_map(|region| {
                if region.polygon.contains_point(point.x, point.y) {
                    Some(region)
                } else {
                    None
                }
            })
            .collect();

        let floor_height_vec = self.floors.get_floor_height_at_point(&point);
        let max = floor_height_vec
            .iter()
            .copied()
            .max_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap();
        let min = floor_height_vec
            .iter()
            .copied()
            .min_by(|a, b| a.partial_cmp(b).unwrap())
            .unwrap();
        let floor_height_at_point = if from_highest_region { max } else { min };
        // in the regions containing the point, convert each one to the mesh, insert the vertex,
        // and then convert back to mutiPolygon3D and then multiple regions
        let ceiling_regions_new: Vec<CeilingRegion> = regions_containing_point
            .iter()
            .map(|region| {
                let mut ceiling_mesh: CeilingMesh = (*region).clone().try_into().unwrap();
                ceiling_mesh.add_new_vertex_with_height(
                    point.x,
                    point.y,
                    new_height + floor_height_at_point,
                );
                ceiling_mesh.into()
            })
            .filter(|conversion: &CeilingRegionConversion| conversion.failed_components == 0)
            .flat_map(|conversion| conversion.regions)
            .collect();
    }

    pub fn change_ceiling_construction_makeup_split(
        &mut self,
        vertices: Vec<Point2<f64>>,
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
        vertices: Vec<Point2<f64>>,
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
        vertices: Vec<Point2<f64>>,
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
    pub fn change_wall_construction_makeup_split(
        &mut self,
        wall_id: Uuid,
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
        wall_id: Uuid,
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
        wall_id: Uuid,
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

    pub fn combine_polygons(&mut self, polygon_ids: Vec<Uuid>) {
        self.floors.merge_regions(&polygon_ids);
    }
}

impl From<Storey> for FloorPlan {
    fn from(storey: Storey) -> Self {
        Self {
            storey: storey.storey_number as u32,
            floors: FloorRegions::from(storey.floors),
            walls: WallRegions::from(storey.walls),
            ceilings: CeilingRegions::from(storey.roofs),
        }
    }
}

pub struct FloorPlanChangeItem {
    pub element: Box<dyn AddRemoveTrait>,
    pub location_uuid: Option<Uuid>,
}
pub struct FloorPlanChangeDyn {
    pub remove: Vec<FloorPlanChangeItem>,
    pub add: Vec<FloorPlanChangeItem>,
    pub modify: Vec<FloorPlanChangeItem>,
}
