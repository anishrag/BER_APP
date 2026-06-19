use serde::{Deserialize, Serialize};

// ------------------- SECTION 6: Renewables -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Renewables {
    pub solar_pv_kwp: f32,
    pub solar_pv_description: String,
}
