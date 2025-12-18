// src/data/systems.rs

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::Photo;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeatingSystemType {
    GasBoiler,
    OilBoiler,
    HeatPump,
    ElectricHeater,
    Other(String),
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatingControls {
    pub id: Uuid,
    pub programmer: bool,
    pub zones: u32,
    pub notes: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatingSystem {
    pub id: Uuid,
    pub system_type: HeatingSystemType, // e.g., "Gas boiler", "Heat pump"
    pub description: String,
    pub year_installed: Option<u32>,
    pub controls: HeatingControls,
    pub separate_hot_water: bool,
}

impl Default for HeatingSystem {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            system_type: HeatingSystemType::Invalid,
            description: String::new(),
            year_installed: None,
            controls: HeatingControls {
                id: Uuid::new_v4(),
                programmer: false,
                zones: 0,
                notes: None,
            },
            separate_hot_water: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VentilationSystemType {
    Natural,
    Mechanical,
    MechanicalWithHeatRecovery,
    Other(String),
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VentilationSystem {
    pub id: Uuid,
    pub system_type: VentilationSystemType, // e.g., "Natural ventilation", "Mechanical ventilation with heat recovery"
    pub description: Option<String>,
}

impl Default for VentilationSystem {
    fn default() -> Self {
        Self {
            id: Uuid::new_v4(),
            system_type: VentilationSystemType::Invalid,
            description: None,
        }
    }
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
    pub id: Uuid,
    pub num_panels: u32,
    pub roof_element_id: Uuid,
    pub estimated_angle_degs: f32,
    pub estimated_direction: Direction,
    pub photo: Option<Photo>,
    pub description: Option<String>,
}
