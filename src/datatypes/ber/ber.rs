use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{ConstructionPhase, Photo};
use super::structural::{Storey, ThermalMassInfo};
use super::systems::{HeatingSystem, SolarElements, VentilationSystem};

#[derive(Debug, Serialize, Deserialize)]
pub struct Ber {
    pub id: Uuid,
    pub address: String,
    pub eircode: String,
    pub construction_phases: Vec<ConstructionPhase>,
    pub heating_system: HeatingSystem,
    pub ventilation_system: VentilationSystem,
    pub sheltered_sides: f32,
    pub number_of_storeys: i32,
    pub storeys: Vec<Storey>,
    pub thermal_mass_info: ThermalMassInfo,
    pub orientation_deg: f32,
    pub solar: Vec<SolarElements>,
    pub photos: Vec<Photo>,
}

// Pure data helpers, optional
impl Ber {
    pub fn new() -> Self {
        Self {
            id: Uuid::new_v4(),
            address: String::new(),
            eircode: String::new(),
            construction_phases: vec![],
            heating_system: Default::default(),
            ventilation_system: Default::default(),
            sheltered_sides: 0.0,
            number_of_storeys: 1,
            storeys: vec![],
            thermal_mass_info: Default::default(),
            orientation_deg: 0.0,
            solar: vec![],
            photos: vec![],
        }
    }
}
