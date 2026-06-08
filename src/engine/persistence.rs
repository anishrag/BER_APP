//! Background SQLite writer — keeps disk writes off the UI thread.
//!
//! The controller builds the (filtered) [`BerRecord`] to save on the UI thread
//! and hands it to [`PersistenceWriter::save`], which queues it for a dedicated
//! thread owning its OWN connection. Bursts are COALESCED — only the latest
//! queued record is written, so a flurry of idle saves collapses to one upsert.
//! On drop the channel closes and we join, so a clean shutdown drains the queue
//! (the last autosave still reaches disk).

use std::sync::mpsc::{self, Sender};
use std::thread::JoinHandle;

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

pub struct PersistenceWriter {
    tx: Option<Sender<SaveBundle>>,
    handle: Option<JoinHandle<()>>,
}

impl PersistenceWriter {
    /// Spawn a writer thread with its own connection to `path` (WAL — see
    /// [`open_db`](super::open_db)). The connection is opened here so a failure
    /// surfaces to the caller, then moved into the thread (`Connection: Send`).
    pub fn spawn(path: String) -> rusqlite::Result<Self> {
        let conn = super::open_db(&path)?;
        let (tx, rx) = mpsc::channel::<SaveBundle>();
        let handle = std::thread::Builder::new()
            .name("ber-persist".into())
            .spawn(move || {
                let repo = BerRepository::new(&conn);
                while let Ok(first) = rx.recv() {
                    // Coalesce a burst: keep only the most recent bundle.
                    let mut latest = first;
                    while let Ok(next) = rx.try_recv() {
                        latest = next;
                    }
                    if let Err(e) = repo.upsert(&latest.record) {
                        log::error!("[persist] background upsert failed: {e}");
                    }
                    if let Some(s) = &latest.snapshot {
                        if let Err(e) =
                            repo.save_snapshot(&s.ber_id, s.version, s.storey_idx, &s.json)
                        {
                            log::error!("[persist] background snapshot save failed: {e}");
                        }
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
            let _ = tx.send(bundle);
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
