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

pub use document::ExposureSection;
use document::{
    ActiveShape, DimensionPrompt, DimensionState, Document, EditState, MovedCoordKind, PendingArc,
    PhaseRegion, ShapeSlot, StoreyDoc, DEFAULT_CEILING_HEIGHT_M,
};
pub use exposure::{ExposureSelection, SectionHandle, WallExposureView};
// `ExposureSection` is re-exported above (the controller's public surface).

use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository};
use crate::engine::dataloader::DataLoader;
use crate::engine::Db;
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::{ConstructionPhase, ConstructionPhases, ItemId};
use datatypes::ber::structural::{FloorRegions, Storey, WallExposure, WallRegions};
use domain::points::{to_f32, to_f64, Point2f32, Point2f64};
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_2d_plan::{
    Axis, CollinearitySuggestion, FloorPlan, ParameterAxis, SubareaOutcome, VertexId,
};
use sketch::model::Stroke;
use sketch::SketchEngine;
use std::collections::{HashMap, HashSet};

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

// There is NO persisted sketch snapshot. The DB stores only finalised
// (complete, metre-scale) resolved geometry — `Storey.floors` (outline),
// `Storey.living_areas`, and `Storey.walls` (exposure). On reload the editor
// rebuilds everything from those resolved points (see `load_storey_into_editor`):
// the outline's parametric model is re-derived (re-edit fidelity — shared
// params, collinearity equivalences, curve arcs — is deliberately not
// preserved), living areas are rebuilt non-editable, and exposure is recovered
// from the wall regions.

/// Format version of the editing-layer snapshot (`Document` serde). Bump when
/// the serialized shape changes incompatibly — old snapshots are then ignored on
/// load and the editor rebuilds from the resolved BER points instead.
// v2: snapshot is now a multi-storey `Document` ({ storeys, active_storey })
// rather than a single storey's flat fields.
const PARAMETRIC_SNAPSHOT_VERSION: i64 = 2;

pub struct AppController {
    conn: Db,
    sketch_engine: SketchEngine,
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
    /// The editable storey model: the outline + its living areas + an in-flight
    /// phase area, which one is active, AND the live editing buffer (the active
    /// shape checked out — `floor_plan`/`dimensioning`/`edit_state`/`pending_arc`).
    /// `sync_active`/`load_active_into_buffer` move state between the buffer and
    /// the slots. This is the UI-free, DB-free model the controller edits.
    document: Document,
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
    /// A phase cut split the storey into two candidate regions (each a
    /// dimensionable slot); the user taps one, which becomes `phase_shape` and is
    /// then dimensioned. Transient.
    pending_phase_cut: Option<[ShapeSlot; 2]>,
    /// DEBUG: the splice-pipeline phases (#1 raw stroke … #4 spliced ring) from
    /// the last extend/trim, captured for the UI's coloured debug overlay.
    splice_debug: Vec<manip_2d_plan::SplicePhase>,
    /// Edit counter, bumped on every in-memory BER refresh (`commit_to_ber`).
    /// `revision != saved_revision` means there are un-persisted edits. The UI
    /// uses `revision` for idle detection; `flush_if_dirty` writes to disk and
    /// catches `saved_revision` up. This is what decouples the (cheap, per-edit)
    /// in-memory refresh from the (costly) SQLite write.
    revision: u64,
    saved_revision: u64,
    /// The `revision` the in-memory `current_record` geometry was last DERIVED
    /// at. `revision != derived_revision` means an edit happened without a
    /// re-derive; `materialize` brings it current on demand (before render /
    /// persist) instead of every edit deriving eagerly.
    derived_revision: u64,
    /// Optional background writer (the real app attaches one; tests don't). When
    /// present, `autosave` hands the disk write to it off the UI thread; when
    /// absent, `autosave` falls back to a synchronous `flush_if_dirty`.
    writer: Option<crate::engine::PersistenceWriter>,
    /// The last snapping action (its raw input + pre-action state + the elective
    /// snaps it made), kept so a forced snap choice can REPLAY it — restore the
    /// pre-state, re-run under the policy, rebuild the shape. `None` until the
    /// first snapping action.
    last_snap_action: Option<LastSnapAction>,
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

/// Snapshot of the editable state for undo/redo: the whole `Document` (shapes +
/// live editing buffer + exposure) plus the living-area mode flag. View
/// (zoom/pan), in-flight prompts, and drag state are deliberately excluded so
/// undo doesn't fight the user's current view or interrupt a prompt mid-flow.
#[derive(Debug, Clone)]
pub struct HistorySnapshot {
    /// The whole editable document. Undo is a `Document` clone/restore; this is
    /// the seam the snapshot has always followed (it once listed these fields by
    /// hand — floor plan, dimensioning, edit state, arc, shapes, exposure).
    document: Document,
    /// The living-area MODE flag (a UI tab toggle) is session state kept OUT of
    /// `Document`, but undo restores it so a draw/undo also round-trips the tab.
    living_area_mode: bool,
}

/// A replayable snapping action: the raw stroke (plus enough context to re-run
/// it from scratch) under a forced snap policy.
#[derive(Debug, Clone)]
enum SnapAction {
    Freehand {
        tool: EditTools,
        points: Vec<Point2f32>,
        closed: bool,
    },
    /// A new-storey outline traced in world (metre) space over the previous
    /// storey's watermark — replayed through the carve path
    /// (`apply_outline_world_inner`).
    OutlineWorld {
        points: Vec<(f32, f32)>,
        closed: bool,
    },
}

/// The last snapping action, kept for replay: the action's input, the document
/// state BEFORE it ran, and the elective snaps it made (each addressable by its
/// `from` point for the forced-choice policy).
struct LastSnapAction {
    action: SnapAction,
    before: HistorySnapshot,
    decisions: Vec<manip_2d_plan::SnapDecision>,
}

/// One clickable elective-snap tag for the canvas overlay (a projection of a
/// recorded `SnapDecision`). The UI draws a marker at `pos`, labels it with
/// `current_kind`, and on click offers `options`; picking one sends `from` back
/// to [`AppController::choose_snap`].
#[derive(Debug, Clone)]
pub struct SnapTagView {
    /// World position to draw the tag (the snapped point, or the drawn point if
    /// currently un-snapped).
    pub pos: (f32, f32),
    /// The decision's drawn point — its stable identity for `choose_snap`.
    pub from: (f32, f32),
    /// The applied kind, or `None` when this snap is currently disabled.
    pub current_kind: Option<manip_2d_plan::SnapKind>,
    /// Candidates to offer on click: `(kind, apply-index)`, most-aggressive first.
    pub options: Vec<(manip_2d_plan::SnapKind, usize)>,
    /// Whether "don't snap" is a valid choice (false for a mandatory extend/trim
    /// attach — the menu then omits it).
    pub skippable: bool,
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
            sketch_engine: SketchEngine::new(),
            current_record: None,
            current_storey_idx: None,
            review: None,
            pending_redraw: None,
            document: Document::default(),
            living_area_mode: false,
            exposure_mode: false,
            selected_exposure_wall: None,
            exposure_sel: None,
            exposure_drag: None,
            pending_cut: None,
            phase_mode: false,
            phase_regions: HashMap::new(),
            pending_phase_cut: None,
            splice_debug: Vec::new(),
            revision: 0,
            saved_revision: 0,
            derived_revision: 0,
            writer: None,
            last_snap_action: None,
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
            document: self.document.clone(),
            living_area_mode: self.living_area_mode,
        }
    }

    fn restore_history_snapshot(&mut self, snap: HistorySnapshot) {
        self.document = snap.document;
        self.living_area_mode = snap.living_area_mode;
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

    /// After a snapping action, capture it for replay: drain the elective snaps it
    /// made (recorded in `manip`), and if any, remember the action + its
    /// pre-state (the undo entry it just pushed). No snaps → nothing to replay.
    fn capture_snap_action(&mut self, action: SnapAction) {
        let decisions = manip_2d_plan::take_snap_decisions();
        if decisions.is_empty() {
            return;
        }
        let Some(before) = self.history.last().cloned() else {
            return;
        };
        self.last_snap_action = Some(LastSnapAction {
            action,
            before,
            decisions,
        });
    }

    /// Re-run a captured snapping action from its raw input (used by replay).
    fn dispatch_snap_action(&mut self, action: &SnapAction) {
        let r = match action {
            SnapAction::Freehand {
                tool,
                points,
                closed,
            } => {
                // Restore the tool so the inner apply_* dispatches the same way it did.
                self.sketch_engine.edit_tool = *tool;
                match tool {
                    EditTools::DrawPerpWalls => self.apply_drawperp_stroke(points, *closed),
                    EditTools::AddDetachedSection => self.apply_addsection_stroke(points, *closed),
                    EditTools::ExtendOrtho | EditTools::ExtendCurve => {
                        self.apply_extend_stroke(points)
                    }
                    EditTools::Trim | EditTools::TrimOrtho | EditTools::TrimCurve => {
                        self.apply_trim_stroke(points)
                    }
                    _ => Ok(()),
                }
            }
            SnapAction::OutlineWorld { points, closed } => {
                self.apply_outline_world_inner(points, *closed)
            }
        };
        if let Err(e) = r {
            eprintln!("[snap replay] re-run failed: {e}");
        }
    }

    /// Replay the last snapping action under a forced-choice snap policy: rewind to
    /// the pre-action state, re-run from the raw stroke with the overrides, and
    /// rebuild. Re-captures so it can be replayed again.
    fn replay_last_snap_action(&mut self, policy: Vec<(Point2f32, manip_2d_plan::SnapChoice)>) {
        let Some(last) = self.last_snap_action.take() else {
            return;
        };
        // Drop the original action's undo entry (the re-run pushes its own), rewind
        // the document, then re-run under the policy.
        self.history.pop();
        self.restore_history_snapshot(last.before);
        manip_2d_plan::set_snap_policy(policy);
        let _ = manip_2d_plan::take_snap_decisions(); // clear stale
        self.dispatch_snap_action(&last.action);
        manip_2d_plan::clear_snap_policy();
        // Re-capture (the direct dispatch bypassed the normal capture point).
        let decisions = manip_2d_plan::take_snap_decisions();
        if let Some(before) = self.history.last().cloned() {
            self.last_snap_action = Some(LastSnapAction {
                action: last.action,
                before,
                decisions,
            });
        }
    }

    /// One clickable snap tag for the canvas overlay: where to draw it, its
    /// current kind (`None` = un-snapped), the alternatives to offer on click, and
    /// the `from` that identifies it back to [`choose_snap`](Self::choose_snap).
    pub fn snap_overlay(&self) -> Vec<SnapTagView> {
        self.last_snap_action.as_ref().map_or(Vec::new(), |a| {
            a.decisions
                .iter()
                .map(|d| {
                    // Tag sits at the snapped point, or the drawn point if skipped.
                    let pos = d.chosen.map_or(d.from, |i| d.candidates[i].to);
                    SnapTagView {
                        pos: (pos.x, pos.y),
                        from: (d.from.x, d.from.y),
                        current_kind: d.chosen.map(|i| d.candidates[i].kind),
                        options: d
                            .candidates
                            .iter()
                            .enumerate()
                            .map(|(i, c)| (c.kind, i))
                            .collect(),
                        skippable: d.skippable,
                    }
                })
                .collect()
        })
    }

    /// Apply a per-snap choice to the snap at `from` and replay: `Some(i)` forces
    /// candidate `i`, `None` un-snaps it. Other snaps of the action keep their
    /// current choice. Drives the tag UI's pick.
    pub fn choose_snap(&mut self, from: Point2f32, choice: Option<usize>) {
        let Some(last) = &self.last_snap_action else {
            return;
        };
        // Build a policy that preserves every other snap's current choice and
        // overrides just this one.
        let policy: Vec<(Point2f32, manip_2d_plan::SnapChoice)> = last
            .decisions
            .iter()
            .map(|d| {
                let c = if (d.from.x - from.x).abs() < 1e-3 && (d.from.y - from.y).abs() < 1e-3 {
                    choice
                } else {
                    d.chosen
                };
                let sc = match c {
                    Some(i) => manip_2d_plan::SnapChoice::Apply(i),
                    None => manip_2d_plan::SnapChoice::Skip,
                };
                (d.from, sc)
            })
            .collect();
        self.replay_last_snap_action(policy);
    }

    /// Replay the last snapping action with EVERY elective snap disabled — revert
    /// to the freehand points. (Test hook for the replay; the tag UI will later
    /// drive `replay_last_snap_action` with per-snap choices.)
    pub fn unsnap_last(&mut self) {
        let Some(last) = &self.last_snap_action else {
            return;
        };
        let policy: Vec<(Point2f32, manip_2d_plan::SnapChoice)> = last
            .decisions
            .iter()
            .map(|d| (d.from, manip_2d_plan::SnapChoice::Skip))
            .collect();
        self.replay_last_snap_action(policy);
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
        self.mark_dirty()?;
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
        self.mark_dirty()?;
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
        // Boundary flush: switching records persists any pending edits on the
        // current one first, so the idle-deferred writes aren't lost when the
        // assessor opens a different (or the same) BER.
        let _ = self.flush_if_dirty();
        let record = BerRepository::new(&self.conn)
            .load(id)
            .map_err(|e| format!("failed to load BER: {}", e))?;
        self.current_record = Some(record);
        // Rebuild ONE StoreyDoc per storey from its resolved points — the fallback
        // when there's no compatible snapshot. Grow the container to N storeys,
        // then rebuild each in place (so multi-storey records stay fully editable
        // even pre-snapshot). A reopened BER lands on storey 0; a snapshot may move
        // it to the storey the assessor was last editing (see `try_restore_snapshot`).
        let n = self
            .current_record
            .as_ref()
            .unwrap()
            .data
            .storeys
            .len()
            .max(1);
        self.document = Document::default();
        for _ in 1..n {
            self.document.push_storey();
        }
        for i in 0..n {
            self.document.set_active_storey(i);
            let storey = self
                .current_record
                .as_ref()
                .unwrap()
                .data
                .storeys
                .get(i)
                .cloned();
            self.load_storey_into_editor(storey.as_ref());
        }
        self.document.set_active_storey(0);
        self.current_storey_idx = Some(0);
        // Restore the exact in-progress editing state (every storey's shared
        // params / collinearity / curves + un-finalised work the resolved record
        // can't hold) from the snapshot; falls back silently to the rebuild above
        // if it's missing or from an incompatible version.
        self.try_restore_snapshot(id);
        let idx = self.current_storey_idx.unwrap_or(0);
        // Phase areas are session-only — rebuild them from the loaded storey's
        // tagged floor regions so the partition stays reproducible.
        self.phase_regions.clear();
        self.reconstruct_phase_regions_for(idx);
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.review = None;
        self.compute_next_dimension_prompt();
        self.history.clear();
        self.redo.clear();
        // Freshly loaded → nothing pending against disk.
        self.saved_revision = self.revision;
        Ok(())
    }

    /// Replace the rebuilt editor state with the editing-layer snapshot for `id`,
    /// if one exists and matches the current format version. Silently keeps the
    /// rebuild on a missing / stale / unparseable snapshot (the resolved record
    /// stays the durable source of truth).
    fn try_restore_snapshot(&mut self, id: &ItemId) {
        let Ok(Some((version, storey_idx, json))) =
            BerRepository::new(&self.conn).load_snapshot(id)
        else {
            return;
        };
        if version != PARAMETRIC_SNAPSHOT_VERSION {
            return;
        }
        let Ok(mut doc) = serde_json::from_str::<Document>(&json) else {
            return;
        };
        // The snapshot is a whole multi-storey Document, written alongside the
        // resolved record — so it must describe the SAME storey count and have at
        // least one storey. On any mismatch keep the rebuild, which is always
        // consistent with `current_record`. `storey_idx` is now vestigial — the
        // active storey lives in the Document.
        let _ = storey_idx;
        let n = self
            .current_record
            .as_ref()
            .map(|r| r.data.storeys.len())
            .unwrap_or(0);
        if doc.storeys.is_empty() || doc.storeys.len() != n {
            return;
        }
        if doc.active_storey >= n {
            doc.active_storey = 0;
        }
        let idx = doc.active_storey;
        self.document = doc;
        self.current_storey_idx = Some(idx);
        // Resume on the OUTLINE (the canonical entry point, same as a fresh open).
        // The in-progress shapes are all restored in their slots; we just point
        // the live buffer back at the outline.
        self.document.active = ActiveShape::Outline;
        self.load_active_into_buffer();
    }

    /// DEV: write the current in-memory BER to the checkpoint file so the next
    /// `BER_CHECKPOINT=1 cargo run` reloads this exact plan instead of you
    /// redrawing it. No-op (error) when no BER is loaded.
    pub fn save_checkpoint(&mut self) -> Result<(), String> {
        // Flush the live editor buffer into `current_record` first, so the
        // checkpoint captures what's on screen right now.
        self.commit_to_ber()?;
        let record = self.current_record.as_ref().ok_or("no BER loaded")?;
        crate::engine::checkpoint::save(&record.data, crate::engine::checkpoint::DEFAULT_PATH)
    }

    /// DEV: load the checkpoint file (if present) into the editor and return
    /// whether it succeeded. Mirrors `open_ber`'s reconstruction. Called on
    /// startup when `BER_CHECKPOINT` is set.
    pub fn load_checkpoint(&mut self) -> bool {
        let Some(ber) = crate::engine::checkpoint::load(crate::engine::checkpoint::DEFAULT_PATH)
        else {
            return false;
        };
        let record = BerRecord {
            id: ber.id,
            name: "checkpoint".to_string(),
            date_assessed: chrono::Local::now().naive_local().date(),
            data: ber,
        };
        self.current_storey_idx = Some(0);
        self.load_storey_into_editor(record.data.storeys.first());
        self.current_record = Some(record);
        self.phase_regions.clear();
        self.reconstruct_phase_regions_for(0);
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.review = None;
        self.compute_next_dimension_prompt();
        self.history.clear();
        self.redo.clear();
        true
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
        // Derive the OUTGOING storey into `current_record` (eager — we're leaving
        // it; `commit_to_ber` syncs its buffer first).
        self.commit_to_ber()?;
        // LOSSLESS switch: the target storey's `StoreyDoc` is kept in place, so we
        // just move the active index — NO rebuild-from-resolved-points (which
        // would drop shared params / collinearity / curves). Its outline, living
        // areas, dimensioning and exposure are exactly as the assessor left them.
        if let Some(i) = target {
            self.document.set_active_storey(i);
        }
        self.current_storey_idx = target;
        // Re-derive the now-active storey on the next render/persist.
        self.mark_dirty()?;
        // Clear only TRANSIENT state — none of it carries across a switch. The
        // storey's SHAPES + dimensioning (above) are preserved.
        self.view = ViewTransform::default();
        self.document.edit_state = EditState::default();
        self.document.pending_arc = None;
        self.review = None;
        self.pending_cut = None;
        self.pending_redraw = None;
        self.edit_drag_prompt = None;
        self.vertex_drop_prompt = None;
        self.living_area_mode = false;
        self.exposure_mode = false;
        self.phase_mode = false;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        match target {
            Some(i) => {
                // Resume on the outline (canonical entry); load its slot into the
                // buffer. The storey's shapes were preserved in place above.
                self.document.active = ActiveShape::Outline;
                self.document.load_active_into_buffer();
                // Rebuild the target storey's phase areas from its tagged floor
                // regions (unless this session already has them).
                if !self.phase_regions.contains_key(&i) {
                    self.reconstruct_phase_regions_for(i);
                }
            }
            None => {
                // "All storeys" overview — no editable shape. The active storey's
                // slots stay intact (synced by `commit_to_ber`); empty only the
                // live buffer so the canvas has nothing to edit on.
                self.document.floor_plan = FloorPlan::default();
                self.document.dimensioning = DimensionState::default();
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
        // Grow the editing container in lock-step: a fresh empty StoreyDoc, made
        // active. (Invariant: document.storeys.len() == current_record storeys.)
        self.document.push_storey();
        self.current_storey_idx = Some(new_idx);
        // Start zoomed out so the previous-storey watermark doesn't fill the
        // canvas — the assessor needs room to draw a footprint that extends
        // BEYOND it (e.g. over a ground-floor garage). `Fit` resets to full-fit.
        self.view = ViewTransform {
            zoom: self.tuning.new_storey.add_storey_zoom,
            ..ViewTransform::default()
        };
        self.document.edit_state = EditState::default();
        self.review = None;
        self.document.pending_arc = None;
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
        // Shell session state resets here; the model rebuild itself is
        // `StoreyDoc::from_storey` (document) — replacing the whole StoreyDoc
        // covers the buffer, slots, active shape, pending arc and wall sections.
        // Any in-flight phase area belongs to the OUTGOING storey — abandon it.
        self.pending_phase_cut = None;
        self.living_area_mode = false;
        self.exposure_mode = false;
        // Phase mode is a sub-area tool like the two above; leaving it on across a
        // storey switch routed the new storey's OUTLINE stroke into the phase
        // carve (against an empty footprint). Reset it so the new storey starts in
        // outline mode.
        self.phase_mode = false;
        self.selected_exposure_wall = None;
        self.exposure_sel = None;
        self.exposure_drag = None;
        let idx = self.document.active_storey;
        self.document.storeys[idx] = storey.map(StoreyDoc::from_storey).unwrap_or_default();
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
        self.document.floor_plan.get_points().len() >= 3
    }

    /// Project current in-memory state into `self.current_record` and persist.
    /// No-op when no BER is loaded. Called by handler methods that complete a
    /// logical user action (stroke end, wall drag end, etc.) — not from
    /// incremental handlers like `sketch_add_point`.
    /// Derive the active storey's geometry into `current_record` (the
    /// [`document::resolve_storey`] projection + phase stamping).
    /// No revision bookkeeping — the callers (`commit_to_ber` / `materialize`)
    /// own that.
    fn refresh_storey(&mut self) {
        if self.current_record.is_none() {
            return;
        }
        // No active storey (the "All storeys" overview tab) → nothing to derive;
        // `current_record` already holds each storey's last-derived geometry.
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        // Flush the active buffer into its slot, then ensure the storey slot + its
        // constructions exist — the two `&mut` prerequisites of the otherwise
        // read-only projection (`document::resolve_storey`).
        self.sync_active();
        let regions: &[PhaseRegion] = self
            .phase_regions
            .get(&idx)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let record = self.current_record.as_mut().unwrap();
        document::ensure_storey_constructions(&mut record.data, idx, regions);
        record.data.storeys[idx] =
            document::resolve_storey(&self.document.storeys[idx], &record.data, idx, regions);
        // Floors are already phase-correct from the partition; this stamps the
        // phase onto the just-rebuilt WALLS from the same session areas.
        document::stamp_wall_phases(&mut record.data, idx, regions);
    }

    /// Mark the active storey edited: the in-memory `current_record` geometry is
    /// now stale, and there's an un-saved change. The re-derive is DEFERRED to
    /// [`materialize`](Self::materialize) (before render / persist), so editing —
    /// including mid-drag — never re-derives the whole BER per edit.
    fn mark_dirty(&mut self) -> Result<(), String> {
        self.revision = self.revision.wrapping_add(1);
        Ok(())
    }

    /// Bring `current_record`'s geometry up to date with the editing model, if an
    /// edit happened since the last derive. Called before anything READS the
    /// resolved geometry (render, persist). No-op when already current.
    pub fn materialize(&mut self) {
        if self.revision != self.derived_revision {
            self.refresh_storey();
            self.derived_revision = self.revision;
        }
    }

    /// Eagerly derive AND mark dirty (and record that we just derived). Kept for
    /// the few boundary callers that need `current_record` current immediately —
    /// e.g. saving the outgoing storey before `add_storey` switches away.
    fn commit_to_ber(&mut self) -> Result<(), String> {
        self.refresh_storey();
        self.revision = self.revision.wrapping_add(1);
        self.derived_revision = self.revision;
        Ok(())
    }

    /// Persist to SQLite only if there are un-saved edits since the last write.
    /// Called on the idle autosave tick and at session boundaries — NOT per edit.
    pub fn flush_if_dirty(&mut self) -> Result<(), String> {
        if self.revision != self.saved_revision {
            self.materialize(); // bring current_record geometry current before writing
            self.persist()?;
            self.saved_revision = self.revision;
        }
        Ok(())
    }

    /// Edit revision — bumped on every in-memory BER refresh. The UI watches this
    /// to detect activity and time the idle autosave.
    pub fn revision(&self) -> u64 {
        self.revision
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
        let regions = document::phase_regions_from_floors(&record.data, idx);
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
        let Some(out) = self.build_persist_record() else {
            return Ok(());
        };
        let repo = BerRepository::new(&self.conn);
        repo.upsert(&out)
            .map_err(|e| format!("upsert failed: {}", e))?;
        if let Some(s) = self.build_snapshot() {
            repo.save_snapshot(&s.ber_id, s.version, s.storey_idx, &s.json)
                .map_err(|e| format!("snapshot save failed: {}", e))?;
        }
        Ok(())
    }

    /// Serialize the editing layer (the live [`Document`]: parametric plans +
    /// dimensioning + slots) for crash-recovery / exact reopen of IN-PROGRESS
    /// work. `None` when no BER is open. Transient fields are `#[serde(skip)]`ped.
    fn build_snapshot(&self) -> Option<crate::engine::DocSnapshot> {
        let ber_id = self.current_record.as_ref()?.id.clone();
        let json = serde_json::to_string(&self.document).ok()?;
        Some(crate::engine::DocSnapshot {
            ber_id,
            version: PARAMETRIC_SNAPSHOT_VERSION,
            storey_idx: self.current_storey_idx.unwrap_or(0) as i64,
            json,
        })
    }

    /// Build the FILTERED record to persist (the DB never holds pixel/incomplete
    /// geometry). This is the UI-thread half of a save — the half that needs
    /// `self.document`; the resulting owned record can then be upserted inline
    /// (`persist`) or handed to the background writer (`autosave`).
    fn build_persist_record(&self) -> Option<BerRecord> {
        let record = self.current_record.as_ref()?;
        let mut out = record.clone();
        let idx = self.current_storey_idx.unwrap_or(0);
        if self.outline_finalised() {
            // Outline is metric + fully dimensioned → keep its floors/walls,
            // and persist only the living areas that are themselves done.
            let lists: Vec<Vec<Point2f64>> = self
                .document
                .living_areas
                .iter()
                .filter(|s| s.finalised())
                .map(|s| s.floor_plan.get_points().into_iter().map(to_f64).collect())
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
        Some(out)
    }

    /// Attach a background persistence writer (the real app does this; tests
    /// leave it unset and fall back to synchronous writes).
    pub fn attach_writer(&mut self, writer: crate::engine::PersistenceWriter) {
        self.writer = Some(writer);
    }

    /// Save pending edits WITHOUT blocking the UI on disk I/O: build the filtered
    /// record on this thread, then hand it to the background writer. Falls back
    /// to a synchronous `flush_if_dirty` when no writer is attached (tests).
    /// Used by the idle autosave tick.
    pub fn autosave(&mut self) -> Result<(), String> {
        if self.revision == self.saved_revision {
            return Ok(());
        }
        if self.writer.is_none() {
            return self.flush_if_dirty();
        }
        self.materialize(); // bring current_record geometry current before snapshotting it
        if let Some(out) = self.build_persist_record() {
            let bundle = crate::engine::SaveBundle {
                record: out,
                snapshot: self.build_snapshot(),
            };
            self.writer.as_ref().unwrap().save(bundle);
        }
        // Optimistic: the latest is queued. A boundary `flush_if_dirty` (open/exit)
        // re-checks against `revision`, so within-window edits aren't lost.
        self.saved_revision = self.revision;
        Ok(())
    }

    /// The outline is finalised when its metre scale is set AND no wall is left
    /// to dimension — i.e. its resolved geometry is complete and in metres.
    fn outline_finalised(&self) -> bool {
        self.document.outline.dimensioning.scale_set && self.document.outline.finalised()
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
        self.document.floor_plan.apply_collinear_suggestion(&sug);
        println!(
            "  → active_parameter_count = {}",
            self.document.floor_plan.active_parameter_count()
        );
        self.advance_review_cursor();
        let result = self.mark_dirty();
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
        if let Err(e) = self.mark_dirty() {
            eprintln!("commit_to_ber after collinearity review: {}", e);
        }
        self.maybe_start_dimensioning();
    }

    /// Check the live state to decide if a suggestion is already-resolved
    /// (no need to ask). Either the two edges are already collinear, or the
    /// pair is transitively rejected via an earlier No.
    fn review_should_skip(&self, sug: &CollinearitySuggestion) -> bool {
        if self
            .document
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
            self.document
                .floor_plan
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
                        .document
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
            self.document.edit_state = EditState::default();
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
            // Keep the raw stroke + tool so this action can be REPLAYED under a
            // forced snap policy (recorded after the dispatch, if it snapped).
            let captured = SnapAction::Freehand {
                tool: edit_tool,
                points: stroke.points.clone(),
                closed: stroke.is_closed,
            };
            match edit_tool {
                EditTools::DrawPerpWalls => {
                    // Drawing the primary outline.
                    if self.document.floor_plan.ring_count() > 0 {
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
            // If the action made elective snaps, remember it for replay/un-snap.
            self.capture_snap_action(captured);
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
        let bad = self.document.floor_plan.non_orthogonal_edges();
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
        self.mark_dirty()?;
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
        self.document.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
            .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
        println!("floor plan: {:?}", self.document.floor_plan);
        self.warn_if_non_orthogonal("draw outline");
        self.document.dimensioning = DimensionState::default();
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
        if self.document.floor_plan.ring_count() > 0 {
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
        let ring = prev.floors.footprint_ring()?;
        Some(ring.into_iter().map(to_f32).collect())
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
            // Drain any decisions the failed carve recorded so they can't be
            // misattributed to the next captured action.
            let _ = manip_2d_plan::take_snap_decisions();
            return;
        }
        // If the carve made elective snaps, remember the action so the tag
        // overlay can offer alternatives / un-snap and replay it.
        self.capture_snap_action(SnapAction::OutlineWorld {
            points: world_points.to_vec(),
            closed,
        });
    }

    /// New storey via the **living-area carve**: trace the storey below as a
    /// reference, follow it where the trace hugs it and build fresh walls where it
    /// diverges — `allow_outside` so overhangs/extensions past the old footprint
    /// are kept rather than clamped in. The carve's boundary-follow subsumes the
    /// old reproduce/splice/extension cases in one pass.
    fn apply_outline_world_inner(
        &mut self,
        world_points: &[(f32, f32)],
        _closed: bool,
    ) -> Result<(), String> {
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        let prev_ring = self
            .previous_storey_ring()
            .ok_or_else(|| "no previous storey to draw on".to_string())?;
        let undo_snap = self.current_history_snapshot();
        let (pinned, active) =
            self.document
                .carve_outline_from_previous(&pts, &prev_ring, &self.tuning.subarea)?;
        self.warn_if_non_orthogonal("draw outline (carve)");
        println!(
            "[new-storey] carved outline (reuse of living-area carve): {} verts, pinned {} of {} \
             active params ({} new wall param(s) left to dimension)",
            self.document.floor_plan.get_points().len(),
            pinned,
            active,
            active.saturating_sub(pinned),
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
        let had_outline = self.document.floor_plan.ring_count() > 0;
        if !had_outline {
            self.document.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
                .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
            // Same as DrawPerpWalls when there was nothing: reset dimensioning.
            self.document.dimensioning = DimensionState::default();
        } else {
            self.document
                .floor_plan
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
            self.document.floor_plan
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
        let _ = manip_2d_plan::take_splice_debug(); // clear stale phases
        let res = self.apply_extend_stroke(&pts);
        self.splice_debug = manip_2d_plan::take_splice_debug();
        if let Err(e) = res {
            eprintln!("apply_extend_stroke: {}", e);
        }
        self.capture_snap_action(SnapAction::Freehand {
            tool: self.sketch_engine.edit_tool,
            points: pts,
            closed: false,
        });
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
            .document
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
        self.mark_dirty()?;
        // Ask about collinearity ONLY for the porch's new walls — e.g. whether
        // a new porch front lines up with an earlier one. We never assume it;
        // and we don't re-surface pairs among the pre-existing walls. (Extend
        // uses this param-scoped review, so it can't share `commit_review_redimension`.)
        let suggestions = self
            .document
            .floor_plan
            .collinear_suggestions_for_params(&minted);
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
        let _ = manip_2d_plan::take_splice_debug(); // clear stale phases
        let res = self.apply_trim_stroke(&pts);
        // Capture the pipeline phases (recorded even when the splice is rejected)
        // for the UI's coloured debug overlay.
        self.splice_debug = manip_2d_plan::take_splice_debug();
        if let Err(e) = res {
            eprintln!("apply_trim_stroke: {}", e);
        }
        self.capture_snap_action(SnapAction::Freehand {
            tool: self.sketch_engine.edit_tool,
            points: pts,
            closed: false,
        });
    }

    /// DEBUG: the splice-pipeline phases captured from the last extend/trim, as
    /// `(label, world points, closed)` per phase — for the UI's coloured overlay.
    pub fn splice_debug_overlay(&self) -> Vec<(String, Vec<(f32, f32)>, bool)> {
        self.splice_debug
            .iter()
            .map(|ph| {
                let pts = ph.points.iter().map(|p| (p.x, p.y)).collect();
                (ph.label.clone(), pts, ph.closed)
            })
            .collect()
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
            EditTools::TrimOrtho => self.document.floor_plan.trim_ortho(points),
            _ => self.document.floor_plan.trim_by_stroke(points),
        }
        .map_err(|e| format!("{}", e))?;
        println!("Trim: {} new param(s) {:?}", minted.len(), minted);
        self.clean_envelope_collinear();
        // History snapshotted before the commit (consistent across apply_*). A
        // plain Trim never asks about collinearity, so no review step here.
        self.push_history_snapshot(undo_snap);
        self.mark_dirty()?;
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
        let removed = self
            .document
            .floor_plan
            .remove_collinear_vertices(&preserve);
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
        let undo_snap = self.current_history_snapshot();
        let (start, end, bulge_ratio) = self
            .document
            .floor_plan
            .begin_arc(points)
            .map_err(|e| format!("{}", e))?;
        // Don't prompt the straight placeholder edge — only its attach stubs.
        if let Some(skip) = self.document.floor_plan.adjacent_edge_start(start, end) {
            self.document.dimensioning.skipped_walls.insert(skip);
        }
        self.document.pending_arc = Some(PendingArc {
            start,
            end,
            bulge_ratio,
        });
        println!(
            "Curve: provisional arc (bulge ratio {:.3}); dimension the two attach lines to bake it in",
            bulge_ratio
        );
        self.resync_dimensioning_to_plan();
        self.mark_dirty()?;
        self.maybe_start_dimensioning();
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// The provisional curve's current bulge (signed sagitta / chord), but only
    /// once it's ready to have its radius tuned: a pending arc whose attach
    /// lines are all set, with no dimension prompt outstanding. The UI binds a
    /// radius slider to this; `None` hides the slider (still drawing /
    /// dimensioning the attach lines).
    pub fn pending_arc_bulge(&self) -> Option<f32> {
        let pa = self.document.pending_arc.as_ref()?;
        if self.document.dimensioning.current_prompt.is_some()
            || !self.document.pending_arc_ready(pa)
        {
            return None;
        }
        Some(pa.bulge_ratio)
    }

    /// Set the provisional curve's bulge from the radius slider (clamped). Just
    /// updates the amber preview; commit happens on accept. No-op with no arc.
    pub fn set_pending_arc_bulge(&mut self, ratio: f32) {
        if let Some(pa) = self.document.pending_arc.as_mut() {
            pa.bulge_ratio = ratio.clamp(-1.5, 1.5);
        }
    }

    /// Accept the provisional curve at its current radius: facet the arc into
    /// the plan and LOCK the facet params (so later dimensioning of other walls
    /// can't let `repair`/`aesthetic_rescale` redistribute the facet vertices
    /// and flatten the curve). Clears the pending arc. If faceting fails the arc
    /// is dropped (the straight placeholder remains) rather than left stuck.
    pub fn commit_pending_arc(&mut self) -> Result<(), String> {
        let Some(pa) = self.document.pending_arc.clone() else {
            return Ok(());
        };
        let undo_snap = self.current_history_snapshot();
        let facet_params =
            match self
                .document
                .floor_plan
                .finish_arc(pa.start, pa.end, pa.bulge_ratio)
            {
                Ok(params) => params,
                Err(e) => {
                    eprintln!(
                        "finish_arc failed ({e}); dropping the provisional curve, keeping \
                     the straight placeholder. bulge={:.3}",
                        pa.bulge_ratio
                    );
                    self.document.pending_arc = None;
                    self.push_history_snapshot(undo_snap);
                    return Ok(());
                }
            };
        self.document
            .dimensioning
            .known_params
            .extend(facet_params.iter().copied());
        self.document.pending_arc = None;
        println!(
            "Curve: committed at bulge {:.3} ({} facet params locked)",
            pa.bulge_ratio,
            facet_params.len()
        );
        self.mark_dirty()?;
        self.push_history_snapshot(undo_snap);
        Ok(())
    }

    /// World-coord points of the provisional arc preview, if one is mid-flow.
    /// The UI draws this as an overlay; the model still holds the straight
    /// placeholder until the attach dimensions are entered.
    pub fn pending_arc_preview(&self) -> Option<Vec<(f64, f64)>> {
        let pa = self.document.pending_arc.as_ref()?;
        let pts = self
            .document
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
        let diag = self.document.floor_plan.bounding_box_diagonal();
        let min_sep = (diag * self.tuning.collinearity.min_separation_ratio).max(1e-4);
        let max_sep = diag * self.tuning.collinearity.max_separation_ratio;
        let newest_ring = self.document.floor_plan.ring_count().saturating_sub(1);
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
            .document
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
mod tests;
