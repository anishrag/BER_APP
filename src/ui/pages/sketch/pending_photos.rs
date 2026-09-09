//! Pending state for the Objects entry panel: PER-ROOM, PER-KIND photo
//! buffers, plus the dismissal-commit builder.
//!
//! A photo captured for an object is PENDING until a commit attaches it
//! (`SketchEvent::AddObject` carries the photos; the controller ingests them).
//! Buffers are keyed by `(room, kind)` so that
//! - switching category/kind PARKS the previous kind's photos instead of
//!   dragging them into the next object's commit (field bug 1: a fan photo
//!   taken without OK ended up on the thermostat added next), and
//! - a photo parked in one room can never resurface in ANOTHER room's commit
//!   of the same kind (field bug 2: a fixture photo from one room rode along
//!   on another room's fixture commit).
//!
//! `Other(name)` kinds are keyed by their FULL name; the panel migrates the
//! active buffer as the name is edited ([`PendingObjectPhotos::rename_other`]),
//! so a photo taken mid-typing follows the rename while a DIFFERENT parked
//! "Other" (e.g. `Other("bath")` vs `Other("shower")`) keeps its own photos.
//!
//! [`take_dismiss_commits`] turns a dismissal (tap outside the box / Done)
//! into the same commits OK would have made: a non-empty in-flight entry is
//! committed, parked photo buffers for the room are committed with it, and an
//! empty entry dismisses silently. A captured photo is never lost.

use std::path::PathBuf;

use crate::ui::widgets::PhotoQuality;
use ber_model::ber::common::ItemId;
use ber_model::ber::objects::ObjectKind;

/// Pending object photos: one buffer per `(room, kind)` being entered.
#[derive(Default)]
pub struct PendingObjectPhotos {
    /// Association list — tiny, so no map type needed (`ObjectKind` isn't
    /// `Hash` anyway).
    entries: Vec<((ItemId, ObjectKind), Vec<(PathBuf, PhotoQuality)>)>,
}

impl PendingObjectPhotos {
    /// Store a captured photo under `(room, kind)`.
    pub fn push(&mut self, room: ItemId, kind: &ObjectKind, photo: (PathBuf, PhotoQuality)) {
        if let Some((_, v)) = self
            .entries
            .iter_mut()
            .find(|((r, k), _)| *r == room && k == kind)
        {
            v.push(photo);
        } else {
            self.entries.push(((room, kind.clone()), vec![photo]));
        }
    }

    /// The photos pending for `(room, kind)` (empty if none).
    pub fn for_kind(&self, room: ItemId, kind: &ObjectKind) -> &[(PathBuf, PhotoQuality)] {
        self.entries
            .iter()
            .find(|((r, k), _)| *r == room && k == kind)
            .map(|(_, v)| v.as_slice())
            .unwrap_or(&[])
    }

    /// Remove and return `(room, kind)`'s pending photos (the OK commit path):
    /// only this buffer is consumed — every other kind's / room's photos stay
    /// parked.
    pub fn take(&mut self, room: ItemId, kind: &ObjectKind) -> Vec<(PathBuf, PhotoQuality)> {
        match self
            .entries
            .iter()
            .position(|((r, k), _)| *r == room && k == kind)
        {
            Some(i) => self.entries.remove(i).1,
            None => Vec::new(),
        }
    }

    /// Drain EVERY buffer parked for `room` (the room-dismissal flush — each
    /// non-empty buffer becomes its own committed object so nothing strands
    /// when the room closes).
    pub fn take_room(&mut self, room: ItemId) -> Vec<(ObjectKind, Vec<(PathBuf, PhotoQuality)>)> {
        let mut taken = Vec::new();
        let mut i = 0;
        while i < self.entries.len() {
            if self.entries[i].0 .0 == room {
                let ((_, kind), photos) = self.entries.remove(i);
                taken.push((kind, photos));
            } else {
                i += 1;
            }
        }
        taken
    }

    /// The "Other" name is edited LIVE (the kind changes every keystroke):
    /// migrate the active buffer from `Other(old_name)` to `Other(new_name)`
    /// in `room`, merging into an existing buffer under the new name (so
    /// retyping a previously parked name restores its photos). Photos follow a
    /// rename — a different parked name keeps its own buffer.
    pub fn rename_other(&mut self, room: ItemId, old_name: &str, new_name: &str) {
        if old_name == new_name {
            return;
        }
        let old_key = ObjectKind::Other(old_name.to_string());
        let Some(i) = self
            .entries
            .iter()
            .position(|((r, k), _)| *r == room && *k == old_key)
        else {
            return;
        };
        let (_, photos) = self.entries.remove(i);
        let new_kind = ObjectKind::Other(new_name.to_string());
        for photo in photos {
            self.push(room, &new_kind, photo);
        }
    }
}

/// One object commit produced by a dismissal (see [`take_dismiss_commits`]).
pub struct EntryCommit {
    pub kind: ObjectKind,
    pub count: u32,
    pub note: Option<String>,
    pub kwh: Option<f32>,
    pub area_m2: Option<f32>,
    /// PV pin fields (photovoltaic in-flight entries only).
    pub panel_wp: Option<f32>,
    pub roof_faces: Option<ber_model::ber::objects::Compass8>,
    pub overshading: Option<ber_model::ber::fenestration::Overshading>,
    pub photos: Vec<(PathBuf, PhotoQuality)>,
}

/// Turn a DISMISSAL of the entry panel (a tap outside the box, the room's
/// Done) into the commits OK would have made — the field rule: "if it's
/// empty, dismissing silently is fine; if anything was entered, treat it as
/// pressing OK".
///
/// - The IN-FLIGHT entry (`selected_kind` + count/note + its pending photos)
///   commits when anything was entered — a count, a note, a photo, or an
///   "Other" name (a typed name is data). It is withheld if its mandatory
///   photo is missing, or its required NAME (boiler / central unit) or kWh
///   (battery) wasn't given (OK would have been disabled; the text is
///   dropped, exactly as the disabled OK would have dropped it).
/// - Every OTHER buffer parked for this room (photos taken for a kind, then
///   the kind was switched) commits as its own object (count 1) — a
///   deliberate capture must not strand, nor leak into another room, when
///   the room closes.
///
/// Pure over the passed state so it's unit-testable; the caller clears the
/// entry widgets and feeds each commit to `AppController::add_object`
/// (mirroring the OK button in `objects_panel.rs`).
#[allow(clippy::too_many_arguments)]
pub fn take_dismiss_commits(
    room: ItemId,
    selected_kind: Option<&ObjectKind>,
    count_input: &str,
    note_input: &str,
    kwh_input: &str,
    area_input: &str,
    // PV entry state (photovoltaic only): the Wp text and the effective
    // faces / overshading the panel would have committed.
    wp_input: &str,
    roof_faces: Option<ber_model::ber::objects::Compass8>,
    overshading: Option<ber_model::ber::fenestration::Overshading>,
    photo_required: impl Fn(&ObjectKind) -> bool,
    pending: &mut PendingObjectPhotos,
) -> Vec<EntryCommit> {
    let mut commits = Vec::new();
    // The in-flight entry first (it owns the typed count/note).
    if let Some(kind) = selected_kind {
        let kind_ready = !matches!(kind, ObjectKind::Other(s) if s.trim().is_empty());
        if kind_ready {
            let count_txt = count_input.trim();
            let note_txt = note_input.trim();
            let kwh = kwh_input.trim().parse::<f32>().ok();
            let area = area_input.trim().parse::<f32>().ok();
            let have_photo = !pending.for_kind(room, kind).is_empty();
            let entered = !count_txt.is_empty()
                || !note_txt.is_empty()
                || have_photo
                || matches!(kind, ObjectKind::Other(_));
            let gates_met = (!photo_required(kind) || have_photo)
                && (!kind.name_required() || !note_txt.is_empty())
                && (!kind.kwh_required() || kwh.is_some())
                && (!kind.area_required() || area.is_some());
            if entered && gates_met {
                let pv = kind.is_pv();
                commits.push(EntryCommit {
                    kind: kind.clone(),
                    count: count_txt.parse::<u32>().unwrap_or(1),
                    note: (!note_txt.is_empty()).then(|| note_txt.to_string()),
                    kwh: if kind.kwh_required() { kwh } else { None },
                    area_m2: if kind.area_required() { area } else { None },
                    panel_wp: if pv {
                        wp_input.trim().parse::<f32>().ok()
                    } else {
                        None
                    },
                    roof_faces: if pv { roof_faces } else { None },
                    overshading: if pv { overshading } else { None },
                    photos: pending.take(room, kind),
                });
            }
        }
    }
    // Then every buffer still parked for this room.
    for (kind, photos) in pending.take_room(room) {
        commits.push(EntryCommit {
            kind,
            count: 1,
            note: None,
            kwh: None,
            area_m2: None,
            panel_wp: None,
            roof_faces: None,
            overshading: None,
            photos,
        });
    }
    commits
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::objects::{FanType, ObjectKind, WaterFixtureKind};

    fn fan() -> ObjectKind {
        ObjectKind::Fan(FanType::Extract)
    }
    fn thermostat() -> ObjectKind {
        ObjectKind::Thermostat
    }
    fn photo(name: &str) -> (PathBuf, PhotoQuality) {
        (
            PathBuf::from(format!("/tmp/{name}.jpg")),
            PhotoQuality::High,
        )
    }

    /// The misattachment regression: a photo captured under kind A must not
    /// surface under kind B after a switch — and must still be there (parked)
    /// when A is re-selected. OK for B takes only B's photos.
    #[test]
    fn photos_are_parked_per_kind() {
        let room = ItemId::default();
        let mut pending = PendingObjectPhotos::default();
        pending.push(room, &fan(), photo("fan"));

        // Switch to another kind: its buffer is empty (gates read this).
        assert!(pending.for_kind(room, &thermostat()).is_empty());
        // OK on the thermostat commits ONLY its own (no) photos…
        assert!(pending.take(room, &thermostat()).is_empty());
        // …and the fan photo is still parked, shown again on switch-back.
        assert_eq!(pending.for_kind(room, &fan()).len(), 1);

        // OK on the fan consumes exactly its buffer, once.
        let taken = pending.take(room, &fan());
        assert_eq!(taken.len(), 1);
        assert_eq!(taken[0].0, PathBuf::from("/tmp/fan.jpg"));
        assert!(pending.for_kind(room, &fan()).is_empty());
    }

    /// The cross-room regression (field: bath/shower photos mixed): a photo
    /// parked in room A must never surface in room B — not for the gates, not
    /// on an OK — even for the SAME kind.
    #[test]
    fn photos_never_cross_rooms() {
        let (room_a, room_b) = (ItemId::default(), ItemId::default());
        let shower = ObjectKind::WaterFixture(WaterFixtureKind::Shower);
        let mut pending = PendingObjectPhotos::default();
        pending.push(room_a, &shower, photo("shower_a"));

        assert!(pending.for_kind(room_b, &shower).is_empty());
        assert!(pending.take(room_b, &shower).is_empty());
        assert_eq!(pending.for_kind(room_a, &shower).len(), 1);
    }

    /// The exact required regression: add Other "bath" + photo (no OK), switch
    /// to Other "shower", photo, OK — shower gets ONLY its photo; back to
    /// "bath", OK — bath still has its own.
    #[test]
    fn other_names_keep_their_own_photos() {
        let room = ItemId::default();
        let mut pending = PendingObjectPhotos::default();
        // Other "bath" typed (the panel migrates the buffer as the name is
        // typed), photo taken, NO OK.
        pending.rename_other(room, "", "bath");
        pending.push(room, &ObjectKind::Other("bath".into()), photo("bath"));

        // Switch: the panel resets the name to "" and "shower" is typed fresh
        // (migrating the EMPTY active buffer, not bath's parked one).
        pending.rename_other(room, "", "shower");
        assert!(
            pending
                .for_kind(room, &ObjectKind::Other("shower".into()))
                .is_empty(),
            "bath's parked photo must not appear under shower"
        );
        pending.push(room, &ObjectKind::Other("shower".into()), photo("shower"));

        // OK on shower: exactly its own photo.
        let shower_taken = pending.take(room, &ObjectKind::Other("shower".into()));
        assert_eq!(shower_taken.len(), 1);
        assert_eq!(shower_taken[0].0, PathBuf::from("/tmp/shower.jpg"));

        // Back to Other, retype "bath": the parked buffer is found again.
        pending.rename_other(room, "", "bath");
        let bath = pending.take(room, &ObjectKind::Other("bath".into()));
        assert_eq!(bath.len(), 1);
        assert_eq!(bath[0].0, PathBuf::from("/tmp/bath.jpg"));
    }

    /// Photos follow a LIVE rename of the in-flight "Other" (keystroke
    /// migration), merging into an existing parked buffer of the target name.
    #[test]
    fn other_rename_migrates_and_merges() {
        let room = ItemId::default();
        let mut pending = PendingObjectPhotos::default();
        pending.push(room, &ObjectKind::Other("heat p".into()), photo("hp1"));
        pending.rename_other(room, "heat p", "heat pump");
        assert_eq!(
            pending
                .for_kind(room, &ObjectKind::Other("heat pump".into()))
                .len(),
            1
        );
        // A second photo under a partial retype merges on completing the name.
        pending.push(room, &ObjectKind::Other("heat".into()), photo("hp2"));
        pending.rename_other(room, "heat", "heat pump");
        assert_eq!(
            pending
                .for_kind(room, &ObjectKind::Other("heat pump".into()))
                .len(),
            2
        );
    }

    /// Tap-out with an EMPTY entry dismisses silently; with anything entered
    /// it commits like OK — and parked buffers for the room flush as their own
    /// objects (photos never strand on room close).
    #[test]
    fn dismiss_commits_non_empty_entries_and_flushes_parked() {
        let (room_a, room_b) = (ItemId::default(), ItemId::default());
        let mut pending = PendingObjectPhotos::default();

        // Empty entry (kind picked, nothing entered): silent dismiss.
        assert!(take_dismiss_commits(
            room_a,
            Some(&fan()),
            "",
            "",
            "",
            "",
            "",
            None,
            None,
            |_| false,
            &mut pending
        )
        .is_empty());

        // A note typed: commits (count defaults to 1), like pressing OK.
        let commits = take_dismiss_commits(
            room_a,
            Some(&fan()),
            "",
            " by door ",
            "",
            "",
            "",
            None,
            None,
            |_| false,
            &mut pending,
        );
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].count, 1);
        assert_eq!(commits[0].note.as_deref(), Some("by door"));

        // Mandatory photo missing: withheld (OK would have been disabled).
        assert!(take_dismiss_commits(
            room_a,
            Some(&fan()),
            "2",
            "",
            "",
            "",
            "",
            None,
            None,
            |_| true,
            &mut pending
        )
        .is_empty());

        // In-flight entry with a photo + a parked buffer for another kind:
        // both commit; the other ROOM's parked buffer stays put.
        pending.push(room_a, &fan(), photo("fan"));
        pending.push(room_a, &thermostat(), photo("stat"));
        pending.push(room_b, &fan(), photo("fan_b"));
        let commits = take_dismiss_commits(
            room_a,
            Some(&fan()),
            "3",
            "",
            "",
            "",
            "",
            None,
            None,
            |_| true,
            &mut pending,
        );
        assert_eq!(commits.len(), 2);
        assert_eq!(commits[0].kind, fan());
        assert_eq!(commits[0].count, 3);
        assert_eq!(commits[0].photos.len(), 1);
        assert_eq!(commits[1].kind, thermostat());
        assert_eq!(commits[1].count, 1);
        assert_eq!(commits[1].photos.len(), 1);
        assert_eq!(
            pending.for_kind(room_b, &fan()).len(),
            1,
            "another room's parked photos are untouched"
        );
    }
}
