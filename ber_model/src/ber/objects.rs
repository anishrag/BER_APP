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

use crate::ber::common::{Evidence, ItemId};
use domain::points::Point2f32;

/// Top-level object category — what the assessor picks first. Most have a
/// SUBTYPE chosen next (see [`ObjectCategory::subtypes`]); a few are singletons
/// (the category IS the object, no sub-choice).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectCategory {
    Vent,
    Fan,
    Light,
    Radiator,
    /// The heating plant + controls, grouped together: boiler (by fuel),
    /// programmer, heating-zone evidence, thermostat, weather compensation.
    /// (Thermostat + weather compensation used to be their own top-level
    /// categories; they're subtypes here now — see [`ObjectCategory::subtypes`].)
    HeatingSystem,
    WaterCylinder,
    Chimney,
    AtticHatch,
    WaterFixture,
    SolarPanel,
    /// Solar HOT WATER (thermal) — the tube/panel collector and its controls
    /// (pump station / controller), split so each is recorded with its own
    /// photo. Legacy undifferentiated `SolarThermal` records group here too.
    SolarWaterHeating,
    Other,
}

impl ObjectCategory {
    /// The categories in picker order.
    pub const ALL: [ObjectCategory; 12] = [
        ObjectCategory::Vent,
        ObjectCategory::Fan,
        ObjectCategory::Light,
        ObjectCategory::Radiator,
        ObjectCategory::HeatingSystem,
        ObjectCategory::WaterCylinder,
        ObjectCategory::Chimney,
        ObjectCategory::AtticHatch,
        ObjectCategory::WaterFixture,
        ObjectCategory::SolarPanel,
        ObjectCategory::SolarWaterHeating,
        ObjectCategory::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ObjectCategory::Vent => "Vent",
            ObjectCategory::Fan => "Fan",
            ObjectCategory::Light => "Light",
            ObjectCategory::Radiator => "Radiator",
            ObjectCategory::HeatingSystem => "Heating system",
            ObjectCategory::WaterCylinder => "Water cylinder",
            ObjectCategory::Chimney => "Chimney",
            ObjectCategory::AtticHatch => "Attic hatch",
            ObjectCategory::WaterFixture => "Shower / bath",
            ObjectCategory::SolarPanel => "Solar PV",
            ObjectCategory::SolarWaterHeating => "Solar hot water",
            ObjectCategory::Other => "Other",
        }
    }

    /// The subtype options shown when this category is chosen. Empty for a
    /// SINGLETON category (use [`ObjectCategory::singleton`] instead): the
    /// category maps to one kind with no sub-choice.
    pub fn subtypes(self) -> Vec<ObjectKind> {
        match self {
            ObjectCategory::Vent => vec![
                ObjectKind::Vent(VentKind::Open),
                ObjectKind::Vent(VentKind::Closable),
            ],
            ObjectCategory::Fan => vec![
                ObjectKind::Fan(FanType::Extract),
                ObjectKind::Fan(FanType::Supply),
                ObjectKind::Fan(FanType::Decentralized),
                ObjectKind::Fan(FanType::Central),
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
            ObjectCategory::HeatingSystem => vec![
                ObjectKind::Boiler(BoilerFuel::Oil),
                ObjectKind::Boiler(BoilerFuel::Gas),
                ObjectKind::Boiler(BoilerFuel::HeatPump),
                ObjectKind::PanelHeater,
                ObjectKind::Programmer,
                ObjectKind::HeatingZones,
                ObjectKind::Thermostat,
                ObjectKind::WeatherCompensation,
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
            ObjectCategory::SolarPanel => vec![
                ObjectKind::SolarPanel(SolarPanelKind::Photovoltaic),
                ObjectKind::SolarPanel(SolarPanelKind::Battery),
            ],
            // Legacy undifferentiated `SolarThermal` isn't offered any more —
            // the split kinds are; old records still display through it.
            ObjectCategory::SolarWaterHeating => vec![
                ObjectKind::SolarPanel(SolarPanelKind::SolarTubes),
                ObjectKind::SolarPanel(SolarPanelKind::SolarControls),
            ],
            // Singletons (no sub-choice).
            ObjectCategory::Other => Vec::new(),
        }
    }

    /// The single kind a SINGLETON category maps to (no subtype). `None` for
    /// categories that require a subtype choice. `Other` starts with empty text.
    pub fn singleton(self) -> Option<ObjectKind> {
        match self {
            ObjectCategory::Other => Some(ObjectKind::Other(String::new())),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VentKind {
    /// A permanently-open (fixed) vent.
    Open,
    /// A vent that can be closed (hit-and-miss / shuttered).
    Closable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FanType {
    Extract,
    Supply,
    Decentralized,
    /// A central ventilation unit (e.g. MVHR / MEV serving the whole
    /// dwelling). Its presence makes the ventilation NOT natural — it drives
    /// [`ventilation_system.system_type`](crate::ber::systems::VentilationSystem)
    /// via [`Ber::sync_ventilation_system_from_objects`](crate::ber::ber_struct::Ber::sync_ventilation_system_from_objects)
    /// — and the panel requires its make/model name (like a boiler).
    Central,
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

/// The type of a whole-dwelling mechanical ventilation unit (the "dropdown"
/// when placing a VentilationUnit object).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VentUnitKind {
    /// Mechanical extract ventilation (MEV) — continuous extract, no heat recovery.
    Mechanical,
    /// Mechanical ventilation with heat recovery (MVHR).
    HeatRecovery,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SolarPanelKind {
    /// Photovoltaic (electricity).
    Photovoltaic,
    /// Solar thermal (hot water).
    SolarThermal,
    /// A home BATTERY (PV storage) — records its capacity in
    /// [`BuildingObject::kwh`] (the panel requires it) plus a photo.
    Battery,
    /// Solar hot water COLLECTOR (evacuated tubes / flat plate) — additive
    /// (2026-08): older records never contain it.
    SolarTubes,
    /// Solar hot water CONTROLS (pump station / controller) — additive.
    SolarControls,
}

/// An 8-way compass sector the ASSESSOR enters (e.g. the roof side a PV
/// array faces — read off the aligned satellite view, not derived).
/// String-serialized ("SE").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Compass8 {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

impl Compass8 {
    pub const ALL: [Compass8; 8] = [
        Compass8::N,
        Compass8::NE,
        Compass8::E,
        Compass8::SE,
        Compass8::S,
        Compass8::SW,
        Compass8::W,
        Compass8::NW,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Compass8::N => "N",
            Compass8::NE => "NE",
            Compass8::E => "E",
            Compass8::SE => "SE",
            Compass8::S => "S",
            Compass8::SW => "SW",
            Compass8::W => "W",
            Compass8::NW => "NW",
        }
    }
}

/// A concrete recorded object kind: a category plus its subtype (where one
/// applies). The single source of an object's identity + label.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ObjectKind {
    Vent(VentKind),
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
    SolarPanel(SolarPanelKind),
    /// Heating PROGRAMMER / time control (a singleton — the presence + a photo
    /// is the record). Groups under [`ObjectCategory::HeatingSystem`].
    Programmer,
    /// Evidence that the heating is ZONED (e.g. a photo of the zone valves /
    /// wiring centre). Groups under [`ObjectCategory::HeatingSystem`].
    HeatingZones,
    /// An electric PANEL HEATER (a primary electric heat source). Groups under
    /// [`ObjectCategory::HeatingSystem`]; drives an `ElectricHeater` primary.
    PanelHeater,
    /// LEGACY: a whole-house MEV / MVHR unit picked as such. No longer
    /// offered (the MEV/MVHR distinction can't be made on site) — the
    /// [`FanType::Central`] "Central unit" is the record now, and either
    /// makes the ventilation non-natural. Kept so old records deserialize;
    /// sync treats it like a Central fan.
    VentilationUnit(VentUnitKind),
    Other(String),
}

/// Whether recording an object of a given kind MUST include a photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoPolicy {
    /// A photo is mandatory on EVERY instance recorded.
    AlwaysRequired,
    /// A photo is mandatory only on the FIRST instance of this kind recorded
    /// house-wide; later instances are optional.
    FirstInstanceRequired,
    /// A photo is never required.
    Optional,
}

impl ObjectKind {
    /// Whether a photo is required for this kind, and how often (see
    /// [`PhotoPolicy`]). The picker's auto-camera opens for the kinds that
    /// require one (every time, or just the first house-wide instance); the
    /// panel's OK gate enforces it. Set from the assessor's per-item spec.
    pub fn photo_policy(&self) -> PhotoPolicy {
        use PhotoPolicy::*;
        match self {
            // First house-wide instance only: representative evidence is enough.
            ObjectKind::Vent(_)
            | ObjectKind::Fan(FanType::Extract | FanType::Supply)
            | ObjectKind::Light(_)
            | ObjectKind::Radiator(_)
            | ObjectKind::PanelHeater
            | ObjectKind::VentilationUnit(_)
            | ObjectKind::AtticHatch(_) => FirstInstanceRequired,
            // Free-text "Other" never forces a photo.
            ObjectKind::Other(_) => Optional,
            // Everything else must be photographed every time.
            ObjectKind::Fan(FanType::Decentralized | FanType::Central)
            | ObjectKind::Boiler(_)
            | ObjectKind::Programmer
            | ObjectKind::HeatingZones
            | ObjectKind::Thermostat
            | ObjectKind::WeatherCompensation
            | ObjectKind::WaterCylinder(_)
            | ObjectKind::Chimney(_)
            | ObjectKind::WaterFixture(_)
            | ObjectKind::SolarPanel(_) => AlwaysRequired,
        }
    }

    /// Whether a photo is mandatory for an instance of this kind recorded NOW,
    /// given whether one of this kind is ALREADY recorded (house-wide) — the
    /// only context the first-instance policy needs.
    pub fn photo_required(&self, already_recorded: bool) -> bool {
        match self.photo_policy() {
            PhotoPolicy::AlwaysRequired => true,
            PhotoPolicy::FirstInstanceRequired => !already_recorded,
            PhotoPolicy::Optional => false,
        }
    }

    /// Whether recording this kind REQUIRES a NAME (make/model) in the
    /// panel's text field: boilers and the central ventilation unit — the
    /// model name is what the office looks up (efficiency / unit specs).
    pub fn name_required(&self) -> bool {
        matches!(
            self,
            ObjectKind::Boiler(_) | ObjectKind::Fan(FanType::Central)
        )
    }

    /// Whether recording this kind REQUIRES a capacity in kWh (the battery).
    pub fn kwh_required(&self) -> bool {
        matches!(self, ObjectKind::SolarPanel(SolarPanelKind::Battery))
    }

    /// Whether recording this kind REQUIRES a collector area in m² (the
    /// solar hot-water tubes).
    pub fn area_required(&self) -> bool {
        matches!(self, ObjectKind::SolarPanel(SolarPanelKind::SolarTubes))
    }

    /// Whether this is a PHOTOVOLTAIC solar-panel object — the kind carrying
    /// the per-pin PV fields (panel Wp rating, roof-facing side, overshading)
    /// the renewables section reads.
    pub fn is_pv(&self) -> bool {
        matches!(self, ObjectKind::SolarPanel(SolarPanelKind::Photovoltaic))
    }

    pub fn category(&self) -> ObjectCategory {
        match self {
            ObjectKind::Vent(_) => ObjectCategory::Vent,
            // A whole-house mechanical unit sits with the fans (it overlaps the
            // Central fan physically); only VentilationUnit drives the system.
            ObjectKind::Fan(_) | ObjectKind::VentilationUnit(_) => ObjectCategory::Fan,
            ObjectKind::Light(_) => ObjectCategory::Light,
            ObjectKind::Radiator(_) => ObjectCategory::Radiator,
            // Boiler + the heating controls all group under Heating system.
            ObjectKind::Boiler(_)
            | ObjectKind::Thermostat
            | ObjectKind::WeatherCompensation
            | ObjectKind::Programmer
            | ObjectKind::HeatingZones
            | ObjectKind::PanelHeater => ObjectCategory::HeatingSystem,
            ObjectKind::WaterCylinder(_) => ObjectCategory::WaterCylinder,
            ObjectKind::Chimney(_) => ObjectCategory::Chimney,
            ObjectKind::AtticHatch(_) => ObjectCategory::AtticHatch,
            ObjectKind::WaterFixture(_) => ObjectCategory::WaterFixture,
            ObjectKind::SolarPanel(
                SolarPanelKind::SolarThermal
                | SolarPanelKind::SolarTubes
                | SolarPanelKind::SolarControls,
            ) => ObjectCategory::SolarWaterHeating,
            ObjectKind::SolarPanel(_) => ObjectCategory::SolarPanel,
            ObjectKind::Other(_) => ObjectCategory::Other,
        }
    }

    /// The subtype's own short label (e.g. "Extract", "With TRV"). Empty for
    /// kinds with no subtype. Used to label the subtype buttons + the review.
    pub fn subtype_label(&self) -> &str {
        match self {
            ObjectKind::Vent(VentKind::Open) => "Open",
            ObjectKind::Vent(VentKind::Closable) => "Closable",
            ObjectKind::Fan(FanType::Extract) => "Extract",
            ObjectKind::Fan(FanType::Supply) => "Supply",
            ObjectKind::Fan(FanType::Decentralized) => "Decentralized",
            ObjectKind::Fan(FanType::Central) => "Central unit",
            ObjectKind::VentilationUnit(VentUnitKind::Mechanical) => "Mechanical extract (MEV)",
            ObjectKind::VentilationUnit(VentUnitKind::HeatRecovery) => "Heat recovery (MVHR)",
            ObjectKind::Light(LightKind::Led) => "LED",
            ObjectKind::Light(LightKind::Halogen) => "Halogen",
            ObjectKind::Light(LightKind::Incandescent) => "Incandescent",
            ObjectKind::Radiator(RadiatorTrv::WithTrv) => "With TRV",
            ObjectKind::Radiator(RadiatorTrv::WithoutTrv) => "Without TRV",
            ObjectKind::Boiler(BoilerFuel::Oil) => "Oil boiler",
            ObjectKind::Boiler(BoilerFuel::Gas) => "Gas boiler",
            ObjectKind::Boiler(BoilerFuel::HeatPump) => "Heat pump",
            ObjectKind::Programmer => "Programmer",
            ObjectKind::HeatingZones => "Heating zones",
            ObjectKind::PanelHeater => "Panel heater",
            ObjectKind::Thermostat => "Thermostat",
            ObjectKind::WeatherCompensation => "Weather compensation",
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
            ObjectKind::SolarPanel(SolarPanelKind::Photovoltaic) => "Photovoltaic (PV)",
            ObjectKind::SolarPanel(SolarPanelKind::SolarThermal) => "Solar water heating",
            ObjectKind::SolarPanel(SolarPanelKind::SolarTubes) => "Tubes",
            ObjectKind::SolarPanel(SolarPanelKind::SolarControls) => "Controls",
            ObjectKind::SolarPanel(SolarPanelKind::Battery) => "Battery",
            ObjectKind::Other(s) => s,
        }
    }

    /// Full display label: category, plus the subtype in parentheses when there
    /// is one (e.g. "Fan (Extract)", "Vent (Open)"); the heating kinds and
    /// "Other" read as their specific label instead.
    pub fn label(&self) -> String {
        let cat = self.category().label();
        match self {
            // The heating kinds are specific enough on their own — "Oil boiler",
            // "Programmer", "Thermostat" — so don't wrap them as
            // "Heating system (…)"; show the subtype label directly.
            ObjectKind::Boiler(_)
            | ObjectKind::Thermostat
            | ObjectKind::WeatherCompensation
            | ObjectKind::Programmer
            | ObjectKind::HeatingZones
            | ObjectKind::PanelHeater
            | ObjectKind::VentilationUnit(_) => self.subtype_label().to_string(),
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
    /// Capacity in kWh — recorded for a [`SolarPanelKind::Battery`] (the
    /// panel requires it there); `None` for every other kind.
    #[serde(default)]
    pub kwh: Option<f32>,
    /// Collector AREA in m² — recorded for a
    /// [`SolarPanelKind::SolarTubes`] solar hot-water collector (the panel
    /// requires it there); `None` for every other kind. ADDITIVE (2026-08):
    /// older records never contain it and load as `None`.
    #[serde(default)]
    pub area_m2: Option<f32>,
    pub photos: Vec<Evidence>,
    /// The "separate time/temperature control for water and space" checkbox —
    /// meaningful only for `Boiler` and `Programmer` objects; drives
    /// `HeatingSystem.separate_hot_water`.
    #[serde(default)]
    pub separate_hw_control: bool,
    /// Watt-peak rating of ONE panel (e.g. 465), read off the panel label /
    /// datasheet — recorded for a `SolarPanel(Photovoltaic)` object; `None`
    /// for every other kind. ADDITIVE serde field (2026-07): older records
    /// never contain it and load as `None`.
    #[serde(default)]
    pub panel_wp: Option<f32>,
    /// The compass side the HOST ROOF PLANE faces, ENTERED by the assessor
    /// (not derived) — recorded for a `SolarPanel(Photovoltaic)` object.
    /// ADDITIVE serde field (2026-07).
    #[serde(default)]
    pub roof_faces: Option<Compass8>,
    /// The array's OVERSHADING, assessed separately from windows and at the
    /// collector's tilt (DEAP Table H3: Heavy / Significant / Modest / None
    /// or very little — the shared 4-level scale) — recorded for a
    /// `SolarPanel(Photovoltaic)` object. ADDITIVE serde field (2026-08).
    #[serde(default)]
    pub overshading: Option<crate::ber::fenestration::Overshading>,
}

impl BuildingObject {
    pub fn new(kind: ObjectKind, count: u32) -> Self {
        Self {
            id: ItemId::default(),
            kind,
            count: count.max(1),
            note: None,
            kwh: None,
            area_m2: None,
            photos: Vec::new(),
            separate_hw_control: false,
            panel_wp: None,
            roof_faces: None,
            overshading: None,
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
    /// Height of the pin above ITS storey's floor datum (metres), recorded when
    /// the pin was placed on a surface in the 3D view. `None` for plan (2D)
    /// pins — those render at the legacy datum + 1 m.
    #[serde(default)]
    pub z_m: Option<f64>,
    /// The picked surface's pitch (degrees from horizontal) when the pin was
    /// placed on a SLOPED surface in 3D — a roof plane (flat roof = 0°) or a
    /// sloped ceiling. `None` for walls, floors, flat ceilings, and 2D pins.
    /// Shown read-only in the add-object dialog (e.g. solar panels on a roof).
    #[serde(default)]
    pub surface_pitch_deg: Option<f32>,
    /// Whether the pin was placed on a dwelling ROOF plane (the weather
    /// envelope — e.g. solar panels), as opposed to any interior surface. Roof
    /// pins belong to the roof, not a storey plan: the per-storey views hide
    /// them (they stay visible in the full-house 3D view). `#[serde(default)]`
    /// so pins recorded before this flag load as interior; see
    /// [`is_roof_mounted`](Self::is_roof_mounted) for the legacy fallback.
    #[serde(default)]
    pub on_roof: bool,
}

impl ObjectRoom {
    pub fn new(plan_position: Point2f32) -> Self {
        Self {
            id: ItemId::default(),
            name: String::new(),
            plan_position,
            objects: Vec::new(),
            z_m: None,
            surface_pitch_deg: None,
            on_roof: false,
        }
    }

    /// A pin placed on a surface in the 3D view: `z_m` above its storey's floor
    /// datum, carrying the surface's pitch when the surface is sloped, and
    /// `on_roof` when the surface is a dwelling roof plane.
    pub fn new_on_surface(
        plan_position: Point2f32,
        z_m: f64,
        surface_pitch_deg: Option<f32>,
        on_roof: bool,
    ) -> Self {
        Self {
            z_m: Some(z_m),
            surface_pitch_deg,
            on_roof,
            ..Self::new(plan_position)
        }
    }

    /// The house-level ATTIC bucket (objects in the attic space — e.g. a
    /// heat-recovery unit). Not a plan pin: it lives on
    /// [`Ber::attic_room`](crate::ber::ber_struct::Ber::attic_room), has no
    /// meaningful plan position, and never renders on a storey plan.
    pub fn new_attic() -> Self {
        Self {
            name: "Attic".to_string(),
            ..Self::new(Point2f32::new(0.0, 0.0))
        }
    }

    /// Whether this pin sits on the dwelling ROOF (drives the per-storey view
    /// filter). The explicit [`on_roof`](Self::on_roof) flag decides for pins
    /// placed since it exists; for LEGACY pins (flag absent → `false`) a
    /// pitched pin ABOVE the storey's flat ceiling height can only be on the
    /// roof envelope — interior sloped-ceiling pins sit at or below it.
    pub fn is_roof_mounted(&self, storey_ceiling_h_m: Option<f64>) -> bool {
        if self.on_roof {
            return true;
        }
        match (self.surface_pitch_deg, self.z_m, storey_ceiling_h_m) {
            (Some(_), Some(z), Some(h)) => z > h + 0.05,
            _ => false,
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The heating plant + controls are ONE picker section — "Heating system" —
    /// whose subtypes are the boiler (by fuel), programmer, zone evidence,
    /// thermostat and weather compensation. (r07 + r09 field reports.)
    #[test]
    fn heating_system_groups_boiler_and_controls() {
        // The category exists, is offered, and reads "Heating system".
        assert!(ObjectCategory::ALL.contains(&ObjectCategory::HeatingSystem));
        assert_eq!(ObjectCategory::HeatingSystem.label(), "Heating system");

        // Its subtypes are exactly the boiler fuels + the four controls, in order.
        let labels: Vec<String> = ObjectCategory::HeatingSystem
            .subtypes()
            .iter()
            .map(|k| k.subtype_label().to_string())
            .collect();
        assert_eq!(
            labels,
            [
                "Oil boiler",
                "Gas boiler",
                "Heat pump",
                "Panel heater",
                "Programmer",
                "Heating zones",
                "Thermostat",
                "Weather compensation",
            ]
        );

        // Every heating kind resolves to the one category…
        for k in ObjectCategory::HeatingSystem.subtypes() {
            assert_eq!(k.category(), ObjectCategory::HeatingSystem, "{k:?}");
        }
        // …and its display label is the specific thing, not "Heating system (…)".
        assert_eq!(ObjectKind::Boiler(BoilerFuel::Oil).label(), "Oil boiler");
        assert_eq!(ObjectKind::Programmer.label(), "Programmer");
        assert_eq!(ObjectKind::Thermostat.label(), "Thermostat");
        assert_eq!(
            ObjectKind::WeatherCompensation.label(),
            "Weather compensation"
        );
    }

    /// Back-compat: records saved before the regroup — a boiler, a lone
    /// thermostat, a weather-compensation object — still deserialize and now
    /// land under Heating system.
    #[test]
    fn legacy_heating_object_kinds_still_load_and_regroup() {
        for json in [
            r#"{"Boiler":"Gas"}"#,
            r#""Thermostat""#,
            r#""WeatherCompensation""#,
        ] {
            let kind: ObjectKind = serde_json::from_str(json).expect("legacy kind loads");
            assert_eq!(kind.category(), ObjectCategory::HeatingSystem, "{json}");
        }
    }
}
