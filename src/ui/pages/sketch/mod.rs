//! The Sketch page: a responsive category layout around the drawing canvas.
//!
//! Module map:
//! - `mod.rs` — [`SketchEvent`], the [`SketchView`] view-model the shell
//!   builds each frame, the page entry point (orientation check + panel
//!   arrangement), and the storey tabs.
//! - `categories.rs` — the tool categories defined ONCE as data, plus the two
//!   renderers over them: portrait → top ribbon, landscape → left side panel.
//! - `prompt_dock.rs` — every contextual flow panel (dimension/position
//!   prompts, cut picks, amount entry, review, …) in one bottom strip.
//! - `canvas.rs` — the canvas itself: overlays, stroke capture, taps, drags,
//!   plus the floating 2D/3D button in its corner.
//! - `overlays.rs` — the floating phase signpost (Mapping → Structure → Objects)
//!   and the live Stage A completion checklist, painted over the canvas.

mod canvas;
pub mod categories;
mod final_panel;
mod number_box;
mod objects_panel;
pub mod overlays;
mod prompt_dock;
mod structure;
pub mod touch;

use egui::epaint::PathStroke;
use egui::Pos2;

use crate::controller::app_controller::{
    ActiveObjectRoomView, AssessmentPhase, ChecklistItem, ComplexWindowPlacement,
    DimensionPromptUiData, DoorSidelightsPlacement, EditDragPromptUiData, ElevationView,
    ExistingGroup, ExistingWindowType, ExposureSelection, FinalMode, NewStoreyPromptUiData,
    ObjectPinView, ObjectReviewView, OpeningFieldsPrompt, OpeningToolKind, PartitionView,
    PhaseDisplayRegion, PhotoView, PositionPromptUiData, ReviewView, RiserView, SelectMode,
    SnapTagView, StructureEdit, StructureReviewView, SurfaceKind, VertexDropPromptUiData,
    WallExposureView, WallFinishField, WallInclusionPromptView, WindowTypeChoice,
};
use crate::ui::pages::events::UiEvent;
use crate::ui::render::ViewParams;
use crate::ui::widgets::HintExt;
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::{ConstructionPhase, ExternalSide, ItemId};
use ber_model::ber::fenestration::{Frame, Glazing, Overshading};
use ber_model::ber::final_details::{FinalDetails, FloorKind, HeatingKind, WallFinish};
use ber_model::ber::objects::{ObjectCategory, ObjectKind};
use ber_model::ber::structural::{SurfaceCounts, WallExposure};
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_plan::CollinearitySuggestion;
use std::collections::HashSet;

/// The UI-friendly version of a stroke
pub struct EguiStroke {
    pub points: Vec<Pos2>,
    pub stroke: PathStroke,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SketchEvent {
    StartStroke(Pos2),
    AddPoint(Pos2),
    EndStroke(Pos2),
    /// Retract the in-flight stroke: the capture turned out to be the start
    /// of a two-finger navigation gesture, not a drawing.
    CancelStroke,
    ChangeMode(SketchMode),
    ChangeEditTool(EditTools),
    /// User confirmed the current collinearity suggestion: walls are the same line.
    AcceptCollinearity,
    /// User rejected the current collinearity suggestion: walls are distinct.
    RejectCollinearity,
    /// User dismissed all remaining collinearity suggestions.
    SkipAllCollinearity,
    /// User chose to replace the current storey with the pending stroke.
    ReplaceStorey,
    /// User chose to add a new storey from the pending stroke (stub feature).
    AddStoreyFromCurrent,
    /// User chose to discard the pending stroke.
    IgnoreRedraw,
    /// Force one elective snap (identified by its drawn `from` point) to a
    /// specific choice — `Some(candidate_index)` or `None` (don't snap) — and
    /// replay the action. Driven by clicking a snap tag's menu.
    ChooseSnap {
        from: (f32, f32),
        choice: Option<usize>,
    },
    /// Primary mouse button pressed in Move mode at the given world-coord
    /// position. Hit-tests a wall and selects it if found.
    EditMouseDown {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Primary mouse drag in Move mode: translate the selected wall so its
    /// perpendicular coord matches `(world_x, world_y)`.
    EditMouseDrag {
        world_x: f64,
        world_y: f64,
    },
    /// Primary mouse release in Move mode: commit + clear drag flag.
    EditMouseUp,
    /// User entered a distance value in the post-drag prompt. The
    /// controller applies it via the currently-selected measurement
    /// option (parameter set / constant shift).
    ApplyEditDrag(f32),
    /// User clicked "Try another measurement" — advance to the next
    /// candidate reference wall.
    CycleEditDragMeasurement,
    /// User cancelled the post-drag prompt — revert to pre-drag value.
    CancelEditDrag,
    /// Double-click in Move mode: insert a new vertex on the wall closest
    /// to the click point and start a free-2D drag on it.
    EditDoubleClick {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// User entered a value in one of the vertex-drop prompt's text boxes.
    /// `idx` is the measurement's index in `VertexDropPrompt.measurements`.
    ApplyVertexMeasurement {
        idx: usize,
        value: f32,
    },
    /// User cancelled the vertex-drop prompt — revert the inserted vertex
    /// to its pre-drag (initial click) position.
    CancelVertexDrop,
    /// Undo the most recent data mutation.
    Undo,
    /// Re-apply the most recently undone mutation.
    Redo,
    /// DEV: save the current plan to the checkpoint file (reloaded on a
    /// `BER_CHECKPOINT=1` launch or via `LoadCheckpoint`).
    SaveCheckpoint,
    /// DEV: load the checkpoint file into the editor, replacing the current
    /// sketch session (same reconstruction as a `BER_CHECKPOINT=1` launch).
    LoadCheckpoint,
    /// DEV: print the roof/wall derivation dump to stdout (and copy it to
    /// the clipboard) for paste-ready diagnosis.
    DebugDump,
    /// Extend-mode stroke completed: the user's captured stroke,
    /// converted to *world* coords via the current fit transform so the
    /// controller can compare it against the polygon (which is in real
    /// units post-dim or in pixel-derived units pre-dim).
    ExtendStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// First outline stroke of a NEW upper storey, captured in *world*
    /// (previous-storey metre) coords rather than canvas pixels — so endpoints
    /// can snap onto the previous storey's outline and shared walls inherit
    /// their known lengths. Closedness is decided controller-side from the
    /// engine's pixel close-check, so no `closed` flag rides along.
    OutlineStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// Trim-mode stroke completed: the captured stroke in *world* coords. Its
    /// first and last points define the straight cut line.
    TrimStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// Scroll-wheel zoom about the given anchor (canvas pixel coords).
    ZoomView {
        factor: f32,
        anchor_x: f32,
        anchor_y: f32,
        canvas_center_x: f32,
        canvas_center_y: f32,
    },
    /// Right-drag pan delta (canvas pixels).
    PanView {
        dx: f32,
        dy: f32,
    },
    /// Primary-drag orbit delta in the 3D view (degrees of yaw/pitch,
    /// already converted from pixels by the canvas).
    OrbitView {
        dyaw: f32,
        dpitch: f32,
    },
    /// Reset the view to pure auto-fit (and the 3D orbit to its default
    /// isometric angles).
    ResetView,
    /// User submitted a value for the current dimension prompt.
    ApplyDimension(f32),
    /// User skipped the current dimension prompt.
    SkipDimension,
    /// User submitted a distance for the current POSITION prompt: how far the
    /// carved phase/step area (or its cut wall) sits from the highlighted
    /// storey-outline reference wall, along the prompt's axis.
    ApplyPositionDimension(f32),
    /// User skipped the current position prompt — accept the drawn position.
    SkipPositionDimension,
    /// User asked to measure the area's position from a different outline
    /// reference wall — advance to the next candidate (wraps).
    CyclePositionReference,
    /// User dragged the curve-radius slider: set the provisional arc's bulge
    /// (signed sagitta / chord). Updates the amber preview live.
    SetCurveBulge(f32),
    /// User accepted the provisional curve's radius — bake it into the plan.
    CommitCurve,
    /// Toggle living-area mode: when on, DrawPerpWalls draws a new living-area
    /// sub-shape and the other tools edit the most-recent one.
    SetLivingAreaMode(bool),
    /// A living-area DrawPerpWalls stroke (world coords) — starts a new living
    /// area inside the storey, drawn like the outline.
    LivingAreaStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// Delete the living area at the given index (living areas are non-editable;
    /// redraw to change).
    DeleteLivingArea(usize),
    /// Tap (world coords) picking which region of a pending cut becomes the
    /// living area. A tap outside both candidate regions cancels.
    ChooseCutRegion {
        world_x: f64,
        world_y: f64,
    },
    /// Toggle construction-phase tagging mode: when on, DrawPerpWalls carves an
    /// area to assign a construction phase (year of construction) to.
    SetPhaseMode(bool),
    /// A phase-tagging DrawPerpWalls stroke (world coords) — carves an area (or
    /// cuts the storey) to be tagged with a construction phase.
    PhaseRegionStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// User picked a construction phase for the pending carved area.
    AssignPhase(ConstructionPhase),
    /// Tap (world coords) picking which region of a pending phase cut to tag.
    ChoosePhaseCutRegion {
        world_x: f64,
        world_y: f64,
    },
    /// Discard the half-finished phase pick without tagging.
    CancelPendingPhase,
    /// Delete the current storey's phase region at the given index.
    DeletePhaseRegion(usize),
    /// Stamp the storey's leftover fabric (outline minus the phase areas drawn)
    /// as explicit `Original` region(s) — the "everything else is the original
    /// build" one-tap that satisfies the Construction-phase checklist.
    AssignRestOriginal,
    /// Toggle floor-step tagging mode: when on, DrawPerpWalls carves an area
    /// whose floor is raised or lowered relative to the storey datum.
    SetStepMode(bool),
    /// A step-tagging DrawPerpWalls stroke (world coords) — carves an area (or
    /// cuts the storey) whose floor level will be raised/lowered.
    StepRegionStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// User entered a signed floor level (metres; + raised, − lowered) for the
    /// pending carved step area.
    AssignStepLevel(f32),
    /// Tap (world coords) picking which region of a pending step cut to level.
    ChooseStepCutRegion {
        world_x: f64,
        world_y: f64,
    },
    /// Discard the half-finished step pick without assigning a level.
    CancelPendingStep,
    /// Delete the current storey's step region at the given index.
    DeleteStepRegion(usize),
    /// Toggle ceiling-step mode: when on, a stroke draws an axis-aligned line
    /// across the plan (or a tap places a point + H/V choice) and the ceiling
    /// steps on one side of it.
    SetCeilingStepMode(bool),
    /// A ceiling-step stroke (world coords) — fitted to an axis-aligned line.
    CeilingStepStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// Tap (world coords) in the ceiling-step flow: places the line's point
    /// when nothing is pending, or picks the stepping side of a pending line.
    CeilingStepTap {
        world_x: f64,
        world_y: f64,
    },
    /// User picked the orientation of a tapped ceiling-step point
    /// (a horizontal line's position varies on Y).
    ChooseCeilingStepOrientation {
        horizontal: bool,
    },
    /// User entered the ceiling-step amount (metres, signed: the Up/Down
    /// buttons own the sign; the typed value contributes |v|).
    AssignCeilingStepDelta(f32),
    /// "Keep same height": split the pending line/area into two same-height
    /// pieces (a divider, no riser) — routed to `keep_ceiling_same_height`
    /// (a 0.0 delta). Bounds a ridge's own area without a step.
    KeepCeilingSameHeight,
    /// "Don't snap" on the pending ceiling-step line's snap tag: revert the
    /// line to its drawn coordinate. Routed to the controller's dedicated
    /// `unsnap_ceiling_step_line` — NOT `ChooseSnap`/`UnsnapLast`, whose
    /// rewind-and-replay machinery must not touch a line outside history.
    UnsnapCeilingStepLine,
    /// Discard the in-flight ceiling-step line at any stage.
    CancelPendingCeilingStep,
    /// Delete the current storey's ceiling-step region identified by its world
    /// ring. A ring (not a list index) because the displayed feed is
    /// divider-FILTERED — its indices don't map to the raw region Vec; the
    /// controller maps the ring back to the raw index.
    DeleteCeilingStepRegion(Vec<(f64, f64)>),
    /// Toggle roof-point mode: when on, tapping a point inside a ceiling area
    /// starts the apex flow (per-axis position questions, then the height).
    SetCeilingPointMode(bool),
    /// Tap (world coords) in roof-point mode placing the apex's position.
    CeilingPointTap {
        world_x: f64,
        world_y: f64,
    },
    /// User entered the apex height (metres, ABSOLUTE above the storey floor
    /// datum) for the pending roof point.
    AssignCeilingApexHeight(f32),
    /// "Don't snap" on the pending roof point's snap tag: revert BOTH axes to
    /// their tapped coordinates and restart the position questions. Routed to
    /// the controller's dedicated `unsnap_roof_point` — NOT
    /// `ChooseSnap`/`UnsnapLast`, whose rewind-and-replay machinery must not
    /// touch a point outside history.
    UnsnapCeilingPoint,
    /// Discard the in-flight roof point at any stage.
    CancelPendingCeilingPoint,
    /// Delete the current storey's roof apex at the given index.
    DeleteCeilingApex(usize),
    /// A roof-RIDGE stroke (world coords) drawn in roof-point mode — fitted
    /// to an axis-aligned segment placing TWO apexes at once (stroke = ridge,
    /// tap = point: the ceiling-step stroke/tap routing pattern).
    CeilingRidgeStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// User entered the ridge LENGTH (metres along the ridge axis — the
    /// extent becomes `[start, start + length]`) for the pending roof ridge.
    AssignCeilingRidgeLength(f32),
    /// Skip the pending roof ridge's length question — accept the drawn
    /// length as-is.
    SkipCeilingRidgeLength,
    /// User entered the ridge height (metres, ABSOLUTE above the storey floor
    /// datum — both endpoints get it) for the pending roof ridge.
    AssignCeilingRidgeHeight(f32),
    /// Pick which area a boundary ridge slopes (the ChooseArea stage) — index
    /// into the offered options.
    ChooseCeilingRidgeArea(usize),
    /// The 'Centre' button on an open roof location stage (ridge cross /
    /// start): apply the computed centre-it-within-its-ceiling-area value
    /// through the same path as a typed distance.
    CentreRoofPosition,
    /// The 'Meet other edge' button on the ridge HEIGHT stage: set the ridge to
    /// the raised neighbour's ceiling height so the sloped area meets it flush.
    MeetOtherEdgeRidgeHeight,
    /// User entered a SIDE LENGTH (metres) for the pending roof RECT's open
    /// length stage (LenX, then LenY — the closed-stroke flat-top flow).
    AssignCeilingRectLength(f32),
    /// Skip the pending roof rect's open side-length question — accept the
    /// drawn length as-is.
    SkipCeilingRectLength,
    /// User entered the flat top's height (metres, ABSOLUTE above the storey
    /// floor datum — all four corners get it) for the pending roof rect.
    AssignCeilingRectHeight(f32),
    /// Discard the in-flight roof rect at any stage.
    CancelPendingCeilingRect,
    /// "Don't snap" on the pending roof ridge's snap tag: revert the cross
    /// coordinate to its drawn value and reopen the position question. Routed
    /// to the controller's dedicated `unsnap_roof_ridge` — NOT
    /// `ChooseSnap`/`UnsnapLast`, whose rewind-and-replay machinery must not
    /// touch a ridge outside history.
    UnsnapCeilingRidge,
    /// Discard the in-flight roof ridge at any stage.
    CancelPendingCeilingRidge,
    /// Toggle wall-exposure mode: when on, tapping an outline wall selects it
    /// (and the section handles), the type buttons mark the selected range.
    SetExposureMode(bool),
    /// Primary press in exposure mode: grab a selection handle if near one,
    /// else select the wall under the pointer.
    ExposurePointerDown {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Drag the grabbed selection handle along the wall. `tol` is the
    /// screen-scaled snap tolerance (world metres) — the handle snaps to
    /// phase lines / section edges / wall ends within it.
    ExposurePointerDrag {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Release the selection handle.
    ExposurePointerUp,
    /// Set the section selection range directly (typed-distance path).
    SetExposureSectionRange {
        start_m: f32,
        end_m: f32,
    },
    /// Assign an exposure type to the current section selection.
    SetWallExposure(WallExposure),
    /// Remove all exposure sections from the selected wall.
    ClearWallExposure,
    /// Mark every outline wall not yet set as exposed.
    SetAllRemainingExposed,
    // ── Wall-face partition ──────────────────────────────────────────────────
    /// Open the selected exposure wall's unrolled face for partitioning (routes
    /// the canvas to the partition overlay).
    OpenWallPartition,
    /// Close the partition overlay (back to the wall-exposure plan).
    CloseWallPartition,
    /// Append a point to the in-flight partition stroke (drag), wall-local
    /// `(along, height)`.
    PartitionStrokePoint {
        along_m: f32,
        height_m: f32,
    },
    /// Finish the in-flight partition stroke (drag end): snap/commit it as a cut.
    PartitionStrokeEnd,
    /// Tap inside a face region (wall-local) to select it for exposure marking.
    PartitionSelectRegion {
        along_m: f32,
        height_m: f32,
    },
    /// Set the exposure of the currently-selected partition region.
    SetPartitionRegionExposure(WallExposure),
    /// Supply the next free coordinate's typed dimension for the in-flight cut.
    PartitionDimEntry(f32),
    /// "Don't snap" on a partition-cut endpoint's snap tag: revert that
    /// endpoint's snapped coordinates to the drawn values and reopen their
    /// dimension prompts. Routed to the controller's dedicated
    /// `unsnap_partition_end` — NOT `ChooseSnap`/`UnsnapLast`, whose
    /// rewind-and-replay machinery must not touch a cut outside history.
    UnsnapPartitionEnd {
        end_b: bool,
    },
    /// Skip dimensioning the in-flight cut's next free coordinate.
    PartitionDimSkip,
    /// Remove the active wall's partition entirely (back to a single face).
    ClearWallPartition,
    // ── Windows/doors ──────────────────────────────────────────────────────
    /// Toggle windows/doors mode.
    SetOpeningsMode(bool),
    /// Pick one of the 6 opening tools from the toolbar — enters openings mode
    /// (if needed) and selects the tool in one step.
    SelectOpeningTool(OpeningToolKind),
    /// Plan tap in openings mode: pick the nearest wall/roof (or open the
    /// tapped wall's elevation in Edit).
    OpeningsPointerDown {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Place the in-flight opening from the floating box's measurements. `width_m`
    /// / `height_m` are the opening (for door+sidelights, the DOOR leaf);
    /// `door_sidelights` carries that tool's mode-specific extras (sidelights +
    /// transom), `None` for every other tool. Sill defaults to 1 m.
    PlaceOpening {
        width_m: f32,
        height_m: f32,
        door_sidelights: Option<DoorSidelightsPlacement>,
        /// The chosen complex-window shape + its dims (`None` for every other
        /// tool). When `Some`, the controller derives width/height/shape from it.
        complex_window: Option<ComplexWindowPlacement>,
        /// Which shared window type the placed window joins (an existing id, or a
        /// new attribute set find-or-created on Place). Ignored for a plain Door.
        window_type: WindowTypeChoice,
        /// Per-window overshading (DEAP solar-gain input), defaulting to Average.
        overshading: Overshading,
        /// Source paths of photos picked in the box (each with the capture
        /// tier its button asked for — see the TEMPORARY dual hi/lo buttons,
        /// `photo_button_pair`), attached to the window(s) this placement
        /// creates. Ignored for a plain Door.
        photos: Vec<(std::path::PathBuf, crate::ui::widgets::PhotoQuality)>,
    },
    /// Abandon the in-flight placement.
    CancelOpeningPlacement,
    /// First-field shortcut: turn the (empty) host wall into a full-surface
    /// "window wall" — no measurements; the whole face becomes one window.
    FillOpeningSurface {
        /// Which shared window type the window-wall joins (see [`Self::PlaceOpening`]).
        window_type: WindowTypeChoice,
        /// Per-window overshading (see [`Self::PlaceOpening`]).
        overshading: Overshading,
        /// Photos to attach to the window-wall (see [`Self::PlaceOpening`]).
        photos: Vec<(std::path::PathBuf, crate::ui::widgets::PhotoQuality)>,
    },
    /// Plan drag / release while a ROOF window is grabbed (Edit-on-surface).
    OpeningsPointerDrag {
        world_x: f64,
        world_y: f64,
    },
    OpeningsPointerUp,
    /// Confirm / cancel the pending delete of the selected opening.
    ConfirmDeleteOpening,
    CancelDeleteOpening,
    /// Elevation overlay press / drag / release at wall-local `(along, height)`.
    ElevationPointerDown {
        along_m: f32,
        height_m: f32,
    },
    ElevationPointerDrag {
        along_m: f32,
        height_m: f32,
    },
    ElevationPointerUp,
    /// Delete the opening selected in the elevation.
    DeleteSelectedOpening,
    /// Open / close the Stage-1 (Mapping) REVIEW takeover view.
    OpenMappingReview,
    CloseMappingReview,
    /// Step the review's side cursor (+1 next / −1 previous, wraps).
    StepMappingReviewSide(i32),
    /// Confirm "all's good": mark the dwelling Mapping-reviewed (unlocks
    /// Structure) and close the review.
    ConfirmMappingReview,
    /// Review elevation press / drag / release at composite `(x, z)` (metres).
    MappingReviewPointerDown {
        x: f64,
        z: f64,
    },
    MappingReviewPointerDrag {
        x: f64,
        z: f64,
    },
    MappingReviewPointerUp,
    /// Open / close the Stage-2 (Structure) REVIEW takeover view.
    OpenStructureReview,
    CloseStructureReview,
    /// Step the review's item cursor (+1 next / −1 previous, wraps).
    StepStructureReview(i32),
    /// Confirm "all's good": record the dwelling Structure-reviewed and close.
    ConfirmStructureReview,
    /// Phase-3 (Objects): tap the plan to drop / open a room pin at world `(x, y)`.
    ObjectsPointerDown {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Phase-3 (Objects): a 2D tap on the ALL-STOREYS tab — the cell it landed in
    /// resolves the target storey, and the point is already in that storey's own
    /// frame. Drops / opens a plan pin on THAT storey.
    ObjectsPointerDownAllStoreys {
        storey_idx: usize,
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Phase-3 (Objects): tap a SURFACE in a 3D view — the canvas has already
    /// resolved the screen point to the closest viewer-facing surface
    /// ([`crate::ui::render::pick_3d`]). Drops / opens a room pin ON that
    /// surface: `z_m` is the hit's height above storey `storey_idx`'s floor
    /// datum; `pitch_deg` is the surface's pitch when it's sloped (a roof
    /// plane / sloped ceiling), shown in the add-object dialog.
    ObjectsPointerDown3D {
        storey_idx: usize,
        world_x: f64,
        world_y: f64,
        z_m: f64,
        pitch_deg: Option<f32>,
        tol: f64,
    },
    /// Close the open object room (the panel's "Done").
    CloseObjectRoom,
    /// Rename the open object room.
    RenameObjectRoom(String),
    /// Add an object to the open room: its kind, count, optional note, and
    /// photos (each with its capture tier — see [`Self::PlaceOpening`]).
    AddObject {
        kind: ObjectKind,
        count: u32,
        note: Option<String>,
        photos: Vec<(std::path::PathBuf, crate::ui::widgets::PhotoQuality)>,
    },
    /// Delete one object from a room (the Delete panel).
    DeleteObject {
        room: ItemId,
        object: ItemId,
    },
    /// Delete a whole object room pin (the Delete panel).
    DeleteObjectRoom(ItemId),
    /// Open / close the Objects REVIEW takeover (house-wide counts + 3D hotspots).
    OpenObjectReview,
    CloseObjectReview,
    /// Select an object-kind row in the review (by index) — drives the hotspots.
    SelectObjectReviewKind(usize),
    /// Review absence prompt: confirm an empty category really has no items.
    ConfirmObjectAbsent(ObjectCategory),
    /// Review absence prompt: skip this category for now (there is one to add).
    SkipObjectAbsent(ObjectCategory),
    /// Confirm the Objects review ("all reviewed & OK") — records it and closes.
    ConfirmObjectReview,
    /// Phase-4 form: set the floor / wall-finish / heating values.
    SetFinalFloor(FloorKind),
    SetFinalWallFinish(WallFinishField, WallFinish),
    SetFinalHeating(HeatingKind),
    /// Phase-4 north-align: one frame of moving / zooming / rotating the image
    /// over the fixed plan (pan in canvas px, zoom factor, rotation degrees).
    NorthImageGesture {
        pan_dx: f32,
        pan_dy: f32,
        zoom_factor: f32,
        rotate_deg: f32,
    },
    /// Recenter the north-align image (reset pan + zoom).
    ResetNorthImage,
    /// Phase-4 sheltered sides: advance one side's shelter category on a tap.
    CycleShelter(ExternalSide),
    /// Phase-4: switch the view sub-mode (form / align / sheltered).
    SetFinalMode(FinalMode),
    /// Phase-4 north-align: finish aligning (records north, back to form).
    ConfirmAlign,
    /// DEV: attach a generated fake satellite image to practice aligning.
    LoadTestSatellite,
    /// Phase-4: confirm the final review (BER complete) — raises the popup.
    ConfirmFinalReview,
    /// Completion popup: "Go back" (dismiss) / "Take me to summary" (open).
    DismissBerComplete,
    OpenSummary,
    /// Summary: "Back to phases" (close the summary).
    CloseSummary,
    /// Close the elevation overlay.
    CloseElevation,
    /// User entered a default ceiling height (m) in the floating number box
    /// (opened from the Heights category): stamp the current scope (overwriting
    /// this storey's value, or filling every unset storey from the All tab).
    /// Stays in the current view — 3D is the floating canvas button's separate
    /// action. A successful set closes the entry box.
    SetDefaultCeilingHeight(f32),
    /// Open the floating default-ceiling-height entry (the Heights tool button).
    OpenCeilingHeightEntry,
    /// Close the floating default-ceiling-height entry without setting (its
    /// "Close" button).
    CloseCeilingHeightEntry,
    /// Switch the editor onto a specific storey, or `None` for the read-only
    /// "All storeys" overview tab.
    SetCurrentStorey(Option<usize>),
    /// Append a fresh empty storey and switch the editor onto it (method 2:
    /// the previous storey is shown as a watermark while the new one is drawn).
    AddStorey,
    /// Delete the CURRENT storey (top storey only, never storey 0) and drop
    /// back to the one below — the "drew it badly, start again" path. Emitted
    /// by the tab row's armed confirm.
    DeleteStorey,
    /// New-storey flow: the user answered the THICKNESS question — the
    /// floor-slab thickness to the storey below (metres). Stamps the durable
    /// value and advances the flow to the method choice.
    AssignFloorThickness(f64),
    /// Heights-category re-edit of the floor-slab thickness (storey ≥ 1).
    /// Same stamp as `AssignFloorThickness` but never touches the flow stage.
    SetFloorThickness(f64),
    /// New-storey flow: the user chose to DRAW the outline (also emitted by
    /// the Browse stage's "Draw instead" button).
    ChooseDrawOutline,
    /// New-storey flow: the user chose to browse SUGGESTED outlines derived
    /// from the storey below.
    ChooseSuggestOutlines,
    /// Browse stage: advance to the next suggestion (wraps).
    NextSuggestion,
    /// Browse stage: go back to the previous suggestion (wraps).
    PrevSuggestion,
    /// Browse stage: accept the current suggestion as the storey's
    /// fully-finalised outline (no review, no dimension prompts).
    AcceptSuggestion,
    /// Switch the workspace onto another assessment phase (clicked in the phase
    /// signpost). The controller gates which phases are reachable.
    SetAssessmentPhase(AssessmentPhase),
    /// Structure phase: TOGGLE a MODE (Walls/Ceilings/Floors) in/out of the active
    /// set — the buttons are non-exclusive, so several kinds can be active at
    /// once. The controller shades every active kind (≥1 always stays active).
    SetStructureKind(SurfaceKind),
    /// Structure phase: set (or clear with `None`) the active global Select
    /// subtool. It acts on the current mode's surface kind.
    SetStructureSelectMode(Option<SelectMode>),
    /// Structure phase (Select subtool active): the assessor tapped the canvas
    /// — toggle the surface under the point in/out of the selection.
    StructureSurfaceTap {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Structure phase, ALL-STOREYS tab: toggle the surface under a tap that
    /// landed in storey `storey_idx`'s cell (coords already in that storey's
    /// own world frame — the canvas inverted the cell's transform).
    StructureSurfaceTapAllStoreys {
        storey_idx: usize,
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Structure phase: clear every selected surface ("Deselect all").
    DeselectAllStructure,
    /// Structure phase (Carve subtool): a drawn stroke in world coords. Carves
    /// the storey like a living-area cut; the surfaces in the resulting region
    /// get selected (a straight cut first asks which half via
    /// `ChooseStructureCutRegion`).
    StructureCarveStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// Structure phase (Carve subtool) on the ALL-STOREYS tab: one drawn
    /// LASSO stroke, converted to EVERY storey's own world frame (index =
    /// storey index; each all-storeys cell has its own screen↔world
    /// transform). The controller closes the ring, clips it per storey, and
    /// stages the clipped regions as a cross-storey selection.
    StructureLassoStrokeWorld {
        per_storey: Vec<Vec<(f32, f32)>>,
    },
    /// Structure phase: pick which carved candidate region to select from.
    ChooseStructureCutRegion {
        world_x: f64,
        world_y: f64,
    },
    /// Structure phase: answer the wall-inclusion ask for the current
    /// highlighted carve edge — `true` keeps the wall along it in the staged
    /// selection, `false` leaves it out. Answering the last edge stages the
    /// carve.
    ResolveWallInclusion {
        include: bool,
    },
    /// Structure phase: open the "Edit attributes" session over the current
    /// selection — groups it by shared construction (walls / floors / ceilings).
    BeginStructureEdit,
    /// Structure phase: make attribute-group `usize` the active popup.
    SetActiveStructureGroup(usize),
    /// Structure phase: set a group's construction (kind-tagged type + make-up);
    /// applies to that group's surfaces.
    SetStructureGroupAttrs {
        group: usize,
        ty: ber_model::ber::structural::ConstructionType,
        makeup: ber_model::ber::structural::ConstructionMakeup,
        phase: ber_model::ber::common::ConstructionPhase,
    },
    /// Structure phase: rename attribute-group `group`'s editable label.
    SetStructureGroupName {
        group: usize,
        name: String,
    },
    /// Structure phase: "New group" on a FRESH group box — skip the
    /// existing-group offer and open the full attribute editor for `group`.
    BeginNewStructureGroup {
        group: usize,
    },
    /// Structure phase: RESET a session-fresh group box back to the two-mode
    /// entry — detaches this selection's attribution (the shared construction
    /// itself is untouched) and clears the session mirror.
    ResetStructureGroup {
        group: usize,
    },
    /// Structure phase: set a group's boundary-condition EXPOSURE (kind-tagged);
    /// applies to that group's surfaces.
    SetStructureGroupExposure {
        group_index: usize,
        exposure: ber_model::ber::structural::SurfaceExposure,
    },
    /// Structure phase: open the build-up LAYER editor for `group` ("Specify…").
    BeginSpecifyLayers {
        group: usize,
    },
    /// Structure phase: close the build-up layer editor.
    EndSpecifyLayers,
    /// Structure phase: replace `group`'s build-up layers (pick / remove /
    /// reorder in the layer editor).
    SetStructureGroupLayers {
        group: usize,
        layers: Vec<ber_model::ber::structural::ConstructionLayer>,
    },
    /// Structure phase: set (or clear with `None`) a CEILING group's insulation
    /// spec — the attribute dialog's "Insulation thickness (mm)" box and its
    /// "unknown" checkbox.
    SetStructureGroupInsulation {
        group: usize,
        insulation: Option<ber_model::ber::structural::CeilingInsulation>,
    },
    /// Structure phase: open / close the PHOTOS view for a group (camera button).
    BeginViewPhotos {
        group: usize,
    },
    EndViewPhotos,
    /// Structure phase: attach a photo (from `path`) to a group's construction
    /// (the photos view's `+`). The controller recompresses the file into the
    /// store at the call site's `quality` tier.
    AddGroupPhoto {
        group: usize,
        path: std::path::PathBuf,
        quality: crate::ui::widgets::PhotoQuality,
    },
    /// Structure phase: close the "Edit attributes" session.
    EndStructureEdit,
    // ── Floor exposure ───────────────────────────────────────────────────────
    /// Toggle the floor-exposure tool. On enter the storey-count gate is raised
    /// (the overlaid view + selection wait for `ConfirmAllStoreysAdded`).
    SetFloorExposureMode(bool),
    /// Gate answer "Yes, finished adding all storeys" — lowers the gate and
    /// reveals the overlaid floor view (inter-storey surfaces are already
    /// derived by the pipeline).
    ConfirmAllStoreysAdded,
    /// Gate answer "Not yet" — exit the tool to add the remaining storeys.
    DeferStoreyCount,
    /// Set (or clear with `None`) the floor-exposure Select subtool.
    SetFloorExposureSelectMode(Option<SelectMode>),
    /// Floor-exposure Click subtool: a tap (world coords) toggling the floor
    /// region under it (across the whole stack) in/out of the selection.
    FloorExposureTap {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Floor-exposure Carve subtool: a drawn stroke (world coords) selecting
    /// every floor region it encloses across the stack.
    FloorExposureCarveStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// Floor-exposure Carve subtool: while a straight-cut split is awaiting a
    /// side pick, the assessor tapped — choose the half (world coords) to
    /// dimension + split.
    ChooseFloorExposureCutRegion {
        world_x: f64,
        world_y: f64,
    },
    /// Clear every selected floor region ("Deselect all").
    DeselectAllFloorExposure,
    /// Assign a floor exposure to the current floor-region selection.
    AssignFloorExposure(ber_model::ber::structural::FloorExposure),
    // ── Ceiling exposure ──────────────────────────────────────────────────────
    /// Toggle the ceiling-exposure tool. On enter the storey-count gate is raised
    /// (the overlaid view + selection wait for `ConfirmAllStoreysAddedCeiling`).
    SetCeilingExposureMode(bool),
    /// Gate answer "Yes, finished adding all storeys" — lowers the gate and
    /// reveals the overlaid ceiling view (inter-storey surfaces are already
    /// derived by the pipeline).
    ConfirmAllStoreysAddedCeiling,
    /// Gate answer "Not yet" — exit the tool to add the remaining storeys.
    DeferStoreyCountCeiling,
    // ── Roof (Phase-1 roof step) ───────────────────────────────────────────────
    /// Toggle the Phase-1 roof step. On enter the storey-count gate is raised
    /// (the whole-house context waits for `ConfirmAllStoreysAddedRoof`).
    SetRoofMode(bool),
    /// Gate answer "Yes, finished adding all storeys" — reveals the whole-house
    /// top-storey-shaded roof context.
    ConfirmAllStoreysAddedRoof,
    /// Gate answer "Not yet" — exit the roof step to add the remaining storeys.
    DeferStoreyCountRoof,
    /// Roof step: a tap (world coords) toggling the ceiling region under it in/out
    /// of the roof-area-in-progress.
    RoofRegionTap {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Roof step: pick the shape for the grouped roof area (Flat commits; Gable/Hip
    /// start the ridge-draw flow).
    PickRoofShape(crate::controller::app_controller::RoofShape),
    /// Roof step: a ridge stroke (world coords) for the grouped area — a gable's
    /// single ridge, or one of a hip's several.
    RoofAreaRidgeStroke {
        points: Vec<(f32, f32)>,
    },
    /// Roof step (hip): done drawing ridges — move to the pitch question.
    FinishRoofRidges,
    /// Roof step (gable): re-centre the placed ridge between the eaves.
    CentreRoofRidge,
    /// Roof step: assign the pitch (degrees) to the placed ridge(s).
    AssignRoofPitch(f32),
    /// Roof step: discard the in-flight per-area drawing (keeps the grouping).
    CancelRoofArea,
    /// Set (or clear with `None`) the ceiling-exposure Select subtool.
    SetCeilingExposureSelectMode(Option<SelectMode>),
    /// Ceiling-exposure Click subtool: a tap (world coords) toggling the ceiling
    /// region under it (across the whole stack) in/out of the selection.
    CeilingExposureTap {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Ceiling-exposure Carve subtool: a drawn stroke (world coords) that
    /// dimensions a carve, splitting the ceilings across the stack.
    CeilingExposureCarveStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// Ceiling-exposure Carve subtool: while a straight-cut split is awaiting a
    /// side pick, the assessor tapped — choose the half (world coords) to
    /// dimension + split.
    ChooseCeilingExposureCutRegion {
        world_x: f64,
        world_y: f64,
    },
    /// Clear every selected ceiling region ("Deselect all").
    DeselectAllCeilingExposure,
    /// Assign a ceiling exposure to the current ceiling-region selection.
    AssignCeilingExposure(ber_model::ber::structural::CeilingExposure),
    // ── External photos ───────────────────────────────────────────────────────
    /// Attach (or replace) the external photo for `side`. `path` is the captured
    /// image the shared `photo_button` widget returned (desktop file dialog /
    /// Android camera — the widget owns that platform split). The controller
    /// recompresses the file into the store at `quality`, staying camera/
    /// file-dialog agnostic — it only ever sees a path. Mirrors `AddGroupPhoto`.
    AddExternalPhoto {
        side: ExternalSide,
        path: std::path::PathBuf,
        quality: crate::ui::widgets::PhotoQuality,
    },
    /// Remove the external photo for `side`.
    RemoveExternalPhoto {
        side: ExternalSide,
    },
    /// Rotate `side`'s external photo a quarter turn (`clockwise` or anti-) so
    /// the assessor can set which way is up. Handled by the controller, which
    /// bakes it into a new stored file.
    RotateExternalPhoto {
        side: ExternalSide,
        clockwise: bool,
    },
    /// Open the paintbrush ANNOTATION takeover for `side`'s external photo
    /// (the "🖌" affordance wherever external photos are shown). Handled by the
    /// app shell, not the controller: it decodes the stored image into the
    /// page-state annotator (`SketchState::photo_annotator`).
    BeginAnnotateExternalPhoto {
        side: ExternalSide,
    },
    /// Save the annotation session: `jpeg` is the stored image re-encoded with
    /// the strokes burned in at image resolution. The controller writes it into
    /// the photo store and REPLACES `side`'s external photo.
    AnnotateExternalPhoto {
        side: ExternalSide,
        jpeg: Vec<u8>,
    },
}

/// Everything the Sketch page needs for one frame, built by the app shell
/// (`BerApp`) from controller view data plus the page's own backing state.
/// `&mut` fields are the cross-frame text-input buffers and the category UI
/// state living on `SketchState`.
pub struct SketchView<'a> {
    pub current: Option<EguiStroke>,
    pub mode: SketchMode,
    pub display: DisplayMode,
    pub edit_tool: EditTools,
    /// CAD-style navigation mode: no tool selected — canvas clicks/drags only
    /// move the view (pan/orbit), placing/drawing nothing.
    pub navigation: bool,
    pub ber: Option<&'a Ber>,
    pub current_storey_idx: Option<usize>,
    pub review: Option<&'a CollinearitySuggestion>,
    pub review_remaining: usize,
    pub has_floor_plan: bool,
    pub pending_redraw: bool,
    pub dim_prompt: Option<DimensionPromptUiData>,
    pub dim_input: &'a mut String,
    pub dim_scale_set: bool,
    pub dim_error: Option<&'a str>,
    pub dimensioned_walls: &'a [(usize, usize)],
    /// Position prompt for a carved phase/step area: how far the area (or its
    /// cut wall) sits from a storey-outline wall. Shown once the area's
    /// wall-length loop is done; both walls are highlighted on the canvas.
    pub position_prompt: Option<PositionPromptUiData>,
    pub position_input: &'a mut String,
    pub position_error: Option<&'a str>,
    /// World-coord points of a provisional ExtendCurve/TrimCurve arc awaiting
    /// its attach dimensions, drawn as an amber preview overlay.
    pub arc_preview: Option<&'a [(f64, f64)]>,
    /// Current bulge of a provisional curve that's ready to have its radius
    /// tuned (attach lines set). `Some` shows the radius slider toolbar.
    pub curve_bulge: Option<f32>,
    /// Living-area drawing mode + the resolved living-area outlines (world
    /// coords) to overlay in a distinct colour; `active_living_area` is the
    /// one currently edited (highlighted).
    pub living_area_mode: bool,
    pub living_area_polygons: &'a [Vec<(f64, f64)>],
    /// Per area: known-length walls as (start, end, length) world coords.
    pub living_area_wall_labels: &'a [Vec<((f64, f64), (f64, f64), f32)>],
    pub active_living_area: Option<usize>,
    /// The two candidate regions of a pending cut (world coords); when
    /// non-empty the user taps one to pick it as the living area.
    pub pending_cut_regions: &'a [Vec<(f64, f64)>],
    /// Wall-exposure mode + the outline perimeter walls (world coords) with
    /// their exposure sections, the current section selection on the selected
    /// wall, and the typed start/end input buffers.
    pub exposure_mode: bool,
    pub exposure_walls: &'a [WallExposureView],
    pub exposure_selection: Option<ExposureSelection>,
    pub exposure_start_input: &'a mut String,
    pub exposure_end_input: &'a mut String,
    /// The active storey's derived RISER walls (dashed 2D lines; selectable
    /// in exposure mode — the type buttons then act on the selected riser).
    pub riser_views: &'a [RiserView],
    /// Wall-face partition mode + its view (the unrolled face, cuts, regions,
    /// plan inset, and dimension prompt). `partition_mode` routes the canvas to
    /// the partition overlay; `partition_view` is `Some` exactly then. The
    /// dimension-entry buffer backs the dock's metre input.
    pub partition_mode: bool,
    pub partition_view: Option<&'a PartitionView>,
    pub partition_dim_input: &'a mut String,
    /// Construction-phase (year-of-construction) tagging: mode flag, the
    /// committed regions `(ring, label)` to tint, the carved area awaiting a
    /// phase pick, the cut candidates to tap between, the phase choices for
    /// the picker, and whether a pick is currently pending.
    pub phase_mode: bool,
    pub phase_region_polygons: &'a [PhaseDisplayRegion],
    /// Delete-panel row labels, index-aligned with the RAW session phase regions
    /// (what `DeletePhaseRegion(i)` indexes) — distinct from the display zones
    /// above, which are unioned per phase.
    pub phase_delete_labels: &'a [String],
    pub pending_phase_polygon: &'a [(f64, f64)],
    pub pending_phase_cut_regions: &'a [Vec<(f64, f64)>],
    pub phase_choices: &'a [(ConstructionPhase, String)],
    pub phase_pick_pending: bool,
    /// Floor-step tagging: mode flag, the committed step regions `(ring,
    /// label)` to tint, the carved area awaiting its level, the cut candidates
    /// to tap between, whether a level entry is pending, and the backing
    /// string for the amount input.
    pub step_mode: bool,
    pub step_region_polygons: &'a [(Vec<(f64, f64)>, String)],
    pub pending_step_polygon: &'a [(f64, f64)],
    pub pending_step_cut_regions: &'a [Vec<(f64, f64)>],
    pub step_pick_pending: bool,
    pub step_amount_input: &'a mut String,
    /// Last error from the step-level assign, shown inline in the step toolbar.
    pub step_error: Option<&'a str>,
    /// Ceiling-step tagging: mode flag + availability gate (needs a default
    /// ceiling height + finalised outline), the committed step regions
    /// `(ring, label)` to tint, the pending line + its two side half-rects,
    /// the per-stage flags (orientation pick / side tap / amount entry),
    /// whether ANY stage is pending, and the amount input's backing string.
    pub ceiling_step_mode: bool,
    pub ceiling_step_available: bool,
    pub ceiling_step_region_polygons: &'a [(Vec<(f64, f64)>, String)],
    /// Same-height divider cut segments (world coords) — drawn as a faint
    /// dashed pass, visually distinct from the amber-solid height steps.
    /// Divider regions are excluded from `ceiling_step_region_polygons`.
    pub ceiling_divider_lines: &'a [((f64, f64), (f64, f64))],
    pub pending_ceiling_step_line: Option<((f64, f64), (f64, f64))>,
    pub pending_ceiling_step_side_rects: &'a [Vec<(f64, f64)>],
    /// The pending line's wall snap as one standard snap tag (its menu
    /// offers only "Don't snap", routed to UnsnapCeilingStepLine).
    pub ceiling_step_snap_tag: Option<&'a SnapTagView>,
    pub ceiling_step_orientation_pending: bool,
    pub ceiling_step_side_pending: bool,
    pub ceiling_step_amount_pending: bool,
    pub ceiling_step_pending: bool,
    pub ceiling_amount_input: &'a mut String,
    /// Last error from the ceiling-step assign, shown inline in its toolbar.
    pub ceiling_step_error: Option<&'a str>,
    /// Roof-point (apex) tagging: mode flag + availability gate (same gate as
    /// the ceiling steps), the committed apexes `(position, label)` to mark
    /// ("▲ 4.20 m"), the pending point's position for the provisional dot,
    /// the per-stage flags (height entry / any stage), and the height input's
    /// backing string. The position stage shows as the shared position panel.
    pub ceiling_point_mode: bool,
    pub ceiling_point_available: bool,
    pub ceiling_apex_views: &'a [((f64, f64), String)],
    pub pending_roof_point: Option<(f64, f64)>,
    /// The pending point's wall snap as one standard snap tag (its menu
    /// offers only "Don't snap", routed to UnsnapCeilingPoint).
    pub ceiling_point_snap_tag: Option<&'a SnapTagView>,
    pub ceiling_point_position_pending: bool,
    pub ceiling_point_height_pending: bool,
    pub ceiling_point_pending: bool,
    pub ceiling_apex_input: &'a mut String,
    /// Roof-RIDGE line input (a stroke in roof-point mode): the committed
    /// ridges `(end_a, end_b, label)` to overlay ("⌃ 4.20 m" /
    /// "4.20→4.50 m"), the pending ridge SEGMENT for the dashed overlay, its
    /// snap tag (menu offers only "Don't snap", routed to UnsnapCeilingRidge),
    /// and the per-stage flags (position questions / length entry / height
    /// entry). The height entry reuses `ceiling_apex_input`; the length entry
    /// has its own buffer (prefilled with the drawn length on stage open).
    pub ceiling_ridge_views: &'a [((f64, f64), (f64, f64), String)],
    pub pending_roof_ridge_line: Option<((f64, f64), (f64, f64))>,
    pub ceiling_ridge_snap_tag: Option<&'a SnapTagView>,
    pub ceiling_ridge_position_pending: bool,
    pub ceiling_ridge_length_pending: bool,
    /// The open length question's extent starts at a SNAPPED wall coordinate
    /// — the panel's "from the highlighted end" hover shows only then.
    pub ceiling_ridge_length_anchored: bool,
    pub ceiling_ridge_height_pending: bool,
    /// On the ridge HEIGHT stage, the raised neighbour's ceiling height when the
    /// ridge abuts one (drives the 'Meet other edge' button + its value); `None`
    /// otherwise, so the button is hidden.
    pub ceiling_ridge_meet_edge_height: Option<f32>,
    pub ceiling_ridge_pending: bool,
    /// When the placed ridge sits on an internal area boundary: the labels of
    /// the areas to choose between (which one slopes). Empty otherwise.
    pub ceiling_ridge_area_options: Vec<String>,
    /// Roof-RECT (flat-topped hip) input — the closed-stroke sibling of the
    /// ridge: committed flat tops `(corners, label)` to overlay, the pending
    /// rect's corners for the dashed overlay, and the per-stage flags. The
    /// open length stage is identified by its AXIS (LenX/LenY share the
    /// ridge length input buffer — the two flows are never pending at once);
    /// the height entry reuses `ceiling_apex_input`.
    pub ceiling_rect_views: &'a [([(f64, f64); 4], String)],
    pub pending_roof_rect_outline: Option<[(f64, f64); 4]>,
    pub ceiling_rect_position_pending: bool,
    pub ceiling_rect_length_axis: Option<manip_plan::ParameterAxis>,
    pub ceiling_rect_height_pending: bool,
    pub ceiling_rect_pending: bool,
    pub ceiling_ridge_length_input: &'a mut String,
    /// Last error from the roof assigns (apex height / ridge length / ridge
    /// height), shown inline in the shared roof toolbar — one field because
    /// the three stages are mutually exclusive within it.
    pub roof_error: Option<&'a str>,
    /// Default ceiling heights: the last error from the Heights field's Set
    /// (shown inline next to it), the current scope's value when it has one
    /// (the field's "2.40 m" display), and whether the scope's heights are
    /// all set (gates the floating 2D/3D toggle). The field's input buffer
    /// lives on `cat_state`.
    pub ceiling_height_error: Option<&'a str>,
    pub default_ceiling_height: Option<f64>,
    pub ceiling_heights_set: bool,
    /// The new-storey flow's prompt (thickness → method choice → suggestion
    /// browse), rendered as the prompt dock's TOP-priority toolbar; while
    /// browsing, the canvas tints the current suggestion's ring over the
    /// previous-storey watermark. `None` when no flow is in progress.
    pub new_storey_prompt: Option<NewStoreyPromptUiData>,
    /// Backing string for the flow's thickness input (prefilled "0.25" when
    /// the Thickness stage opens — the one-shot guard lives on `SketchState`).
    pub new_storey_thickness_input: &'a mut String,
    /// Last error from the new-storey flow's toolbar (thickness assign,
    /// suggest, accept), shown inline in it.
    pub new_storey_error: Option<&'a str>,
    /// The current storey's floor-slab thickness for the Heights field's
    /// "0.30 m" display (`None` on storey 0 / the All tab / unset). The
    /// field's input buffer lives on `cat_state`.
    pub floor_thickness: Option<f64>,
    /// Last error from the Heights thickness field's Set, shown inline.
    pub floor_thickness_error: Option<&'a str>,
    pub edit_selected_wall: Option<((f64, f64), (f64, f64))>,
    pub edit_selected_vertex: Option<(f64, f64)>,
    pub edit_drag_prompt: Option<EditDragPromptUiData>,
    pub edit_drag_input: &'a mut String,
    pub edit_drag_error: Option<&'a str>,
    pub vertex_drop_prompt: Option<VertexDropPromptUiData>,
    pub vertex_drop_inputs: &'a mut [String; 4],
    pub vertex_drop_error: Option<&'a str>,
    pub can_undo: bool,
    pub can_redo: bool,
    pub view_params: ViewParams,
    /// DEBUG: splice-pipeline phases from the last extend/trim —
    /// `(label, world points, closed)` — drawn as a coloured overlay.
    pub splice_debug: &'a [(String, Vec<(f32, f32)>, bool)],
    /// Elective snaps of the last action — `(world_x, world_y, kind, has_alt)`
    /// — drawn as clickable tags.
    pub snap_tags: &'a [SnapTagView],
    /// The category layout's cross-frame UI state (active ribbon tab,
    /// wall-length label visibility, the number-entry input mode). Lives on
    /// `SketchState`.
    pub cat_state: &'a mut categories::CategoryUiState,
    /// Armed delete-storey confirm in the tab row: `Some(idx)` while the
    /// inline "Delete Storey N?" confirm is showing for storey `idx`. Lives on
    /// `SketchState`; the tab row clears it whenever the deletable storey
    /// changes.
    pub delete_storey_armed: &'a mut Option<usize>,
    /// Background Bluetooth laser-meter connection (Leica DISTO), drained by the
    /// number-entry box in Laser mode. Lives on `SketchState`.
    pub laser: &'a mut crate::number_entry::bluetooth::LaserSource,
    /// Whether the floating default-ceiling-height entry box is open (by value —
    /// the shell toggles the backing flag via events). Lives on `SketchState`.
    pub ceiling_entry_open: bool,
    /// Autosave status line for the State category ("saved Ns ago"), derived
    /// by the shell from its revision bookkeeping. `None` hides the label.
    pub autosave_status: Option<String>,
    // ── Windows/doors ──────────────────────────────────────────────────────
    pub openings_mode: bool,
    /// The opening type the toolbar places next (window / roof window / door /
    /// door+sidelights / edit-elevation).
    pub openings_tool: OpeningToolKind,
    /// The measurement fields the floating box should collect for the in-flight
    /// placement (Width + Height together; door+sidelights adds the optional
    /// lights), if a placement is in flight. Window type is defaulted and the
    /// sill defaults to 1 m — both refined later in Edit on surface / the 2D view.
    pub opening_fields_prompt: Option<OpeningFieldsPrompt>,
    /// Inline error for the staged placement, shown in the floating box.
    pub opening_dim_error: Option<&'a str>,
    /// Per-field measurement buffers — the multi-field box binds each by the
    /// matching `opening_fields_prompt` entry (Width, Height, then the lights).
    pub opening_width_input: &'a mut String,
    pub opening_height_input: &'a mut String,
    /// Door+sidelights composer inputs. DETAILED mode: optional left/right
    /// sidelight width + per-side sill + transom height. A sidelight's sill field
    /// is active only when its `*_has_sill` toggle is on.
    pub sidelight_left_input: &'a mut String,
    pub sidelight_left_sill_input: &'a mut String,
    pub sidelight_left_has_sill: &'a mut bool,
    pub sidelight_right_input: &'a mut String,
    pub sidelight_right_sill_input: &'a mut String,
    pub sidelight_right_has_sill: &'a mut bool,
    pub transom_input: &'a mut String,
    pub transom_semicircular: &'a mut bool,
    /// OVERALL mode: the door-leaf W/H typed under the whole-doorset W/H (which
    /// reuse `opening_width_input`/`opening_height_input`), plus which side(s) the
    /// leftover width becomes sidelight glass. The single sill reuses
    /// `sidelight_left_sill_input` (gated by `sidelight_left_has_sill`).
    pub door_width_input: &'a mut String,
    pub door_height_input: &'a mut String,
    pub sidelight_left_present: &'a mut bool,
    pub sidelight_right_present: &'a mut bool,
    /// Complex-window third/fourth dimension buffers (fields 0/1 reuse
    /// `opening_width_input`/`opening_height_input`); the chosen shape kind (a
    /// pinned per-page pick) lives on `cat_state`.
    pub window_shape_c_input: &'a mut String,
    pub window_shape_d_input: &'a mut String,
    /// Window-type picker state for the placement box. `wt_existing_id = Some`
    /// reuses an existing shared type by id; `None` defines a new one from the
    /// glazing/frame/year/cert fields below (deduped on Place). See
    /// [`crate::ui::app::SketchState`].
    pub wt_existing_id: &'a mut Option<ItemId>,
    pub wt_glazing: &'a mut Glazing,
    pub wt_frame: &'a mut Frame,
    pub wt_year_input: &'a mut String,
    pub wt_certified: &'a mut bool,
    pub wt_gap_input: &'a mut String,
    pub wt_name_input: &'a mut String,
    /// Per-window overshading selected in the placement box. See
    /// [`crate::ui::app::SketchState`].
    pub overshading: &'a mut Overshading,
    /// Photos picked in the placement box for the window being placed, each
    /// with the capture tier its button asked for (TEMPORARY dual hi/lo
    /// buttons). See [`crate::ui::app::SketchState`].
    pub pending_window_photos: &'a mut Vec<(std::path::PathBuf, crate::ui::widgets::PhotoQuality)>,
    /// Window types already in use on the record — offered in the placement box's
    /// picker so a new window can JOIN an existing type. Empty when none exist.
    pub existing_window_types: Vec<ExistingWindowType>,
    /// The open per-wall ELEVATION overlay, if any (the unrolled wall + its
    /// draggable openings).
    pub elevation_view: Option<ElevationView>,
    /// A ROOF window is selected in the plan view (Edit-on-surface) — its id, so
    /// the toolbar can offer Delete and the canvas can highlight it.
    pub roof_window_selected: Option<ItemId>,
    /// A delete is awaiting confirmation (show the confirm/cancel controls).
    pub delete_confirm_pending: bool,
    /// Stage A completion checklist rows (Measurements / Heights / Exposures),
    /// recomputed each frame from the editing model — drawn as the floating,
    /// draggable checklist window over the canvas.
    pub checklist: Vec<ChecklistItem>,
    /// External elevation photos: the four sides (Front/Right/Left/Back) in
    /// [`ExternalSide::ALL`] order, each with its current photo or `None`.
    /// Backs the floating "Add external pictures" panel. Shown in the Mapping
    /// phase when a record is loaded.
    pub external_photos: Vec<(ExternalSide, Option<PhotoView>)>,
    /// The assessor's current workflow phase — drives the phase signpost
    /// highlight, the checklist title, and which toolset the left panel shows.
    pub current_phase: AssessmentPhase,
    /// Whether the Mapping phase is complete (every Stage A item green on all
    /// storeys). Enables the "Review" button on the Mapping checklist.
    pub mapping_complete: bool,
    /// Whether the Mapping REVIEW has been confirmed ("all's good") — the gate
    /// that unlocks the Structure phase (replaces raw completeness as the gate).
    pub mapping_reviewed: bool,
    /// The open Mapping-REVIEW takeover view, if active: the current side's
    /// external elevation + its photo. When `Some`, it replaces the plan canvas.
    pub mapping_review_view: Option<ReviewView>,
    /// Whether the whole house's Structure attribution is complete — gates the
    /// Structure-phase "Review" entry.
    pub structure_complete: bool,
    /// Whether the Structure REVIEW has been confirmed (recorded on the record).
    pub structure_reviewed: bool,
    /// Whether the Objects review has been confirmed (gates the Final phase).
    pub objects_reviewed: bool,
    /// Whether the Final-phase review has been confirmed (its ✓).
    pub details_reviewed: bool,
    /// Phase-4 form values (floor / wall finish / heating), for the form panel.
    pub final_details: FinalDetails,
    /// Phase-4 per-side sheltering, for the sheltered-sides step.
    pub sheltering: ber_model::ber::final_details::ShelteringBySide,
    /// Phase-4 dwelling orientation (degrees from north) for the north-align step.
    pub orientation_deg: f32,
    /// Pre-populated north-up site image path (`file://`), if any.
    pub site_satellite_image: Option<String>,
    /// The north-align satellite image + its current transform over the plan
    /// (the canvas overlay), while in Phase-4 step 2. `None` otherwise.
    pub north_image: Option<crate::controller::app_controller::NorthImageView>,
    /// The Phase-4 view sub-mode (form / align / sheltered).
    pub final_mode: FinalMode,
    /// Whether all three Phase-4 parts are done (gates "Mark BER complete").
    pub final_complete: bool,
    /// The Phase-4 "BER complete" popup is up.
    pub show_ber_complete: bool,
    /// The (dummy) summary screen is up.
    pub show_summary: bool,
    /// The open Structure-REVIEW takeover view, if active: the current group's
    /// details + photos + highlight set. When `Some`, it replaces the plan canvas.
    pub structure_review_view: Option<StructureReviewView>,
    /// Phase-3 (Objects): the room pins on the current storey, for the plan
    /// overlay. Empty outside the Objects phase.
    pub object_pins: Vec<ObjectPinView>,
    /// Phase-3 (Objects): EVERY storey's pins as world-3D points (datum +
    /// surface height) — the hotspot markers for the full-house 3D view.
    /// Empty outside the Objects phase.
    pub object_pins_world: Vec<(f64, f64, f64)>,
    /// The open object room's panel view (objects so far + categories present),
    /// or `None` when none is open.
    pub active_object_room: Option<ActiveObjectRoomView>,
    /// Object-entry UI state (the open room's panel): the category chosen (to
    /// show its subtypes), the count / note / "Other" buffers, and the photos
    /// picked for the object being added. See [`crate::ui::app::SketchState`].
    pub object_selected_category: &'a mut Option<ber_model::ber::objects::ObjectCategory>,
    pub object_selected_kind: &'a mut Option<ObjectKind>,
    pub object_count_input: &'a mut String,
    pub object_note_input: &'a mut String,
    pub object_other_name: &'a mut String,
    pub object_pending_photos: &'a mut Vec<(std::path::PathBuf, crate::ui::widgets::PhotoQuality)>,
    /// Object kinds already recorded house-wide — the panel combines this with
    /// each kind's `photo_policy` to decide whether the next instance needs a
    /// photo (drives the auto-camera + OK gate). From `recorded_object_kinds`.
    pub object_recorded_kinds: &'a [ObjectKind],
    /// The kind the camera was already auto-opened for (one-shot latch — see
    /// [`crate::ui::app::SketchState::object_auto_photo_for`]).
    pub object_auto_photo_for: &'a mut Option<ObjectKind>,
    /// The open external-photo ANNOTATION session (paintbrush over a photo), or
    /// `None`. Lives on [`crate::ui::app::SketchState`]; opened by
    /// [`SketchEvent::BeginAnnotateExternalPhoto`], drawn as a takeover window
    /// over the canvas, closed by its own Save/Cancel.
    pub photo_annotator: &'a mut Option<crate::ui::widgets::PhotoAnnotator>,
    /// Whether any object pin exists house-wide — gates the Objects "Review" entry.
    pub objects_present: bool,
    /// The open Objects-REVIEW takeover (house-wide counts + selected-kind 3D
    /// hotspots), or `None`. When `Some`, it replaces the plan canvas.
    pub object_review_view: Option<ObjectReviewView>,
    /// Current storey's wall / roof / floor counts — the Structure-phase tool
    /// read-out.
    pub surface_counts: SurfaceCounts,
    /// Per-kind "attributed/total" Structure progress (in-scope surfaces only)
    /// — the mode buttons' "Walls · 3/12" counters.
    pub structure_progress: crate::controller::app_controller::StructureProgress,
    /// The active Structure-phase MODES (Walls/Ceilings/Floors; non-exclusive), so
    /// the panel shows which are selected.
    pub structure_kinds: std::collections::BTreeSet<SurfaceKind>,
    /// The active global Select subtool, so the panel shows it. `None` = none.
    pub structure_select: Option<SelectMode>,
    /// Region ids selected in the Structure phase — the canvas shades these.
    pub structure_selected: &'a HashSet<ItemId>,
    /// The in-flight "Edit attributes" session (selection grouped by
    /// construction), so the panel renders the per-group popups. `None` = none.
    pub structure_edit: Option<&'a StructureEdit>,
    /// Constructions already in use on this storey — offered in the attribute
    /// editor so a selection can JOIN an existing group instead of re-entering
    /// attributes. Empty when no edit session is open.
    pub structure_existing_groups: Vec<ExistingGroup>,
    /// Candidate regions of a pending Structure carve/cut (world coords) — drawn
    /// to tap between. Empty when no carve is awaiting a pick.
    pub structure_cut_regions: &'a [Vec<(f64, f64)>],
    /// The in-flight Structure split ring (world coords) — drawn highlighted
    /// while the assessor dimensions it. Empty when no split is in-flight.
    pub pending_structure_split_polygon: &'a [(f64, f64)],
    /// STAGED carve rings (world coords) selected but not yet split — shaded as
    /// the pending selection until "Edit attributes" commits them. Empty when
    /// no carve is staged.
    pub pending_structure_carves: &'a [Vec<(f64, f64)>],
    /// The staged carve rings TAGGED by storey (each in its storey's own
    /// world frame) — the all-storeys canvas shades each into its cell. Empty
    /// off the all-storeys tab.
    pub pending_structure_carves_by_storey: &'a [(usize, Vec<(f64, f64)>)],
    /// The wall-inclusion ask for a just-dimensioned structure carve: the
    /// current ambiguous edge (highlighted on the canvas) plus its "wall N of
    /// M" cursor; the panel shows Include / Exclude. `None` when nothing is
    /// being asked.
    pub wall_inclusion_prompt: Option<WallInclusionPromptView>,
    /// The app-wide reusable construction-layer library — the build-up layer
    /// editor's "previously typed layers" pick list.
    pub layer_library: &'a [ber_model::ber::structural::ConstructionLayer],
    // ── Floor exposure ───────────────────────────────────────────────────────
    /// Floor-exposure tool active: the canvas shows the overlaid all-storeys
    /// floor view + selection instead of the normal plan, gated until the
    /// storey-count confirm.
    pub floor_exposure_mode: bool,
    /// The storey-count gate is up — show the gate prompt, hide the overlaid
    /// view + selection until confirmed.
    pub floor_exposure_gate_pending: bool,
    /// The active floor-exposure Select subtool (Click/Carve/All/Remaining).
    pub floor_exposure_select: Option<SelectMode>,
    /// Per-storey floor regions for the overlaid view (with selection flags).
    pub floor_exposure_storeys: &'a [crate::controller::app_controller::FloorExposureStoreyView],
    /// The in-flight floor-exposure CARVE ring (world coords) while it's being
    /// dimensioned — highlighted over the overlaid view so the assessor sees the
    /// region tighten as they enter measurements. Empty when no carve is in flight.
    pub floor_exposure_carve_polygon: &'a [(f64, f64)],
    /// Candidate halves of a pending floor-exposure straight CUT (world coords) —
    /// shaded over the overlaid view so the assessor taps the side they want.
    /// Empty when no cut is awaiting a pick.
    pub floor_exposure_cut_regions: &'a [Vec<(f64, f64)>],
    // ── Roof (Phase-1 roof step) ───────────────────────────────────────────────
    /// Roof step active: the canvas shows the whole-house roof-drawing takeover
    /// (top-storey-shaded outline) instead of the normal plan, gated until the
    /// storey-count confirm.
    pub roof_mode: bool,
    /// The roof storey-count gate is up.
    pub roof_gate_pending: bool,
    /// Each storey's footprint ring (world metres) for the top-storey-shaded
    /// context overlay, drawn bottom→top. Empty while the gate is up.
    pub roof_context: &'a [crate::controller::app_controller::RoofContextRegion],
    /// Selectable ceiling regions (non-party) for grouping into a roof area, with
    /// the in-progress selection flag. Empty while the gate is up.
    pub roof_regions: &'a [crate::controller::app_controller::RoofRegionView],
    /// At least one area is grouped and no per-area drawing is in flight — the
    /// shape buttons (Flat/Gable/Hip) are offered.
    pub roof_area_can_pick_shape: bool,
    /// The grouped area's storey has a sloped ceiling (converted attic) — the
    /// "Follow ceiling" shape is offered so the roof adopts that slope.
    pub roof_area_ceiling_sloped: bool,
    /// The grouped area awaits its ridge stroke(s).
    pub roof_area_ridge_pending: bool,
    /// `Some(ridges drawn so far)` while a HIP collects its ridges — shows the
    /// count and the "Finish ridges" button. `None` for a gable's single stroke.
    pub roof_area_hip_ridge_count: Option<usize>,
    /// The placed ridge(s) await their pitch (degrees).
    pub roof_area_pitch_pending: bool,
    /// A gable ridge is placed — offer "Centre on roof" alongside the pitch.
    pub roof_area_can_centre_ridge: bool,
    /// Every placed ridge's endpoints (world coords) for the dashed overlay
    /// (a hip's collected set while drawing, the full set during the pitch).
    pub roof_area_ridge_lines: Vec<((f64, f64), (f64, f64))>,
    /// Backing string for the roof pitch input (degrees).
    pub roof_pitch_input: &'a mut String,
    // ── Ceiling exposure ─────────────────────────────────────────────────────
    /// Ceiling-exposure tool active: the canvas shows the overlaid all-storeys
    /// ceiling view + selection instead of the normal plan, gated until the
    /// storey-count confirm.
    pub ceiling_exposure_mode: bool,
    /// The ceiling-exposure storey-count gate is up.
    pub ceiling_exposure_gate_pending: bool,
    /// The active ceiling-exposure Select subtool (Click/Carve/All/Remaining).
    pub ceiling_exposure_select: Option<SelectMode>,
    /// Per-storey ceiling regions for the overlaid view (with selection flags).
    pub ceiling_exposure_storeys:
        &'a [crate::controller::app_controller::CeilingExposureStoreyView],
    /// The in-flight ceiling-exposure CARVE ring (world coords) while it's being
    /// dimensioned. Empty when no carve is in flight.
    pub ceiling_exposure_carve_polygon: &'a [(f64, f64)],
    /// Candidate halves of a pending ceiling-exposure straight CUT (world
    /// coords) — shaded over the overlaid view so the assessor taps a side.
    /// Empty when no cut is awaiting a pick.
    pub ceiling_exposure_cut_regions: &'a [Vec<(f64, f64)>],
}

impl SketchView<'_> {
    /// An all-storeys overlay mode (roof / floor-exposure / ceiling-exposure) is
    /// live (post-gate): the overlaid all-storeys view REPLACES the single-storey
    /// plan, so the single-storey plan chrome must be suppressed. Derived from the
    /// same fields the canvas uses to pick which overlay to draw.
    pub fn overlay_active(&self) -> bool {
        (self.floor_exposure_mode && !self.floor_exposure_gate_pending)
            || (self.ceiling_exposure_mode && !self.ceiling_exposure_gate_pending)
            || (self.roof_mode && !self.roof_gate_pending)
    }

    /// A floor/ceiling-exposure carve ring is in flight (being dimensioned). Its
    /// dimensioning still runs the dim/position prompts + the floating number box,
    /// so those stay live even under an overlay mode.
    pub fn carve_dimensioning(&self) -> bool {
        !self.floor_exposure_carve_polygon.is_empty()
            || !self.ceiling_exposure_carve_polygon.is_empty()
    }
}

#[derive(Default)]
pub struct SketchPage;

impl SketchPage {
    pub fn render<F>(
        ui: &mut egui::Ui,
        mut view: SketchView<'_>,
        touch: &mut touch::TouchNav,
        mut on_event: F,
    ) where
        F: FnMut(UiEvent),
    {
        // Orientation = aspect ratio of the available rect, re-checked every
        // frame (free with egui): portrait → ribbon on top, landscape → side
        // panel on the left. Both render the SAME category data. The
        // layout-preview override (the top-strip "Layout" button — a dev
        // affordance for trying both arrangements on one device) wins when
        // set; Auto falls through to the aspect-ratio check.
        let avail = ui.available_size();
        let portrait = match view.cat_state.layout_override {
            Some(categories::LayoutOrientation::Portrait) => true,
            Some(categories::LayoutOrientation::Landscape) => false,
            None => avail.y > avail.x,
        };
        // Owned snapshot of the flags the tool buttons need, so the panel
        // closures below don't hold a borrow of `view`.
        let flags = categories::ToolFlags::from_view(&view);

        // --- Top strip: storey tabs (the 2D/3D + display-mode choices are
        // floating canvas buttons — see `canvas.rs`), then — in portrait —
        // the category ribbon.
        egui::TopBottomPanel::top("sketch_top_strip").show_inside(ui, |ui| {
            storey_tabs(
                ui,
                view.ber,
                view.current_storey_idx,
                &mut view.cat_state.layout_override,
                view.delete_storey_armed,
                &mut on_event,
            );
            if portrait {
                ui.separator();
                categories::ribbon(ui, &flags, view.cat_state, &mut on_event);
            }
        });

        // --- Bottom bar: the contextual INSTRUCTION line only ("what to do" in
        // the current mode/flow). The controls themselves float over the canvas
        // (the flow panel + the top-right action buttons, below).
        if let Some(text) = prompt_dock::instruction(&view) {
            egui::TopBottomPanel::bottom("sketch_prompt_dock").show_inside(ui, |ui| {
                ui.add_space(2.0);
                ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(195, 195, 195)));
                ui.add_space(2.0);
            });
        }

        // --- Landscape: all categories at once in a left side panel.
        if !portrait {
            egui::SidePanel::left("sketch_category_panel")
                .resizable(false)
                .default_width(170.0)
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| {
                        categories::side_panel(ui, &flags, view.cat_state, &mut on_event);
                    });
                });
        }

        // --- The canvas fills whatever's left.
        egui::CentralPanel::default().show_inside(ui, |ui| {
            let canvas_rect = ui.max_rect();
            canvas::render(ui, &view, touch, &mut on_event);
            // External-photo ANNOTATION takeover (paintbrush over the photo):
            // a centred floating window over the canvas while a session is
            // open. Saves via `AnnotateExternalPhoto`; Save/Cancel both close
            // the session by clearing the slot.
            crate::ui::widgets::photo_annotate::render(
                ui.ctx(),
                canvas_rect,
                &mut *view.photo_annotator,
                &mut on_event,
            );
            // The floating number-entry box: every single-value measurement
            // prompt is typed here, in a draggable window over the canvas
            // (constrained to it), rather than in the bottom prompt dock.
            // Rendered OUTSIDE `canvas::render`, so it isn't covered by the
            // canvas's overlay-mode suppression — gate it here to the same rule:
            // suppress under an all-storeys overlay mode UNLESS a carve is being
            // dimensioned (a carve types its length in this same box).
            if !view.overlay_active() || view.carve_dimensioning() {
                number_box::render(ui.ctx(), canvas_rect, &mut view, &mut on_event);
            }
            // Phase-3 (Objects): the open room's entry panel (category → subtype
            // → count + photos → Add), a floating window over the canvas.
            objects_panel::render(ui.ctx(), canvas_rect, &mut view, &mut on_event);
            // Phase-4 (Final) wizard: form / north / sheltered + completion.
            final_panel::render(ui.ctx(), canvas_rect, &mut view, &mut on_event);
            // The contextual flow controls (exposure / phase / step / roof /
            // new-storey / redraw / curve / vertex / review) live in a DRAGGABLE
            // floating window over the canvas, opening roughly centred — buttons
            // and inputs only; the bottom bar carries the instruction. Pressing a
            // button CLOSES the panel (it reopens when the panel's activation
            // token changes — a new selection / stage / target). The curve slider
            // is a continuous drag, not a "press", so it doesn't dismiss.
            let dismiss_id = egui::Id::new("sketch_flow_panel_dismissed");
            if prompt_dock::has_controls(&view) {
                let token = prompt_dock::panel_token(&view);
                let stored = ui.ctx().data(|d| d.get_temp::<String>(dismiss_id));
                let dismissed = stored.as_deref() == Some(token.as_str());
                if stored.is_some() && !dismissed {
                    // The remembered dismissal belongs to a PREVIOUS activation
                    // (another stage/selection) — forget it, or a later return
                    // to the dismissed stage's token would stay closed.
                    ui.ctx().data_mut(|d| d.remove::<String>(dismiss_id));
                }
                if !dismissed {
                    let mut pressed = false;
                    egui::Window::new(prompt_dock::panel_title(&view))
                        .id(egui::Id::new("sketch_flow_panel"))
                        .order(egui::Order::Foreground)
                        .default_pos(canvas_rect.center())
                        .pivot(egui::Align2::CENTER_CENTER)
                        .constrain_to(canvas_rect)
                        .resizable(false)
                        .collapsible(true)
                        .show(ui.ctx(), |ui| {
                            ui.set_max_width((canvas_rect.width() - 24.0).min(520.0));
                            let mut catch = |ev: UiEvent| {
                                // A button press dismisses the panel; the curve
                                // slider's continuous updates do not.
                                if !matches!(ev, UiEvent::Sketch(SketchEvent::SetCurveBulge(_))) {
                                    pressed = true;
                                }
                                on_event(ev);
                            };
                            prompt_dock::render_controls(ui, &mut view, &mut catch);
                        });
                    // Persistent select-then-set tools (exposure / partition) stay
                    // open across clicks — you select then set, possibly several
                    // times — and close only via their own "Done".
                    if pressed && !prompt_dock::panel_persists(&view) {
                        ui.ctx()
                            .data_mut(|d| d.insert_temp(dismiss_id, token.clone()));
                    }
                }
            } else {
                // No panel this frame — any remembered dismissal belongs to a
                // FINISHED activation, so forget it. Tokens are only unique per
                // stage, not per flow instance: "Draw outline" pressed on one
                // new-storey flow stores "ns-method", and without this reset a
                // LATER flow's ChooseMethod (same token, reached via the
                // panel-less thickness stage) would silently stay dismissed —
                // the add → draw → delete → add-again case.
                ui.ctx().data_mut(|d| d.remove::<String>(dismiss_id));
            }
            // Top-right floating action buttons for the active selection.
            prompt_dock::render_corner_actions(ui.ctx(), canvas_rect, &view, &mut on_event);
            // The single "Delete" panel — fixed at the window's bottom-left (the
            // Continue-bar level); remove committed steps / phases / roof points
            // here, grouped in sections, instead of from each tool's control box.
            prompt_dock::render_delete_panel(ui.ctx(), &view, &mut on_event);
        });

        // Keyboard shortcuts: Ctrl+Z undo, Ctrl+Shift+Z or Ctrl+Y redo.
        // `consume_key` ensures the key event is claimed so it doesn't
        // also fire as input into focused text boxes.
        let (undo_pressed, redo_pressed) = ui.input_mut(|i| {
            let mods = i.modifiers;
            let ctrl = mods.ctrl || mods.mac_cmd;
            let z = ctrl && i.consume_key(mods, egui::Key::Z);
            let y = ctrl && i.consume_key(mods, egui::Key::Y);
            let undo = z && !mods.shift;
            let redo = (z && mods.shift) || y;
            (undo, redo)
        });
        if undo_pressed {
            on_event(UiEvent::Sketch(SketchEvent::Undo));
        }
        if redo_pressed {
            on_event(UiEvent::Sketch(SketchEvent::Redo));
        }
    }
}

/// Horizontal row of tabs for storey navigation:
/// `[All]` `[Storey 1]` `[Storey 2]` … `[+ Add new]`, plus the
/// layout-preview cycler (`Layout: Auto ▸ Portrait ▸ Landscape`).
/// `[All]` (read-only overview) sets `current_storey_idx = None`; each
/// storey tab sets `Some(i)`; `[+ Add new]` triggers `AddStorey`. When the
/// TOP storey (index ≥ 1) is selected a small delete button follows — first
/// tap arms an inline confirm (`delete_armed`), the confirm's Delete emits
/// `DeleteStorey` (the "drew it badly, start again" path).
fn storey_tabs<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    layout_override: &mut Option<categories::LayoutOrientation>,
    delete_armed: &mut Option<usize>,
    on_event: &mut F,
) {
    let storeys = ber.map(|b| b.storeys.as_slice()).unwrap_or(&[]);
    ui.horizontal(|ui| {
        // All-storeys tab. Selected when `current_storey_idx == None`.
        if ui
            .selectable_label(current_storey_idx.is_none(), "All")
            .hint("Read-only overview of every storey")
            .clicked()
            && current_storey_idx.is_some()
        {
            on_event(UiEvent::Sketch(SketchEvent::SetCurrentStorey(None)));
        }
        for (i, storey) in storeys.iter().enumerate() {
            let label = if storey.name.is_empty() {
                format!("Storey {}", storey.storey_number + 1)
            } else {
                storey.name.clone()
            };
            let selected = current_storey_idx == Some(i);
            if ui.selectable_label(selected, label).clicked() && !selected {
                on_event(UiEvent::Sketch(SketchEvent::SetCurrentStorey(Some(i))));
            }
        }
        // "+ Add new" — always last. Disabled (the button still draws but
        // does nothing) when no BER is loaded so no storey can be created.
        let enabled = ber.is_some();
        if ui
            .add_enabled(enabled, egui::Button::new("+ Add new"))
            .hint(
                "Add a new storey. Draw it with the previous storey shown faintly \
                 as a guide; endpoints snap onto it.",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::AddStorey));
        }
        // Delete-storey affordance: only when the SELECTED tab is the last
        // storey and it isn't the ground storey (the controller enforces the
        // same rule). First tap arms an inline confirm; any change of the
        // deletable storey (switch, add, delete) disarms it. Not shown for a
        // brand-new empty storey's dock flows either way — this tab-row button
        // is the single affordance.
        let deletable = current_storey_idx
            .filter(|&k| k >= 1 && k + 1 == storeys.len())
            .is_some();
        if !deletable {
            *delete_armed = None;
        } else {
            let k = current_storey_idx.unwrap();
            if *delete_armed != Some(k) {
                *delete_armed = None;
                if ui
                    .button("🗑")
                    .hint("Delete this storey and start again")
                    .clicked()
                {
                    *delete_armed = Some(k);
                }
            } else {
                ui.label(format!(
                    "Delete Storey {}? Its outline and details are removed.",
                    k + 1
                ));
                if ui.button("Delete").clicked() {
                    *delete_armed = None;
                    on_event(UiEvent::Sketch(SketchEvent::DeleteStorey));
                }
                if ui.button("Cancel").clicked() {
                    *delete_armed = None;
                }
            }
        }
        // Layout-preview cycler (a preview/dev affordance the user requested):
        // force the category layout to the portrait ribbon or landscape side
        // panel; Auto picks by aspect ratio. Pure page state — no controller
        // round-trip. Plain text for now — visual design deferred.
        use categories::LayoutOrientation::{Landscape, Portrait};
        let label = match layout_override {
            None => "Layout: Auto",
            Some(Portrait) => "Layout: Portrait",
            Some(Landscape) => "Layout: Landscape",
        };
        if ui
            .button(label)
            .hint(
                "Preview the toolbar layout: Auto (by aspect ratio) \
                 ▸ Portrait (top ribbon) ▸ Landscape (side panel)",
            )
            .clicked()
        {
            *layout_override = match layout_override {
                None => Some(Portrait),
                Some(Portrait) => Some(Landscape),
                Some(Landscape) => None,
            };
        }
    });
}
