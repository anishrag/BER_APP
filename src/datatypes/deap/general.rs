use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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
    pub year_of_construction: u16,
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
