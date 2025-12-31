use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ------------------- SECTION 5: Lighting -------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lighting {
    pub led_cfl: u32,
    pub halogen: u32,
    pub incandescent: u32,
    pub halogen_lv: u32,
    pub linear_flourescent: u32,
}
