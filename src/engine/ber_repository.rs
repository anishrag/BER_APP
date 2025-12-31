use chrono::NaiveDate;
use rusqlite::{params, Connection, Result};
use serde_json;
use uuid::Uuid;

use datatypes::ber::ber::Ber;

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use rusqlite::Connection;
    use uuid::Uuid;

    fn make_test_record(id: Uuid) -> BerRecord {
        let ber = Ber {
            id,
            address: "123 Test Street".to_string(),
            eircode: "D01TEST".to_string(),
            ..Default::default()
        };

        BerRecord {
            id,
            name: "Test BER".to_string(),
            date_assessed: NaiveDate::from_ymd_opt(2024, 1, 1).unwrap(),
            data: ber,
        }
    }

    #[test]
    fn init_db_creates_table() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);

        repo.init_db().unwrap();

        let exists: bool = conn
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM sqlite_master
                    WHERE type='table' AND name='ber'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert!(exists, "ber table should exist after init_db");
    }

    #[test]
    fn upsert_inserts_row() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let record = make_test_record(Uuid::new_v4());
        repo.upsert(&record).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ber", [], |row| row.get(0))
            .unwrap();

        assert_eq!(count, 1);
    }

    #[test]
    fn upsert_stores_valid_json() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let record = make_test_record(Uuid::new_v4());
        repo.upsert(&record).unwrap();

        let json: String = conn
            .query_row("SELECT data_json FROM ber", [], |row| row.get(0))
            .unwrap();

        let parsed: Result<Ber, _> = serde_json::from_str(&json);
        assert!(parsed.is_ok(), "Stored data_json should be valid BER JSON");
    }

    #[test]
    fn load_returns_same_record() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let id = Uuid::new_v4();
        let record = make_test_record(id);

        repo.upsert(&record).unwrap();

        let loaded = repo.load(&id).unwrap();

        assert_eq!(loaded.id, record.id);
        assert_eq!(loaded.name, record.name);
        assert_eq!(loaded.date_assessed, record.date_assessed);
        assert_eq!(loaded.data.address, record.data.address);
        assert_eq!(loaded.data.eircode, record.data.eircode);
    }

    #[test]
    fn upsert_updates_existing_row() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let id = Uuid::new_v4();
        let mut record = make_test_record(id);

        repo.upsert(&record).unwrap();

        record.name = "Updated Name".to_string();
        record.data.address = "456 Updated Street".to_string();

        repo.upsert(&record).unwrap();

        let loaded = repo.load(&id).unwrap();
        assert_eq!(loaded.name, "Updated Name");
        assert_eq!(loaded.data.address, "456 Updated Street");
    }

    #[test]
    fn list_returns_summaries() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let record1 = make_test_record(Uuid::new_v4());
        let record2 = make_test_record(Uuid::new_v4());

        repo.upsert(&record1).unwrap();
        repo.upsert(&record2).unwrap();

        let list = repo.list().unwrap();

        assert_eq!(list.len(), 2);
        assert!(!list[0].address.is_empty());
        assert!(!list[0].eircode.is_empty());
    }

    #[test]
    fn delete_removes_row() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let record = make_test_record(Uuid::new_v4());
        repo.upsert(&record).unwrap();

        repo.delete(&record.id).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ber", [], |row| row.get(0))
            .unwrap();

        assert_eq!(count, 0);
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let result = repo.load(&Uuid::new_v4());
        assert!(result.is_err());
    }
}
