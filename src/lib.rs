// src/lib.rs
//
pub mod config;
pub mod controller;
pub mod engine;
pub mod paths;
pub mod ui;

#[cfg(target_os = "android")]
mod android;

pub use ui::app::BerApp;

use std::cell::RefCell;
use std::rc::Rc;

use controller::app_controller::AppController;
use paths::Paths;

/// Shared bootstrap both entry points call: open the DB, build the
/// controller, attach the background writer (falling back to synchronous
/// saves if it can't spawn). Logging is initialised by the caller first.
pub fn bootstrap(paths: &Paths) -> Rc<RefCell<AppController>> {
    // One resolution of the DB file so open_db and the writer can't drift.
    let db = paths.db_file();
    let conn = engine::open_db(&db).expect("Failed to open database");
    let mut controller = AppController::new(conn);
    // Background writer (its own connection, WAL) so idle autosaves don't block
    // the UI on SQLite. If it can't spawn, fall back to synchronous saves.
    match engine::PersistenceWriter::spawn(db) {
        Ok(writer) => controller.attach_writer(writer),
        Err(e) => log::warn!("persistence writer unavailable, saving synchronously: {e}"),
    }
    Rc::new(RefCell::new(controller))
}
