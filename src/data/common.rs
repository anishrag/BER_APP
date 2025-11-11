#[derive(Debug, Serialize, Deserialize)]
pub enum DefaultOrNot {
    Default,
    Custom(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Photo {
    pub id: Uuid,
    pub file_path: String, // local path to image
    pub description: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ConstructionPhase {
    pub id: Uuid,
    pub phase_name: String, // e.g., "Original", "Extension 1"
    pub year_built: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ElementType {
    Floor,
    Wall,
    Roof,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ChimneyFuel {
    Gas,
    SolidFuel,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub enum ItemType {
    Thermostat,
    Vent,
    Fan,
    Chimney { open: bool, fuel_type: ChimneyFuel },
    Light,
    Other(String),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Item {
    pub id: Uuid,
    pub name: String,
    pub item_type: ItemType,
    pub photos: Vec<Photo>,
    pub description: Option<String>,
}
