use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------- SECTION 2: Ventilation -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum StructureType {
    Masonry,
    TimberSteelFrame,
    ICF,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VentillationItemsCounts {
    pub attic_hatches_draught_stripped: u16,
    pub attic_hatches_not_draught_stripped: u16,
    pub chimneys: u32,
    pub flues: u32,
    pub fans: u32,
    pub passive_vents: u32,
    pub flueless_combustion_room_heaters: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ventilation {
    pub method: VentilationType,
    pub air_permeability: Option<f32>, // m³/h.m² at 50Pa
    pub suspended_ground_floor: bool,
    pub structure_type: StructureType,
    pub ventillation_items_counts: VentillationItemsCounts,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VentilationType {
    Natural,
    PositiveInputFromLoft,
    PositiveInputFromOutside {
        sfp: f32,
    },
    WholeHouseExtract {
        sfp: f32,
    },
    ExhaustAirHeatPump {
        cop: f32,
    },
    MVHR {
        sfp: f32,
        efficiency: f32,
        external_uninsulated_ducting: bool,
    },
    MVNHR {
        sfp: f32,
    },
}
