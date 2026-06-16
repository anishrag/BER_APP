//! The Stage A ("Mapping") completion checklist: a per-item red/yellow/green
//! status the Sketch overlay shows, recomputed from the editing model each
//! frame. An item is "complete on a storey" when its mapping data is fully
//! entered there; the status rolls the per-storey results up against the
//! current view scope:
//!   - green  ([`ChecklistStatus::AllStoreys`]) — complete on every storey
//!   - yellow ([`ChecklistStatus::ThisStorey`])  — complete on the current
//!     storey but not all
//!   - red    ([`ChecklistStatus::Incomplete`])  — not complete on the current
//!     storey
//!
//! On the All-storeys tab there's no "current" storey, so an item is green
//! (every storey done) or red (not). This is presentation-only view data — the
//! per-storey predicates live in `document` (`StoreyDoc::measurements_complete`
//! / `exposure_complete`, and the BER `Storey::ceiling_height_m` for heights);
//! the controller only rolls them up against the scope.
use super::*;

/// Completion of one checklist item against the current view scope — the colour
/// of its dot in the Sketch overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecklistStatus {
    /// Not complete on the current storey (red).
    Incomplete,
    /// Complete on the current storey, but not on every storey (yellow).
    ThisStorey,
    /// Complete on every storey (green).
    AllStoreys,
}

/// One row of the Stage A checklist: a label + its recomputed status.
#[derive(Debug, Clone, Copy)]
pub struct ChecklistItem {
    pub label: &'static str,
    pub status: ChecklistStatus,
}

impl AppController {
    /// The Stage A checklist rows, recomputed from the current editing model.
    /// Every row is [`ChecklistStatus::Incomplete`] when no BER is loaded.
    pub fn stage_a_checklist(&self) -> Vec<ChecklistItem> {
        vec![
            ChecklistItem {
                label: "Measurements",
                status: self.checklist_status(|s, _| s.measurements_complete()),
            },
            ChecklistItem {
                label: "Heights",
                status: self.checklist_status(|_, h| h.is_some()),
            },
            ChecklistItem {
                label: "Exposures",
                status: self.checklist_status(|s, _| s.exposure_complete()),
            },
        ]
    }

    /// Roll a per-storey completion predicate up against the current scope. The
    /// predicate gets each storey's editing model plus its durable ceiling
    /// height (which lives on the BER `Storey`, not the `StoreyDoc`).
    fn checklist_status(
        &self,
        complete_on: impl Fn(&StoreyDoc, Option<f64>) -> bool,
    ) -> ChecklistStatus {
        let Some(record) = self.current_record.as_ref() else {
            return ChecklistStatus::Incomplete;
        };
        let storeys = &record.data.storeys;
        // One `StoreyDoc` per BER storey; `min` guards a transient mismatch.
        let n = self.document.storeys.len().min(storeys.len());
        if n == 0 {
            return ChecklistStatus::Incomplete;
        }
        let done = |i: usize| complete_on(&self.document.storeys[i], storeys[i].ceiling_height_m);
        if (0..n).all(done) {
            ChecklistStatus::AllStoreys
        } else if self.current_storey_idx.is_some_and(|i| i < n && done(i)) {
            ChecklistStatus::ThisStorey
        } else {
            ChecklistStatus::Incomplete
        }
    }
}
