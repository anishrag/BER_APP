//! Edit-dimension workflow — re-dimensioning a wall of a FINALISED outline,
//! choosing which end of the wall absorbs the change. The model mutation
//! (the plane move + every same-storey satellite) is
//! [`StoreyDoc::resize_outline_wall`](document::StoreyDoc); this shell owns
//! the prompt state, the cross-storey propagation (other storeys stacked on
//! the same building face follow), the session zone transforms, undo, and
//! the re-resolve.

use document::{resize_record_storey, PlaneMoveOutcome, WallEnd};
use domain::points::Point2f32;
use manip_plan::VertexId;

use super::AppController;

/// The open edit-dimension prompt: which outline wall was picked and which of
/// its ends the user has chosen to move. Transient (not persisted / undone).
#[derive(Debug, Clone, Copy)]
pub struct EditDimensionState {
    pub wall_start: VertexId,
    pub moving_end: WallEnd,
}

/// UI-facing view of the open edit-dimension prompt: the wall in plan coords,
/// its current length, and which endpoint will move. The canvas highlights
/// the wall and marks the moving endpoint; the number box shows the length
/// and a side toggle.
#[derive(Debug, Clone, Copy)]
pub struct EditDimensionUiData {
    pub wall_start_pt: Point2f32,
    pub wall_end_pt: Point2f32,
    pub current_length: f32,
    pub moving_end: WallEnd,
}

impl AppController {
    /// Whether edit-dimension mode is on (canvas taps pick a wall).
    pub fn edit_dimension_mode(&self) -> bool {
        self.edit_dimension_mode
    }

    /// Toggle edit-dimension mode. Entering it leaves the drawing/area modes
    /// (one canvas owner at a time — same discipline as exposure mode);
    /// leaving it closes any open prompt.
    pub fn set_edit_dimension_mode(&mut self, on: bool) {
        self.pending_redraw = None;
        self.edit_dimension_mode = on;
        self.edit_dimension = None;
        if on {
            self.exit_openings_mode();
            self.sync_active();
            self.set_living_area_mode(false);
            self.set_porch_mode(false);
            self.phase_mode = false;
            self.step_mode = false;
            self.exposure_mode = false;
            self.selected_exposure_wall = None;
            self.selected_riser = None;
        }
    }

    /// Edit-dimension tap: pick the outline perimeter wall within `tol` of the
    /// world point and open the prompt on it. Only meaningful on a finalised
    /// outline (the tool is only offered there); a miss clears the prompt.
    /// Returns whether a wall was picked.
    pub fn edit_dimension_tap(&mut self, world_x: f64, world_y: f64, tol: f64) -> bool {
        if !self.document.outline.dimensioning.scale_set {
            self.edit_dimension = None;
            return false;
        }
        let plan = self.document.outline.floor_plan.clone();
        let Some((ring, edge_start)) = plan.hit_test_wall(world_x, world_y, tol) else {
            self.edit_dimension = None;
            return false;
        };
        if ring != 0 {
            self.edit_dimension = None;
            return false; // only perimeter walls carry an editable dimension
        }
        let Some(vid) = plan.vertex_id_at(ring, edge_start) else {
            return false;
        };
        if plan.wall_along_ends(ring, edge_start).is_none() {
            return false; // diagonal wall — no single along-axis to edit
        }
        self.edit_dimension = Some(EditDimensionState {
            wall_start: vid,
            // Default to the wall's END: for the walls drawn ring-forward this
            // is most often the far corner rather than the V0 anchor, and the
            // toggle (plus the canvas marker) makes the choice explicit anyway.
            moving_end: WallEnd::End,
        });
        true
    }

    /// Flip which end of the picked wall will absorb the length change.
    pub fn toggle_edit_dimension_side(&mut self) {
        if let Some(s) = self.edit_dimension.as_mut() {
            s.moving_end = match s.moving_end {
                WallEnd::Start => WallEnd::End,
                WallEnd::End => WallEnd::Start,
            };
        }
    }

    /// Close the edit-dimension prompt without changing anything.
    pub fn cancel_edit_dimension(&mut self) {
        self.edit_dimension = None;
    }

    /// The open prompt for the UI, or `None`.
    pub fn edit_dimension_for_ui(&self) -> Option<EditDimensionUiData> {
        let s = self.edit_dimension?;
        let plan = &self.document.outline.floor_plan;
        let (a, b) = plan.wall_endpoints(s.wall_start)?;
        Some(EditDimensionUiData {
            wall_start_pt: a,
            wall_end_pt: b,
            current_length: ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt(),
            moving_end: s.moving_end,
        })
    }

    /// Apply the user's corrected length for the picked wall: one undoable
    /// action that moves the chosen face on THIS storey (outline, sub-shapes,
    /// wall-keyed side tables), transforms the storey's durable record
    /// geometry (splits, steps, opening anchors), propagates the same plane
    /// move to every other storey stacked on that face, transforms the
    /// session zone maps, and re-resolves the whole house. On any rejection
    /// everything rolls back and the prompt stays open with the message.
    pub fn apply_edit_dimension(&mut self, value: f32) -> Result<(), String> {
        let Some(state) = self.edit_dimension else {
            return Err("no wall picked to re-dimension".into());
        };
        // The record must be current before we transform it alongside the
        // document (same "materialize at the top" rule as the structure ops).
        self.materialize();
        let undo_snap = self.current_history_snapshot_full();
        let active_idx = self.current_storey_idx.unwrap_or(0);

        let result = (|| -> Result<Option<()>, String> {
            let Some(outcome) = self.document.storeys[active_idx].resize_outline_wall(
                state.wall_start,
                value,
                state.moving_end,
            )?
            else {
                return Ok(None); // same length — nothing to do
            };
            let mv = outcome.plane_move;
            let mut outcomes: Vec<(usize, PlaneMoveOutcome)> = vec![(active_idx, outcome)];
            // Storeys stacked on the same building face follow it.
            for idx in 0..self.document.storeys.len() {
                if idx == active_idx {
                    continue;
                }
                if let Some(o) = self.document.storeys[idx].apply_plane_move(mv)? {
                    outcomes.push((idx, o));
                }
            }
            // Durable record geometry + opening anchors, per moved storey.
            if let Some(record) = self.current_record.as_mut() {
                for (idx, o) in &outcomes {
                    if let Some(storey) = record.data.storeys.get_mut(*idx) {
                        resize_record_storey(storey, o)?;
                    }
                }
            }
            // Session zone maps (phase / floor-step / ceiling-step areas) are
            // world-space rings outside the document — same pointwise rule.
            for (idx, _) in &outcomes {
                if let Some(regions) = self.phase_regions.get_mut(idx) {
                    for r in regions {
                        mv.move_ring_f64(&mut r.polygon);
                    }
                }
                if let Some(regions) = self.step_regions.get_mut(idx) {
                    for r in regions {
                        for p in &mut r.polygon {
                            mv.move_point_f32(p);
                        }
                    }
                }
                if let Some(regions) = self.ceiling_step_regions.get_mut(idx) {
                    for r in regions {
                        for p in &mut r.polygon {
                            mv.move_point_f32(p);
                        }
                    }
                }
            }
            Ok(Some(()))
        })();

        match result {
            Ok(None) => {
                self.edit_dimension = None;
                Ok(())
            }
            Ok(Some(())) => {
                // Whole-house re-resolve: refit floors/ceilings/roofs, re-derive
                // exposure + InterStorey, re-link openings off the new anchors.
                self.refresh_all_storeys();
                self.revision = self.revision.wrapping_add(1);
                self.derived_revision = self.revision;
                self.push_history_snapshot(undo_snap);
                self.edit_dimension = None;
                Ok(())
            }
            Err(e) => {
                // A follower storey / record transform may have mutated before
                // the failure — the snapshot restore is the transaction rollback.
                self.restore_history_snapshot(undo_snap);
                Err(e)
            }
        }
    }
}
