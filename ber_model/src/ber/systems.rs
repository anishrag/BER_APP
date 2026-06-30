// src/data/systems.rs

use serde::{Deserialize, Serialize};

use super::common::{ItemId, Photo, Year};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
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
    pub photo: Option<Photo>,
    pub description: Option<String>,
}
