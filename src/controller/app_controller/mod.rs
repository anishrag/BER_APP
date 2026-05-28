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
        });
        self.floor_plan = snap.floor_plan;
        self.dimensioning = snap.dimensioning;
        self.edit_state = snap.edit_state;
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
        });
        self.floor_plan = snap.floor_plan;
        self.dimensioning = snap.dimensioning;
        self.edit_state = snap.edit_state;
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
            EditTools::Extend | EditTools::Trim | EditTools::TrimOrtho
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
                EditTools::Extend => {
                    if let Err(e) = self.apply_extend_stroke(&stroke.points) {
                        // For now just log; in future we'll surface this
                        // to the UI alongside the dimension error.
                        eprintln!("apply_extend_stroke: {}", e);
                    }
                }
                EditTools::Trim | EditTools::TrimOrtho => {
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
    /// splicing the porch into the parametric plan — lives in
    /// `FloorPlan::extend`; here we only orchestrate app state. Because the
    /// splice preserves existing vertex/parameter IDs, the dimensioning state
    /// stays valid: we keep `known_params` (dropping only any that a cross-edge
    /// cut removed) instead of resetting, so the user is asked to dimension
    /// just the porch's new walls.
    fn apply_extend_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        let undo_snap = self.current_history_snapshot();
        let minted = self
            .floor_plan
            .extend(points)
            .map_err(|e| format!("{}", e))?;
        println!("Extend: {} new param(s) {:?}", minted.len(), minted);
        let active = self.floor_plan.all_active_params();
        self.dimensioning
            .known_params
            .retain(|p| active.contains(p));
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
    /// tool: `Trim` cuts a straight (possibly diagonal) line through the
    /// outline; `TrimOrtho` carves an orthogonal stepped cut (the path is
    /// orthogonalized and spliced like Extend, but removing area). The geometry
    /// lives in `FloorPlan::trim_by_stroke` / `trim_by_polygon`; here we only
    /// orchestrate app state. Surviving walls keep their parameters.
    fn apply_trim_stroke(&mut self, points: &[Point2f32]) -> Result<(), String> {
        if points.len() < 2 {
            return Err("trim stroke needs at least 2 points".into());
        }
        let undo_snap = self.current_history_snapshot();
        let minted = if matches!(self.sketch_engine.edit_tool, EditTools::TrimOrtho) {
            self.floor_plan.trim_by_polygon(points)
        } else {
            self.floor_plan.trim_by_stroke(points)
        }
        .map_err(|e| format!("{}", e))?;
        println!("Trim: {} new param(s) {:?}", minted.len(), minted);
        let active = self.floor_plan.all_active_params();
        self.dimensioning
            .known_params
            .retain(|p| active.contains(p));
        self.commit_to_ber()?;
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
        Ok(())
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
}
