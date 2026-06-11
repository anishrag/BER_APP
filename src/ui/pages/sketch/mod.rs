//! The Sketch page: a responsive category layout around the drawing canvas.
//!
//! Module map:
//! - `mod.rs` — [`SketchEvent`], the [`SketchView`] view-model the shell
//!   builds each frame, the page entry point (orientation check + panel
//!   arrangement), the storey tabs, and the 2D/3D + display-mode selectors
//!   (these stay up top for now; slice 3 makes them floating canvas buttons).
//! - `categories.rs` — the tool categories defined ONCE as data, plus the two
//!   renderers over them: portrait → top ribbon, landscape → left side panel.
//! - `prompt_dock.rs` — every contextual flow panel (dimension/position
//!   prompts, cut picks, amount entry, review, …) in one bottom strip.
//! - `canvas.rs` — the canvas itself: overlays, stroke capture, taps, drags.

mod canvas;
pub mod categories;
mod prompt_dock;

use egui::epaint::PathStroke;
use egui::Pos2;
use strum::IntoEnumIterator;

use crate::controller::app_controller::{
    DimensionPromptUiData, EditDragPromptUiData, ExposureSelection, PositionPromptUiData,
    SnapTagView, VertexDropPromptUiData, WallExposureView,
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
    StrokeClosed {
        stroke_idx: usize,
    },
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
    /// Reset the view to pure auto-fit.
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
    /// User typed a default ceiling height (m) in the first-3D prompt: stamp
    /// the current scope (this storey, or every unset storey from the All tab)
    /// and complete the switch into the 3D view that was requested.
    SetDefaultCeilingHeight(f32),
    /// User dismissed the ceiling-height prompt without setting a height —
    /// stays in 2D.
    CancelCeilingHeightPrompt,
    /// Switch the editor onto a specific storey, or `None` for the read-only
    /// "All storeys" overview tab.
    SetCurrentStorey(Option<usize>),
    /// Append a fresh empty storey and switch the editor onto it (method 2:
    /// the previous storey is shown as a watermark while the new one is drawn).
    AddStorey,
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
    /// First-3D ceiling-height gate: whether the typed prompt is open, the
    /// backing string for its input, its last error, and whether the current
    /// scope already has its heights (drives the 3D option's hover hint).
    pub ceiling_height_prompt: bool,
    pub ceiling_height_input: &'a mut String,
    pub ceiling_height_error: Option<&'a str>,
    pub ceiling_heights_set: bool,
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
        // panel on the left. Both render the SAME category data.
        let avail = ui.available_size();
        let portrait = avail.y > avail.x;
        // Owned snapshot of the flags the tool buttons need, so the panel
        // closures below don't hold a borrow of `view`.
        let flags = categories::ToolFlags::from_view(&view);

        // --- Top strip: storey tabs (storey navigation is above the
        // view-mode choice — picking a storey is the higher-level axis),
        // then the 2D/3D + display-mode selectors (unchanged for now;
        // slice 3 turns them into floating canvas buttons), then — in
        // portrait — the category ribbon.
        egui::TopBottomPanel::top("sketch_top_strip").show_inside(ui, |ui| {
            storey_tabs(ui, view.ber, view.current_storey_idx, &mut on_event);
            if let Some(new_mode) = mode_toolbar(ui, view.mode, view.ceiling_heights_set) {
                on_event(UiEvent::Sketch(SketchEvent::ChangeMode(new_mode)));
            }
            if let Some(new_display) = display_toolbar(ui, view.display) {
                on_event(UiEvent::Sketch(SketchEvent::ChangeDisplay(new_display)));
            }
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
/// `[All]` `[Storey 1]` `[Storey 2]` … `[+ Add new]`.
/// `[All]` (read-only overview) sets `current_storey_idx = None`; each
/// storey tab sets `Some(i)`; `[+ Add new]` triggers `AddStorey`.
fn storey_tabs<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
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
    });
}

fn mode_toolbar(
    ui: &mut egui::Ui,
    mode: SketchMode,
    ceiling_heights_set: bool,
) -> Option<SketchMode> {
    let mut change_mode: Option<SketchMode> = None;
    ui.horizontal(|ui| {
        for mode_item in SketchMode::iter() {
            let selected = mode == mode_item;
            let mut response = ui.selectable_label(selected, format!("{:?}", mode_item));
            // 3D stays CLICKABLE without ceiling heights — clicking opens
            // the height prompt (the controller gate) — but the hint warns
            // the assessor what the click will ask for first.
            if matches!(mode_item, SketchMode::Storey3D) && !ceiling_heights_set {
                response =
                    response.on_hover_text("Sets the default ceiling height first — 3D needs it");
            }
            if response.clicked() {
                change_mode = Some(mode_item);
            }
        }
    });
    change_mode
}

fn display_toolbar(ui: &mut egui::Ui, display: DisplayMode) -> Option<DisplayMode> {
    let mut change_display: Option<DisplayMode> = None;
    ui.horizontal(|ui| {
        for display_item in DisplayMode::iter() {
            let selected = display == display_item;
            if ui
                .selectable_label(selected, format!("{:?}", display_item))
                .clicked()
            {
                change_display = Some(display_item);
            }
        }
    });
    change_display
}
