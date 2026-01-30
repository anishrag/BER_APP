use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ops::Deref;

use uuid::Uuid;

pub type Year = u32;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct ItemId(pub Uuid);

impl Default for ItemId {
    fn default() -> Self {
        ItemId(Uuid::new_v4())
    }
}

impl Deref for ItemId {
    type Target = Uuid;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

use std::str::FromStr;

impl FromStr for ItemId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s).map(ItemId)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Photo {
    pub id: ItemId,
    pub file_path: String, // local path to image
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
pub enum ConstructionPhase {
    #[default]
    Original,
    Extension1,
    Extension2,
    Extension3,
    Extension4,
    Other(String),
}

/// BER-level construction dates mapping
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConstructionPhases {
    pub years: HashMap<ConstructionPhase, Year>,
}

impl ConstructionPhases {
    pub fn new(years: HashMap<ConstructionPhase, Year>) -> Self {
        Self { years }
    }

    pub fn get_year(&self, phase: ConstructionPhase) -> Year {
        self.years
            .get(&phase)
            .copied()
            .expect("Phase not found in construction years")
    }
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
    pub id: ItemId,
    pub name: String,
    pub item_type: ItemType,
    pub photos: Vec<Photo>,
    pub description: Option<String>,
}
