//! Wall-exposure marking: classifying the storey's perimeter (envelope) walls
//! by heat loss — `Exposed` (open to outside), `SemiExposed` (borders an
//! unheated space or a neighbour's dwelling), or `NonHeatLoss` (party/internal).
//!
//! A wall can carry several exposure SECTIONS along its length (each a range in
//! metres from the wall's start vertex). Each section is an independent,
//! single-exposure piece — at the BER layer a wall splits into one `WallRegion`
//! per section (never a fused multi-exposure region). Un-marked length defaults
//! to `Exposed` (the envelope assumption). Exposure is keyed by each wall's
//! stable start `VertexId`, lives on the OUTLINE only (living-area interior
//! walls aren't envelope/heat-loss elements), and is derived into BER
//! `WallRegions` on commit (see `commit_to_ber`).
//!
//! Sections are full-height for now. A future face/height split (e.g. a dormer
//! side wall cut diagonally into exposed top + semi-exposed bottom) can be added
//! by giving `ExposureSection` a face subdivision — see the plan; the BER output
//! (`WallRegion.polygon` is an arbitrary `Polygon3D`) already allows it.
use super::*;
use document::range_at;
use manip_2d_plan::{
    lerp_along, project_distance_along as project_distance, segment_len as wall_len,
};

/// Which end of the current selection a drag is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionHandle {
    Start,
    End,
}

/// One outline wall for rendering: endpoints (world coords), its length, the
/// exposure sections set on it, and whether it's selected.
pub struct WallExposureView {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub length_m: f32,
    pub sections: Vec<ExposureSection>,
    pub selected: bool,
}

/// The selected wall's current section selection, for drawing handles and the
/// toolbar readout.
pub struct ExposureSelection {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub start_m: f32,
    pub end_m: f32,
    pub length_m: f32,
}

impl AppController {
    /// True when the editor is in wall-exposure mode (drives the UI toggle and
    /// routes a canvas tap/drag to wall selection rather than drawing).
    pub fn exposure_mode(&self) -> bool {
        self.exposure_mode
    }

    /// Enter or leave wall-exposure mode. Exposure applies to the storey
    /// envelope, so entering forces the outline active (and leaves living-area
    /// mode). All selection/drag state is cleared.
    pub fn set_exposure_mode(&mut self, on: bool) {
        self.exposure_mode = on;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        if on {
            // Flush the live buffer to its slot first: `activate_shape(Outline)`
            // early-returns without syncing when the outline is already active,
            // which would leave `self.document.outline.floor_plan` stale (the renderer
            // reads it). Then leave living-area mode and act on the outline.
            self.sync_active();
            self.set_living_area_mode(false);
        }
    }

    /// Endpoints + length of the selected wall, if any.
    fn selected_wall_geom(&self) -> Option<(Point2f32, Point2f32, f32)> {
        let vid = self.selected_exposure_wall?;
        self.document
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(v, _, _)| *v == vid)
            .map(|(_, a, b)| (a, b, wall_len(a, b)))
    }

    /// Primary press in exposure mode. If a selection handle is within `tol` of
    /// the pointer, begin dragging it; otherwise select the wall under the
    /// pointer (defaulting its selection to the whole wall). Tapping empty space
    /// clears the selection.
    pub fn exposure_pointer_down(&mut self, world_x: f64, world_y: f64, tol: f64) {
        // 1. Grab a selection handle if the pointer is on one.
        if let (Some((a, b, len)), Some((s_m, e_m))) =
            (self.selected_wall_geom(), self.exposure_sel)
        {
            let (px, py) = (world_x as f32, world_y as f32);
            let (sx, sy) = lerp_along(a, b, len, s_m);
            let (ex, ey) = lerp_along(a, b, len, e_m);
            let t2 = (tol * tol) as f32;
            let ds = (px - sx).powi(2) + (py - sy).powi(2);
            let de = (px - ex).powi(2) + (py - ey).powi(2);
            if ds <= t2 || de <= t2 {
                self.exposure_drag = Some(if ds <= de {
                    SectionHandle::Start
                } else {
                    SectionHandle::End
                });
                return;
            }
        }
        // 2. Otherwise (re)select the wall under the pointer (ring 0 only),
        //    picking just the SUB-SECTION under the tap — the coverage range (a
        //    marked section or an un-marked gap) containing the tapped distance.
        //    So once a wall is split, tapping a leftover piece selects only that
        //    piece, not the whole wall.
        match self
            .document
            .floor_plan
            .hit_test_wall(world_x, world_y, tol)
        {
            Some((0, edge_start)) => {
                self.selected_exposure_wall = self.document.floor_plan.vertex_id_at(0, edge_start);
                self.exposure_drag = None;
                self.exposure_sel = if let Some((a, b, len)) = self.selected_wall_geom() {
                    let d = project_distance(a, b, len, world_x as f32, world_y as f32);
                    let range = match self
                        .selected_exposure_wall
                        .and_then(|v| self.document.wall_sections.get(&v))
                    {
                        Some(secs) => range_at(len, secs, d),
                        None => (0.0, len),
                    };
                    Some(range)
                } else {
                    None
                };
            }
            _ => {
                self.selected_exposure_wall = None;
                self.exposure_sel = None;
                self.exposure_drag = None;
            }
        }
    }

    /// Drag the grabbed handle: project the pointer onto the wall line and set
    /// that end's distance (clamped to the wall). No-op if no handle is held.
    pub fn exposure_pointer_drag(&mut self, world_x: f64, world_y: f64) {
        let Some(handle) = self.exposure_drag else {
            return;
        };
        let Some((a, b, len)) = self.selected_wall_geom() else {
            return;
        };
        let Some((mut s_m, mut e_m)) = self.exposure_sel else {
            return;
        };
        let d = project_distance(a, b, len, world_x as f32, world_y as f32);
        match handle {
            SectionHandle::Start => s_m = d,
            SectionHandle::End => e_m = d,
        }
        self.exposure_sel = Some((s_m, e_m));
    }

    /// End a handle drag.
    pub fn exposure_pointer_up(&mut self) {
        self.exposure_drag = None;
    }

    /// Set the selection range directly (the typed-distance path). Clamped to
    /// the wall; no-op if no wall is selected.
    pub fn set_exposure_section_range(&mut self, start_m: f32, end_m: f32) {
        let Some((_, _, len)) = self.selected_wall_geom() else {
            return;
        };
        self.exposure_sel = Some((start_m.clamp(0.0, len), end_m.clamp(0.0, len)));
    }

    /// Paint the current selection range on the selected wall with `exposure`,
    /// then materialize the WHOLE wall into explicit sections — any leftover
    /// length around the mark becomes its own `Exposed` section. So marking a
    /// range inside a wall splits it into N explicit sections (and N
    /// `WallRegion`s on commit). Undoable; persists immediately.
    pub fn set_wall_exposure(&mut self, exposure: WallExposure) {
        let Some(vid) = self.selected_exposure_wall else {
            return;
        };
        let Some((s, e)) = self.exposure_sel else {
            return;
        };
        let Some((_, _, len)) = self.selected_wall_geom() else {
            return;
        };
        let undo_snap = self.current_history_snapshot();
        self.document.set_wall_section(vid, s, e, len, exposure);
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// Remove all exposure sections from the selected wall (it reverts to the
    /// default Exposed assumption). Undoable; persists immediately.
    pub fn clear_wall_exposure(&mut self) {
        let Some(vid) = self.selected_exposure_wall else {
            return;
        };
        if !self.document.wall_sections.contains_key(&vid) {
            return;
        }
        let undo_snap = self.current_history_snapshot();
        self.document.clear_wall_section(vid);
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo_snap);
    }

    /// Mark every outline wall with no sections yet as fully `Exposed` (one
    /// full-length section). Walls that already carry sections are untouched.
    /// Undoable; persists immediately. No-op if nothing changes.
    pub fn set_all_remaining_exposed(&mut self) {
        self.sync_active();
        let undo_snap = self.current_history_snapshot();
        if self.document.set_all_remaining_exposed() {
            let _ = self.mark_dirty();
            self.push_history_snapshot(undo_snap);
        }
    }

    /// The outline perimeter walls with their sections + selection state, for
    /// rendering. World coords (f64), ring order.
    ///
    /// Sections are sourced from the in-memory `Storey.walls` (the `Ber` struct
    /// in `current_record`) by geometry-matching each `WallRegion` to its
    /// perimeter edge — BER data rendered from BER. The runtime `wall_sections`
    /// cache stays the editing model (where paint/clear/`set_remaining`
    /// mutate); on every commit those changes flow into `Storey.walls`, which
    /// is what this method then reads back. Selection is interaction state and
    /// stays live (`selected_exposure_wall` keyed by `VertexId`).
    pub fn wall_exposure_views(&self) -> Vec<WallExposureView> {
        // Sections come DIRECTLY from the editing model (`document.wall_sections`,
        // where paint/clear/set-remaining mutate) — no round-trip through the
        // committed BER's `WallRegions`. Selection is live interaction state.
        self.document
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .map(|(vid, a, b)| WallExposureView {
                a: (a.x as f64, a.y as f64),
                b: (b.x as f64, b.y as f64),
                length_m: wall_len(a, b),
                sections: self
                    .document
                    .wall_sections
                    .get(&vid)
                    .cloned()
                    .unwrap_or_default(),
                selected: self.selected_exposure_wall == Some(vid),
            })
            .collect()
    }

    /// The current section selection on the selected wall (for handles + the
    /// toolbar readout), or `None` when no wall is selected.
    pub fn exposure_selection(&self) -> Option<ExposureSelection> {
        let (a, b, len) = self.selected_wall_geom()?;
        let (s, e) = self.exposure_sel?;
        Some(ExposureSelection {
            a: (a.x as f64, a.y as f64),
            b: (b.x as f64, b.y as f64),
            start_m: s,
            end_m: e,
            length_m: len,
        })
    }
}
