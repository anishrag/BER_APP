use chrono::NaiveDate;
use rusqlite::{params, Connection, OptionalExtension, Result};
use serde_json;

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::ConstructionLayer;

#[derive(Debug, Clone)]
pub struct BerRecord {
    pub id: ItemId,
    pub name: String,
    pub date_assessed: NaiveDate,
    pub data: Ber, // includes address and eircode
}

#[derive(Debug)]
pub struct BerSummary {
    pub id: ItemId,
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

            -- Editing-layer snapshot (parametric plans + dimensioning) for crash
            -- recovery + exact reopen of IN-PROGRESS work that `ber.data_json`
            -- (finalised geometry only) doesn't hold. Disposable: a version
            -- mismatch / parse failure falls back to rebuild-from-points.
            CREATE TABLE IF NOT EXISTS parametric_snapshot (
                ber_id        TEXT PRIMARY KEY REFERENCES ber(id) ON DELETE CASCADE,
                version       INTEGER NOT NULL,
                storey_idx    INTEGER NOT NULL,
                snapshot_json TEXT NOT NULL
            );

            -- APP-WIDE reusable construction-layer library (NOT tied to a BER):
            -- the build-up editor's 'previously typed layers'. A typed layer is
            -- saved here once and offered in every future assessment. UNIQUE keeps
            -- re-typing the same layer idempotent.
            CREATE TABLE IF NOT EXISTS construction_layer (
                name         TEXT NOT NULL,
                thickness_mm INTEGER NOT NULL,
                lambda       REAL NOT NULL,
                bridged      INTEGER NOT NULL,
                UNIQUE(name, thickness_mm, lambda, bridged)
            );
            ",
        )?;
        Ok(())
    }

    /// Every layer in the app-wide library (the build-up editor's pick list),
    /// ordered by name then thickness.
    pub fn list_construction_layers(&self) -> Result<Vec<ConstructionLayer>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, thickness_mm, lambda, bridged FROM construction_layer
             ORDER BY name, thickness_mm",
        )?;
        let rows = stmt.query_map([], |r| {
            let name: String = r.get(0)?;
            let thickness_mm: i64 = r.get(1)?;
            let lambda: f64 = r.get(2)?;
            let bridged: i64 = r.get(3)?;
            Ok(ConstructionLayer::typed(
                name,
                thickness_mm as u32,
                lambda as f32,
                bridged != 0,
            ))
        })?;
        rows.collect()
    }

    /// Add a typed layer to the app-wide library (idempotent — re-adding an
    /// identical layer is a no-op via the UNIQUE constraint).
    pub fn add_construction_layer(&self, layer: &ConstructionLayer) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO construction_layer (name, thickness_mm, lambda, bridged)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                layer.name(),
                layer.thickness_mm as i64,
                layer.lambda() as f64,
                layer.bridged as i64,
            ],
        )?;
        Ok(())
    }

    /// Upsert the editing-layer snapshot for `ber_id`. `version` lets a loader
    /// reject a stale format; `storey_idx` records which storey the snapshot's
    /// document belongs to (reopen restores it there).
    pub fn save_snapshot(
        &self,
        ber_id: &ItemId,
        version: i64,
        storey_idx: i64,
        snapshot_json: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO parametric_snapshot (ber_id, version, storey_idx, snapshot_json)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(ber_id) DO UPDATE SET
                version = excluded.version,
                storey_idx = excluded.storey_idx,
                snapshot_json = excluded.snapshot_json",
            params![ber_id.to_string(), version, storey_idx, snapshot_json],
        )?;
        Ok(())
    }

    /// Load the editing-layer snapshot for `ber_id`, if any:
    /// `(version, storey_idx, snapshot_json)`.
    pub fn load_snapshot(&self, ber_id: &ItemId) -> Result<Option<(i64, i64, String)>> {
        self.conn
            .query_row(
                "SELECT version, storey_idx, snapshot_json FROM parametric_snapshot WHERE ber_id = ?1",
                params![ber_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
    }

    /// List BERs as lightweight summaries (extracting address and eircode from JSON)
    pub fn list(&self) -> Result<Vec<BerSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, date_assessed, data_json FROM ber ORDER BY date_assessed DESC",
        )?;

        let rows = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let id = id_str.parse().map_err(|e| {
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

    /// Delete a BER by id, including its editing-layer snapshot. The schema's
    /// `ON DELETE CASCADE` only fires with `PRAGMA foreign_keys=ON` (which we
    /// don't set), so the snapshot row is deleted explicitly. Both deletes run
    /// in one transaction so a failure can't leave an orphaned snapshot or a
    /// snapshot-less half-delete.
    pub fn delete(&self, id: &ItemId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM parametric_snapshot WHERE ber_id = ?1",
            params![id.to_string()],
        )?;
        tx.execute("DELETE FROM ber WHERE id = ?1", params![id.to_string()])?;
        tx.commit()
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
    pub fn load(&self, id: &ItemId) -> Result<BerRecord> {
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

    fn make_test_record(id: ItemId) -> BerRecord {
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

        let record = make_test_record(ItemId::default());
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

        let record = make_test_record(ItemId::default());
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

        let id = ItemId::default();
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

        let id = ItemId::default();
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

        let record1 = make_test_record(ItemId::default());
        let record2 = make_test_record(ItemId::default());

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

        let record = make_test_record(ItemId::default());
        repo.upsert(&record).unwrap();
        repo.save_snapshot(&record.id, 1, 0, "{}").unwrap();

        repo.delete(&record.id).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM ber", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 0);

        // The editing-layer snapshot goes with it (no FK pragma → explicit).
        let snap_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM parametric_snapshot", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(snap_count, 0);
    }

    #[test]
    fn load_nonexistent_returns_error() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = BerRepository::new(&conn);
        repo.init_db().unwrap();

        let result = repo.load(&ItemId::default());
        assert!(result.is_err());
    }
}
