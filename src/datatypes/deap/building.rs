use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------- SECTION 1: Building -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Building {
    pub floors: Floors,
    pub roofs: Roofs,
    pub walls: Walls,
    pub doors: Doors,
    pub windows: WindowGroups,
    pub global_factors: GlobalFactors,
}

// ------------------- Floors -------------------

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

// ------------------- Roofs -------------------

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

// ------------------- Walls -------------------

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

// ------------------- Doors -------------------

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

// ------------------- Windows -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GlassCoating {
    None,
    LowE015Hard,
    LowE010Hard,
    LowE005Hard,
    LowE015Soft,
    LowE010Soft,
    LowE005Soft,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GasFill {
    Air,
    Argon,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomWindow {
    pub u_value: f32,
    pub g_value: f32,
    pub frame_factor: f32,
    pub glazing_number: u32,
    pub glazing_gaps_mm: Vec<f32>,
    pub frame_type: String,
    pub fill_gas: GasFill,
    pub coating: GlassCoating,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowDetails {
    Default,
    Custom { custom_window: CustomWindow },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowGroups {
    pub id: Uuid,
    pub window_groups: Vec<WindowGroup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowGroup {
    pub year_installed: Option<u16>,
    pub window_details: WindowDetails,
    pub windows: Vec<Window>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: Uuid,
    pub area: f32,
    pub orientation: Orientation,
    pub overshading: OverShading,
    pub num_draught_stripped: u32,
    pub num_not_draught_stripped: u32,
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
    Horizontal,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OverShading {
    Little,
    Average,
    MoreThanAverage,
    Heavy,
}

// ------------------- Global Factors -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThermalMassCategory {
    Low,
    MediumLow,
    Medium,
    MediumHigh,
    High,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalFactors {
    pub thermal_bridging: ThermalBridging,
    pub thermal_mass: ThermalMassCategory,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ThermalBridging {
    Default,
    ACDs,
    TGDL2005,
    Custom { y_value: f32 },
}
