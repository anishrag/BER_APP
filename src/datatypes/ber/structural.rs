// src/data/structural.rs
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{Item, Photo};
use super::fenestration::{Door, Window};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opes {
    pub doors: Vec<Uuid>,   // references to Door IDs
    pub windows: Vec<Uuid>, // references to Window IDs
}

impl Default for Opes {
    fn default() -> Self {
        Self {
            doors: vec![],
            windows: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstructionMaterial {
    Concrete,
    InsulationEPS,
    Plasterboard,
    Timber,
    Custom {
        lambda: f32, // W/m·K
        name: String,
    },
}

impl ConstructionMaterial {
    pub fn lambda(&self) -> f32 {
        match self {
            ConstructionMaterial::Concrete => 1.7,
            ConstructionMaterial::InsulationEPS => 0.035,
            ConstructionMaterial::Plasterboard => 0.25,
            ConstructionMaterial::Timber => 0.13,
            ConstructionMaterial::Custom { lambda, .. } => *lambda,
        }
    }

    pub fn name(&self) -> String {
        match self {
            ConstructionMaterial::Concrete => "Concrete".into(),
            ConstructionMaterial::InsulationEPS => "EPS Insulation".into(),
            ConstructionMaterial::Plasterboard => "Plasterboard".into(),
            ConstructionMaterial::Timber => "Timber".into(),
            ConstructionMaterial::Custom { name, .. } => name.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionLayer {
    pub thickness_mm: u32,              // thickness in meters
    pub material: ConstructionMaterial, // material name or ID
    pub bridged: bool,                  // true if thermal bridging exists
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstructionMakeup {
    NonHeatLoss,                    // no heat loss
    Default,                        // use default construction
    Tbd,                            // to be determined
    Layers(Vec<ConstructionLayer>), // custom stack of layers
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloorData {
    pub storey: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanarElement {
    Floor {
        id: Uuid,
        vertices: Vec<(f32, f32, f32)>,
        construction_phase_id: Uuid,
        opes: Opes,
        construction: ConstructionMakeup,
        photos: Vec<Photo>,
        description: Option<String>,

        floor: FloorData,
    },

    Wall {
        id: Uuid,
        vertices: Vec<(f32, f32, f32)>,
        construction_phase_id: Uuid,
        opes: Opes,
        construction: ConstructionMakeup,
        photos: Vec<Photo>,
        description: Option<String>,
    },

    Roof {
        id: Uuid,
        vertices: Vec<(f32, f32, f32)>,
        construction_phase_id: Uuid,
        opes: Opes,
        construction: ConstructionMakeup,
        photos: Vec<Photo>,
        description: Option<String>,
    },
}

impl PlanarElement {
    pub fn new_floor(
        vertices: Vec<(f32, f32, f32)>,
        construction_phase_id: Uuid,
        construction: ConstructionMakeup,
    ) -> Self {
        Self::Floor {
            id: Uuid::new_v4(),
            vertices,
            construction_phase_id,
            opes: Opes::default(),
            construction,
            photos: vec![],
            description: None,
            floor: FloorData { storey: 0 },
        }
    }

    pub fn new_wall(
        vertices: Vec<(f32, f32, f32)>,
        construction_phase_id: Uuid,
        construction: ConstructionMakeup,
    ) -> Self {
        Self::Wall {
            id: Uuid::new_v4(),
            vertices,
            construction_phase_id,
            opes: Opes::default(),
            construction,
            photos: vec![],
            description: None,
        }
    }

    pub fn new_roof(
        vertices: Vec<(f32, f32, f32)>,
        construction_phase_id: Uuid,
        construction: ConstructionMakeup,
    ) -> Self {
        Self::Roof {
            id: Uuid::new_v4(),
            vertices,
            construction_phase_id,
            opes: Opes::default(),
            construction,
            photos: vec![],
            description: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: Uuid,
    pub name: String,
    pub vertices: Vec<(f32, f32, f32)>,
    pub thermostats: Vec<Item>,
    pub vents: Vec<Item>,
    pub fans: Vec<Item>,
    pub lights: Vec<Item>,
    pub chimneys: Vec<Item>,
    pub trvs: Vec<Item>,
    pub living_room_candidate: bool,
    pub underfloor_heating: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Storey {
    pub id: Uuid,
    pub name: String, // e.g., "Ground Floor", "First Floor"
    pub floors: Vec<PlanarElement>,
    pub walls: Vec<PlanarElement>,
    pub roofs: Vec<PlanarElement>,
    pub windows: Vec<Window>,
    pub doors: Vec<Door>,
    pub rooms: Vec<Room>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelteredSideInfo {
    pub id: Uuid,
    pub sheltered_sides: i32, // e.g., ["North", "West"]
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThermalMassCategoryEnum {
    Low,
    Medium,
    High,
    Invalid,
    Other(String),
}
impl Default for ThermalMassCategoryEnum {
    fn default() -> Self {
        ThermalMassCategoryEnum::Invalid
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalMassInfo {
    pub id: Uuid,
    pub floors: ThermalMassCategoryEnum,
    pub partition_walls: ThermalMassCategoryEnum,
    pub external_walls: ThermalMassCategoryEnum,
    pub separating_walls: ThermalMassCategoryEnum,
}

impl Default for ThermalMassInfo {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),         // generate a new UUID
            floors: Default::default(), // ThermalMassCategoryEnum::Invalid
            partition_walls: Default::default(),
            external_walls: Default::default(),
            separating_walls: Default::default(),
        }
    }
}
