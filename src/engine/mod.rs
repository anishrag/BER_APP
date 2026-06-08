// engine/mod.rs
pub mod ber_repository;
pub mod checkpoint;
pub mod dataloader;
pub mod persistence;

pub use persistence::{DocSnapshot, PersistenceWriter, SaveBundle};

use std::path::Path;

/// Opaque database handle. Callers outside `src/engine/` should hold this type
/// rather than reaching for `rusqlite::Connection` directly — keeps SQL access
/// funneled through `BerRepository`.
pub type Db = rusqlite::Connection;

/// Open a SQLite database at `path` and ensure the BER schema exists.
/// Pass `":memory:"` for an ephemeral in-memory database (tests, examples).
///
/// Enables WAL + a busy timeout so the background [`PersistenceWriter`]'s own
/// connection and this one can read/write the same file concurrently without
/// locking each other. Both are no-ops on `:memory:`.
pub fn open_db(path: impl AsRef<Path>) -> rusqlite::Result<Db> {
    let conn = rusqlite::Connection::open(path)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    // WAL returns the resulting mode as a row; read+discard it. Ignored on
    // `:memory:` (which stays in "memory" journal mode).
    let _: rusqlite::Result<String> = conn.query_row("PRAGMA journal_mode=WAL;", [], |r| r.get(0));
    ber_repository::BerRepository::new(&conn).init_db()?;
    Ok(conn)
}
