// src/data/structural.rs

use super::common::{DefaultOrNot, ElementType, Photo};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize)]
pub struct Opes {
    pub doors: Vec<Uuid>,   // references to Door IDs
    pub windows: Vec<Uuid>, // references to Window IDs
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PlanarElement {
    pub id: Uuid,
    pub vertices: Vec<(f32, f32, f32)>, // e.g., 3D points (x, y, z)
    pub element_type: ElementType,      // e.g., "floor", "wall", "roof"
    pub construction_phase_id: Uuid,
    pub opes: Opes,
    pub default_or_not: DefaultOrNot, // e.g., "Default" or custom description
    pub photos: Vec<Photo>,
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConstructionPhase {
    pub id: Uuid,
    pub phase_name: String, // e.g., "Original", "Extension 1"
    pub year_built: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Room {
    pub id: Uuid,
    pub name: String,
    pub vertices: Vec<(f32, f32, f32)>,
    pub thermostats: Vec<Item>,
    pub vents: Vec<Item>,
    pub fans: Vec<Item>,
    pub lights: Vec<Item>,
    pub chimneys: Vec<Item>,
    pub trvs: Vec<Item>,
    pub living_room_candidate: bool,
    pub underfloor_heating: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Storey {
    pub id: Uuid,
    pub name: String, // e.g., "Ground Floor", "First Floor"
    pub floors: Vec<PlanarElement>,
    pub walls: Vec<PlanarElement>,
    pub roofs: Vec<PlanarElement>,
    pub windows: Vec<Window>,
    pub doors: Vec<Door>,
    pub rooms: Vec<Room>,
}
