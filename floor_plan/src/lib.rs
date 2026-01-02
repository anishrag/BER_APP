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
    pub floors: FloorRegions,
    pub walls: WallRegions,
    pub ceilings: CeilingRegions,
}

//TODO: ddecide where storey lives
impl FloorPlan {
    pub fn new_ground_floor(
        vertices: Vec<Point2<f64>>,
        construction_type: FloorConstructionType,
    ) -> (Self, Storey) {
        let floor_regions = FloorRegions::new(vertices, Some(construction_type), None, None);
        (
            Self {
                floors: floor_regions.clone(),
                walls: WallRegions { regions: vec![] },
                ceilings: CeilingRegions { regions: vec![] },
            },
            Storey {
                id: Uuid::new_v4(),
                name: "Ground Floor".to_string(),
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
