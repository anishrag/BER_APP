// engine/mod.rs
pub mod ber_repository;
pub mod checkpoint;
pub mod dataloader;

use std::path::Path;

/// Opaque database handle. Callers outside `src/engine/` should hold this type
/// rather than reaching for `rusqlite::Connection` directly — keeps SQL access
/// funneled through `BerRepository`.
pub type Db = rusqlite::Connection;

/// Open a SQLite database at `path` and ensure the BER schema exists.
/// Pass `":memory:"` for an ephemeral in-memory database (tests, examples).
pub fn open_db(path: impl AsRef<Path>) -> rusqlite::Result<Db> {
    let conn = rusqlite::Connection::open(path)?;
    ber_repository::BerRepository::new(&conn).init_db()?;
    Ok(conn)
}
