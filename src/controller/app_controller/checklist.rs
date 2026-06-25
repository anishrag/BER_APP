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
                label: "Wall exposures",
                status: self.checklist_status(|s, _| s.exposure_complete()),
            },
            ChecklistItem {
                label: "Floor exposures",
                status: self.ber_storey_status(document::floor_exposure_complete),
            },
            ChecklistItem {
                label: "Ceiling exposures",
                status: self.ber_storey_status(document::ceiling_exposure_complete),
            },
            ChecklistItem {
                label: "Construction phase",
                status: self.ber_storey_status(document::construction_phase_complete),
            },
            ChecklistItem {
                label: "Living area",
                status: self.living_area_status(),
            },
            ChecklistItem {
                label: "External photos",
                status: self.external_photos_status(),
            },
        ]
    }

    /// Per-DWELLING external-photos gate: green ([`ChecklistStatus::AllStoreys`])
    /// when all four sides (Front/Right/Left/Back) have a photo, red otherwise.
    /// A dwelling-level gate (like [`living_area_status`](Self::living_area_status));
    /// reads `Ber.photos` via [`external_photos_complete`](Self::external_photos_complete).
    fn external_photos_status(&self) -> ChecklistStatus {
        if self.external_photos_complete() {
            ChecklistStatus::AllStoreys
        } else {
            ChecklistStatus::Incomplete
        }
    }

    /// Per-DWELLING living-area gate: green ([`ChecklistStatus::AllStoreys`])
    /// when ANY storey has a finalised living area (the assessor only needs to
    /// mark the heated living space once per dwelling), red otherwise. Reads the
    /// editing model ([`StoreyDoc::has_living_area`]).
    fn living_area_status(&self) -> ChecklistStatus {
        if self.document.storeys.iter().any(StoreyDoc::has_living_area) {
            ChecklistStatus::AllStoreys
        } else {
            ChecklistStatus::Incomplete
        }
    }

    /// Roll a per-storey predicate over the RESOLVED [`Ber`] (Store A) up against
    /// the current scope — the read-from-resolved-storey analogue of
    /// [`checklist_status`](Self::checklist_status). Used for the Mapping rows
    /// that read committed regions (Floor/Ceiling exposures, Construction phase)
    /// and the Structure rows ([`Ber::structure_surfaces_complete`]).
    fn ber_storey_status(&self, ok: impl Fn(&Ber, usize) -> bool) -> ChecklistStatus {
        let Some(record) = self.current_record.as_ref() else {
            return ChecklistStatus::Incomplete;
        };
        let storeys = &record.data.storeys;
        if storeys.is_empty() {
            return ChecklistStatus::Incomplete;
        }
        let done = |i: usize| ok(&record.data, i);
        if (0..storeys.len()).all(done) {
            ChecklistStatus::AllStoreys
        } else if self
            .current_storey_idx
            .is_some_and(|i| i < storeys.len() && done(i))
        {
            ChecklistStatus::ThisStorey
        } else {
            ChecklistStatus::Incomplete
        }
    }

    /// Whether the Mapping (Stage A) phase is complete — every checklist item
    /// green (done on every storey). Gates switching onto the Structure phase.
    pub fn mapping_complete(&self) -> bool {
        let items = self.stage_a_checklist();
        !items.is_empty()
            && items
                .iter()
                .all(|item| item.status == ChecklistStatus::AllStoreys)
    }

    /// The Structure phase checklist: one row per surface kind (Walls / Roofs /
    /// Floors), green only when EVERY surface of that kind is fully attributed
    /// (construction type + build-up set, plus a photo where the build-up is a
    /// specified custom stack) on every storey — see
    /// [`Ber::structure_surfaces_complete`]. Yellow when only the current storey
    /// is done, red otherwise.
    pub fn structure_checklist(&self) -> Vec<ChecklistItem> {
        use ber_model::ber::structural::SurfaceKind;
        vec![
            ChecklistItem {
                label: "Walls",
                status: self
                    .ber_storey_status(|b, i| b.structure_surfaces_complete(i, SurfaceKind::Walls)),
            },
            ChecklistItem {
                label: "Roofs",
                status: self
                    .ber_storey_status(|b, i| b.structure_surfaces_complete(i, SurfaceKind::Roofs)),
            },
            ChecklistItem {
                label: "Floors",
                status: self.ber_storey_status(|b, i| {
                    b.structure_surfaces_complete(i, SurfaceKind::Floors)
                }),
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
