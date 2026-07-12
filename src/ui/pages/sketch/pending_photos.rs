//! Per-kind pending-photo buffers for the Objects entry panel.
//!
//! A photo captured for an object is PENDING until its OK commits the object
//! (`SketchEvent::AddObject` carries the photos; the controller attaches them).
//! These buffers are keyed by the object kind being entered so that switching
//! category/kind PARKS the previous kind's photos instead of dragging them into
//! the next object's commit (the field bug: a fan photo taken without OK ended
//! up attached to the thermostat added next). Switching back restores them —
//! a captured photo is never lost. See `objects_panel.rs`.

use std::path::PathBuf;

use crate::ui::widgets::PhotoQuality;
use ber_model::ber::objects::ObjectKind;

/// Pending object photos, one buffer per object kind being entered.
#[derive(Default)]
pub struct PendingObjectPhotos {
    /// Association list keyed by the NORMALIZED kind (see [`Self::key`]) —
    /// tiny, so no map type needed (`ObjectKind` isn't `Hash` anyway).
    entries: Vec<(ObjectKind, Vec<(PathBuf, PhotoQuality)>)>,
}

impl PendingObjectPhotos {
    /// The buffer key for a kind. All `Other(..)` kinds share ONE buffer
    /// regardless of the typed name: the name is edited live (the kind changes
    /// every keystroke), so keying on it would strand photos under partial
    /// names. Only one "Other" entry is in flight at a time, so sharing is safe.
    fn key(kind: &ObjectKind) -> ObjectKind {
        match kind {
            ObjectKind::Other(_) => ObjectKind::Other(String::new()),
            k => k.clone(),
        }
    }

    /// Store a captured photo under `kind`'s buffer.
    pub fn push(&mut self, kind: &ObjectKind, photo: (PathBuf, PhotoQuality)) {
        let key = Self::key(kind);
        if let Some((_, v)) = self.entries.iter_mut().find(|(k, _)| *k == key) {
            v.push(photo);
        } else {
            self.entries.push((key, vec![photo]));
        }
    }

    /// The photos pending for `kind` (empty if none).
    pub fn for_kind(&self, kind: &ObjectKind) -> &[(PathBuf, PhotoQuality)] {
        let key = Self::key(kind);
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    /// Remove and return `kind`'s pending photos (the OK commit path): only
    /// this kind's buffer is consumed — every other kind's photos stay parked.
    pub fn take(&mut self, kind: &ObjectKind) -> Vec<(PathBuf, PhotoQuality)> {
        let key = Self::key(kind);
        match self.entries.iter().position(|(k, _)| *k == key) {
            Some(i) => self.entries.remove(i).1,
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::objects::{FanType, ObjectKind};

    fn fan() -> ObjectKind {
        ObjectKind::Fan(FanType::Extract)
    }
    fn thermostat() -> ObjectKind {
        ObjectKind::Thermostat
    }

    /// The misattachment regression: a photo captured under kind A must not
    /// surface under kind B after a switch — and must still be there (parked)
    /// when A is re-selected. OK for B takes only B's photos.
    #[test]
    fn photos_are_parked_per_kind() {
        let mut pending = PendingObjectPhotos::default();
        pending.push(&fan(), (PathBuf::from("/tmp/fan.jpg"), PhotoQuality::High));

        // Switch to another kind: its buffer is empty (gates read this).
        assert!(pending.for_kind(&thermostat()).is_empty());
        // OK on the thermostat commits ONLY its own (no) photos…
        assert!(pending.take(&thermostat()).is_empty());
        // …and the fan photo is still parked, shown again on switch-back.
        assert_eq!(pending.for_kind(&fan()).len(), 1);

        // OK on the fan consumes exactly its buffer, once.
        let taken = pending.take(&fan());
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, PathBuf::from("/tmp/fan.jpg"));
        assert!(pending.for_kind(&fan()).is_empty());
    }

    /// "Other" kinds share one buffer regardless of the typed name — the name
    /// is edited keystroke-by-keystroke, so a photo captured mid-typing must
    /// still be found under the final name.
    #[test]
    fn other_kinds_share_one_buffer_across_name_edits() {
        let mut pending = PendingObjectPhotos::default();
        pending.push(
            &ObjectKind::Other("Heat p".into()),
            (PathBuf::from("/tmp/hp.jpg"), PhotoQuality::Low),
        );
        assert_eq!(
            pending
                .for_kind(&ObjectKind::Other("Heat pump".into()))
                .len(),
            1
        );
        assert_eq!(
            pending.take(&ObjectKind::Other("Heat pump".into())).len(),
            1
        );
        assert!(pending
            .for_kind(&ObjectKind::Other(String::new()))
            .is_empty());
    }
}
