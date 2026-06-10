use serde::{Deserialize, Serialize};

use super::common::{ConstructionPhases, ItemId, Photo};
use super::structural::{
    CeilingConstruction, FloorConstruction, Storey, ThermalMassInfo, WallConstruction,
};
use super::systems::{HeatingSystem, SolarElements, VentilationSystem};

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Ber {
    pub id: ItemId,
    pub address: String,
    pub eircode: String,
    pub construction_phases: ConstructionPhases,
    pub heating_system: HeatingSystem,
    pub ventilation_system: VentilationSystem,
    pub sheltered_sides: f32,
    pub number_of_storeys: i32,
    pub storeys: Vec<Storey>,
    /// Shared wall/floor/ceiling constructions ("a few per house"). Storey
    /// regions reference these by `construction_id`; grouping for DEAP export
    /// collapses regions onto their shared construction. `#[serde(default)]` so
    /// records written before the split still load.
    #[serde(default)]
    pub wall_constructions: Vec<WallConstruction>,
    #[serde(default)]
    pub floor_constructions: Vec<FloorConstruction>,
    #[serde(default)]
    pub ceiling_constructions: Vec<CeilingConstruction>,
    pub thermal_mass_info: ThermalMassInfo,
    pub orientation_deg: f32,
    pub solar: Vec<SolarElements>,
    pub photos: Vec<Photo>,
}

// Pure data helpers, optional
impl Ber {
    pub fn new() -> Self {
        Self {
            number_of_storeys: 1,
            ..Default::default()
        }
    }

    /// Stamp the default ceiling height: on storey `storey_idx` when given, else
    /// on every storey that doesn't already have one (the all-storeys 3D prompt —
    /// one typed value fills the gaps, never overwrites a set storey).
    /// Caller has validated `height_m > 0`.
    pub fn set_default_ceiling_height(&mut self, storey_idx: Option<usize>, height_m: f64) {
        match storey_idx {
            Some(idx) => {
                if let Some(storey) = self.storeys.get_mut(idx) {
                    storey.ceiling_height_m = Some(height_m);
                }
            }
            None => {
                for storey in &mut self.storeys {
                    if storey.ceiling_height_m.is_none() {
                        storey.ceiling_height_m = Some(height_m);
                    }
                }
            }
        }
    }
}
