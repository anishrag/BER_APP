//! Background SQLite writer — keeps disk writes off the UI thread.
//!
//! The controller builds the (filtered) [`BerRecord`] to save on the UI thread
//! and hands it to [`PersistenceWriter::save`], which queues it for a dedicated
//! thread owning its OWN connection. Bursts are COALESCED — only the latest
//! queued record is written, so a flurry of idle saves collapses to one upsert.
//! On drop the channel closes and we join, so a clean shutdown drains the queue
//! (the last autosave still reaches disk).
//!
//! [`PersistenceWriter::drain`] is the synchronous barrier: it blocks until
//! everything queued before it has been written, so a caller about to act on
//! the database directly (e.g. delete a record) can't race a queued upsert.

use std::sync::mpsc::{self, Sender, SyncSender};
use std::thread::JoinHandle;
use std::time::Duration;

use datatypes::ber::common::ItemId;

use super::ber_repository::{BerRecord, BerRepository};

/// The editing-layer snapshot half of a save (parametric plans + dimensioning),
/// already serialized on the UI thread. `version` lets the loader reject a stale
/// format; `storey_idx` is the storey the document belongs to.
pub struct DocSnapshot {
    pub ber_id: ItemId,
    pub version: i64,
    pub storey_idx: i64,
    pub json: String,
}

/// One save handed to the background writer: the filtered resolved record plus
/// the optional editing snapshot. Written together so they stay in step.
pub struct SaveBundle {
    pub record: BerRecord,
    pub snapshot: Option<DocSnapshot>,
}

/// What flows down the writer's channel: a save to (eventually) write, or a
/// drain barrier — acked once every save queued before it has hit the DB.
enum WriterMsg {
    Save(SaveBundle),
    Drain(SyncSender<()>),
}

pub struct PersistenceWriter {
    tx: Option<Sender<WriterMsg>>,
    handle: Option<JoinHandle<()>>,
}

impl PersistenceWriter {
    /// Spawn a writer thread with its own connection to `path` (WAL — see
    /// [`open_db`](super::open_db)). The connection is opened here so a failure
    /// surfaces to the caller, then moved into the thread (`Connection: Send`).
    pub fn spawn(path: String) -> rusqlite::Result<Self> {
        let conn = super::open_db(&path)?;
        let (tx, rx) = mpsc::channel::<WriterMsg>();
        let handle = std::thread::Builder::new()
            .name("ber-persist".into())
            .spawn(move || {
                let repo = BerRepository::new(&conn);
                while let Ok(first) = rx.recv() {
                    // Coalesce a burst: keep only the most recent bundle, and
                    // collect any drain barriers seen along the way (they're
                    // acked AFTER the write, preserving their guarantee).
                    let mut latest: Option<SaveBundle> = None;
                    let mut drains: Vec<SyncSender<()>> = Vec::new();
                    let mut msg = first;
                    loop {
                        match msg {
                            WriterMsg::Save(bundle) => latest = Some(bundle),
                            WriterMsg::Drain(ack) => drains.push(ack),
                        }
                        match rx.try_recv() {
                            Ok(next) => msg = next,
                            Err(_) => break,
                        }
                    }
                    if let Some(bundle) = latest {
                        if let Err(e) = repo.upsert(&bundle.record) {
                            log::error!("[persist] background upsert failed: {e}");
                        }
                        if let Some(s) = &bundle.snapshot {
                            if let Err(e) =
                                repo.save_snapshot(&s.ber_id, s.version, s.storey_idx, &s.json)
                            {
                                log::error!("[persist] background snapshot save failed: {e}");
                            }
                        }
                    }
                    for ack in drains {
                        // Receiver may have timed out and gone — ignore.
                        let _ = ack.send(());
                    }
                }
            })
            .expect("spawn persistence thread");
        Ok(Self {
            tx: Some(tx),
            handle: Some(handle),
        })
    }

    /// Queue a save bundle and return immediately. The UI thread never blocks on
    /// the disk I/O.
    pub fn save(&self, bundle: SaveBundle) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(WriterMsg::Save(bundle));
        }
    }

    /// Block until every save queued before this call has been written. Used at
    /// workspace-exit so subsequent direct DB operations (delete, list) can't
    /// race a still-queued upsert. Generous timeout: a hung writer logs and
    /// returns rather than freezing the UI forever.
    pub fn drain(&self) {
        let Some(tx) = &self.tx else { return };
        let (ack_tx, ack_rx) = mpsc::sync_channel::<()>(1);
        if tx.send(WriterMsg::Drain(ack_tx)).is_err() {
            // Writer thread gone — its queue is necessarily drained.
            return;
        }
        if ack_rx.recv_timeout(Duration::from_secs(10)).is_err() {
            log::warn!("[persist] drain barrier timed out waiting for the background writer");
        }
    }
}

impl Drop for PersistenceWriter {
    fn drop(&mut self) {
        // Close the channel (so the thread drains its queue and exits), then
        // join — a clean shutdown doesn't lose the last queued write.
        self.tx.take();
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use datatypes::ber::ber_struct::Ber;

    /// `drain` must not return until the queued save is on disk: queue a save,
    /// drain, then assert the row is visible synchronously on a fresh
    /// connection.
    #[test]
    fn drain_blocks_until_queued_save_is_written() {
        let path = std::env::temp_dir().join(format!(
            "ber_drain_test_{}_{}.sqlite",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let path_str = path.to_string_lossy().into_owned();

        let writer = PersistenceWriter::spawn(path_str.clone()).unwrap();
        let ber = Ber::new();
        let id = ber.id;
        writer.save(SaveBundle {
            record: BerRecord {
                id,
                name: "drain test".to_string(),
                date_assessed: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
                data: ber,
            },
            snapshot: None,
        });

        writer.drain();

        // No drop/join yet — the barrier alone must have flushed the write.
        let conn = crate::engine::open_db(&path_str).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM ber WHERE id = ?1",
                rusqlite::params![id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);

        drop(writer);
        let _ = std::fs::remove_file(&path);
        // WAL sidecars.
        let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
        let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    }
}
