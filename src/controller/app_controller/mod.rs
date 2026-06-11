// `AppController`'s behaviour is split across per-concern submodules
// (children of this module, so they can use the private fields/methods
// declared here). This file holds only the struct, its constructor, and the
// module wiring; each submodule owns one concern:
//   - `ceiling`       — default ceiling heights (Heights category entry + 3D availability).
//   - `ceiling_steps` — ceiling-step (raise/lower part of the ceiling) tagging.
//   - `dimensioning`  — the dimension-prompt workflow.
//   - `editing`       — Edit-mode interaction + measurement prompts.
//   - `exposure`      — wall-exposure marking interaction.
//   - `history`       — undo/redo snapshots.
//   - `living_areas`  — living-area carving workflow.
//   - `persistence`   — BER record lifecycle, persist/autosave/snapshot.
//   - `phase_regions` — construction-phase tagging workflow.
//   - `review`        — the collinearity-review cursor.
//   - `snap`          — elective-snap tags, choose/un-snap replay.
//   - `step_regions`  — floor-step (raise/lower area) tagging workflow.
//   - `storeys`       — storey switching + the deferred-derive pipeline.
//   - `strokes`       — sketch entry points + the apply-stroke family.
//   - `view`          — canvas pan/zoom.
mod ceiling;
mod ceiling_steps;
mod dimensioning;
mod editing;
mod exposure;
mod living_areas;
mod phase_regions;

use document::{
    ActiveShape, CeilingStepRegion, DimensionPrompt, DimensionState, Document, EditState,
    MovedCoordKind, PendingArc, PhaseRegion, ShapeSlot, StepRegion, StoreyDoc,
};
pub use document::{ExposureSection, PositionPromptKind};
pub use exposure::{ExposureSelection, SectionHandle, WallExposureView};
// `ExposureSection` is re-exported above (the controller's public surface).

use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository, BerSummary};
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

mod history;
mod persistence;
mod review;
mod snap;
mod step_regions;
mod storeys;
mod strokes;
mod view;

use ceiling_steps::PendingCeilingStep;
pub use dimensioning::{DimensionPromptUiData, PositionPromptUiData};
pub use editing::{
    EditDragPrompt, EditDragPromptUiData, MeasurementOption, VertexDropPrompt,
    VertexDropPromptUiData, VertexMeasurement, VertexMeasurementKind, VertexMeasurementUiData,
    WallPositions,
};
pub use history::HistorySnapshot;
#[cfg(test)]
use persistence::PARAMETRIC_SNAPSHOT_VERSION;
pub use review::CollinearityReview;
pub use snap::SnapTagView;
use snap::{LastSnapAction, SnapAction};
pub use strokes::PendingRedraw;
pub use view::ViewTransform;

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
    /// True when the editor is tagging floor-step areas (raised/lowered floor
    /// levels). Routes `DrawPerpWalls` to step carving; mutually exclusive with
    /// living-area / exposure / phase modes. Toggled by the UI.
    step_mode: bool,
    /// Session-only floor-step areas, keyed by storey index. NOT persisted in
    /// the BER — they re-split the floor regions at commit; the durable record
    /// is each floor region's `level_m`. See `step_regions.rs`.
    step_regions: HashMap<usize, Vec<StepRegion>>,
    /// A step cut split the storey into two candidate regions (each a
    /// dimensionable slot); the user taps one, which becomes `step_shape` and is
    /// then dimensioned. Transient.
    pending_step_cut: Option<[ShapeSlot; 2]>,
    /// True when the editor is tagging ceiling steps (raised/lowered ceiling
    /// areas, entered as a line + side + amount). Routes canvas strokes/taps
    /// to the line input; mutually exclusive with living-area / exposure /
    /// phase / floor-step modes. Toggled by the UI; gated on the storey's
    /// default ceiling height + a finalised outline.
    ceiling_step_mode: bool,
    /// Session-only ceiling-step areas, keyed by storey index. NOT persisted
    /// in the BER — they re-split the roof regions at commit; the durable
    /// record is each roof region's per-vertex z. Canonicalized to the exact
    /// derived pieces after each assign. See `ceiling_steps.rs`.
    ceiling_step_regions: HashMap<usize, Vec<CeilingStepRegion>>,
    /// The in-flight ceiling-step line, advanced tap by tap (orientation →
    /// side → position → amount). Transient — outside undo, cleared on storey
    /// switch / mode exit / cancel.
    pending_ceiling_step: Option<PendingCeilingStep>,
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
            step_mode: false,
            step_regions: HashMap::new(),
            pending_step_cut: None,
            ceiling_step_mode: false,
            ceiling_step_regions: HashMap::new(),
            pending_ceiling_step: None,
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
