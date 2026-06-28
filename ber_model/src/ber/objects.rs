//! Phase-3 (Objects) survey model: the fixed building-services objects an
//! assessor records walking the house — fans, lights, radiators, boilers,
//! cylinders, chimneys, etc. Each object has a CATEGORY (the top-level thing,
//! e.g. "Fan") and, for most, a SUBTYPE (e.g. extract / supply / decentralized);
//! it's captured with a count and photos, grouped by the approximate room
//! location (a pin on the storey plan) it was seen at.
//!
//! Pure data. The DEAP house-level systems (heating/ventilation/water/lighting)
//! are entered/derived elsewhere — this is the physical inventory that informs
//! them. The catalog is two enums (categories + subtype-carrying kinds) so the
//! picker can show categories first, then the subtype.

use serde::{Deserialize, Serialize};

use crate::ber::common::{ItemId, Photo};
use domain::points::Point2f32;

/// Top-level object category — what the assessor picks first. Most have a
/// SUBTYPE chosen next (see [`ObjectCategory::subtypes`]); a few are singletons
/// (the category IS the object, no sub-choice).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectCategory {
    Vent,
    Fan,
    Light,
    Radiator,
    Boiler,
    WaterCylinder,
    Thermostat,
    Chimney,
    AtticHatch,
    WeatherCompensation,
    WaterFixture,
    Other,
}

impl ObjectCategory {
    /// The categories in picker order.
    pub const ALL: [ObjectCategory; 12] = [
        ObjectCategory::Vent,
        ObjectCategory::Fan,
        ObjectCategory::Light,
        ObjectCategory::Radiator,
        ObjectCategory::Boiler,
        ObjectCategory::WaterCylinder,
        ObjectCategory::Thermostat,
        ObjectCategory::Chimney,
        ObjectCategory::AtticHatch,
        ObjectCategory::WeatherCompensation,
        ObjectCategory::WaterFixture,
        ObjectCategory::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ObjectCategory::Vent => "Vent",
            ObjectCategory::Fan => "Fan",
            ObjectCategory::Light => "Light",
            ObjectCategory::Radiator => "Radiator",
            ObjectCategory::Boiler => "Boiler",
            ObjectCategory::WaterCylinder => "Water cylinder",
            ObjectCategory::Thermostat => "Thermostat",
            ObjectCategory::Chimney => "Chimney",
            ObjectCategory::AtticHatch => "Attic hatch",
            ObjectCategory::WeatherCompensation => "Weather compensation",
            ObjectCategory::WaterFixture => "Shower / bath",
            ObjectCategory::Other => "Other",
        }
    }

    /// The subtype options shown when this category is chosen. Empty for a
    /// SINGLETON category (use [`ObjectCategory::singleton`] instead): the
    /// category maps to one kind with no sub-choice.
    pub fn subtypes(self) -> Vec<ObjectKind> {
        match self {
            ObjectCategory::Fan => vec![
                ObjectKind::Fan(FanType::Extract),
                ObjectKind::Fan(FanType::Supply),
                ObjectKind::Fan(FanType::Decentralized),
            ],
            ObjectCategory::Light => vec![
                ObjectKind::Light(LightKind::Led),
                ObjectKind::Light(LightKind::Halogen),
                ObjectKind::Light(LightKind::Incandescent),
            ],
            ObjectCategory::Radiator => vec![
                ObjectKind::Radiator(RadiatorTrv::WithTrv),
                ObjectKind::Radiator(RadiatorTrv::WithoutTrv),
            ],
            ObjectCategory::Boiler => vec![
                ObjectKind::Boiler(BoilerFuel::Oil),
                ObjectKind::Boiler(BoilerFuel::Gas),
                ObjectKind::Boiler(BoilerFuel::HeatPump),
            ],
            ObjectCategory::WaterCylinder => vec![
                ObjectKind::WaterCylinder(CylinderKind::Standard),
                ObjectKind::WaterCylinder(CylinderKind::InternalHeatPump),
            ],
            ObjectCategory::Chimney => vec![
                ObjectKind::Chimney(ChimneyState::Open),
                ObjectKind::Chimney(ChimneyState::Closed),
                ObjectKind::Chimney(ChimneyState::Blocked),
            ],
            ObjectCategory::AtticHatch => vec![
                ObjectKind::AtticHatch(AtticHatchSeal::DraughtStripped),
                ObjectKind::AtticHatch(AtticHatchSeal::NotDraughtStripped),
            ],
            ObjectCategory::WaterFixture => vec![
                ObjectKind::WaterFixture(WaterFixtureKind::Shower),
                ObjectKind::WaterFixture(WaterFixtureKind::Bath),
                ObjectKind::WaterFixture(WaterFixtureKind::ElectricShower),
            ],
            // Singletons (no sub-choice).
            ObjectCategory::Vent
            | ObjectCategory::Thermostat
            | ObjectCategory::WeatherCompensation
            | ObjectCategory::Other => Vec::new(),
        }
    }

    /// The single kind a SINGLETON category maps to (no subtype). `None` for
    /// categories that require a subtype choice. `Other` starts with empty text.
    pub fn singleton(self) -> Option<ObjectKind> {
        match self {
            ObjectCategory::Vent => Some(ObjectKind::Vent),
            ObjectCategory::Thermostat => Some(ObjectKind::Thermostat),
            ObjectCategory::WeatherCompensation => Some(ObjectKind::WeatherCompensation),
            ObjectCategory::Other => Some(ObjectKind::Other(String::new())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FanType {
    Extract,
    Supply,
    Decentralized,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LightKind {
    Led,
    Halogen,
    Incandescent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RadiatorTrv {
    WithTrv,
    WithoutTrv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BoilerFuel {
    Oil,
    Gas,
    HeatPump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CylinderKind {
    Standard,
    InternalHeatPump,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChimneyState {
    Open,
    Closed,
    Blocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AtticHatchSeal {
    DraughtStripped,
    NotDraughtStripped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WaterFixtureKind {
    Shower,
    Bath,
    ElectricShower,
}

/// A concrete recorded object kind: a category plus its subtype (where one
/// applies). The single source of an object's identity + label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    Vent,
    Fan(FanType),
    Light(LightKind),
    Radiator(RadiatorTrv),
    Boiler(BoilerFuel),
    WaterCylinder(CylinderKind),
    Thermostat,
    Chimney(ChimneyState),
    AtticHatch(AtticHatchSeal),
    WeatherCompensation,
    WaterFixture(WaterFixtureKind),
    Other(String),
}

impl ObjectKind {
    pub fn category(&self) -> ObjectCategory {
        match self {
            ObjectKind::Vent => ObjectCategory::Vent,
            ObjectKind::Fan(_) => ObjectCategory::Fan,
            ObjectKind::Light(_) => ObjectCategory::Light,
            ObjectKind::Radiator(_) => ObjectCategory::Radiator,
            ObjectKind::Boiler(_) => ObjectCategory::Boiler,
            ObjectKind::WaterCylinder(_) => ObjectCategory::WaterCylinder,
            ObjectKind::Thermostat => ObjectCategory::Thermostat,
            ObjectKind::Chimney(_) => ObjectCategory::Chimney,
            ObjectKind::AtticHatch(_) => ObjectCategory::AtticHatch,
            ObjectKind::WeatherCompensation => ObjectCategory::WeatherCompensation,
            ObjectKind::WaterFixture(_) => ObjectCategory::WaterFixture,
            ObjectKind::Other(_) => ObjectCategory::Other,
        }
    }

    /// The subtype's own short label (e.g. "Extract", "With TRV"). Empty for
    /// kinds with no subtype. Used to label the subtype buttons + the review.
    pub fn subtype_label(&self) -> &str {
        match self {
            ObjectKind::Fan(FanType::Extract) => "Extract",
            ObjectKind::Fan(FanType::Supply) => "Supply",
            ObjectKind::Fan(FanType::Decentralized) => "Decentralized",
            ObjectKind::Light(LightKind::Led) => "LED",
            ObjectKind::Light(LightKind::Halogen) => "Halogen",
            ObjectKind::Light(LightKind::Incandescent) => "Incandescent",
            ObjectKind::Radiator(RadiatorTrv::WithTrv) => "With TRV",
            ObjectKind::Radiator(RadiatorTrv::WithoutTrv) => "Without TRV",
            ObjectKind::Boiler(BoilerFuel::Oil) => "Oil",
            ObjectKind::Boiler(BoilerFuel::Gas) => "Gas",
            ObjectKind::Boiler(BoilerFuel::HeatPump) => "Heat pump",
            ObjectKind::WaterCylinder(CylinderKind::Standard) => "Standard",
            ObjectKind::WaterCylinder(CylinderKind::InternalHeatPump) => "Internal heat pump",
            ObjectKind::Chimney(ChimneyState::Open) => "Open",
            ObjectKind::Chimney(ChimneyState::Closed) => "Closed",
            ObjectKind::Chimney(ChimneyState::Blocked) => "Blocked",
            ObjectKind::AtticHatch(AtticHatchSeal::DraughtStripped) => "Draught-stripped",
            ObjectKind::AtticHatch(AtticHatchSeal::NotDraughtStripped) => "Not draught-stripped",
            ObjectKind::WaterFixture(WaterFixtureKind::Shower) => "Shower",
            ObjectKind::WaterFixture(WaterFixtureKind::Bath) => "Bath",
            ObjectKind::WaterFixture(WaterFixtureKind::ElectricShower) => "Electric shower",
            ObjectKind::Other(s) => s,
            _ => "",
        }
    }

    /// Full display label: category, plus the subtype in parentheses when there
    /// is one (e.g. "Fan (Extract)", "Boiler (Gas)", "Vent").
    pub fn label(&self) -> String {
        let cat = self.category().label();
        match self {
            ObjectKind::Vent | ObjectKind::Thermostat | ObjectKind::WeatherCompensation => {
                cat.to_string()
            }
            ObjectKind::Other(s) if !s.trim().is_empty() => s.clone(),
            ObjectKind::Other(_) => "Other".to_string(),
            _ => format!("{cat} ({})", self.subtype_label()),
        }
    }
}

/// One recorded fixed object: its kind, how many at this spot, an optional note,
/// and its photos.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuildingObject {
    pub id: ItemId,
    pub kind: ObjectKind,
    /// How many of this object at this location (e.g. 5 radiators, 8 downlights).
    pub count: u32,
    pub note: Option<String>,
    pub photos: Vec<Photo>,
}

impl BuildingObject {
    pub fn new(kind: ObjectKind, count: u32) -> Self {
        Self {
            id: ItemId::default(),
            kind,
            count: count.max(1),
            note: None,
            photos: Vec::new(),
        }
    }
}

/// A room (or location) the assessor stopped at: an approximate pin on the
/// storey plan plus the fixed objects recorded there. `name` is optional;
/// `plan_position` is world-metre plan coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectRoom {
    pub id: ItemId,
    pub name: String,
    pub plan_position: Point2f32,
    pub objects: Vec<BuildingObject>,
}

impl ObjectRoom {
    pub fn new(plan_position: Point2f32) -> Self {
        Self {
            id: ItemId::default(),
            name: String::new(),
            plan_position,
            objects: Vec::new(),
        }
    }

    /// Total objects recorded here (summing each entry's count).
    pub fn total_count(&self) -> u32 {
        self.objects.iter().map(|o| o.count).sum()
    }

    /// The distinct categories present at this pin — used to HIGHLIGHT the
    /// categories already recorded here in the picker.
    pub fn categories_present(&self) -> Vec<ObjectCategory> {
        let mut cats: Vec<ObjectCategory> = Vec::new();
        for o in &self.objects {
            let c = o.kind.category();
            if !cats.contains(&c) {
                cats.push(c);
            }
        }
        cats
    }
}
