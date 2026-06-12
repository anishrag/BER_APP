//! The editable model of ONE storey — the "document" the controller edits.
//!
//! `Document` owns the storey's shapes (the outline + its living areas + an
//! in-flight phase area), which one is active, and the geometric relationships
//! *between* them (a sub-area carves against the outline, inherits its scale,
//! shares its walls). It is UI-free and DB-free: the controller (`AppController`)
//! is the shell that drives it — translating UI events, owning the undo stack
//! and persistence, and deriving UI-facing view data.
//!
//! `Document` owns the full editable state: the shapes + which is active, the
//! live editing buffer (the active shape checked out — `floor_plan` +
//! `dimensioning` + `edit_state` + `pending_arc`), and per-wall exposure. It also
//! owns the editing *operations* that are pure model mutation (the slot
//! mechanism, cross-shape geometry, the dimensioning loop). Operations that need
//! the shell — undo, persistence, UI prompts — stay on `AppController` and call
//! in here.

mod ceiling_step;
mod debug;
mod dimensioning;
mod edit;
mod exposure;
mod load;
mod phase;
mod position;
mod resolve;
mod roof_point;
mod step;

pub use ceiling_step::{
    ceiling_step_regions_from_roofs, CeilingStepRegion, LinePositionOption, LinePositionPrompt,
    LineSide,
};
pub use debug::debug_dump_storey;
/// Derive full per-wall exposure coverage — the shell's `commit_to_ber` uses it.
pub use exposure::{range_at, wall_coverage, RiserKind, RiserWall};
pub use phase::{phase_regions_from_floors, stamp_wall_phases, PhaseRegion};
pub use position::{PositionOption, PositionPrompt, PositionPromptKind};
pub use resolve::{ensure_storey_constructions, resolve_storey};
pub use roof_point::{
    centred_rect_lo, centred_ridge_cross, centred_ridge_start, validate_roof_apex,
    validate_roof_rect_lengths, validate_roof_rect_placement, validate_roof_rect_ridge,
    validate_roof_ridge, validate_roof_ridge_extent, validate_roof_ridge_length,
};
pub use step::{inherited_step_regions, step_regions_from_floors, StepRegion};

use datatypes::ber::structural::WallExposure;
use domain::points::Point2f32;
use manip_2d_plan::{
    Error, FloorPlan, ParameterAxis, ParameterId, SubareaOutcome, SubareaTuning, VertexId,
};
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};

/// A single "what's the length of this wall?" question to put to the assessor.
#[derive(Debug, Clone)]
pub struct DimensionPrompt {
    /// Stable identifier of the wall's starting vertex. Survives reflows so
    /// the prompt can be carried across other operations without going stale.
    pub wall_start_vertex: VertexId,
    /// The currently-unknown parameter this wall constrains. Setting it from
    /// the user's entered length is what "applying the dimension" does.
    pub controlled_param: ParameterId,
}

/// Tracks the user-confirmed dimensions on a shape. Before any dimension is
/// entered the polygon is a pixel-scale notepad sketch; once the first dimension
/// lands, the whole plan converts to real units (metres) and each subsequent
/// dimension pins another parameter.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct DimensionState {
    /// Parameter IDs the user has explicitly entered a length for.
    pub known_params: HashSet<ParameterId>,
    /// Current "which wall do I ask about next?" prompt, or `None` when all
    /// dimensions have been supplied (or there's nothing to dimension yet).
    /// Transient — re-derived after load, so excluded from the snapshot.
    #[serde(skip)]
    pub current_prompt: Option<DimensionPrompt>,
    /// True once the first dimension is entered — from that point on the
    /// plan is in real units rather than pixels.
    pub scale_set: bool,
    /// Walls the user has explicitly dimensioned, identified by the stable
    /// VertexId of each wall's starting vertex. The UI uses this to decide
    /// which length labels to render — only user-set walls show lengths;
    /// derived/un-set walls show no label.
    pub dimensioned_walls: Vec<VertexId>,
    /// Walls the user explicitly skipped (by VertexId of the wall's start).
    /// Skip is per-wall, not per-param: skipping wall A doesn't prevent the
    /// user being asked about wall B even if both would set the same param.
    /// A param controlled by a skipped wall just isn't pinned by that wall;
    /// if no other wall can set it, the param stays un-dimensioned.
    pub skipped_walls: HashSet<VertexId>,
    /// Axes along which the shape's drawn POSITION is approximate — the carve
    /// boundary didn't pin it. Set once by [`StoreyDoc::pinned_slot_for`]
    /// against the boundary (which isn't retained later). Living areas carry
    /// the record too but are never position-prompted.
    #[serde(default)]
    pub position_free_axes: Vec<ParameterAxis>,
    /// Free axes whose position has been dealt with: a distance entered, the
    /// drawn position explicitly accepted (skip), or auto-resolved (logged)
    /// because there was no outline reference to measure against.
    #[serde(default)]
    pub position_resolved_axes: Vec<ParameterAxis>,
    /// Params whose `Wall`-kind position prompt was skipped or auto-resolved
    /// (the wall — e.g. a cut line — stays at its drawn coordinate).
    #[serde(default)]
    pub position_skipped_params: HashSet<ParameterId>,
    /// Current "how far is this area from the outline?" prompt, or `None`.
    /// Transient — re-derived by `recompute_position_prompt` (hooked into
    /// `recompute_prompt`), so excluded from the snapshot.
    #[serde(skip)]
    pub current_position_prompt: Option<PositionPrompt>,
}

/// In-memory state for one editable shape (the storey outline or a living
/// area): its parametric plan plus the dimensioning progress against it. The
/// active shape's slot is mirrored live in the controller's editing buffer;
/// the inactive shapes rest in their slots until activated.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ShapeSlot {
    pub floor_plan: FloorPlan,
    pub dimensioning: DimensionState,
}

impl ShapeSlot {
    /// Finalised — nothing left to dimension: no wall the dimensioning loop
    /// would still prompt for. (The caller also checks the metre scale for the
    /// outline; living areas inherit it.)
    pub fn finalised(&self) -> bool {
        self.floor_plan
            .next_promptable_wall(
                &self.dimensioning.known_params,
                &self.dimensioning.skipped_walls,
            )
            .is_none()
    }
}

/// Which shape the editor is currently acting on. The controller's live buffer
/// mirrors this one; switching saves the live state back to its slot and loads
/// the target's.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub enum ActiveShape {
    /// The storey outline (the primary footprint).
    #[default]
    Outline,
    /// The living area at this index in `living_areas`.
    LivingArea(usize),
    /// The in-flight construction-phase area being dimensioned (held in
    /// `phase_shape`). There's only ever one at a time, so no index.
    PhaseArea,
    /// The in-flight floor-step area being dimensioned (held in `step_shape`).
    /// There's only ever one at a time, so no index.
    StepArea,
}

/// A provisional curved wall awaiting its two attach dimensions before being
/// faceted into the model. Only the two endpoints are spliced in (a straight
/// placeholder edge); once both attach stubs are dimensioned, `finish_arc`
/// facets the arc between them and this clears.
#[derive(Debug, Clone)]
pub struct PendingArc {
    /// Endpoint at the stroke's start (drawn first).
    pub start: VertexId,
    /// Endpoint at the stroke's end (drawn last).
    pub end: VertexId,
    /// Signed sagitta / chord (left of start→end). Scale-invariant: survives
    /// the pixel→metre rescale and any move of the endpoints by dimensioning.
    pub bulge_ratio: f32,
}

/// Edit-mode click/drag state — a cursor into the live `floor_plan` (its
/// `VertexId`s are only meaningful for whichever plan is currently checked out).
#[derive(Debug, Default, Clone)]
pub struct EditState {
    /// VertexId of the start vertex of the currently selected wall.
    pub selected_wall: Option<VertexId>,
    /// VertexId of a free vertex the user inserted via double-click and
    /// is currently dragging. Mutually exclusive with `selected_wall`.
    pub selected_vertex: Option<VertexId>,
    /// True while the user is holding the mouse button after clicking a wall
    /// or double-clicking to insert a vertex.
    pub dragging: bool,
    /// The parameter/constant value before the user started a wall drag —
    /// snapshotted on mouse-down so Cancel on the post-drag prompt can
    /// revert without re-loading the entire floor plan.
    pub pre_drag_value: Option<f32>,
    /// Kind of coord that a wall drag affects (parameter or anchor
    /// constant), stashed so `edit_mouse_up` can populate the post-drag
    /// prompt.
    pub pre_drag_kind: Option<MovedCoordKind>,
    /// Pre-drag world-coord position of a vertex drag. Used for revert
    /// if the user cancels the post-drop prompt.
    pub pre_drag_vertex_pos: Option<(f32, f32)>,
    /// True once we've taken an undo snapshot for the current drag. Reset
    /// on each mouse-down. Ensures only ONE snapshot per drag.
    pub drag_snapshot_taken: bool,
}

/// What kind of coordinate the just-moved wall is anchored to. The post-drag
/// prompt applies the user's entered distance by mutating either a parameter
/// (`set_parameter`) or a constant (`shift_constant`).
#[derive(Debug, Clone, Copy)]
pub enum MovedCoordKind {
    Parameter(ParameterId),
    /// The constant value as of mouse-down (`shift_constant` matches on this
    /// value so it knows which vertices to update).
    Constant(f32),
}

/// One heat-loss section along an outline wall: `[start_m, end_m)` from the
/// wall's start vertex, with its exposure class. A wall with no sections is
/// single-exposure; per wall they're kept sorted and non-overlapping.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExposureSection {
    pub start_m: f32,
    pub end_m: f32,
    pub exposure: WallExposure,
}

/// One storey's editable shapes and which is active. The building-level
/// [`Document`] holds one of these per storey; it `Deref`s to the active one, so
/// per-storey field/method access reads through transparently.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct StoreyDoc {
    /// The storey outline (primary footprint).
    pub outline: ShapeSlot,
    /// Potential living-area sub-shapes, drawn inside the storey the same way as
    /// the outline. The most-recent is the one the tools edit; the largest is
    /// chosen as THE living area later, but all are kept.
    pub living_areas: Vec<ShapeSlot>,
    /// The in-flight construction-phase area being dimensioned (a sub-shape, like
    /// a living area, but tagged with a phase at the end rather than kept).
    pub phase_shape: Option<ShapeSlot>,
    /// The in-flight floor-step area being dimensioned (a sub-shape, like a
    /// living area, but assigned a floor level at the end rather than kept).
    #[serde(default)]
    pub step_shape: Option<ShapeSlot>,
    /// Which shape the live editing buffer below currently mirrors.
    pub active: ActiveShape,

    // ── Live editing buffer ── the active shape, checked out for editing.
    // `sync_active`/`load_active_into_buffer` move state between this and the
    // active slot above.
    /// The active shape's parametric plan, live.
    pub floor_plan: FloorPlan,
    /// Dimensioning progress against `floor_plan`.
    pub dimensioning: DimensionState,
    /// Edit-mode click/drag cursor into `floor_plan`. Transient — not snapshotted.
    #[serde(skip)]
    pub edit_state: EditState,
    /// A provisional curved wall mid-flow against `floor_plan`, if any. Transient.
    #[serde(skip)]
    pub pending_arc: Option<PendingArc>,

    /// Exposure SECTIONS of each OUTLINE perimeter wall, keyed by the wall's
    /// stable start `VertexId` (survives edits). Holds ONLY painted sections;
    /// un-marked length derives into `Unset` BER `WallRegions` (it exists but
    /// carries no exposure claim until the assessor marks it). Part of the
    /// document (editable, undoable) but keyed to the outline, not the active
    /// shape.
    pub wall_sections: HashMap<VertexId, Vec<ExposureSection>>,
}

/// Of the available `pieces`, the one to carve the `stroke` against. Scored by
/// how much the stroke and the piece OVERLAP, counting BOTH directions, so it
/// works whether the stroke runs THROUGH the piece or LOOPS AROUND it:
/// - stroke points inside the piece (an open cut traversing it), plus
/// - piece vertices inside the stroke loop (a closed area drawn around it).
///
/// Ties (and the "stroke over already-taken space — no overlap anywhere" case)
/// break to the LARGEST piece. Returns `None` only when there are NO pieces (the
/// whole outline is taken). Crucially this never lets the caller fall back to the
/// full outline when free space exists, so a new area can't overlap an existing
/// one — the clip confines it to the available region (or rejects it).
fn pick_piece_for_stroke(pieces: Vec<FloorPlan>, stroke: &[Point2f32]) -> Option<FloorPlan> {
    pieces
        .into_iter()
        .map(|p| {
            let pts_in_piece = stroke.iter().filter(|s| p.contains_point(s.x, s.y)).count();
            let piece_in_stroke = p
                .get_points()
                .iter()
                .filter(|v| manip_2d_plan::ring_contains_point(stroke, v.x, v.y))
                .count();
            let overlap = pts_in_piece + piece_in_stroke;
            // Tie-break on ring area (the crate-shared shoelace in `step`).
            (overlap, step::ring_area2(&p.get_points()), p)
        })
        .max_by(|(oa, aa, _), (ob, ab, _)| oa.cmp(ob).then(aa.total_cmp(ab)))
        .map(|(_, _, p)| p)
}

/// The editable model of a whole BUILDING: one [`StoreyDoc`] per storey plus
/// which storey is active. It `Deref`s to the active storey, so the controller's
/// existing per-storey access (`document.floor_plan`, `document.sync_active()`)
/// transparently targets the active storey. Switching storeys is just moving
/// `active_storey` — each storey keeps its own parametric editing state, so a
/// switch is LOSSLESS (no rebuild from resolved points).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Document {
    /// One editable model per storey. Invariant: never empty (always ≥ 1), so
    /// `Deref` always has an active storey to target.
    pub storeys: Vec<StoreyDoc>,
    /// Which storey is being edited — the one `Deref` targets.
    pub active_storey: usize,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            storeys: vec![StoreyDoc::default()],
            active_storey: 0,
        }
    }
}

impl Deref for Document {
    type Target = StoreyDoc;
    fn deref(&self) -> &StoreyDoc {
        &self.storeys[self.active_storey]
    }
}

impl DerefMut for Document {
    fn deref_mut(&mut self) -> &mut StoreyDoc {
        &mut self.storeys[self.active_storey]
    }
}

impl Document {
    /// Switch the active storey, if `idx` is a valid storey. The target storey's
    /// own editing buffer is preserved across the switch.
    pub fn set_active_storey(&mut self, idx: usize) {
        if idx < self.storeys.len() {
            self.active_storey = idx;
        }
    }

    /// Append a fresh empty storey and make it active. Returns its index.
    pub fn push_storey(&mut self) -> usize {
        self.storeys.push(StoreyDoc::default());
        self.active_storey = self.storeys.len() - 1;
        self.active_storey
    }
}

impl StoreyDoc {
    /// Write the live editing buffer back into whichever slot is active, so the
    /// slots are current before they're read (persist / switch / snapshot).
    pub fn sync_active(&mut self) {
        let slot = ShapeSlot {
            floor_plan: self.floor_plan.clone(),
            dimensioning: self.dimensioning.clone(),
        };
        match self.active {
            ActiveShape::Outline => self.outline = slot,
            ActiveShape::LivingArea(i) => {
                if let Some(s) = self.living_areas.get_mut(i) {
                    *s = slot;
                }
            }
            ActiveShape::PhaseArea => self.phase_shape = Some(slot),
            ActiveShape::StepArea => self.step_shape = Some(slot),
        }
    }

    /// Load the active shape's slot into the live editing buffer.
    pub fn load_active_into_buffer(&mut self) {
        let slot = match self.active {
            ActiveShape::Outline => self.outline.clone(),
            ActiveShape::LivingArea(i) => self.living_areas.get(i).cloned().unwrap_or_default(),
            ActiveShape::PhaseArea => self.phase_shape.clone().unwrap_or_default(),
            ActiveShape::StepArea => self.step_shape.clone().unwrap_or_default(),
        };
        self.floor_plan = slot.floor_plan;
        self.dimensioning = slot.dimensioning;
    }

    /// Resolve a freehand sub-area stroke against the AVAILABLE region — the
    /// storey outline minus `holes` (the already-drawn regions of the same kind,
    /// which a new sub-area must not overlap). The available region can be
    /// disconnected; the cut runs against the piece the stroke is drawn in.
    /// Returns that piece (the caller pins walls against it) plus the outcome.
    /// `holes` empty → the whole outline (the relationship "sub-areas are carved
    /// against the outline" lives here, not in the caller).
    pub fn carve_or_cut_in_available(
        &self,
        stroke: &[Point2f32],
        holes: &[Vec<Point2f32>],
        tuning: &SubareaTuning,
    ) -> Result<(FloorPlan, SubareaOutcome), Error> {
        let pieces = self.outline.floor_plan.subtract_all(holes);
        let boundary = pick_piece_for_stroke(pieces, stroke)
            .unwrap_or_else(|| self.outline.floor_plan.clone());
        let outcome = boundary.carve_or_cut(stroke, tuning, false)?;
        Ok((boundary, outcome))
    }

    /// New storey outline via the living-area carve: trace `prev_ring` (the
    /// previous storey's footprint, in metres) as the reference, follow it where
    /// the `stroke` hugs it and build fresh walls where it diverges —
    /// `allow_outside` so overhangs/extensions past the old footprint are kept
    /// rather than clamped in. The carved plan is adopted into the live buffer
    /// with the metre scale inherited and every shared wall pinned
    /// ([`FloorPlan::params_pinned_to_outline`]) so it isn't re-prompted — only
    /// the genuinely-new walls remain to dimension. Returns
    /// `(pinned, active)` parameter counts for the shell's logging. On error the
    /// buffer is untouched.
    pub fn carve_outline_from_previous(
        &mut self,
        stroke: &[Point2f32],
        prev_ring: &[Point2f32],
        tuning: &SubareaTuning,
    ) -> Result<(usize, usize), String> {
        let prev_plan = FloorPlan::from_clean_ring(prev_ring)
            .map_err(|e| format!("Failed to build reference outline: {:?}", e))?;
        let outcome = prev_plan
            .carve_or_cut(stroke, tuning, true)
            .map_err(|e| format!("Failed to trace new storey: {:?}", e))?;
        let new_plan = match outcome {
            SubareaOutcome::Area(plan) => plan,
            // An outline trace is a closed loop → an Area; a Cut only arises from
            // an open/partial trace, where the first region is the drawn side.
            SubareaOutcome::Cut(a, _b) => {
                log::info!(
                    "[new-storey] carve resolved to a CUT (open trace?) — using first region"
                );
                a
            }
        };
        self.dimensioning = DimensionState::default();
        self.dimensioning.scale_set = true;
        self.floor_plan = new_plan;
        self.dimensioning.known_params = prev_plan.params_pinned_to_outline(&self.floor_plan);
        let active = self.floor_plan.all_active_params();
        Ok((self.dimensioning.known_params.len(), active.len()))
    }

    /// Adopt `ring` (world metres, open — no closing duplicate) as this
    /// storey's FULLY-FINALISED outline: the accepted-suggestion path of the
    /// new-storey flow. Mirrors the loaded-storey pattern
    /// ([`StoreyDoc::from_storey`]): exact points via `from_clean_ring`, all
    /// active params known, metre scale set — nothing left to dimension, no
    /// carve, no collinearity questions. Replaces the outline slot AND the
    /// live editing buffer (the outline becomes active), and clears the
    /// transient cursors a buffer swap invalidates (pending arc, edit
    /// cursor). Living areas / exposure are untouched (a fresh storey has
    /// none). `Err` on a degenerate ring (< 3 vertices, or one the
    /// parametric build rejects); the storey is then unchanged.
    pub fn adopt_finalised_outline(&mut self, ring: &[Point2f32]) -> Result<(), String> {
        if ring.len() < 3 {
            return Err(format!(
                "outline ring is degenerate ({} vertices)",
                ring.len()
            ));
        }
        let plan = FloorPlan::from_clean_ring(ring)
            .map_err(|e| format!("outline ring rejected: {:?}", e))?;
        let dimensioning = DimensionState {
            known_params: plan.all_active_params(),
            scale_set: true,
            ..Default::default()
        };
        self.outline = ShapeSlot {
            floor_plan: plan.clone(),
            dimensioning: dimensioning.clone(),
        };
        self.active = ActiveShape::Outline;
        self.floor_plan = plan;
        self.dimensioning = dimensioning;
        self.pending_arc = None;
        self.edit_state = EditState::default();
        Ok(())
    }

    /// Build a `ShapeSlot` for a freshly-constructed sub-area `plan`: inherit the
    /// outline's metre scale, and pre-mark as KNOWN the parameters pinned to
    /// `boundary` (walls snapped onto it) so dimensioning only asks about the
    /// walls the user actually drew. `boundary` is the storey outline for a
    /// living area, or the available region for a phase area. Also records the
    /// axes the boundary does NOT pin (`position_free_axes`) — the boundary
    /// isn't retained, so the area's positional freedom is captured here, once.
    pub fn pinned_slot_for(&self, boundary: &FloorPlan, plan: FloorPlan) -> ShapeSlot {
        ShapeSlot {
            dimensioning: DimensionState {
                scale_set: self.outline.dimensioning.scale_set,
                known_params: boundary.params_pinned_to_outline(&plan),
                position_free_axes: boundary.free_position_axes(&plan),
                ..Default::default()
            },
            floor_plan: plan,
        }
    }

    /// Append `slot` as a new living area and make it the active shape (loading it
    /// into the live buffer, dropping any in-flight arc / edit cursor).
    pub fn add_active_living_area(&mut self, slot: ShapeSlot) {
        self.living_areas.push(slot);
        self.active = ActiveShape::LivingArea(self.living_areas.len() - 1);
        self.load_active_into_buffer();
        self.pending_arc = None;
        self.edit_state = EditState::default();
    }

    /// Make `slot` the in-flight phase area and the active shape (loading it into
    /// the live buffer, dropping any in-flight arc / edit cursor).
    pub fn begin_phase_area(&mut self, slot: ShapeSlot) {
        self.phase_shape = Some(slot);
        self.active = ActiveShape::PhaseArea;
        self.load_active_into_buffer();
        self.pending_arc = None;
        self.edit_state = EditState::default();
    }

    /// Make `slot` the in-flight step area and the active shape (loading it into
    /// the live buffer, dropping any in-flight arc / edit cursor).
    pub fn begin_step_area(&mut self, slot: ShapeSlot) {
        self.step_shape = Some(slot);
        self.active = ActiveShape::StepArea;
        self.load_active_into_buffer();
        self.pending_arc = None;
        self.edit_state = EditState::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// `pick_piece_for_stroke` returns the available piece the stroke is drawn in
    /// (the one containing most of its points), not just any / the largest.
    #[test]
    fn picks_the_piece_the_stroke_is_drawn_in() {
        let left =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(4., 0.), p(4., 2.), p(0., 2.)]).unwrap();
        let right =
            FloorPlan::from_clean_ring(&[p(6., 0.), p(10., 0.), p(10., 2.), p(6., 2.)]).unwrap();
        // a stroke inside the RIGHT piece
        let stroke = vec![p(7., 0.5), p(8., 1.0), p(9., 1.5)];
        let picked =
            pick_piece_for_stroke(vec![left, right], &stroke).expect("a piece contains it");
        assert!(picked.contains_point(8.0, 1.0), "picked the right piece");
        assert!(!picked.contains_point(2.0, 1.0), "not the left piece");
    }

    /// A stroke that LOOPS AROUND an available piece (its points outside the
    /// piece, but the piece inside the loop) still picks that piece — overlap is
    /// counted both ways, so an area drawn as a loop around free space works.
    #[test]
    fn picks_the_piece_the_stroke_loops_around() {
        let inner =
            FloorPlan::from_clean_ring(&[p(4., 4.), p(6., 4.), p(6., 6.), p(4., 6.)]).unwrap();
        let elsewhere =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(1., 0.), p(1., 1.), p(0., 1.)]).unwrap();
        // a loop AROUND `inner`: its points are outside `inner`, but `inner` is
        // enclosed by it.
        let stroke = vec![p(3., 3.), p(7., 3.), p(7., 7.), p(3., 7.), p(3., 3.)];
        let picked = pick_piece_for_stroke(vec![elsewhere, inner], &stroke).expect("a piece");
        assert!(
            picked.contains_point(5.0, 5.0),
            "picked the looped-around piece"
        );
    }

    /// A stroke drawn over ALREADY-TAKEN space (no available piece contains it)
    /// still picks the largest available piece — NOT `None` — so the caller never
    /// falls back to the full outline and the new area can't overlap an existing
    /// one. `None` only when there are no pieces at all.
    #[test]
    fn picks_largest_available_when_stroke_is_over_taken_space() {
        let small =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(2., 0.), p(2., 2.), p(0., 2.)]).unwrap();
        let big = FloorPlan::from_clean_ring(&[p(10., 0.), p(20., 0.), p(20., 10.), p(10., 10.)])
            .unwrap();
        // stroke is over neither piece → picks the bigger available piece
        let stroke = vec![p(50., 50.), p(60., 60.)];
        let picked = pick_piece_for_stroke(vec![small, big], &stroke).expect("a piece");
        assert!(
            picked.contains_point(15., 5.),
            "picked the larger piece: nope"
        );
        // no pieces at all → None
        assert!(pick_piece_for_stroke(vec![], &stroke).is_none());
    }

    /// `adopt_finalised_outline` lands the ring as a FINALISED outline — the
    /// from_storey pattern: metre scale set, every param known, no prompt to
    /// come — with the live buffer mirroring the slot and the transient edit
    /// cursor cleared. The resolve projection round-trips the ring exactly.
    #[test]
    fn adopt_finalised_outline_finalises_and_round_trips() {
        let ring = [p(0., 0.), p(6., 0.), p(6., 4.), p(0., 4.)];
        let mut doc = StoreyDoc::default();
        doc.edit_state.dragging = true; // a stale cursor the adopt must drop
        doc.adopt_finalised_outline(&ring)
            .expect("clean ring adopts");

        assert!(doc.dimensioning.scale_set, "metre scale inherited");
        assert!(doc.outline.finalised(), "nothing left to dimension");
        assert_eq!(
            doc.dimensioning.known_params,
            doc.floor_plan.all_active_params()
        );
        assert!(doc.dimensioning.current_prompt.is_none());
        assert_eq!(doc.active, ActiveShape::Outline);
        assert_eq!(
            doc.outline.floor_plan.get_points(),
            doc.floor_plan.get_points(),
            "live buffer mirrors the outline slot"
        );
        assert!(!doc.edit_state.dragging, "edit cursor cleared");

        // The resolve projection round-trips the adopted ring EXACTLY (the
        // empty-step verbatim branch — no geo ops).
        let mut ber = datatypes::ber::ber_struct::Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let lists = storey.floors.to_vertex_lists();
        assert_eq!(lists.len(), 1);
        let expected: Vec<domain::points::Point2f64> =
            ring.iter().copied().map(domain::points::to_f64).collect();
        assert_eq!(lists[0], expected, "resolved floor is the ring verbatim");
    }

    /// A degenerate ring is rejected with `Err` and the storey is unchanged.
    #[test]
    fn adopt_finalised_outline_rejects_degenerate_rings() {
        let mut doc = StoreyDoc::default();
        doc.adopt_finalised_outline(&[p(0., 0.), p(6., 0.), p(6., 4.), p(0., 4.)])
            .unwrap();
        let before = doc.floor_plan.get_points();
        assert!(
            doc.adopt_finalised_outline(&[p(0., 0.), p(1., 0.)])
                .is_err(),
            "< 3 vertices is degenerate"
        );
        assert_eq!(doc.floor_plan.get_points(), before, "storey unchanged");
        assert!(doc.dimensioning.scale_set, "previous adoption intact");
    }

    /// An active `StepArea` round-trips through `step_shape`: `begin_step_area`
    /// checks the slot out into the live buffer, `sync_active` writes buffer
    /// changes back to the slot, and `load_active_into_buffer` restores them.
    #[test]
    fn step_area_round_trips_sync_and_load_through_step_shape() {
        let plan =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(2., 0.), p(2., 2.), p(0., 2.)]).unwrap();
        let mut doc = StoreyDoc::default();
        doc.begin_step_area(ShapeSlot {
            floor_plan: plan,
            dimensioning: DimensionState::default(),
        });
        assert_eq!(doc.active, ActiveShape::StepArea);
        assert_eq!(doc.floor_plan.get_points().len(), 4, "slot checked out");

        // Mutate the live buffer, sync back → the change lands in step_shape.
        doc.dimensioning.scale_set = true;
        doc.sync_active();
        assert!(doc.step_shape.as_ref().unwrap().dimensioning.scale_set);

        // Clobber the buffer, reload → the slot's state comes back.
        doc.dimensioning = DimensionState::default();
        doc.floor_plan = FloorPlan::default();
        doc.load_active_into_buffer();
        assert!(doc.dimensioning.scale_set);
        assert_eq!(doc.floor_plan.get_points().len(), 4);
    }
}
