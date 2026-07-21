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
//   - `porches`       — porch carving workflow (mirrors living areas).
//   - `persistence`   — BER record lifecycle, persist/autosave/snapshot.
//   - `phase_regions` — construction-phase tagging workflow.
//   - `review`        — the collinearity-review cursor.
//   - `ceiling_points`   — roof-point (apex / sloped-ceiling) tagging.
//   - `ceiling_ridges`   — roof-RIDGE line input (a stroke placing two apexes).
//   - `ceiling_dormers`  — flat-topped DORMER input (a closed stroke, non-resloping).
//   - `snap`          — elective-snap tags, choose/un-snap replay.
//   - `step_regions`  — floor-step (raise/lower area) tagging workflow.
//   - `storeys`       — storey switching + the deferred-derive pipeline.
//   - `strokes`       — sketch entry points + the apply-stroke family.
//   - `view`          — canvas pan/zoom.
mod assessment_phase;
mod bug_report;
mod ceiling;
mod ceiling_exposure;
mod ceiling_steps;
mod checklist;
mod debug;
mod dimensioning;
mod editing;
mod exposure;
mod final_phase;
mod floor_exposure;
mod followup;
mod living_areas;
mod mapping_review;
mod objects;
mod openings;
mod partition;
mod phase_regions;
mod photos;
mod porches;
mod roof;
mod structure;
mod structure_review;

pub use assessment_phase::{AssessmentPhase, SelectMode, StructureProgress};
pub use structure::{
    EditGroup, ExistingGroup, PhotoQuality, StructureEdit, WallInclusionPromptView,
};
// `SurfaceKind` is the data-model surface taxonomy — it lives in `ber_model`
// (the queries dispatch on it) and is re-exported here so UI + controller call
// sites keep reaching it via `app_controller::SurfaceKind`.
pub use ber_model::ber::structural::SurfaceKind;
pub use ceiling_exposure::{CeilingExposureRegionView, CeilingExposureStoreyView};
pub use checklist::{ChecklistItem, ChecklistStatus};
use document::{
    ActiveShape, CeilingStepRegion, DimensionPrompt, DimensionState, Document, EditState,
    MovedCoordKind, PendingArc, PhaseRegion, ShapeSlot, StepRegion, StoreyDoc,
};
pub use document::{ExposureSection, PositionPromptKind, RiserKind};
pub use exposure::{
    ExposureSelection, ExposureSnapKind, RiserView, SectionHandle, WallExposureView,
};
pub use floor_exposure::{FloorExposureRegionView, FloorExposureStoreyView};
pub use followup::{FollowUpPointRow, FollowUpRowKey, FollowUpView};
pub use phase_regions::PhaseDisplayRegion;
pub use roof::{RoofContextRegion, RoofRegionView, RoofShape};
// `ExposureSection` is re-exported above (the controller's public surface).

use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository, BerSummary};
use crate::engine::dataloader::DataLoader;
use crate::engine::Db;
use ber_model::ber::ber_struct::{Ber, IntakeDetails};
use ber_model::ber::common::{ConstructionPhase, ConstructionPhases, ItemId};
use ber_model::ber::structural::{FloorRegions, Storey, SurfaceCounts, WallExposure};
use domain::points::{to_f32, to_f64, Point2f32, Point2f64};
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_plan::{
    Axis, CollinearitySuggestion, FloorPlan, ParameterAxis, SubareaOutcome, VertexId,
};
use sketch::model::Stroke;
use sketch::SketchEngine;
use std::collections::{HashMap, HashSet};

mod ceiling_dormers;
mod ceiling_points;
mod ceiling_rects;
mod ceiling_ridges;
mod copy_height;
mod history;
mod new_storey;
mod persistence;
mod review;
mod snap;
mod step_regions;
mod storeys;
mod strokes;
mod view;

use ceiling_points::{ContinuationTool, PendingCeilingContinuation, PendingCeilingPoint};
use ceiling_rects::{PendingCeilingRect, RectPins};
use ceiling_ridges::{PendingCeilingRidge, RidgeQuestion};
use ceiling_steps::PendingCeilingStep;
use copy_height::CopyHeightTarget;
pub use dimensioning::{CentreBetweenPick, DimensionPromptUiData, PositionPromptUiData};
pub use editing::{
    EditDragPrompt, EditDragPromptUiData, MeasurementOption, VertexDropPrompt,
    VertexDropPromptUiData, VertexMeasurement, VertexMeasurementKind, VertexMeasurementUiData,
    WallPositions,
};
pub use final_phase::{FinalDetails, FinalMode, NorthImageView, WallFinishField};
pub use history::HistorySnapshot;
pub use mapping_review::ReviewView;
pub use new_storey::NewStoreyPromptUiData;
use new_storey::{NewStoreyStage, PendingNewStorey};
pub use objects::{
    ActiveObjectRoomView, ObjectPinView, ObjectReviewView, ObjectRowView, ObjectTotalView,
};
pub use openings::{
    ComplexWindowPlacement, DoorEditPrefill, DoorSidelightsMode, DoorSidelightsPlacement,
    ElevationView, ExistingWindowType, FullSpanInfo, OpeningEditPrefill, OpeningFieldSpec,
    OpeningFieldsPrompt, OpeningPromptUiData, OpeningToolKind, OpeningViewKind, WindowTypeChoice,
};
use openings::{OpeningEdit, PendingOpening};
pub use partition::{
    PartitionDimAxis, PartitionDimPrompt, PartitionInset, PartitionRegionView, PartitionSnapTag,
    PartitionView,
};
use partition::{PartitionInstantCommit, PendingCut};
#[cfg(test)]
use persistence::PARAMETRIC_SNAPSHOT_VERSION;
pub use photos::PhotoView;
pub use review::CollinearityReview;
pub use snap::SnapTagView;
use snap::{LastSnapAction, SnapAction, SubareaFlow};
pub use strokes::PendingRedraw;
pub use structure_review::{StructureReviewKind, StructureReviewView};
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
    /// Cached app-wide construction-layer library (the build-up editor's pick
    /// list); loaded on create, refreshed when a typed layer is saved.
    layer_library: Vec<ber_model::ber::structural::ConstructionLayer>,
    /// Where attached/captured photo files are stored. Set from `Paths` in
    /// `bootstrap`; a relative default keeps tests (no photos) working.
    photos_dir: std::path::PathBuf,
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
    /// True when the editor is in porch mode: `DrawPerpWalls` starts a new
    /// porch region (drawn, carved and dimensioned exactly like a living
    /// area; recorded for the resolve step — no effect on calculations yet).
    /// Mutually exclusive with living-area mode. Toggled by the UI.
    porch_mode: bool,
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
    /// What the dragged section handle currently snaps to (phase line, section
    /// edge, wall end, …) — the toolbar readout's one-line hint. `None` while
    /// not snapped. Transient, cleared with the drag/selection.
    exposure_snap_hint: Option<ExposureSnapKind>,
    /// In exposure mode, the derived RISER wall (by its stable region
    /// `ItemId` — `with_risers`' amend keeps it across re-derivations) the
    /// user last tapped on its dashed 2D line. Mutually exclusive with
    /// `selected_exposure_wall`; the exposure type buttons then act on the
    /// riser. Cleared everywhere the wall selection is.
    selected_riser: Option<ItemId>,
    /// A straight CUT line has split the storey into two candidate regions; the
    /// user picks which becomes the living area OR the porch by tapping inside
    /// it. The flow is STAGED with the regions (by the stroke that cut them) so
    /// the pick commits to the right list even if the mode flags have shifted
    /// since — `choose_cut_region` never routes by the current mode. Transient
    /// (not persisted / not in undo history — a restore clears it); `None` when
    /// nothing is awaiting a choice.
    pending_cut: Option<(SubareaFlow, [ShapeSlot; 2])>,
    /// True when the editor is marking HORIZONTAL-surface (floor/ceiling)
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
    /// The in-flight 'Centre between 2 points' pick for a whole-shape
    /// `Translate` position prompt: `First` (awaiting tap 1) → `Second` (tap 1
    /// snapped + placed, awaiting tap 2) → apply + clear. Transient — outside
    /// undo, cleared on cancel / apply / storey switch / record load (its
    /// `Second { a }` holds a coordinate in the active storey's metre space, so
    /// it must not survive into another storey — same reset discipline as
    /// `pending_ceiling_step` / `pending_roof_point`); the open document
    /// position prompt stays put behind it. See `dimensioning.rs`.
    pending_centre_between: Option<CentreBetweenPick>,
    /// True when the editor is tagging roof points (apexes that fan a ceiling
    /// region up to a height, entered as a tap + per-axis position + height).
    /// Routes canvas taps to the point input; mutually exclusive with the
    /// other area tools. Toggled by the UI; same gate as the ceiling steps.
    ceiling_point_mode: bool,
    /// The in-flight roof point, advanced stage by stage (tap → per-axis
    /// position → height). Transient — outside undo, cleared on storey
    /// switch / mode exit / cancel. The DURABLE record is
    /// `Storey::ceiling_apexes`; see `ceiling_points.rs`.
    pending_roof_point: Option<PendingCeilingPoint>,
    /// The in-flight roof RIDGE (a stroke in roof-point mode placing TWO
    /// apexes at once), advanced stage by stage (stroke → cross-coordinate
    /// position → height). Transient — outside undo, cleared everywhere
    /// `pending_roof_point` is. The DURABLE record is the same
    /// `Storey::ceiling_apexes`; see `ceiling_ridges.rs`.
    pending_roof_ridge: Option<PendingCeilingRidge>,
    /// The in-flight roof RECT (a CLOSED stroke in roof-point mode placing
    /// FOUR corner apexes at once — the flat-topped hip), advanced stage by
    /// stage (stroke → side lengths → lo positions → height). Transient —
    /// outside undo, cleared everywhere `pending_roof_ridge` is. The
    /// DURABLE record is the same `Storey::ceiling_apexes`; see
    /// `ceiling_rects.rs`.
    pending_roof_rect: Option<PendingCeilingRect>,
    /// Which CONTINUE-ceiling tool is toggled on (the Heights row's
    /// "Neighbour ceiling" / "Ceiling from storey" toggles) — a mode like
    /// the dormer/roof tools, mutually exclusive with them. `None` = off.
    continuation_tool: Option<ContinuationTool>,
    /// The in-flight CONTINUE-from pick while a continuation tool is on:
    /// `PickTarget` → (`PickSource { target }` →) commit → back to
    /// `PickTarget` (paint-style until the tool is toggled off). Transient —
    /// outside undo, cleared on storey switch / mode exit. The DURABLE
    /// record is `Storey::ceiling_continuations`; see `ceiling_points.rs`.
    pending_ceiling_continuation: Option<PendingCeilingContinuation>,
    /// Two-step confirm for the destructive "Reset heights" button: the
    /// first tap arms this, the second executes. Cleared on cancel / storey
    /// switch / mode changes. See `ceiling_points.rs`.
    reset_ceiling_confirm: bool,
    /// True when the editor is placing a flat-topped DORMER (a raised
    /// rectangular patch carved into an already-sloped attic ceiling —
    /// unlike the pavilion rect, it does NOT reslope its neighbours; its
    /// vertical walls fall out of `CeilingRegions::riser_faces`). Mutually
    /// exclusive with the other ceiling modes; same gate as the rect/apex
    /// tools. Toggled by the UI. See `ceiling_dormers.rs`.
    dormer_mode: bool,
    /// Discriminates the SHARED `PendingCeilingRect` FSM: true iff the
    /// in-flight rect was drawn in `dormer_mode`, so its final HEIGHT stage
    /// commits a `Dormer` (non-resloping) rather than four pavilion apexes.
    /// Only meaningful while `pending_roof_rect.is_some()` (read via
    /// [`pending_rect_is_dormer`](Self::pending_rect_is_dormer)); the pavilion
    /// flow leaves it false, byte-for-byte unchanged.
    pending_rect_is_dormer: bool,
    /// Which SIDES of the in-flight rect are PINNED — snapped flush onto a
    /// reference edge at draw time, so their dimension prompt is skipped and
    /// their coordinate is un-overridable (see `ceiling_dormers.rs` /
    /// `ceiling_rects.rs`). Set on BOTH the dormer path (surrounding roof/dormer
    /// edges) and the pavilion path (the containing ceiling region's walls);
    /// (re)set clean on every rect birth in `apply_roof_rect_stroke`, so the FSM
    /// never reads a stale pin and the guards key on the pins directly.
    pending_rect_pins: RectPins,
    /// True when the editor is placing windows/doors. A tap near a wall drops
    /// the selected opening type on the nearest wall (then prompts size); a tap
    /// with no pending placement opens that wall's ELEVATION editor. Mutually
    /// exclusive with the other area tools; gated on a finalised outline.
    openings_mode: bool,
    /// CAD-style navigation mode: no tool is selected, so a click/drag on the
    /// canvas only moves the view (pan/orbit) — it draws/places nothing. Entered
    /// automatically on a view (2D↔3D) or page change so a tool never persists
    /// into a context where a stray tap would do something unexpected; left by
    /// picking any tool. Transient session state.
    navigation_mode: bool,
    /// Which opening the toolbar places next (window / roof window / door /
    /// door+sidelights).
    openings_tool: OpeningToolKind,
    /// The in-flight placement (a wall/roof pick) awaiting size + type entry.
    /// Transient — outside undo, cleared on storey switch / mode exit.
    pending_opening: Option<PendingOpening>,
    /// Inline error for the in-flight placement (e.g. wall too short), shown in
    /// the floating measurement box. Transient.
    opening_dim_error: Option<String>,
    /// Monotonic counter bumped on each fresh opening host pick. Surfaced in the
    /// fields prompt so the UI resets the floating box's field focus per placement.
    placement_seq: u64,
    /// When `Some`, the ELEVATION overlay is open for this wall (by id): the
    /// canvas shows the unrolled wall and drags reposition its openings.
    /// Transient.
    elevation_wall: Option<ber_model::ber::common::ItemId>,
    /// The opening grabbed during an elevation drag (transient).
    elevation_drag: Option<ber_model::ber::openings::OpeningRef>,
    /// The opening selected in the elevation, for Delete (transient).
    elevation_selection: Option<ber_model::ber::openings::OpeningRef>,
    /// The opening being attribute-edited on-surface ("Edit…"): a wall window or
    /// door from the elevation, or a roof window from the plan. The placement box
    /// re-opens in edit mode prefilled from it (size / type / attributes).
    /// Transient — cleared with the rest of the openings edit state.
    opening_edit: Option<OpeningEdit>,
    /// A ROOF window selected in the PLAN view (Edit-on-surface tool), for drag /
    /// delete — roof windows live in plan, so their "surface" is the footprint.
    /// Transient.
    roof_window_sel: Option<ber_model::ber::common::ItemId>,
    /// The roof window grabbed during a plan drag (transient).
    roof_window_drag: Option<ber_model::ber::common::ItemId>,
    /// A delete is awaiting confirmation (the active selection — `elevation_selection`
    /// for a wall opening, else `roof_window_sel` — names the target). Transient.
    confirm_delete: bool,
    /// The Stage-1 (Mapping) REVIEW takeover view is open (transient). The canvas
    /// shows the current side's external elevation beside its photo; the assessor
    /// scrolls the four sides and confirms "all's good" (`mapping_review.rs`).
    mapping_review_mode: bool,
    /// Which external side the review is showing (the scroll cursor).
    mapping_review_side: ber_model::ber::common::ExternalSide,
    /// The opening grabbed during a review elevation drag: (storey index, wall id,
    /// opening). Transient.
    mapping_review_drag: Option<(
        usize,
        ber_model::ber::common::ItemId,
        ber_model::ber::openings::OpeningRef,
    )>,
    /// The opening selected in the review elevation (transient).
    mapping_review_selection: Option<ber_model::ber::openings::OpeningRef>,
    /// Stage-2 (Structure) REVIEW takeover: when on, the canvas shows the current
    /// review item's details beside the orbitable 3D house with that group's
    /// surfaces shaded (`structure_review.rs`).
    structure_review_mode: bool,
    /// Which review item (group) is showing — index into the whole-house item
    /// list built each frame; clamped/wrapped against its length.
    structure_review_cursor: usize,
    /// Phase-3 (Objects): the room pin currently OPEN for adding objects, by
    /// `ObjectRoom` id. `None` = no room open (tap the plan to drop/open one).
    active_object_room: Option<ItemId>,
    /// Objects REVIEW takeover open?
    object_review_mode: bool,
    /// Which object kind is selected in the review (index into the flat totals
    /// list built each frame; clamped). Drives the 3D hotspot highlight.
    object_review_selected: usize,
    /// Empty categories the assessor said "No" to during THIS review (there is one
    /// — they'll go add it), so the absent-confirmation prompt skips past them for
    /// the session. Cleared each time the review opens. (A "Yes" persists on the
    /// record instead — `Ber.objects_no_items_confirmed`.)
    object_review_skipped: std::collections::HashSet<ber_model::ber::objects::ObjectCategory>,
    /// The attic counterpart of `object_review_skipped`: "Anything in the
    /// attic?" answered "Yes — there is" this session (they'll go add it).
    /// Cleared each time the review opens; a "No, empty" persists on the
    /// record instead (`Ber.attic_no_items_confirmed`).
    object_review_attic_skipped: bool,
    /// Phase-3 heating: the `HeatingZones` object awaiting its zone-count entry
    /// (item 3). `Some` raises the "how many zones?" number box; the answer
    /// lands in `heating_system.controls.zones`. Transient.
    zone_prompt: Option<ItemId>,
    /// Session flag: the "is the house heated by panel heaters?" question (item
    /// 4) was answered this Objects review. Reset when the review opens.
    panel_heater_asked: bool,
    /// Session flag: the "which is the primary heat source?" pick (item 5) was
    /// answered this Objects review. Reset when the review opens.
    heating_primary_asked: bool,
    /// Phase-4 north-align: the satellite IMAGE's transient pan offset (canvas
    /// px) and zoom over the fixed plan. The image's ROTATION is the persisted
    /// `orientation_deg` (the plan's north). Pan/zoom are session-only.
    north_img_offset: (f32, f32),
    north_img_zoom: f32,
    /// Session flags for the Phase-4 completion flow: the "BER complete" popup is
    /// up, and the (dummy) summary screen is up. Set when the final review is
    /// confirmed; cleared by the popup/summary buttons. Not persisted.
    show_ber_complete: bool,
    show_summary: bool,
    /// Session flag: the "you deferred these photos — take them now" prompt is
    /// up (raised by `confirm_final_review` when a deferred side still lacks
    /// its photo, blocking completion until the shots are captured). Not
    /// persisted — the DURABLE deferral state is `Ber::deferred_photo_sides`.
    show_deferred_photos_prompt: bool,
    /// Photos whose missing THUMBNAIL this session already tried to backfill
    /// (`ensure_external_photo_thumbnail` — the panel previews only
    /// thumbnails). One attempt each: a failure (e.g. an undecodable file)
    /// must not re-decode every frame the placeholder is visible.
    thumbnail_backfill_attempted: std::collections::HashSet<ItemId>,
    /// Phase-4 view sub-mode (form / aligning the image / marking sheltered
    /// sides). Session-only.
    final_mode: final_phase::FinalMode,
    /// When `Some`, the PARTITION overlay is open for this outline wall (by its
    /// stable start `VertexId`): the canvas shows the unrolled wall face and
    /// strokes/taps cut + attribute it. Opened from the wall-exposure toolbar.
    /// Transient — cleared on storey switch / mode exit. The DURABLE record is
    /// `StoreyDoc::wall_face_partitions`; see `partition.rs`.
    partition_wall: Option<VertexId>,
    /// The in-flight freehand partition stroke in WALL-LOCAL coords (along,
    /// height) — the UI converts screen → wall-local before pushing. Cleared on
    /// stroke end. Transient.
    partition_stroke: Vec<Point2f32>,
    /// A partition cut mid-dimensioning: its snapped endpoints + per-coordinate
    /// free flags, awaiting typed dimensions for the un-snapped coords. Transient
    /// (outside undo); `None` when no cut is in flight.
    partition_pending: Option<PendingCut>,
    /// The last INSTANTLY-committed partition cut (both endpoints fully
    /// snapped at stroke end), kept so its snap tags still show and "Don't
    /// snap" can un-commit + reopen it. Controller-transient, OUTSIDE undo;
    /// valid only while nothing else has happened since the commit (checked
    /// against the history length); cleared by the next stroke, partition
    /// open/close, any dimension-path commit, and undo/redo. See
    /// `partition.rs`.
    partition_instant_commit: Option<PartitionInstantCommit>,
    /// The wall-local sample point of the face region tapped, for assigning its
    /// exposure. Transient; `None` when no region is selected.
    partition_selected_region: Option<Point2f32>,
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
    /// Optional background cloud-sync worker (attached by the real app when
    /// sync config is present; absent → the app runs fully offline).
    sync: Option<crate::engine::SyncWorker>,
    /// Optional background cloud-push worker (same config gate as `sync`).
    /// Poked after every save so local changes reach the backend "whenever it
    /// can"; absent → BERs stay local until sync is configured.
    push: Option<crate::engine::PushWorker>,
    /// Where captured bug reports are staged/queued before upload. Set from
    /// `Paths` in `bootstrap`; a relative default keeps tests working.
    bug_reports_dir: std::path::PathBuf,
    /// Optional background bug-report uploader (attached when sync config is
    /// present). Absent → reports still queue locally, upload on a later run.
    bug_uploader: Option<crate::engine::BugReportUploader>,
    /// The cloud-config file path (`sync.json`), so the one-shot Follow-ups
    /// "send" can reach the backend with the same config as the workers. Set
    /// from `Paths` in `bootstrap`; a relative default keeps tests working.
    sync_config_file: std::path::PathBuf,
    /// In-flight follow-up "send email" call: the BER it was fired for (so a
    /// completion lands on the RIGHT record even if the assessor navigated away
    /// mid-send) plus the worker-thread state, polled each frame
    /// (`poll_followup_send`). `None` when no send is running. See [`followup`].
    followup_send: Option<followup::PendingSend>,
    /// Transient message from the last follow-up send that DIDN'T succeed (no
    /// cloud job / network error), tagged with the BER it belongs to so it only
    /// shows on that record's tab. Cleared on a retry.
    followup_send_error: Option<(ItemId, String)>,
    /// The staging path of an in-flight audio recording (`Some` between
    /// `start_bug_recording` and `finish_bug_report`/`cancel_bug_recording`).
    bug_recording_path: Option<std::path::PathBuf>,
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
    /// Diagonal (metres) of the currently-VISIBLE world rectangle, pushed each
    /// frame by the sketch UI (`set_view_world_diagonal_m`). A plain scalar so no
    /// egui type crosses the boundary; the snap-tolerance policy
    /// (`snap_view_diag` + `snap_tol_*`) reads it. `None` before the first frame
    /// / in headless runs — the helpers then fall back to the plan-bbox diagonal.
    view_world_diagonal_m: Option<f32>,
    /// Runtime thresholds loaded from `tuning.yaml` (new-storey + collinearity).
    tuning: crate::config::Tuning,
    /// The assessor's current workflow phase (Mapping → Structure → Objects) —
    /// drives the Sketch page's toolset, checklist, and canvas overlays.
    current_phase: AssessmentPhase,
    /// The active Structure-phase MODES — the surface kinds (Walls/Ceilings/Floors)
    /// the global Select tools act on and the canvas shades. NON-EXCLUSIVE: the
    /// assessor can have several active at once (e.g. Walls + Floors), so the
    /// selection and the "Edit attributes" floaties span every active kind.
    /// Defaults to {Walls}; reset on phase switch. Ordered (Walls < Ceilings <
    /// Floors) for deterministic iteration / display.
    structure_kinds: std::collections::BTreeSet<SurfaceKind>,
    /// The active global Select subtool (Click/Carve/All/Remaining), or `None`.
    /// Transient interaction state — Structure-only, cleared on phase switch.
    structure_select: Option<SelectMode>,
    /// The Structure-phase SELECTED region ids (walls/ceilings/floors of the
    /// active kind) — the canvas shades exactly these. Transient; cleared on
    /// phase switch / kind change / Deselect all.
    structure_selected: HashSet<ItemId>,
    /// Dimensioned CARVE rings (plan coords) selected but NOT yet split — the
    /// split is DEFERRED to "Edit attributes" (`commit_pending_structure_carves`)
    /// so a carve selection that turns out wrong changes no geometry. Each ring
    /// splits every active kind at commit time and carries the interior edges
    /// whose walls the assessor EXCLUDED from the selection. Transient; cleared
    /// on Deselect all / phase switch / storey switch, and drained when the edit
    /// opens.
    pending_structure_carves: Vec<structure::StagedStructureCarve>,
    /// The in-flight per-edge wall-inclusion ASK for a just-dimensioned
    /// structure carve: each carve edge with house area beyond it (interior to
    /// the storey footprint) is ambiguous, so the assessor answers
    /// Include/Exclude edge by edge before the carve stages. `None` when nothing
    /// is being asked. Transient — same clear sites as the pending carves.
    pending_wall_inclusion: Option<structure::WallInclusionPrompt>,
    /// Wall-inclusion asks QUEUED behind the active one — a cross-storey lasso
    /// can raise one ask per storey/clipped region, answered in sequence
    /// (resolving the active ask pops the next). Empty outside the lasso flow.
    /// Transient — same clear sites as the pending carves.
    queued_wall_inclusions: Vec<structure::WallInclusionPrompt>,
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
    /// True when the FLOOR-exposure tool is active: the canvas shows every
    /// storey's floors overlaid and taps/carves select floor regions across the
    /// stack. Mutually exclusive with the other Mapping modes. Toggled by the UI.
    floor_exposure_mode: bool,
    /// True while the storey-count gate prompt is up (entered the tool but the
    /// assessor hasn't yet confirmed every storey is added). The overlaid view
    /// + selection are gated until confirm (inter-storey surfaces derive from
    /// the full stack).
    floor_exposure_gate_pending: bool,
    /// The active floor-exposure Select subtool (Click/Carve/All/Remaining), or
    /// `None`. Transient floor-exposure interaction state.
    floor_exposure_select: Option<SelectMode>,
    /// The selected floor regions, keyed by `(storey_idx, region centroid key)`
    /// since the selection spans the whole stack. A centroid key (not an
    /// `ItemId`) is used because floor-region ids are re-minted on every resolve
    /// — and `materialize` re-resolves every frame — so an id-keyed selection
    /// would go stale the moment a `mark_dirty` fired. The overlaid view halos
    /// these. Transient; cleared on tool exit / Deselect.
    floor_exposure_selected: HashSet<(usize, document::CentroidKey)>,
    /// True while a floor-exposure CARVE is in-flight in the dimensioning buffer
    /// (the carved region drawn as a `PhaseArea` shape, awaiting its measurements
    /// so the split ring is pinned). Discriminates the shared
    /// `maybe_stage_structure_carve` staging hook: when set, a finished carve
    /// stages into [`pending_floor_exposure_carves`] (splits every overlapping
    /// storey's FLOOR) rather than the Structure-phase pending carves. `None`
    /// when no floor-exposure carve is in-flight.
    floor_exposure_carve_in_flight: bool,
    /// The reference storey whose outline a floor-exposure carve is dimensioned
    /// against (the All tab has no active storey, so the carve borrows a storey's
    /// outline into the live buffer). The carve ring lands in plan/world coords —
    /// applied to every overlapping storey at commit. `None` when no carve is
    /// in-flight; restores nothing (the buffer is reloaded on exit).
    floor_exposure_carve_ref: Option<usize>,
    /// A floor-exposure straight CUT that split the whole building into two
    /// candidate slots (each pinned against the same whole-building boundary the
    /// carve uses); the assessor taps one, which becomes the in-flight carve
    /// shape and is then dimensioned + committed like an Area carve. `None` when
    /// no cut is awaiting a pick. Transient; cleared on tool exit / gate / storey
    /// switch alongside the other floor-exposure carve state.
    floor_exposure_pending_cut: Option<[ShapeSlot; 2]>,
    /// True when the CEILING-exposure tool is active: the canvas shows every
    /// storey's ceilings overlaid and taps/carves select ceiling regions across
    /// the stack. The mirror of [`floor_exposure_mode`](Self::floor_exposure_mode);
    /// mutually exclusive with the other Mapping modes. Toggled by the UI.
    ceiling_exposure_mode: bool,
    /// True while the ceiling-exposure storey-count gate prompt is up (entered
    /// the tool but the assessor hasn't yet confirmed every storey is added).
    ceiling_exposure_gate_pending: bool,
    /// Phase-1 ROOF step: when on, the canvas shows the whole-house outline
    /// shaded by top storey and the roof-drawing controls. Gated (after all
    /// storeys) like the exposure tools.
    roof_mode: bool,
    roof_gate_pending: bool,
    /// The ceiling regions tapped into the current roof-area-in-progress, keyed
    /// by `(storey_idx, centroid key)` (stable across re-resolves, unlike ItemId).
    roof_selected: HashSet<(usize, document::CentroidKey)>,
    /// The in-flight per-area roof drawing (ridge stroke → pitch), once a sloped
    /// shape is picked for the grouped area. `None` while only grouping.
    pending_roof_area: Option<roof::PendingRoofArea>,
    /// Completed background photo-processing jobs, awaiting application to the
    /// model by id ([`apply_finished_photos`](Self::apply_finished_photos)).
    /// Pushed from worker threads; drained each frame.
    photo_results: std::sync::Arc<std::sync::Mutex<Vec<structure::PhotoJobResult>>>,
    /// Count of photo jobs still processing on background threads — the UI keeps
    /// polling / repainting while this is non-zero so finished photos appear.
    photo_jobs_inflight: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// The active ceiling-exposure Select subtool (Click/Carve/All/Remaining), or
    /// `None`. Transient ceiling-exposure interaction state.
    ceiling_exposure_select: Option<SelectMode>,
    /// The selected ceiling regions, keyed by `(storey_idx, region centroid key)`
    /// (the selection spans the whole stack; a centroid key is used because
    /// ceiling-region ids are re-minted on every resolve). The overlaid view
    /// halos these. Transient; cleared on tool exit / Deselect.
    ceiling_exposure_selected: HashSet<(usize, document::CentroidKey)>,
    /// True while a ceiling-exposure CARVE is in-flight in the dimensioning
    /// buffer (the carved region drawn as a `PhaseArea` shape). Discriminates the
    /// shared staging hook: when set, a finished carve splits every overlapping
    /// storey's ROOFS rather than the floor / Structure carves.
    ceiling_exposure_carve_in_flight: bool,
    /// The reference storey whose outline a ceiling-exposure carve is dimensioned
    /// against (mirror of [`floor_exposure_carve_ref`](Self::floor_exposure_carve_ref)).
    ceiling_exposure_carve_ref: Option<usize>,
    /// A ceiling-exposure straight CUT awaiting the assessor's side pick (mirror
    /// of [`floor_exposure_pending_cut`](Self::floor_exposure_pending_cut)).
    ceiling_exposure_pending_cut: Option<[ShapeSlot; 2]>,
    /// Active "copy height from a known surface" pick, targeting whichever of
    /// the dormer / ceiling-step / floor-step height prompts was open when it
    /// started. `Some` = the canvas is in pick mode (a tap on a known surface
    /// copies + commits its height). Transient — outside undo. See
    /// `copy_height.rs`.
    copy_height_pick: Option<CopyHeightTarget>,
    /// Transient rejection message for the copy-height pick (tapped an unknown
    /// surface, or the copied height failed the commit's guard). Shown under the
    /// pick hint; cleared on start / cancel / a successful copy.
    copy_height_error: Option<String>,
    /// Transient notice for a SILENTLY auto-resolved junction ridge (a typed
    /// height on a ridge claimed by ≥ 2 ceiling areas where exactly one was
    /// slope-able — no ChooseArea prompt opened): names which side sloped so
    /// the decision is visible/diagnosable. Set by `assign_roof_ridge_height`;
    /// suppressed on the assessor-initiated "meet other edge" path; cleared on
    /// the next ridge action / cancel / mode exit / storey switch / record
    /// load. Read via `roof_ridge_notice()`. See `ceiling_ridges.rs`.
    roof_ridge_notice: Option<String>,
    /// Transient inline message for the CONTINUE-ceiling tools: why the last
    /// pick tap was REFUSED (missed every region, no lateral source on the
    /// neighbour storey, source == target …) or what to do next. NO refused
    /// tap may be silent (deb23cbf live failure — taps that "did nothing").
    /// Set by `ceiling_continuation_tap` / the from-storey auto-commit;
    /// cleared on the next tap, a successful commit, tool toggle, and
    /// cancel. Read via `ceiling_continuation_notice()`.
    ceiling_continuation_notice: Option<String>,
    /// Transient SUCCESS confirmation for the CONTINUE-ceiling tools —
    /// naming what a just-applied continuation produced ("Continued Storey 1
    /// area from Storey 2 — 4.25 m → 2.75 m"), so a single-neighbour
    /// auto-apply doesn't read as a no-op (the deb23cbf feedback gap).
    /// Set on a successful commit; cleared on the next tap, a refusal, tool
    /// toggle, and cancel. Read via `ceiling_continuation_confirm()`.
    ceiling_continuation_confirm: Option<String>,
}

impl AppController {
    pub fn new(conn: Db) -> Self {
        // The app-wide construction-layer library (the build-up editor's pick
        // list). Cached so the per-frame view build doesn't hit SQLite; refreshed
        // when a new layer is typed. `open_db` ran `init_db`, so the table exists.
        let layer_library = crate::engine::ber_repository::BerRepository::new(&conn)
            .list_construction_layers()
            .unwrap_or_default();
        Self {
            conn,
            layer_library,
            photos_dir: std::path::PathBuf::from("photos"),
            bug_reports_dir: std::path::PathBuf::from("bug-reports"),
            bug_uploader: None,
            sync_config_file: std::path::PathBuf::from("sync.json"),
            followup_send: None,
            followup_send_error: None,
            bug_recording_path: None,
            loader_engine: DataLoader::new(),
            sketch_engine: SketchEngine::new(),
            current_record: None,
            current_storey_idx: None,
            review: None,
            pending_redraw: None,
            document: Document::default(),
            living_area_mode: false,
            porch_mode: false,
            exposure_mode: false,
            selected_exposure_wall: None,
            exposure_sel: None,
            exposure_drag: None,
            exposure_snap_hint: None,
            selected_riser: None,
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
            pending_centre_between: None,
            ceiling_point_mode: false,
            pending_roof_point: None,
            continuation_tool: None,
            pending_ceiling_continuation: None,
            reset_ceiling_confirm: false,
            pending_roof_ridge: None,
            pending_roof_rect: None,
            dormer_mode: false,
            pending_rect_is_dormer: false,
            pending_rect_pins: RectPins::default(),
            openings_mode: false,
            navigation_mode: false,
            openings_tool: OpeningToolKind::Window,
            pending_opening: None,
            opening_dim_error: None,
            placement_seq: 0,
            elevation_wall: None,
            elevation_drag: None,
            elevation_selection: None,
            opening_edit: None,
            roof_window_sel: None,
            roof_window_drag: None,
            confirm_delete: false,
            mapping_review_mode: false,
            mapping_review_side: ber_model::ber::common::ExternalSide::Front,
            mapping_review_drag: None,
            mapping_review_selection: None,
            structure_review_mode: false,
            structure_review_cursor: 0,
            active_object_room: None,
            object_review_mode: false,
            object_review_selected: 0,
            object_review_skipped: std::collections::HashSet::new(),
            object_review_attic_skipped: false,
            zone_prompt: None,
            panel_heater_asked: false,
            heating_primary_asked: false,
            north_img_offset: (0.0, 0.0),
            north_img_zoom: 1.0,
            show_ber_complete: false,
            show_summary: false,
            show_deferred_photos_prompt: false,
            thumbnail_backfill_attempted: std::collections::HashSet::new(),
            final_mode: final_phase::FinalMode::Form,
            partition_wall: None,
            partition_stroke: Vec::new(),
            partition_pending: None,
            partition_instant_commit: None,
            partition_selected_region: None,
            pending_new_storey: None,
            inherited_seed_pending: HashSet::new(),
            splice_debug: Vec::new(),
            revision: 0,
            saved_revision: 0,
            derived_revision: 0,
            writer: None,
            sync: None,
            push: None,
            last_snap_action: None,
            edit_drag_prompt: None,
            vertex_drop_prompt: None,
            history: Vec::new(),
            redo: Vec::new(),
            view: ViewTransform::default(),
            view_world_diagonal_m: None,
            tuning: crate::config::Tuning::load(),
            current_phase: AssessmentPhase::default(),
            structure_kinds: std::collections::BTreeSet::from([SurfaceKind::Walls]),
            structure_select: None,
            structure_selected: HashSet::new(),
            pending_structure_carves: Vec::new(),
            pending_wall_inclusion: None,
            queued_wall_inclusions: Vec::new(),
            pending_structure_cut: None,
            structure_split_kind: None,
            structure_edit: None,
            floor_exposure_mode: false,
            floor_exposure_gate_pending: false,
            floor_exposure_select: None,
            floor_exposure_selected: HashSet::new(),
            floor_exposure_carve_in_flight: false,
            floor_exposure_carve_ref: None,
            floor_exposure_pending_cut: None,
            ceiling_exposure_mode: false,
            ceiling_exposure_gate_pending: false,
            roof_mode: false,
            roof_gate_pending: false,
            roof_selected: HashSet::new(),
            pending_roof_area: None,
            photo_results: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
            photo_jobs_inflight: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            ceiling_exposure_select: None,
            ceiling_exposure_selected: HashSet::new(),
            ceiling_exposure_carve_in_flight: false,
            ceiling_exposure_carve_ref: None,
            ceiling_exposure_pending_cut: None,
            copy_height_pick: None,
            copy_height_error: None,
            roof_ridge_notice: None,
            ceiling_continuation_notice: None,
            ceiling_continuation_confirm: None,
        }
    }
}

#[cfg(test)]
mod tests;
