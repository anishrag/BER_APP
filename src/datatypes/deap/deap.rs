use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Top-level DEAP input structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeapInput {
    pub metadata: DeapMetadata,            // Section 0
    pub dwelling_details: DwellingDetails, // Section 1
    pub building: Building,                // Section 3
    pub ventilation: Ventilation,          // Section 2
    pub fabric: Fabric,                    // Section 4
    pub openings: Openings,                // Section 5
    pub thermal_bridging: ThermalBridging, // Section 6
    pub space_heating: SpaceHeating,       // Section 7
    pub water_heating: WaterHeating,       // Section 8
    pub lighting: Lighting,                // Section 9
    pub renewables: Renewables,            // Section 10
}

// ------------------- SECTION 0: Metadata -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeapMetadata {
    pub ber_id: Uuid,
    pub date_assessed: NaiveDate,
}

// ------------------- SECTION 1: Dwelling Details -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DwellingDetails {
    pub mprn: Option<String>,
    pub dwelling_type: DwellingType,
    pub year_of_construction: Option<u16>,
    pub address: String,
    pub eircode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DwellingType {
    Detached,
    SemiDetached,
    MidTerrace,
    Apartment,
    Other(String),
}

// ------------------- SECTION 2: Building -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Building {
    pub floors: Floors,
    pub roofs: Roofs,
    pub walls: Walls,
    pub doors: Doors,
    pub windows: Windows,
    pub global_factors: GlobalFactors,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExposedPerimeter {
    NotApplicable,
    Custom(f32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UValue {
    Default,
    Custom(f32),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FloorHeatLossType {
    GroundSolid,
    GroundSuspended,
    NonHeatLoss,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloorElement {
    pub id: Uuid,
    pub description: String,
    pub floor_type: FloorHeatLossType,
    pub area: f32,
    pub exposed_perimeter_m: ExposedPerimeter,
    pub u_value: UValue,
    pub construction_id: Uuid,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Floors {
    pub id: Uuid,
    pub num_storeys: u32,
    pub living_area_m2: f32,
    pub avg_ceiling_heights_m: Vec<f32>,
    pub elements: Vec<FloorElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RoofHeatLossType {
    SlopedInsOnSlope,
    SlopedInsOnCeiling,
    Flat,
    Other(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoofElement {
    pub id: Uuid,
    pub description: String,
    pub roof_type: RoofHeatLossType,
    pub insulation_thickness_mm: Option<f32>,
    pub area: f32,
    pub construction_id: Uuid,
    pub u_value: UValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Roofs {
    pub id: Uuid,
    pub elements: Vec<RoofElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WallHeatLossType {
    ConcreteHollowBlock,
    Cavity,
    FilledCavity,
    TimberFrame,
    Other(String),
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WallElement {
    pub id: Uuid,
    pub description: String,
    pub area: f32,
    pub construction_id: Uuid,
    pub wall_type: WallHeatLossType,
    pub u_value: UValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Walls {
    pub id: Uuid,
    pub elements: Vec<WallElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Door {
    pub id: Uuid,
    pub area_m2: f32,
    pub u_value: UValue,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Doors {
    pub id: Uuid,
    pub doors: Vec<Door>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowType {
    pub id: Uuid,
    pub glazing_number: u32,
    pub details: WindowDetails,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GlassCoating {
    None,
    LowE015Hard,
    LowE010Hard,
    LowE005Hard,
    LowE015Soft,
    LowE010Soft,
    LowE005Soft,
    Default,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GasFill {
    Air,
    Argon,
    Default,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowDetails {
    pub glazing_number: u32,
    pub glazing_gaps_mm: Vec<f32>,
    pub frame_type: String,
    pub fill_gas: GasFill,
    pub coating: GlassCoating,
}

// ------------------- SECTION 3: Ventilation -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ventilation {
    pub method: VentilationType,
    pub air_permeability: f32, // m³/h.m² at 50Pa
    pub chimneys: u32,
    pub flues: u32,
    pub fans: u32,
    pub passive_vents: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum VentilationType {
    Natural,
    MEV,
    MVHR { efficiency: f32 },
}

// ------------------- SECTION 4: Fabric -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Fabric {
    pub constructions: Vec<Construction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Construction {
    pub id: Uuid,
    pub name: String,
    pub u_value: f32,
    pub layers: Option<Vec<Layer>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layer {
    pub material: String,
    pub thickness_mm: f32,
    pub conductivity_w_mk: f32,
}

// ------------------- SECTION 5: Openings -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Openings {
    pub windows: Vec<Window>,
    pub doors: Vec<Door>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: Uuid,
    pub area: f32,
    pub orientation: Orientation,
    pub u_value: f32,
    pub g_value: f32,
    pub frame_factor: f32,
    pub overshading: OverShading,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Door {
    pub id: Uuid,
    pub area: f32,
    pub u_value: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Orientation {
    N,
    NE,
    E,
    SE,
    S,
    SW,
    W,
    NW,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OverShading {
    None,
    Light,
    Medium,
    Heavy,
}

// ------------------- SECTION 6: Thermal Bridging -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThermalBridging {
    pub accredited: bool,
    pub y_value: f32,
}

// ------------------- SECTION 7: Space Heating -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceHeating {
    pub main_systems: Vec<HeatingSystem>,
    pub secondary_system: Option<HeatingSystem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatingSystem {
    pub id: Uuid,
    pub system_type: HeatingSystemType,
    pub efficiency: f32,
    pub emitter_type: EmitterType,
    pub fraction_of_load: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum HeatingSystemType {
    Boiler { fuel: FuelType, condensing: bool },
    HeatPump { cop: f32 },
    RoomHeater { fuel: FuelType },
    DistrictHeating,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FuelType {
    Oil,
    Gas,
    Electricity,
    Peat,
    Biomass,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EmitterType {
    Radiators,
    Underfloor,
    Electric,
}

// ------------------- SECTION 8: Water Heating -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaterHeating {
    pub dhw_system: DHWSystem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DHWSystem {
    pub cylinder_volume_l: f32,
    pub insulation: DHWInsulation,
    pub primary_system_efficiency: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DHWInsulation {
    FactoryApplied { thickness_mm: f32 },
    Jacket { thickness_mm: f32 },
    None,
}

// ------------------- SECTION 9: Lighting -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lighting {
    pub low_energy_lamps_percent: f32,
}

// ------------------- SECTION 10: Renewables -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Renewables {
    pub solar_pv_kwp: f32,
    pub solar_thermal_area: f32,
}
