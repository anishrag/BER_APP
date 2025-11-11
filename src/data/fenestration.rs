// src/data/fenestration.rs

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub enum Glazing {
    Single,
    Double,
    Triple,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Frame {
    Wood_UPVC,
    Metal,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WindowType {
    pub id: Uuid,
    pub name: String, // e.g., "Double Glazed", "Triple Glazed"
    pub year: i32,
    pub glazing: Glazing,
    pub frame: Frame,
    pub specifics: DefaultOrNot,
    pub photos: Vec<Photo>,
    pub notes: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Window {
    pub id: Uuid,
    pub vertices: Vec<(f32, f32, f32)>,
    pub width_m: f32,
    pub height_m: f32,
    pub window_type_id: Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WindowTypeGroup {
    pub window_type: WindowType,
    pub windows: Vec<Uuid>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Door {
    pub id: Uuid,
    pub type_name: String,
    pub vertices: Vec<(f32, f32, f32)>,
    pub photos: Vec<Photo>,
}

impl WindowTypeGroup {
    pub fn validate(&self, windows: &[Window]) -> bool {
        self.windows.iter().all(|id| {
            windows
                .iter()
                .any(|w| w.id == *id && w.window_type_id == self.window_type.id)
        })
    }
}
