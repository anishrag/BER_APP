use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::building::Building;
use super::general::{DeapMetadata, DwellingDetails};
use super::lighting::Lighting;
use super::renewables::Renewables;
use super::space_heating::SpaceHeating;
use super::ventilation::Ventilation;
use super::water_heating::WaterHeating;

/// Top-level DEAP input structure
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeapInput {
    pub metadata: DeapMetadata,            // Section 0
    pub dwelling_details: DwellingDetails, // Section 0
    pub building: Building,                // Section 1
    pub ventilation: Ventilation,          // Section 2
    pub space_heating: SpaceHeating,       // Section 3
    pub water_heating: WaterHeating,       // Section 4
    pub lighting: Lighting,                // Section 5
    pub renewables: Option<Renewables>,    // Section 6
}
