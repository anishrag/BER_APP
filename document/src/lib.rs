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

mod dimensioning;
mod edit;
mod exposure;

/// Derive full per-wall exposure coverage — the shell's `commit_to_ber` uses it.
pub use exposure::wall_coverage;

use datatypes::ber::structural::WallExposure;
use domain::points::Point2f32;
use manip_2d_plan::{Error, FloorPlan, ParameterId, SubareaOutcome, SubareaTuning, VertexId};
use std::collections::{HashMap, HashSet};

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
#[derive(Debug, Default, Clone)]
pub struct DimensionState {
    /// Parameter IDs the user has explicitly entered a length for.
    pub known_params: HashSet<ParameterId>,
    /// Current "which wall do I ask about next?" prompt, or `None` when all
    /// dimensions have been supplied (or there's nothing to dimension yet).
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
}

/// In-memory state for one editable shape (the storey outline or a living
/// area): its parametric plan plus the dimensioning progress against it. The
/// active shape's slot is mirrored live in the controller's editing buffer;
/// the inactive shapes rest in their slots until activated.
#[derive(Debug, Clone, Default)]
pub struct ShapeSlot {
    pub floor_plan: FloorPlan,
    pub dimensioning: DimensionState,
}

/// Which shape the editor is currently acting on. The controller's live buffer
/// mirrors this one; switching saves the live state back to its slot and loads
/// the target's.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum ActiveShape {
    /// The storey outline (the primary footprint).
    #[default]
    Outline,
    /// The living area at this index in `living_areas`.
    LivingArea(usize),
    /// The in-flight construction-phase area being dimensioned (held in
    /// `phase_shape`). There's only ever one at a time, so no index.
    PhaseArea,
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

/// One storey's editable shapes and which is active.
#[derive(Debug, Clone, Default)]
pub struct Document {
    /// The storey outline (primary footprint).
    pub outline: ShapeSlot,
    /// Potential living-area sub-shapes, drawn inside the storey the same way as
    /// the outline. The most-recent is the one the tools edit; the largest is
    /// chosen as THE living area later, but all are kept.
    pub living_areas: Vec<ShapeSlot>,
    /// The in-flight construction-phase area being dimensioned (a sub-shape, like
    /// a living area, but tagged with a phase at the end rather than kept).
    pub phase_shape: Option<ShapeSlot>,
    /// Which shape the live editing buffer below currently mirrors.
    pub active: ActiveShape,

    // ── Live editing buffer ── the active shape, checked out for editing.
    // `sync_active`/`load_active_into_buffer` move state between this and the
    // active slot above.
    /// The active shape's parametric plan, live.
    pub floor_plan: FloorPlan,
    /// Dimensioning progress against `floor_plan`.
    pub dimensioning: DimensionState,
    /// Edit-mode click/drag cursor into `floor_plan`.
    pub edit_state: EditState,
    /// A provisional curved wall mid-flow against `floor_plan`, if any.
    pub pending_arc: Option<PendingArc>,

    /// Exposure SECTIONS of each OUTLINE perimeter wall, keyed by the wall's
    /// stable start `VertexId` (survives edits). Un-marked length defaults to
    /// `Exposed` when derived into BER `WallRegions`. Part of the document
    /// (editable, undoable) but keyed to the outline, not the active shape.
    pub wall_sections: HashMap<VertexId, Vec<ExposureSection>>,
}

impl Document {
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
        }
    }

    /// Load the active shape's slot into the live editing buffer.
    pub fn load_active_into_buffer(&mut self) {
        let slot = match self.active {
            ActiveShape::Outline => self.outline.clone(),
            ActiveShape::LivingArea(i) => self.living_areas.get(i).cloned().unwrap_or_default(),
            ActiveShape::PhaseArea => self.phase_shape.clone().unwrap_or_default(),
        };
        self.floor_plan = slot.floor_plan;
        self.dimensioning = slot.dimensioning;
    }

    /// Resolve a freehand sub-area stroke against the storey OUTLINE — clip,
    /// then cut or carve. The relationship "sub-areas are carved against the
    /// outline" lives here, not in the caller.
    pub fn carve_or_cut_against_outline(
        &self,
        stroke: &[Point2f32],
        tuning: &SubareaTuning,
    ) -> Result<SubareaOutcome, Error> {
        self.outline.floor_plan.carve_or_cut(stroke, tuning)
    }

    /// The outline MINUS `holes` (already-placed regions), as the area still
    /// available to carve into — so a new phase tiles against its neighbours
    /// without overlap. Falls back to the whole outline when nothing is removed.
    pub fn available_region(&self, holes: &[Vec<Point2f32>]) -> FloorPlan {
        self.outline
            .floor_plan
            .subtract(holes)
            .unwrap_or_else(|| self.outline.floor_plan.clone())
    }

    /// Build a `ShapeSlot` for a freshly-constructed sub-area `plan`: inherit the
    /// outline's metre scale, and pre-mark as KNOWN the parameters pinned to
    /// `boundary` (walls snapped onto it) so dimensioning only asks about the
    /// walls the user actually drew. `boundary` is the storey outline for a
    /// living area, or the available region for a phase area.
    pub fn pinned_slot_for(&self, boundary: &FloorPlan, plan: FloorPlan) -> ShapeSlot {
        ShapeSlot {
            dimensioning: DimensionState {
                scale_set: self.outline.dimensioning.scale_set,
                known_params: boundary.params_pinned_to_outline(&plan),
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
}
