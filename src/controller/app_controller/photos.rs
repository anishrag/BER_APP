//! External photos: per-elevation pictures of the dwelling (Front / Right /
//! Left / Back). They live on `Ber.photos` (the dwelling-level photo list) as
//! entries with `side == Some(_)`, so they serialize with the record and the
//! existing persist/autosave path writes them — no dedicated SQL hook.
//!
//! The "External photos" Mapping checklist row is complete only when ALL FOUR
//! sides have a photo (a dwelling-level gate, like the living area).
//!
//! Capture is the SHARED photo module: the UI's `photo_button` widget hands the
//! controller a captured/picked file path (desktop file dialog / Android camera
//! — the widget owns that platform split), and this layer recompresses + stores
//! it through [`store_photo_file`](AppController::store_photo_file), the same
//! modular core the Structure-phase construction photos use. The controller +
//! model stay camera/file-dialog agnostic: they only ever see a source `Path`.
use super::*;
use ber_model::ber::common::ExternalSide;

/// One external side's current photo for the UI panel — its stored full-res
/// path plus the lightweight thumbnail (for the panel preview) and optional
/// description. The side itself is carried alongside in
/// [`AppController::external_photos`].
#[derive(Debug, Clone)]
pub struct PhotoView {
    pub file_path: String,
    pub thumbnail_path: Option<String>,
    pub description: Option<String>,
}

impl AppController {
    /// Attach (or replace) the external photo for `side` from a captured/picked
    /// `source` image. Recompresses + copies into the photo store via
    /// [`store_photo_file`](Self::store_photo_file) (`quality` is the capture
    /// tier), tags the stored `Photo` with `side`, and REPLACES any existing
    /// photo of that side in `Ber.photos` (re-shooting overwrites) — else pushes
    /// a new external `Photo`. Snapshots for undo; `mark_dirty` re-derives.
    /// No-op if there's no record or the store I/O fails.
    pub fn add_external_photo(
        &mut self,
        side: ExternalSide,
        source: &std::path::Path,
        quality: PhotoQuality,
    ) {
        if self.current_record.is_none() {
            return;
        }
        let Some(mut photo) = self.store_photo_file(source, quality) else {
            return;
        };
        photo.side = Some(side);
        let undo_snap = self.current_history_snapshot();
        {
            let photos = &mut self.current_record.as_mut().unwrap().data.photos;
            // Re-shooting a side overwrites the prior photo for that side.
            photos.retain(|p| p.side != Some(side));
            photos.push(photo);
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// Remove the external photo for `side`, if one is attached. Snapshots for
    /// undo like [`add_external_photo`](Self::add_external_photo).
    pub fn remove_external_photo(&mut self, side: ExternalSide) {
        if self.current_record.is_none() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        {
            let record = self.current_record.as_mut().unwrap();
            record.data.photos.retain(|p| p.side != Some(side));
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// The four external sides in [`ExternalSide::ALL`] order, each paired with
    /// its current photo (or `None`) — the UI panel's backing data.
    pub fn external_photos(&self) -> Vec<(ExternalSide, Option<PhotoView>)> {
        let photos = self
            .current_record
            .as_ref()
            .map(|r| r.data.photos.as_slice())
            .unwrap_or(&[]);
        ExternalSide::ALL
            .iter()
            .map(|&side| {
                let view = photos
                    .iter()
                    .find(|p| p.side == Some(side))
                    .map(|p| PhotoView {
                        file_path: p.file_path.clone(),
                        thumbnail_path: p.thumbnail_path.clone(),
                        description: p.description.clone(),
                    });
                (side, view)
            })
            .collect()
    }

    /// Whether ALL FOUR external sides have a photo — the dwelling-level gate
    /// for the "External photos" Mapping checklist row.
    pub fn external_photos_complete(&self) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        let photos = &record.data.photos;
        ExternalSide::ALL
            .iter()
            .all(|&side| photos.iter().any(|p| p.side == Some(side)))
    }
}
