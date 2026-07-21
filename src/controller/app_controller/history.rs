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
    /// The porch MODE flag — same session-state-outside-`Document` story as
    /// `living_area_mode` (the shared cut-pick also routes by it on replay).
    pub(super) porch_mode: bool,
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
    /// records ACROSS storeys (e.g. the floor/ceiling-exposure carves, which
    /// push splits onto every overlapping storey's record). When `Some`, the
    /// restore writes back every storey's record (and SKIPS the single
    /// `record_storey` write); `None` (the common case) keeps the active-only
    /// behaviour. The normal `current_history_snapshot` leaves this `None`.
    pub(super) all_storeys: Option<Vec<ber_model::ber::structural::Storey>>,
    /// The dwelling-level photo list AS IT WAS — external pictures (Front /
    /// Right / Left / Back) live on `Ber.photos`, not on a storey, so they're
    /// captured here so an external-photo add/replace/remove undoes. `None`
    /// when no record is loaded.
    pub(super) photos: Option<Vec<ber_model::ber::common::Evidence>>,
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
            porch_mode: self.porch_mode,
            record_storey,
            window_types,
            all_storeys: None,
            photos,
        }
    }

    /// A FULL snapshot that also captures the WHOLE storey stack — for actions
    /// that mutate records across multiple storeys (the floor/ceiling-exposure
    /// carves). Restore writes back every storey's record, so carves pushed onto
    /// non-active storeys revert too. Otherwise identical to
    /// [`current_history_snapshot`](Self::current_history_snapshot).
    pub(super) fn current_history_snapshot_full(&self) -> HistorySnapshot {
        let mut snap = self.current_history_snapshot();
        snap.all_storeys = self.current_record.as_ref().map(|r| r.data.storeys.clone());
        snap
    }

    pub(super) fn restore_history_snapshot(&mut self, snap: HistorySnapshot) {
        let all_storeys_restored = snap.all_storeys.is_some();
        self.document = snap.document;
        self.living_area_mode = snap.living_area_mode;
        self.porch_mode = snap.porch_mode;
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
        // The per-storey SESSION region maps (phase / floor-step / ceiling-step
        // areas) mirror committed record state but live OUTSIDE the snapshot,
        // so the write-back above left them stale — an undone area would
        // survive as a phantom the next carve subtracts from its available
        // region, collapsing a redraw over the same footprint to a line.
        // Re-derive them from the just-restored record (the storey-switch
        // landing sequence).
        self.refresh_session_regions_after_restore(all_storeys_restored);
        // In-flight prompts no longer match the restored state — clear. The
        // staged cut-pick goes with them: its candidate regions were carved
        // from the pre-undo geometry, so an undo across it must not leave a
        // resurrectable pick behind.
        self.pending_cut = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
    }

    /// Re-derive the per-storey SESSION region maps (`phase_regions`,
    /// `step_regions`, `ceiling_step_regions`) from the restored record — the
    /// tail of every [`restore_history_snapshot`]. Each reconstruct fn
    /// inserts-or-removes its storey's entry, so this is a full refresh of the
    /// touched storeys: the active one, or ALL of them after a full-stack
    /// (`all_storeys`) restore — the cross-storey exposure carves rewrite
    /// every storey's record. Runs BEFORE the caller's `mark_dirty`, so the
    /// deferred re-resolve reads maps consistent with the restored record.
    ///
    /// SKIPPED for a storey whose restored outline is NOT finalised: before
    /// finalisation the maps hold FORWARD-LOOKING seeds (the inherited
    /// floor-level / phase seed lands before the record can carry it — outside
    /// undo by design), not mirrors of the record. The phantom hazard only
    /// exists once the outline is finalised — the gate every carve flow sits
    /// behind — which is exactly when the maps mirror committed record state.
    ///
    /// [`restore_history_snapshot`]: Self::restore_history_snapshot
    fn refresh_session_regions_after_restore(&mut self, all_storeys: bool) {
        let indices: Vec<usize> = if all_storeys {
            let n = self
                .current_record
                .as_ref()
                .map(|r| r.data.storeys.len())
                .unwrap_or(0);
            (0..n).collect()
        } else {
            self.current_storey_idx.into_iter().collect()
        };
        for i in indices {
            // Per-storey `outline_finalised`, on the restored outline SLOT.
            // (`ShapeSlot::finalised` is vacuously true for an EMPTY plan, so
            // require a drawn outline too.)
            let outline_finalised = self.document.storeys.get(i).is_some_and(|s| {
                !s.outline.floor_plan.get_points().is_empty()
                    && s.outline.dimensioning.scale_set
                    && s.outline.finalised()
            });
            if !outline_finalised {
                continue;
            }
            self.reconstruct_phase_regions_for(i);
            self.reconstruct_step_regions_for(i);
            self.load_ceiling_step_regions_for(i);
        }
    }

    pub(super) fn push_history_snapshot(&mut self, snap: HistorySnapshot) {
        self.history.push(snap);
        const MAX_HISTORY: usize = 100;
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
            // The trim shifts every stack index down — the snap capture's and
            // the partition instant-commit record's `hlen` now point one entry
            // too high, so a replay/un-commit would rewind to the wrong state.
            // Both are best-effort conveniences; drop them.
            self.last_snap_action = None;
            self.partition_instant_commit = None;
        }
        self.redo.clear();
    }

    /// Restore the most recent snapshot from history. Pushes the current
    /// state to redo so it can be re-applied. No-op when history is empty.
    pub fn undo(&mut self) -> Result<(), String> {
        let Some(snap) = self.history.pop() else {
            return Ok(());
        };
        // The partition instant-commit record and the snap capture both index
        // into the history stack — stale (and dangerous to replay) after any
        // undo/redo.
        self.partition_instant_commit = None;
        self.last_snap_action = None;
        // Bring the record current BEFORE capturing the redo snapshot: an
        // action that only marked dirty (e.g. a floor-step level assign) has
        // its session-map effect derived into the record here, so the
        // snapshot's record is faithful — the restore-side session-map
        // reconstruction reads it. No-op when nothing is pending.
        self.materialize();
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
        // See `undo`: the partition instant-commit record and the snap capture
        // index into the history stack, which redo is about to grow past.
        self.partition_instant_commit = None;
        self.last_snap_action = None;
        // See `undo`: capture a MATERIALIZED record so the history snapshot
        // is faithful for the restore-side session-map reconstruction.
        self.materialize();
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
