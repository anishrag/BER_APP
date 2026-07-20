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
use manip_plan::{
    crossing_distances_along, lerp_along, project_distance_along as project_distance,
    segment_len as wall_len,
};

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

/// What a dragged section handle snapped to — the toolbar readout's hint.
/// Ordering is irrelevant (the nearest candidate wins); the kind only labels
/// the landing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExposureSnapKind {
    /// The wall's own start/end (0 or its full length).
    WallEnd,
    /// An existing exposure-section boundary on the same wall.
    SectionEdge,
    /// A construction-phase region edge crossing/meeting the wall.
    PhaseLine,
    /// A living-area wall meeting the outline wall.
    LivingAreaWall,
    /// A porch wall meeting the outline wall.
    PorchWall,
    /// A floor/ceiling step (riser) line meeting the wall.
    StepLine,
}

impl ExposureSnapKind {
    /// User-facing name for the readout hint.
    pub fn label(self) -> &'static str {
        match self {
            Self::WallEnd => "wall end",
            Self::SectionEdge => "section edge",
            Self::PhaseLine => "phase line",
            Self::LivingAreaWall => "living-area wall",
            Self::PorchWall => "porch wall",
            Self::StepLine => "step line",
        }
    }
}

/// Crossing/graze + dedupe tolerance (metres) for the along-wall snap
/// candidates — how exactly a reference line must meet the wall to count,
/// NOT the grab distance (that's the screen-scaled `tol` of the drag).
const SNAP_CANDIDATE_EPS: f32 = 0.01;

/// One outline wall for rendering: endpoints (world coords), its length, and
/// the exposure sections set on it. (The current selection renders through
/// [`ExposureSelection`], not per-wall flags.)
pub struct WallExposureView {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub length_m: f32,
    pub sections: Vec<ExposureSection>,
    /// For a FACE-PARTITIONED wall (which has no 1-D `sections`): the DISTINCT
    /// exposures across its face regions, first-seen order. Empty for an
    /// ordinary wall. The plan draws this wall as an INTERLEAVED line cycling
    /// these colours, so a mixed wall reads as mixed at a glance.
    pub partition_exposures: Vec<WallExposure>,
}

/// The selected wall's current section selection, for drawing handles and the
/// toolbar readout.
pub struct ExposureSelection {
    pub a: (f64, f64),
    pub b: (f64, f64),
    pub start_m: f32,
    pub end_m: f32,
    pub length_m: f32,
    /// What the dragged handle is currently snapped to, as a user-facing
    /// label ("phase line", "section edge", …) — `None` while not snapped.
    pub snap_hint: Option<&'static str>,
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
        // Leaving the outline draw tool: drop any un-decided re-draw stroke
        // (a stray second outline) so it doesn't linger on the canvas. (n3)
        self.pending_redraw = None;
        // The wall-face partition view lives under exposure mode; leaving (or
        // re-entering) exposure must not leave it stranded over another tool.
        self.close_wall_partition();
        self.exposure_mode = on;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        self.exposure_snap_hint = None;
        self.selected_riser = None;
        if on {
            self.exit_openings_mode();
            // Flush the live buffer to its slot first: `activate_shape(Outline)`
            // early-returns without syncing when the outline is already active,
            // which would leave `self.document.outline.floor_plan` stale (the renderer
            // reads it). Then leave living-area mode and act on the outline.
            self.sync_active();
            self.set_living_area_mode(false);
            self.set_porch_mode(false);
            self.phase_mode = false;
            self.step_mode = false;
            // Mutually exclusive with the ceiling-step line and roof-point
            // tools too.
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.ceiling_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            self.dormer_mode = false;
            self.pending_rect_is_dormer = false;
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
        // A fresh press starts with no snap landing (the first drag frame
        // recomputes it).
        self.exposure_snap_hint = None;
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
        // Hit-test against the OUTLINE slot — the same source the renderer
        // (`wall_exposure_views`) and `selected_wall_geom` read. Exposure mode
        // does NOT load the outline into the live edit buffer
        // (`document.floor_plan`, see `set_exposure_mode`); on a storey whose
        // active shape is a living area (etc.) the live buffer holds DIFFERENT
        // geometry + VertexIds, so hit-testing it yields a vid absent from the
        // outline's `perimeter_walls()` and the wall silently fails to select.
        match self
            .document
            .outline
            .floor_plan
            .hit_test_wall(world_x, world_y, tol)
        {
            Some((0, edge_start)) => {
                self.selected_exposure_wall =
                    self.document.outline.floor_plan.vertex_id_at(0, edge_start);
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
    /// geometric discriminator lives there). Derived `InterStorey` risers
    /// (buried behind a neighbour storey — `derive_inter_storey`'s stamp) are
    /// EXCLUDED: they aren't pickable, drawable or paintable, so every
    /// consumer (hit-test, views, the bulk pass) sees only the labellable
    /// ones. Empty when no record/storey.
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
        self.document
            .riser_walls(storey)
            .into_iter()
            .filter(|r| r.exposure != WallExposure::InterStorey)
            .collect()
    }

    /// The active storey's floor + ceiling STEP lines as world-metre reference
    /// segments — each riser's plan base edge from [`riser_walls`](Self::riser_walls)
    /// (so buried `InterStorey` risers are already excluded, and BOTH
    /// `RiserKind::Floor` and `RiserKind::Ceiling` base edges are included). The
    /// along-wall snap in exposure mode ([`exposure_snap_candidates`]) and the
    /// phase-area carve both level onto these, so a boundary drawn beside a step
    /// lands flush on it. ONE source of truth for "where the step lines are".
    /// Empty when the storey has no steps.
    ///
    /// [`exposure_snap_candidates`]: Self::exposure_snap_candidates
    pub(super) fn step_line_segments(&self) -> Vec<[Point2f32; 2]> {
        self.riser_walls()
            .into_iter()
            .map(|r| {
                [
                    Point2f32::new(r.a.x as f32, r.a.y as f32),
                    Point2f32::new(r.b.x as f32, r.b.y as f32),
                ]
            })
            .collect()
    }

    /// Drag the grabbed handle: project the pointer onto the wall line and set
    /// that end's distance (clamped to the wall), SNAPPING to the nearest
    /// meaningful along-wall coordinate within `tol` (world metres — the same
    /// screen-scaled tolerance the handle grab uses): phase-region lines,
    /// existing section edges, living-area walls, step lines, and the wall's
    /// own ends (see [`exposure_snap_candidates`]). The landing's kind is kept
    /// as the readout hint. No-op if no handle is held.
    ///
    /// [`exposure_snap_candidates`]: Self::exposure_snap_candidates
    pub fn exposure_pointer_drag(&mut self, world_x: f64, world_y: f64, tol: f64) {
        let Some(handle) = self.exposure_drag else {
            return;
        };
        let Some((a, b, len)) = self.selected_wall_geom() else {
            return;
        };
        let Some((mut s_m, mut e_m)) = self.exposure_sel else {
            return;
        };
        let mut d = project_distance(a, b, len, world_x as f32, world_y as f32);
        // Snap to the NEAREST candidate within tolerance (live, every drag
        // frame — dragging away just as smoothly un-snaps).
        self.exposure_snap_hint = None;
        let mut best: Option<(f32, f32, ExposureSnapKind)> = None; // (dist, at, kind)
        for (at, kind) in self.exposure_snap_candidates(a, b, len) {
            let dist = (at - d).abs();
            if dist <= tol as f32 && best.is_none_or(|(bd, _, _)| dist < bd) {
                best = Some((dist, at, kind));
            }
        }
        if let Some((_, at, kind)) = best {
            d = at;
            self.exposure_snap_hint = Some(kind);
        }
        match handle {
            SectionHandle::Start => s_m = d,
            SectionHandle::End => e_m = d,
        }
        self.exposure_sel = Some((s_m, e_m));
    }

    /// The along-wall coordinates a section handle should land on, as
    /// `(metres-from-start, kind)`: the wall's ends, the wall's existing
    /// section boundaries, and the crossings of phase-region edges,
    /// living-area walls, and riser (step) lines with the wall — the shell
    /// only GATHERS the reference segments; the crossing math is
    /// `manip_plan::crossing_distances_along`. Unsorted (the drag takes the
    /// nearest within tolerance).
    fn exposure_snap_candidates(
        &self,
        a: Point2f32,
        b: Point2f32,
        len: f32,
    ) -> Vec<(f32, ExposureSnapKind)> {
        let mut out = vec![
            (0.0, ExposureSnapKind::WallEnd),
            (len, ExposureSnapKind::WallEnd),
        ];
        if let Some(secs) = self
            .selected_exposure_wall
            .and_then(|v| self.document.wall_sections.get(&v))
        {
            for s in secs {
                out.push((s.start_m, ExposureSnapKind::SectionEdge));
                out.push((s.end_m, ExposureSnapKind::SectionEdge));
            }
        }
        let mut crossings = |segs: &[[Point2f32; 2]], kind: ExposureSnapKind| {
            for d in crossing_distances_along(a, b, len, segs, SNAP_CANDIDATE_EPS) {
                out.push((d, kind));
            }
        };
        crossings(&self.phase_reference_walls(), ExposureSnapKind::PhaseLine);
        crossings(
            &self.document.living_area_wall_segments(),
            ExposureSnapKind::LivingAreaWall,
        );
        crossings(
            &self.document.porch_wall_segments(),
            ExposureSnapKind::PorchWall,
        );
        crossings(&self.step_line_segments(), ExposureSnapKind::StepLine);
        out
    }

    /// End a handle drag.
    pub fn exposure_pointer_up(&mut self) {
        self.exposure_drag = None;
        self.exposure_snap_hint = None;
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
        if self.set_riser_exposure_by_id(id, exposure) {
            let _ = self.mark_dirty();
        }
    }

    /// The record + side-table mutation of [`set_riser_exposure`], by riser
    /// id — shared with the bulk "set remaining exposed" pass. Returns whether
    /// anything changed (the caller marks dirty). Same no-undo semantics.
    /// No-op when the target riser currently classifies `InterStorey` (the
    /// derivation owns it) or when `InterStorey` is passed as the value (it's
    /// derived-only, never assessor-set).
    ///
    /// [`set_riser_exposure`]: Self::set_riser_exposure
    fn set_riser_exposure_by_id(&mut self, id: ItemId, exposure: WallExposure) -> bool {
        if exposure == WallExposure::InterStorey {
            return false; // derived-only classification — never assessor-set
        }
        let Some(idx) = self.current_storey_idx else {
            return false;
        };
        // The riser's durable base-edge key, computed before the &mut borrow.
        let key = self
            .riser_walls()
            .into_iter()
            .find(|r| r.id == id)
            .map(|r| document::riser_key(r.a, r.b));
        let Some(region) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx))
            .and_then(|s| s.walls.regions.iter_mut().find(|r| r.id == id))
        else {
            return false;
        };
        if region.exposure == WallExposure::InterStorey {
            return false; // the derivation owns this riser — refuse the write
        }
        if region.exposure == exposure {
            return false;
        }
        region.exposure = exposure;
        // Record the override in the durable side-table — the source of truth the
        // resolve stamps from, so it survives the wall re-derivation.
        if let (Some(key), Some(sd)) = (key, self.document.storeys.get_mut(idx)) {
            sd.riser_exposure.insert(key, exposure);
        }
        true
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
    /// unmarked walls and the unset gaps around painted sections — AND every
    /// still-`Unset` riser face. Painted sections and classified risers are
    /// untouched. The outline part is undoable and persists immediately; the
    /// riser part is record-level, outside undo, like a single riser set
    /// ([`set_riser_exposure`]). No-op if nothing changes.
    ///
    /// [`set_riser_exposure`]: Self::set_riser_exposure
    pub fn set_all_remaining_exposed(&mut self) {
        self.sync_active();
        let undo_snap = self.current_history_snapshot();
        if self.document.set_all_remaining_exposed() {
            let _ = self.mark_dirty();
            self.push_history_snapshot(undo_snap);
        }
        // Risers: whole-face, derived Unset until classified — the bulk pass
        // is how "everything left is just exposed" covers them too.
        let unset: Vec<ItemId> = self
            .riser_walls()
            .into_iter()
            .filter(|r| r.exposure == WallExposure::Unset)
            .map(|r| r.id)
            .collect();
        let mut riser_changed = false;
        for id in unset {
            riser_changed |= self.set_riser_exposure_by_id(id, WallExposure::Exposed);
        }
        if riser_changed {
            let _ = self.mark_dirty();
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
        let height = self.face_height();
        self.document
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .map(|(vid, a, b)| {
                let length_m = wall_len(a, b);
                // A face-partitioned wall carries its exposure in 2-D face
                // regions, not 1-D sections — collect the DISTINCT exposures so
                // the plan can draw it interleaved. (The exposure SET is
                // height-independent, so any positive face height works.)
                let partition_exposures = if self.document.is_wall_partitioned(vid) {
                    let mut exps: Vec<WallExposure> = Vec::new();
                    for (_, e) in self.document.partition_face_regions(vid, length_m, height) {
                        if !exps.contains(&e) {
                            exps.push(e);
                        }
                    }
                    exps
                } else {
                    Vec::new()
                };
                WallExposureView {
                    a: (a.x as f64, a.y as f64),
                    b: (b.x as f64, b.y as f64),
                    length_m,
                    sections: self
                        .document
                        .wall_sections
                        .get(&vid)
                        .cloned()
                        .unwrap_or_default(),
                    partition_exposures,
                }
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
            snap_hint: self.exposure_snap_hint.map(ExposureSnapKind::label),
        })
    }
}
