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
use manip_2d_plan::{
    find_flipped_edge, intersect_circles, project_to_circle, solve_axis_wall, Axis,
    CollinearitySuggestion, FloorPlan, ParameterAxis, ParameterId, VertexId,
};
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

    /// VertexId of the wall currently selected in Edit mode, if any. The UI
    /// uses this to render the selection highlight.
    pub fn edit_selected_wall(&self) -> Option<VertexId> {
        self.edit_state.selected_wall
    }

    /// World-coord endpoints of the currently selected Edit-mode wall, or
    /// `None` if nothing is selected. Returned in the same order the
    /// polygon traverses (wall_a then wall_b).
    pub fn edit_selected_wall_positions(&self) -> Option<((f64, f64), (f64, f64))> {
        let vid = self.edit_state.selected_wall?;
        let (ring, edge_start) = self.floor_plan.find_vertex(vid)?;
        let polygons = self.floor_plan.polygons();
        let ring_pts = polygons.get(ring)?;
        let n = ring_pts.len();
        if n < 2 {
            return None;
        }
        let a = ring_pts.get(edge_start)?;
        let b = ring_pts.get((edge_start + 1) % n)?;
        Some(((a.x as f64, a.y as f64), (b.x as f64, b.y as f64)))
    }

    /// World-coord position of the currently selected Edit-mode vertex
    /// (from double-click insert), or `None`. Used by the UI to render
    /// a highlight marker on the inserted vertex.
    pub fn edit_selected_vertex_position(&self) -> Option<(f64, f64)> {
        let vid = self.edit_state.selected_vertex?;
        let (ring, idx) = self.floor_plan.find_vertex(vid)?;
        let polygons = self.floor_plan.polygons();
        let p = polygons.get(ring)?.get(idx)?;
        Some((p.x as f64, p.y as f64))
    }

    /// Edit-mode primary-button press. Hit-test the world-coord position
    /// against all walls; if any wall is within `tol`, select it and start
    /// a drag. Snapshots the pre-drag value of the wall's controlling
    /// coord so the post-drag prompt can revert on Cancel.
    pub fn edit_mouse_down(&mut self, world_x: f64, world_y: f64, tol: f64) -> bool {
        if !matches!(self.sketch_engine.edit_tool, EditTools::Edit) {
            return false;
        }
        let Some((ring, edge_start)) = self.floor_plan.hit_test_wall(world_x, world_y, tol) else {
            self.edit_state.selected_wall = None;
            self.edit_state.dragging = false;
            return false;
        };
        let Some(vid) = self.floor_plan.vertex_id_at(ring, edge_start) else {
            return false;
        };
        // Snapshot what kind of coord this wall is anchored to and its
        // current value, so a Cancel on the post-drag prompt can revert.
        if let Some((p, _axis)) = self.floor_plan.wall_movable_param(ring, edge_start) {
            self.edit_state.pre_drag_value = self.floor_plan.parameter_value(p);
            self.edit_state.pre_drag_kind = Some(MovedCoordKind::Parameter(p));
        } else if let Some((c, _axis)) = self.floor_plan.wall_anchor_constant(ring, edge_start) {
            self.edit_state.pre_drag_value = Some(c);
            self.edit_state.pre_drag_kind = Some(MovedCoordKind::Constant(c));
        } else {
            self.edit_state.pre_drag_value = None;
            self.edit_state.pre_drag_kind = None;
        }
        self.edit_state.selected_wall = Some(vid);
        self.edit_state.dragging = true;
        self.edit_state.drag_snapshot_taken = false;
        true
    }

    /// Edit-mode double-click: insert a new vertex on the wall closest to
    /// `(world_x, world_y)` (within `tol`), place it at the click point,
    /// and start a free-2D drag on it. Returns true if a vertex was
    /// inserted.
    pub fn edit_double_click(&mut self, world_x: f64, world_y: f64, tol: f64) -> bool {
        if !matches!(self.sketch_engine.edit_tool, EditTools::Edit) {
            return false;
        }
        let Some((ring, edge_start)) = self.floor_plan.hit_test_wall(world_x, world_y, tol) else {
            return false;
        };
        // Snapshot before inserting the vertex — the whole insert + drag
        // + prompt-apply sequence undoes as one action.
        self.record_history();
        let new_vid = match self.floor_plan.insert_vertex_on_wall(
            ring,
            edge_start,
            world_x as f32,
            world_y as f32,
        ) {
            Some(v) => v,
            None => return false,
        };
        self.edit_state.selected_wall = None;
        self.edit_state.selected_vertex = Some(new_vid);
        self.edit_state.dragging = true;
        // Drag snapshot already taken (covers the whole interaction
        // starting with the insert); flag it so edit_mouse_drag doesn't
        // take a second one.
        self.edit_state.drag_snapshot_taken = true;
        self.edit_state.pre_drag_vertex_pos = Some((world_x as f32, world_y as f32));
        self.edit_state.pre_drag_value = None;
        self.edit_state.pre_drag_kind = None;
        // Push the new topology to BER so the renderer sees the split
        // wall + new vertex immediately. Without this the polygon stays
        // at its pre-insert shape during the drag and only one edge
        // looks like it's following the cursor.
        if let Err(e) = self.commit_to_ber() {
            eprintln!("commit_to_ber after insert_vertex_on_wall: {}", e);
        }
        println!(
            "vertex inserted: ring {}/edge {}+1 (vertex_id {}) at ({:.3}, {:.3})",
            ring, edge_start, new_vid, world_x, world_y
        );
        true
    }

    /// Edit-mode drag update. Dispatches based on what's selected:
    ///
    /// * If a `selected_vertex` is set (from a double-click insert),
    ///   updates both its x and y parameters to the cursor's world-coord
    ///   position — free 2D drag.
    /// * Otherwise translates the selected wall via `set_parameter` or
    ///   `shift_constant` (same as before).
    pub fn edit_mouse_drag(&mut self, world_x: f64, world_y: f64) {
        if !self.edit_state.dragging {
            return;
        }
        // Take a single undo snapshot at the very first mutation of the
        // current drag. Subsequent frames don't re-snapshot — the whole
        // drag undoes/redoes as one action.
        if !self.edit_state.drag_snapshot_taken {
            self.record_history();
            self.edit_state.drag_snapshot_taken = true;
        }
        let mut did_mutate = false;
        // Vertex drag — free 2D.
        if let Some(vid) = self.edit_state.selected_vertex {
            if let Some((ring, idx)) = self.floor_plan.find_vertex(vid) {
                let x_param = self.floor_plan.vertex_x_param(ring, idx);
                let y_param = self.floor_plan.vertex_y_param(ring, idx);
                if let (Some(xp), Some(yp)) = (x_param, y_param) {
                    self.floor_plan.set_parameter(xp, world_x as f32);
                    self.floor_plan.set_parameter(yp, world_y as f32);
                    did_mutate = true;
                }
            }
        } else if let Some(vid) = self.edit_state.selected_wall {
            // Wall drag — translate perpendicular axis.
            if let Some((ring, edge_start)) = self.floor_plan.find_vertex(vid) {
                if let Some((param, axis)) = self.floor_plan.wall_movable_param(ring, edge_start) {
                    let new_value = match axis {
                        ParameterAxis::X => world_x as f32,
                        ParameterAxis::Y => world_y as f32,
                    };
                    self.floor_plan.set_parameter(param, new_value);
                    did_mutate = true;
                } else if let Some((old_value, axis)) =
                    self.floor_plan.wall_anchor_constant(ring, edge_start)
                {
                    let new_value = match axis {
                        ParameterAxis::X => world_x as f32,
                        ParameterAxis::Y => world_y as f32,
                    };
                    self.floor_plan.shift_constant(old_value, axis, new_value);
                    did_mutate = true;
                }
            }
        }
        // Push the live drag state to BER so the renderer sees the wall
        // / vertex follow the cursor instead of snapping only on release.
        if did_mutate {
            if let Err(e) = self.commit_to_ber() {
                eprintln!("commit_to_ber during edit_mouse_drag: {}", e);
            }
        }
    }

    /// Edit-mode drag release. Routes by what was being dragged:
    ///
    /// * Vertex drag → for now, just commits at the rough drop position.
    ///   A post-drop measurement prompt (separate from the wall one) is
    ///   a follow-up.
    /// * Wall drag → if dimensioned, populate the post-drag prompt;
    ///   otherwise commit directly.
    pub fn edit_mouse_up(&mut self) -> Result<(), String> {
        if !self.edit_state.dragging {
            return Ok(());
        }
        self.edit_state.dragging = false;
        if self.edit_state.selected_vertex.is_some() {
            self.commit_to_ber()?;
            if self.dimensioning.scale_set {
                self.populate_vertex_drop_prompt();
            }
            return Ok(());
        }
        if self.dimensioning.scale_set {
            self.populate_edit_drag_prompt();
        } else {
            self.commit_to_ber()?;
        }
        Ok(())
    }

    /// Build the post-drop prompt for the just-inserted vertex: collect up
    /// to 4 candidate distances (V to prev × {X,Y}, V to next × {X,Y}),
    /// then de-duplicate axes where prev and next share that coord
    /// (yielding 3 distinct measurements for the typical case).
    fn populate_vertex_drop_prompt(&mut self) {
        let Some(vid) = self.edit_state.selected_vertex else {
            return;
        };
        let Some((ring, idx)) = self.floor_plan.find_vertex(vid) else {
            return;
        };
        let polygons = self.floor_plan.polygons();
        let Some(ring_pts) = polygons.get(ring) else {
            return;
        };
        let n = ring_pts.len();
        if n < 3 {
            return;
        }
        let v = ring_pts[idx];
        let prev_idx = (idx + n - 1) % n;
        let next_idx = (idx + 1) % n;
        let prev = ring_pts[prev_idx];
        let next = ring_pts[next_idx];

        let prev_vid = match self.floor_plan.vertex_id_at(ring, prev_idx) {
            Some(v) => v,
            None => return,
        };
        let next_vid = match self.floor_plan.vertex_id_at(ring, next_idx) {
            Some(v) => v,
            None => return,
        };

        let mut measurements: Vec<VertexMeasurement> = Vec::new();
        let tol = 1e-3_f32;
        // X-axis measurements. If prev.x == next.x (vertex inserted on a
        // vertical wall), one measurement covers both.
        let prev_dx = v.x - prev.x;
        let next_dx = v.x - next.x;
        if (prev.x - next.x).abs() < tol {
            if prev_dx.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::X),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dx.abs(),
                    sign: prev_dx.signum(),
                });
            }
        } else {
            if prev_dx.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::X),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dx.abs(),
                    sign: prev_dx.signum(),
                });
            }
            if next_dx.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::X),
                    neighbor_vid: next_vid,
                    current_distance: next_dx.abs(),
                    sign: next_dx.signum(),
                });
            }
        }
        // Y-axis: mirror of the above.
        let prev_dy = v.y - prev.y;
        let next_dy = v.y - next.y;
        if (prev.y - next.y).abs() < tol {
            if prev_dy.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::Y),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dy.abs(),
                    sign: prev_dy.signum(),
                });
            }
        } else {
            if prev_dy.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::Y),
                    neighbor_vid: prev_vid,
                    current_distance: prev_dy.abs(),
                    sign: prev_dy.signum(),
                });
            }
            if next_dy.abs() > tol {
                measurements.push(VertexMeasurement {
                    kind: VertexMeasurementKind::Axis(ParameterAxis::Y),
                    neighbor_vid: next_vid,
                    current_distance: next_dy.abs(),
                    sign: next_dy.signum(),
                });
            }
        }
        // Wall-length measurements: Euclidean distance to each neighbour.
        // The user may prefer measuring along a diagonal wall directly
        // (the new wall they just created by moving V off-axis).
        let prev_dist = ((v.x - prev.x).powi(2) + (v.y - prev.y).powi(2)).sqrt();
        if prev_dist > tol {
            measurements.push(VertexMeasurement {
                kind: VertexMeasurementKind::WallLength,
                neighbor_vid: prev_vid,
                current_distance: prev_dist,
                sign: 1.0,
            });
        }
        let next_dist = ((v.x - next.x).powi(2) + (v.y - next.y).powi(2)).sqrt();
        if next_dist > tol {
            measurements.push(VertexMeasurement {
                kind: VertexMeasurementKind::WallLength,
                neighbor_vid: next_vid,
                current_distance: next_dist,
                sign: 1.0,
            });
        }

        if measurements.is_empty() {
            return;
        }
        self.vertex_drop_prompt = Some(VertexDropPrompt {
            vertex_vid: vid,
            measurements,
            applied: Vec::new(),
        });
    }

    /// Apply one measurement from the vertex-drop prompt. The first apply
    /// snaps V to satisfy that one constraint; the second triggers a joint
    /// solve over both, then the prompt is cleared. Supports any mix of
    /// axis-aligned (linear) and wall-length (circular) measurements.
    pub fn apply_vertex_measurement(&mut self, idx: usize, distance: f32) -> Result<(), String> {
        if !distance.is_finite() || distance < 0.0 {
            return Err(format!(
                "distance must be a non-negative number (got {})",
                distance
            ));
        }
        let prompt = self
            .vertex_drop_prompt
            .clone()
            .ok_or_else(|| "no active vertex-drop prompt".to_string())?;
        let m = prompt
            .measurements
            .get(idx)
            .cloned()
            .ok_or_else(|| "invalid measurement index".to_string())?;
        let vertex_vid = prompt.vertex_vid;
        let (v_ring, v_idx) = self
            .floor_plan
            .find_vertex(vertex_vid)
            .ok_or_else(|| "vertex no longer exists".to_string())?;
        let polygons = self.floor_plan.polygons();
        let v_pos = polygons
            .get(v_ring)
            .and_then(|r| r.get(v_idx))
            .copied()
            .ok_or_else(|| "vertex position unavailable".to_string())?;
        let cur = (v_pos.x, v_pos.y);
        let n_pos = self.neighbor_pos(m.neighbor_vid)?;

        // First applied measurement → snap to its constraint alone.
        // Subsequent applies → joint-solve the latest prior + this one.
        let new_pos = if prompt.applied.is_empty() {
            snap_single(&m, distance, n_pos, cur)
        } else {
            let (prior_idx, prior_val) = *prompt.applied.last().unwrap();
            let prior_m = &prompt.measurements[prior_idx];
            let prior_n_pos = self.neighbor_pos(prior_m.neighbor_vid)?;
            joint_solve(prior_m, prior_val, prior_n_pos, &m, distance, n_pos, cur)
        };

        let x_p = self
            .floor_plan
            .vertex_x_param(v_ring, v_idx)
            .ok_or_else(|| "vertex.x_expr isn't a parameter".to_string())?;
        let y_p = self
            .floor_plan
            .vertex_y_param(v_ring, v_idx)
            .ok_or_else(|| "vertex.y_expr isn't a parameter".to_string())?;
        self.floor_plan.set_parameter(x_p, new_pos.0);
        self.floor_plan.set_parameter(y_p, new_pos.1);
        // Both params land in `known_params` so the new walls get length
        // labels via the standard `walls_with_known_length` path.
        self.dimensioning.known_params.insert(x_p);
        self.dimensioning.known_params.insert(y_p);
        self.commit_to_ber()?;

        if let Some(p) = self.vertex_drop_prompt.as_mut() {
            p.applied.push((idx, distance));
            if p.applied.len() >= 2 {
                self.vertex_drop_prompt = None;
                self.edit_state.selected_vertex = None;
                self.edit_state.pre_drag_vertex_pos = None;
            }
        }
        Ok(())
    }

    /// World-coord position of a vertex by VertexId. Helper for the
    /// vertex-drop prompt's solve.
    fn neighbor_pos(&self, vid: VertexId) -> Result<(f32, f32), String> {
        let (r, i) = self
            .floor_plan
            .find_vertex(vid)
            .ok_or_else(|| "neighbour vertex no longer exists".to_string())?;
        let polygons = self.floor_plan.polygons();
        let p = polygons
            .get(r)
            .and_then(|ring| ring.get(i))
            .ok_or_else(|| "neighbour position unavailable".to_string())?;
        Ok((p.x, p.y))
    }

    /// Cancel the vertex-drop prompt. Reverts the vertex to its pre-drag
    /// position (the click point on the wall) and clears the prompt and
    /// selection. Pops the drag's undo snapshot for the same reason as
    /// `cancel_edit_drag`.
    pub fn cancel_vertex_drop(&mut self) -> Result<(), String> {
        if self.edit_state.drag_snapshot_taken {
            self.history.pop();
            self.edit_state.drag_snapshot_taken = false;
        }
        let Some(prompt) = self.vertex_drop_prompt.take() else {
            return Ok(());
        };
        let Some((wx, wy)) = self.edit_state.pre_drag_vertex_pos else {
            self.edit_state.selected_vertex = None;
            return Ok(());
        };
        if let Some((v_ring, v_idx)) = self.floor_plan.find_vertex(prompt.vertex_vid) {
            if let Some(xp) = self.floor_plan.vertex_x_param(v_ring, v_idx) {
                self.floor_plan.set_parameter(xp, wx);
            }
            if let Some(yp) = self.floor_plan.vertex_y_param(v_ring, v_idx) {
                self.floor_plan.set_parameter(yp, wy);
            }
        }
        self.edit_state.selected_vertex = None;
        self.edit_state.pre_drag_vertex_pos = None;
        self.commit_to_ber()?;
        Ok(())
    }

    /// Snapshot of the vertex-drop prompt for the UI. Includes all
    /// measurements that haven't yet been applied (i.e. not in
    /// `prompt.applied`), each with a live-recomputed distance and the
    /// neighbour's current world position so the renderer can draw the
    /// matching arrow.
    pub fn vertex_drop_prompt_for_ui(&self) -> Option<VertexDropPromptUiData> {
        let prompt = self.vertex_drop_prompt.as_ref()?;
        let (v_ring, v_idx) = self.floor_plan.find_vertex(prompt.vertex_vid)?;
        let polygons = self.floor_plan.polygons();
        let v_ring_pts = polygons.get(v_ring)?;
        let v = v_ring_pts.get(v_idx)?;
        let applied_set: std::collections::HashSet<usize> =
            prompt.applied.iter().map(|(i, _)| *i).collect();
        let mut active: Vec<VertexMeasurementUiData> = Vec::new();
        for (i, m) in prompt.measurements.iter().enumerate() {
            if applied_set.contains(&i) {
                continue;
            }
            let (n_ring, n_idx) = self.floor_plan.find_vertex(m.neighbor_vid)?;
            let n = polygons.get(n_ring).and_then(|r| r.get(n_idx))?;
            let live = match m.kind {
                VertexMeasurementKind::Axis(ParameterAxis::X) => (v.x - n.x).abs(),
                VertexMeasurementKind::Axis(ParameterAxis::Y) => (v.y - n.y).abs(),
                VertexMeasurementKind::WallLength => {
                    ((v.x - n.x).powi(2) + (v.y - n.y).powi(2)).sqrt()
                }
            };
            active.push(VertexMeasurementUiData {
                idx: i,
                neighbor_pos: (n.x as f64, n.y as f64),
                kind: m.kind,
                current_distance: live,
            });
        }
        Some(VertexDropPromptUiData {
            vertex_pos: (v.x as f64, v.y as f64),
            active,
        })
    }

    /// Construct the post-drag prompt: list every other parallel wall as
    /// a candidate "measure distance from this reference" option, sorted
    /// by traversal order so the same wall consistently shows up first.
    fn populate_edit_drag_prompt(&mut self) {
        let Some(vid) = self.edit_state.selected_wall else {
            return;
        };
        let Some((ring, edge_start)) = self.floor_plan.find_vertex(vid) else {
            return;
        };
        let Some(kind) = self.edit_state.pre_drag_kind else {
            return;
        };
        let polygons = self.floor_plan.polygons();
        let Some(ring_pts) = polygons.get(ring) else {
            return;
        };
        let n = ring_pts.len();
        if n < 2 {
            return;
        }
        let a = ring_pts[edge_start];
        let b = ring_pts[(edge_start + 1) % n];
        // Axis-aligned wall: determine orientation. Tolerance against floats.
        let dx = (b.x - a.x).abs();
        let dy = (b.y - a.y).abs();
        let is_vertical = dy > dx;
        let axis = if is_vertical {
            ParameterAxis::X
        } else {
            ParameterAxis::Y
        };
        let moved_value = if is_vertical { a.x } else { a.y };

        let mut options: Vec<MeasurementOption> = Vec::new();
        for (ri, r) in polygons.iter().enumerate() {
            for i in 0..r.len() {
                if ri == ring && i == edge_start {
                    continue;
                }
                let p = r[i];
                let q = r[(i + 1) % r.len()];
                let pdx = (q.x - p.x).abs();
                let pdy = (q.y - p.y).abs();
                let other_is_vertical = pdy > pdx;
                if other_is_vertical != is_vertical {
                    continue;
                }
                let ref_value = if is_vertical { p.x } else { p.y };
                let delta = moved_value - ref_value;
                let distance = delta.abs();
                if distance < 1e-3 {
                    continue;
                }
                let Some(ref_vid) = self.floor_plan.vertex_id_at(ri, i) else {
                    continue;
                };
                options.push(MeasurementOption {
                    reference_wall_vid: ref_vid,
                    reference_value: ref_value,
                    sign: delta.signum(),
                    current_distance: distance,
                });
            }
        }
        if options.is_empty() {
            // Nothing parallel to the moved wall — can't form a measurement.
            return;
        }
        self.edit_drag_prompt = Some(EditDragPrompt {
            moved_wall_vid: vid,
            axis,
            kind,
            options,
            current_idx: 0,
        });
    }

    /// Apply the user's entered distance from the current measurement
    /// reference. Updates the moved wall's parameter (or shifts its
    /// anchor constant) and clears the prompt.
    pub fn apply_edit_drag(&mut self, distance: f32) -> Result<(), String> {
        if !distance.is_finite() || distance < 0.0 {
            return Err(format!(
                "distance must be a non-negative number (got {})",
                distance
            ));
        }
        let prompt = self
            .edit_drag_prompt
            .clone()
            .ok_or_else(|| "no active edit-drag prompt".to_string())?;
        let Some(option) = prompt.options.get(prompt.current_idx).cloned() else {
            return Err("invalid measurement index".into());
        };
        let new_value = option.reference_value + option.sign * distance;
        match prompt.kind {
            MovedCoordKind::Parameter(p) => {
                self.floor_plan.set_parameter(p, new_value);
            }
            MovedCoordKind::Constant(old_value) => {
                self.floor_plan
                    .shift_constant(old_value, prompt.axis, new_value);
            }
        }
        self.commit_to_ber()?;
        self.edit_drag_prompt = None;
        self.edit_state.pre_drag_value = None;
        self.edit_state.pre_drag_kind = None;
        Ok(())
    }

    /// Advance the prompt to the next candidate measurement. Wraps around
    /// when the user reaches the end of the list. No-op if the prompt
    /// isn't active.
    pub fn cycle_edit_drag_measurement(&mut self) {
        if let Some(p) = self.edit_drag_prompt.as_mut() {
            if p.options.is_empty() {
                return;
            }
            p.current_idx = (p.current_idx + 1) % p.options.len();
        }
    }

    /// Cancel the post-drag prompt: revert the moved coord to its pre-drag
    /// value and clear the prompt. Pops the drag's undo snapshot because
    /// the action is effectively erased — no need for the user to undo
    /// after they cancelled.
    pub fn cancel_edit_drag(&mut self) -> Result<(), String> {
        if self.edit_state.drag_snapshot_taken {
            self.history.pop();
            self.edit_state.drag_snapshot_taken = false;
        }
        let (Some(pre_value), Some(kind)) = (
            self.edit_state.pre_drag_value,
            self.edit_state.pre_drag_kind,
        ) else {
            self.edit_drag_prompt = None;
            return Ok(());
        };
        let prompt_axis = self.edit_drag_prompt.as_ref().map(|p| p.axis);
        match kind {
            MovedCoordKind::Parameter(p) => {
                self.floor_plan.set_parameter(p, pre_value);
            }
            MovedCoordKind::Constant(_old_value) => {
                if let Some(axis) = prompt_axis {
                    // The current constant value may have been mutated by
                    // the drag; look it up from the moved wall to find the
                    // *current* constant, then shift back to pre_value.
                    if let Some(vid) = self.edit_state.selected_wall {
                        if let Some((ring, edge_start)) = self.floor_plan.find_vertex(vid) {
                            if let Some((current_const, _)) =
                                self.floor_plan.wall_anchor_constant(ring, edge_start)
                            {
                                self.floor_plan
                                    .shift_constant(current_const, axis, pre_value);
                            }
                        }
                    }
                }
            }
        }
        self.commit_to_ber()?;
        self.edit_drag_prompt = None;
        self.edit_state.pre_drag_value = None;
        self.edit_state.pre_drag_kind = None;
        Ok(())
    }

    /// Snapshot of the active post-drag prompt for the UI to render. Includes
    /// the moved wall + the current reference wall + the distance value to
    /// pre-populate the textbox with.
    pub fn edit_drag_prompt_for_ui(&self) -> Option<EditDragPromptUiData> {
        let prompt = self.edit_drag_prompt.as_ref()?;
        let option = prompt.options.get(prompt.current_idx)?;
        let (moved_ring, moved_edge) = self.floor_plan.find_vertex(prompt.moved_wall_vid)?;
        let (ref_ring, ref_edge) = self.floor_plan.find_vertex(option.reference_wall_vid)?;
        let polygons = self.floor_plan.polygons();
        let moved_ring_pts = polygons.get(moved_ring)?;
        let ref_ring_pts = polygons.get(ref_ring)?;
        let moved_a = moved_ring_pts.get(moved_edge)?;
        let moved_b = moved_ring_pts.get((moved_edge + 1) % moved_ring_pts.len())?;
        let ref_a = ref_ring_pts.get(ref_edge)?;
        let ref_b = ref_ring_pts.get((ref_edge + 1) % ref_ring_pts.len())?;
        Some(EditDragPromptUiData {
            moved_wall: WallPositions {
                wall_a: (moved_a.x as f64, moved_a.y as f64),
                wall_b: (moved_b.x as f64, moved_b.y as f64),
            },
            reference_wall: WallPositions {
                wall_a: (ref_a.x as f64, ref_a.y as f64),
                wall_b: (ref_b.x as f64, ref_b.y as f64),
            },
            current_distance: option.current_distance,
            option_idx: prompt.current_idx,
            option_count: prompt.options.len(),
        })
    }

    /// Snapshot of the current dimension prompt for the UI to render, or
    /// `None` when nothing is being asked.
    pub fn current_dimension_prompt(&self) -> Option<DimensionPrompt> {
        self.dimensioning.current_prompt.clone()
    }

    /// True when there's an active dimension prompt waiting for the user.
    pub fn dimensioning_active(&self) -> bool {
        self.dimensioning.current_prompt.is_some()
    }

    /// True once the user has entered at least one dimension. Until then the
    /// polygon is in pixel space; after the first dimension lands, everything
    /// converts to real units in one scale_all() pass.
    pub fn dimensioning_scale_set(&self) -> bool {
        self.dimensioning.scale_set
    }

    /// Resolved positions (ring index, edge-start vertex index) of every wall
    /// the user has explicitly dimensioned. The UI uses this to decide which
    /// wall length labels to render — derived/un-set walls show no label.
    pub fn dimensioned_walls(&self) -> Vec<(usize, usize)> {
        self.dimensioning
            .dimensioned_walls
            .iter()
            .filter_map(|vid| self.floor_plan.find_vertex(*vid))
            .collect()
    }

    /// Every wall whose length is currently *known* — meaning all parameters
    /// in its length expression are in `known_params`. This is the superset
    /// of `dimensioned_walls`: it also catches walls whose length is implied
    /// by equality (e.g. the opposite side of a rectangle, after one side
    /// was dimensioned) or by sums/differences of already-set parameters.
    /// The UI uses this to label every wall whose length can be displayed.
    pub fn walls_with_known_length(&self) -> Vec<(usize, usize)> {
        let polygons = self.floor_plan.polygons();
        let known = &self.dimensioning.known_params;
        let mut result = Vec::new();
        for (ri, ring) in polygons.iter().enumerate() {
            for i in 0..ring.len() {
                let deps = self.floor_plan.wall_param_deps(ri, i);
                // Skip walls with no param deps — pre-first-dim, those are
                // pixel-space artifacts and labeling them in meters would be
                // misleading. Once at least one dim is set, scale_set is true.
                if deps.is_empty() {
                    continue;
                }
                if deps.iter().all(|p| known.contains(p)) {
                    result.push((ri, i));
                }
            }
        }
        result
    }

    /// Resolved view of the current prompt for the UI: world-coord endpoints
    /// of the primary wall + all length-equivalent walls + current length.
    /// None when there's no active prompt.
    pub fn dimension_prompt_for_ui(&self) -> Option<DimensionPromptUiData> {
        let prompt = self.dimensioning.current_prompt.as_ref()?;
        let (ring, edge_start) = self.floor_plan.find_vertex(prompt.wall_start_vertex)?;
        let polygons = self.floor_plan.polygons();
        let current_length = self.floor_plan.wall_length(ring, edge_start)?;

        // Equivalent walls = primary + any other walls with the same length
        // expression. Ensure the primary is first.
        let mut equiv = self.floor_plan.equivalent_length_walls(ring, edge_start);
        if let Some(pos) = equiv.iter().position(|w| *w == (ring, edge_start)) {
            if pos != 0 {
                equiv.swap(0, pos);
            }
        } else {
            equiv.insert(0, (ring, edge_start));
        }

        let walls: Vec<WallPositions> = equiv
            .into_iter()
            .filter_map(|(r, e)| {
                let pts = polygons.get(r)?;
                let nr = pts.len();
                if nr < 2 {
                    return None;
                }
                let pa = pts.get(e)?;
                let pb = pts.get((e + 1) % nr)?;
                Some(WallPositions {
                    wall_a: (pa.x as f64, pa.y as f64),
                    wall_b: (pb.x as f64, pb.y as f64),
                })
            })
            .collect();

        if walls.is_empty() {
            return None;
        }
        Some(DimensionPromptUiData {
            walls,
            current_length,
        })
    }

    /// Apply the user's entered length for the current dimension prompt.
    /// First entry: scale the whole plan so this wall has that length.
    /// Subsequent entries: set the prompt's controlled parameter directly.
    pub fn apply_dimension(&mut self, value: f32) -> Result<(), String> {
        if !value.is_finite() || value <= 0.0 {
            return Err(format!(
                "dimension must be a positive number (got {})",
                value
            ));
        }
        // Snapshot for undo. Only pushed at the very end on success — any
        // early `return Err(...)` (including internal rollbacks) discards
        // the snapshot.
        let undo_snap = self.current_history_snapshot();
        let prompt = self
            .dimensioning
            .current_prompt
            .clone()
            .ok_or_else(|| "no active dimension prompt".to_string())?;

        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        let pre_length = self.floor_plan.wall_length(ring, edge_start);
        println!(
            "dimension #{}: ring {}/edge {} (vertex_id {}, param P{}) → {} m  [was {:.3}, scale_set={}]",
            self.dimensioning.dimensioned_walls.len() + 1,
            ring,
            edge_start,
            prompt.wall_start_vertex,
            prompt.controlled_param,
            value,
            pre_length.unwrap_or(f32::NAN),
            self.dimensioning.scale_set,
        );

        // Snapshot so we can roll back if the dimension produces an invalid
        // (self-intersecting / collapsed / direction-flipped) polygon. The
        // parametric model alone doesn't enforce "all right-side vertical
        // segments sum to the spine height" — that's an emergent property
        // of the polygon being simple. Likewise it doesn't enforce that
        // edges keep going in the same direction; a parameter change can
        // push a coord past a fixed boundary and flip an edge without
        // producing a self-intersection.
        let snapshot = self.floor_plan.clone();
        let scale_set_before = self.dimensioning.scale_set;
        let known_params_before = self.dimensioning.known_params.clone();
        let dimensioned_walls_before = self.dimensioning.dimensioned_walls.clone();
        let edge_signs_before = snapshot.edge_signs();

        // Optimistically mark the just-set param as known up front. The repair
        // logic only scales params NOT in `known_params`; if we delayed the
        // insert until after the validate+repair, the repair would treat the
        // user's param as un-set and scale it down — both clobbering the
        // user's input and preventing convergence (the just-set wall and a
        // sibling wall scale in lockstep, never resolving their overlap).
        // On full rejection we'll roll back to `known_params_before`.
        self.dimensioning
            .known_params
            .insert(prompt.controlled_param);
        self.dimensioning
            .dimensioned_walls
            .push(prompt.wall_start_vertex);

        if !self.dimensioning.scale_set {
            // First dimension — convert the whole plan from pixels to real units
            // in one stroke by scaling every parameter and constant uniformly.
            let pixel_length = self
                .floor_plan
                .wall_length(ring, edge_start)
                .ok_or_else(|| "couldn't read wall length".to_string())?;
            if pixel_length < 1e-6 {
                return Err("wall has zero length, can't compute scale".into());
            }
            let scale = value / pixel_length;
            println!("  first dimension — scaling whole plan by {:.6}", scale);
            self.floor_plan.scale_all(scale);
            self.dimensioning.scale_set = true;
        } else {
            // Subsequent dimensions — adjust just the controlled parameter so
            // the wall takes the new length, preserving its direction along axis.
            if let Err(e) =
                self.floor_plan
                    .set_wall_length(ring, edge_start, value, prompt.controlled_param)
            {
                self.floor_plan = snapshot;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                println!("  → REJECTED (set_wall_length error): {:?}", e);
                return Err(format!("set_wall_length: {:?}", e));
            }
            println!(
                "  set_wall_length: P{} = {:?}",
                prompt.controlled_param,
                self.floor_plan.parameter_value(prompt.controlled_param)
            );
        }

        // If the apply produced an invalid polygon, try to repair it by
        // scaling the un-set parameters on the same axis as the dimensioned
        // wall — the un-set walls are still "free" (their original pixel
        // proportions were placeholders), so we can move them out of the way
        // to accommodate the user's input. E.g. when the bottom-arm height
        // of an E grows past where the middle arm currently sits, shrink the
        // middle arm's un-set y-coords toward y=0 to make room.
        //
        // Only if no factor produces a valid polygon do we reject the input
        // outright.
        if let Err(initial_err) = self.floor_plan.validate_simple() {
            let dim_axis = self
                .floor_plan
                .param_axis(prompt.controlled_param)
                .unwrap_or(ParameterAxis::Y);
            println!(
                "  polygon invalid post-apply ({:?}) — attempting repair on axis {:?}",
                initial_err, dim_axis
            );
            if !self.floor_plan.repair_unset_params(
                dim_axis,
                &self.dimensioning.known_params,
                &snapshot,
            ) {
                self.floor_plan = snapshot;
                self.dimensioning.scale_set = scale_set_before;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                println!(
                    "  → REJECTED: no repair found ({:?}). Rolled back.",
                    initial_err
                );
                return Err(format!(
                    "{} is incompatible with the shape — even shrinking the \
                     un-set walls didn't yield a simple polygon ({:?}). Try \
                     a value that's consistent with the other walls already set.",
                    value, initial_err
                ));
            }
            println!("  repair succeeded.");
        }

        // Direction-flip guard: even after a "simple" polygon validation,
        // we still need to reject cases where a parameter change pushed an
        // un-set coord past a known coord and silently inverted an edge.
        // E.g. setting the bottom arm of a T to 10m can drop its top
        // endpoint above the polygon's top edge — no edges cross, but the
        // adjacent wall now goes "the wrong way" and visually the polygon
        // has changed shape.
        let edge_signs_after = self.floor_plan.edge_signs();
        if let Some((r, e)) = find_flipped_edge(&edge_signs_before, &edge_signs_after) {
            self.floor_plan = snapshot;
            self.dimensioning.scale_set = scale_set_before;
            self.dimensioning.known_params = known_params_before;
            self.dimensioning.dimensioned_walls = dimensioned_walls_before;
            println!(
                "  → REJECTED: ring {}/edge {} would flip direction. Rolled back.",
                r, e
            );
            return Err(format!(
                "{} would invert ring {}/edge {} (its direction would flip — \
                 that's a shape change, not a dimension adjustment). Try a \
                 value that's consistent with the other walls already set.",
                value, r, e
            ));
        }

        // known_params already updated optimistically above.
        // Apply an aesthetic nudge if the polygon's aspect got extreme from
        // this dimension. Only touches un-set parameters; user's set values
        // stay exact.
        self.floor_plan
            .aesthetic_rescale(&self.dimensioning.known_params);
        self.commit_to_ber()?;
        let post_length = self.floor_plan.wall_length(ring, edge_start);
        println!(
            "  ✓ accepted. wall now {:.3} m. known params: {:?}. dimensioned walls: {}/{}",
            post_length.unwrap_or(f32::NAN),
            {
                let mut k: Vec<_> = self.dimensioning.known_params.iter().copied().collect();
                k.sort();
                k
            },
            self.dimensioning.dimensioned_walls.len(),
            self.floor_plan.all_active_params().len(),
        );
        self.compute_next_dimension_prompt();
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// Skip the current dimension prompt without entering a value. The
    /// specific WALL is suppressed from future prompts; the param it would
    /// have set remains un-set — but other walls (if any) can still set it.
    pub fn skip_current_dimension(&mut self) {
        self.record_history();
        if let Some(prompt) = self.dimensioning.current_prompt.take() {
            self.dimensioning
                .skipped_walls
                .insert(prompt.wall_start_vertex);
            let (ring, edge) = self
                .floor_plan
                .find_vertex(prompt.wall_start_vertex)
                .unwrap_or((0, 0));
            println!(
                "wall skipped: ring {}/edge {} (would have set P{}). \
                 The heuristic will continue looking for other walls.",
                ring, edge, prompt.controlled_param
            );
            self.compute_next_dimension_prompt();
        }
    }

    /// Recompute `current_prompt` based on which params are still unknown.
    /// Picks the first wall whose length depends on exactly one unknown
    /// parameter; clears the prompt when every active param is known.
    fn compute_next_dimension_prompt(&mut self) {
        // A wall is promptable when:
        //   - It's not in `skipped_walls` (the user already passed on it).
        //   - Its length depends on exactly one parameter not yet in
        //     `known_params`, with the rest of its deps all known.
        //
        // Skip is per-WALL, not per-param. Skipping wall W (whose controlled
        // param would have been P) doesn't preclude prompting wall W' that
        // also sets P — the user might prefer to measure W' instead. If no
        // other wall can cleanly set P, P stays un-dimensioned but the
        // user is still asked about every other param via walls that don't
        // depend on P.
        let active = self.floor_plan.all_active_params();
        let known = self.dimensioning.known_params.clone();
        let to_ask: HashSet<ParameterId> =
            active.into_iter().filter(|p| !known.contains(p)).collect();
        if to_ask.is_empty() {
            self.dimensioning.current_prompt = None;
            return;
        }

        let polygons = self.floor_plan.polygons();
        for (ring_idx, ring_points) in polygons.iter().enumerate() {
            for edge_start in 0..ring_points.len() {
                // Skip walls the user has already passed on.
                let Some(vid) = self.floor_plan.vertex_id_at(ring_idx, edge_start) else {
                    continue;
                };
                if self.dimensioning.skipped_walls.contains(&vid) {
                    continue;
                }
                // A wall is promptable iff exactly one of its deps is in
                // `to_ask` and the rest are in `known`. Deps that aren't in
                // either set (e.g. params whose only wall was skipped and
                // never set) block this wall from being promptable.
                let deps = self.floor_plan.wall_param_deps(ring_idx, edge_start);
                let mut to_ask_count = 0usize;
                let mut to_ask_param: Option<ParameterId> = None;
                let mut blocked = false;
                for d in &deps {
                    if to_ask.contains(d) {
                        to_ask_count += 1;
                        to_ask_param = Some(*d);
                    } else if !known.contains(d) {
                        // dep is neither known nor in to_ask — shouldn't
                        // happen, but be defensive.
                        blocked = true;
                        break;
                    }
                }
                if blocked {
                    continue;
                }
                if to_ask_count == 1 {
                    self.dimensioning.current_prompt = Some(DimensionPrompt {
                        wall_start_vertex: vid,
                        controlled_param: to_ask_param.unwrap(),
                    });
                    return;
                }
            }
        }
        // No promptable wall at all — every to_ask param is reachable only
        // through walls the user has skipped, or via walls touching multiple
        // un-set params. The remaining params stay un-dimensioned.
        self.dimensioning.current_prompt = None;
    }

    /// Kick off dimensioning if nothing else is in the way (no pending redraw,
    /// no in-flight collinearity review). No-op otherwise.
    fn maybe_start_dimensioning(&mut self) {
        if self.review.is_some()
            || self.pending_redraw.is_some()
            || self.dimensioning.current_prompt.is_some()
        {
            return;
        }
        // Collapse any zero-length edges + collinear vertices that the
        // collinearity review may have introduced (e.g. when the user marked
        // a top-left nook's edges as collinear with the polygon's outer top
        // edge, the nook's vertices end up at the same point as their
        // neighbours). Done before dimensioning starts so we don't prompt
        // for dimensions on degenerate walls. Only safe while scale isn't
        // set — afterwards the parametric IDs are referenced by known_params
        // and a rebuild would dangle them.
        if !self.dimensioning.scale_set {
            self.floor_plan.simplify_degenerate();
            if let Err(e) = self.commit_to_ber() {
                eprintln!("commit_to_ber after simplify_degenerate: {}", e);
            }
        }
        self.compute_next_dimension_prompt();
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
        let stroke_opt = if matches!(edit_tool, EditTools::Extend) {
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

/// Snap V to satisfy a single measurement. The other coord defaults to the
/// current vertex position (axis case) or is computed from the circle's
/// closest-point projection (wall-length case).
fn snap_single(
    m: &VertexMeasurement,
    distance: f32,
    n_pos: (f32, f32),
    cur: (f32, f32),
) -> (f32, f32) {
    match m.kind {
        VertexMeasurementKind::Axis(ParameterAxis::X) => (n_pos.0 + m.sign * distance, cur.1),
        VertexMeasurementKind::Axis(ParameterAxis::Y) => (cur.0, n_pos.1 + m.sign * distance),
        VertexMeasurementKind::WallLength => project_to_circle(cur, n_pos, distance),
    }
}

/// Solve for V given two measurements jointly. Handles every combination
/// of axis / wall-length constraints. Falls back to overriding the prior
/// constraint when the two are linearly dependent (e.g. two X axes) or
/// when the system is infeasible (circles don't reach a fixed coord).
fn joint_solve(
    m1: &VertexMeasurement,
    d1: f32,
    n1: (f32, f32),
    m2: &VertexMeasurement,
    d2: f32,
    n2: (f32, f32),
    cur: (f32, f32),
) -> (f32, f32) {
    use VertexMeasurementKind::*;
    match (m1.kind, m2.kind) {
        (Axis(ParameterAxis::X), Axis(ParameterAxis::Y)) => {
            (n1.0 + m1.sign * d1, n2.1 + m2.sign * d2)
        }
        (Axis(ParameterAxis::Y), Axis(ParameterAxis::X)) => {
            (n2.0 + m2.sign * d2, n1.1 + m1.sign * d1)
        }
        (Axis(a1), Axis(a2)) if std::mem::discriminant(&a1) == std::mem::discriminant(&a2) => {
            // Same axis applied twice — second overrides, other coord stays.
            snap_single(m2, d2, n2, cur)
        }
        (Axis(_), Axis(_)) => snap_single(m2, d2, n2, cur),
        (Axis(axis), WallLength) => solve_axis_wall(axis, m1.sign, d1, n1, n2, d2, cur),
        (WallLength, Axis(axis)) => solve_axis_wall(axis, m2.sign, d2, n2, n1, d1, cur),
        (WallLength, WallLength) => intersect_circles(n1, d1, n2, d2, cur),
    }
}

/// Statistics about a floor plan
#[derive(Debug, Clone)]
pub struct FloorPlanStats {
    pub perimeter: f32,
    pub area: f32,
    pub num_vertices: usize,
}
