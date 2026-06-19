//! Dev checkpoint: serialize the in-memory `Ber` to a JSON file so a test plan
//! can be reloaded on startup instead of redrawn every run. NOT used in the
//! normal flow — gated behind the `BER_CHECKPOINT` env var (see `BerApp::new`)
//! and a manual "Save checkpoint" button.
use ber_model::ber::ber_struct::Ber;

/// Default checkpoint file (repo cwd, alongside `bers.db`).
pub const DEFAULT_PATH: &str = "checkpoint.json";

/// Write `ber` to `path` as pretty JSON (captures the FULL in-memory model, not
/// just the finalised geometry the DB keeps).
pub fn save(ber: &Ber, path: &str) -> Result<(), String> {
    let json = serde_json::to_string_pretty(ber).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write {path}: {e}"))
}

/// Load a `Ber` from `path`, or `None` if it's missing/unreadable/invalid.
pub fn load(path: &str) -> Option<Ber> {
    let json = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&json).ok()
}
