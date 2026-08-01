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
    /// The heat-source APPLIANCE's documentation (BERresolve Certificates
    /// stage): a heat pump's datasheet (COP/SPF), a boiler's efficiency
    /// document — whatever backs the DEAP heating inputs when they are
    /// assembled. `#[serde(default)]` so older records load.
    #[serde(default)]
    pub certificates: Vec<crate::ber::common::Evidence>,
    /// "Skip for now" from the resolver's Certificates stage — see
    /// [`crate::ber::fenestration::WindowType::cert_skipped`].
    #[serde(default)]
    pub cert_skipped: bool,
}

/// Office-resolved SOLAR PV details (BERresolve Certificates stage). The
/// survey pins the array — placement, panel counts, photos — but the array's
/// RATING lives in the installation's documentation, so the office reads the
/// total kWp off the panel datasheet / installer cert and attaches the
/// documents here. Consumed by the report's renewables section (the record
/// pre-answers its own `solar-pv-kwp` query). All-default so records from
/// before the struct load empty.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SolarPvResolution {
    /// Total installed capacity (kWp), from the documentation.
    #[serde(default)]
    pub kwp: Option<f32>,
    /// The panel datasheet / installer cert documents.
    #[serde(default)]
    pub certificates: Vec<crate::ber::common::Evidence>,
    /// "Skip for now" from the resolver's Certificates stage — see
    /// [`crate::ber::fenestration::WindowType::cert_skipped`].
    #[serde(default)]
    pub cert_skipped: bool,
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

/// What the CENTRAL VENTILATION UNIT actually is — resolved at the office
/// from the unit's make/model (it can't be told apart on site; the survey
/// only records the "Central unit" fan pin + its name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CentralUnitType {
    /// MEV — continuous mechanical extract.
    MechanicalExtract,
    /// MVHR — needs the heat-recovery EFFICIENCY too.
    HeatRecovery,
    /// Exhaust-air heat pump — needs the EXHAUST AIR FLOW RATE too.
    ExhaustAirHeatPump,
}

impl CentralUnitType {
    pub fn label(&self) -> &'static str {
        match self {
            CentralUnitType::MechanicalExtract => "Mechanical extract (MEV)",
            CentralUnitType::HeatRecovery => "Heat recovery (MVHR)",
            CentralUnitType::ExhaustAirHeatPump => "Exhaust-air heat pump",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VentilationSystem {
    pub id: ItemId,
    pub system_type: VentilationSystemType, // e.g., "Natural ventilation", "Mechanical ventilation with heat recovery"
    pub description: Option<String>,
    /// ---- Central-unit details, entered at the office (BERresolve
    /// Certificates stage) when the survey pinned a Central-unit fan ----
    /// What the unit is (MEV / MVHR / exhaust-air heat pump).
    #[serde(default)]
    pub unit_type: Option<CentralUnitType>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Specific fan power, W/(l/s).
    #[serde(default)]
    pub sfp_w_per_ls: Option<f32>,
    /// Heat-recovery efficiency (%) — MVHR only.
    #[serde(default)]
    pub heat_recovery_efficiency_pct: Option<f32>,
    /// Exhaust air flow rate (l/s) — exhaust-air heat pump only.
    #[serde(default)]
    pub exhaust_air_flow_ls: Option<f32>,
    /// The unit's certificate / datasheet documents.
    #[serde(default)]
    pub certificates: Vec<crate::ber::common::Evidence>,
    /// "Skip for now" from the resolver's Certificates stage — the central
    /// unit's cert deliberately deferred (see `WindowType::cert_skipped`).
    #[serde(default)]
    pub cert_skipped: bool,
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
