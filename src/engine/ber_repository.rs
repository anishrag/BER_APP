use chrono::NaiveDate;
use rusqlite::{params, Connection, Result};
use serde_json;
use uuid::Uuid;

use crate::datatypes::ber::ber::Ber;

#[derive(Debug)]
pub struct BerRecord {
    pub id: Uuid,
    pub name: String,
    pub date_assessed: NaiveDate,
    pub data: Ber, // includes address and eircode
}

#[derive(Debug)]
pub struct BerSummary {
    pub id: Uuid,
    pub name: String,
    pub date_assessed: NaiveDate,
    pub address: String, // extracted from Ber JSON
    pub eircode: String, // extracted from Ber JSON
}

pub struct BerRepository<'a> {
    conn: &'a Connection,
}

impl<'a> BerRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn init_db(&self) -> Result<()> {
        self.conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS ber (
                id TEXT PRIMARY KEY,
                name TEXT NOT NULL,
                date_assessed TEXT NOT NULL,
                data_json TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_ber_date ON ber(date_assessed);
            ",
        )?;
        Ok(())
    }

    /// List BERs as lightweight summaries (extracting address and eircode from JSON)
    pub fn list(&self) -> Result<Vec<BerSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, date_assessed, data_json FROM ber ORDER BY date_assessed DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let id = Uuid::parse_str(&id_str).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            let date_str: String = row.get(2)?;
            let date_assessed = NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    2,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            let json: String = row.get(3)?;
            let ber: Ber = serde_json::from_str(&json).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    3,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })?;

            Ok(BerSummary {
                id,
                name: row.get(1)?,
                date_assessed,
                address: ber.address,
                eircode: ber.eircode,
            })
        })?;

        rows.collect()
    }

    /// Delete a BER by id
    pub fn delete(&self, id: &Uuid) -> Result<()> {
        self.conn
            .execute("DELETE FROM ber WHERE id = ?1", params![id.to_string()])?;
        Ok(())
    }

    /// Insert or update a BER record
    pub fn upsert(&self, record: &BerRecord) -> Result<()> {
        let data_json = serde_json::to_string(&record.data)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        let date_str = record.date_assessed.format("%Y-%m-%d").to_string();

        self.conn.execute(
            "INSERT INTO ber (id, name, date_assessed, data_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                date_assessed = excluded.date_assessed,
                data_json = excluded.data_json
            ",
            params![record.id.to_string(), record.name, date_str, data_json,],
        )?;

        Ok(())
    }

    /// Load a full BER record by id
    pub fn load(&self, id: &Uuid) -> Result<BerRecord> {
        self.conn.query_row(
            "SELECT name, date_assessed, data_json FROM ber WHERE id = ?1",
            params![id.to_string()],
            |row| {
                let name: String = row.get(0)?;
                let date_str: String = row.get(1)?;
                let json: String = row.get(2)?;

                let date_assessed =
                    NaiveDate::parse_from_str(&date_str, "%Y-%m-%d").map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            1,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?;

                let data: Ber = serde_json::from_str(&json).map_err(|e| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(e),
                    )
                })?;

                Ok(BerRecord {
                    id: *id,
                    name,
                    date_assessed,
                    data,
                })
            },
        )
    }
}
