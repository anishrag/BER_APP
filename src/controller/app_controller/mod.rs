// `AppController`'s behaviour is split across per-concern submodules
// (children of this module, so they can use the private fields/methods
// declared here). This file holds only the struct, its constructor, and the
// module wiring; each submodule owns one concern:
//   - `ceiling`       — default ceiling heights (Heights category entry + 3D availability).
//   - `ceiling_steps` — ceiling-step (raise/lower part of the ceiling) tagging.
//   - `debug`         — the paste-ready debug dump of the derivation state.
//   - `dimensioning`  — the dimension-prompt workflow.
//   - `editing`       — Edit-mode interaction + measurement prompts.
//   - `exposure`      — wall-exposure marking interaction.
//   - `history`       — undo/redo snapshots.
//   - `living_areas`  — living-area carving workflow.
//   - `persistence`   — BER record lifecycle, persist/autosave/snapshot.
//   - `phase_regions` — construction-phase tagging workflow.
//   - `review`        — the collinearity-review cursor.
//   - `roof_points`   — roof-point (apex / sloped-ceiling) tagging.
//   - `roof_ridges`   — roof-RIDGE line input (a stroke placing two apexes).
//   - `snap`          — elective-snap tags, choose/un-snap replay.
//   - `step_regions`  — floor-step (raise/lower area) tagging workflow.
//   - `storeys`       — storey switching + the deferred-derive pipeline.
//   - `strokes`       — sketch entry points + the apply-stroke family.
//   - `view`          — canvas pan/zoom.
mod assessment_phase;
mod ceiling;
mod ceiling_steps;
mod checklist;
mod debug;
mod dimensioning;
mod editing;
mod exposure;
mod living_areas;
mod openings;
mod phase_regions;
mod structure;

pub use assessment_phase::{AssessmentPhase, SelectMode};
pub use structure::{EditGroup, StructureEdit};
// `SurfaceKind` is the data-model surface taxonomy — it lives in `ber_model`
// (the queries dispatch on it) and is re-exported here so UI + controller call
// sites keep reaching it via `app_controller::SurfaceKind`.
pub use ber_model::ber::structural::SurfaceKind;
pub use checklist::{ChecklistItem, ChecklistStatus};
use document::{
    ActiveShape, CeilingStepRegion, DimensionPrompt, DimensionState, Document, EditState,
    MovedCoordKind, PendingArc, PhaseRegion, ShapeSlot, StepRegion, StoreyDoc,
};
pub use document::{ExposureSection, PositionPromptKind, RiserKind};
pub use exposure::{
    ExposureSelection, RiserView, SectionHandle, SurfaceExposureView, WallExposureView,
};
// `ExposureSection` is re-exported above (the controller's public surface).

use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository, BerSummary};
use crate::engine::dataloader::DataLoader;
use crate::engine::Db;
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::{ConstructionPhase, ConstructionPhases, ItemId};
use ber_model::ber::structural::{FloorRegions, Storey, SurfaceCounts, WallExposure};
use domain::points::{to_f32, Point2f32, Point2f64};
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_plan::{
    Axis, CollinearitySuggestion, FloorPlan, ParameterAxis, SubareaOutcome, VertexId,
};
use sketch::model::Stroke;
use sketch::SketchEngine;
use std::collections::{HashMap, HashSet};

mod history;
mod new_storey;
mod persistence;
mod review;
mod roof_points;
mod roof_rects;
mod roof_ridges;
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
pub use new_storey::NewStoreyPromptUiData;
use new_storey::{NewStoreyStage, PendingNewStorey};
use openings::PendingOpening;
pub use openings::{ElevationView, OpeningPromptUiData, OpeningToolKind, OpeningViewKind};
#[cfg(test)]
use persistence::PARAMETRIC_SNAPSHOT_VERSION;
pub use review::CollinearityReview;
use roof_points::PendingRoofPoint;
use roof_rects::PendingRoofRect;
use roof_ridges::{PendingRoofRidge, RidgeQuestion};
pub use snap::SnapTagView;
use snap::{LastSnapAction, SnapAction};
pub use strokes::PendingRedraw;
pub use view::{ViewTransform, DEFAULT_PITCH_DEG, DEFAULT_YAW_DEG};

/// Print a stroke as a `vec![p(x,y), …]` literal (matching the curve-debug
/// dumps) so a whole session can be copy-pasted into a bit-for-bit integration
/// test. Coordinates are whatever the caller passes — pixels for the initial
/// outline draw, metres for the world-coord extend/trim strokes.
fn dump_stroke(label: &str, pts: &[Point2f32]) {
    let body: String = pts
        .iter()
        .map(|p| format!("p({:.3},{:.3}),", p.x, p.y))
        .collect();
    log::debug!("  {label} stroke ({} pts): vec![{body}]", pts.len());
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
    /// In exposure mode, the derived RISER wall (by its stable region
    /// `ItemId` — `with_risers`' amend keeps it across re-derivations) the
    /// user last tapped on its dashed 2D line. Mutually exclusive with
    /// `selected_exposure_wall`; the exposure type buttons then act on the
    /// riser. Cleared everywhere the wall selection is.
    selected_riser: Option<ItemId>,
    /// A straight CUT line has split the storey into two candidate regions; the
    /// user picks which becomes the living area by tapping inside it. Transient
    /// (not persisted / not in undo history); `None` when nothing is awaiting a
    /// choice.
    pending_cut: Option<[ShapeSlot; 2]>,
    /// True when the editor is marking HORIZONTAL-surface (floor/ceiling)
    /// exposure: tapping a region picks it and the type buttons set its
    /// FloorExposure/CeilingExposure. Mutually exclusive with the sibling
    /// Mapping tools (wall exposure / living-area / phase / step). Toggled by
    /// the UI. Manual marks are keyed by region centroid (durable; outside undo,
    /// like the riser exposure).
    surface_exposure_mode: bool,
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
    /// True when the editor is tagging roof points (apexes that fan a ceiling
    /// region up to a height, entered as a tap + per-axis position + height).
    /// Routes canvas taps to the point input; mutually exclusive with the
    /// other area tools. Toggled by the UI; same gate as the ceiling steps.
    roof_point_mode: bool,
    /// The in-flight roof point, advanced stage by stage (tap → per-axis
    /// position → height). Transient — outside undo, cleared on storey
    /// switch / mode exit / cancel. The DURABLE record is
    /// `Storey::roof_apexes`; see `roof_points.rs`.
    pending_roof_point: Option<PendingRoofPoint>,
    /// The in-flight roof RIDGE (a stroke in roof-point mode placing TWO
    /// apexes at once), advanced stage by stage (stroke → cross-coordinate
    /// position → height). Transient — outside undo, cleared everywhere
    /// `pending_roof_point` is. The DURABLE record is the same
    /// `Storey::roof_apexes`; see `roof_ridges.rs`.
    pending_roof_ridge: Option<PendingRoofRidge>,
    /// The in-flight roof RECT (a CLOSED stroke in roof-point mode placing
    /// FOUR corner apexes at once — the flat-topped hip), advanced stage by
    /// stage (stroke → side lengths → lo positions → height). Transient —
    /// outside undo, cleared everywhere `pending_roof_ridge` is. The
    /// DURABLE record is the same `Storey::roof_apexes`; see
    /// `roof_rects.rs`.
    pending_roof_rect: Option<PendingRoofRect>,
    /// True when the editor is placing windows/doors. A tap near a wall drops
    /// the selected opening type on the nearest wall (then prompts size); a tap
    /// with no pending placement opens that wall's ELEVATION editor. Mutually
    /// exclusive with the other area tools; gated on a finalised outline.
    openings_mode: bool,
    /// Which opening the toolbar places next (window / roof window / door /
    /// door+sidelights).
    openings_tool: OpeningToolKind,
    /// The in-flight placement (a wall/roof pick) awaiting size + type entry.
    /// Transient — outside undo, cleared on storey switch / mode exit.
    pending_opening: Option<PendingOpening>,
    /// When `Some`, the ELEVATION overlay is open for this wall (by id): the
    /// canvas shows the unrolled wall and drags reposition its openings.
    /// Transient.
    elevation_wall: Option<ber_model::ber::common::ItemId>,
    /// The opening grabbed during an elevation drag (transient).
    elevation_drag: Option<ber_model::ber::openings::OpeningRef>,
    /// The opening selected in the elevation, for Delete (transient).
    elevation_selection: Option<ber_model::ber::openings::OpeningRef>,
    /// The in-flight NEW-STOREY flow (thickness → method choice → suggestion
    /// browse). Controller-transient — OUTSIDE undo, never persisted; set by
    /// `add_storey`, cleared on storey switch / load / open / exit. See
    /// `new_storey.rs`.
    pending_new_storey: Option<PendingNewStorey>,
    /// Upper storeys (idx > 0) whose INHERITED floor levels haven't been
    /// seeded yet — the drawn-path seed fires when the storey's outline
    /// FINALISES. Inserted by `add_storey`; re-derived on open/restore as
    /// {idx > 0 with an un-finalised outline} (restart-safe); consumed at
    /// seed; cleared on `exit_ber`. See `new_storey.rs`.
    inherited_seed_pending: HashSet<usize>,
    /// DEBUG: the splice-pipeline phases (#1 raw stroke … #4 spliced ring) from
    /// the last extend/trim, captured for the UI's coloured debug overlay.
    splice_debug: Vec<manip_plan::SplicePhase>,
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
    /// The assessor's current workflow phase (Mapping → Structure → Objects) —
    /// drives the Sketch page's toolset, checklist, and canvas overlays.
    current_phase: AssessmentPhase,
    /// The active Structure-phase MODES — the surface kinds (Walls/Roofs/Floors)
    /// the global Select tools act on and the canvas shades. NON-EXCLUSIVE: the
    /// assessor can have several active at once (e.g. Walls + Floors), so the
    /// selection and the "Edit attributes" floaties span every active kind.
    /// Defaults to {Walls}; reset on phase switch. Ordered (Walls < Roofs <
    /// Floors) for deterministic iteration / display.
    structure_kinds: std::collections::BTreeSet<SurfaceKind>,
    /// The active global Select subtool (Click/Carve/All/Remaining), or `None`.
    /// Transient interaction state — Structure-only, cleared on phase switch.
    structure_select: Option<SelectMode>,
    /// The Structure-phase SELECTED region ids (walls/roofs/floors of the
    /// active kind) — the canvas shades exactly these. Transient; cleared on
    /// phase switch / kind change / Deselect all.
    structure_selected: HashSet<ItemId>,
    /// Dimensioned CARVE rings (plan coords) selected but NOT yet split — the
    /// split is DEFERRED to "Edit attributes" (`commit_pending_structure_carves`)
    /// so a carve selection that turns out wrong changes no geometry. Each ring
    /// splits every active kind at commit time. Transient; cleared on Deselect
    /// all / phase switch / storey switch, and drained when the edit opens.
    pending_structure_carves: Vec<Vec<Point2f64>>,
    /// A Structure-phase straight CUT that split the storey into two candidate
    /// slots (each a dimensionable sub-area); the assessor taps one, which
    /// becomes the in-flight split shape and is then dimensioned. `None` when no
    /// cut is awaiting a pick.
    pending_structure_cut: Option<[ShapeSlot; 2]>,
    /// The surface kind being split while a Structure split shape is in-flight
    /// (carved/dimensioned). Drives which durable split field the committed ring
    /// pushes onto and which surfaces auto-select. `None` when no split is
    /// in-flight. Transient — cleared on phase switch / storey switch / commit.
    structure_split_kind: Option<SurfaceKind>,
    /// The in-flight "Edit attributes" session — the current selection grouped by
    /// shared construction, with one group's popup active. `None` when not
    /// editing. Transient; cleared on phase switch / kind change.
    structure_edit: Option<StructureEdit>,
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
            selected_riser: None,
            surface_exposure_mode: false,
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
            roof_point_mode: false,
            pending_roof_point: None,
            pending_roof_ridge: None,
            pending_roof_rect: None,
            openings_mode: false,
            openings_tool: OpeningToolKind::Window,
            pending_opening: None,
            elevation_wall: None,
            elevation_drag: None,
            elevation_selection: None,
            pending_new_storey: None,
            inherited_seed_pending: HashSet::new(),
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
            current_phase: AssessmentPhase::default(),
            structure_kinds: std::collections::BTreeSet::from([SurfaceKind::Walls]),
            structure_select: None,
            structure_selected: HashSet::new(),
            pending_structure_carves: Vec::new(),
            pending_structure_cut: None,
            structure_split_kind: None,
            structure_edit: None,
        }
    }
}

#[cfg(test)]
mod tests;
