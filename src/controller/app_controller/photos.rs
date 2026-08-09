//! External photos: per-elevation pictures of the dwelling (Front / Right /
//! Left / Back). They live on `Ber.photos` (the dwelling-level photo list) as
//! entries with `side == Some(_)`, so they serialize with the record and the
//! existing persist/autosave path writes them — no dedicated SQL hook.
//!
//! The "External photos" Mapping checklist row is complete once the REQUIRED
//! sides (Front + Back) have a photo; Left + Right are optional (a dwelling-level
//! gate, like the living area).
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
    pub id: ber_model::ber::common::ItemId,
    pub file_path: String,
    pub thumbnail_path: Option<String>,
    pub description: Option<String>,
}

impl AppController {
    /// Attach an external photo for `side` from a captured/picked `source`
    /// image — APPENDED, a side can hold any number of shots. Recompresses +
    /// copies into the photo store via
    /// [`store_photo_file`](Self::store_photo_file) (`quality` is the capture
    /// tier) and tags the stored `Evidence` with `side`. Snapshots for undo;
    /// `mark_dirty` re-derives. No-op if there's no record or the store I/O
    /// fails.
    pub fn add_external_photo(
        &mut self,
        side: ExternalSide,
        source: &std::path::Path,
        quality: PhotoQuality,
    ) {
        if self.current_record.is_none() {
            return;
        }
        let Some(mut photo) = self.ingest_photo(source, quality) else {
            return;
        };
        photo.side = Some(side);
        let undo_snap = self.current_history_snapshot();
        {
            let data = &mut self.current_record.as_mut().unwrap().data;
            data.photos.push(photo);
            // Capturing a deferred side's photo CLEARS the deferral (defer =
            // postpone to the end, so the capture fulfils it) — same undo
            // snapshot, so undo restores both the photo slot and the flag.
            data.set_photo_deferred(side, false);
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// Replace `side`'s external photo with `jpeg` — the ANNOTATED image the UI
    /// re-encoded with paint strokes burned in at image resolution (see
    /// `ui::widgets::photo_annotate`). Writes the bytes into the photo store as
    /// a NEW stored file (fresh id, the same store the capture path uses) and
    /// regenerates the thumbnail, then swaps it into `Ber.photos` keeping the
    /// side + description. Snapshots for undo (the superseded file stays on
    /// disk, so undo still displays it). No-op when there's no record, the side
    /// has no photo (annotation edits an existing shot), or store I/O fails —
    /// the prior photo then stands.
    pub fn annotate_external_photo(&mut self, target: ber_model::ber::common::ItemId, jpeg: &[u8]) {
        let Some((side, description)) = self.current_record.as_ref().and_then(|r| {
            r.data
                .photos
                .iter()
                .find(|p| p.id == target && p.side.is_some())
                .map(|p| (p.side, p.description.clone()))
        }) else {
            return;
        };
        if std::fs::create_dir_all(&self.photos_dir).is_err() {
            return;
        }
        // Canonicalize to an ABSOLUTE dir so the stored path loads as a
        // `file://` image regardless of the process's cwd (as `ingest_photo`).
        let dir =
            std::fs::canonicalize(&self.photos_dir).unwrap_or_else(|_| self.photos_dir.clone());
        let id = ber_model::ber::common::ItemId::default();
        let dest = dir.join(format!("{}.jpg", id.0));
        if std::fs::write(&dest, jpeg).is_err() {
            return;
        }
        let thumb_dest = dir.join(format!("{}_thumb.jpg", id.0));
        let thumbnail_path = Self::write_thumbnail(&dest, &thumb_dest)
            .then(|| thumb_dest.to_string_lossy().into_owned());
        let photo = ber_model::ber::common::Evidence {
            id,
            file_path: dest.to_string_lossy().into_owned(),
            thumbnail_path,
            description,
            side,
            content_type: None,
        };
        let undo_snap = self.current_history_snapshot();
        {
            let photos = &mut self.current_record.as_mut().unwrap().data.photos;
            if let Some(slot) = photos.iter_mut().find(|p| p.id == target) {
                *slot = photo;
            }
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// Rotate `side`'s external photo a quarter turn (`clockwise`, else
    /// anti-clockwise) so the assessor can set which way is up / match it to the
    /// plan. Bakes the turn into a NEW stored file (fresh id) — like
    /// [`annotate_external_photo`](Self::annotate_external_photo) — so every site
    /// that shows the photo (review, annotator, report) sees it rotated and undo
    /// (which restores the prior entry, still on disk) reverts it. Regenerates
    /// the thumbnail. No-op with no record, no photo for the side, or on
    /// decode/store failure (the prior photo then stands).
    pub fn rotate_external_photo(
        &mut self,
        target: ber_model::ber::common::ItemId,
        clockwise: bool,
    ) {
        let Some((side, src_path, description)) = self.current_record.as_ref().and_then(|r| {
            r.data
                .photos
                .iter()
                .find(|p| p.id == target && p.side.is_some())
                .map(|p| (p.side, p.file_path.clone(), p.description.clone()))
        }) else {
            return;
        };
        if std::fs::create_dir_all(&self.photos_dir).is_err() {
            return;
        }
        // Decode the stored (already-upright) image and turn it a quarter.
        let Ok(reader) = image::ImageReader::open(&src_path).and_then(|r| r.with_guessed_format())
        else {
            return;
        };
        let Ok(img) = reader.decode() else {
            return;
        };
        let rotated = if clockwise {
            img.rotate90()
        } else {
            img.rotate270()
        };
        // Absolute dir so the stored path loads as `file://` regardless of cwd
        // (as `ingest_photo` / `annotate_external_photo`).
        let dir =
            std::fs::canonicalize(&self.photos_dir).unwrap_or_else(|_| self.photos_dir.clone());
        let id = ber_model::ber::common::ItemId::default();
        let dest = dir.join(format!("{}.jpg", id.0));
        if rotated.to_rgb8().save(&dest).is_err() {
            return;
        }
        let thumb_dest = dir.join(format!("{}_thumb.jpg", id.0));
        let thumbnail_path = Self::write_thumbnail(&dest, &thumb_dest)
            .then(|| thumb_dest.to_string_lossy().into_owned());
        let photo = ber_model::ber::common::Evidence {
            id,
            file_path: dest.to_string_lossy().into_owned(),
            thumbnail_path,
            description,
            side,
            content_type: None,
        };
        let undo_snap = self.current_history_snapshot();
        {
            let photos = &mut self.current_record.as_mut().unwrap().data.photos;
            if let Some(slot) = photos.iter_mut().find(|p| p.id == target) {
                *slot = photo;
            }
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// Remove one external photo by its id. Snapshots for undo like
    /// [`add_external_photo`](Self::add_external_photo).
    /// Attach an EXTRA photo — the general bucket for shots with no other
    /// home (no side tag, no object, no surface): appended to `Ber::photos`
    /// with `side: None`. Same store/undo mechanics as the external sides;
    /// removal shares [`remove_external_photo`] (an id-keyed retain).
    pub fn add_extra_photo(&mut self, source: &std::path::Path, quality: PhotoQuality) {
        if self.current_record.is_none() {
            return;
        }
        let Some(photo) = self.ingest_photo(source, quality) else {
            return;
        };
        let undo_snap = self.current_history_snapshot();
        {
            let record = self.current_record.as_mut().unwrap();
            record.data.photos.push(photo);
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// The untagged EXTRA photos (`side: None`) for the floating panel.
    pub fn extra_photos_view(&self) -> Vec<PhotoView> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        record
            .data
            .photos
            .iter()
            .filter(|p| p.side.is_none())
            .map(|p| PhotoView {
                id: p.id,
                file_path: p.file_path.clone(),
                thumbnail_path: p.thumbnail_path.clone(),
                description: p.description.clone(),
            })
            .collect()
    }

    pub fn remove_external_photo(&mut self, target: ber_model::ber::common::ItemId) {
        if self.current_record.is_none() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        {
            let record = self.current_record.as_mut().unwrap();
            record.data.photos.retain(|p| p.id != target);
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// The four external sides in [`ExternalSide::ALL`] order, each paired
    /// with ALL its photos (capture order; possibly none) — the UI panel's
    /// backing data.
    pub fn external_photos(&self) -> Vec<(ExternalSide, Vec<PhotoView>)> {
        let photos = self
            .current_record
            .as_ref()
            .map(|r| r.data.photos.as_slice())
            .unwrap_or(&[]);
        ExternalSide::ALL
            .iter()
            .map(|&side| {
                let views = photos
                    .iter()
                    .filter(|p| p.side == Some(side))
                    .map(|p| PhotoView {
                        id: p.id,
                        file_path: p.file_path.clone(),
                        thumbnail_path: p.thumbnail_path.clone(),
                        description: p.description.clone(),
                    })
                    .collect();
                (side, views)
            })
            .collect()
    }

    /// Whether the REQUIRED external sides have a photo — the dwelling-level gate
    /// for the "External photos" Mapping checklist row. Front + Back are
    /// mandatory; Left + Right are OPTIONAL (a mid-terrace or otherwise
    /// inaccessible dwelling often has no reachable side elevations). A
    /// DEFERRED side counts as set — the Final phase re-collects it before
    /// "BER complete" (Front can't be deferred, so the front photo is always
    /// really required here).
    pub fn external_photos_complete(&self) -> bool {
        // Wraps the reason core (`Ber::missing_external_photo_sides`) the
        // checklist row's bubble lists, so the gate and its explanation can't
        // disagree.
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.missing_external_photo_sides().is_empty())
    }

    /// Toggle `side`'s photo DEFERRAL (the panel's per-side "Defer" button):
    /// deferred counts as set for the Mapping gates, and the Final phase
    /// re-prompts for the shot before "BER complete". No-op for a
    /// non-deferrable side (Front — see [`ExternalSide::photo_deferrable`])
    /// or with no record. Snapshots for undo like the other photo mutations.
    pub fn toggle_defer_external_photo(&mut self, side: ExternalSide) {
        if !side.photo_deferrable() || self.current_record.is_none() {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        {
            let data = &mut self.current_record.as_mut().unwrap().data;
            let now_deferred = !data.photo_deferred(side);
            data.set_photo_deferred(side, now_deferred);
        }
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// The sides currently flagged DEFERRED (normalized — never contains
    /// Front), for the panel's per-side Defer toggles.
    pub fn deferred_photo_sides(&self) -> Vec<ExternalSide> {
        let Some(record) = self.current_record.as_ref() else {
            return vec![];
        };
        ExternalSide::ALL
            .iter()
            .copied()
            .filter(|&s| record.data.photo_deferred(s))
            .collect()
    }

    /// The deferred sides still LACKING a photo — what the end-of-assessment
    /// prompt lists and what blocks "BER complete"
    /// ([`Ber::deferred_photos_pending`](ber_model::ber::ber_struct::Ber::deferred_photos_pending)).
    pub fn deferred_photos_pending(&self) -> Vec<ExternalSide> {
        self.current_record
            .as_ref()
            .map(|r| r.data.deferred_photos_pending())
            .unwrap_or_default()
    }

    /// Backfill the missing THUMBNAIL for `side`'s stored external photo, off
    /// the UI thread. The panel previews ONLY thumbnails — texturing a
    /// full-res ~12 MP camera file costs ~48 MB, OOM territory on the tablet
    /// — so a photo without one (a record from before thumbnails existed, or
    /// a failed generation) shows a placeholder that requests this every
    /// frame. Guards: at most ONE attempt per photo per session (an
    /// undecodable file must not retry each frame), and a no-op while any
    /// photo job is in flight — a fresh capture's own background job is
    /// about to deliver its thumbnail anyway. The result lands through the
    /// shared photo-job queue
    /// ([`apply_finished_photos`](Self::apply_finished_photos)) with
    /// `provisional == file_path`, which marks it as a backfill there: the
    /// photo's file already stands and must survive the orphan cleanup.
    pub fn ensure_external_photo_thumbnail(&mut self, side: ExternalSide) {
        if self.photos_processing() {
            return;
        }
        let Some((id, file_path)) = self.current_record.as_ref().and_then(|r| {
            r.data
                .photos
                .iter()
                .find(|p| {
                    p.side == Some(side)
                        && p.thumbnail_path.is_none()
                        && !self.thumbnail_backfill_attempted.contains(&p.id)
                })
                .map(|p| (p.id, p.file_path.clone()))
        }) else {
            return;
        };
        if !self.thumbnail_backfill_attempted.insert(id) {
            return;
        }
        if std::fs::create_dir_all(&self.photos_dir).is_err() {
            return;
        }
        // Absolute dir so the stored thumbnail path loads as `file://`
        // regardless of cwd (as `ingest_photo`).
        let dir =
            std::fs::canonicalize(&self.photos_dir).unwrap_or_else(|_| self.photos_dir.clone());
        let thumb_dest = dir.join(format!("{}_thumb.jpg", id.0));
        let results = self.photo_results.clone();
        let inflight = self.photo_jobs_inflight.clone();
        inflight.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        std::thread::spawn(move || {
            let outcome = Self::write_thumbnail(std::path::Path::new(&file_path), &thumb_dest)
                .then(|| {
                    (
                        file_path.clone(),
                        Some(thumb_dest.to_string_lossy().into_owned()),
                    )
                });
            if let Ok(mut q) = results.lock() {
                q.push(super::structure::PhotoJobResult {
                    id,
                    outcome,
                    provisional: file_path,
                });
            }
            inflight.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        });
    }
}
