// engine/mod.rs
pub mod ber_repository;
pub mod dataloader;

/// Opaque database handle. Callers outside `src/engine/` should hold this type
/// rather than reaching for `rusqlite::Connection` directly — keeps SQL access
/// funneled through `BerRepository`.
pub type Db = rusqlite::Connection;

/// Open an in-memory SQLite database and initialize the BER schema.
pub fn open_db() -> rusqlite::Result<Db> {
    let conn = rusqlite::Connection::open_in_memory()?;
    ber_repository::BerRepository::new(&conn).init_db()?;
    Ok(conn)
}
