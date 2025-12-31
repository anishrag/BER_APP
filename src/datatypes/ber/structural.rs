// src/data/structural.rs
use nalgebra::Point3;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{ConstructionPhase, Item, Photo};
use super::fenestration::{Door, Window};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Opes {
    pub doors: Vec<Uuid>,   // references to Door IDs
    pub windows: Vec<Uuid>, // references to Window IDs
}

#[cfg_attr(test, derive(PartialEq))]
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

#[cfg_attr(test, derive(PartialEq))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionLayer {
    pub thickness_mm: u32,              // thickness in meters
    pub material: ConstructionMaterial, // material name or ID
    pub bridged: bool,                  // true if thermal bridging exists
}
#[cfg_attr(test, derive(PartialEq))]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ConstructionMakeup {
    NonHeatLoss,                    // no heat loss
    Default,                        // use default construction
    Tbd,                            // to be determined
    Layers(Vec<ConstructionLayer>), // custom stack of layers
}

impl Default for ConstructionMakeup {
    fn default() -> Self {
        ConstructionMakeup::Tbd
    }
}
/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FloorConstructionType {
    SolidFloor,
    SuspendedFloor,
    NonHeatLossFloor,
    Uninitialised,
    Other,
}

/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CeilingConstructionType {
    FlatRoof,
    SlopedInsOnRafter,
    SlopedInsOnCeiling,
    SlopedInsOnCeilingAndRafter,
    NonHeatLossCeiling,
    Uninitialised,
    Other,
}

/// Wall construction types
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WallConstructionType {
    CavityWall,
    FilledCavityWall,
    ConcreteHollowBlocks,
    TimberConstruction,
    ICF,
    Floor,
    Uninitialised,
}

/// Thermal exposure classification
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WallExposure {
    Exposed,     // Exterior wall exposed to outside
    SemiExposed, // Wall to unheated space
    NonHeatLoss, // Internal wall, no heat loss
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloorData {
    pub construction_type: FloorConstructionType,
    pub storey: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WallData {
    pub construction_type: WallConstructionType,
    pub exposure: WallExposure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoofData {
    pub construction_type: CeilingConstructionType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanarCommon {
    pub id: Uuid,
    pub vertices: Vec<Point3<f64>>,
    pub construction_phase: ConstructionPhase,
    pub opes: Opes,
    pub construction: ConstructionMakeup,
    pub photos: Vec<Photo>,
    pub description: Option<String>,
}

impl PlanarCommon {
    fn new(
        vertices: Vec<Point3<f64>>,
        construction_phase: ConstructionPhase,
        construction: ConstructionMakeup,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            vertices,
            construction_phase,
            opes: Opes::default(),
            construction,
            photos: Vec::new(),
            description: None,
        }
    }
}

// --- Planar elements enum ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlanarElement {
    Floor {
        common: PlanarCommon,
        floor_specifics: FloorData,
    },

    Wall {
        common: PlanarCommon,
        wall_specifics: WallData,
    },

    Roof {
        common: PlanarCommon,
        roof_specifics: RoofData,
    },
}

// --- Constructors ---

impl PlanarElement {
    pub fn new_floor(
        vertices: Vec<Point3<f64>>,
        construction_phase: ConstructionPhase,
        construction: ConstructionMakeup,
    ) -> Self {
        Self::Floor {
            common: PlanarCommon::new(vertices, construction_phase, construction),
            floor_specifics: FloorData {
                storey: 0,
                construction_type: FloorConstructionType::Uninitialised,
            },
        }
    }

    pub fn new_wall(
        vertices: Vec<Point3<f64>>,
        construction_phase: ConstructionPhase,
        construction: ConstructionMakeup,
    ) -> Self {
        Self::Wall {
            common: PlanarCommon::new(vertices, construction_phase, construction),
            wall_specifics: WallData {
                construction_type: WallConstructionType::Uninitialised,
                exposure: WallExposure::Exposed,
            },
        }
    }

    pub fn new_roof(
        vertices: Vec<Point3<f64>>,
        construction_phase: ConstructionPhase,
        construction: ConstructionMakeup,
    ) -> Self {
        Self::Roof {
            common: PlanarCommon::new(vertices, construction_phase, construction),
            roof_specifics: RoofData {
                construction_type: CeilingConstructionType::Uninitialised,
            },
        }
    }
}

// --- Shared accessors ---

impl PlanarElement {
    pub fn common(&self) -> &PlanarCommon {
        match self {
            Self::Floor { common, .. } | Self::Wall { common, .. } | Self::Roof { common, .. } => {
                common
            }
        }
    }

    pub fn common_mut(&mut self) -> &mut PlanarCommon {
        match self {
            Self::Floor { common, .. } | Self::Wall { common, .. } | Self::Roof { common, .. } => {
                common
            }
        }
    }
}

// --- Example ergonomic helpers (optional but recommended) ---

impl PlanarElement {
    pub fn id(&self) -> Uuid {
        self.common().id
    }

    pub fn vertices(&self) -> &[Point3<f64>] {
        &self.common().vertices
    }

    pub fn add_photo(&mut self, photo: Photo) {
        self.common_mut().photos.push(photo);
    }

    pub fn set_description(&mut self, description: impl Into<String>) {
        self.common_mut().description = Some(description.into());
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: Uuid,
    pub name: String,
    pub vertices: Vec<Point3<f64>>,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
