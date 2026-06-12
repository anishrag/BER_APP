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

impl ConstructionPhase {
    /// The phase's built-in display name ("Original", "Extension 1", …) —
    /// what the UI shows when no nickname has been given.
    pub fn default_name(&self) -> String {
        match self {
            ConstructionPhase::Original => "Original".to_string(),
            ConstructionPhase::Extension1 => "Extension 1".to_string(),
            ConstructionPhase::Extension2 => "Extension 2".to_string(),
            ConstructionPhase::Extension3 => "Extension 3".to_string(),
            ConstructionPhase::Extension4 => "Extension 4".to_string(),
            ConstructionPhase::Other(s) => s.clone(),
        }
    }
}

/// BER-level construction dates mapping
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ConstructionPhases {
    pub years: HashMap<ConstructionPhase, Year>,
    /// User-given nicknames per phase (e.g. "Back extension", "over garage") —
    /// what the UI shows instead of "Extension 1". Optional per phase.
    #[serde(default)]
    pub nicknames: HashMap<ConstructionPhase, String>,
}

impl ConstructionPhases {
    pub fn new(years: HashMap<ConstructionPhase, Year>) -> Self {
        Self {
            years,
            nicknames: HashMap::new(),
        }
    }

    /// Set (or clear, if empty) the nickname for `phase`.
    pub fn set_nickname(&mut self, phase: ConstructionPhase, nickname: String) {
        if nickname.trim().is_empty() {
            self.nicknames.remove(&phase);
        } else {
            self.nicknames.insert(phase, nickname);
        }
    }

    /// The nickname for `phase`, if one was given.
    pub fn nickname_of(&self, phase: &ConstructionPhase) -> Option<&str> {
        self.nicknames.get(phase).map(|s| s.as_str())
    }

    pub fn get_year(&self, phase: ConstructionPhase) -> Year {
        self.years
            .get(&phase)
            .copied()
            .expect("Phase not found in construction years")
    }

    /// Non-panicking lookup of a phase's year, if one has been set.
    pub fn year_of(&self, phase: &ConstructionPhase) -> Option<Year> {
        self.years.get(phase).copied()
    }

    /// Set (or replace) the year for `phase`.
    pub fn set_year(&mut self, phase: ConstructionPhase, year: Year) {
        self.years.insert(phase, year);
    }

    /// A human label for `phase`: the user's nickname if given (e.g. "Back
    /// extension"), else the default name ("Original", "Extension 1"), with
    /// the year appended when known — e.g. "Back extension (2008)",
    /// "Original (1985)".
    pub fn display_label(&self, phase: &ConstructionPhase) -> String {
        let name = self
            .nickname_of(phase)
            .map(|s| s.to_string())
            .unwrap_or_else(|| phase.default_name());
        match self.year_of(phase) {
            Some(year) => format!("{name} ({year})"),
            None => name,
        }
    }

    /// The phases that have a year set, in canonical order (Original, then
    /// Extension1..4, then any `Other`). This is the list the assessor may
    /// assign to an area — only construction dates entered up front are
    /// selectable.
    pub fn defined_phases(&self) -> Vec<ConstructionPhase> {
        let mut out: Vec<ConstructionPhase> = [
            ConstructionPhase::Original,
            ConstructionPhase::Extension1,
            ConstructionPhase::Extension2,
            ConstructionPhase::Extension3,
            ConstructionPhase::Extension4,
        ]
        .into_iter()
        .filter(|p| self.years.contains_key(p))
        .collect();
        for k in self.years.keys() {
            if matches!(k, ConstructionPhase::Other(_)) {
                out.push(k.clone());
            }
        }
        out
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
