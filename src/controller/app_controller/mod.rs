// `AppController`'s behaviour is split across this module and two submodules
// (children, so they can use the private fields/methods declared here):
//   - `dimensioning` — the dimension-prompt workflow.
//   - `editing` — Edit-mode interaction + measurement prompts.
// This file holds the struct + state types and the rest (history, view, BER
// record/persistence, collinearity review, sketch/stroke handling).
mod dimensioning;
mod editing;

use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository};
use crate::engine::dataloader::DataLoader;
use crate::engine::Db;
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::ItemId;
use datatypes::ber::structural::{FloorRegions, Storey};
use domain::points::{Point2f32, Point2f64};
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_2d_plan::{CollinearitySuggestion, FloorPlan, ParameterAxis, ParameterId, VertexId};
use sketch::model::Stroke;
use sketch::SketchEngine;
use std::collections::HashSet;

/// Default storey ceiling height in meters, used as a placeholder until the UI
/// collects it from the assessor.
const DEFAULT_CEILING_HEIGHT_M: f64 = 2.4;

/// Range (in pixels) for what counts as "near-collinear, ask the user."
/// `MIN_SEPARATION` is just above the parametric layer's dedup tolerance —
/// pairs closer than this already share a coordinate expression (and are
/// skipped by the detector). Anything from there up to `MAX_SEPARATION` is
/// fair game to ask the user about; pairs farther than `MAX_SEPARATION` are
/// treated as distinct walls.
const COLLINEARITY_MIN_SEPARATION: f32 = 1.0;
const COLLINEARITY_MAX_SEPARATION: f32 = 60.0;

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

/// Version stamp for the persisted sketch snapshot. Bump when `FloorPlan`'s
/// serialized shape or the snapshot fields change in a backward-incompatible
/// way — older snapshots then fail the version check and the editor rebuilds
/// from the resolved `floors` geometry instead of crashing.
const SKETCH_SNAPSHOT_VERSION: u32 = 1;

/// The editable parametric state for one storey, serialized into
/// `Storey.sketch_state` so reopening a BER restores every collinearity
/// equivalence and entered dimension. Excludes the transient `current_prompt`
/// (recomputed on load).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SketchSnapshot {
    version: u32,
    floor_plan: FloorPlan,
    known_params: HashSet<ParameterId>,
    scale_set: bool,
    dimensioned_walls: Vec<VertexId>,
    skipped_walls: HashSet<VertexId>,
}

pub struct AppController {
    conn: Db,
    sketch_engine: SketchEngine,
    floor_plan: FloorPlan,
    loader_engine: DataLoader,
    /// The BER record being edited. `commit_to_ber` writes through this to SQLite.
    /// `None` means no BER is loaded yet — sketches stay in-memory until a BER exists.
    current_record: Option<BerRecord>,
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
    dimensioning: DimensionState,
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
}

/// Snapshot of the data state for undo/redo. Includes the floor plan +
/// dimensioning + edit-state — view (zoom/pan), prompts, and in-flight
/// drag state are deliberately excluded so undo doesn't fight the user's
/// current view or interrupt an in-progress prompt mid-flow.
#[derive(Debug, Clone)]
pub struct HistorySnapshot {
    floor_plan: FloorPlan,
    dimensioning: DimensionState,
    edit_state: EditState,
    /// A mid-flow provisional arc is part of the undoable state: undoing the
    /// stroke that began it must clear it; undoing a dimension that finished it
    /// must restore it.
    pending_arc: Option<PendingArc>,
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
            review: None,
            pending_redraw: None,
            pending_arc: None,
            dimensioning: DimensionState::default(),
            edit_state: EditState::default(),
            edit_drag_prompt: None,
            vertex_drop_prompt: None,
            history: Vec::new(),
            redo: Vec::new(),
            view: ViewTransform::default(),
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
        }
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
        // Save current state for redo.
        self.redo.push(HistorySnapshot {
            floor_plan: self.floor_plan.clone(),
            dimensioning: self.dimensioning.clone(),
            edit_state: self.edit_state.clone(),
            pending_arc: self.pending_arc.clone(),
        });
        self.floor_plan = snap.floor_plan;
        self.dimensioning = snap.dimensioning;
        self.edit_state = snap.edit_state;
        self.pending_arc = snap.pending_arc;
        // Any in-flight prompts no longer match the restored state — clear.
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        // Push the restored polygon to BER so the canvas reflects it.
        self.commit_to_ber()?;
        Ok(())
    }

    /// Re-apply the last undone state. No-op if redo stack is empty.
    pub fn redo(&mut self) -> Result<(), String> {
        let Some(snap) = self.redo.pop() else {
            return Ok(());
        };
        self.history.push(HistorySnapshot {
            floor_plan: self.floor_plan.clone(),
            dimensioning: self.dimensioning.clone(),
            edit_state: self.edit_state.clone(),
            pending_arc: self.pending_arc.clone(),
        });
        self.floor_plan = snap.floor_plan;
        self.dimensioning = snap.dimensioning;
        self.edit_state = snap.edit_state;
        self.pending_arc = snap.pending_arc;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
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
        ber
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
        self.restore_sketch_state(record.data.storeys.first());
        self.current_record = Some(record);
        self.view = ViewTransform::default();
        self.edit_state = EditState::default();
        self.review = None;
        self.compute_next_dimension_prompt();
        self.history.clear();
        self.redo.clear();
        Ok(())
    }

    /// Rebuild `self.floor_plan` + `self.dimensioning` from a storey's saved
    /// state. Prefers the parametric snapshot; on absence/parse failure/version
    /// mismatch, rebuilds from the resolved floor geometry (lossy).
    fn restore_sketch_state(&mut self, storey: Option<&Storey>) {
        self.dimensioning = DimensionState::default();
        // A provisional arc is transient and tied to the OUTGOING plan's
        // vertices; loading a record must not carry a stale one over (it would
        // dangle / render a phantom preview against the new geometry).
        self.pending_arc = None;
        let Some(storey) = storey else {
            self.floor_plan = FloorPlan::default();
            return;
        };

        // Primary path: the parametric snapshot.
        if let Some(json) = storey.sketch_state.as_deref() {
            match serde_json::from_str::<SketchSnapshot>(json) {
                Ok(snap) if snap.version == SKETCH_SNAPSHOT_VERSION => {
                    self.floor_plan = snap.floor_plan;
                    self.dimensioning.known_params = snap.known_params;
                    self.dimensioning.scale_set = snap.scale_set;
                    self.dimensioning.dimensioned_walls = snap.dimensioned_walls;
                    self.dimensioning.skipped_walls = snap.skipped_walls;
                    return;
                }
                Ok(snap) => eprintln!(
                    "sketch snapshot version {} != {}, rebuilding from geometry",
                    snap.version, SKETCH_SNAPSHOT_VERSION
                ),
                Err(e) => eprintln!("sketch snapshot parse failed ({}), rebuilding", e),
            }
        }

        // Fallback: rebuild a fresh parametric model from the resolved floors.
        // Equivalences and dimensions can't be recovered — they were never
        // stored for this record.
        let rings = storey.floors.to_vertex_lists();
        let primary: Vec<Point2f32> = rings
            .into_iter()
            .find(|r| r.len() >= 3)
            .map(|r| {
                r.iter()
                    .map(|p| Point2f32::new(p.x as f32, p.y as f32))
                    .collect()
            })
            .unwrap_or_default();
        self.floor_plan = if primary.len() >= 3 {
            FloorPlan::from_points_to_orthogonal(&primary, 10.0, true).unwrap_or_default()
        } else {
            FloorPlan::default()
        };
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
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };

        if record.data.storeys.is_empty() {
            record.data.storeys.push(Storey::default());
        }

        // Convert every polygon (ring) of the floor plan into a f64-typed
        // polygon for the BER data model. One ring → one FloorRegion.
        let polygons_f64: Vec<Vec<Point2f64>> = self
            .floor_plan
            .polygons()
            .into_iter()
            .map(|ring| {
                ring.into_iter()
                    .map(|p| Point2f64::new(p.x as f64, p.y as f64))
                    .collect()
            })
            .collect();

        if polygons_f64.iter().any(|p| p.len() >= 3) {
            record.data.storeys[0].floors = FloorRegions::from_vertex_lists(
                polygons_f64,
                DEFAULT_CEILING_HEIGHT_M,
                None,
                None,
                None,
            );
        }

        // Persist the editable parametric model + dimensioning state alongside
        // the resolved geometry, so reopening this BER keeps all collinearity
        // equivalences and entered dimensions. `floors` above is the derived
        // view; this snapshot is the source of truth for editing.
        let snapshot = SketchSnapshot {
            version: SKETCH_SNAPSHOT_VERSION,
            floor_plan: self.floor_plan.clone(),
            known_params: self.dimensioning.known_params.clone(),
            scale_set: self.dimensioning.scale_set,
            dimensioned_walls: self.dimensioning.dimensioned_walls.clone(),
            skipped_walls: self.dimensioning.skipped_walls.clone(),
        };
        match serde_json::to_string(&snapshot) {
            Ok(json) => record.data.storeys[0].sketch_state = Some(json),
            Err(e) => eprintln!("failed to serialize sketch snapshot: {}", e),
        }

        BerRepository::new(&self.conn)
            .upsert(record)
            .map_err(|e| format!("upsert failed: {}", e))
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
        self.maybe_start_dimensioning();
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
            self.maybe_start_dimensioning();
        }
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
    fn apply_drawperp_stroke(&mut self, points: &[Point2f32], closed: bool) -> Result<(), String> {
        // Snapshot pre-stroke (may be empty / pre-dim) for undo.
        let undo_snap = self.current_history_snapshot();
        self.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
            .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
        println!("floor plan: {:?}", self.floor_plan);
        self.dimensioning = DimensionState::default();
        self.push_history_snapshot(undo_snap);
        self.commit_to_ber()?;
        self.populate_collinearity_review();
        self.maybe_start_dimensioning();
        Ok(())
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
        self.commit_to_ber()?;
        self.populate_collinearity_review();
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
        Ok(())
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
        // The porch is a fix-up to the ENVELOPE — once spliced, that it was a
        // "porch" is irrelevant. Drop any vertex it left collinear (e.g. a
        // corner a flush porch turned into a mid-wall point) so the outline
        // stays minimal, then re-sync dimensioning state.
        self.clean_envelope_collinear();
        self.commit_to_ber()?;
        // Ask about collinearity ONLY for the porch's new walls — e.g. whether
        // a new porch front lines up with an earlier one. We never assume it;
        // and we don't re-surface pairs among the pre-existing walls.
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
        self.push_history_snapshot(undo_snap);
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
        self.commit_to_ber()?;
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
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
        let active = self.floor_plan.all_active_params();
        self.dimensioning
            .known_params
            .retain(|p| active.contains(p));
        self.dimensioning
            .dimensioned_walls
            .retain(|v| self.floor_plan.find_vertex(*v).is_some());
        self.dimensioning
            .skipped_walls
            .retain(|v| self.floor_plan.find_vertex(*v).is_some());
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
        let active = self.floor_plan.all_active_params();
        self.dimensioning
            .known_params
            .retain(|p| active.contains(p));

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
            let mut a: Vec<_> = active.iter().copied().collect();
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
        let newest_ring = self.floor_plan.ring_count().saturating_sub(1);
        let suggestions: Vec<_> = self
            .floor_plan
            .suggest_collinear(COLLINEARITY_MIN_SEPARATION, COLLINEARITY_MAX_SEPARATION)
            .into_iter()
            .filter(|s| s.ring_a == newest_ring || s.ring_b == newest_ring)
            .collect();
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
}
