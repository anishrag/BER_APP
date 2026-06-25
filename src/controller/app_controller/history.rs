//! Undo/redo: whole-`Document` snapshots, the history/redo stacks.
use super::*;

/// Snapshot of the editable state for undo/redo: the whole `Document` (shapes +
/// live editing buffer + exposure) plus the living-area mode flag. View
/// (zoom/pan), in-flight prompts, and drag state are deliberately excluded so
/// undo doesn't fight the user's current view or interrupt a prompt mid-flow.
#[derive(Debug, Clone)]
pub struct HistorySnapshot {
    /// The whole editable document. Undo is a `Document` clone/restore; this is
    /// the seam the snapshot has always followed (it once listed these fields by
    /// hand — floor plan, dimensioning, edit state, arc, shapes, exposure).
    pub(super) document: Document,
    /// The living-area MODE flag (a UI tab toggle) is session state kept OUT of
    /// `Document`, but undo restores it so a draw/undo also round-trips the tab.
    pub(super) living_area_mode: bool,
    /// The active storey's BER record AS IT WAS — the only undoable state that
    /// lives on the BER side rather than in `Document`: window/door OPENINGS
    /// (and the `opes` links + window types). Captured per snapshot so an
    /// opening add/move/delete undoes; `None` when no record/storey is active.
    /// On restore it's written back BEFORE the re-resolve, which then derives
    /// vertices and amends `opes` from it.
    pub(super) record_storey: Option<(usize, ber_model::ber::structural::Storey)>,
    /// The window-type registry (windows reference it by id) — restored with the
    /// storey so an undone window doesn't leave a dangling type reference.
    pub(super) window_types: Option<Vec<ber_model::ber::fenestration::WindowType>>,
    /// The WHOLE storey stack AS IT WAS — captured only by actions that mutate
    /// records ACROSS storeys (e.g. `auto_expose_all_storeys`, which pushes
    /// carves onto every storey's `floor_splits`/`roof_splits`). When `Some`, the
    /// restore writes back every storey's record (and SKIPS the single
    /// `record_storey` write); `None` (the common case) keeps the active-only
    /// behaviour. The normal `current_history_snapshot` leaves this `None`.
    pub(super) all_storeys: Option<Vec<ber_model::ber::structural::Storey>>,
    /// The dwelling-level photo list AS IT WAS — external pictures (Front /
    /// Right / Left / Back) live on `Ber.photos`, not on a storey, so they're
    /// captured here so an external-photo add/replace/remove undoes. `None`
    /// when no record is loaded.
    pub(super) photos: Option<Vec<ber_model::ber::common::Photo>>,
}

impl AppController {
    /// Snapshot the current data state just before a mutation. Push to
    /// the history stack so undo can return here. Clears the redo stack
    /// because any new mutation invalidates redo-able future states.
    pub(super) fn record_history(&mut self) {
        let snap = self.current_history_snapshot();
        self.push_history_snapshot(snap);
    }

    pub(super) fn current_history_snapshot(&self) -> HistorySnapshot {
        let record_storey = self.current_storey_idx.and_then(|idx| {
            self.current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(idx).map(|s| (idx, s.clone())))
        });
        let window_types = self
            .current_record
            .as_ref()
            .map(|r| r.data.window_types.clone());
        let photos = self.current_record.as_ref().map(|r| r.data.photos.clone());
        HistorySnapshot {
            document: self.document.clone(),
            living_area_mode: self.living_area_mode,
            record_storey,
            window_types,
            all_storeys: None,
            photos,
        }
    }

    /// A FULL snapshot that also captures the WHOLE storey stack — for actions
    /// that mutate records across multiple storeys (`auto_expose_all_storeys`).
    /// Restore writes back every storey's record, so carves pushed onto
    /// non-active storeys revert too. Otherwise identical to
    /// [`current_history_snapshot`](Self::current_history_snapshot).
    pub(super) fn current_history_snapshot_full(&self) -> HistorySnapshot {
        let mut snap = self.current_history_snapshot();
        snap.all_storeys = self.current_record.as_ref().map(|r| r.data.storeys.clone());
        snap
    }

    pub(super) fn restore_history_snapshot(&mut self, snap: HistorySnapshot) {
        self.document = snap.document;
        self.living_area_mode = snap.living_area_mode;
        // Restore the BER-side openings (and window types) BEFORE the next
        // re-resolve, which re-derives vertices and amends `opes` from them.
        if let Some(record) = self.current_record.as_mut() {
            if let Some(types) = snap.window_types {
                record.data.window_types = types;
            }
            // Dwelling-level external photos (not part of a storey).
            if let Some(photos) = snap.photos {
                record.data.photos = photos;
            }
            // A full snapshot restores the WHOLE stack (and SKIPS the
            // single-storey write); otherwise restore just the active storey.
            if let Some(all) = snap.all_storeys {
                record.data.storeys = all;
            } else if let Some((idx, storey)) = snap.record_storey {
                if let Some(slot) = record.data.storeys.get_mut(idx) {
                    *slot = storey;
                }
            }
        }
        // In-flight prompts no longer match the restored state — clear.
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
    }

    pub(super) fn push_history_snapshot(&mut self, snap: HistorySnapshot) {
        self.history.push(snap);
        const MAX_HISTORY: usize = 100;
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.redo.clear();
    }

    /// Restore the most recent snapshot from history. Pushes the current
    /// state to redo so it can be re-applied. No-op when history is empty.
    pub fn undo(&mut self) -> Result<(), String> {
        let Some(snap) = self.history.pop() else {
            return Ok(());
        };
        let cur = self.current_history_snapshot();
        self.redo.push(cur);
        self.restore_history_snapshot(snap);
        // Push the restored polygon to BER so the canvas reflects it.
        self.mark_dirty()?;
        Ok(())
    }

    /// Re-apply the last undone state. No-op if redo stack is empty.
    pub fn redo(&mut self) -> Result<(), String> {
        let Some(snap) = self.redo.pop() else {
            return Ok(());
        };
        let cur = self.current_history_snapshot();
        self.history.push(cur);
        self.restore_history_snapshot(snap);
        self.mark_dirty()?;
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }
}
