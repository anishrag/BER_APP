// src/data/systems.rs

use serde::{Deserialize, Serialize};

use super::common::{Evidence, ItemId, Year};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum HeatingSystemType {
    GasBoiler,
    OilBoiler,
    HeatPump,
    ElectricHeater,
    Other(String),
    #[default]
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeatingControls {
    pub programmer: bool,
    pub zones: u32,
    /// Count of room thermostats recorded (summed from `Thermostat` objects).
    #[serde(default)]
    pub room_thermostat: u32,
    pub notes: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HeatingSystem {
    pub id: ItemId,
    pub system_type: HeatingSystemType, // e.g., "Gas boiler", "Heat pump"
    pub description: String,
    pub year_installed: Option<Year>,
    pub controls: HeatingControls,
    pub separate_hot_water: bool,
    /// The heat-EMITTER (distribution) type — radiators / underfloor / etc.
    /// Migrated from the pre-refactor `FinalDetails::heating` (see
    /// `Ber::migrate_legacy`). `#[serde(default)]` so older records load.
    #[serde(default)]
    pub distribution: crate::ber::final_details::HeatingKind,
    /// Non-primary heat sources present in the dwelling (for the report) — the
    /// distinct heat-source object types other than the primary `system_type`,
    /// plus `Other("Chimney")` for any chimney. Synced from placed objects; see
    /// [`Ber::sync_heating_system_from_objects`](crate::ber::ber_struct::Ber::sync_heating_system_from_objects).
    #[serde(default)]
    pub secondary_heating: Vec<HeatingSystemType>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum VentilationSystemType {
    Natural,
    Mechanical,
    MechanicalWithHeatRecovery,
    Other(String),
    #[default]
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VentilationSystem {
    pub id: ItemId,
    pub system_type: VentilationSystemType, // e.g., "Natural ventilation", "Mechanical ventilation with heat recovery"
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Direction {
    North,
    East,
    South,
    West,
    Northeast,
    Northwest,
    Southeast,
    Southwest,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SolarElements {
    pub id: ItemId,
    pub num_panels: u32,
    pub ceiling_element_id: ItemId,
    pub estimated_angle_degs: f32,
    pub estimated_direction: Direction,
    pub photo: Option<Evidence>,
    pub description: Option<String>,
}
