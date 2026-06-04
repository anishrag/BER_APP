// `AppController`'s behaviour is split across this module and two submodules
// (children, so they can use the private fields/methods declared here):
//   - `dimensioning` — the dimension-prompt workflow.
//   - `editing` — Edit-mode interaction + measurement prompts.
// This file holds the struct + state types and the rest (history, view, BER
// record/persistence, collinearity review, sketch/stroke handling).
mod dimensioning;
mod editing;
mod exposure;
mod living_areas;
mod phase_regions;

pub use exposure::{ExposureSection, ExposureSelection, SectionHandle, WallExposureView};
use living_areas::{reconstruct_living_areas, shape_finalised};
use phase_regions::{resolved_phase, PhaseRegion};

use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository};
use crate::engine::dataloader::DataLoader;
use crate::engine::Db;
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::{ConstructionPhase, ConstructionPhases, ItemId};
use datatypes::ber::structural::{FloorRegions, Storey, WallExposure, WallRegions};
use domain::points::{Point2f32, Point2f64};
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_2d_plan::{
    Axis, CollinearitySuggestion, FloorPlan, ParameterAxis, ParameterId, VertexId,
};
use sketch::model::Stroke;
use sketch::SketchEngine;
use std::collections::{HashMap, HashSet};

/// Default storey ceiling height in meters, used as a placeholder until the UI
/// collects it from the assessor.
const DEFAULT_CEILING_HEIGHT_M: f64 = 2.4;

/// Print a stroke as a `vec![p(x,y), …]` literal (matching the curve-debug
/// dumps) so a whole session can be copy-pasted into a bit-for-bit integration
/// test. Coordinates are whatever the caller passes — pixels for the initial
/// outline draw, metres for the world-coord extend/trim strokes.
fn dump_stroke(label: &str, pts: &[Point2f32]) {
    let body: String = pts
        .iter()
        .map(|p| format!("p({:.3},{:.3}),", p.x, p.y))
        .collect();
    println!("  {label} stroke ({} pts): vec![{body}]", pts.len());
}

/// In-session state for the "is this pair of walls the same line?" review.
/// Populated by `sketch_end_stroke` and consumed by `accept_current_review` /
/// `reject_current_review`.
#[derive(Debug, Default)]
pub struct CollinearityReview {
    /// All candidate pairs the detector returned for the current floor plan.
    pub suggestions: Vec<CollinearitySuggestion>,
    /// Index of the next suggestion to consider. We walk forward, skipping
    /// any pair that's become "already determined" (yes-side via the live
    /// parametric state, no-side via the `rejections` list).
    pub cursor: usize,
    /// Pairs the user explicitly said No to. Stored as full suggestions so
    /// we have the ring indices available for transitive-rejection checking.
    /// Reused for: if (A,B) was rejected and (A,C) gets approved later
    /// (C joins A's equivalence class), then asking (B,C) is redundant.
    pub rejections: Vec<CollinearitySuggestion>,
}

/// A completed stroke that's waiting for the user to confirm what to do with it.
/// Populated when `DrawPerpWalls` finishes a stroke while a floor plan already
/// exists — the controller doesn't apply the stroke until the user picks:
/// Replace / Add-new-storey / Ignore.
#[derive(Debug, Clone)]
pub struct PendingRedraw {
    pub stroke_points: Vec<Point2f32>,
    pub closed: bool,
}

/// A PROVISIONAL curved wall (`ExtendCurve` / `TrimCurve`): the freehand stroke
/// fitted to a circular arc and held un-baked. `FloorPlan::begin_arc` has
/// spliced the two endpoints onto the outline as pinned vertices, leaving a
/// straight placeholder edge (shape unchanged). The user dimensions the two
/// attach stubs through the normal prompt flow; once both are known,
/// `finish_arc` facets the arc between them and this clears. The placeholder
/// edge's start vertex is in `skipped_walls`, so only the stubs are prompted.
#[derive(Debug, Clone)]
struct PendingArc {
    /// Endpoint at the stroke's start (drawn first).
    start: VertexId,
    /// Endpoint at the stroke's end (drawn last).
    end: VertexId,
    /// Signed sagitta / chord (left of start→end). Scale-invariant: survives
    /// the pixel→metre rescale and any move of the endpoints by dimensioning.
    bulge_ratio: f32,
}

/// User-controlled view transform on the sketch canvas, on top of the
/// auto-fit. `zoom = 1.0` and zero offset = pure auto-fit. The assessor uses
/// this to zoom out (giving room to draw a disjoint section) or pan around.
#[derive(Debug, Clone, Copy)]
pub struct ViewTransform {
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

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

/// World-coord endpoints of one wall.
#[derive(Debug, Clone, Copy)]
pub struct WallPositions {
    pub wall_a: (f64, f64),
    pub wall_b: (f64, f64),
}

/// UI-friendly view of the post-drag measurement prompt: the wall the user
/// just moved + the wall they're measuring distance *from* + the current
/// distance value + which option is selected out of how many.
#[derive(Debug, Clone, Copy)]
pub struct EditDragPromptUiData {
    pub moved_wall: WallPositions,
    pub reference_wall: WallPositions,
    pub current_distance: f32,
    pub option_idx: usize,
    pub option_count: usize,
}

/// What kind of geometric constraint a vertex measurement represents.
/// Axis = linear (|V.x - N.x| or |V.y - N.y|), pins one coord at a time.
/// WallLength = euclidean |V - N|, a circle constraint that needs another
/// measurement for full pinning (circle intersection / substitution).
#[derive(Debug, Clone, Copy)]
pub enum VertexMeasurementKind {
    Axis(ParameterAxis),
    WallLength,
}

/// One candidate measurement for the just-dropped vertex.
#[derive(Debug, Clone)]
pub struct VertexMeasurement {
    pub kind: VertexMeasurementKind,
    /// Neighbour vertex this measurement is from.
    pub neighbor_vid: VertexId,
    /// Live distance: axis → `|V.coord - N.coord|`, wall → `|V - N|`.
    pub current_distance: f32,
    /// For axis measurements: sign of `(V.coord - N.coord)` at populate
    /// time. Preserved on apply so V ends up on the same side of N.
    /// Unused for wall-length measurements (left as 1.0).
    pub sign: f32,
}

/// Active post-vertex-drop prompt. After releasing a newly-inserted
/// vertex, the user is shown text boxes for each relevant neighbour
/// distance and fills them in to pin the vertex precisely.
#[derive(Debug, Clone)]
pub struct VertexDropPrompt {
    /// The newly-inserted vertex.
    pub vertex_vid: VertexId,
    /// Candidate measurements — up to 5 typically (3 H/V de-duped + 2
    /// wall-lengths to prev and next).
    pub measurements: Vec<VertexMeasurement>,
    /// Applied measurements (index into `measurements`, entered value)
    /// in chronological order. The prompt is cleared once 2 are applied
    /// — at that point the joint solve has fully pinned V's position.
    pub applied: Vec<(usize, f32)>,
}

/// UI-friendly snapshot of a vertex-drop prompt. Holds vertex/neighbour
/// world coords so the renderer can draw an arrow per active measurement
/// and the UI can label each text box.
#[derive(Debug, Clone)]
pub struct VertexDropPromptUiData {
    /// The newly-inserted vertex's world position.
    pub vertex_pos: (f64, f64),
    /// One entry per still-active measurement (axis not yet pinned).
    pub active: Vec<VertexMeasurementUiData>,
}

#[derive(Debug, Clone, Copy)]
pub struct VertexMeasurementUiData {
    /// Original index in `VertexDropPrompt.measurements` — used by Apply
    /// to know which measurement the user is responding to.
    pub idx: usize,
    /// World position of the neighbour.
    pub neighbor_pos: (f64, f64),
    /// Kind of constraint this measurement represents.
    pub kind: VertexMeasurementKind,
    /// Live distance.
    pub current_distance: f32,
}

/// UI-friendly snapshot of the current dimension prompt. `walls[0]` is the
/// wall the prompt was generated for ("primary"); subsequent entries are
/// other walls whose length expression is identical — the user can measure
/// any of them and enter the same value. World coordinates here are the same
/// coord system as the rendered BER polygon; the renderer transforms via
/// its `Transform2d`.
#[derive(Debug, Clone)]
pub struct DimensionPromptUiData {
    pub walls: Vec<WallPositions>,
    /// Current length of the primary wall in the plan's current units.
    pub current_length: f32,
}

/// Tracks the user-confirmed dimensions on the floor plan. Before any
/// dimension is entered the polygon is a pixel-scale notepad sketch; once the
/// first dimension lands, the whole plan converts to real units (meters) and
/// each subsequent dimension pins another parameter.
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

// There is NO persisted sketch snapshot. The DB stores only finalised
// (complete, metre-scale) resolved geometry — `Storey.floors` (outline),
// `Storey.living_areas`, and `Storey.walls` (exposure). On reload the editor
// rebuilds everything from those resolved points (see `load_storey_into_editor`):
// the outline's parametric model is re-derived (re-edit fidelity — shared
// params, collinearity equivalences, curve arcs — is deliberately not
// preserved), living areas are rebuilt non-editable, and exposure is recovered
// from the wall regions.

/// In-memory state for one editable shape (the storey outline or a living
/// area): its parametric plan plus the dimensioning progress against it. The
/// active shape's slot is mirrored live in `AppController::floor_plan` /
/// `dimensioning`; the inactive shapes rest in their slots until activated.
#[derive(Debug, Clone, Default)]
struct ShapeSlot {
    floor_plan: FloorPlan,
    dimensioning: DimensionState,
}

/// Which shape the editor is currently acting on. `floor_plan`/`dimensioning`
/// mirror this one; switching saves the live state back to its slot and loads
/// the target's.
#[derive(Debug, Clone, Copy, PartialEq)]
enum ActiveShape {
    /// The storey outline (the primary footprint).
    Outline,
    /// The living area at this index in `living_areas`.
    LivingArea(usize),
}

pub struct AppController {
    conn: Db,
    sketch_engine: SketchEngine,
    floor_plan: FloorPlan,
    loader_engine: DataLoader,
    /// The BER record being edited. `commit_to_ber` writes through this to SQLite.
    /// `None` means no BER is loaded yet — sketches stay in-memory until a BER exists.
    current_record: Option<BerRecord>,
    /// Which storey of `current_record` the editor is acting on. `Some(i)` =
    /// editing storey `i`; `None` = the "All storeys" view (read-only overview,
    /// no drawing/editing). Default `Some(0)` on create/open. Editor state
    /// (outline / living_areas / wall_sections / dimensioning) belongs to this
    /// storey; switching storey commits the live state into `current_record`
    /// and rebuilds the editor from the target storey via `load_storey_into_editor`.
    current_storey_idx: Option<usize>,
    /// Pending "is this collinear?" questions for the current floor plan.
    /// `None` when there's nothing to ask.
    review: Option<CollinearityReview>,
    /// A drawn-but-not-applied stroke awaiting the user's redraw decision.
    /// Only set when DrawPerpWalls finishes a stroke and there's already a plan.
    pending_redraw: Option<PendingRedraw>,
    /// A provisional curved wall awaiting its two attach dimensions before being
    /// faceted into the model. `None` when no curve is mid-flow.
    pending_arc: Option<PendingArc>,
    /// Tracks which parameters the user has dimensioned + the current prompt.
    /// Mirrors the ACTIVE shape (see `active`); switching shapes swaps this and
    /// `floor_plan` in/out of the shape slots.
    dimensioning: DimensionState,
    /// Which shape `floor_plan`/`dimensioning` currently mirror.
    active: ActiveShape,
    /// Saved state of the storey outline (stale while a living area is active;
    /// `sync_active` refreshes it from the live buffer before it's read).
    outline: ShapeSlot,
    /// Potential living-area sub-shapes, drawn inside the storey the same way as
    /// the outline. The most-recent is the one the tools edit; the largest is
    /// chosen as THE living area later, but all are kept.
    living_areas: Vec<ShapeSlot>,
    /// True when the editor is in living-area mode: `DrawPerpWalls` starts a new
    /// living area (rather than replacing the outline), and the other tools act
    /// on the most-recent living area. Toggled by the UI.
    living_area_mode: bool,
    /// True when the editor is in wall-exposure mode: tapping an outline wall
    /// selects it, and the type buttons mark it exposed/semi/non-heat-loss.
    /// Toggled by the UI.
    exposure_mode: bool,
    /// In exposure mode, the outline wall (by stable start `VertexId`) the user
    /// last tapped — the target of the next type assignment. `None` when none
    /// is selected.
    selected_exposure_wall: Option<VertexId>,
    /// Current section selection `(start_m, end_m)` on the selected wall (metres
    /// from its start vertex), set by tapping a wall (defaults to the whole
    /// wall), dragging a handle, or typing. Transient — not persisted/undone.
    exposure_sel: Option<(f32, f32)>,
    /// Which selection handle a canvas drag is currently moving. Transient.
    exposure_drag: Option<SectionHandle>,
    /// Exposure SECTIONS of each OUTLINE perimeter wall, keyed by the wall's
    /// stable start `VertexId` (survives edits). Un-marked length defaults to
    /// `Exposed` when derived into BER `WallRegions`.
    wall_sections: HashMap<VertexId, Vec<ExposureSection>>,
    /// A straight CUT line has split the storey into two candidate regions; the
    /// user picks which becomes the living area by tapping inside it. Transient
    /// (not persisted / not in undo history); `None` when nothing is awaiting a
    /// choice.
    pending_cut: Option<[ShapeSlot; 2]>,
    /// True when the editor is tagging construction-phase areas (year of
    /// construction). Routes `DrawPerpWalls` to phase carving; mutually
    /// exclusive with living-area / exposure modes. Toggled by the UI.
    phase_mode: bool,
    /// Session-only construction-phase areas, keyed by storey index. NOT
    /// persisted in the BER — they stamp surfaces' phase at commit; the durable
    /// record is each surface's `construction_id`. See `phase_regions.rs`.
    phase_regions: HashMap<usize, Vec<PhaseRegion>>,
    /// A carved phase area awaiting its phase pick (world-coord ring). The UI
    /// shows the phase picker while this is `Some`. Transient.
    pending_phase_polygon: Option<Vec<Point2f32>>,
    /// A phase cut split the storey into two candidate regions; the user taps
    /// one, which then becomes `pending_phase_polygon`. Transient.
    pending_phase_cut: Option<[Vec<Point2f32>; 2]>,
    /// Edit-mode interaction state: which wall the user has selected via
    /// click + whether a drag is currently in progress. Always reset when
    /// `edit_tool` switches away from `Edit`.
    edit_state: EditState,
    /// Active post-drag measurement prompt. Set on mouse-up after a drag if
    /// the plan is already dimensioned; cleared when the user applies or
    /// cancels. The user types a distance + cycles through candidate
    /// reference walls until they find one they can actually measure.
    edit_drag_prompt: Option<EditDragPrompt>,
    /// Active post-vertex-drop prompt. Set on mouse-up after a vertex
    /// insert+drag when the plan is dimensioned; up to 3 distances to
    /// neighbours that the user fills in to pin the vertex.
    vertex_drop_prompt: Option<VertexDropPrompt>,
    /// Undo history. Each entry is a snapshot of the data state taken just
    /// before a user-visible mutation. Undo restores the most recent;
    /// cleared on new mutations.
    history: Vec<HistorySnapshot>,
    /// Redo stack. Populated by undo; cleared by any new mutation.
    redo: Vec<HistorySnapshot>,
    /// View transform layered on top of the canvas auto-fit (pan + zoom).
    view: ViewTransform,
    /// Runtime thresholds loaded from `tuning.yaml` (new-storey + collinearity).
    tuning: crate::config::Tuning,
}

/// Snapshot of the data state for undo/redo. Includes the floor plan +
/// dimensioning + edit-state — view (zoom/pan), prompts, and in-flight
/// drag state are deliberately excluded so undo doesn't fight the user's
/// current view or interrupt an in-progress prompt mid-flow.
#[derive(Debug, Clone)]
pub struct HistorySnapshot {
    /// Live buffer of the active shape.
    floor_plan: FloorPlan,
    dimensioning: DimensionState,
    edit_state: EditState,
    /// A mid-flow provisional arc is part of the undoable state: undoing the
    /// stroke that began it must clear it; undoing a dimension that finished it
    /// must restore it.
    pending_arc: Option<PendingArc>,
    /// All shapes + which is active, so undo restores the right one (drawing or
    /// dimensioning a living area, switching shapes, etc. are all undoable).
    outline: ShapeSlot,
    living_areas: Vec<ShapeSlot>,
    active: ActiveShape,
    living_area_mode: bool,
    /// Per-wall exposure sections, so marking/clearing exposure is undoable.
    wall_sections: HashMap<VertexId, Vec<ExposureSection>>,
}

/// Edit-mode click/drag state.
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
    /// on each mouse-down. Ensures only ONE snapshot per drag (not one
    /// per mouse-move frame) so undo treats the whole drag as one action.
    pub drag_snapshot_taken: bool,
}

/// What kind of coordinate the just-moved wall is anchored to. The
/// post-drag prompt applies the user's entered distance by mutating
/// either a parameter (set_parameter) or a constant (shift_constant).
#[derive(Debug, Clone, Copy)]
pub enum MovedCoordKind {
    Parameter(ParameterId),
    /// The constant value as of mouse-down (`shift_constant` matches on
    /// this value so it knows which vertices to update).
    Constant(f32),
}

/// One candidate measurement for the post-drag prompt: "distance from this
/// other wall to the moved one." The user can cycle through these to find
/// one they can practically measure with a tape.
#[derive(Debug, Clone)]
pub struct MeasurementOption {
    /// Reference wall's start-vertex id.
    pub reference_wall_vid: VertexId,
    /// Reference's perpendicular coord (the world-coord value the moved
    /// wall is measured *from*).
    pub reference_value: f32,
    /// Sign of `(moved_value - reference_value)` at the time the prompt
    /// was constructed. Preserved when the user enters a distance so the
    /// moved wall stays on the same side of the reference as it was after
    /// the drag.
    pub sign: f32,
    /// Current `|moved_value - reference_value|` (post-drag). Shown to the
    /// user as the "current" measurement before they edit it.
    pub current_distance: f32,
}

#[derive(Debug, Clone)]
pub struct EditDragPrompt {
    /// VertexId of the moved wall.
    pub moved_wall_vid: VertexId,
    /// Perpendicular axis the wall moves along.
    pub axis: ParameterAxis,
    /// Whether the moved coord is a Parameter or a Constant.
    pub kind: MovedCoordKind,
    /// Candidate measurements; the user cycles through them.
    pub options: Vec<MeasurementOption>,
    /// Index of the option currently being asked about.
    pub current_idx: usize,
}

impl AppController {
    pub fn new(conn: Db) -> Self {
        Self {
            conn,
            loader_engine: DataLoader::new(),
            floor_plan: FloorPlan::default(),
            sketch_engine: SketchEngine::new(),
            current_record: None,
            current_storey_idx: None,
            review: None,
            pending_redraw: None,
            pending_arc: None,
            dimensioning: DimensionState::default(),
            active: ActiveShape::Outline,
            outline: ShapeSlot::default(),
            living_areas: Vec::new(),
            living_area_mode: false,
            exposure_mode: false,
            selected_exposure_wall: None,
            exposure_sel: None,
            exposure_drag: None,
            wall_sections: HashMap::new(),
            pending_cut: None,
            phase_mode: false,
            phase_regions: HashMap::new(),
            pending_phase_polygon: None,
            pending_phase_cut: None,
            edit_state: EditState::default(),
            edit_drag_prompt: None,
            vertex_drop_prompt: None,
            history: Vec::new(),
            redo: Vec::new(),
            view: ViewTransform::default(),
            tuning: crate::config::Tuning::load(),
        }
    }

    /// Snapshot the current data state just before a mutation. Push to
    /// the history stack so undo can return here. Clears the redo stack
    /// because any new mutation invalidates redo-able future states.
    fn record_history(&mut self) {
        let snap = self.current_history_snapshot();
        self.push_history_snapshot(snap);
    }

    fn current_history_snapshot(&self) -> HistorySnapshot {
        HistorySnapshot {
            floor_plan: self.floor_plan.clone(),
            dimensioning: self.dimensioning.clone(),
            edit_state: self.edit_state.clone(),
            pending_arc: self.pending_arc.clone(),
            outline: self.outline.clone(),
            living_areas: self.living_areas.clone(),
            active: self.active,
            living_area_mode: self.living_area_mode,
            wall_sections: self.wall_sections.clone(),
        }
    }

    fn restore_history_snapshot(&mut self, snap: HistorySnapshot) {
        self.floor_plan = snap.floor_plan;
        self.dimensioning = snap.dimensioning;
        self.edit_state = snap.edit_state;
        self.pending_arc = snap.pending_arc;
        self.outline = snap.outline;
        self.living_areas = snap.living_areas;
        self.active = snap.active;
        self.living_area_mode = snap.living_area_mode;
        self.wall_sections = snap.wall_sections;
        // In-flight prompts no longer match the restored state — clear.
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
    }

    fn push_history_snapshot(&mut self, snap: HistorySnapshot) {
        self.history.push(snap);
        const MAX_HISTORY: usize = 100;
        if self.history.len() > MAX_HISTORY {
            self.history.remove(0);
        }
        self.redo.clear();
    }

    /// Restore the most recent snapshot from history. Pushes the current
    /// state to redo so it can be re-applied. No-op when history is empty.
    pub fn undo(&mut self) -> Result<(), String> {
        let Some(snap) = self.history.pop() else {
            return Ok(());
        };
        let cur = self.current_history_snapshot();
        self.redo.push(cur);
        self.restore_history_snapshot(snap);
        // Push the restored polygon to BER so the canvas reflects it.
        self.commit_to_ber()?;
        Ok(())
    }

    /// Re-apply the last undone state. No-op if redo stack is empty.
    pub fn redo(&mut self) -> Result<(), String> {
        let Some(snap) = self.redo.pop() else {
            return Ok(());
        };
        let cur = self.current_history_snapshot();
        self.history.push(cur);
        self.restore_history_snapshot(snap);
        self.commit_to_ber()?;
        Ok(())
    }

    pub fn can_undo(&self) -> bool {
        !self.history.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Is there a pending DrawPerpWalls stroke awaiting the user's redraw decision?
    pub fn has_pending_redraw(&self) -> bool {
        self.pending_redraw.is_some()
    }

    /// User said: replace the current storey with the pending stroke.
    pub fn confirm_replace_storey(&mut self) -> Result<(), String> {
        let Some(pending) = self.pending_redraw.take() else {
            return Ok(());
        };
        self.apply_drawperp_stroke(&pending.stroke_points, pending.closed)
    }

    /// User said: add a new storey using the pending stroke as its outline.
    /// Stub — implementation deferred to the multi-storey feature.
    pub fn confirm_add_storey_from_current(&mut self) {
        if let Some(_pending) = self.pending_redraw.take() {
            println!("Add new storey from current — TODO (stub for upcoming feature)");
        }
    }

    /// User said: discard the pending stroke without doing anything.
    pub fn discard_pending_redraw(&mut self) {
        self.pending_redraw = None;
    }

    /// Current view transform (auto-fit + user pan/zoom).
    pub fn view(&self) -> ViewTransform {
        self.view
    }

    /// Multiply current zoom by `factor`, keeping `anchor` (canvas pixel pos)
    /// stationary. Clamped to a reasonable range.
    pub fn zoom_view(&mut self, factor: f32, anchor: (f32, f32), canvas_center: (f32, f32)) {
        let old = self.view.zoom;
        let new = (old * factor).clamp(0.1, 10.0);
        let actual_factor = new / old;
        // Anchor's pre-zoom position relative to center, scaled by the actual factor
        // we applied, then re-anchored. Offset shifts so the anchor stays put.
        let rel_x = anchor.0 - canvas_center.0 - self.view.offset_x;
        let rel_y = anchor.1 - canvas_center.1 - self.view.offset_y;
        self.view.offset_x += rel_x * (1.0 - actual_factor);
        self.view.offset_y += rel_y * (1.0 - actual_factor);
        self.view.zoom = new;
    }

    /// Translate the view by a canvas-pixel delta (positive = pan right/down).
    pub fn pan_view(&mut self, delta_x: f32, delta_y: f32) {
        self.view.offset_x += delta_x;
        self.view.offset_y += delta_y;
    }

    /// Reset the view to pure auto-fit.
    pub fn reset_view(&mut self) {
        self.view = ViewTransform::default();
    }

    pub fn load_data_async(&self, tx: Sender<(String, f32)>) {
        let loader = self.loader_engine.clone();

        std::thread::spawn(move || {
            let _ = loader.load_general_data(tx);
        });
    }

    pub fn create_ber(&mut self, name: String, date_assessed: NaiveDate) -> Ber {
        let ber = Ber::new();
        let record = BerRecord {
            id: ber.id,
            name,
            date_assessed,
            data: ber.clone(),
        };

        BerRepository::new(&self.conn)
            .upsert(&record)
            .expect("upsert failed");
        self.current_record = Some(record);
        // A freshly created BER starts on its first storey (which `commit_to_ber`
        // lazily appends on the first stroke).
        self.current_storey_idx = Some(0);
        ber
    }

    /// Set the BER's construction phases (house + extension years). These are the
    /// only phases the assessor can later assign to an area; captured up front on
    /// the New-BER form. No-op when no BER is loaded. Persists.
    pub fn set_construction_phases(&mut self, phases: ConstructionPhases) {
        if let Some(record) = self.current_record.as_mut() {
            record.data.construction_phases = phases;
        }
        let _ = self.persist();
    }

    pub fn get_ber(&self, id: &ItemId) -> Option<Ber> {
        let repo = BerRepository::new(&self.conn);
        repo.load(id).ok().map(|record| record.data)
    }

    /// Load an existing BER into the editor: sets it as the current record and
    /// reconstructs the parametric sketch + dimensioning state from the stored
    /// snapshot, so editing resumes with every collinearity equivalence and
    /// entered dimension intact. If the snapshot is missing or from an older,
    /// incompatible version, falls back to rebuilding a fresh parametric model
    /// from the resolved `floors` geometry (equivalences/dimensions are then
    /// lost — unavoidable for pre-snapshot records).
    pub fn open_ber(&mut self, id: &ItemId) -> Result<(), String> {
        let record = BerRepository::new(&self.conn)
            .load(id)
            .map_err(|e| format!("failed to load BER: {}", e))?;
        // A reopened BER always lands on storey 0 (multi-storey navigation
        // doesn't persist the per-session cursor — by design, see plan).
        self.current_storey_idx = Some(0);
        self.load_storey_into_editor(record.data.storeys.first());
        self.current_record = Some(record);
        // Phase areas are session-only — rebuild them from the loaded storey's
        // tagged floor regions so the partition stays reproducible.
        self.phase_regions.clear();
        self.reconstruct_phase_regions_for(0);
        self.view = ViewTransform::default();
        self.edit_state = EditState::default();
        self.review = None;
        self.compute_next_dimension_prompt();
        self.history.clear();
        self.redo.clear();
        Ok(())
    }

    /// Which storey the editor is currently acting on. `Some(i)` = storey `i`
    /// is being edited; `None` = the "All storeys" overview (read-only, no
    /// drawing). Default `Some(0)` on create/open.
    pub fn current_storey_idx(&self) -> Option<usize> {
        self.current_storey_idx
    }

    /// Switch which storey the editor acts on. Commits the live editor state
    /// to its current storey, then loads the target (`None` for the read-only
    /// All-storeys overview). No-op when already on `target`.
    pub fn set_current_storey(&mut self, target: Option<usize>) -> Result<(), String> {
        if self.current_storey_idx == target {
            return Ok(());
        }
        // Save the current storey first so its in-memory state is current.
        self.commit_to_ber()?;
        self.current_storey_idx = target;
        // Clear transient per-storey state — none of it carries across a switch.
        self.view = ViewTransform::default();
        self.edit_state = EditState::default();
        self.review = None;
        self.pending_arc = None;
        self.pending_cut = None;
        self.pending_redraw = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        let storey = target.and_then(|i| {
            self.current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(i).cloned())
        });
        self.load_storey_into_editor(storey.as_ref());
        // Rebuild the target storey's phase areas from its tagged floor regions
        // (unless this session already has them) so its partition is reproducible.
        if let Some(i) = target {
            if !self.phase_regions.contains_key(&i) {
                self.reconstruct_phase_regions_for(i);
            }
        }
        self.compute_next_dimension_prompt();
        // Undo/redo is per-editing-session of one storey — don't carry across.
        self.history.clear();
        self.redo.clear();
        Ok(())
    }

    /// Append a fresh empty `Storey` to the current BER and switch the editor
    /// onto it. The previous storey is committed first; the new one starts
    /// with an empty outline, no living areas, no wall sections, and a fresh
    /// dimensioning state (scale calibrates again on its first dimension).
    /// Errors if no BER is loaded.
    pub fn add_storey(&mut self) -> Result<(), String> {
        if self.current_record.is_none() {
            return Err("no BER loaded".into());
        }
        // Save whatever storey is live before leaving it.
        self.commit_to_ber()?;
        let new_idx = {
            let record = self.current_record.as_mut().unwrap();
            let n = record.data.storeys.len();
            record.data.storeys.push(Storey {
                storey_number: n as i32,
                name: format!("Storey {}", n + 1),
                ..Default::default()
            });
            n
        };
        println!(
            "[new-storey] add_storey: appended storey {} (now editing it, zoomed out for drawing room)",
            new_idx
        );
        self.current_storey_idx = Some(new_idx);
        // Start zoomed out so the previous-storey watermark doesn't fill the
        // canvas — the assessor needs room to draw a footprint that extends
        // BEYOND it (e.g. over a ground-floor garage). `Fit` resets to full-fit.
        self.view = ViewTransform {
            zoom: self.tuning.new_storey.add_storey_zoom,
            ..ViewTransform::default()
        };
        self.edit_state = EditState::default();
        self.review = None;
        self.pending_arc = None;
        self.pending_cut = None;
        self.pending_redraw = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        // Fresh, empty editor — scale_set false so the first wall dimensioned on
        // the new storey calibrates its scale (independent of any prior storey).
        self.load_storey_into_editor(None);
        self.history.clear();
        self.redo.clear();
        Ok(())
    }

    /// Rebuild the editor state from a storey's RESOLVED geometry. There is no
    /// parametric snapshot — the DB holds only finalised, metre-scale geometry,
    /// so this is unambiguous. The outline is rebuilt from its floor polygon
    /// (exact points, marked complete + metric so it isn't re-prompted); living
    /// areas are rebuilt non-editable; wall exposure is recovered from the wall
    /// regions. Re-edit fidelity (shared params, collinearity equivalences,
    /// curve arcs) is intentionally not preserved — drawing tools work from the
    /// loaded points.
    fn load_storey_into_editor(&mut self, storey: Option<&Storey>) {
        self.dimensioning = DimensionState::default();
        // A provisional arc is transient and tied to the OUTGOING plan's
        // vertices; loading a record must not carry a stale one over.
        self.pending_arc = None;
        self.living_areas = Vec::new();
        self.active = ActiveShape::Outline;
        self.living_area_mode = false;
        self.exposure_mode = false;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        self.wall_sections = HashMap::new();
        self.outline = ShapeSlot::default();
        let Some(storey) = storey else {
            self.floor_plan = FloorPlan::default();
            return;
        };

        // Outline: rebuild from the resolved floor footprint. A SINGLE region is
        // the whole footprint — use its exact stored points (no geo, so it
        // round-trips). Only when the floor is split into several phase zones do
        // we MERGE them back into one outline ring (the first region would be
        // just one zone).
        let primary: Vec<Point2f32> = if storey.floors.regions.len() > 1 {
            storey
                .floors
                .merged_outline()
                .into_iter()
                .map(|p| Point2f32::new(p.x as f32, p.y as f32))
                .collect()
        } else {
            storey
                .floors
                .to_vertex_lists()
                .into_iter()
                .find(|r| r.len() >= 3)
                .map(|r| {
                    r.iter()
                        .map(|p| Point2f32::new(p.x as f32, p.y as f32))
                        .collect()
                })
                .unwrap_or_default()
        };
        if primary.len() >= 3 {
            self.floor_plan = FloorPlan::from_clean_ring(&primary).unwrap_or_default();
            // Loaded geometry is finalised: metre scale set, nothing to dimension.
            self.dimensioning = DimensionState {
                known_params: self.floor_plan.all_active_params(),
                scale_set: true,
                dimensioned_walls: Vec::new(),
                skipped_walls: HashSet::new(),
                current_prompt: None,
            };
        } else {
            self.floor_plan = FloorPlan::default();
            self.dimensioning = DimensionState::default();
        }
        self.outline = ShapeSlot {
            floor_plan: self.floor_plan.clone(),
            dimensioning: self.dimensioning.clone(),
        };
        self.living_areas = reconstruct_living_areas(&storey.living_areas);
        self.wall_sections = self.sections_from_wall_regions(&storey.walls);
    }

    /// Snapshot of the BER currently being edited, for the UI to render.
    /// Returns None if no BER has been created/loaded yet.
    pub fn current_ber_clone(&self) -> Option<Ber> {
        self.current_record.as_ref().map(|r| r.data.clone())
    }

    /// True when there's a completed orthogonal shape to act on. The UI uses
    /// this to gate downstream edit tools (Select, Move, Extend, Trim, …)
    /// behind the prerequisite "draw the orthogonal shape first."
    pub fn has_floor_plan(&self) -> bool {
        self.floor_plan.get_points().len() >= 3
    }

    /// Project current in-memory state into `self.current_record` and persist.
    /// No-op when no BER is loaded. Called by handler methods that complete a
    /// logical user action (stroke end, wall drag end, etc.) — not from
    /// incremental handlers like `sketch_add_point`.
    fn commit_to_ber(&mut self) -> Result<(), String> {
        if self.current_record.is_none() {
            return Ok(());
        }
        // Flush the live buffer into the active shape's slot, so `outline` and
        // `living_areas` are current regardless of which shape is being edited.
        self.sync_active();

        // The storey footprint is always the OUTLINE (never a living area, even
        // when one is active). One ring → one FloorRegion.
        let polygons_f64: Vec<Vec<Point2f64>> = self
            .outline
            .floor_plan
            .polygons()
            .into_iter()
            .map(|ring| {
                ring.into_iter()
                    .map(|p| Point2f64::new(p.x as f64, p.y as f64))
                    .collect()
            })
            .collect();

        // Derive the heat-loss walls from the outline perimeter. ONLY walls the
        // user has marked become regions — an un-marked wall produces NO region
        // (it stays "unset", and `WallRegions` is the single source of truth, so
        // that distinction survives reload). A marked wall is split into one
        // vertical `WallRegion` per section (sub-segment endpoints interpolated
        // by distance) extruded to `DEFAULT_CEILING_HEIGHT_M` — the same
        // provisional height floors use — until real heights are entered.
        // Computed before the mutable `record` borrow below.
        let wall_segments: Vec<(Point2f64, Point2f64, WallExposure)> = self
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .flat_map(|(vid, a, b)| {
                let sections = match self.wall_sections.get(&vid) {
                    Some(s) if !s.is_empty() => s,
                    _ => return Vec::new(), // un-marked wall → no region
                };
                let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                let lerp = move |d: f32| {
                    let t = if len > 1e-6 { d / len } else { 0.0 };
                    Point2f64::new(
                        (a.x + t * (b.x - a.x)) as f64,
                        (a.y + t * (b.y - a.y)) as f64,
                    )
                };
                exposure::wall_coverage(len, sections)
                    .into_iter()
                    .map(move |(s, e, exposure)| (lerp(s), lerp(e), exposure))
                    .collect::<Vec<_>>()
            })
            .collect();

        // Resolved living-area polygons (each slot is current after sync_active).
        // Stored in `Storey.living_areas`, NOT the snapshot — they're
        // non-editable on reload, so their resolved geometry suffices.
        let living_area_lists: Vec<Vec<Point2f64>> = self
            .living_areas
            .iter()
            .map(|slot| {
                slot.floor_plan
                    .get_points()
                    .into_iter()
                    .map(|p| Point2f64::new(p.x as f64, p.y as f64))
                    .collect()
            })
            .collect();

        // In-memory: write the FULL resolved geometry (dimensioned or not) into
        // `current_record` so the canvas — which renders `current_ber_clone` —
        // stays live. The DB write (`persist`) then filters this down to the
        // FINALISED parts only, so the database never holds pixel/incomplete
        // geometry. There's no parametric snapshot: the outline is the only
        // thing that needs lossless re-edit, and we've traded that away (reload
        // rebuilds everything from these resolved points).
        let idx = self.current_storey_idx.unwrap_or(0);
        // Phase areas for this storey (world/metre rings + phase), cloned before
        // the mutable record borrow — they drive the floor PARTITION below.
        let phase_area_rings: Vec<(Vec<Point2f64>, ConstructionPhase)> = self
            .phase_regions
            .get(&idx)
            .map(|rs| {
                rs.iter()
                    .map(|r| {
                        let ring = r
                            .polygon
                            .iter()
                            .map(|p| Point2f64::new(p.x as f64, p.y as f64))
                            .collect();
                        (ring, r.phase.clone())
                    })
                    .collect()
            })
            .unwrap_or_default();
        let record = self.current_record.as_mut().unwrap();
        // Ensure the target storey exists. The single-storey case (idx 0,
        // empty vec) lazy-creates here; multi-storey flows always push a
        // storey via `add_storey` before pointing `current_storey_idx` at it,
        // so this loop only ever runs once in practice.
        while record.data.storeys.len() <= idx {
            record.data.storeys.push(Storey::default());
        }
        // `floor_cid` is the default (unset, `Original`) floor construction — the
        // partition's REMAINDER (untagged) and living areas use it.
        let floor_cid = record.data.ensure_floor_construction(
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let wall_cid = record.data.ensure_wall_construction(
            Default::default(),
            Default::default(),
            Default::default(),
        );
        // Resolve each phase area's floor construction (default type/make-up,
        // that area's phase), then PARTITION each outline ring by those areas so
        // the storey floor splits into one region per phase zone + the remainder.
        let areas_with_cids: Vec<(Vec<Point2f64>, ItemId)> = phase_area_rings
            .iter()
            .map(|(ring, phase)| {
                let cid = record.data.ensure_floor_construction(
                    Default::default(),
                    Default::default(),
                    phase.clone(),
                );
                (ring.clone(), cid)
            })
            .collect();
        if polygons_f64.iter().any(|p| p.len() >= 3) {
            record.data.storeys[idx].floors = if areas_with_cids.is_empty() {
                // No phase zones → keep the whole-outline floor verbatim (NO geo
                // boolean ops, so the stored points round-trip exactly and the
                // rebuilt outline isn't re-normalised — which broke collinearity).
                FloorRegions::from_vertex_lists(polygons_f64, DEFAULT_CEILING_HEIGHT_M, floor_cid)
            } else {
                // Tagged → partition each outline ring into phase zones.
                let mut regions = Vec::new();
                for ring in &polygons_f64 {
                    if ring.len() < 3 {
                        continue;
                    }
                    let part = FloorRegions::partition(
                        ring,
                        &areas_with_cids,
                        floor_cid,
                        DEFAULT_CEILING_HEIGHT_M,
                    );
                    regions.extend(part.regions);
                }
                FloorRegions { regions }
            };
        }
        record.data.storeys[idx].living_areas =
            FloorRegions::from_vertex_lists(living_area_lists, DEFAULT_CEILING_HEIGHT_M, floor_cid);
        // Re-derive walls from exposure (empty when nothing is marked),
        // preserving the construction link of the region a split came from.
        record.data.storeys[idx].walls = WallRegions::rebuild_preserving(
            wall_segments,
            &record.data.storeys[idx].walls,
            0.0,
            DEFAULT_CEILING_HEIGHT_M,
            wall_cid,
        );

        // Floors are already phase-correct from the partition; this stamps the
        // phase onto the just-rebuilt WALLS from the same session areas.
        self.resolve_storey_phases(idx);

        self.persist()
    }

    /// Stamp the construction PHASE onto storey `idx`'s WALLS from the session
    /// phase areas (floors are already phase-correct from the commit-time
    /// partition). Per wall, by its base midpoint: a phase area containing it
    /// WINS (latest first); else the wall keeps its existing non-`Original` phase
    /// (rebuild inherited it); else `Original`. Type/make-up are preserved — only
    /// the phase axis changes, so the construction is re-`ensure`d with the same
    /// `(type, makeup)`.
    fn resolve_storey_phases(&mut self, idx: usize) {
        // Clone the storey's phase polygons so we don't borrow `self` while the
        // record is mutably borrowed below.
        let phase_polys: Vec<(Vec<Point2f32>, ConstructionPhase)> = self
            .phase_regions
            .get(&idx)
            .map(|rs| {
                rs.iter()
                    .map(|r| (r.polygon.clone(), r.phase.clone()))
                    .collect()
            })
            .unwrap_or_default();
        let Some(record) = self.current_record.as_mut() else {
            return;
        };
        let ber = &mut record.data;
        if ber.storeys.len() <= idx {
            return;
        }

        // WALLS: `rebuild_preserving` already carried each wall's construction
        // (and phase) across; apply the polygon override on top.
        let n_walls = ber.storeys[idx].walls.regions.len();
        for i in 0..n_walls {
            let Some(mid) = ber.storeys[idx].walls.regions[i].base_midpoint() else {
                continue;
            };
            let cur_cid = ber.storeys[idx].walls.regions[i].construction_id;
            let (ty, makeup, cur_phase) = match ber.wall_construction(cur_cid) {
                Some(c) => (
                    c.construction_type,
                    c.base.construction_makeup.clone(),
                    c.base.construction_phase.clone(),
                ),
                None => (
                    Default::default(),
                    Default::default(),
                    ConstructionPhase::Original,
                ),
            };
            let new_phase =
                resolved_phase(&phase_polys, mid.x as f32, mid.y as f32, cur_phase.clone());
            if new_phase != cur_phase {
                let cid = ber.ensure_wall_construction(ty, makeup, new_phase);
                ber.storeys[idx].walls.regions[i].construction_id = cid;
            }
        }
    }

    /// Rebuild the session phase areas for storey `idx` from its persisted floor
    /// regions (each non-`Original` floor region → a phase area). Phase areas are
    /// session-only, so on reload they'd otherwise be empty and the NEXT commit
    /// would re-partition with no areas — wiping the split. Reconstructing here
    /// keeps the partition reproducible and lets a new selection still snap to the
    /// existing zones. No-op when nothing is tagged.
    fn reconstruct_phase_regions_for(&mut self, idx: usize) {
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let Some(storey) = record.data.storeys.get(idx) else {
            return;
        };
        let mut regions: Vec<PhaseRegion> = Vec::new();
        for fr in &storey.floors.regions {
            let phase = record
                .data
                .floor_construction(fr.construction_id)
                .map(|c| c.base.construction_phase.clone())
                .unwrap_or(ConstructionPhase::Original);
            if phase == ConstructionPhase::Original {
                continue;
            }
            let mut ring: Vec<Point2f32> = fr
                .polygon
                .exterior()
                .coords()
                .map(|c| Point2f32::new(c.x as f32, c.y as f32))
                .collect();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() >= 3 {
                regions.push(PhaseRegion {
                    polygon: ring,
                    phase,
                });
            }
        }
        if regions.is_empty() {
            self.phase_regions.remove(&idx);
        } else {
            self.phase_regions.insert(idx, regions);
        }
    }

    /// Write a FINALISED copy of the current record to the DB. Components that
    /// aren't fully dimensioned — and thus may still be in pixel units — are
    /// dropped from what's SAVED (never from memory). So the database only ever
    /// holds complete, metric geometry, and reloading it "as points" is
    /// unambiguous. Triggered on every commit; the DB simply gains each part as
    /// it becomes finalised.
    fn persist(&self) -> Result<(), String> {
        let Some(record) = &self.current_record else {
            return Ok(());
        };
        let mut out = record.clone();
        let idx = self.current_storey_idx.unwrap_or(0);
        if self.outline_finalised() {
            // Outline is metric + fully dimensioned → keep its floors/walls,
            // and persist only the living areas that are themselves done.
            let lists: Vec<Vec<Point2f64>> = self
                .living_areas
                .iter()
                .filter(|s| shape_finalised(&s.dimensioning, &s.floor_plan))
                .map(|s| {
                    s.floor_plan
                        .get_points()
                        .into_iter()
                        .map(|p| Point2f64::new(p.x as f64, p.y as f64))
                        .collect()
                })
                .collect();
            // `ensure_*` borrows `out.data`, so resolve the id before the
            // storey borrow below.
            let floor_cid = out.data.ensure_floor_construction(
                Default::default(),
                Default::default(),
                Default::default(),
            );
            if let Some(storey) = out.data.storeys.get_mut(idx) {
                storey.living_areas =
                    FloorRegions::from_vertex_lists(lists, DEFAULT_CEILING_HEIGHT_M, floor_cid);
            }
        } else if let Some(storey) = out.data.storeys.get_mut(idx) {
            // Outline not finalised → persist no geometry yet (it's likely
            // still in pixels; living areas can't be metric before it is).
            storey.floors = FloorRegions::default();
            storey.walls = WallRegions::default();
            storey.living_areas = FloorRegions::default();
        }
        BerRepository::new(&self.conn)
            .upsert(&out)
            .map_err(|e| format!("upsert failed: {}", e))
    }

    /// The outline is finalised when its metre scale is set AND no wall is left
    /// to dimension — i.e. its resolved geometry is complete and in metres.
    fn outline_finalised(&self) -> bool {
        self.outline.dimensioning.scale_set
            && shape_finalised(&self.outline.dimensioning, &self.outline.floor_plan)
    }

    /// COLLINEARITY REVIEW-------------------------------------------
    /// The next collinearity suggestion the UI should ask the user about,
    /// or None if there's nothing pending. Walks forward from the cursor,
    /// skipping suggestions that have become "already determined" since the
    /// list was generated (yes-side via the live parametric state, no-side
    /// via the rejections list).
    pub fn current_review_suggestion(&self) -> Option<&CollinearitySuggestion> {
        let review = self.review.as_ref()?;
        for sug in &review.suggestions[review.cursor..] {
            if !self.review_should_skip(sug) {
                return Some(sug);
            }
        }
        None
    }

    /// Clone of the current suggestion, for cheap UI handoff without borrow gymnastics.
    pub fn current_review_suggestion_clone(&self) -> Option<CollinearitySuggestion> {
        self.current_review_suggestion().cloned()
    }

    /// Number of suggestions still to be considered (including the current).
    pub fn review_remaining(&self) -> usize {
        let Some(review) = self.review.as_ref() else {
            return 0;
        };
        review.suggestions[review.cursor..]
            .iter()
            .filter(|s| !self.review_should_skip(s))
            .count()
    }

    /// User said Yes: apply the suggestion, advance past it, commit.
    pub fn accept_current_review(&mut self) -> Result<(), String> {
        let Some(sug) = self.next_pending_suggestion() else {
            return Ok(());
        };
        self.record_history();
        println!(
            "collinearity YES: ring {}/edge {} & ring {}/edge {} along {:?} (coords {:.2}, {:.2})",
            sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, sug.axis, sug.coord_a, sug.coord_b
        );
        self.floor_plan.apply_collinear_suggestion(&sug);
        println!(
            "  → active_parameter_count = {}",
            self.floor_plan.active_parameter_count()
        );
        self.advance_review_cursor();
        let result = self.commit_to_ber();
        self.after_review_step();
        result
    }

    /// User said No: record the rejection (for transitive skipping), advance past it.
    pub fn reject_current_review(&mut self) {
        let Some(sug) = self.next_pending_suggestion() else {
            return;
        };
        self.record_history();
        println!(
            "collinearity NO:  ring {}/edge {} & ring {}/edge {} along {:?} (coords {:.2}, {:.2})",
            sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, sug.axis, sug.coord_a, sug.coord_b
        );
        if let Some(review) = self.review.as_mut() {
            review.rejections.push(sug);
        }
        self.advance_review_cursor();
        self.after_review_step();
    }

    /// User said Skip-all: clear the entire review without applying anything.
    /// Once the collinearity review is gone the controller picks up the
    /// dimensioning flow, if there's anything to ask about.
    pub fn skip_all_review(&mut self) {
        self.review = None;
        self.finish_review();
    }

    /// Internal: pop the next pending suggestion by cloning it (so callers
    /// don't have to juggle borrows against `self`).
    fn next_pending_suggestion(&self) -> Option<CollinearitySuggestion> {
        self.current_review_suggestion().cloned()
    }

    /// Internal: move the cursor past the just-handled suggestion and any
    /// that follow which are now resolved. Clears `self.review` when there's
    /// nothing left to ask.
    fn advance_review_cursor(&mut self) {
        if let Some(review) = self.review.as_mut() {
            review.cursor += 1;
        } else {
            return;
        }
        // Skip past any suggestions that are now resolved. We re-borrow each
        // iteration so `review_should_skip` can immutably borrow `self` while
        // we mutate `self.review`.
        loop {
            let next_sug = self
                .review
                .as_ref()
                .and_then(|r| r.suggestions.get(r.cursor).cloned());
            let Some(sug) = next_sug else {
                self.review = None;
                return;
            };
            if self.review_should_skip(&sug) {
                if let Some(r) = self.review.as_mut() {
                    r.cursor += 1;
                }
            } else {
                return;
            }
        }
    }

    /// Kick off dimensioning when the review naturally drains to None.
    /// Called from accept/reject paths after `advance_review_cursor`.
    fn after_review_step(&mut self) {
        if self.review.is_none() {
            self.finish_review();
        }
    }

    /// Wrap up once the collinearity review is done (drained or skipped).
    /// Accepting "same line" merges two edges onto one coordinate, which leaves
    /// the vertex between them as a redundant straight-through point — drop
    /// those spurious collinear vertices (preserving deliberate subdivisions)
    /// so the user isn't later prompted for a length that can't be set without
    /// changing the shape. Then commit the cleaned outline and start dimensioning.
    fn finish_review(&mut self) {
        self.clean_envelope_collinear();
        if let Err(e) = self.commit_to_ber() {
            eprintln!("commit_to_ber after collinearity review: {}", e);
        }
        self.maybe_start_dimensioning();
    }

    /// Check the live state to decide if a suggestion is already-resolved
    /// (no need to ask). Either the two edges are already collinear, or the
    /// pair is transitively rejected via an earlier No.
    fn review_should_skip(&self, sug: &CollinearitySuggestion) -> bool {
        if self
            .floor_plan
            .edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis)
        {
            return true;
        }
        // A prior "same line" accept can collapse one of this pair's edges to
        // zero length (e.g. the short connector of a nib whose two parallel
        // sides were just merged). Asking about a degenerate edge is meaningless
        // — skip it; the end-of-review cleanup drops the vertex entirely.
        let degenerate = |ring, edge| {
            self.floor_plan
                .wall_length(ring, edge)
                .map_or(true, |l| l < 1e-3)
        };
        if degenerate(sug.ring_a, sug.edge_a) || degenerate(sug.ring_b, sug.edge_b) {
            return true;
        }
        let Some(review) = self.review.as_ref() else {
            return false;
        };
        // Transitive rejection: if any rejected pair is "the same question"
        // as `sug` (i.e. each side matches a side of the rejection through
        // existing equivalences), skip.
        for rej in &review.rejections {
            if rej.axis != sug.axis {
                continue;
            }
            let same = |sa: (usize, usize), ra: (usize, usize)| -> bool {
                (sa.0 == ra.0 && sa.1 == ra.1)
                    || self
                        .floor_plan
                        .edges_collinear(sa.0, sa.1, ra.0, ra.1, &rej.axis)
            };
            let sug_a = (sug.ring_a, sug.edge_a);
            let sug_b = (sug.ring_b, sug.edge_b);
            let rej_a = (rej.ring_a, rej.edge_a);
            let rej_b = (rej.ring_b, rej.edge_b);
            let aa_bb = same(sug_a, rej_a) && same(sug_b, rej_b);
            let ab_ba = same(sug_a, rej_b) && same(sug_b, rej_a);
            if aa_bb || ab_ba {
                return true;
            }
        }
        false
    }

    /// SKETCHING STUFF--------------------------------------------
    pub fn sketch_stroke(&self) -> Option<Stroke> {
        self.sketch_engine.stroke.clone()
    }

    pub fn sketch_mode(&self) -> SketchMode {
        self.sketch_engine.mode
    }
    pub fn set_sketch_mode(&mut self, mode: SketchMode) {
        self.sketch_engine.set_mode(mode);
    }

    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.sketch_engine.set_display(mode);
    }
    pub fn display_mode(&self) -> DisplayMode {
        self.sketch_engine.display
    }

    pub fn set_edit_mode(&mut self, mode: EditTools) {
        if !matches!(mode, EditTools::Edit) {
            // Leaving Edit mode clears the selection so a stale highlight
            // doesn't carry over into the next tool.
            self.edit_state = EditState::default();
        }
        self.sketch_engine.set_edit_tools(mode);
    }
    pub fn edit_mode(&self) -> EditTools {
        self.sketch_engine.edit_tool
    }

    pub fn sketch_start_stroke(&mut self, start: Point2f32) -> Result<(), String> {
        // A new stroke invalidates any pending collinearity review — the
        // polygon is about to change.
        self.review = None;
        self.sketch_engine
            .start_stroke(start)
            .map_err(|e| format!("Failed to start stroke: {:?}", e))
    }

    pub fn sketch_add_point(&mut self, point: Point2f32) -> Result<(), String> {
        self.sketch_engine
            .add_point(point)
            .map_err(|e| format!("Failed to add point: {:?}", e))
    }

    pub fn sketch_end_stroke(&mut self, _point: Point2f32) -> Result<(), String> {
        let edit_tool = self.sketch_engine.edit_tool;
        // Extend strokes don't need to self-close (they start and end at
        // different points on the existing polygon). Take the raw stroke
        // out of the engine without running the close-check; other tools
        // still go through end_stroke which requires close.
        let stroke_opt = if matches!(
            edit_tool,
            EditTools::ExtendOrtho
                | EditTools::ExtendCurve
                | EditTools::Trim
                | EditTools::TrimOrtho
                | EditTools::TrimCurve
        ) {
            self.sketch_engine.take_stroke_raw()
        } else {
            self.sketch_engine
                .end_stroke()
                .map_err(|e| format!("Failed to end stroke: {:?}", e))?
        };
        if let Some(stroke) = stroke_opt {
            println!("Stroke completed with {} points", stroke.points.len());
            match edit_tool {
                EditTools::DrawPerpWalls => {
                    // Drawing the primary outline.
                    if self.floor_plan.ring_count() > 0 {
                        // A plan already exists — don't silently overwrite.
                        // Stash the stroke and let the UI ask the assessor
                        // what to do (Replace / Add new storey / Ignore).
                        self.pending_redraw = Some(PendingRedraw {
                            stroke_points: stroke.points,
                            closed: stroke.is_closed,
                        });
                    } else {
                        // No existing plan — apply directly.
                        self.apply_drawperp_stroke(&stroke.points, stroke.is_closed)?;
                    }
                }
                EditTools::AddDetachedSection => {
                    self.apply_addsection_stroke(&stroke.points, stroke.is_closed)?;
                }
                EditTools::ExtendOrtho | EditTools::ExtendCurve => {
                    if let Err(e) = self.apply_extend_stroke(&stroke.points) {
                        // For now just log; in future we'll surface this
                        // to the UI alongside the dimension error.
                        eprintln!("apply_extend_stroke: {}", e);
                    }
                }
                EditTools::Trim | EditTools::TrimOrtho | EditTools::TrimCurve => {
                    if let Err(e) = self.apply_trim_stroke(&stroke.points) {
                        eprintln!("apply_trim_stroke: {}", e);
                    }
                }
                _ => {
                    // Other tools don't consume the stroke at this layer yet.
                }
            }
        }
        Ok(())
    }

    /// Apply a primary-outline stroke: replace the floor plan, persist, and
    /// populate the collinearity review. Resets dimensioning state — the new
    /// plan is a fresh pixel-scale sketch that hasn't been dimensioned yet.
    /// Diagnostic: log any non-orthogonal (slanted) edge of the active shape.
    /// A no-op when fully orthogonal. Called after operations that should leave
    /// the shape rectilinear (draw / extend-ortho / carve / dimension) so a
    /// "wall isn't straight" report shows up in the log with the exact edge.
    /// (A plain `Trim` may create a deliberate diagonal cut, so it's not checked.)
    pub(crate) fn warn_if_non_orthogonal(&self, ctx: &str) {
        let bad = self.floor_plan.non_orthogonal_edges();
        if bad.is_empty() {
            return;
        }
        eprintln!("⚠ NON-ORTHOGONAL edge(s) after {ctx} ({}):", bad.len());
        for (r, e, a, b) in &bad {
            eprintln!(
                "    ring {r} edge {e}: ({:.3},{:.3})→({:.3},{:.3})  dx={:.4} dy={:.4}",
                a.x,
                a.y,
                b.x,
                b.y,
                (a.x - b.x).abs(),
                (a.y - b.y).abs()
            );
        }
    }

    /// Common tail after a stroke mutates the plan: persist, surface any
    /// collinearity questions, then (re)start dimensioning on the result.
    /// Callers snapshot history (`push_history_snapshot`) BEFORE calling this, so
    /// an undo entry exists even if the commit fails.
    fn commit_review_redimension(&mut self) -> Result<(), String> {
        self.commit_to_ber()?;
        self.populate_collinearity_review();
        self.maybe_start_dimensioning();
        Ok(())
    }

    fn apply_drawperp_stroke(&mut self, points: &[Point2f32], closed: bool) -> Result<(), String> {
        // Snapshot pre-stroke (may be empty / pre-dim) for undo.
        let undo_snap = self.current_history_snapshot();
        dump_stroke("draw-outline", points);
        println!(
            "[new-storey] PIXEL outline path: storey={:?}, {} points (this is the from-scratch \
             pixel path)",
            self.current_storey_idx,
            points.len()
        );
        if matches!(self.current_storey_idx, Some(i) if i > 0) {
            eprintln!(
                "[new-storey] WARNING: pixel path on upper storey {:?} — the world/snap path did \
                 NOT trigger (expected the previous storey's watermark + metre-space capture). The \
                 outline won't snap to the storey below and will re-scale from scratch.",
                self.current_storey_idx
            );
        }
        // Method-2 snap: when this is the FIRST stroke on a new upper storey
        // and a previous storey exists, snap each freehand point onto the
        // previous storey's outline so the new footprint reuses those exact
        // coordinates wherever the user drew close. Threshold scales to the
        // previous storey's diagonal (see `snap_to_previous_storey_outline`).
        let snapped_owned = self.snap_to_previous_storey_outline(points);
        let points: &[Point2f32] = snapped_owned.as_deref().unwrap_or(points);
        self.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
            .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
        println!("floor plan: {:?}", self.floor_plan);
        self.warn_if_non_orthogonal("draw outline");
        self.dimensioning = DimensionState::default();
        self.push_history_snapshot(undo_snap);
        self.commit_review_redimension()
    }

    /// If the editor is on a NEW storey (current outline empty) and a previous
    /// storey exists, snap each stroke point that lands within ~6% of the
    /// previous outline's diagonal onto that outline. Returns `None` (caller
    /// uses the raw stroke as-is) when no snap applies — first storey, no
    /// previous geometry, or current storey already has an outline.
    fn snap_to_previous_storey_outline(&self, points: &[Point2f32]) -> Option<Vec<Point2f32>> {
        // Only on a fresh storey — once an outline exists this is a redraw, not
        // a method-2 trace, and we don't want to silently snap the user's stroke.
        if self.floor_plan.ring_count() > 0 {
            return None;
        }
        let ring = self.previous_storey_ring()?;
        // ~8% of the previous diagonal: forgiving enough that a wall the user
        // drew "close" to the storey below lands on it, yet far below the depth
        // of a real extension (e.g. a garage bump-out), which stays un-snapped.
        let tol =
            (manip_2d_plan::ring_diagonal(&ring) * self.tuning.new_storey.snap_tol_ratio).max(1e-3);
        let snapped = manip_2d_plan::snap_stroke_to_ring(points, &ring, tol);
        let moved = points
            .iter()
            .zip(snapped.iter())
            .filter(|(a, b)| (a.x - b.x).abs() > 1e-4 || (a.y - b.y).abs() > 1e-4)
            .count();
        println!(
            "[new-storey] snap to previous outline: tol {:.3} m, {}/{} points pulled onto it",
            tol,
            moved,
            points.len()
        );
        Some(snapped)
    }

    /// The previous storey's outline ring as `Point2f32` (world/metre coords),
    /// or `None` when the editor is on storey 0 / there's no previous geometry.
    /// Used both to snap a new storey's stroke onto it and to build a reference
    /// `FloorPlan` for pinning shared walls.
    fn previous_storey_ring(&self) -> Option<Vec<Point2f32>> {
        let idx = self.current_storey_idx?;
        if idx == 0 {
            return None;
        }
        let prev = self.current_record.as_ref()?.data.storeys.get(idx - 1)?;
        // Merge phase zones back to the full footprint when the previous storey
        // is split; otherwise the single region IS the footprint.
        let ring_f64 = if prev.floors.regions.len() > 1 {
            prev.floors.merged_outline()
        } else {
            prev.floors
                .to_vertex_lists()
                .into_iter()
                .find(|r| r.len() >= 3)?
        };
        if ring_f64.len() < 3 {
            return None;
        }
        Some(
            ring_f64
                .into_iter()
                .map(|p| Point2f32::new(p.x as f32, p.y as f32))
                .collect(),
        )
    }

    /// Apply the FIRST outline stroke of a new upper storey, captured in the
    /// previous storey's *world* (metre) space (see [`SketchEvent::OutlineStrokeWorld`]).
    ///
    /// Because the stroke is already in metres, the new storey inherits scale
    /// for free: endpoints snap onto the previous outline, and every wall the
    /// new footprint *shares* with the previous storey is pinned (via
    /// [`FloorPlan::params_pinned_to_outline`]) so it isn't re-prompted — only
    /// the genuinely-new walls (e.g. a garage the upper storey extends over)
    /// remain to dimension. The geometry (snap, orthogonalize, pin) all lives
    /// in `manip_2d_plan`; here we only sequence it.
    pub fn apply_outline_stroke_world(&mut self, world_points: &[(f32, f32)]) {
        println!(
            "[new-storey] WORLD outline path: storey={:?}, {} world points",
            self.current_storey_idx,
            world_points.len()
        );
        dump_stroke(
            "new-storey",
            &world_points
                .iter()
                .map(|(x, y)| Point2f32::new(*x, *y))
                .collect::<Vec<_>>(),
        );
        // Run the engine's pixel-based close-check to learn whether the user
        // closed the loop; the pixel points themselves are discarded in favour
        // of the world points the UI already converted.
        let closed = match self.sketch_engine.end_stroke() {
            Ok(Some(stroke)) => stroke.is_closed,
            Ok(None) => {
                println!("[new-storey] no stroke in engine — nothing to apply");
                return;
            }
            Err(e) => {
                eprintln!("[new-storey] end_stroke failed: {:?}", e);
                return;
            }
        };
        if let Err(e) = self.apply_outline_world_inner(world_points, closed) {
            eprintln!("[new-storey] apply_outline_stroke_world failed: {}", e);
        }
    }

    fn apply_outline_world_inner(
        &mut self,
        world_points: &[(f32, f32)],
        closed: bool,
    ) -> Result<(), String> {
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        // Snap to the previous outline (meaningful now that both are metres),
        // then build a reference plan from that same ring for pinning.
        let snapped = self
            .snap_to_previous_storey_outline(&pts)
            .unwrap_or_else(|| pts.clone());
        let prev_ring = self
            .previous_storey_ring()
            .ok_or_else(|| "no previous storey to draw on".to_string())?;
        // Metre-scale RDP tolerance: the pixel-tuned 10.0 used for fresh sketches
        // would collapse a real outline. ~1% of the previous diagonal smooths
        // freehand jitter without eating real corners.
        let prev_diag = manip_2d_plan::ring_diagonal(&prev_ring);
        let ns = self.tuning.new_storey.clone();
        let tol = (prev_diag * ns.rdp_tol_ratio).max(ns.rdp_tol_floor);
        println!(
            "[new-storey] closed={}, prev_ring {} verts (diag {:.2} m), RDP tol {:.3} m",
            closed,
            prev_ring.len(),
            prev_diag,
            tol
        );

        let undo_snap = self.current_history_snapshot();
        let prev_plan = FloorPlan::from_clean_ring(&prev_ring)
            .map_err(|e| format!("Failed to build reference outline: {:?}", e))?;

        // Does the trace merely REPRODUCE the previous outline (following the
        // boundary, with at most hand slop), or is there a SUSTAINED new excursion
        // (an extension)? If there's no run of consecutive points sitting clearly
        // off the boundary, copy the previous outline VERBATIM — carrying its
        // exact linear representation up, curves and all, instead of
        // orthogonalizing and squaring them off. A real extension (a sustained run
        // of points well off the boundary) takes the orthogonalize path, which
        // adds the new walls (squaring off only the freshly-drawn part).
        //
        // `NEW_WALL_TOL_M` is ABSOLUTE metres (this path is always metre-scale)
        // and deliberately MUCH looser than the snap tolerance: tracing a curve
        // freehand strays past the snap radius but stays within ~1 m of the
        // boundary — that's still "following", not a new wall. Two-plus
        // consecutive points beyond it are a genuine feature (a garage, a nib).
        let new_wall_tol_m = ns.new_wall_off_tol_m;
        // Measure reproduce-ness on the RAW trace, NOT the snapped one: the snap
        // has already pulled a shallow setback wall ONTO the boundary, which would
        // erase the very divergence we're testing for. A faithful trace strays
        // only by hand slop, so raw still reads as a reproduce; a deliberate
        // setback shows up.
        let off_run = prev_plan.longest_offring_run(&pts, new_wall_tol_m);
        // A reproduce must ALSO cover the whole previous perimeter — a trace that
        // SKIPS a chunk (stops early on a side, cuts a bulge off, overhangs a
        // cut-out) isn't a reproduce. `cover_tol` is tighter than the snap radius
        // (and capped in absolute metres) so a small setback registers as
        // unvisited rather than being absorbed as slop; faithful curve tracing
        // stays well within it.
        let cover_tol = (prev_diag * ns.cover_tol_ratio).clamp(ns.cover_tol_min, ns.cover_tol_max);
        let uncovered = prev_plan.uncovered_perimeter_run(&pts, cover_tol);
        let uncovered_limit = prev_diag * ns.uncovered_limit_ratio;
        let reproduces_outline = off_run < 2 && uncovered < uncovered_limit;
        println!(
            "[new-storey] reproduce check: longest off-ring run = {} pts (> {:.2} m off), \
             largest unvisited boundary span = {:.2} m (limit {:.2} m) → {}",
            off_run,
            new_wall_tol_m,
            uncovered,
            uncovered_limit,
            if reproduces_outline {
                "REPRODUCE (copy verbatim, curves kept)"
            } else {
                "ORTHOGONALIZE (different storey — new walls / skipped boundary)"
            }
        );
        self.dimensioning = DimensionState::default();
        self.dimensioning.scale_set = true;
        if reproduces_outline {
            // Verbatim copy: the whole outline is shared → every param is known,
            // nothing to dimension. The curve survives unchanged.
            self.floor_plan = prev_plan;
            self.dimensioning.known_params = self.floor_plan.all_active_params();
            println!(
                "[new-storey] trace reproduces the previous outline → copied verbatim ({} verts, curve preserved)",
                self.floor_plan.get_points().len()
            );
            self.push_history_snapshot(undo_snap);
            return self.commit_review_redimension();
        }

        // Partial-reproduce path: the storey is MOSTLY the one below but differs
        // over a short run (a setback, an overhang). Splice — follow the previous
        // outline VERBATIM (curves and all) where the trace hugs it, and square
        // off only the diverging walls. Pass the RAW (un-snapped) trace so a
        // diverging wall keeps its drawn position. Falls through to the
        // whole-trace orthogonalize below if the trace doesn't decompose cleanly.
        if let Some(spliced) = prev_plan.outline_following_trace(&pts, cover_tol, tol, &ns.splice) {
            match FloorPlan::from_clean_ring(&spliced) {
                Ok(fp) => {
                    self.floor_plan = fp;
                    self.warn_if_non_orthogonal("draw outline (world, spliced)");
                    self.dimensioning.known_params =
                        prev_plan.params_pinned_to_outline(&self.floor_plan);
                    let active = self.floor_plan.all_active_params();
                    println!(
                        "[new-storey] spliced outline: followed the previous boundary and squared \
                         off only the new walls → {} verts, pinned {} of {} active params \
                         ({} new wall param(s) left to dimension)",
                        self.floor_plan.get_points().len(),
                        self.dimensioning.known_params.len(),
                        active.len(),
                        active
                            .len()
                            .saturating_sub(self.dimensioning.known_params.len()),
                    );
                    self.push_history_snapshot(undo_snap);
                    return self.commit_review_redimension();
                }
                Err(e) => println!(
                    "[new-storey] splice built {} verts but they're not a simple ring ({:?}) \
                     → falling back to orthogonalize",
                    spliced.len(),
                    e
                ),
            }
        }

        // Extension path: orthogonalize to the WORLD axes (not the stroke's
        // dominant angle) so a slightly-rotated freehand trace can't tilt the
        // storey off the frame below; pin the walls shared with the previous
        // storey so dimensioning prompts only the genuinely-new ones.
        self.floor_plan = FloorPlan::from_points_to_world_orthogonal(&snapped, tol, closed)
            .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
        self.warn_if_non_orthogonal("draw outline (world)");
        self.dimensioning.known_params = prev_plan.params_pinned_to_outline(&self.floor_plan);
        let active = self.floor_plan.all_active_params();
        println!(
            "[new-storey] built outline: {} verts, scale_set=true, pinned {} of {} active params \
             ({} new wall param(s) left to dimension)",
            self.floor_plan.get_points().len(),
            self.dimensioning.known_params.len(),
            active.len(),
            active
                .len()
                .saturating_sub(self.dimensioning.known_params.len()),
        );

        self.push_history_snapshot(undo_snap);
        self.commit_review_redimension()
    }

    /// Apply an AddDetachedSection stroke: append a ring (or replace if
    /// there's no outline yet), persist, populate review.
    fn apply_addsection_stroke(
        &mut self,
        points: &[Point2f32],
        closed: bool,
    ) -> Result<(), String> {
        let undo_snap = self.current_history_snapshot();
        let had_outline = self.floor_plan.ring_count() > 0;
        if !had_outline {
            self.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
                .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
            // Same as DrawPerpWalls when there was nothing: reset dimensioning.
            self.dimensioning = DimensionState::default();
        } else {
            self.floor_plan
                .add_polygon(points, 10.0, closed)
                .map_err(|e| format!("Failed to add section: {:?}", e))?;
            // Existing known_params remain valid — apply_orthogonal_constraints
            // reuses param IDs for coords that match (ring 0's coords didn't
            // change). The new ring contributes additional unknown params we'll
            // pick up in the next dimensioning round.
            // KNOWN LIMITATION: if dimensions were already set (scale_set),
            // the new ring's vertex coords are in pixels while existing rings
            // are in meters. Mixed units will look wrong on the canvas until
            // a per-stroke pixels→meters conversion is added.
        }
        println!(
            "floor plan (after AddDetachedSection): {:?}",
            self.floor_plan
        );
        // History snapshotted BEFORE the commit (consistent with the other
        // apply_* methods) so an undo entry exists even if the commit fails.
        self.push_history_snapshot(undo_snap);
        self.commit_review_redimension()
    }

    /// Entry point for Extend strokes from the UI. The stroke comes in as
    /// (f32, f32) world-coord pairs (already converted from canvas pixels
    /// by sketch.rs via `fit_transform.inverse`). Clears any leftover
    /// canvas-pixel stroke from the sketch engine, then forwards to the
    /// merge logic.
    pub fn apply_extend_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        println!("Extend stroke (world coords): {} points", pts.len());
        dump_stroke("extend", &pts);
        if let Err(e) = self.apply_extend_stroke(&pts) {
            eprintln!("apply_extend_stroke: {}", e);
        }
    }

    /// Apply an Extend stroke. The geometry — snapping, orthogonalizing, and
    /// splicing the path into the parametric plan — lives in `FloorPlan`; here
    /// we only orchestrate app state. Dispatches by the active tool: `ExtendOrtho`
    /// adds a stepped porch, `ExtendCurve` a faceted arc. Because the splice
    /// preserves existing vertex/parameter IDs, the dimensioning state stays
    /// valid: we keep `known_params` (dropping only any that were removed)
    /// instead of resetting, so the user is asked to dimension just the new
    /// walls.
    fn apply_extend_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        // ExtendCurve defers: hold a provisional arc until its attach lines are
        // dimensioned (see `begin_arc_stroke`), rather than faceting now.
        if self.sketch_engine.edit_tool == EditTools::ExtendCurve {
            return self.begin_arc_stroke(points);
        }
        let undo_snap = self.current_history_snapshot();
        let minted = self
            .floor_plan
            .extend_ortho(points)
            .map_err(|e| format!("{}", e))?;
        println!("Extend: {} new param(s) {:?}", minted.len(), minted);
        self.warn_if_non_orthogonal("extend");
        // The porch is a fix-up to the ENVELOPE — once spliced, that it was a
        // "porch" is irrelevant. Drop any vertex it left collinear (e.g. a
        // corner a flush porch turned into a mid-wall point) so the outline
        // stays minimal, then re-sync dimensioning state.
        self.clean_envelope_collinear();
        // History snapshotted before the commit (consistent across apply_*).
        self.push_history_snapshot(undo_snap);
        self.commit_to_ber()?;
        // Ask about collinearity ONLY for the porch's new walls — e.g. whether
        // a new porch front lines up with an earlier one. We never assume it;
        // and we don't re-surface pairs among the pre-existing walls. (Extend
        // uses this param-scoped review, so it can't share `commit_review_redimension`.)
        let suggestions = self.floor_plan.collinear_suggestions_for_params(&minted);
        if !suggestions.is_empty() {
            self.review = Some(CollinearityReview {
                suggestions,
                cursor: 0,
                rejections: Vec::new(),
            });
        }
        // Kicks off dimensioning for the porch's new walls — but defers while a
        // collinearity review is pending (it returns early when `review` is set;
        // resolving the review then starts dimensioning).
        self.maybe_start_dimensioning();
        Ok(())
    }

    /// Entry point for Trim strokes from the UI (world coords, like Extend).
    /// The stroke's first and last points define the straight cut line.
    pub fn apply_trim_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        println!("Trim stroke (world coords): {} points", pts.len());
        dump_stroke("trim", &pts);
        if let Err(e) = self.apply_trim_stroke(&pts) {
            eprintln!("apply_trim_stroke: {}", e);
        }
    }

    /// Apply a Trim, discarding the smaller side. Dispatches by the active
    /// tool: `Trim` cuts a straight (possibly diagonal) line; `TrimOrtho` carves
    /// an orthogonal stepped cut; `TrimCurve` carves a faceted curved recess.
    /// The geometry lives in `FloorPlan::trim_by_stroke` / `trim_ortho` /
    /// `trim_curve`; here we only orchestrate app state. Surviving walls keep
    /// their parameters.
    fn apply_trim_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        if points.len() < 2 {
            return Err("trim stroke needs at least 2 points".into());
        }
        // TrimCurve defers like ExtendCurve: a provisional arc dimensioned by
        // its attach lines, then faceted in. The drawn bulge direction (inward)
        // is what carves the recess — no separate keep-larger step needed.
        if self.sketch_engine.edit_tool == EditTools::TrimCurve {
            return self.begin_arc_stroke(points);
        }
        let undo_snap = self.current_history_snapshot();
        let minted = match self.sketch_engine.edit_tool {
            EditTools::TrimOrtho => self.floor_plan.trim_ortho(points),
            _ => self.floor_plan.trim_by_stroke(points),
        }
        .map_err(|e| format!("{}", e))?;
        println!("Trim: {} new param(s) {:?}", minted.len(), minted);
        self.clean_envelope_collinear();
        // History snapshotted before the commit (consistent across apply_*). A
        // plain Trim never asks about collinearity, so no review step here.
        self.push_history_snapshot(undo_snap);
        self.commit_to_ber()?;
        self.maybe_start_dimensioning();
        Ok(())
    }

    /// After a committed extend/trim splice, drop the collinear vertices it left
    /// redundant — what matters is the resulting ENVELOPE, not that a porch was
    /// added separately. (A flush porch turns its attach corner into a mid-wall
    /// point; that's spurious.) Surviving vertices keep their IDs, so we just
    /// re-sync the dimensioning state to the params/vertices that remain.
    ///
    /// `preserve` is where deliberately-placed subdivision vertices go — e.g. a
    /// point marking a change of flooring type along an otherwise-straight wall,
    /// which is collinear but NOT spurious. None exist yet, so it's empty; when
    /// that feature lands, those vertex IDs are passed here and kept.
    fn clean_envelope_collinear(&mut self) {
        let preserve: HashSet<VertexId> = HashSet::new();
        let removed = self.floor_plan.remove_collinear_vertices(&preserve);
        if !removed.is_empty() {
            println!(
                "Cleanup: removed {} spurious collinear vertex/vertices {:?}",
                removed.len(),
                removed
            );
        }
        self.resync_dimensioning_to_plan();
    }

    /// Begin a provisional curved wall (shared by `ExtendCurve`/`TrimCurve`).
    /// `FloorPlan::begin_arc` fits the stroke to a circular arc and splices only
    /// its two endpoints onto the outline (a straight placeholder; the shape is
    /// unchanged). We suppress that placeholder edge from dimensioning prompts —
    /// so only the two attach stubs are asked about — and stash the arc in
    /// `pending_arc`. Once both stubs are dimensioned (or both endpoints reused
    /// corners, so there's nothing to dimension), the UI offers a radius slider
    /// ([`pending_arc_bulge`](Self::pending_arc_bulge)); accepting it
    /// ([`commit_pending_arc`](Self::commit_pending_arc)) bakes the curve in.
    /// Nothing about the geometry changes until then.
    fn begin_arc_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        // ---- CURVE DEBUG (temporary): everything needed to reproduce ----
        // Plan + dimensioning state BEFORE the curve, so the exact dimensioned
        // shape can be reconstructed in a test.
        println!("=== CURVE DEBUG: begin_arc_stroke ===");
        println!("  plan BEFORE: {:?}", self.floor_plan);
        {
            let mut k: Vec<_> = self.dimensioning.known_params.iter().copied().collect();
            k.sort();
            println!(
                "  known_params={:?} scale_set={}",
                k, self.dimensioning.scale_set
            );
        }
        // The raw stroke, as a paste-able Rust literal.
        print!("  stroke ({} pts): vec![", points.len());
        for p in points {
            print!("p({:.3},{:.3}),", p.x, p.y);
        }
        println!("]");
        // ---- end debug header ----

        let undo_snap = self.current_history_snapshot();
        let (start, end, bulge_ratio) = self
            .floor_plan
            .begin_arc(points)
            .map_err(|e| format!("{}", e))?;
        // Don't prompt the straight placeholder edge — only its attach stubs.
        if let Some(skip) = self.placeholder_skip_vertex(start, end) {
            self.dimensioning.skipped_walls.insert(skip);
        }
        self.pending_arc = Some(PendingArc {
            start,
            end,
            bulge_ratio,
        });
        println!(
            "Curve: provisional arc (bulge ratio {:.3}); dimension the two attach lines to bake it in",
            bulge_ratio
        );
        self.resync_dimensioning_to_plan();

        // ---- CURVE DEBUG (temporary): the spliced structure ----
        println!(
            "  begin: start_vid={} end_vid={} bulge={:.3} skip_vid={:?}",
            start,
            end,
            bulge_ratio,
            self.placeholder_skip_vertex(start, end)
        );
        if let (Some((r, is)), Some((_, ie))) = (
            self.floor_plan.find_vertex(start),
            self.floor_plan.find_vertex(end),
        ) {
            println!("  start at ring{r}/idx{is}, end at idx{ie}");
        }
        {
            let mut a: Vec<_> = self.floor_plan.all_active_params().into_iter().collect();
            a.sort();
            let mut k: Vec<_> = self.dimensioning.known_params.iter().copied().collect();
            k.sort();
            println!("  active_params={a:?} known_after_retain={k:?}");
        }
        let ring = self.floor_plan.polygons()[0].clone();
        for e in 0..ring.len() {
            println!(
                "  edge {e} {:?}->{:?} axis={:?} deps={:?}",
                (ring[e].x, ring[e].y),
                (ring[(e + 1) % ring.len()].x, ring[(e + 1) % ring.len()].y),
                self.floor_plan.wall_axis(0, e),
                self.floor_plan.wall_param_deps(0, e),
            );
        }
        println!("=== end CURVE DEBUG ===");
        // ---- end debug ----

        self.commit_to_ber()?;
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// The provisional arc's placeholder edge connects `start` and `end`
    /// directly; return whichever is that edge's RING-START vertex (the one
    /// `next_promptable_wall` keys the placeholder wall by), so it can be
    /// skipped. `None` if the two are no longer adjacent on one ring.
    fn placeholder_skip_vertex(&self, start: VertexId, end: VertexId) -> Option<VertexId> {
        let (r1, is) = self.floor_plan.find_vertex(start)?;
        let (r2, ie) = self.floor_plan.find_vertex(end)?;
        if r1 != r2 {
            return None;
        }
        let n = self.floor_plan.polygons().get(r1)?.len();
        if n == 0 {
            return None;
        }
        if (is + 1) % n == ie {
            Some(start)
        } else if (ie + 1) % n == is {
            Some(end)
        } else {
            None
        }
    }

    /// True once both of the provisional arc's attach stubs are dimensioned —
    /// i.e. every parameter the placeholder edge's length depends on is known,
    /// fixing both endpoints. That's the cue to facet the arc in.
    fn pending_arc_ready(&self, pa: &PendingArc) -> bool {
        let (Some((r1, is)), Some((r2, ie))) = (
            self.floor_plan.find_vertex(pa.start),
            self.floor_plan.find_vertex(pa.end),
        ) else {
            return false;
        };
        if r1 != r2 {
            return false;
        }
        let Some(n) = self.floor_plan.polygons().get(r1).map(|p| p.len()) else {
            return false;
        };
        let edge_start = if (is + 1) % n == ie {
            is
        } else if (ie + 1) % n == is {
            ie
        } else {
            return false;
        };
        let deps = self.floor_plan.wall_param_deps(r1, edge_start);
        !deps.is_empty()
            && deps
                .iter()
                .all(|p| self.dimensioning.known_params.contains(p))
    }

    /// The provisional curve's current bulge (signed sagitta / chord), but only
    /// once it's ready to have its radius tuned: a pending arc whose attach
    /// lines are all set, with no dimension prompt outstanding. The UI binds a
    /// radius slider to this; `None` hides the slider (still drawing /
    /// dimensioning the attach lines).
    pub fn pending_arc_bulge(&self) -> Option<f32> {
        let pa = self.pending_arc.as_ref()?;
        if self.dimensioning.current_prompt.is_some() || !self.pending_arc_ready(pa) {
            return None;
        }
        Some(pa.bulge_ratio)
    }

    /// Set the provisional curve's bulge from the radius slider (clamped). Just
    /// updates the amber preview; commit happens on accept. No-op with no arc.
    pub fn set_pending_arc_bulge(&mut self, ratio: f32) {
        if let Some(pa) = self.pending_arc.as_mut() {
            pa.bulge_ratio = ratio.clamp(-1.5, 1.5);
        }
    }

    /// Accept the provisional curve at its current radius: facet the arc into
    /// the plan and LOCK the facet params (so later dimensioning of other walls
    /// can't let `repair`/`aesthetic_rescale` redistribute the facet vertices
    /// and flatten the curve). Clears the pending arc. If faceting fails the arc
    /// is dropped (the straight placeholder remains) rather than left stuck.
    pub fn commit_pending_arc(&mut self) -> Result<(), String> {
        let Some(pa) = self.pending_arc.clone() else {
            return Ok(());
        };
        let undo_snap = self.current_history_snapshot();
        let facet_params = match self.floor_plan.finish_arc(pa.start, pa.end, pa.bulge_ratio) {
            Ok(params) => params,
            Err(e) => {
                eprintln!(
                    "finish_arc failed ({e}); dropping the provisional curve, keeping \
                     the straight placeholder. bulge={:.3}",
                    pa.bulge_ratio
                );
                self.pending_arc = None;
                self.push_history_snapshot(undo_snap);
                return Ok(());
            }
        };
        self.dimensioning
            .known_params
            .extend(facet_params.iter().copied());
        self.pending_arc = None;
        println!(
            "Curve: committed at bulge {:.3} ({} facet params locked)",
            pa.bulge_ratio,
            facet_params.len()
        );
        self.commit_to_ber()?;
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// World-coord points of the provisional arc preview, if one is mid-flow.
    /// The UI draws this as an overlay; the model still holds the straight
    /// placeholder until the attach dimensions are entered.
    pub fn pending_arc_preview(&self) -> Option<Vec<(f64, f64)>> {
        let pa = self.pending_arc.as_ref()?;
        let pts = self
            .floor_plan
            .arc_preview(pa.start, pa.end, pa.bulge_ratio)?;
        Some(pts.iter().map(|p| (p.x as f64, p.y as f64)).collect())
    }

    /// Detect near-collinear walls and populate the review if any.
    ///
    /// After AddDetachedSection appends a new ring, pairs entirely within older
    /// rings were either resolved when those rings were drawn or were
    /// intentionally left as distinct walls — don't re-ask. We only
    /// surface pairs that involve the newly-added ring. For DrawPerpWalls
    /// the floor_plan was just replaced with a single ring at index 0,
    /// so the filter is a no-op.
    fn populate_collinearity_review(&mut self) {
        // Separations scale to the plan's extent so the same review behaviour
        // holds whether the plan is in pixels (pre-dimension) or metres (an
        // upper storey drawn in world coords). A fixed range tuned for pixels
        // would treat every wall in a metre-scale storey as near-collinear.
        let diag = self.floor_plan.bounding_box_diagonal();
        let min_sep = (diag * self.tuning.collinearity.min_separation_ratio).max(1e-4);
        let max_sep = diag * self.tuning.collinearity.max_separation_ratio;
        let newest_ring = self.floor_plan.ring_count().saturating_sub(1);
        // When this storey was drawn over a previous one, prefer to KEEP the
        // line that lies on the previous storey's outline as the merge target,
        // so a near-collinear pair collapses onto the storey-below base rather
        // than onto a spurious slipped-nib offset. `apply_collinear_suggestion`
        // rewrites edge B onto edge A, so the wall on the previous outline must
        // be edge A. We measure each side's distance to the nearest parallel
        // edge of the previous ring and put the closer one in the A slot.
        let prev_ring = self.previous_storey_ring();
        let dist_to_prev = |coord: f32, axis: &Axis| -> f32 {
            let Some(ring) = prev_ring.as_ref() else {
                return f32::MAX;
            };
            let n = ring.len();
            let mut best = f32::MAX;
            for i in 0..n {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                match axis {
                    // A horizontal previous edge anchors a horizontal pair's y;
                    // a vertical one anchors a vertical pair's x.
                    Axis::Horizontal if (a.y - b.y).abs() < 1e-3 => {
                        best = best.min((coord - a.y).abs())
                    }
                    Axis::Vertical if (a.x - b.x).abs() < 1e-3 => {
                        best = best.min((coord - a.x).abs())
                    }
                    _ => {}
                }
            }
            best
        };
        let suggestions: Vec<_> = self
            .floor_plan
            .suggest_collinear(min_sep, max_sep)
            .into_iter()
            .filter(|s| s.ring_a == newest_ring || s.ring_b == newest_ring)
            .map(|mut s| {
                if dist_to_prev(s.coord_b, &s.axis) < dist_to_prev(s.coord_a, &s.axis) {
                    std::mem::swap(&mut s.ring_a, &mut s.ring_b);
                    std::mem::swap(&mut s.edge_a, &mut s.edge_b);
                    std::mem::swap(&mut s.coord_a, &mut s.coord_b);
                }
                s
            })
            .collect();
        println!(
            "[new-storey] collinearity scan: plan diag {:.2}, sep range [{:.3}, {:.3}] → {} \
             near-collinear pair(s) to review",
            diag,
            min_sep,
            max_sep,
            suggestions.len()
        );
        if !suggestions.is_empty() {
            self.review = Some(CollinearityReview {
                suggestions,
                cursor: 0,
                rejections: Vec::new(),
            });
        }
    }
}

/// Statistics about a floor plan
#[derive(Debug, Clone)]
pub struct FloorPlanStats {
    pub perimeter: f32,
    pub area: f32,
    pub num_vertices: usize,
}

#[cfg(test)]
mod trim_collapse_tests {
    use super::*;

    /// The assessor's flow: draw a square, trim a corner, then zero the two
    /// remnant cut walls so the cuts snap onto corners — leaving a triangle.
    /// Exercises the real controller path: `compute_next_dimension_prompt`
    /// picks each axis-aligned remnant wall, and `apply_dimension(0.0)`
    /// collapses it (the diagonal cut wall is never offered).
    #[test]
    fn trim_corner_then_zero_remnants_makes_a_triangle() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());

        // A 10x10 square (5th point is the lift-near-start drag point).
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        // Treat it as fully dimensioned: scale set, both square params known.
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();

        // Trim off the top-right corner: right edge at y=5 to top edge at x=5.
        c.floor_plan
            .trim_by_stroke(&[Point2f32::new(10.0, 5.0), Point2f32::new(5.0, 10.0)])
            .unwrap();
        assert_eq!(
            c.floor_plan.polygons()[0].len(),
            5,
            "corner cut should leave a pentagon"
        );

        // Zero the two remnant cut walls. Each apply_dimension(0.0) collapses
        // one cut vertex; the diagonal is never prompted (non-axis-aligned).
        for step in 0..2 {
            c.compute_next_dimension_prompt();
            assert!(
                c.dimensioning.current_prompt.is_some(),
                "expected a promptable remnant wall at step {step}"
            );
            c.apply_dimension(0.0)
                .unwrap_or_else(|e| panic!("zero-collapse failed at step {step}: {e}"));
        }

        let ring = &c.floor_plan.polygons()[0];
        assert_eq!(
            ring.len(),
            3,
            "zeroing both remnants should yield a triangle"
        );
        assert!(c.floor_plan.validate_simple().is_ok());
        // The triangle is the square minus the cut corner: (0,0),(10,0),(0,10).
        for corner in [(0.0, 0.0), (10.0, 0.0), (0.0, 10.0)] {
            assert!(
                ring.iter()
                    .any(|p| (p.x - corner.0).abs() < 1e-3 && (p.y - corner.1).abs() < 1e-3),
                "triangle should keep corner {corner:?}"
            );
        }
    }

    /// Zeroing a regular (orthogonal) wall is rejected — only a trim cut wall
    /// can be collapsed. Guards against silently deleting a drawn wall.
    #[test]
    fn zero_rejected_for_a_regular_wall() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        // Scaled, but no walls dimensioned yet — so a plain orthogonal wall is
        // promptable. (No trim, so no diagonal cut edge anywhere.)
        c.dimensioning.scale_set = true;
        c.compute_next_dimension_prompt();
        assert!(c.dimensioning.current_prompt.is_some());

        let err = c
            .apply_dimension(0.0)
            .expect_err("0 on a regular wall must be rejected");
        assert!(err.contains("trim cut"), "unexpected error: {err}");
        // Shape untouched.
        assert_eq!(c.floor_plan.polygons()[0].len(), 4);
    }

    /// After a corner trim there are 3 undimensioned segments (2 axis-aligned
    /// remnants + the diagonal cut) but only 2 degrees of freedom. The assessor
    /// must be able to dimension ANY two — including the diagonal — since in the
    /// field they may only be able to measure certain ones. This sets one
    /// remnant, then dimensions the diagonal itself, and checks the diagonal
    /// ends up the requested length.
    #[test]
    fn diagonal_cut_wall_is_dimensionable() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();

        // Trim the top-right corner: right edge at y=5 to top edge at x=5.
        c.floor_plan
            .trim_by_stroke(&[Point2f32::new(10.0, 5.0), Point2f32::new(5.0, 10.0)])
            .unwrap();

        // Set the first prompted (axis-aligned) remnant to a known length.
        c.compute_next_dimension_prompt();
        assert!(c.dimensioning.current_prompt.is_some());
        c.apply_dimension(4.0).expect("set first remnant");

        // Now cycle prompts to the diagonal (non-axis-aligned), skipping any
        // remaining axis-aligned remnant, and dimension it to 7.0.
        let mut dimensioned_diagonal = false;
        for _ in 0..5 {
            c.compute_next_dimension_prompt();
            let Some(prompt) = c.dimensioning.current_prompt.clone() else {
                break;
            };
            let (ring, edge) = c.floor_plan.find_vertex(prompt.wall_start_vertex).unwrap();
            if c.floor_plan.wall_axis(ring, edge).is_none() {
                c.apply_dimension(7.0).expect("dimension the diagonal");
                dimensioned_diagonal = true;
                break;
            }
            c.skip_current_dimension();
        }
        assert!(
            dimensioned_diagonal,
            "the diagonal cut wall should be offered and settable"
        );

        // Verify the polygon's one diagonal edge is ~7.0 long.
        let ring = &c.floor_plan.polygons()[0];
        let n = ring.len();
        let mut diag_len = None;
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            if (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3 {
                diag_len = Some(((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt());
            }
        }
        let diag_len = diag_len.expect("a diagonal edge");
        assert!(
            (diag_len - 7.0).abs() < 1e-2,
            "diagonal should be 7.0, got {diag_len}"
        );
        assert!(c.floor_plan.validate_simple().is_ok());
    }

    /// The TrimOrtho tool routes to the orthogonal-polygon trim: a stepped cut
    /// path carves a notch (area removed, extra vertices, stays orthogonal) —
    /// not the straight line cut the plain Trim tool would attempt.
    #[test]
    fn trim_ortho_tool_carves_an_orthogonal_notch() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();

        let area = |c: &AppController| -> f32 {
            let r = &c.floor_plan.polygons()[0];
            let n = r.len();
            (0..n)
                .map(|i| {
                    let (a, b) = (r[i], r[(i + 1) % n]);
                    a.x * b.y - b.x * a.y
                })
                .sum::<f32>()
                .abs()
                * 0.5
        };
        let before = area(&c);

        c.set_edit_mode(EditTools::TrimOrtho);
        // Rectangular notch into the top edge (y=10), down to y=7, x in [3,7].
        let notch = vec![
            Point2f32::new(3.0, 10.0),
            Point2f32::new(3.0, 7.0),
            Point2f32::new(7.0, 7.0),
            Point2f32::new(7.0, 10.0),
        ];
        c.apply_trim_stroke(&notch)
            .expect("ortho trim should succeed");

        let after = area(&c);
        assert!(
            after < before,
            "notch should remove area ({after} vs {before})"
        );
        assert!(
            (before - after - 12.0).abs() < 1.0,
            "should remove ~12 (4x3); removed {}",
            before - after
        );
        let ring = &c.floor_plan.polygons()[0];
        assert!(ring.len() > 4, "notch adds vertices");
        // Orthogonal notch — no diagonal edge.
        let n = ring.len();
        let has_diag = (0..n).any(|i| {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3
        });
        assert!(!has_diag, "an orthogonal notch must have no diagonal edge");
        assert!(c.floor_plan.validate_simple().is_ok());
    }

    fn diag_edges(c: &AppController) -> usize {
        let ring = &c.floor_plan.polygons()[0];
        let n = ring.len();
        (0..n)
            .filter(|&i| {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3
            })
            .count()
    }

    /// Extending a porch flush at a corner leaves that corner mid-wall
    /// (collinear). Since only the envelope matters, the controller cleans it
    /// up after the splice — so the outline has no spurious collinear vertex.
    #[test]
    fn extend_porch_at_corner_cleans_the_spurious_vertex() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.set_edit_mode(EditTools::ExtendOrtho);
        // Freehand porch flush at corner (10,10).
        let porch = vec![
            Point2f32::new(10.3, 10.2),
            Point2f32::new(10.1, 14.1),
            Point2f32::new(5.8, 13.9),
            Point2f32::new(6.1, 10.3),
        ];
        c.apply_extend_stroke(&porch).unwrap();

        let ring = &c.floor_plan.polygons()[0];
        // The flush corner is gone — the right wall is one straight segment.
        assert!(
            !ring
                .iter()
                .any(|q| (q.x - 10.0).abs() < 1e-2 && (q.y - 10.0).abs() < 1e-2),
            "the flush corner should be cleaned from the envelope, ring={ring:?}"
        );
        assert_eq!(diag_edges(&c), 0, "envelope stays orthogonal");
        assert!(c.floor_plan.validate_simple().is_ok());
        // No collinear vertex remains.
        let n = ring.len();
        let collinear = (0..n)
            .filter(|&i| {
                let (pr, cu, nx) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
                let (ux, uy) = (cu.x - pr.x, cu.y - pr.y);
                let (vx, vy) = (nx.x - cu.x, nx.y - cu.y);
                let len = ((ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt()).max(1e-9);
                (ux * vy - uy * vx).abs() / len < 1e-3 && (ux * vx + uy * vy) > 0.0
            })
            .count();
        assert_eq!(collinear, 0, "no spurious collinear vertices remain");
    }

    fn ring_area(c: &AppController) -> f32 {
        let ring = &c.floor_plan.polygons()[0];
        let n = ring.len();
        let mut sum = 0.0_f32;
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            sum += a.x * b.y - b.x * a.y;
        }
        (sum * 0.5).abs()
    }

    /// Dragging a wall in Edit mode must prompt for a distance PERPENDICULAR to
    /// the wall: a vertical wall is measured horizontally (axis X), a horizontal
    /// wall vertically (axis Y). (User: "moving the E's left wall showed me a
    /// vertical dimension; it should be horizontal.")
    #[test]
    fn dragging_a_wall_measures_perpendicular_to_it() {
        let make = || {
            let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
            let square = vec![
                Point2f32::new(0.0, 0.0),
                Point2f32::new(10.0, 0.0),
                Point2f32::new(10.0, 10.0),
                Point2f32::new(0.0, 10.0),
                Point2f32::new(0.2, 0.2),
            ];
            c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
            // The drag prompt only appears once the plan is dimensioned.
            c.dimensioning.scale_set = true;
            c.dimensioning.known_params = c.floor_plan.all_active_params();
            c.set_edit_mode(EditTools::Edit);
            c
        };

        // Left (vertical) wall at x=0 → measured along X.
        let mut c = make();
        assert!(c.edit_mouse_down(0.0, 5.0, 1.0), "should hit the left wall");
        c.edit_mouse_drag(0.5, 5.0); // nudge it horizontally
        c.edit_mouse_up().unwrap();
        let p = c
            .edit_drag_prompt
            .as_ref()
            .expect("a measurement prompt after dragging a wall");
        assert_eq!(
            p.axis,
            ParameterAxis::X,
            "a vertical wall is measured horizontally"
        );

        // Bottom (horizontal) wall at y=0 → measured along Y.
        let mut c = make();
        assert!(
            c.edit_mouse_down(5.0, 0.0, 1.0),
            "should hit the bottom wall"
        );
        c.edit_mouse_drag(5.0, 0.5);
        c.edit_mouse_up().unwrap();
        let p = c
            .edit_drag_prompt
            .as_ref()
            .expect("a measurement prompt after dragging a wall");
        assert_eq!(
            p.axis,
            ParameterAxis::Y,
            "a horizontal wall is measured vertically"
        );
    }

    /// Drawing a living area inside a dimensioned storey: it inherits the
    /// storey's scale (no recalibration), becomes the active shape, is stored,
    /// and the storey outline is untouched when you switch back.
    #[test]
    fn living_area_inherits_scale_is_stored_and_leaves_outline_intact() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true; // storey is dimensioned (metres)
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        let outline_verts = c.floor_plan.polygons()[0].len();

        // Enter living-area mode and draw a sub-area inside the storey.
        c.set_living_area_mode(true);
        assert!(c.living_area_mode());
        let la: Vec<(f32, f32)> = vec![(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0), (2.1, 2.1)];
        c.apply_living_area_stroke_event(&la);

        assert_eq!(c.living_area_count(), 1, "the living area is stored");
        assert_eq!(
            c.active_living_area_index(),
            Some(0),
            "it's the active shape"
        );
        // Inherits the storey scale — no first-dimension calibration step.
        assert!(
            c.dimensioning_scale_set(),
            "a living area inside a dimensioned storey inherits its scale"
        );
        assert!(
            c.floor_plan.polygons()[0].len() >= 4,
            "a sub-area was drawn"
        );
        assert_eq!(c.living_area_polygons().len(), 1, "renderable");

        // Switch back to the outline: it's restored exactly, area still stored.
        c.set_living_area_mode(false);
        assert_eq!(c.active_living_area_index(), None);
        assert_eq!(
            c.floor_plan.polygons()[0].len(),
            outline_verts,
            "the storey outline is untouched by drawing a living area"
        );
        assert_eq!(c.living_area_count(), 1, "the living area is still kept");
    }

    /// A fully-dimensioned living area is persisted as resolved geometry and
    /// comes back on reopen (alongside the outline). Un-dimensioned areas are
    /// NOT written to the DB — only finalised geometry is.
    #[test]
    fn living_areas_survive_save_and_reopen() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let ber = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        c.set_living_area_mode(true);
        c.apply_living_area_stroke_event(&[
            (2.0, 2.0),
            (6.0, 2.0),
            (6.0, 6.0),
            (2.0, 6.0),
            (2.1, 2.1),
        ]);
        assert_eq!(c.living_area_count(), 1);
        // Finalise the living area (mark every wall dimensioned) so it's written
        // to the DB — only finalised geometry is persisted.
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        // Reopen from the DB (same connection): the living area is restored.
        c.open_ber(&ber.id).unwrap();
        assert_eq!(
            c.living_area_count(),
            1,
            "the living area survives save + reopen"
        );
        assert_eq!(
            c.active_living_area_index(),
            None,
            "reopen starts on the storey outline"
        );
        assert_eq!(
            c.living_area_polygons().len(),
            1,
            "still renderable after reopen"
        );
    }

    /// An un-dimensioned outline is still in pixels, not metres, so it is NOT
    /// written to the DB — only a finalised (scale-set, fully dimensioned)
    /// outline persists. (The in-memory copy keeps it, for rendering.)
    #[test]
    fn undimensioned_outline_is_not_persisted() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let ber = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        // Draw an outline but leave it un-dimensioned (scale never set).
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                Point2f32::new(0., 0.),
                Point2f32::new(10., 0.),
                Point2f32::new(10., 10.),
                Point2f32::new(0., 10.),
                Point2f32::new(0.2, 0.2),
            ],
            0.5,
            true,
        )
        .unwrap();
        c.commit_to_ber().unwrap();

        // Reopen from the DB: nothing was persisted (the outline wasn't final).
        c.open_ber(&ber.id).unwrap();
        assert!(
            c.outline.floor_plan.get_points().is_empty(),
            "an un-dimensioned outline isn't written to the DB"
        );
    }

    /// A living area drawn but not dimensioned is excluded from the DB write
    /// (kept in memory); the dimensioned outline around it still persists.
    #[test]
    fn undimensioned_living_area_is_not_persisted() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let ber = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                Point2f32::new(0., 0.),
                Point2f32::new(10., 0.),
                Point2f32::new(10., 10.),
                Point2f32::new(0., 10.),
                Point2f32::new(0.2, 0.2),
            ],
            0.5,
            true,
        )
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        c.set_living_area_mode(true);
        c.apply_living_area_stroke_event(&[
            (2.0, 2.0),
            (6.0, 2.0),
            (6.0, 6.0),
            (2.0, 6.0),
            (2.1, 2.1),
        ]);
        assert_eq!(c.living_area_count(), 1, "drawn (in memory)");

        c.open_ber(&ber.id).unwrap();
        assert_eq!(
            c.living_area_count(),
            0,
            "un-dimensioned living area isn't persisted"
        );
        assert!(
            !c.outline.floor_plan.get_points().is_empty(),
            "but the dimensioned outline is"
        );
    }

    /// Reproduces the reported "stuck dimensioning a living area" bug: a
    /// free-standing interior square living area should accept its first
    /// dimension (it inherits the storey scale, so dimension #1 sets a wall
    /// directly — no calibration step).
    #[test]
    fn dimensioning_a_living_area_square_accepts_the_first_wall() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let ber = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        let _ = ber;
        // 2m x 2m storey outline, already in metres + fully dimensioned.
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(2.0, 0.0),
            Point2f32::new(2.0, 2.0),
            Point2f32::new(0.0, 2.0),
            Point2f32::new(0.05, 0.05),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.1, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();

        c.set_living_area_mode(true);
        // ~0.9m square, interior (doesn't reach the walls).
        c.apply_living_area_stroke_event(&[
            (0.5, 0.5),
            (1.4, 0.5),
            (1.4, 1.4),
            (0.5, 1.4),
            (0.55, 0.55),
        ]);
        assert_eq!(c.living_area_count(), 1, "living area drawn");
        assert!(
            c.dimensioning_active(),
            "it should be prompting to dimension the new area"
        );
        let r = c.apply_dimension(0.9);
        assert!(r.is_ok(), "first living-area dimension should apply: {r:?}");
    }

    /// The reported case: a living area drawn as a square with a nook, in a
    /// dimensioned (metre-scale) storey. The orthogonalizer's coordinate-merge
    /// is scale-relative, so the nook is PRESERVED (it isn't merged away by a
    /// pixel-scale absolute tolerance), and the resulting shape is a valid
    /// polygon that dimensions cleanly — not the old "stuck" zero-length-wall
    /// case.
    #[test]
    fn living_area_keeps_a_nook_and_dimensions_cleanly() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(2.6, 0.0),
            Point2f32::new(2.6, 2.0),
            Point2f32::new(0.0, 2.0),
            Point2f32::new(0.05, 0.05),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.1, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();

        c.set_living_area_mode(true);
        // ~0.9m square with a nook off the right side, offset into the storey.
        c.apply_living_area_stroke_event(&[
            (1.1, 0.5),
            (2.0, 0.5),
            (2.0, 0.8),
            (2.07, 0.8), // nook out
            (2.07, 0.95),
            (2.0, 0.95),
            (2.0, 1.4),
            (1.1, 1.4),
            (1.12, 0.52),
        ]);
        assert_eq!(c.living_area_count(), 1);
        let poly = c.living_area_polygons()[0].clone();
        assert!(
            poly.len() > 4,
            "the nook is preserved (not flattened by a pixel-scale tolerance): {poly:?}"
        );
        assert!(
            poly.iter().any(|(x, _)| (*x - 2.07).abs() < 0.02),
            "the nook's outer edge survives: {poly:?}"
        );
        assert!(c.dimensioning_active(), "prompts to dimension the area");
        let r = c.apply_dimension(0.93);
        assert!(
            r.is_ok(),
            "the living area dimensions without rejection: {r:?}"
        );
    }

    /// Drawing a living area against the house walls (an OPEN path whose ends
    /// reach the outline) produces a corner room that shares those walls — this
    /// is the path that previously drew nothing (the stroke was force-closed
    /// into a degenerate loop and rejected). Entering living-area mode also arms
    /// the draw tool so the stroke routes to a new area.
    #[test]
    fn living_area_against_a_corner_shares_house_walls_and_renders() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();

        c.set_living_area_mode(true);
        assert_eq!(
            c.edit_mode(),
            EditTools::DrawPerpWalls,
            "entering living-area mode arms the draw tool"
        );
        // Open L against the left + bottom walls: only the 2 NEW interior walls.
        // An edge-to-edge stroke divides the storey, so it offers both regions to
        // pick — tap inside the corner to keep that one.
        c.apply_living_area_stroke_event(&[(0.0, 4.0), (4.0, 4.0), (4.0, 0.0)]);
        assert!(c.cut_pick_pending(), "the dividing L offers a region pick");
        c.choose_cut_region(2.0, 2.0);

        assert_eq!(
            c.living_area_count(),
            1,
            "the corner living area is created"
        );
        let polys = c.living_area_polygons();
        assert_eq!(polys.len(), 1, "and it's renderable (was drawing nothing)");
        assert_eq!(polys[0].len(), 4, "quad: 2 drawn + 2 shared house walls");
        assert!(
            polys[0]
                .iter()
                .any(|(x, y)| x.abs() < 0.05 && y.abs() < 0.05),
            "shares the storey corner (0,0): {:?}",
            polys[0]
        );
    }

    /// A shared-wall (carved) living area must keep its FULL shape when saved —
    /// including the vertices that lie on the house walls (the shared corner and
    /// the snap points) — so it round-trips through the DB intact rather than
    /// losing the house-side boundary.
    #[test]
    fn shared_wall_living_area_keeps_house_vertices_on_save() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let ber = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        c.set_living_area_mode(true);
        // Corner room: only the two interior walls drawn; the left + bottom house
        // walls (and the (0,0) corner) are filled in. The edge-to-edge stroke
        // offers a region pick — tap inside the corner.
        c.apply_living_area_stroke_event(&[(0.0, 4.0), (4.0, 4.0), (4.0, 0.0)]);
        c.choose_cut_region(2.0, 2.0);
        let before = c.living_area_polygons()[0].clone();
        assert_eq!(before.len(), 4, "full closed shape before save: {before:?}");
        // Finalise it so it's persisted (only finalised geometry is written).
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        // Save + reopen from the DB.
        c.open_ber(&ber.id).unwrap();
        let after = c.living_area_polygons()[0].clone();
        assert_eq!(
            after.len(),
            4,
            "the full shape (incl. house-wall vertices) survives save+reopen: {after:?}"
        );
        // The shared corner (0,0) and the two snap points on the house walls
        // are all still present.
        for want in [(0.0, 0.0), (0.0, 4.0), (4.0, 0.0)] {
            assert!(
                after
                    .iter()
                    .any(|(x, y)| (x - want.0).abs() < 0.1 && (y - want.1).abs() < 0.1),
                "house-wall vertex {want:?} retained: {after:?}"
            );
        }
    }

    /// Drawing a single cut line splits the storey into two candidate regions
    /// and waits for the user to tap one — which then becomes the living area.
    /// Tapping the left side keeps the left region; tapping outside cancels.
    #[test]
    fn cut_line_offers_both_regions_then_keeps_the_tapped_one() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        // 10m x 6m storey, in metres + dimensioned.
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                Point2f32::new(0.0, 0.0),
                Point2f32::new(10.0, 0.0),
                Point2f32::new(10.0, 6.0),
                Point2f32::new(0.0, 6.0),
                Point2f32::new(0.1, 0.1),
            ],
            0.2,
            true,
        )
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        c.set_living_area_mode(true);
        // A vertical cut at x~3 (drawn as a noisy near-straight stroke).
        c.apply_living_area_stroke_event(&[(3.0, 0.05), (3.0, 2.0), (3.0, 5.95)]);
        assert!(c.cut_pick_pending(), "the cut is awaiting a pick");
        assert_eq!(c.living_area_count(), 0, "nothing committed until picked");
        assert_eq!(c.pending_cut_regions().len(), 2, "two candidate regions");

        // Tap outside both regions → cancels (no area, no pending).
        c.choose_cut_region(50.0, 50.0);
        assert!(!c.cut_pick_pending());
        assert_eq!(c.living_area_count(), 0);

        // Redraw the cut and tap the LEFT region (x<3) → keeps the left ~3x6=18.
        c.apply_living_area_stroke_event(&[(3.0, 0.05), (3.0, 2.0), (3.0, 5.95)]);
        assert!(c.cut_pick_pending());
        c.choose_cut_region(1.0, 3.0);
        assert!(!c.cut_pick_pending());
        assert_eq!(
            c.living_area_count(),
            1,
            "the tapped region became the area"
        );
        let poly = c.living_area_polygons()[0].clone();
        let area: f64 = {
            let n = poly.len();
            let mut s = 0.0;
            for i in 0..n {
                let (x1, y1) = poly[i];
                let (x2, y2) = poly[(i + 1) % n];
                s += x1 * y2 - x2 * y1;
            }
            (s / 2.0).abs()
        };
        assert!(
            (area - 18.0).abs() < 1.0,
            "kept the left region (~18 m²): {area}"
        );
    }

    /// Walls of a living area that lie on the storey are PINNED (their
    /// parameter pre-known), so dimensioning only asks about the walls the user
    /// drew. A left cut-region shares the storey's left/top/bottom walls; only
    /// the cut wall's position remains to set.
    #[test]
    fn snapped_walls_are_pinned_so_only_drawn_walls_dimension() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                Point2f32::new(0.0, 0.0),
                Point2f32::new(10.0, 0.0),
                Point2f32::new(10.0, 6.0),
                Point2f32::new(0.0, 6.0),
                Point2f32::new(0.1, 0.1),
            ],
            0.2,
            true,
        )
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        c.set_living_area_mode(true);
        c.apply_living_area_stroke_event(&[(3.0, 0.05), (3.0, 2.0), (3.0, 5.95)]);
        c.choose_cut_region(1.0, 3.0); // left region: shares left/top/bottom house walls
        assert_eq!(c.living_area_count(), 1);

        let total = c.floor_plan.all_active_params().len();
        let known = c.dimensioning.known_params.len();
        assert!(
            known >= 1,
            "a shared wall's parameter is pre-known (total={total}, known={known})"
        );
        assert_eq!(
            total - known,
            1,
            "only the cut wall's parameter remains to dimension (total={total}, known={known})"
        );
    }

    /// Helper: a 10×10 dimensioned storey with a BER record, ready for exposure
    /// marking. Returns the controller.
    #[cfg(test)]
    fn dimensioned_square_controller() -> AppController {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                p(0., 0.),
                p(10., 0.),
                p(10., 10.),
                p(0., 10.),
                p(0.05, 0.05),
            ],
            0.1,
            true,
        )
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();
        c
    }

    /// Living areas persist as resolved polygons in `Storey.living_areas` (part
    /// of the `Ber` struct), NOT in `sketch_state`, and are rebuilt from there
    /// on reload.
    #[test]
    fn living_areas_persist_in_ber_and_reload() {
        let mut c = dimensioned_square_controller();
        c.set_living_area_mode(true);
        c.apply_living_area_stroke_event(&[
            (2.0, 2.0),
            (6.0, 2.0),
            (6.0, 6.0),
            (2.0, 6.0),
            (2.1, 2.1),
        ]);
        assert_eq!(c.living_area_count(), 1, "one living area drawn");

        let storey = c.current_record.as_ref().unwrap().data.storeys[0].clone();
        assert_eq!(
            storey.living_areas.regions.len(),
            1,
            "persisted into Storey.living_areas"
        );

        // Reload from the full storey → the living area is rebuilt.
        c.load_storey_into_editor(Some(&storey));
        assert_eq!(c.living_area_count(), 1, "rebuilt on reload");

        // Source is Storey.living_areas, not sketch_state: clear the regions but
        // keep sketch_state → no living areas come back.
        let mut storey_no_la = storey.clone();
        storey_no_la.living_areas = FloorRegions::default();
        c.load_storey_into_editor(Some(&storey_no_la));
        assert_eq!(
            c.living_area_count(),
            0,
            "sketch_state doesn't carry living areas"
        );
    }

    /// Deleting a living area removes it from the editor and the `Ber` struct.
    #[test]
    fn delete_living_area_removes_it() {
        let mut c = dimensioned_square_controller();
        c.set_living_area_mode(true);
        c.apply_living_area_stroke_event(&[
            (2.0, 2.0),
            (6.0, 2.0),
            (6.0, 6.0),
            (2.0, 6.0),
            (2.1, 2.1),
        ]);
        assert_eq!(c.living_area_count(), 1);

        c.delete_living_area(0);
        assert_eq!(c.living_area_count(), 0, "removed from the editor");
        let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
        assert_eq!(
            storey.living_areas.regions.len(),
            0,
            "removed from the Ber struct"
        );
    }

    /// Tapping a wall in exposure mode selects it (whole-wall selection);
    /// assigning a type stores a full-length section AND derives a BER
    /// `WallRegion` (vertical plane, default ceiling height) of that exposure.
    /// Unmarked walls default to `Exposed`.
    #[test]
    fn marking_a_wall_sets_exposure_and_derives_a_ber_wall() {
        let mut c = dimensioned_square_controller();
        let walls = c.outline.floor_plan.perimeter_walls();
        assert!(walls.len() >= 4, "square has 4 perimeter walls");
        let (vid0, a0, b0) = walls[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);

        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        assert_eq!(c.selected_exposure_wall, Some(vid0), "tap selects the wall");
        c.set_wall_exposure(WallExposure::SemiExposed);
        let secs = c.wall_sections.get(&vid0).expect("wall0 has a section");
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].exposure, WallExposure::SemiExposed);

        // Only the MARKED wall derives a region — un-marked walls produce none.
        let storey = &c.current_record.as_ref().unwrap().data.storeys[0];
        assert_eq!(storey.walls.regions.len(), 1, "only the marked wall");
        let _ = walls;
        let w0 = &storey.walls.regions[0];
        assert_eq!(w0.exposure, WallExposure::SemiExposed);
        assert_eq!(w0.polygon.exterior.len(), 4, "vertical plane = 4 vertices");
        assert!(w0.polygon.exterior.iter().any(|q| q.z == 0.0));
        assert!(w0
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - DEFAULT_CEILING_HEIGHT_M).abs() < 1e-9));
    }

    /// A section over PART of a wall derives several regions: the marked range
    /// plus the un-marked remainder as Exposed, sub-segment lengths summing to
    /// the wall length.
    #[test]
    fn mid_wall_section_splits_into_multiple_regions() {
        let mut c = dimensioned_square_controller();
        let (vid0, a0, b0) = c.outline.floor_plan.perimeter_walls()[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
        let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();

        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        assert_eq!(c.selected_exposure_wall, Some(vid0));
        // Mark the middle [0.2L, 0.5L] semi-exposed (typed-range path).
        c.set_exposure_section_range(len * 0.2, len * 0.5);
        c.set_wall_exposure(WallExposure::SemiExposed);
        // The wall is materialized into 3 explicit sections: Exposed, Semi,
        // Exposed (the leftovers become their own sections).
        let secs = c.wall_sections.get(&vid0).expect("wall0 has sections");
        assert_eq!(secs.len(), 3);
        assert_eq!(secs[0].exposure, WallExposure::Exposed);
        assert_eq!(secs[1].exposure, WallExposure::SemiExposed);
        assert_eq!(secs[2].exposure, WallExposure::Exposed);

        // wall0 is derived first → its 3 sub-regions are regions[0..3].
        let r = &c.current_record.as_ref().unwrap().data.storeys[0]
            .walls
            .regions;
        assert!(r.len() >= 3);
        assert_eq!(r[0].exposure, WallExposure::Exposed);
        assert_eq!(r[1].exposure, WallExposure::SemiExposed);
        assert_eq!(r[2].exposure, WallExposure::Exposed);
        let base = |reg: &datatypes::ber::structural::WallRegion| {
            let e = &reg.polygon.exterior;
            ((e[1].x - e[0].x).powi(2) + (e[1].y - e[0].y).powi(2)).sqrt()
        };
        let l = len as f64;
        assert!((base(&r[0]) - l * 0.2).abs() < 0.02, "{}", base(&r[0]));
        assert!((base(&r[1]) - l * 0.3).abs() < 0.02, "{}", base(&r[1]));
        assert!((base(&r[2]) - l * 0.5).abs() < 0.02, "{}", base(&r[2]));
    }

    /// Splitting a wall preserves its fabric properties: each piece inherits the
    /// construction of the region it was carved from; only exposure changes.
    #[test]
    fn wall_construction_is_preserved_when_splitting() {
        use datatypes::ber::structural::WallConstructionType;
        let mut c = dimensioned_square_controller();
        let (_, a0, b0) = c.outline.floor_plan.perimeter_walls()[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
        let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();

        c.set_exposure_mode(true);
        // Mark wall0 whole so it derives to a region we can attach a construction to.
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        c.set_wall_exposure(WallExposure::Exposed);
        // Point wall0's region at a known (non-default) shared construction, and
        // give that construction a description.
        let cid = {
            let record = c.current_record.as_mut().unwrap();
            let cid = record.data.ensure_wall_construction(
                WallConstructionType::CavityWall,
                Default::default(),
                Default::default(),
            );
            record
                .data
                .wall_constructions
                .iter_mut()
                .find(|wc| wc.base.id == cid)
                .unwrap()
                .base
                .description = Some("party".into());
            record.data.storeys[0].walls.regions[0].construction_id = cid;
            cid
        };

        // Split wall0: mark the middle semi-exposed.
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        c.set_exposure_section_range(len * 0.2, len * 0.5);
        c.set_wall_exposure(WallExposure::SemiExposed);

        // wall0 is now 3 regions (derived first) — all inherit the SAME shared
        // construction id; only exposure differs.
        let record = c.current_record.as_ref().unwrap();
        let r = &record.data.storeys[0].walls.regions;
        for i in 0..3 {
            assert_eq!(r[i].construction_id, cid, "piece {i} keeps construction");
        }
        // The shared construction still carries its type + description.
        let wc = record
            .data
            .wall_constructions
            .iter()
            .find(|wc| wc.base.id == cid)
            .unwrap();
        assert_eq!(wc.construction_type, WallConstructionType::CavityWall);
        assert_eq!(wc.base.description.as_deref(), Some("party"));
        assert_eq!(r[1].exposure, WallExposure::SemiExposed, "middle is semi");
    }

    /// A wall's resolved construction phase, via its region's `construction_id`.
    fn wall_phase(c: &AppController, region_idx: usize) -> ConstructionPhase {
        let rec = c.current_record.as_ref().unwrap();
        let cid = rec.data.storeys[0].walls.regions[region_idx].construction_id;
        rec.data
            .wall_construction(cid)
            .unwrap()
            .base
            .construction_phase
            .clone()
    }

    /// Construction phases entered up front are stored on the BER and surface as
    /// labelled tagging choices.
    #[test]
    fn set_construction_phases_populates_choices() {
        use datatypes::ber::common::ConstructionPhases;
        let mut c = dimensioned_square_controller();
        let mut phases = ConstructionPhases::default();
        phases.set_year(ConstructionPhase::Original, 1985);
        phases.set_year(ConstructionPhase::Extension1, 2008);
        c.set_construction_phases(phases);

        let choices = c.construction_phase_choices();
        assert_eq!(choices.len(), 2, "two phases offered");
        assert_eq!(choices[0].0, ConstructionPhase::Original);
        assert!(choices[0].1.contains("1985"), "label carries the year");
        assert_eq!(choices[1].0, ConstructionPhase::Extension1);
        assert!(choices[1].1.contains("2008"));
    }

    /// A phase polygon stamps its phase onto walls whose midpoint falls inside it;
    /// walls outside stay `Original`.
    #[test]
    fn phase_polygon_stamps_contained_walls() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = dimensioned_square_controller();
        c.set_all_remaining_exposed(); // all 4 perimeter walls → regions
                                       // Tag the right side (x >= 6) as Extension1.
        c.phase_regions.entry(0).or_default().push(PhaseRegion {
            polygon: vec![p(6., -1.), p(12., -1.), p(12., 12.), p(6., 12.)],
            phase: ConstructionPhase::Extension1,
        });
        c.commit_to_ber().unwrap();

        let rec = c.current_record.as_ref().unwrap();
        let walls = rec.data.storeys[0].walls.regions.clone();
        assert!(!walls.is_empty(), "walls became regions");
        for (i, w) in walls.iter().enumerate() {
            let mid = w.base_midpoint().unwrap();
            let phase = wall_phase(&c, i);
            if mid.x >= 6.0 {
                assert_eq!(
                    phase,
                    ConstructionPhase::Extension1,
                    "wall {i} (right) tagged"
                );
            } else {
                assert_eq!(
                    phase,
                    ConstructionPhase::Original,
                    "wall {i} (other) original"
                );
            }
        }
    }

    /// A floor phase area splits the storey floor into multiple regions, each
    /// carrying its own phase (tagged zone + remainder).
    #[test]
    fn phase_area_splits_floor_into_regions() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = dimensioned_square_controller();
        c.phase_regions.entry(0).or_default().push(PhaseRegion {
            polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
            phase: ConstructionPhase::Extension1,
        });
        c.commit_to_ber().unwrap();
        let rec = c.current_record.as_ref().unwrap();
        let floors = &rec.data.storeys[0].floors.regions;
        assert_eq!(floors.len(), 2, "floor split into tagged zone + remainder");
        let phases: Vec<ConstructionPhase> = floors
            .iter()
            .map(|f| {
                rec.data
                    .floor_construction(f.construction_id)
                    .unwrap()
                    .base
                    .construction_phase
                    .clone()
            })
            .collect();
        assert!(phases.contains(&ConstructionPhase::Extension1));
        assert!(phases.contains(&ConstructionPhase::Original));
    }

    /// The floor split survives a DB round-trip: reopening rebuilds the split
    /// regions, reconstructs the session phase areas, and the editor outline is
    /// re-merged from the zones (one clean footprint, not a single zone).
    #[test]
    fn phase_split_survives_reload() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = dimensioned_square_controller();
        c.phase_regions.entry(0).or_default().push(PhaseRegion {
            polygon: vec![p(5., 0.), p(10., 0.), p(10., 10.), p(5., 10.)],
            phase: ConstructionPhase::Extension1,
        });
        c.commit_to_ber().unwrap();
        let id = c.current_record.as_ref().unwrap().id;

        c.open_ber(&id).unwrap();
        let floors = &c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .regions;
        assert_eq!(floors.len(), 2, "split persisted + reloaded");
        assert!(
            c.phase_regions.get(&0).is_some_and(|v| !v.is_empty()),
            "phase areas reconstructed from floor regions"
        );
        // Outline merged back to the full 10x10 footprint (~100 m²), cleaned to
        // 4 corners (no phantom vertex at the dissolved seam) — not a single zone.
        let pts = c.outline.floor_plan.get_points();
        assert_eq!(pts.len(), 4, "merged outline is a clean rectangle");
        let n = pts.len();
        let mut a2 = 0.0f32;
        for i in 0..n {
            let (p, q) = (pts[i], pts[(i + 1) % n]);
            a2 += p.x * q.y - q.x * p.y;
        }
        let area = (a2.abs() / 2.0) as f64;
        assert!(
            (area - 100.0).abs() < 1.0,
            "merged outline ~100 m², got {area}"
        );
    }

    /// REGRESSION: an UNTAGGED storey's outline must survive commit→reload
    /// unchanged — no `geo` re-normalisation. When the partition + merged-outline
    /// ran unconditionally (even with no phase zones), the rebuilt ring was
    /// distorted, which inflated the plan extent and made the collinearity review
    /// flag far-apart (opposite-side) walls. An L-shape exercises the concave
    /// case geo is most likely to mangle.
    #[test]
    fn untagged_outline_survives_reload_without_distortion() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                p(0., 0.),
                p(10., 0.),
                p(10., 6.),
                p(4., 6.),
                p(4., 10.),
                p(0., 10.),
                p(0.05, 0.05),
            ],
            0.1,
            true,
        )
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();
        let before = c.outline.floor_plan.get_points();
        // Untagged → exactly one floor region (no split).
        assert_eq!(
            c.current_record.as_ref().unwrap().data.storeys[0]
                .floors
                .regions
                .len(),
            1,
            "untagged storey stays a single floor region"
        );

        let id = c.current_record.as_ref().unwrap().id;
        c.open_ber(&id).unwrap();
        let after = c.outline.floor_plan.get_points();

        assert_eq!(
            before.len(),
            after.len(),
            "vertex count preserved across reload (no geo-introduced/dropped corners): \
             before {before:?} after {after:?}"
        );
        for b in &before {
            assert!(
                after
                    .iter()
                    .any(|a| (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3),
                "vertex {b:?} missing/moved after reload; got {after:?}"
            );
        }
    }

    /// A later phase polygon overwrites an existing non-`Original` phase under it
    /// (polygon always wins; latest tag first).
    #[test]
    fn polygon_overwrites_existing_phase() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let right = vec![p(6., -1.), p(12., -1.), p(12., 12.), p(6., 12.)];
        let mut c = dimensioned_square_controller();
        c.set_all_remaining_exposed();
        let regions = c.phase_regions.entry(0).or_default();
        regions.push(PhaseRegion {
            polygon: right.clone(),
            phase: ConstructionPhase::Extension2,
        });
        c.commit_to_ber().unwrap();
        // Now tag the SAME area Extension1 — must override the Extension2.
        c.phase_regions.entry(0).or_default().push(PhaseRegion {
            polygon: right,
            phase: ConstructionPhase::Extension1,
        });
        c.commit_to_ber().unwrap();

        let walls = c.current_record.as_ref().unwrap().data.storeys[0]
            .walls
            .regions
            .clone();
        let right_idx = walls
            .iter()
            .position(|w| w.base_midpoint().unwrap().x >= 6.0)
            .unwrap();
        assert_eq!(wall_phase(&c, right_idx), ConstructionPhase::Extension1);
    }

    /// Once a wall is tagged, removing the polygon does NOT revert it — a surface
    /// keeps its non-`Original` phase when no polygon covers it (the durable
    /// record is `construction_id`). This is the reload-safety guarantee.
    #[test]
    fn preserve_existing_phase_when_no_polygon() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = dimensioned_square_controller();
        c.set_all_remaining_exposed();
        c.phase_regions.entry(0).or_default().push(PhaseRegion {
            polygon: vec![p(6., -1.), p(12., -1.), p(12., 12.), p(6., 12.)],
            phase: ConstructionPhase::Extension1,
        });
        c.commit_to_ber().unwrap();
        let walls = c.current_record.as_ref().unwrap().data.storeys[0]
            .walls
            .regions
            .clone();
        let right_idx = walls
            .iter()
            .position(|w| w.base_midpoint().unwrap().x >= 6.0)
            .unwrap();
        let left_idx = walls
            .iter()
            .position(|w| w.base_midpoint().unwrap().x < 6.0)
            .unwrap();
        assert_eq!(wall_phase(&c, right_idx), ConstructionPhase::Extension1);

        // Drop all polygons (as a reload/new session would) and re-commit.
        c.phase_regions.clear();
        c.commit_to_ber().unwrap();
        assert_eq!(
            wall_phase(&c, right_idx),
            ConstructionPhase::Extension1,
            "tagged wall keeps its phase with no polygon"
        );
        assert_eq!(
            wall_phase(&c, left_idx),
            ConstructionPhase::Original,
            "untagged wall stays original"
        );
    }

    /// After splitting a wall, tapping a leftover piece selects just that piece
    /// (the coverage range under the tap), not the whole wall.
    #[test]
    fn tapping_a_split_wall_selects_the_piece_under_the_tap() {
        let mut c = dimensioned_square_controller();
        let (_, a0, b0) = c.outline.floor_plan.perimeter_walls()[0];
        let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
        let along = |d: f32| {
            let t = d / len;
            (a0.x + t * (b0.x - a0.x), a0.y + t * (b0.y - a0.y))
        };

        c.set_exposure_mode(true);
        // Select the whole wall, mark the middle [0.4L, 0.6L] semi-exposed.
        let mid = along(len * 0.5);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        c.set_exposure_section_range(len * 0.4, len * 0.6);
        c.set_wall_exposure(WallExposure::SemiExposed);

        // Tap the LEFT leftover piece → selects (0, 0.4L), not the whole wall.
        let left = along(len * 0.1);
        c.exposure_pointer_down(left.0 as f64, left.1 as f64, 0.5);
        let sel = c.exposure_selection().expect("a piece is selected");
        assert!(
            sel.start_m.abs() < 0.05,
            "left piece starts at 0: {}",
            sel.start_m
        );
        assert!(
            (sel.end_m - len * 0.4).abs() < 0.05,
            "left piece ends at the section start: {}",
            sel.end_m
        );

        // Tap the semi piece → selects (0.4L, 0.6L).
        let centre = along(len * 0.5);
        c.exposure_pointer_down(centre.0 as f64, centre.1 as f64, 0.5);
        let sel2 = c.exposure_selection().unwrap();
        assert!((sel2.start_m - len * 0.4).abs() < 0.05);
        assert!((sel2.end_m - len * 0.6).abs() < 0.05);
    }

    /// Dragging a selection handle moves that end of the section selection.
    #[test]
    fn drag_updates_the_section_selection() {
        let mut c = dimensioned_square_controller();
        let (_, a0, b0) = c.outline.floor_plan.perimeter_walls()[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
        let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();

        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5); // select, sel = (0, len)
        let sel0 = c.exposure_selection().expect("a wall is selected");
        assert!(
            (sel0.end_m - len).abs() < 0.01,
            "end starts at the far corner"
        );
        // Grab the END handle (at the far corner) and drag it to the midpoint.
        c.exposure_pointer_down(b0.x as f64, b0.y as f64, 0.5);
        c.exposure_pointer_drag(mid.0 as f64, mid.1 as f64);
        let sel1 = c.exposure_selection().unwrap();
        assert!(
            (sel1.end_m - len * 0.5).abs() < 0.1,
            "dragged end to mid: {}",
            sel1.end_m
        );
        c.exposure_pointer_up();
    }

    /// Exposure is reconstructed on reload from the persisted `WallRegions`
    /// (the single source of truth) — NOT duplicated into `sketch_state`. A
    /// marked wall comes back; un-marked walls stay unset.
    #[test]
    fn exposure_reconstructs_from_wall_regions_on_reload() {
        let mut c = dimensioned_square_controller();
        let (_, a0, b0) = c.outline.floor_plan.perimeter_walls()[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        c.set_wall_exposure(WallExposure::NonHeatLoss); // mark only one wall

        let storey = c.current_record.as_ref().unwrap().data.storeys[0].clone();
        c.wall_sections.clear();
        c.load_storey_into_editor(Some(&storey));
        // Vertex ids are reassigned on reload (rebuilt from points), so check by
        // count: exactly one wall reconstructed, the rest unset.
        assert_eq!(c.wall_sections.len(), 1, "one wall marked; rest unset");
        let secs = c.wall_sections.values().next().unwrap();
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].exposure, WallExposure::NonHeatLoss);
    }

    /// A split wall's sections reconstruct exactly from its regions on reload.
    #[test]
    fn split_wall_sections_reconstruct_on_reload() {
        let mut c = dimensioned_square_controller();
        let (_, a0, b0) = c.outline.floor_plan.perimeter_walls()[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
        let len = ((b0.x - a0.x).powi(2) + (b0.y - a0.y).powi(2)).sqrt();
        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        c.set_exposure_section_range(len * 0.2, len * 0.5);
        c.set_wall_exposure(WallExposure::SemiExposed); // → 3 sections

        let storey = c.current_record.as_ref().unwrap().data.storeys[0].clone();
        c.wall_sections.clear();
        c.load_storey_into_editor(Some(&storey));
        assert_eq!(c.wall_sections.len(), 1, "one split wall reconstructed");
        let secs = c.wall_sections.values().next().unwrap();
        assert_eq!(secs.len(), 3);
        assert_eq!(secs[0].exposure, WallExposure::Exposed);
        assert_eq!(secs[1].exposure, WallExposure::SemiExposed);
        assert_eq!(secs[2].exposure, WallExposure::Exposed);
        assert!(
            (secs[1].start_m - len * 0.2).abs() < 0.02,
            "{}",
            secs[1].start_m
        );
        assert!(
            (secs[1].end_m - len * 0.5).abs() < 0.02,
            "{}",
            secs[1].end_m
        );
    }

    /// "Set remaining → Exposed" fills only walls with no sections yet, leaving
    /// already-marked walls (semi/non) untouched.
    #[test]
    fn set_all_remaining_fills_only_unset_walls() {
        let mut c = dimensioned_square_controller();
        let walls = c.outline.floor_plan.perimeter_walls();
        let (vid0, a0, b0) = walls[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        c.set_wall_exposure(WallExposure::SemiExposed);

        c.set_all_remaining_exposed();
        assert_eq!(c.wall_sections.len(), walls.len(), "every wall now set");
        assert_eq!(
            c.wall_sections.get(&vid0).map(|s| s[0].exposure),
            Some(WallExposure::SemiExposed),
            "the marked wall keeps its type"
        );
        for (vid, _, _) in &walls {
            if *vid != vid0 {
                assert_eq!(
                    c.wall_sections.get(vid).map(|s| s[0].exposure),
                    Some(WallExposure::Exposed)
                );
            }
        }
    }

    /// Assigning an exposure is undoable.
    #[test]
    fn undo_reverts_an_exposure_marking() {
        let mut c = dimensioned_square_controller();
        let (_, a0, b0) = c.outline.floor_plan.perimeter_walls()[0];
        let mid = ((a0.x + b0.x) * 0.5, (a0.y + b0.y) * 0.5);
        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        assert!(c.wall_sections.is_empty());
        c.set_wall_exposure(WallExposure::NonHeatLoss);
        assert_eq!(c.wall_sections.len(), 1);
        c.undo().unwrap();
        assert!(c.wall_sections.is_empty(), "undo clears the marking");
    }

    /// Guardrail: a dimension that would push a living area OUTSIDE the storey
    /// is rejected (and rolled back), like an invalid-shape dimension.
    #[test]
    fn dimension_pushing_living_area_outside_is_rejected() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        // 10m x 10m storey, dimensioned.
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                Point2f32::new(0.0, 0.0),
                Point2f32::new(10.0, 0.0),
                Point2f32::new(10.0, 10.0),
                Point2f32::new(0.0, 10.0),
                Point2f32::new(0.05, 0.05),
            ],
            0.1,
            true,
        )
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        c.set_living_area_mode(true);
        // ~2m room near the right edge (x in [6,8]).
        c.apply_living_area_stroke_event(&[
            (6.0, 3.0),
            (8.0, 3.0),
            (8.0, 5.0),
            (6.0, 5.0),
            (6.1, 3.1),
        ]);
        assert_eq!(c.living_area_count(), 1);
        assert!(c.dimensioning_active());
        let before = c.floor_plan.polygons()[0].clone();
        // 9m wall on a room that starts at x=6 / y=3 would reach past the 10m
        // storey — must be rejected, and the shape left unchanged.
        let r = c.apply_dimension(9.0);
        assert!(
            r.is_err(),
            "a dimension pushing the room outside the storey is rejected: {r:?}"
        );
        assert_eq!(
            c.floor_plan.polygons()[0],
            before,
            "the rejected dimension is rolled back"
        );
    }

    /// A carved living area must be — and STAY — orthogonal: every edge
    /// horizontal or vertical, right after carving and through dimensioning.
    /// (A slanted edge here is the "left line isn't straight" report.)
    #[test]
    fn carved_living_area_stays_orthogonal_through_dimensioning() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();

        c.set_living_area_mode(true);
        c.apply_living_area_stroke_event(&[(0.0, 4.0), (4.0, 4.0), (4.0, 0.0)]);
        c.choose_cut_region(2.0, 2.0); // edge-to-edge stroke → pick the corner region
        assert_eq!(c.living_area_count(), 1);
        assert!(
            c.floor_plan.non_orthogonal_edges().is_empty(),
            "carved living area orthogonal right after carve: {:?}",
            c.floor_plan.non_orthogonal_edges()
        );

        let mut guard = 0;
        while c.dimensioning_active() && guard < 20 {
            guard += 1;
            let _ = c.apply_dimension(2.5);
            assert!(
                c.floor_plan.non_orthogonal_edges().is_empty(),
                "still orthogonal after dimension #{guard}: {:?}",
                c.floor_plan.non_orthogonal_edges()
            );
        }
    }

    /// The curve tools defer: drawing the arc only splits the wall (no shape
    /// change); the user then dimensions the two attach stubs through the normal
    /// prompt flow — only axis-aligned stubs are ever offered, never a facet —
    /// and ONLY THEN is the circular arc faceted into the plan.
    #[test]
    fn curve_arc_is_provisional_until_attach_lines_are_dimensioned() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let square = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
            Point2f32::new(0.2, 0.2),
        ];
        c.floor_plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        let area0 = ring_area(&c);

        c.set_edit_mode(EditTools::ExtendCurve);
        // An arc bulging out of the top edge.
        let arc = vec![
            Point2f32::new(3.0, 10.0),
            Point2f32::new(3.6, 11.0),
            Point2f32::new(4.3, 11.7),
            Point2f32::new(5.0, 12.0),
            Point2f32::new(5.7, 11.7),
            Point2f32::new(6.4, 11.0),
            Point2f32::new(7.0, 10.0),
        ];
        c.apply_extend_stroke(&arc).expect("begin provisional arc");

        // Provisional: arc held, shape UNCHANGED (no facets, area the same), but
        // a preview is available and a dimension prompt is waiting.
        assert!(c.pending_arc.is_some(), "arc should be held provisional");
        assert_eq!(diag_edges(&c), 0, "no facets while provisional");
        assert!(
            (ring_area(&c) - area0).abs() < 0.5,
            "begin must not change the shape"
        );
        assert!(c.pending_arc_preview().is_some(), "preview available");
        assert!(
            c.dimensioning_active(),
            "the attach stubs should be prompted"
        );

        // Dimension every prompted wall: each must be an axis-aligned stub, never
        // a facet. Two stubs get set; the arc stays provisional (not auto-baked).
        let mut dimensioned = 0;
        for _ in 0..10 {
            let Some(prompt) = c.current_dimension_prompt() else {
                break;
            };
            let (ring, edge) = c.floor_plan.find_vertex(prompt.wall_start_vertex).unwrap();
            assert!(
                c.floor_plan.wall_axis(ring, edge).is_some(),
                "a curve facet must never be offered for dimensioning"
            );
            c.apply_dimension(2.0).expect("dimension an attach stub");
            dimensioned += 1;
        }
        assert_eq!(
            dimensioned, 2,
            "exactly the two attach stubs are dimensioned"
        );

        // Attach lines set, but the curve is NOT committed yet — it waits for
        // the radius to be accepted. The radius slider is now available, and
        // it's still a straight provisional placeholder (no facets).
        assert!(c.pending_arc.is_some(), "arc not auto-committed");
        assert_eq!(diag_edges(&c), 0, "still a straight placeholder pre-commit");
        let bulge0 = c
            .pending_arc_bulge()
            .expect("radius slider is available once attach lines are set");

        // Adjust the radius (slider), then accept.
        c.set_pending_arc_bulge(bulge0 + 0.3);
        assert!((c.pending_arc_bulge().unwrap() - (bulge0 + 0.3)).abs() < 1e-4);
        c.commit_pending_arc().expect("commit the curve");

        // Now baked: facets present, area grew, valid simple polygon.
        assert!(
            c.pending_arc.is_none(),
            "arc committed after accepting radius"
        );
        assert!(c.floor_plan.validate_simple().is_ok());
        assert!(diag_edges(&c) >= 2, "arc faceted after commit");
        assert!(
            ring_area(&c) > area0 + 1.0,
            "outward arc adds area once baked"
        );

        // The baked arc's facet params must be LOCKED (known). Otherwise a later
        // dimension elsewhere would treat them as free and `repair` /
        // `aesthetic_rescale` could redistribute the facet vertices — flattening
        // the curve back into a straight line (the reported extend-curve bug).
        let active = c.floor_plan.all_active_params();
        assert!(
            active
                .iter()
                .all(|p| c.dimensioning.known_params.contains(p)),
            "every facet param of a baked arc must be locked into known_params"
        );

        // Concretely: a subsequent aesthetic rescale (which only ever touches
        // un-set params) must leave the curved shape untouched.
        let area_before_rescale = ring_area(&c);
        let diag_before = diag_edges(&c);
        c.floor_plan.aesthetic_rescale(&c.dimensioning.known_params);
        assert_eq!(diag_edges(&c), diag_before, "rescale must not drop facets");
        assert!(
            (ring_area(&c) - area_before_rescale).abs() < 1e-3,
            "a locked arc must survive aesthetic rescale unchanged"
        );
    }

    // ----- multi-storey navigation -----------------------------------------

    /// Drop a dimensioned 10x10 square outline into whichever storey is current.
    /// Used by the multi-storey tests to set each storey up identically.
    fn finalise_current_storey_as_square(c: &mut AppController, side: f32) {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        c.floor_plan = FloorPlan::from_points_to_orthogonal(
            &[
                p(0., 0.),
                p(side, 0.),
                p(side, side),
                p(0., side),
                p(0.05, 0.05),
            ],
            0.1,
            true,
        )
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();
    }

    /// `add_storey` appends a fresh empty storey and moves the editor onto it.
    #[test]
    fn add_storey_appends_and_switches() {
        let mut c = dimensioned_square_controller();
        assert_eq!(c.current_storey_idx(), Some(0));
        assert_eq!(c.current_record.as_ref().unwrap().data.storeys.len(), 1);

        c.add_storey().expect("add_storey");
        assert_eq!(c.current_storey_idx(), Some(1), "switched onto new storey");
        assert_eq!(
            c.current_record.as_ref().unwrap().data.storeys.len(),
            2,
            "new storey appended"
        );
        // The new storey is empty (no outline, no living areas, no walls); the
        // editor scale resets so the first dimension calibrates afresh.
        assert_eq!(c.floor_plan.ring_count(), 0, "editor on a blank storey");
        assert!(
            !c.dimensioning.scale_set,
            "scale resets so the new storey calibrates fresh"
        );
        assert_eq!(c.living_area_count(), 0);
        assert!(c.wall_sections.is_empty());
    }

    /// Switching storeys round-trips each storey's geometry through the editor:
    /// dimensioning the first, adding & dimensioning the second, then switching
    /// back leaves both intact in `current_record.data.storeys`.
    #[test]
    fn switching_storeys_preserves_each_storeys_geometry() {
        let mut c = dimensioned_square_controller(); // storey 0 = 10x10
        let storey0_polys = c.current_record.as_ref().unwrap().data.storeys[0]
            .floors
            .to_vertex_lists();
        assert!(!storey0_polys.is_empty(), "storey 0 has a polygon");

        c.add_storey().unwrap();
        finalise_current_storey_as_square(&mut c, 6.0); // storey 1 = 6x6
        let storeys_after = c.current_record.as_ref().unwrap().data.storeys.clone();
        assert_eq!(storeys_after.len(), 2);
        let storey1_polys = storeys_after[1].floors.to_vertex_lists();
        assert!(!storey1_polys.is_empty(), "storey 1 has a polygon");

        // Switch back to storey 0 — its previously-committed geometry survives.
        c.set_current_storey(Some(0)).unwrap();
        assert_eq!(c.current_storey_idx(), Some(0));
        let storeys_back = c.current_record.as_ref().unwrap().data.storeys.clone();
        assert_eq!(storeys_back.len(), 2, "no storey lost on switch");
        assert_eq!(
            storeys_back[0].floors.to_vertex_lists(),
            storey0_polys,
            "storey 0's footprint preserved"
        );
        assert_eq!(
            storeys_back[1].floors.to_vertex_lists(),
            storey1_polys,
            "storey 1's footprint preserved"
        );
        // The editor now mirrors storey 0 again.
        assert_eq!(c.floor_plan.ring_count(), 1);
        assert!(c.dimensioning.scale_set);
    }

    /// All-storeys tab: `set_current_storey(None)` leaves the editor in a
    /// read-only state — the live floor_plan is empty (no shape to edit on)
    /// and the storeys list is preserved.
    #[test]
    fn switching_to_all_storeys_clears_editor_and_keeps_data() {
        let mut c = dimensioned_square_controller();
        c.add_storey().unwrap();
        finalise_current_storey_as_square(&mut c, 6.0);
        let snapshot = c.current_record.as_ref().unwrap().data.storeys.clone();

        c.set_current_storey(None).unwrap();
        assert_eq!(c.current_storey_idx(), None);
        assert_eq!(c.floor_plan.ring_count(), 0, "no live shape on All tab");
        assert_eq!(
            c.current_record.as_ref().unwrap().data.storeys.len(),
            snapshot.len(),
            "every storey still present"
        );
        // Returning to a specific storey rehydrates the editor from its geometry.
        c.set_current_storey(Some(1)).unwrap();
        assert_eq!(c.floor_plan.ring_count(), 1);
    }

    /// Method-2 snap: on the FIRST stroke of a new upper storey, freehand points
    /// drawn near the previous storey's outline get pulled onto it. We check
    /// the helper directly (the stroke→pixel canvas conversion in
    /// `apply_drawperp_stroke` is wrapped around a 10-unit RDP simplification
    /// tolerance — large-scale coords keep the corners alive through that).
    #[test]
    fn first_stroke_on_new_storey_snaps_to_previous_outline() {
        let mut c = dimensioned_square_controller(); // storey 0 = 10x10
        c.add_storey().unwrap();
        // Endpoints near (but not on) the previous outline's corners. The
        // snap tolerance scales to the previous diagonal (~6%), so 10x10 →
        // tol ≈ 0.85; any point within that lands on the outline exactly.
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let stroke = vec![
            p(0.2, 0.2), // near (0, 0) — snaps to (0, 0)
            p(9.8, 0.2), // near (10, 0) — snaps to (10, 0)
            p(9.8, 5.0), // near right wall at y=5 — snaps to x=10
            p(9.8, 9.8), // near (10, 10) — snaps to (10, 10)
            p(0.2, 9.8), // near (0, 10) — snaps to (0, 10)
            p(0.3, 0.3), // near (0, 0)
        ];
        let snapped = c
            .snap_to_previous_storey_outline(&stroke)
            .expect("snap helper engaged");
        // Endpoints aligned to the previous outline's corners exactly.
        let approx = |a: f32, b: f32| (a - b).abs() < 1e-3;
        assert!(approx(snapped[1].x, 10.0), "{:?}", snapped[1]);
        assert!(approx(snapped[1].y, 0.0));
        assert!(approx(snapped[3].x, 10.0), "{:?}", snapped[3]);
        assert!(approx(snapped[3].y, 10.0));
        // Mid-wall point pulled onto x=10, leaving y untouched.
        assert!(approx(snapped[2].x, 10.0), "{:?}", snapped[2]);
        assert!(approx(snapped[2].y, 5.0));
    }

    /// Without a previous storey to snap to, the helper returns `None`. Storey 0
    /// has no watermark.
    #[test]
    fn first_stroke_on_storey_0_does_not_snap() {
        let c = dimensioned_square_controller(); // storey 0 only
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let stroke = vec![p(0.2, 0.2), p(9.7, 0.2)];
        // On storey 0 there's no previous outline → helper returns None.
        assert!(c.snap_to_previous_storey_outline(&stroke).is_none());
    }

    /// A new upper storey drawn (in world coords) tracing the SAME footprint as
    /// the storey below inherits scale and pins every shared wall — so it's
    /// fully resolved with no dimension prompt.
    #[test]
    fn outline_world_inherits_scale_and_pins_shared_walls() {
        let mut c = dimensioned_square_controller(); // storey 0 = 10x10 (metres)
        c.add_storey().unwrap();
        // Trace the 10x10 freehand-style (a little off each corner) so the snap
        // pulls the endpoints onto the previous outline.
        let world = vec![(0.2, 0.2), (9.8, 0.2), (9.8, 9.8), (0.2, 9.8), (0.3, 0.3)];
        c.apply_outline_world_inner(&world, true)
            .expect("apply world outline");

        assert_eq!(c.floor_plan.ring_count(), 1, "outline built");
        assert!(
            c.dimensioning.scale_set,
            "geometry is already metres → scale inherited"
        );
        assert!(
            !c.dimensioning.known_params.is_empty(),
            "shared walls pinned"
        );
        // Footprint ≈ the 10x10 below.
        let area = ring_area(&c);
        assert!(
            (area - 100.0).abs() < 2.0,
            "footprint ≈ 100 m² (got {area})"
        );
        // Every wall coincides with the storey below → nothing left to ask.
        assert!(
            c.dimensioning.current_prompt.is_none(),
            "all walls shared → no dimension prompt"
        );
    }

    /// A storey ~5 m across with a curved LEFT wall and a bulged RIGHT side.
    /// Used to check the new-storey reproduce/splice decision at a realistic
    /// small scale (where the snap radius is a large fraction of the plan).
    fn bulged_storey_controller() -> AppController {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        c.floor_plan = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(3., 0.),
            p(3.5, 1.5), // right side bulges OUT to x=3.5
            p(3., 3.),
            p(0., 3.),
            p(-0.2, 2.2), // left side is a 3-facet curve bulging out
            p(-0.3, 1.5),
            p(-0.2, 0.8),
        ])
        .unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();
        c
    }

    /// A second storey that runs a STRAIGHT wall where the storey below bulges
    /// out (a ~0.5 m setback) must NOT be mistaken for a faithful reproduction —
    /// even though the setback is small relative to the snap radius. The curve on
    /// the OTHER side, which the trace did follow, is preserved (spliced
    /// verbatim, not squared). A genuinely faithful trace still reproduces.
    #[test]
    fn second_storey_setback_over_a_bulge_is_not_a_reproduce() {
        let sample = |a: (f32, f32), b: (f32, f32), out: &mut Vec<(f32, f32)>| {
            let steps = 30;
            for s in 0..steps {
                let f = s as f32 / steps as f32;
                out.push((a.0 + f * (b.0 - a.0), a.1 + f * (b.1 - a.1)));
            }
        };
        // Faithful trace: follow every wall, bulge and curve included.
        let mut faithful = Vec::new();
        for (a, b) in [
            ((0., 0.), (3., 0.)),
            ((3., 0.), (3.5, 1.5)),
            ((3.5, 1.5), (3., 3.)),
            ((3., 3.), (0., 3.)),
            ((0., 3.), (-0.2, 2.2)),
            ((-0.2, 2.2), (-0.3, 1.5)),
            ((-0.3, 1.5), (-0.2, 0.8)),
            ((-0.2, 0.8), (0., 0.)),
        ] {
            sample(a, b, &mut faithful);
        }
        faithful.push((0., 0.));

        let mut cf = bulged_storey_controller();
        let area0 = ring_area(&cf);
        cf.add_storey().unwrap();
        cf.apply_outline_world_inner(&faithful, true).unwrap();
        assert!(
            (ring_area(&cf) - area0).abs() < 0.2,
            "a faithful trace reproduces the storey below verbatim"
        );

        // Setback trace: identical EXCEPT the right side is a straight wall at
        // x=2.5 — a MID-span setback (not at the storey-below's corners), cutting
        // off the bulge.
        let mut setback = Vec::new();
        for (a, b) in [
            ((0., 0.), (2.5, 0.)),
            ((2.5, 0.), (2.5, 3.)), // straight, set back from the bulge
            ((2.5, 3.), (0., 3.)),
            ((0., 3.), (-0.2, 2.2)),
            ((-0.2, 2.2), (-0.3, 1.5)),
            ((-0.3, 1.5), (-0.2, 0.8)),
            ((-0.2, 0.8), (0., 0.)),
        ] {
            sample(a, b, &mut setback);
        }
        setback.push((0., 0.));

        let mut cs = bulged_storey_controller();
        cs.add_storey().unwrap();
        cs.apply_outline_world_inner(&setback, true).unwrap();
        // The mid-span setback wall is NEW — its position must be queried, not
        // accepted as drawn (the parametric origin must land on a shared corner,
        // not the new wall, or its position freezes as a constant).
        assert!(
            cs.dimensioning.current_prompt.is_some(),
            "the new set-back wall's position is offered for dimensioning"
        );
        let pts = cs.floor_plan.get_points();
        // The bulge apex is gone — the right wall was straightened.
        assert!(
            !pts.iter().any(|q| (q.x - 3.5).abs() < 0.2),
            "the bulge the trace cut off is not reproduced"
        );
        // The LEFT curve the trace DID follow is preserved (diagonal facets kept).
        assert!(
            !cs.floor_plan.non_orthogonal_edges().is_empty(),
            "the followed left curve is spliced verbatim, not squared off"
        );
        assert!(
            ring_area(&cs) < area0 - 0.3,
            "cutting the bulge reduced the footprint (got {} vs {area0})",
            ring_area(&cs)
        );
    }

    /// The core feature: an upper storey that extends past the storey below
    /// (a bump-out over a ground-floor garage). The walls it shares with the
    /// storey below are pinned; the genuinely-new garage walls are NOT, so the
    /// assessor is still prompted to dimension them.
    #[test]
    fn garage_extension_leaves_new_wall_unpinned() {
        let mut c = dimensioned_square_controller(); // storey 0 = 10x10
        c.add_storey().unwrap();
        // Hug three sides of the 10x10, but bump out a 3x3 garage past x=10
        // between y=4 and y=7. Shared-wall points are a touch off (snap pulls
        // them in); garage corners are exact and well outside the snap radius.
        let world = vec![
            (0.2, 0.2),  // → (0,0)
            (9.8, 0.2),  // → (10,0)
            (9.8, 4.0),  // → x=10 on the right wall
            (13.0, 4.0), // garage corner (new)
            (13.0, 7.0), // garage corner (new)
            (9.8, 7.0),  // → x=10 on the right wall
            (9.8, 9.8),  // → (10,10)
            (0.2, 9.8),  // → (0,10)
            (0.3, 0.3),  // → (0,0)
        ];
        c.apply_outline_world_inner(&world, true)
            .expect("apply world outline");

        assert_eq!(c.floor_plan.ring_count(), 1);
        assert!(c.dimensioning.scale_set);
        assert!(
            !c.dimensioning.known_params.is_empty(),
            "shared walls pinned"
        );
        // Footprint is bigger than the 100 m² below (gained the garage bump).
        let area = ring_area(&c);
        assert!(area > 105.0, "extended footprint > 100 m² (got {area})");
        // The genuinely-new (garage) walls are NOT pinned: some active param is
        // still unknown, so the assessor will be asked to dimension it (after
        // any collinearity review that the new corners trigger).
        let active = c.floor_plan.all_active_params();
        assert!(
            active
                .iter()
                .any(|p| !c.dimensioning.known_params.contains(p)),
            "a new (garage) wall's parameter is left unpinned to dimension \
             (active={}, known={})",
            active.len(),
            c.dimensioning.known_params.len()
        );
    }

    /// Collinearity separations scale to the plan's units: a clean 10x10 *metre*
    /// square has its opposite walls 10 m apart — far beyond the near-collinear
    /// range — so nothing is flagged. (With the old fixed pixel range a 10 m gap
    /// looked "near-collinear" and every parallel wall pair was asked about.)
    #[test]
    fn collinearity_range_scales_to_metre_plan() {
        let mut c = dimensioned_square_controller(); // 10x10 metre square
        c.review = None;
        c.populate_collinearity_review();
        assert!(
            c.review.is_none(),
            "a clean metre-scale square must not flag every wall as collinear"
        );
    }

    /// Accepting a collinearity decision drops the now-redundant vertex. A
    /// spurious "step" (from mouse jitter) flagged near-collinear collapses to a
    /// clean wall once accepted — the straight-through vertices are removed, so
    /// the user is never asked for a length that can't be set without changing
    /// the shape. Runs with `scale_set = true` (the storey is already in metres,
    /// as an upper storey is) so the `simplify_degenerate` safety net is skipped
    /// and the cleanup must collapse the zero-length step edge itself.
    #[test]
    fn accepting_collinearity_removes_spurious_step_vertices() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = dimensioned_square_controller();
        // A 10x10 whose bottom wall has a spurious 0.3 m step (two horizontals
        // at y=0 and y=0.3 joined by a tiny vertical) — six vertices.
        c.floor_plan = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(5., 0.),
            p(5., 0.3),
            p(10., 0.3),
            p(10., 10.),
            p(0., 10.),
        ])
        .unwrap();
        // Already metric (like an upper storey drawn in world coords): scale set,
        // every current param known. This is the path where `simplify_degenerate`
        // is NOT run, so only the collinear/coincident cleanup can remove the step.
        c.dimensioning = DimensionState::default();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        assert_eq!(c.floor_plan.polygons()[0].len(), 6, "starts with the step");

        c.populate_collinearity_review();
        assert!(c.review.is_some(), "the near-collinear step is flagged");

        // Accept every pending suggestion (the step's two horizontals = 1 pair).
        while c.review.is_some() {
            c.accept_current_review().unwrap();
        }

        // The step's redundant vertices are gone → back to a clean 4-corner box.
        assert_eq!(
            c.floor_plan.polygons()[0].len(),
            4,
            "spurious step vertices removed after collinearity accept"
        );
        let area = ring_area(&c);
        assert!(
            (area - 100.0).abs() < 1.0,
            "outline is a clean 10x10 (got {area})"
        );
    }

    /// Reproduces the field report: a new upper storey traced over the one below
    /// with an exaggerated "nib" bumping off the base (a slipped pen). Two fixes
    /// are exercised together:
    ///   1. The near-collinear horizontals collapse onto the PREVIOUS storey's
    ///      base (y=0), not the nib's offset line (y=1.2) — so the footprint
    ///      stays at ~100 m² rather than shrinking to ~88.
    ///   2. Once that merge collapses the nib's connectors to zero length, the
    ///      review's leftover vertical suggestion about them is auto-skipped, so
    ///      the user isn't asked about a line that no longer exists.
    #[test]
    fn nib_collapses_onto_previous_base_and_skips_degenerate_connector() {
        let mut c = dimensioned_square_controller(); // storey 0 = 10x10, base y=0
        c.add_storey().unwrap();
        // Base mostly on y=0, with a 0.3-wide nib bumping up to y=1.2 between
        // x=4.0 and x=4.3. The 1.2 m bump clears the snap tolerance (so it
        // survives as a near-collinear question) but stays within range.
        let world = vec![
            (0.0, 0.0),
            (4.0, 0.0),
            (4.0, 1.2),
            (4.3, 1.2),
            (4.3, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (0.05, 0.05),
        ];
        c.apply_outline_world_inner(&world, true)
            .expect("apply world outline");

        let n = c.review.as_ref().map_or(0, |r| r.suggestions.len());
        assert!(
            n >= 2,
            "the nib creates a horizontal (base/top) and a vertical (connectors) pair (got {n})"
        );

        // Accept the first (horizontal) pair.
        c.accept_current_review().unwrap();
        // The connectors are now zero-length, so the remaining vertical
        // suggestion about them is auto-skipped → the review is already done.
        assert!(
            c.review.is_none(),
            "degenerate-connector suggestion auto-skipped after the merge"
        );

        // Nib gone, clean 4-corner box, and crucially the base sat DOWN onto
        // y=0 (area ~100) instead of UP onto the nib line (which would give ~88).
        assert_eq!(
            c.floor_plan.polygons()[0].len(),
            4,
            "nib removed, clean box"
        );
        let area = ring_area(&c);
        assert!(
            (area - 100.0).abs() < 1.0,
            "collapsed onto the previous base (y=0) → ~100 m² (got {area})"
        );
    }

    /// Sample a vertex ring into a DENSE closed trace (~`step` spacing), the way
    /// a real freehand outline stroke arrives — hundreds of points, not just the
    /// corners. The new-storey reproduce/coverage check measures how closely the
    /// trace hugs the boundary, which only behaves like real input when the trace
    /// is sampled, not corner-to-corner.
    fn densify_ring(verts: &[Point2f32], step: f32) -> Vec<(f32, f32)> {
        let n = verts.len();
        let mut out = Vec::new();
        for i in 0..n {
            let (a, b) = (verts[i], verts[(i + 1) % n]);
            let len = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
            let nsamp = (len / step).ceil().max(1.0) as usize;
            for s in 0..nsamp {
                let f = s as f32 / nsamp as f32;
                out.push((a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        }
        out.push((verts[0].x, verts[0].y));
        out
    }

    /// Building a new storey by tracing over a storey that has a CURVED wall
    /// copies that storey's exact (faceted) linear representation up — the curve
    /// is preserved, not squared off into right angles. Reproduces the manual
    /// session: storey 0 has a 3-facet rounded corner; the new storey traces the
    /// same outline.
    #[test]
    fn new_storey_over_curved_storey_copies_curve_verbatim() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        // Storey 0: a 4x3 box whose top-right corner is rounded by 3 facets.
        let curved = vec![
            p(0., 0.),
            p(4., 0.),
            p(4., 2.0),
            p(3.7, 2.6),
            p(3.2, 2.9),
            p(2.5, 3.0),
            p(0., 3.0),
        ];
        c.floor_plan = FloorPlan::from_clean_ring(&curved).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();
        let prev_nonortho = c.floor_plan.non_orthogonal_edges().len();
        assert!(
            prev_nonortho >= 2,
            "storey 0 has a curved (non-orthogonal) run"
        );

        c.add_storey().unwrap();
        // Trace the same outline DENSELY (as a real freehand stroke is), reproducing
        // it with no real excursion.
        let trace = densify_ring(&curved, 0.1);
        c.apply_outline_world_inner(&trace, true).unwrap();

        // The curve is carried up VERBATIM (not squared off): same number of
        // non-orthogonal facet edges, every param pinned, nothing to dimension.
        assert_eq!(
            c.floor_plan.non_orthogonal_edges().len(),
            prev_nonortho,
            "curve facets preserved in the new storey (not squared off)"
        );
        assert!(c.dimensioning.scale_set);
        assert!(
            c.dimensioning.current_prompt.is_none(),
            "whole outline shared → nothing to dimension"
        );
        let area = ring_area(&c);
        let prev_area = {
            let n = curved.len();
            let mut a = 0.0;
            for i in 0..n {
                let j = (i + 1) % n;
                a += curved[i].x * curved[j].y - curved[j].x * curved[i].y;
            }
            (a * 0.5).abs()
        };
        assert!(
            (area - prev_area).abs() < 0.5,
            "footprint matches the storey below (got {area}, expected {prev_area})"
        );
    }

    /// Full process, the part that was broken: a finished (dimensioned, curved)
    /// first storey, then a second storey traced over it with realistic hand
    /// SLOP — points that stray past the snap radius (so they don't snap exactly)
    /// but stay within the new-wall tolerance. That's loose tracing, not a new
    /// wall, so the curve must still be followed verbatim. (The old
    /// max-distance reproduce check mistook the stray for an excursion and
    /// squared the curve off.)
    #[test]
    fn second_storey_follows_curve_despite_trace_slop() {
        let p = |x: f32, y: f32| Point2f32::new(x, y);
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let _ = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        // Finished first storey: a 4x3 box with a 3-facet rounded corner.
        let curved = vec![
            p(0., 0.),
            p(4., 0.),
            p(4., 2.0),
            p(3.7, 2.6),
            p(3.2, 2.9),
            p(2.5, 3.0),
            p(0., 3.0),
        ];
        c.floor_plan = FloorPlan::from_clean_ring(&curved).unwrap();
        c.dimensioning.scale_set = true;
        c.dimensioning.known_params = c.floor_plan.all_active_params();
        c.commit_to_ber().unwrap();
        let curve_edges = c.floor_plan.non_orthogonal_edges().len();
        assert!(curve_edges >= 2, "first storey has the curved run");
        assert!(
            c.dimensioning.current_prompt.is_none(),
            "first storey fully dimensioned"
        );

        c.add_storey().unwrap();
        // Trace the SAME outline densely, but TWO isolated points stray ~0.7 m off
        // the boundary — past the snap radius (~0.4 m here), within the new-wall
        // tolerance (1 m), with on-boundary neighbours either side. Hand jitter,
        // not a sustained new wall, so the curve must still be followed verbatim.
        let mut trace = densify_ring(&curved, 0.1);
        let q = trace.len() / 4;
        trace[q] = (trace[q].0, trace[q].1 - 0.7); // a point on the bottom strays down
        let q3 = trace.len() * 3 / 4;
        trace[q3] = (trace[q3].0 - 0.7, trace[q3].1); // a point on the left strays out
        c.apply_outline_world_inner(&trace, true)
            .expect("apply world outline");

        // Curve followed verbatim; whole outline shared → nothing to dimension.
        assert_eq!(
            c.floor_plan.non_orthogonal_edges().len(),
            curve_edges,
            "the curve is followed despite slop (not squared off)"
        );
        assert!(
            c.dimensioning.current_prompt.is_none(),
            "whole outline shared → no dimension prompt"
        );
    }

    /// `resync_dimensioning_to_plan` (the consolidated post-topology-change
    /// cleanup) drops dimensioning state that no longer refers to a live
    /// param/vertex, and keeps the rest.
    #[test]
    fn resync_dimensioning_drops_stale_params_and_vertices() {
        let mut c = dimensioned_square_controller();
        let active = c.floor_plan.all_active_params();
        assert!(!active.is_empty(), "the square has active params");

        // Seed real state + bogus entries (ParameterId/VertexId are usize/u64).
        c.dimensioning.known_params = active.clone();
        c.dimensioning.known_params.insert(99_999); // not an active param
        c.dimensioning.dimensioned_walls = vec![123_456]; // not a live vertex
        c.dimensioning.skipped_walls = std::iter::once(123_456).collect();

        c.resync_dimensioning_to_plan();

        assert_eq!(
            c.dimensioning.known_params, active,
            "stale param pruned, real ones kept"
        );
        assert!(
            c.dimensioning.dimensioned_walls.is_empty(),
            "stale dimensioned wall pruned"
        );
        assert!(
            c.dimensioning.skipped_walls.is_empty(),
            "stale skipped wall pruned"
        );
    }

    /// A new upper storey starts zoomed out so the previous-storey watermark
    /// leaves room to draw a larger footprint; storey 0 fits normally.
    #[test]
    fn add_storey_zooms_out() {
        let mut c = dimensioned_square_controller();
        assert_eq!(c.view().zoom, 1.0, "storey 0 fits normally");
        c.add_storey().unwrap();
        assert!(
            c.view().zoom < 1.0,
            "new storey zoomed out to leave drawing room (got {})",
            c.view().zoom
        );
    }

    /// Two-storey DB round-trip: both finalised storeys come back on reopen.
    #[test]
    fn two_storey_db_round_trip() {
        let mut c = AppController::new(crate::engine::open_db(":memory:").unwrap());
        let ber = c.create_ber(
            "T".into(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        );
        finalise_current_storey_as_square(&mut c, 10.0);
        c.add_storey().unwrap();
        finalise_current_storey_as_square(&mut c, 6.0);

        c.open_ber(&ber.id).expect("reload");
        let storeys = &c.current_record.as_ref().unwrap().data.storeys;
        assert_eq!(storeys.len(), 2, "both storeys reloaded");
        let s0 = storeys[0].floors.to_vertex_lists();
        let s1 = storeys[1].floors.to_vertex_lists();
        assert!(!s0.is_empty(), "storey 0 has a footprint");
        assert!(!s1.is_empty(), "storey 1 has a footprint");
        // Areas (signed shoelace, abs) — 10x10 ≈ 100, 6x6 ≈ 36.
        let area_of = |ring: &Vec<Point2f64>| {
            let mut a = 0.0;
            let n = ring.len();
            for i in 0..n {
                let j = (i + 1) % n;
                a += ring[i].x * ring[j].y - ring[j].x * ring[i].y;
            }
            (a * 0.5).abs()
        };
        let a0 = area_of(&s0[0]);
        let a1 = area_of(&s1[0]);
        assert!((a0 - 100.0).abs() < 1.0, "storey 0 ~ 100 m² (got {a0})");
        assert!((a1 - 36.0).abs() < 1.0, "storey 1 ~ 36 m² (got {a1})");
        // After reopen the editor lands on storey 0 by default.
        assert_eq!(c.current_storey_idx(), Some(0));
    }
}
