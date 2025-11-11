// src/data/systems.rs

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub enum HeatingSystemType {
    GasBoiler,
    OilBoiler,
    HeatPump,
    ElectricHeater,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HeatingControls {
    pub id: Uuid,
    pub programmer: bool,
    pub zones: u32,
    pub notes: Option<String>,
}
#[derive(Debug, Serialize, Deserialize)]
pub struct HeatingSystem {
    pub id: Uuid,
    pub system_type: HeatingSystemType, // e.g., "Gas boiler", "Heat pump"
    pub year_installed: i32,
    pub controls: HeatingControls,
    pub separate_hot_water: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum VentilationSystemType {
    Natural,
    Mechanical,
    MechanicalWithHeatRecovery,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct VentilationSystem {
    pub id: Uuid,
    pub system_type: VentilationSystemType, // e.g., "Natural ventilation", "Mechanical ventilation with heat recovery"
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
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

#[derive(Debug, Serialize, Deserialize)]
pub struct SolarElements {
    pub id: Uuid,
    pub num_panels: u32,
    pub roof_element_id: Uuid,
    pub estimated_angle_degs: f32,
    pub estimated_direction: Direction,
    pub photo: Option<Photo>,
    pub description: Option<String>,
}
