use uuid::Uuid;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Photo {
    pub id: Uuid,
    pub file_path: String, // local path to image
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConstructionPhase {
    pub id: Uuid,
    pub phase_name: String, // e.g., "Original", "Extension 1"
    pub year_built: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Radiator {
    pub with_trv: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ChimneyFuel {
    Gas,
    SolidFuel,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chimney {
    pub open: bool,
    pub fuel_type: ChimneyFuel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TypeLight {
    Incandescent,
    Halogen,
    HalogenLV,
    LinearFlourescent,
    LEDCFL,
    Other(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Light {
    pub count: u32,
    pub light_type: TypeLight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thermostat {
    pub delayed_start: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Vent {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FanType {
    Kitchen,
    Bathroom,
    Other(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fan {
    pub fan_type: FanType,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ItemType {
    Thermostat(Thermostat),
    Vent(Vent),
    Fan(Fan),
    Radiator(Radiator),
    Chimney(Chimney),
    Light(Light),
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub item_type: ItemType,
    pub photos: Vec<Photo>,
    pub description: Option<String>,
}
