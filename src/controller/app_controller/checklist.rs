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
//!
//! Every non-green row also carries REASONS — the concrete unmet conditions
//! (counts, dimensions, overhangs), tap-to-show in the UI. Reasons come from
//! the SAME reason-returning cores whose emptiness defines the completeness
//! bools (`document::*_reasons` / `windows_issues`, `Ber::roof_coverage` /
//! `structure_incomplete_reasons` / `missing_external_photo_sides`), and each
//! row's colour is derived from the same per-storey reason lists it displays —
//! so colour and explanation can never disagree: a row is green **iff** its
//! `reasons` are empty.
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

/// One row of a phase checklist: a label, its recomputed status, and — for a
/// non-green row — the concrete unmet conditions behind that status (already
/// storey-prefixed where the dwelling has several storeys). Empty `reasons`
/// **iff** the row is green.
#[derive(Debug, Clone)]
pub struct ChecklistItem {
    pub label: &'static str,
    pub status: ChecklistStatus,
    pub reasons: Vec<String>,
}

impl ChecklistItem {
    /// A row whose status + reasons came from one of the rollups below.
    fn new(label: &'static str, (status, reasons): (ChecklistStatus, Vec<String>)) -> Self {
        Self {
            label,
            status,
            reasons,
        }
    }
}

/// The reason shown when no BER record is open — every gate reads the record,
/// so every row reports the same root cause.
const NO_RECORD: &str = "No dwelling loaded yet";

impl AppController {
    /// The Stage A checklist rows, recomputed from the current editing model.
    /// Every row is [`ChecklistStatus::Incomplete`] when no BER is loaded.
    pub fn stage_a_checklist(&self) -> Vec<ChecklistItem> {
        vec![
            ChecklistItem::new(
                "Measurements",
                self.checklist_rollup(|s, _| s.measurements_reasons()),
            ),
            ChecklistItem::new(
                "Heights",
                self.checklist_rollup(|_, h| {
                    if h.is_some() {
                        Vec::new()
                    } else {
                        vec!["Ceiling height not set".to_string()]
                    }
                }),
            ),
            ChecklistItem::new(
                "Wall exposures",
                self.ber_storey_rollup(document::wall_exposure_reasons),
            ),
            ChecklistItem::new(
                "Floor exposures",
                self.ber_storey_rollup(document::floor_exposure_reasons),
            ),
            ChecklistItem::new(
                "Ceiling exposures",
                self.ber_storey_rollup(document::ceiling_exposure_reasons),
            ),
            ChecklistItem::new("Roof", self.roof_row()),
            ChecklistItem::new(
                "Construction phase",
                self.ber_storey_rollup(document::construction_phase_reasons),
            ),
            ChecklistItem::new("Windows", self.windows_row()),
            ChecklistItem::new("Living area", self.living_area_row()),
            ChecklistItem::new("External photos", self.external_photos_row()),
        ]
    }

    /// Per-DWELLING external-photos gate: green ([`ChecklistStatus::AllStoreys`])
    /// when every REQUIRED side (Front/Back; Left/Right are optional, deferral
    /// counts as set) has a photo, red otherwise. Reads
    /// [`Ber::missing_external_photo_sides`] — the same core
    /// [`external_photos_complete`](Self::external_photos_complete) wraps.
    fn external_photos_row(&self) -> (ChecklistStatus, Vec<String>) {
        let Some(record) = self.current_record.as_ref() else {
            return no_record_row();
        };
        let reasons: Vec<String> = record
            .data
            .missing_external_photo_sides()
            .into_iter()
            .map(|side| {
                format!(
                    "No {} photo yet (tap Defer to postpone a side)",
                    side.label()
                )
            })
            .collect();
        (dwelling_status(&reasons), reasons)
    }

    /// Per-DWELLING windows gate: green ([`ChecklistStatus::AllStoreys`]) when
    /// the dwelling has at least four windows house-wide AND every multi-storey
    /// window stays within its wall's same-type run (doesn't hang off the
    /// building) AND every ROOF window fits on the single ceiling surface it
    /// sits on (its ceiling-plane footprint doesn't spill across a ridge onto a
    /// neighbouring surface — the assessor must reposition, resize, or rotate an
    /// over-large rooflight to fit), red otherwise. Reads the RESOLVED [`Ber`]
    /// via [`document::windows_issues`] +
    /// [`ber_model::ber::openings::roof_window_overhangs`] — the reason cores
    /// whose emptiness IS `windows_complete` / `oversized_roof_windows`.
    fn windows_row(&self) -> (ChecklistStatus, Vec<String>) {
        use ber_model::ber::openings;
        let Some(record) = self.current_record.as_ref() else {
            return no_record_row();
        };
        let ber = &record.data;
        let mut reasons: Vec<String> = document::windows_issues(ber)
            .into_iter()
            .map(|issue| match issue.storey_idx {
                Some(idx) => self.prefixed_reason(idx, issue.message),
                None => issue.message,
            })
            .collect();
        for (idx, storey) in ber.storeys.iter().enumerate() {
            for o in openings::roof_window_overhangs(storey) {
                // Below the message's 2-decimal resolution the magnitude would
                // read "0.00 m" — a corner sitting right AT the surface edge
                // (nudge-tolerance territory). Say that instead of a useless
                // zero.
                let msg = if o.overhang_m < 0.005 {
                    format!(
                        "Roof window {:.2} × {:.2} m spills just past the {} edge of its roof \
                         surface — move or resize it",
                        o.width_m, o.height_m, o.direction
                    )
                } else {
                    format!(
                        "Roof window {:.2} × {:.2} m overhangs its roof surface \
                         by {:.2} m ({} edge) — move or resize it",
                        o.width_m, o.height_m, o.overhang_m, o.direction
                    )
                };
                reasons.push(self.prefixed_reason(idx, msg));
            }
        }
        (dwelling_status(&reasons), reasons)
    }

    /// Per-DWELLING roof gate: green ([`ChecklistStatus::AllStoreys`]) once EVERY
    /// ceiling region that has sky / attic above it — i.e. everything except a
    /// `Party` ceiling (another dwelling above) and a derived `InterStorey` one
    /// (our own storey above) — has a roof assigned over it, red otherwise.
    /// Lenient on geometry: whether the roof's eave drops short or overhangs
    /// doesn't matter, only that something covers each area. A dwelling-level
    /// gate (the roof step runs at the end of Mapping, after every storey).
    /// The derivation lives in [`Ber::roof_coverage`]; this row formats its
    /// gaps (which storey, how many m² uncovered).
    fn roof_row(&self) -> (ChecklistStatus, Vec<String>) {
        let Some(record) = self.current_record.as_ref() else {
            return no_record_row();
        };
        let coverage = record.data.roof_coverage();
        let reasons: Vec<String> = if coverage.complete() {
            Vec::new()
        } else if coverage.roofable_regions == 0 {
            vec![
                "No roof-able ceiling surfaces derived yet — set every storey's ceiling height \
                 first"
                    .to_string(),
            ]
        } else {
            coverage
                .uncovered
                .iter()
                .map(|gap| {
                    self.prefixed_reason(
                        gap.storey_idx,
                        format!("{:.1} m² of ceiling has no roof over it", gap.area_m2),
                    )
                })
                .collect()
        };
        (dwelling_status(&reasons), reasons)
    }

    /// Per-DWELLING living-area gate: green ([`ChecklistStatus::AllStoreys`])
    /// when ANY storey has a finalised living area (the assessor only needs to
    /// mark the heated living space once per dwelling), red otherwise. Reads the
    /// editing model ([`StoreyDoc::has_living_area`]).
    fn living_area_row(&self) -> (ChecklistStatus, Vec<String>) {
        if self.current_record.is_none() {
            return no_record_row();
        }
        let reasons: Vec<String> = if self.document.storeys.iter().any(StoreyDoc::has_living_area) {
            Vec::new()
        } else {
            vec![
                "No living area marked on any storey — outline the heated living space once"
                    .to_string(),
            ]
        };
        (dwelling_status(&reasons), reasons)
    }

    /// Roll per-storey reason lists (from the RESOLVED [`Ber`], Store A) up
    /// against the current scope — the read-from-resolved-storey analogue of
    /// [`checklist_rollup`](Self::checklist_rollup). Used for the Mapping rows
    /// that read committed regions (Wall/Floor/Ceiling exposures, Construction
    /// phase) and the Structure rows ([`Ber::structure_incomplete_reasons`]).
    fn ber_storey_rollup(
        &self,
        reasons_on: impl Fn(&Ber, usize) -> Vec<String>,
    ) -> (ChecklistStatus, Vec<String>) {
        let Some(record) = self.current_record.as_ref() else {
            return no_record_row();
        };
        self.rollup(record.data.storeys.len(), |i| reasons_on(&record.data, i))
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

    /// The Structure phase checklist: one row per surface kind (Walls / Ceilings /
    /// Floors), green only when EVERY surface of that kind is fully attributed
    /// (construction type + build-up set, plus a photo where the build-up is a
    /// specified custom stack) on every storey — see
    /// [`Ber::structure_surfaces_complete`] and its reason core
    /// [`Ber::structure_incomplete_reasons`]. Yellow when only the current
    /// storey is done, red otherwise.
    pub fn structure_checklist(&self) -> Vec<ChecklistItem> {
        use ber_model::ber::structural::SurfaceKind;
        vec![
            ChecklistItem::new(
                "Walls",
                self.ber_storey_rollup(|b, i| {
                    b.structure_incomplete_reasons(i, SurfaceKind::Walls)
                }),
            ),
            ChecklistItem::new(
                "Ceilings",
                self.ber_storey_rollup(|b, i| {
                    b.structure_incomplete_reasons(i, SurfaceKind::Ceilings)
                }),
            ),
            ChecklistItem::new(
                "Floors",
                self.ber_storey_rollup(|b, i| {
                    b.structure_incomplete_reasons(i, SurfaceKind::Floors)
                }),
            ),
        ]
    }

    /// The Phase-3 (Objects) checklist. There's no fixed completion target — the
    /// survey is as thorough as the assessor makes it — so these rows are
    /// PROGRESS indicators (green once present, red while empty): whether any room
    /// pin has been dropped, and whether any object has been recorded. Both red
    /// when no BER is loaded.
    pub fn objects_checklist(&self) -> Vec<ChecklistItem> {
        let Some(record) = self.current_record.as_ref() else {
            return vec![
                ChecklistItem::new("Rooms recorded", no_record_row()),
                ChecklistItem::new("Objects recorded", no_record_row()),
            ];
        };
        let rooms: usize = record
            .data
            .storeys
            .iter()
            .map(|s| s.object_rooms.len())
            .sum();
        let objects: usize = record
            .data
            .storeys
            .iter()
            .flat_map(|s| s.object_rooms.iter())
            .map(|rm| rm.objects.len())
            .sum();
        let row = |n: usize, reason: &str| {
            let reasons = if n > 0 {
                Vec::new()
            } else {
                vec![reason.to_string()]
            };
            (dwelling_status(&reasons), reasons)
        };
        vec![
            ChecklistItem::new(
                "Rooms recorded",
                row(rooms, "No room pins dropped yet — tap a room to place one"),
            ),
            ChecklistItem::new(
                "Objects recorded",
                row(objects, "No objects recorded in any room yet"),
            ),
        ]
    }

    /// Roll a per-storey reason list (over the EDITING model plus the durable
    /// ceiling height, which lives on the BER `Storey`, not the `StoreyDoc`)
    /// up against the current scope. Colour comes from the same per-storey
    /// lists that are returned as the row's reasons — empty per-storey list =
    /// that storey is done.
    fn checklist_rollup(
        &self,
        reasons_on: impl Fn(&StoreyDoc, Option<f64>) -> Vec<String>,
    ) -> (ChecklistStatus, Vec<String>) {
        let Some(record) = self.current_record.as_ref() else {
            return no_record_row();
        };
        let storeys = &record.data.storeys;
        // One `StoreyDoc` per BER storey; `min` guards a transient mismatch.
        let n = self.document.storeys.len().min(storeys.len());
        self.rollup(n, |i| {
            reasons_on(&self.document.storeys[i], storeys[i].ceiling_height_m)
        })
    }

    /// The shared scope rollup: per-storey reason lists → (status, flattened
    /// storey-prefixed reasons). A storey is done iff its list is empty; green
    /// = all done, yellow = the current storey is done but others aren't, red
    /// otherwise. Because the status is DERIVED from the same lists that are
    /// returned, a row can never show green with reasons or red without.
    fn rollup(
        &self,
        n: usize,
        reasons_for: impl Fn(usize) -> Vec<String>,
    ) -> (ChecklistStatus, Vec<String>) {
        if n == 0 {
            return (
                ChecklistStatus::Incomplete,
                vec!["No storey drawn yet".to_string()],
            );
        }
        let per: Vec<Vec<String>> = (0..n).map(reasons_for).collect();
        let done = |i: usize| per[i].is_empty();
        let status = if (0..n).all(done) {
            ChecklistStatus::AllStoreys
        } else if self.current_storey_idx.is_some_and(|i| i < n && done(i)) {
            ChecklistStatus::ThisStorey
        } else {
            ChecklistStatus::Incomplete
        };
        let multi = n > 1;
        let reasons = per
            .into_iter()
            .enumerate()
            .flat_map(|(i, rs)| {
                rs.into_iter()
                    .map(move |r| (i, r))
                    .collect::<Vec<_>>()
                    .into_iter()
            })
            .map(|(i, r)| if multi { self.prefixed_reason(i, r) } else { r })
            .collect();
        (status, reasons)
    }

    /// Prefix a reason with its storey's display name ("First Floor: …") so a
    /// reason names WHERE the problem is when the dwelling has several
    /// storeys. Single-storey dwellings skip the prefix (there's only one
    /// place it could be) — callers outside [`rollup`](Self::rollup) go
    /// through this too so the convention stays in one place.
    fn prefixed_reason(&self, idx: usize, reason: String) -> String {
        let multi = self
            .current_record
            .as_ref()
            .is_some_and(|r| r.data.storeys.len() > 1);
        match self.storey_name(idx) {
            Some(name) if multi => format!("{name}: {reason}"),
            _ => reason,
        }
    }
}

/// Status of a DWELLING-level row (no per-storey rollup): red with the reasons,
/// green without — the same "green iff no reasons" invariant as the rollups.
fn dwelling_status(reasons: &[String]) -> ChecklistStatus {
    if reasons.is_empty() {
        ChecklistStatus::AllStoreys
    } else {
        ChecklistStatus::Incomplete
    }
}

/// The row every gate collapses to when no BER record is open.
fn no_record_row() -> (ChecklistStatus, Vec<String>) {
    (ChecklistStatus::Incomplete, vec![NO_RECORD.to_string()])
}
