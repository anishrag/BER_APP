//! External photos: per-elevation pictures of the dwelling (Front / Right /
//! Left / Back). They live on `Ber.photos` (the dwelling-level photo list) as
//! entries with `side == Some(_)`, so they serialize with the record and the
//! existing persist/autosave path writes them — no dedicated SQL hook.
//!
//! The "External photos" Mapping checklist row is complete only when ALL FOUR
//! sides have a photo (a dwelling-level gate, like the living area).
//!
//! TODO(android-camera): the actual capture is a DESKTOP FILE PICK today (the
//! UI layer opens an `rfd::FileDialog` and hands the controller the chosen
//! path). Real on-device camera capture on Android is a follow-up: the
//! controller + model are camera-agnostic (they only ever see a `file_path`
//! String), so a camera path can replace the file-pick by emitting the same
//! `AddExternalPhoto { side, path }` event with a captured-image path.
use super::*;
use ber_model::ber::common::{ExternalSide, Photo};

/// One external side's current photo for the UI panel — its stored path and
/// optional description. The side itself is carried alongside in
/// [`AppController::external_photos`].
#[derive(Debug, Clone)]
pub struct PhotoView {
    pub file_path: String,
    pub description: Option<String>,
}

impl AppController {
    /// Attach (or replace) the external photo for `side`. Snapshots for undo,
    /// REPLACES any existing photo of that side in `Ber.photos` (re-shooting a
    /// side overwrites it) — else pushes a new external `Photo`. `mark_dirty`
    /// re-derives; the snapshot is pushed so the add/replace undoes.
    pub fn add_external_photo(&mut self, side: ExternalSide, file_path: String) {
        if self.current_record.is_none() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        {
            let record = self.current_record.as_mut().unwrap();
            let photos = &mut record.data.photos;
            if let Some(existing) = photos.iter_mut().find(|p| p.side == Some(side)) {
                existing.file_path = file_path;
                existing.description = None;
            } else {
                photos.push(Photo {
                    file_path,
                    side: Some(side),
                    ..Default::default()
                });
            }
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
