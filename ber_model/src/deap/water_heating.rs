use serde::{Deserialize, Serialize};

// ------------------- SECTION 4: Water Heating -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaterHeating {
    pub water_storage: Option<Storage>,
    storage_indoors: bool,
    mfct_declared_loss: Option<f32>,
    distribution_losses: bool,
    low_water_usage: bool,
    combi_boiler: CombiBoiler,
    baths: u32,
    electric_showers: u32,
    mixer_showers_vented: u32,
    mixer_showers_unvented: u32,
    mixer_showers_vented_with_pump: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Storage {
    pub cylinder_volume_l: f32,
    pub insulation: DHWInsulation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DHWInsulation {
    FactoryApplied { thickness_mm: f32 },
    Jacket { thickness_mm: f32 },
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombiBoiler {
    pub combi_type: CombiType,
    pub keep_hot: CombiKeepHot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CombiType {
    InstantaneousNoKeepHot,
    InstantaneousWithKeepHotTimeClocked,
    InstantaneousWithKeepHotNotTimeClocked,
    StorageLessThan55L,
    StorageMoreThan55L,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum CombiKeepHot {
    None,
    KeepHotTimeClocked,
    KeepHotNotTimeClocked,
}
