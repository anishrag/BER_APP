pub mod ceiling;
pub mod geometry;
pub mod types;
pub mod walls;

mod floor;

use ceiling::CeilingRegions;
use datatypes::ber::diffable::AddRemoveTrait;
use floor::FloorRegions;
use walls::WallRegions;

use uuid::Uuid;

pub struct FloorPlan {
    pub floors: FloorRegions,
    pub walls: WallRegions,
    pub ceilings: CeilingRegions,
}

pub struct FloorPlanChangeItem {
    pub element: Box<dyn AddRemoveTrait>,
    pub location_uuid: Option<Uuid>,
}
pub struct FloorPlanChangeDyn {
    pub remove: Vec<Box<dyn AddRemoveTrait>>,
    pub add: Vec<Box<dyn AddRemoveTrait>>,
    pub modify: Vec<Box<dyn AddRemoveTrait>>,
}
