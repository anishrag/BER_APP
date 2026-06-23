//! MODULAR photo-attach button — reused from every site that collects photos
//! (construction build-ups, surfaces, openings, …).
//!
//! - **Desktop**: opens a native "choose image" dialog (`rfd`) and returns the
//!   chosen file path; the caller's controller copies it into the photo store.
//! - **Android**: opens the in-app camera with flash + a quality whose DEFAULT
//!   the call site supplies via [`PhotoConfig`]. The native CameraX/JNI capture
//!   is a scaffolded seam (`capture` below) — wired on-device later; the rest of
//!   the system (storage, attachment, UI) is complete and platform-agnostic.
//!
//! Each call site renders [`photo_button`], reads the returned source path, and
//! emits its own "attach photo" event — the storage + attachment live in the
//! controller, so this widget stays a thin, reusable trigger.

use std::path::PathBuf;

/// Capture quality for the Android camera. Its DEFAULT is chosen per call site
/// (e.g. a build-up close-up wants higher quality than a context shot).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhotoQuality {
    Low,
    Medium,
    High,
    Full,
}

/// Per-call-site photo configuration. `default_quality` seeds the Android
/// camera's quality control (the assessor can still change it on the camera
/// screen, along with the flash toggle).
#[derive(Debug, Clone, Copy)]
pub struct PhotoConfig {
    pub default_quality: PhotoQuality,
}

/// A reusable "add photo" button. Returns `Some(path)` on the frame a photo is
/// obtained (a chosen file on desktop, a captured file on Android), else `None`.
/// The caller copies/attaches the file (via its controller); this only triggers
/// the platform capture.
pub fn photo_button(ui: &mut egui::Ui, enabled: bool, config: PhotoConfig) -> Option<PathBuf> {
    let clicked = ui
        .add_enabled(enabled, egui::Button::new("📷"))
        .on_hover_text("Add photo")
        .clicked();
    if clicked {
        capture(config)
    } else {
        None
    }
}

/// Desktop: a native file-open dialog filtered to images.
#[cfg(not(target_os = "android"))]
fn capture(_config: PhotoConfig) -> Option<PathBuf> {
    rfd::FileDialog::new()
        .add_filter("Image", &["jpg", "jpeg", "png", "heic", "webp", "bmp"])
        .set_title("Add photo")
        .pick_file()
}

/// Android: the in-app camera (flash + `config.default_quality`).
///
/// SEAM — the native CameraX/JNI capture isn't wired yet (it needs an on-device
/// build this environment can't produce). When implemented it launches the
/// camera, returning the captured file's path. For now it logs and returns
/// `None` so the rest of the photo system builds and runs.
#[cfg(target_os = "android")]
fn capture(config: PhotoConfig) -> Option<PathBuf> {
    log::warn!(
        "android camera capture not yet wired (default quality {:?}, flash supported)",
        config.default_quality
    );
    None
}
