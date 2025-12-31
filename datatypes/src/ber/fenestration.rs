// src/data/fenestration.rs

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::common::{Photo, Year};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Glazing {
    Single,
    Double,
    Triple,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Frame {
    WoodUPVC,
    Metal,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WindowNameSpecifics {
    Standard,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowType {
    pub id: Uuid,
    pub year: Year,
    pub glazing: Glazing,
    pub gap: Vec<u32>, // gap in mm for double/triple glazing
    pub frame: Frame,
    pub specifics: WindowNameSpecifics,
    pub photos: Vec<Photo>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: Uuid,
    pub vertices: Vec<(f32, f32, f32)>,
    pub width_m: f32,
    pub height_m: f32,
    pub window_type_id: Uuid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Door {
    pub id: Uuid,
    pub type_name: String,
    pub vertices: Vec<(f32, f32, f32)>,
    pub photos: Vec<Photo>,
}
