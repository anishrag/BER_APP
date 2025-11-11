use chrono::NaiveDate;
use rusqlite::{params, Connection, Result};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::fenestration::{DoorTypeGroup, WindowTypeGroup};
use super::structural::{PlanarElement, Storey, ThermalMassInfo};
use super::systems::{HeatingSystem, ShelteredSideInfo, VentilationSystem};

#[derive(Debug, Serialize, Deserialize)]
pub struct Ber {
    pub id: Uuid,
    pub address: String,
    pub date_assessed: NaiveDate,
    pub construction_phases: Vec<ConstructionPhase>,
    pub window_types: Vec<WindowTypeGroup>,
    pub heating_system: HeatingSystem,
    pub ventilation_system: VentilationSystem,
    pub sheltered_sides: f32,
    pub number_of_storeys: i32,
    pub storeys: Vec<Storey>,
    pub thermal_mass_info: ThermalMassInfo,
    pub orientation_deg: f32, // The orientation of 'up' on the drawing in degrees from north
    // (clockwise, so 0 = north, 90 = east, etc.)
    pub solar: Vec<SolarElements>,
    pub photos: Vec<Photo>,
}

impl Ber {
    pub fn init_db(conn: &Connection) -> Result<()> {
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS ber (
                id TEXT PRIMARY KEY,
                address NOT NULL,
                date_assessed TEXT NOT NULL, -- store NaiveDate as YYYY-MM-DD
                data_json TEXT NOT NULL
            );
            ",
        )?;
        Ok(())
    }

    pub fn upsert(&self, conn: &Connection) -> Result<()> {
        let data_json = serde_json::to_string(self).unwrap();
        let date_str = self.date_assessed.format("%Y-%m-%d").to_string(); // <- store as text
        conn.execute(
            "INSERT INTO ber (id, address, date_assessed, data_json) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET address=excluded.address, date_assessed=excluded.date_assessed, data_json=excluded.data_json",
            params![self.id.to_string(), self.address, date_str, data_json]
        )?;
        Ok(())
    }

    pub fn load(conn: &Connection, id: &Uuid) -> Result<Self> {
        let mut stmt = conn.prepare("SELECT data_json, date_assessed FROM ber WHERE id = ?1")?;
        let row = stmt.query_row(params![id.to_string()], |row| {
            let data_json: String = row.get(0)?;
            let date_str: String = row.get(1)?;
            let mut ber: Ber = serde_json::from_str(&data_json).unwrap();
            ber.date_assessed = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").unwrap();
            Ok(ber)
        })?;
        Ok(row)
    }
}
