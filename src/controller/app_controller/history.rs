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
        HistorySnapshot {
            document: self.document.clone(),
            living_area_mode: self.living_area_mode,
        }
    }

    pub(super) fn restore_history_snapshot(&mut self, snap: HistorySnapshot) {
        self.document = snap.document;
        self.living_area_mode = snap.living_area_mode;
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
