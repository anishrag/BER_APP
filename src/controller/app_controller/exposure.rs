//! Wall-exposure marking: classifying the storey's perimeter (envelope) walls
//! by heat loss — `Exposed` (open to outside), `SemiExposed` (borders an
//! unheated space or a neighbour's dwelling), or `NonHeatLoss` (party/internal).
//!
//! A wall can carry several exposure SECTIONS along its length (each a range in
//! metres from the wall's start vertex). Each section is an independent,
//! single-exposure piece — at the BER layer a wall splits into one `WallRegion`
//! per coverage range (never a fused multi-exposure region). Un-marked length
//! is genuinely `Unset` — it derives an `Unset` region (drawn grey) rather
//! than being assumed `Exposed`. Exposure is keyed by each wall's
//! stable start `VertexId`, lives on the OUTLINE only (living-area interior
//! walls aren't envelope/heat-loss elements), and is derived into BER
//! `WallRegions` on commit (see `commit_to_ber`).
//!
//! Sections are full-height for now. A future face/height split (e.g. a dormer
//! side wall cut diagonally into exposed top + semi-exposed bottom) can be added
//! by giving `ExposureSection` a face subdivision — see the plan; the BER output
//! (`WallRegion.polygon` is an arbitrary `Polygon3D`) already allows it.
use super::*;
use document::{range_at, RiserKind};
use manip_plan::{lerp_along, project_distance_along as project_distance, segment_len as wall_len};

/// A tap selects a riser only within this fraction of the wall-pick
/// tolerance. Risers get FIRST claim inside the tighter band (their dashed
/// lines are interior and thin — a deliberate tap on one shouldn't lose to a
/// perimeter wall that happens to be within the looser tolerance), while a
/// tap that's merely NEAR a riser but comfortably within wall range still
/// picks the wall.
const RISER_PICK_TOL_FRACTION: f64 = 0.6;

/// Which end of the current selection a drag is moving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SectionHandle {
    Start,
    End,
}

/// One outline wall for rendering: endpoints (world coords), its length, and
/// the exposure sections set on it. (The current selection renders through
/// [`ExposureSelection`], not per-wall flags.)
pub struct WallExposureView {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub length_m: f32,
    pub sections: Vec<ExposureSection>,
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

/// One derived RISER wall for rendering/selection: its stable region id, its
/// plan base edge (world coords), exposure, kind (floor/ceiling step — the
/// toolbar's label; the clear-default is `Unset` for both kinds), and whether
/// it's the current riser selection. Drawn as a DASHED line so it reads apart
/// from the outline walls. (`id` is the stable identity — `with_risers`'
/// amend keeps it across re-derivations; the UI itself selects via pointer
/// taps, not ids.)
pub struct RiserView {
    pub id: ItemId,
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub exposure: WallExposure,
    pub kind: RiserKind,
    pub selected: bool,
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
        self.selected_riser = None;
        if on {
            self.exit_openings_mode();
            // Flush the live buffer to its slot first: `activate_shape(Outline)`
            // early-returns without syncing when the outline is already active,
            // which would leave `self.document.outline.floor_plan` stale (the renderer
            // reads it). Then leave living-area mode and act on the outline.
            self.sync_active();
            self.set_living_area_mode(false);
            // Mutually exclusive with the ceiling-step line and roof-point
            // tools too.
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.roof_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
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
        // 2. A riser's dashed line gets first claim, inside a TIGHTER band
        //    than the wall pick (see `RISER_PICK_TOL_FRACTION`). Risers are
        //    whole-face selections — no section range, no handles.
        if let Some(id) = self.riser_hit(world_x, world_y, tol * RISER_PICK_TOL_FRACTION) {
            self.selected_riser = Some(id);
            self.selected_exposure_wall = None;
            self.exposure_sel = None;
            self.exposure_drag = None;
            return;
        }
        // 3. Otherwise (re)select the wall under the pointer (ring 0 only),
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
                self.selected_riser = None;
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
                self.selected_riser = None;
            }
        }
    }

    /// The riser whose base edge is nearest the point, within `tol` (world
    /// metres). Distances via the same point-to-segment primitive the
    /// document's recovery matching uses.
    fn riser_hit(&self, world_x: f64, world_y: f64, tol: f64) -> Option<ItemId> {
        self.riser_walls()
            .into_iter()
            .map(|r| {
                let d = domain::points::point_to_segment_distance(
                    world_x, world_y, r.a.x, r.a.y, r.b.x, r.b.y,
                );
                (d, r.id)
            })
            .filter(|(d, _)| *d <= tol)
            .min_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, id)| id)
    }

    /// The active storey's derived riser walls, classified by the document
    /// model from the materialized record (`StoreyDoc::riser_walls` — the
    /// geometric discriminator lives there). Empty when no record/storey.
    fn riser_walls(&self) -> Vec<document::RiserWall> {
        let Some(idx) = self.current_storey_idx else {
            return Vec::new();
        };
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        self.document.riser_walls(storey)
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

    /// Paint the current selection range on the selected wall with `exposure`.
    /// Only the painted section is stored — leftover length around the mark
    /// stays unset (it derives `Unset` `WallRegion`s on commit, so a partly
    /// marked wall still splits into one region per coverage range). Undoable;
    /// persists immediately. With a RISER selected instead, the assignment
    /// routes to it (`set_riser_exposure`) — whole-face, outside undo.
    pub fn set_wall_exposure(&mut self, exposure: WallExposure) {
        if self.selected_riser.is_some() {
            self.set_riser_exposure(exposure);
            return;
        }
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

    /// Set the selected RISER's exposure: mutate its region in the
    /// materialized record by stable id, then mark dirty (the next
    /// materialize re-derives — `with_risers`' amend PRESERVES the override,
    /// and the idle autosave persists the record, so it also survives
    /// reload).
    ///
    /// UNDO semantics, honestly: the override lives in the RECORD, not the
    /// `Document`, so it sits OUTSIDE undo (a `HistorySnapshot` is a
    /// `Document` clone) — the same deal as phase/step/ceiling-step areas
    /// and roof apexes. No snapshot is pushed: pushing one would add a no-op
    /// undo entry (and clear redo) without ever reverting the riser. The
    /// "Clear" button is the way back (to the kind's derived default).
    pub fn set_riser_exposure(&mut self, exposure: WallExposure) {
        let Some(id) = self.selected_riser else {
            return;
        };
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        let Some(region) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx))
            .and_then(|s| s.walls.regions.iter_mut().find(|r| r.id == id))
        else {
            return;
        };
        if region.exposure == exposure {
            return;
        }
        region.exposure = exposure;
        let _ = self.mark_dirty();
    }

    /// Revert the selected riser to its DERIVED default exposure — `Unset`
    /// for both kinds, like every other wall (see
    /// `RiserKind::default_exposure`). Same record-level/no-undo semantics
    /// as `set_riser_exposure`.
    pub fn clear_riser_exposure(&mut self) {
        let Some(id) = self.selected_riser else {
            return;
        };
        let Some(kind) = self
            .riser_walls()
            .into_iter()
            .find(|r| r.id == id)
            .map(|r| r.kind)
        else {
            return;
        };
        self.set_riser_exposure(kind.default_exposure());
    }

    /// Remove all exposure sections from the selected wall (it reverts to
    /// unset). Undoable; persists immediately. With a RISER selected instead,
    /// reverts it to its derived default (`clear_riser_exposure`).
    pub fn clear_wall_exposure(&mut self) {
        if self.selected_riser.is_some() {
            self.clear_riser_exposure();
            return;
        }
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

    /// Paint every unset coverage range on the outline `Exposed` — whole
    /// unmarked walls and the unset gaps around painted sections. Painted
    /// sections themselves are untouched. Undoable; persists immediately.
    /// No-op if nothing changes.
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
            })
            .collect()
    }

    /// The derived RISER walls of the active storey with selection state, for
    /// the 2D dashed-line rendering and the riser toolbar context. Reads the
    /// materialized record (call after `materialize` — the render loop
    /// already does); classification is `StoreyDoc::riser_walls`.
    pub fn riser_views(&self) -> Vec<RiserView> {
        self.riser_walls()
            .into_iter()
            .map(|r| RiserView {
                id: r.id,
                a: (r.a.x, r.a.y),
                b: (r.b.x, r.b.y),
                exposure: r.exposure,
                kind: r.kind,
                selected: self.selected_riser == Some(r.id),
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
