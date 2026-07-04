//! Dev checkpoint: serialize the in-memory plan to a JSON file so a test plan
//! can be reloaded on startup instead of redrawn every run. NOT used in the
//! normal flow — gated behind the `BER_CHECKPOINT` env var (see `BerApp::new`)
//! and a manual "Save checkpoint" button.
//!
//! The checkpoint carries the resolved [`Ber`] AND the editing-layer `Document`
//! snapshot (the parametric plans + dimensioning), exactly like the DB save's
//! two halves. The snapshot is what preserves the outline's parametric
//! `VertexId`s across a reload — WITHOUT it a reload rebuilds a FRESH plan from
//! the resolved footprint (`from_clean_ring`), re-minting those ids and re-homing
//! every opening (whose anchor keys on a perimeter `VertexId`) onto the wrong
//! wall. Both halves are optional so a legacy bare-`Ber` checkpoint, and a
//! snapshot whose format version the loader rejects, still load — falling back
//! to the resolved-record rebuild.
use ber_model::ber::ber_struct::Ber;
use serde::{Deserialize, Serialize};

/// Default checkpoint file (repo cwd, alongside `bers.db`).
pub const DEFAULT_PATH: &str = "checkpoint.json";

/// On-disk checkpoint wrapper: the resolved `Ber` plus the editing `Document`
/// snapshot (as a JSON string, version-gated like the DB snapshot).
#[derive(Serialize, Deserialize)]
struct Checkpoint {
    ber: Ber,
    #[serde(default)]
    snapshot_version: Option<i64>,
    #[serde(default)]
    document_json: Option<String>,
}

/// A loaded checkpoint: the `Ber`, plus the editing `Document` snapshot
/// `(version, json)` when the file carried one (the caller version-checks it).
pub struct Loaded {
    pub ber: Ber,
    pub snapshot: Option<(i64, String)>,
}

/// Serialize `ber` + the editing `Document` snapshot `(version, json)` into the
/// checkpoint wrapper as pretty JSON. Pass `None` for `snapshot` to serialize a
/// `Ber`-only checkpoint. Shared by [`save`] and the bug-report state dump
/// (`engine::bug_report`) so both capture the exact same state shape.
pub fn to_json(ber: &Ber, snapshot: Option<(i64, &str)>) -> Result<String, String> {
    let cp = Checkpoint {
        ber: ber.clone(),
        snapshot_version: snapshot.map(|(v, _)| v),
        document_json: snapshot.map(|(_, j)| j.to_string()),
    };
    serde_json::to_string_pretty(&cp).map_err(|e| format!("serialize: {e}"))
}

/// Write `ber` + the editing `Document` snapshot `(version, json)` to `path` as
/// pretty JSON. Pass `None` for `snapshot` to write a `Ber`-only checkpoint.
pub fn save(ber: &Ber, snapshot: Option<(i64, &str)>, path: &str) -> Result<(), String> {
    let json = to_json(ber, snapshot)?;
    std::fs::write(path, json).map_err(|e| format!("write {path}: {e}"))
}

/// Load a checkpoint from `path`, or `None` if it's missing/unreadable/invalid.
/// Reads the wrapper format first; falls back to a LEGACY bare-`Ber` checkpoint
/// (written before the snapshot was added) so old files still open.
pub fn load(path: &str) -> Option<Loaded> {
    let json = std::fs::read_to_string(path).ok()?;
    if let Ok(cp) = serde_json::from_str::<Checkpoint>(&json) {
        let snapshot = cp.snapshot_version.zip(cp.document_json);
        return Some(Loaded {
            ber: cp.ber,
            snapshot,
        });
    }
    let ber = serde_json::from_str::<Ber>(&json).ok()?;
    Some(Loaded {
        ber,
        snapshot: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_path(tag: &str) -> String {
        let mut p = std::env::temp_dir();
        p.push(format!("ber_checkpoint_{tag}_{}.json", std::process::id()));
        p.to_string_lossy().into_owned()
    }

    /// A checkpoint round-trips the `Ber` AND the editing Document snapshot
    /// `(version, json)` — the snapshot is what preserves the parametric plan
    /// (and its VertexIds) across a reload.
    #[test]
    fn round_trips_ber_and_document_snapshot() {
        let path = tmp_path("with_snapshot");
        let mut ber = Ber::default();
        ber.storeys.push(Default::default());
        save(&ber, Some((3, "{\"doc\":true}")), &path).unwrap();
        let loaded = load(&path).expect("loads");
        assert_eq!(loaded.ber.storeys.len(), 1, "ber round-trips");
        assert_eq!(
            loaded.snapshot,
            Some((3, "{\"doc\":true}".to_string())),
            "the Document snapshot round-trips"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// A legacy bare-`Ber` checkpoint (written before the snapshot was added)
    /// still loads, with no snapshot — the reload falls back to the rebuild.
    #[test]
    fn loads_legacy_bare_ber_checkpoint() {
        let path = tmp_path("legacy");
        let ber = Ber::default();
        std::fs::write(&path, serde_json::to_string(&ber).unwrap()).unwrap();
        let loaded = load(&path).expect("legacy file still loads");
        assert!(
            loaded.snapshot.is_none(),
            "no snapshot in a legacy checkpoint"
        );
        let _ = std::fs::remove_file(&path);
    }
}
