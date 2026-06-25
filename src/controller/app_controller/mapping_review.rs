//! The Stage-1 (Mapping) REVIEW flow: a takeover view that, per external side
//! (Front/Right/Left/Back), shows that side's external elevation beside its
//! external photo so the assessor can confirm the model matches reality and
//! nudge openings. Confirming "all's good" sets `Ber.mapping_reviewed`, which is
//! what unlocks the Structure phase (raw checklist completeness only enables the
//! Review button — see `checklist.rs` / the phase gate in `overlays.rs`).
//!
//! The geometry (`Ber::side_elevation`) and the opening-move constraints
//! (`openings::move_opening`) live in `ber_model`; this is the thin shell that
//! drives them with selection, drag→move mapping, undo, and persistence.
use super::*;
use ber_model::ber::common::ExternalSide;
use ber_model::ber::openings::{self, OpeningRef};
use ber_model::ber::structural::SideElevation;

/// Everything the review overlay needs for one side: the side's external
/// elevation, its paired external photo (if taken), and the currently selected
/// opening. Built fresh each frame by [`AppController::mapping_review_view`].
pub struct ReviewView {
    pub side: ExternalSide,
    pub elevation: SideElevation,
    pub photo: Option<PhotoView>,
    pub selection: Option<OpeningRef>,
}

impl AppController {
    /// Whether the Mapping review takeover view is open.
    pub fn mapping_review_mode(&self) -> bool {
        self.mapping_review_mode
    }

    /// Open / close the Mapping review. Entering resets to the first side and
    /// clears any in-flight drag/selection.
    pub fn set_mapping_review_mode(&mut self, on: bool) {
        self.mapping_review_mode = on;
        self.mapping_review_drag = None;
        self.mapping_review_selection = None;
        if on {
            self.mapping_review_side = ExternalSide::Front;
        }
    }

    /// Show a specific side in the review.
    pub fn set_mapping_review_side(&mut self, side: ExternalSide) {
        self.mapping_review_side = side;
        self.mapping_review_selection = None;
        self.mapping_review_drag = None;
    }

    /// Step the review to the next / previous side (wraps), for the scroll
    /// controls. `delta` is +1 (next) or -1 (previous).
    pub fn step_mapping_review_side(&mut self, delta: i32) {
        let n = ExternalSide::ALL.len() as i32;
        let cur = ExternalSide::ALL
            .iter()
            .position(|&s| s == self.mapping_review_side)
            .unwrap_or(0) as i32;
        let next = (((cur + delta) % n) + n) % n;
        self.set_mapping_review_side(ExternalSide::ALL[next as usize]);
    }

    /// Whether the dwelling's Mapping review has been confirmed — the gate that
    /// unlocks the Structure phase.
    pub fn mapping_reviewed(&self) -> bool {
        self.current_record
            .as_ref()
            .is_some_and(|r| r.data.mapping_reviewed)
    }

    /// Confirm "all's good": mark the dwelling Mapping-reviewed (persisted) and
    /// close the review. This is what unlocks the Structure phase.
    pub fn confirm_mapping_review(&mut self) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.mapping_reviewed = true;
        }
        let _ = self.mark_dirty();
        self.set_mapping_review_mode(false);
    }

    /// The current side's review view (elevation + paired photo + selection), or
    /// `None` when no record is loaded.
    pub fn mapping_review_view(&self) -> Option<ReviewView> {
        let record = self.current_record.as_ref()?;
        let side = self.mapping_review_side;
        let elevation = record.data.side_elevation(side);
        let photo = self
            .external_photos()
            .into_iter()
            .find(|(s, _)| *s == side)
            .and_then(|(_, p)| p);
        Some(ReviewView {
            side,
            elevation,
            photo,
            selection: self.mapping_review_selection,
        })
    }

    /// Press in the review elevation at composite `(x, z)` (metres): select +
    /// grab the opening under the cursor (if any). Snapshots the WHOLE storey
    /// stack once for the drag, because the grabbed opening may live on a
    /// non-active storey.
    pub fn mapping_review_pointer_down(&mut self, x: f64, z: f64) {
        let hit = self.current_record.as_ref().and_then(|r| {
            r.data
                .side_elevation(self.mapping_review_side)
                .openings
                .into_iter()
                .find(|o| {
                    let half = o.width_m * 0.5;
                    x >= o.x_center - half
                        && x <= o.x_center + half
                        && z >= o.sill_z
                        && z <= o.sill_z + o.height_m
                })
        });
        if let Some(o) = hit {
            let snap = self.current_history_snapshot_full();
            self.push_history_snapshot(snap);
            self.mapping_review_drag = Some((o.storey, o.wall_id, o.opening));
            self.mapping_review_selection = Some(o.opening);
        } else {
            self.mapping_review_selection = None;
        }
    }

    /// Drag the grabbed opening so its centre follows the composite cursor
    /// `(x, z)`: map back to the opening's wall-local `(along, sill)` and apply
    /// via `openings::move_opening`, which keeps it on the wall and non-
    /// overlapping. No-op if nothing is grabbed.
    pub fn mapping_review_pointer_drag(&mut self, x: f64, z: f64) {
        let Some((si, wall_id, opref)) = self.mapping_review_drag else {
            return;
        };
        let h = self.mapping_review_side.view_axis();

        // First borrow: recover the wall-local along (the projection inverse lives
        // in `ber_model`) and the sill (height above the wall's composite base).
        let Some((along_m, sill_m)) = self.current_record.as_ref().and_then(|r| {
            let storey = r.data.storeys.get(si)?;
            let wall = storey.walls.regions.iter().find(|w| w.id == wall_id)?;
            let along = wall.elevation_along_at(h, x)?;
            let (_origin, _u, _len, base_z) = wall.base_frame()?;
            let datum = r.data.storey_floor_datums().get(si).copied().unwrap_or(0.0);
            let half_h = openings::opening_height(storey, opref) as f64 / 2.0;
            let sill = (z - (base_z + datum) - half_h).max(0.0);
            Some((along as f32, sill as f32))
        }) else {
            return;
        };

        // Second borrow: apply the move (constrained) + refresh the anchor.
        if let Some(record) = self.current_record.as_mut() {
            if let Some(storey) = record.data.storeys.get_mut(si) {
                let _ = openings::move_opening(storey, opref, along_m, sill_m);
            }
        }
        // NB: no `mark_dirty` here. The review elevation reads opening PLACEMENTS
        // directly (`wall_openings`), so the move shows live without a re-derive;
        // marking dirty every drag-frame re-resolved the whole storey per frame,
        // which (next to the photo repaint) made dragging jittery. We commit the
        // re-derive + anchor reseed + persist ONCE on release instead.
    }

    /// Release the dragged opening — commit the move: reseed the anchor + mark
    /// dirty so the storey re-resolves and persists exactly once for the drag.
    pub fn mapping_review_pointer_up(&mut self) {
        if let Some((si, _, _)) = self.mapping_review_drag {
            self.reseed_opening_anchors(si);
            let _ = self.mark_dirty();
        }
        self.mapping_review_drag = None;
    }
}
