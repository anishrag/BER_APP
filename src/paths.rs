//! Where the app keeps its mutable files.
//!
//! Computed once by the platform entry point (`main` / `android_main`) and
//! passed to [`crate::bootstrap`]; nothing below the entry points resolves
//! paths.

use std::path::PathBuf;

pub struct Paths {
    data_dir: PathBuf,
}

impl Paths {
    /// Wrap a platform-provided directory (Android: `internal_data_path()`).
    pub fn in_dir(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    /// Desktop: the current working directory — `./bers.db` next to wherever
    /// the app is launched, unchanged from the pre-port behaviour (deliberate:
    /// dev data lives in the repo root).
    pub fn desktop() -> Self {
        Self {
            data_dir: PathBuf::from("."),
        }
    }

    /// The SQLite database file.
    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("bers.db")
    }

    /// Directory holding attached/captured photo files (created on first save).
    pub fn photos_dir(&self) -> PathBuf {
        self.data_dir.join("photos")
    }

    /// Optional cloud-sync config (surveyor API base URL + key). Absent means the
    /// app runs fully offline; see [`crate::engine::sync::SyncConfig`].
    pub fn sync_config_file(&self) -> PathBuf {
        self.data_dir.join("sync.json")
    }

    /// Local-first queue of captured bug reports (one subdir per report: an
    /// optional `audio.m4a`, a `state.json` dump, and `meta.json`). The
    /// background uploader drains it to the intake backend when WiFi allows;
    /// see [`crate::engine::bug_report`].
    pub fn bug_reports_dir(&self) -> PathBuf {
        self.data_dir.join("bug-reports")
    }
}
