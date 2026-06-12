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
//!   plus the floating 2D/3D + display-mode buttons in its corner.

mod canvas;
pub mod categories;
mod prompt_dock;

use egui::epaint::PathStroke;
use egui::Pos2;

use crate::controller::app_controller::{
    DimensionPromptUiData, EditDragPromptUiData, ExposureSelection, NewStoreyPromptUiData,
    PositionPromptUiData, RiserView, SnapTagView, VertexDropPromptUiData, WallExposureView,
};
use crate::ui::pages::events::UiEvent;
use crate::ui::render::ViewParams;
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::ConstructionPhase;
use datatypes::ber::structural::WallExposure;
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_2d_plan::CollinearitySuggestion;

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
    ChangeMode(SketchMode),
    ChangeDisplay(DisplayMode),
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
    /// Replay the last snapping action with every elective snap disabled
    /// (revert to the freehand points). Test hook for the snap-choice replay.
    UnsnapLast,
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
    /// "Don't snap" on the pending ceiling-step line's snap tag: revert the
    /// line to its drawn coordinate. Routed to the controller's dedicated
    /// `unsnap_ceiling_step_line` — NOT `ChooseSnap`/`UnsnapLast`, whose
    /// rewind-and-replay machinery must not touch a line outside history.
    UnsnapCeilingStepLine,
    /// Discard the in-flight ceiling-step line at any stage.
    CancelPendingCeilingStep,
    /// Delete the current storey's ceiling-step region at the given index.
    DeleteCeilingStepRegion(usize),
    /// Toggle roof-point mode: when on, tapping a point inside a ceiling area
    /// starts the apex flow (per-axis position questions, then the height).
    SetRoofPointMode(bool),
    /// Tap (world coords) in roof-point mode placing the apex's position.
    RoofPointTap {
        world_x: f64,
        world_y: f64,
    },
    /// User entered the apex height (metres, ABSOLUTE above the storey floor
    /// datum) for the pending roof point.
    AssignRoofApexHeight(f32),
    /// "Don't snap" on the pending roof point's snap tag: revert BOTH axes to
    /// their tapped coordinates and restart the position questions. Routed to
    /// the controller's dedicated `unsnap_roof_point` — NOT
    /// `ChooseSnap`/`UnsnapLast`, whose rewind-and-replay machinery must not
    /// touch a point outside history.
    UnsnapRoofPoint,
    /// Discard the in-flight roof point at any stage.
    CancelPendingRoofPoint,
    /// Delete the current storey's roof apex at the given index.
    DeleteRoofApex(usize),
    /// A roof-RIDGE stroke (world coords) drawn in roof-point mode — fitted
    /// to an axis-aligned segment placing TWO apexes at once (stroke = ridge,
    /// tap = point: the ceiling-step stroke/tap routing pattern).
    RoofRidgeStrokeWorld {
        points: Vec<(f32, f32)>,
    },
    /// User entered the ridge LENGTH (metres along the ridge axis — the
    /// extent becomes `[start, start + length]`) for the pending roof ridge.
    AssignRoofRidgeLength(f32),
    /// Skip the pending roof ridge's length question — accept the drawn
    /// length as-is.
    SkipRoofRidgeLength,
    /// User entered the ridge height (metres, ABSOLUTE above the storey floor
    /// datum — both endpoints get it) for the pending roof ridge.
    AssignRoofRidgeHeight(f32),
    /// "Don't snap" on the pending roof ridge's snap tag: revert the cross
    /// coordinate to its drawn value and reopen the position question. Routed
    /// to the controller's dedicated `unsnap_roof_ridge` — NOT
    /// `ChooseSnap`/`UnsnapLast`, whose rewind-and-replay machinery must not
    /// touch a ridge outside history.
    UnsnapRoofRidge,
    /// Discard the in-flight roof ridge at any stage.
    CancelPendingRoofRidge,
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
    /// Drag the grabbed selection handle along the wall.
    ExposurePointerDrag {
        world_x: f64,
        world_y: f64,
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
    /// User typed a default ceiling height (m) in the Heights category's
    /// field: stamp the current scope (overwriting this storey's value, or
    /// filling every unset storey from the All tab). Stays in the current
    /// view — 3D is the floating canvas button's separate action.
    SetDefaultCeilingHeight(f32),
    /// Switch the editor onto a specific storey, or `None` for the read-only
    /// "All storeys" overview tab.
    SetCurrentStorey(Option<usize>),
    /// Append a fresh empty storey and switch the editor onto it (method 2:
    /// the previous storey is shown as a watermark while the new one is drawn).
    AddStorey,
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
    /// Construction-phase (year-of-construction) tagging: mode flag, the
    /// committed regions `(ring, label)` to tint, the carved area awaiting a
    /// phase pick, the cut candidates to tap between, the phase choices for
    /// the picker, and whether a pick is currently pending.
    pub phase_mode: bool,
    pub phase_region_polygons: &'a [(Vec<(f64, f64)>, String)],
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
    pub roof_point_mode: bool,
    pub roof_point_available: bool,
    pub roof_apex_views: &'a [((f64, f64), String)],
    pub pending_roof_point: Option<(f64, f64)>,
    /// The pending point's wall snap as one standard snap tag (its menu
    /// offers only "Don't snap", routed to UnsnapRoofPoint).
    pub roof_point_snap_tag: Option<&'a SnapTagView>,
    pub roof_point_position_pending: bool,
    pub roof_point_height_pending: bool,
    pub roof_point_pending: bool,
    pub roof_apex_input: &'a mut String,
    /// Roof-RIDGE line input (a stroke in roof-point mode): the committed
    /// ridges `(end_a, end_b, label)` to overlay ("⌃ 4.20 m" /
    /// "4.20→4.50 m"), the pending ridge SEGMENT for the dashed overlay, its
    /// snap tag (menu offers only "Don't snap", routed to UnsnapRoofRidge),
    /// and the per-stage flags (position questions / length entry / height
    /// entry). The height entry reuses `roof_apex_input`; the length entry
    /// has its own buffer (prefilled with the drawn length on stage open).
    pub roof_ridge_views: &'a [((f64, f64), (f64, f64), String)],
    pub pending_roof_ridge_line: Option<((f64, f64), (f64, f64))>,
    pub roof_ridge_snap_tag: Option<&'a SnapTagView>,
    pub roof_ridge_position_pending: bool,
    pub roof_ridge_length_pending: bool,
    /// The open length question's extent starts at a SNAPPED wall coordinate
    /// — the panel's "from the highlighted end" hover shows only then.
    pub roof_ridge_length_anchored: bool,
    pub roof_ridge_height_pending: bool,
    pub roof_ridge_pending: bool,
    pub roof_ridge_length_input: &'a mut String,
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
    /// Backing string for the flow's thickness input (prefilled "0.30" when
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
    /// wall-length label visibility). Lives on `SketchState`.
    pub cat_state: &'a mut categories::CategoryUiState,
    /// Autosave status line for the State category ("saved Ns ago"), derived
    /// by the shell from its revision bookkeeping. `None` hides the label.
    pub autosave_status: Option<String>,
}

#[derive(Default)]
pub struct SketchPage;

impl SketchPage {
    pub fn render<F>(ui: &mut egui::Ui, mut view: SketchView<'_>, mut on_event: F)
    where
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
                &mut on_event,
            );
            if portrait {
                ui.separator();
                categories::ribbon(ui, &flags, view.cat_state, &mut on_event);
            }
        });

        // --- Prompt dock: every contextual flow panel renders here, in both
        // orientations. Collapsed entirely when nothing needs showing.
        if prompt_dock::has_content(&view) {
            egui::TopBottomPanel::bottom("sketch_prompt_dock").show_inside(ui, |ui| {
                prompt_dock::render(ui, &mut view, &mut on_event);
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
            canvas::render(ui, &view, &mut on_event);
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
/// storey tab sets `Some(i)`; `[+ Add new]` triggers `AddStorey`.
fn storey_tabs<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    layout_override: &mut Option<categories::LayoutOrientation>,
    on_event: &mut F,
) {
    let storeys = ber.map(|b| b.storeys.as_slice()).unwrap_or(&[]);
    ui.horizontal(|ui| {
        // All-storeys tab. Selected when `current_storey_idx == None`.
        if ui
            .selectable_label(current_storey_idx.is_none(), "All")
            .on_hover_text("Read-only overview of every storey")
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
            .on_hover_text(
                "Add a new storey. Draw it with the previous storey shown faintly \
                 as a guide; endpoints snap onto it.",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::AddStorey));
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
            .on_hover_text(
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
