pub mod ceiling;
pub mod geometry;
pub mod types;
pub mod walls;

mod floor;
use nalgebra::Point2;

use ceiling::CeilingRegions;
use datatypes::ber::diffable::AddRemoveTrait;
use datatypes::ber::structural::{FloorConstructionType, Storey};
use floor::FloorRegions;
use walls::WallRegions;

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
}
impl From<Storey> for FloorPlan {
    fn from(storey: Storey) -> Self {
        Self {
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
