use egui::epaint::{PathShape, PathStroke};
use egui::Pos2;
use strum::IntoEnumIterator;

use crate::controller::app_controller::{
    DimensionPromptUiData, EditDragPromptUiData, ExposureSelection, PositionPromptUiData,
    SnapTagView, VertexDropPromptUiData, WallExposureView,
};
use crate::ui::pages::events::UiEvent;
use crate::ui::render;
use crate::ui::render::ViewParams;
use datatypes::ber::common::ConstructionPhase;
use datatypes::ber::structural::WallExposure;

/// Parse a dimension expression. Supports:
/// - A single non-negative number: `"4.2"`, `"0.5"`, `"0"`
/// - Sums and differences of numbers: `"3.2 + 1.5"`, `"5 - 1.8 + 0.3"`
/// - Optional leading `+`; a leading `-` is rejected since dimensions can't
///   be negative.
///
/// Zero is accepted — it collapses the wall (used to zero a trim remnant so a
/// cut snaps onto a corner). Whitespace is ignored. Returns `None` for
/// malformed input or a negative result.
fn parse_dimension_expression(input: &str) -> Option<f32> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut total: f32 = 0.0;
    let mut sign: f32 = 1.0;
    let mut current = String::new();
    let mut started = false;
    for ch in trimmed.chars() {
        if ch.is_whitespace() {
            continue;
        }
        if ch == '+' || ch == '-' {
            if !started {
                if ch == '-' {
                    return None;
                }
                started = true;
                continue;
            }
            if current.is_empty() {
                return None;
            }
            let v: f32 = current.parse().ok()?;
            total += sign * v;
            sign = if ch == '+' { 1.0 } else { -1.0 };
            current.clear();
        } else {
            current.push(ch);
            started = true;
        }
    }
    if current.is_empty() {
        return None;
    }
    let v: f32 = current.parse().ok()?;
    total += sign * v;
    // Zero is allowed: entering 0 collapses the wall (e.g. zeroing a trim
    // remnant to snap a cut onto a corner). Negatives are still rejected.
    if total.is_finite() && total >= 0.0 {
        Some(total)
    } else {
        None
    }
}

#[cfg(test)]
mod expression_tests {
    use super::parse_dimension_expression;

    fn approx(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn single_number() {
        assert!(approx(parse_dimension_expression("4.2").unwrap(), 4.2));
        assert!(approx(parse_dimension_expression("  3  ").unwrap(), 3.0));
    }

    #[test]
    fn sum_and_difference() {
        assert!(approx(parse_dimension_expression("3 + 1.5").unwrap(), 4.5));
        assert!(approx(parse_dimension_expression("5 - 1.8").unwrap(), 3.2));
        assert!(approx(
            parse_dimension_expression("2 + 3.5 - 0.3").unwrap(),
            5.2
        ));
    }

    #[test]
    fn leading_plus_allowed_leading_minus_rejected() {
        assert!(approx(parse_dimension_expression("+4.2").unwrap(), 4.2));
        assert!(parse_dimension_expression("-4.2").is_none());
    }

    #[test]
    fn malformed_inputs_rejected() {
        assert!(parse_dimension_expression("").is_none());
        assert!(parse_dimension_expression("3 +").is_none());
        assert!(parse_dimension_expression("3 + + 2").is_none());
        assert!(parse_dimension_expression("hello").is_none());
        assert!(parse_dimension_expression("3 - 5").is_none()); // negative result
    }

    #[test]
    fn zero_accepted_for_collapsing_a_wall() {
        // 0 is a valid dimension: it collapses the wall (e.g. zero a trim
        // remnant). Only negatives are rejected.
        assert!(approx(parse_dimension_expression("0").unwrap(), 0.0));
        assert!(approx(parse_dimension_expression("3 - 3").unwrap(), 0.0));
    }
}

use datatypes::ber::ber_struct::Ber;
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
    /// Primary mouse button pressed in Edit mode at the given world-coord
    /// position. Hit-tests a wall and selects it if found.
    EditMouseDown {
        world_x: f64,
        world_y: f64,
        tol: f64,
    },
    /// Primary mouse drag in Edit mode: translate the selected wall so its
    /// perpendicular coord matches `(world_x, world_y)`.
    EditMouseDrag {
        world_x: f64,
        world_y: f64,
    },
    /// Primary mouse release in Edit mode: commit + clear drag flag.
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
    /// Double-click in Edit mode: insert a new vertex on the wall closest
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
    /// `BER_CHECKPOINT=1` launch).
    SaveCheckpoint,
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

#[derive(Default)]
pub struct SketchPage;
impl SketchPage {
    #[allow(clippy::too_many_arguments)]
    pub fn render<F>(
        ui: &mut egui::Ui,
        current: Option<EguiStroke>,
        mode: SketchMode,
        display: DisplayMode,
        edit_tool: EditTools,
        ber: Option<&Ber>,
        current_storey_idx: Option<usize>,
        review: Option<&CollinearitySuggestion>,
        review_remaining: usize,
        has_floor_plan: bool,
        pending_redraw: bool,
        dim_prompt: Option<DimensionPromptUiData>,
        dim_input: &mut String,
        dim_scale_set: bool,
        dim_error: Option<&str>,
        dimensioned_walls: &[(usize, usize)],
        // Position prompt for a carved phase/step area: how far the area (or
        // its cut wall) sits from a storey-outline wall. Shown once the area's
        // wall-length loop is done; both walls are highlighted on the canvas.
        position_prompt: Option<PositionPromptUiData>,
        position_input: &mut String,
        position_error: Option<&str>,
        // World-coord points of a provisional ExtendCurve/TrimCurve arc awaiting
        // its attach dimensions, drawn as an amber preview overlay.
        arc_preview: Option<&[(f64, f64)]>,
        // Current bulge of a provisional curve that's ready to have its radius
        // tuned (attach lines set). `Some` shows the radius slider toolbar.
        curve_bulge: Option<f32>,
        // Living-area drawing mode + the resolved living-area outlines (world
        // coords) to overlay in a distinct colour; `active_living_area` is the
        // one currently edited (highlighted).
        living_area_mode: bool,
        living_area_polygons: &[Vec<(f64, f64)>],
        // Per area: known-length walls as (start, end, length) world coords.
        living_area_wall_labels: &[Vec<((f64, f64), (f64, f64), f32)>],
        active_living_area: Option<usize>,
        // The two candidate regions of a pending cut (world coords); when
        // non-empty the user taps one to pick it as the living area.
        pending_cut_regions: &[Vec<(f64, f64)>],
        // Wall-exposure mode + the outline perimeter walls (world coords) with
        // their exposure sections, the current section selection on the selected
        // wall, and the typed start/end input buffers.
        exposure_mode: bool,
        exposure_walls: &[WallExposureView],
        exposure_selection: Option<ExposureSelection>,
        exposure_start_input: &mut String,
        exposure_end_input: &mut String,
        // Construction-phase (year-of-construction) tagging: mode flag, the
        // committed regions `(ring, label)` to tint, the carved area awaiting a
        // phase pick, the cut candidates to tap between, the phase choices for
        // the picker, and whether a pick is currently pending.
        phase_mode: bool,
        phase_region_polygons: &[(Vec<(f64, f64)>, String)],
        pending_phase_polygon: &[(f64, f64)],
        pending_phase_cut_regions: &[Vec<(f64, f64)>],
        phase_choices: &[(ConstructionPhase, String)],
        phase_pick_pending: bool,
        // Floor-step tagging: mode flag, the committed step regions
        // `(ring, label)` to tint, the carved area awaiting its level, the cut
        // candidates to tap between, whether a level entry is pending, and the
        // backing string for the amount input.
        step_mode: bool,
        step_region_polygons: &[(Vec<(f64, f64)>, String)],
        pending_step_polygon: &[(f64, f64)],
        pending_step_cut_regions: &[Vec<(f64, f64)>],
        step_pick_pending: bool,
        step_amount_input: &mut String,
        // Ceiling-step tagging: mode flag + availability gate (needs a default
        // ceiling height + finalised outline), the committed step regions
        // `(ring, label)` to tint, the pending line + its two side half-rects,
        // the per-stage flags (orientation pick / side tap / amount entry),
        // whether ANY stage is pending, and the amount input's backing string.
        ceiling_step_mode: bool,
        ceiling_step_available: bool,
        ceiling_step_region_polygons: &[(Vec<(f64, f64)>, String)],
        pending_ceiling_step_line: Option<((f64, f64), (f64, f64))>,
        pending_ceiling_step_side_rects: &[Vec<(f64, f64)>],
        ceiling_step_orientation_pending: bool,
        ceiling_step_side_pending: bool,
        ceiling_step_amount_pending: bool,
        ceiling_step_pending: bool,
        ceiling_amount_input: &mut String,
        // First-3D ceiling-height gate: whether the typed prompt is open, the
        // backing string for its input, its last error, and whether the current
        // scope already has its heights (drives the 3D option's hover hint).
        ceiling_height_prompt: bool,
        ceiling_height_input: &mut String,
        ceiling_height_error: Option<&str>,
        ceiling_heights_set: bool,
        edit_selected_wall: Option<((f64, f64), (f64, f64))>,
        edit_selected_vertex: Option<(f64, f64)>,
        edit_drag_prompt: Option<EditDragPromptUiData>,
        edit_drag_input: &mut String,
        edit_drag_error: Option<&str>,
        vertex_drop_prompt: Option<VertexDropPromptUiData>,
        vertex_drop_inputs: &mut [String; 4],
        vertex_drop_error: Option<&str>,
        can_undo: bool,
        can_redo: bool,
        view: ViewParams,
        // DEBUG: splice-pipeline phases from the last extend/trim —
        // `(label, world points, closed)` — drawn as a coloured overlay.
        splice_debug: &[(String, Vec<(f32, f32)>, bool)],
        // Elective snaps of the last action — `(world_x, world_y, kind, has_alt)`
        // — drawn as clickable tags.
        snap_tags: &[SnapTagView],
        mut on_event: F,
    ) where
        F: FnMut(UiEvent),
    {
        let reviewing = review.is_some();
        let dim_prompting = dim_prompt.is_some();

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical(|ui| {
                // --- Top toolbar: Storey tabs (storey navigation is above the
                // view-mode choice — picking a storey is the higher-level axis).
                Self::storey_tabs(ui, ber, current_storey_idx, &mut on_event);

                // --- Mode toolbar: SketchMode (per-storey view choice) ---
                if let Some(new_mode) = Self::mode_toolbar(ui, mode, ceiling_heights_set) {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeMode(new_mode)));
                }

                // --- Middle toolbar: DisplayMode ---
                if let Some(new_display) = Self::display_toolbar(ui, display) {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeDisplay(new_display)));
                }

                ui.separator();

                let bottom_toolbar_height = 30.0;
                let separator_height = ui.spacing().item_spacing.y;
                let canvas_height =
                    ui.available_height() - bottom_toolbar_height - separator_height;

                // `click_and_drag` so middle/secondary drags register too
                // (plain `drag` is biased toward primary-button drag).
                let (rect, response) = ui.allocate_exact_size(
                    egui::Vec2::new(ui.available_width(), canvas_height),
                    egui::Sense::click_and_drag(),
                );

                let painter = ui.painter_at(rect);
                painter.rect_filled(rect, 0.0, egui::Color32::from_gray(30));

                // BER view layer with optional collinearity highlight overlay.
                render::render_ber(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    display,
                    review,
                    dimensioned_walls,
                    view,
                );

                // Living-area sub-shapes overlaid in green (active brighter),
                // with length labels on their known walls (like the storey).
                render::render_living_areas(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    display,
                    view,
                    living_area_polygons,
                    living_area_wall_labels,
                    active_living_area,
                );

                // Pending cut: show both candidate regions to tap between.
                render::render_cut_candidates(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    pending_cut_regions,
                );

                // DEBUG: splice-pipeline phases of the last extend/trim.
                render::render_splice_debug(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    splice_debug,
                );

                // Elective-snap tags of the last action (orange markers + kind).
                render::render_snap_tags(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    snap_tags,
                );

                // Left-click menu per snap tag: pick a different candidate, or
                // "Don't snap", to replay the action with just that snap changed.
                if !snap_tags.is_empty() {
                    if let Some(t) =
                        render::active_storey_transform(rect, ber, current_storey_idx, mode, view)
                    {
                        for (ti, tag) in snap_tags.iter().enumerate() {
                            let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
                            let label = match tag.current_kind {
                                Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                                None => "no snap ▾".to_string(),
                            };
                            egui::Area::new(ui.id().with(("snap_tag", ti)))
                                .fixed_pos(sp + egui::vec2(7.0, -7.0))
                                .order(egui::Order::Foreground)
                                .show(ui.ctx(), |ui| {
                                    ui.menu_button(label, |ui| {
                                        for (kind, idx) in &tag.options {
                                            if ui.button(render::snap_kind_label(*kind)).clicked() {
                                                on_event(UiEvent::Sketch(
                                                    SketchEvent::ChooseSnap {
                                                        from: tag.from,
                                                        choice: Some(*idx),
                                                    },
                                                ));
                                                ui.close_menu();
                                            }
                                        }
                                        if tag.skippable && ui.button("Don't snap").clicked() {
                                            on_event(UiEvent::Sketch(SketchEvent::ChooseSnap {
                                                from: tag.from,
                                                choice: None,
                                            }));
                                            ui.close_menu();
                                        }
                                    });
                                });
                        }
                    }
                }

                // Construction-phase areas: committed regions (purple, labelled),
                // plus the pending carved area / cut candidates while picking.
                let phase_tint = egui::Color32::from_rgb(200, 120, 255); // purple
                render::render_phase_regions(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    phase_region_polygons,
                    phase_tint,
                );
                render::render_cut_candidates(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    pending_phase_cut_regions,
                );
                if pending_phase_polygon.len() >= 3 {
                    render::render_phase_regions(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        std::slice::from_ref(&(pending_phase_polygon.to_vec(), "?".to_string())),
                        phase_tint,
                    );
                }

                // Floor-step areas: committed regions (teal, labelled with the
                // signed level), plus the pending carved area / cut candidates.
                let step_tint = egui::Color32::from_rgb(60, 220, 200); // teal
                render::render_phase_regions(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    step_region_polygons,
                    step_tint,
                );
                render::render_cut_candidates(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    pending_step_cut_regions,
                );
                if pending_step_polygon.len() >= 3 {
                    render::render_phase_regions(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        std::slice::from_ref(&(pending_step_polygon.to_vec(), "?".to_string())),
                        step_tint,
                    );
                }

                // Ceiling-step areas: committed regions (amber, labelled with
                // the signed delta — hue distinct from the floor steps' teal),
                // the two side half-rects while the side tap is pending, and
                // the dashed provisional line.
                let ceiling_step_tint = egui::Color32::from_rgb(255, 170, 60); // amber
                render::render_phase_regions(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    ceiling_step_region_polygons,
                    ceiling_step_tint,
                );
                render::render_cut_candidates(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    pending_ceiling_step_side_rects,
                );
                if let Some((a, b)) = pending_ceiling_step_line {
                    render::render_ceiling_step_line(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        a,
                        b,
                    );
                }

                // Wall exposure: colour each outline wall by its heat-loss class
                // (brighter in exposure mode; the selected wall is haloed).
                render::render_wall_exposure(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    view,
                    exposure_walls,
                    exposure_selection.as_ref(),
                    exposure_mode,
                );

                // Edit-mode selection highlight: paint the selected wall
                // in red so the user sees which wall their drag is moving.
                // The inserted-vertex marker (if any) takes precedence —
                // both can't be selected at once.
                if matches!(edit_tool, EditTools::Edit) {
                    if let Some((wa, wb)) = edit_selected_wall {
                        render::render_equivalent_wall(
                            &painter,
                            rect,
                            ber,
                            current_storey_idx,
                            mode,
                            view,
                            wa,
                            wb,
                        );
                    }
                    if let Some((vx, vy)) = edit_selected_vertex {
                        render::render_vertex_marker(
                            &painter,
                            rect,
                            ber,
                            current_storey_idx,
                            mode,
                            view,
                            vx,
                            vy,
                        );
                    }
                }

                // Vertex-drop prompt: draw a perpendicular distance arrow
                // for each still-active measurement (axis not yet pinned),
                // so the user sees what each text box refers to.
                if let Some(vp) = &vertex_drop_prompt {
                    for m in &vp.active {
                        use crate::controller::app_controller::VertexMeasurementKind;
                        use manip_2d_plan::ParameterAxis;
                        // Axis measurements get a perpendicular arrow at
                        // the vertex's row/column. WallLength measurements
                        // draw the arrow directly along the diagonal from
                        // neighbour → vertex (= the new wall itself).
                        let (arrow_a, arrow_b) = match m.kind {
                            VertexMeasurementKind::Axis(ParameterAxis::X) => (
                                (m.neighbor_pos.0, vp.vertex_pos.1),
                                (vp.vertex_pos.0, vp.vertex_pos.1),
                            ),
                            VertexMeasurementKind::Axis(ParameterAxis::Y) => (
                                (vp.vertex_pos.0, m.neighbor_pos.1),
                                (vp.vertex_pos.0, vp.vertex_pos.1),
                            ),
                            VertexMeasurementKind::WallLength => (m.neighbor_pos, vp.vertex_pos),
                        };
                        render::render_dimension_prompt(
                            &painter,
                            rect,
                            ber,
                            current_storey_idx,
                            mode,
                            view,
                            arrow_a,
                            arrow_b,
                        );
                    }
                }

                // Post-drag measurement prompt overlay:
                //
                //   * Reference wall highlighted in red (same as the moved
                //     wall) so the user sees both walls involved.
                //   * A cyan arrow drawn PERPENDICULAR to both walls,
                //     spanning the actual distance being measured. For a
                //     vertical wall pair, the arrow is horizontal; for a
                //     horizontal wall pair, vertical.
                if let Some(ep) = &edit_drag_prompt {
                    render::render_equivalent_wall(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        ep.reference_wall.wall_a,
                        ep.reference_wall.wall_b,
                    );
                    // Two parallel axis-aligned walls. Build a virtual
                    // arrow perpendicular to both at the midpoint of the
                    // moved wall's parallel range. render_dimension_prompt
                    // handles drawing the arrow + arrowheads; the
                    // "interior offset" auto-detects None for non-vertex
                    // points so the arrow lands directly on the line.
                    let m_dx = (ep.moved_wall.wall_b.0 - ep.moved_wall.wall_a.0).abs();
                    let m_dy = (ep.moved_wall.wall_b.1 - ep.moved_wall.wall_a.1).abs();
                    let is_vertical = m_dy > m_dx;
                    let (arrow_a, arrow_b) = if is_vertical {
                        // Horizontal arrow at moved wall's midpoint y.
                        let mid_y = (ep.moved_wall.wall_a.1 + ep.moved_wall.wall_b.1) * 0.5;
                        (
                            (ep.reference_wall.wall_a.0, mid_y),
                            (ep.moved_wall.wall_a.0, mid_y),
                        )
                    } else {
                        // Vertical arrow at moved wall's midpoint x.
                        let mid_x = (ep.moved_wall.wall_a.0 + ep.moved_wall.wall_b.0) * 0.5;
                        (
                            (mid_x, ep.reference_wall.wall_a.1),
                            (mid_x, ep.moved_wall.wall_a.1),
                        )
                    };
                    render::render_dimension_prompt(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        arrow_a,
                        arrow_b,
                    );
                }

                // Dimension-prompt overlay. Every wall the user can
                // equivalently measure (including the primary) is highlighted
                // in red. The primary additionally gets the cyan interior
                // arrow + length label, drawn last so it sits on top.
                if let Some(p) = &dim_prompt {
                    for wall in &p.walls {
                        render::render_equivalent_wall(
                            &painter,
                            rect,
                            ber,
                            current_storey_idx,
                            mode,
                            view,
                            wall.wall_a,
                            wall.wall_b,
                        );
                    }
                    if let Some(primary) = p.walls.first() {
                        render::render_dimension_prompt(
                            &painter,
                            rect,
                            ber,
                            current_storey_idx,
                            mode,
                            view,
                            primary.wall_a,
                            primary.wall_b,
                        );
                    }
                }

                // Position-prompt overlay (mirrors the edit-drag prompt's):
                //
                //   * Area wall being measured highlighted in ORANGE, the
                //     outline reference wall in RED — two different plans, so
                //     two colours keep them apart.
                //   * A cyan arrow drawn along the prompt's axis between the
                //     two walls, spanning the distance being measured.
                if let Some(pp) = &position_prompt {
                    render::render_wall_highlight(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        pp.area_wall.wall_a,
                        pp.area_wall.wall_b,
                        egui::Color32::from_rgb(255, 170, 40), // orange: the area wall
                    );
                    render::render_wall_highlight(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        pp.reference_wall.wall_a,
                        pp.reference_wall.wall_b,
                        egui::Color32::from_rgb(255, 80, 80), // red: the outline reference
                    );
                    let (arrow_a, arrow_b) = match pp.axis {
                        manip_2d_plan::ParameterAxis::X => {
                            // Horizontal arrow at the area wall's midpoint y.
                            let mid_y = (pp.area_wall.wall_a.1 + pp.area_wall.wall_b.1) * 0.5;
                            (
                                (pp.reference_wall.wall_a.0, mid_y),
                                (pp.area_wall.wall_a.0, mid_y),
                            )
                        }
                        manip_2d_plan::ParameterAxis::Y => {
                            // Vertical arrow at the area wall's midpoint x.
                            let mid_x = (pp.area_wall.wall_a.0 + pp.area_wall.wall_b.0) * 0.5;
                            (
                                (mid_x, pp.reference_wall.wall_a.1),
                                (mid_x, pp.area_wall.wall_a.1),
                            )
                        }
                    };
                    render::render_dimension_prompt(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        arrow_a,
                        arrow_b,
                    );
                }

                // Provisional curved-wall preview (amber arc) drawn on top, so
                // the user sees the curve while dimensioning its attach lines.
                if let Some(pts) = arc_preview {
                    render::render_arc_preview(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        pts,
                    );
                }

                // Pan / zoom on the canvas.
                //   - Scroll wheel  → zoom about the cursor
                //   - Right-drag    → pan
                if response.hovered() {
                    let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll_y.abs() > 0.0 {
                        let factor = if scroll_y > 0.0 { 1.1 } else { 1.0 / 1.1 };
                        let anchor = ui
                            .input(|i| i.pointer.hover_pos())
                            .unwrap_or_else(|| rect.center());
                        on_event(UiEvent::Sketch(SketchEvent::ZoomView {
                            factor,
                            anchor_x: anchor.x,
                            anchor_y: anchor.y,
                            canvas_center_x: rect.center().x,
                            canvas_center_y: rect.center().y,
                        }));
                    }
                }
                // Middle or right mouse drag pans the canvas. We deliberately
                // skip primary (left) because that's the stroke-drawing button.
                if response.dragged_by(egui::PointerButton::Middle)
                    || response.dragged_by(egui::PointerButton::Secondary)
                {
                    let d = response.drag_delta();
                    if d.x.abs() > 0.0 || d.y.abs() > 0.0 {
                        on_event(UiEvent::Sketch(SketchEvent::PanView { dx: d.x, dy: d.y }));
                    }
                }

                // Pending cut: a primary click picks which candidate region to
                // keep as the living area. Takes priority over stroke/edit input.
                let cut_pending = !pending_cut_regions.is_empty();
                if cut_pending {
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some((wx, wy)) = response.interact_pointer_pos().and_then(|pos| {
                            screen_to_world(ber, current_storey_idx, rect, view, pos)
                        }) {
                            on_event(UiEvent::Sketch(SketchEvent::ChooseCutRegion {
                                world_x: wx,
                                world_y: wy,
                            }));
                        }
                    }
                }

                // Pending PHASE cut: a primary click picks which candidate region
                // to tag with a construction phase. Mirrors the living-area cut.
                let phase_cut_pending = !pending_phase_cut_regions.is_empty();
                if phase_cut_pending {
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some((wx, wy)) = response.interact_pointer_pos().and_then(|pos| {
                            screen_to_world(ber, current_storey_idx, rect, view, pos)
                        }) {
                            on_event(UiEvent::Sketch(SketchEvent::ChoosePhaseCutRegion {
                                world_x: wx,
                                world_y: wy,
                            }));
                        }
                    }
                }

                // Pending STEP cut: a primary click picks which candidate region
                // gets the floor level. Mirrors the phase cut.
                let step_cut_pending = !pending_step_cut_regions.is_empty();
                if step_cut_pending {
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some((wx, wy)) = response.interact_pointer_pos().and_then(|pos| {
                            screen_to_world(ber, current_storey_idx, rect, view, pos)
                        }) {
                            on_event(UiEvent::Sketch(SketchEvent::ChooseStepCutRegion {
                                world_x: wx,
                                world_y: wy,
                            }));
                        }
                    }
                }

                // Ceiling-step taps: in mode with nothing pending, a tap places
                // the line's point (H/V picked next); while the side pick is
                // pending — including with the mode toggled off — the tap picks
                // the stepping side. Other stages ignore taps (the controller's
                // stage dispatch owns that), so this can stay coarse.
                if ceiling_step_mode || ceiling_step_side_pending {
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some((wx, wy)) = response.interact_pointer_pos().and_then(|pos| {
                            screen_to_world(ber, current_storey_idx, rect, view, pos)
                        }) {
                            on_event(UiEvent::Sketch(SketchEvent::CeilingStepTap {
                                world_x: wx,
                                world_y: wy,
                            }));
                        }
                    }
                }

                // Exposure mode: a tap selects the wall under the pointer; a
                // primary drag near a selection handle slides that handle along
                // the wall. Takes priority over stroke/edit input (suppressed via
                // `primary_available` above).
                if exposure_mode && !cut_pending {
                    if let Some(t) = canvas_transform(ber, current_storey_idx, rect, view, false) {
                        let tol_world = (12.0 / t.scale()) as f64;
                        if response.drag_started_by(egui::PointerButton::Primary) {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let (wx, wy) = t.inverse(pos.x, pos.y);
                                on_event(UiEvent::Sketch(SketchEvent::ExposurePointerDown {
                                    world_x: wx,
                                    world_y: wy,
                                    tol: tol_world,
                                }));
                            }
                        }
                        if response.dragged_by(egui::PointerButton::Primary) {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let (wx, wy) = t.inverse(pos.x, pos.y);
                                on_event(UiEvent::Sketch(SketchEvent::ExposurePointerDrag {
                                    world_x: wx,
                                    world_y: wy,
                                }));
                            }
                        }
                        if response.drag_stopped_by(egui::PointerButton::Primary) {
                            on_event(UiEvent::Sketch(SketchEvent::ExposurePointerUp));
                        }
                        // A plain tap (no drag) still selects the wall under it.
                        if response.clicked_by(egui::PointerButton::Primary) {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let (wx, wy) = t.inverse(pos.x, pos.y);
                                on_event(UiEvent::Sketch(SketchEvent::ExposurePointerDown {
                                    world_x: wx,
                                    world_y: wy,
                                    tol: tol_world,
                                }));
                            }
                        }
                    }
                }

                // Primary-button input: stroke drawing OR Edit-mode
                // click/drag, depending on `edit_tool`. Both gated on
                // 2D mode and "no overlay is open" (review / pending
                // redraw / dim prompt / cut-pick take priority).
                let primary_available = matches!(mode, SketchMode::Storey2D)
                    && !reviewing
                    && !pending_redraw
                    && !dim_prompting
                    && !cut_pending
                    && !phase_cut_pending
                    && !phase_pick_pending
                    && !step_cut_pending
                    && !step_pick_pending
                    && !ceiling_step_pending
                    && !exposure_mode;

                if primary_available && !matches!(edit_tool, EditTools::Edit) {
                    if response.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let local_pos = pos - rect.left_top().to_vec2();
                            on_event(UiEvent::Sketch(SketchEvent::StartStroke(local_pos)));
                        }
                    }
                    if response.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let local_pos = pos - rect.left_top().to_vec2();
                            on_event(UiEvent::Sketch(SketchEvent::AddPoint(local_pos)));
                        }
                    }
                    if response.drag_stopped_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let local_pos = pos - rect.left_top().to_vec2();
                            // Extend and Trim both operate on the existing
                            // polygon, which may be in real (meter) units while
                            // the stroke is in canvas pixels. Convert each
                            // captured stroke point to world coords via the fit
                            // transform and send the tool's dedicated world-coord
                            // event. Other stroke tools fall through to the
                            // normal EndStroke flow (canvas pixels).
                            // A living-area DrawPerpWalls is drawn in the storey's
                            // (metre) space too, so it also needs world coords.
                            let living_area_draw =
                                living_area_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                            // Construction-phase tagging draws in the storey's
                            // metre space too (carve an area to assign a year).
                            let phase_draw =
                                phase_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                            // Floor-step tagging draws in the storey's metre
                            // space too (carve an area to raise/lower its floor).
                            let step_draw =
                                step_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                            // A ceiling-step stroke is the LINE the ceiling
                            // steps along — drawn in metre space as well.
                            let ceiling_step_draw =
                                ceiling_step_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                            // First outline of a fresh upper storey: drawn over
                            // the previous-storey watermark, so it must be
                            // captured in that storey's metre space (mirrors the
                            // watermark predicate in render::render_ber).
                            let outline_on_watermark =
                                matches!(edit_tool, EditTools::DrawPerpWalls)
                                    && !living_area_mode
                                    && !phase_mode
                                    && !step_mode
                                    && !ceiling_step_mode
                                    && matches!(current_storey_idx, Some(i) if i > 0)
                                    && ber
                                        .and_then(|b| b.storeys.get(current_storey_idx?))
                                        .is_some_and(|s| s.floors.regions.is_empty());
                            let needs_world = living_area_draw
                                || phase_draw
                                || step_draw
                                || ceiling_step_draw
                                || outline_on_watermark
                                || matches!(
                                    edit_tool,
                                    EditTools::ExtendOrtho
                                        | EditTools::ExtendCurve
                                        | EditTools::Trim
                                        | EditTools::TrimOrtho
                                        | EditTools::TrimCurve
                                );
                            let world_converted = if needs_world {
                                // Outline capture must invert the SAME transform
                                // the watermark was drawn with (bbox includes the
                                // previous storey), hence `watermark = true`.
                                if let (Some(stroke), Some(t)) = (
                                    current.as_ref(),
                                    canvas_transform(ber, current_storey_idx, rect, view, true),
                                ) {
                                    let topleft = rect.left_top();
                                    let world_points: Vec<(f32, f32)> = stroke
                                        .points
                                        .iter()
                                        .map(|p| {
                                            let abs_x = p.x + topleft.x;
                                            let abs_y = p.y + topleft.y;
                                            let (wx, wy) = t.inverse(abs_x, abs_y);
                                            (wx as f32, wy as f32)
                                        })
                                        .collect();
                                    // Living-area draw starts a new sub-shape;
                                    // an outline-on-watermark sends
                                    // OutlineStrokeWorld; trims send
                                    // TrimStrokeWorld, extends send
                                    // ExtendStrokeWorld; the controller dispatches
                                    // the exact variant by the active tool.
                                    let event = if living_area_draw {
                                        SketchEvent::LivingAreaStrokeWorld {
                                            points: world_points,
                                        }
                                    } else if phase_draw {
                                        SketchEvent::PhaseRegionStrokeWorld {
                                            points: world_points,
                                        }
                                    } else if step_draw {
                                        SketchEvent::StepRegionStrokeWorld {
                                            points: world_points,
                                        }
                                    } else if ceiling_step_draw {
                                        SketchEvent::CeilingStepStrokeWorld {
                                            points: world_points,
                                        }
                                    } else if outline_on_watermark {
                                        SketchEvent::OutlineStrokeWorld {
                                            points: world_points,
                                        }
                                    } else if matches!(
                                        edit_tool,
                                        EditTools::Trim
                                            | EditTools::TrimOrtho
                                            | EditTools::TrimCurve
                                    ) {
                                        SketchEvent::TrimStrokeWorld {
                                            points: world_points,
                                        }
                                    } else {
                                        SketchEvent::ExtendStrokeWorld {
                                            points: world_points,
                                        }
                                    };
                                    on_event(UiEvent::Sketch(event));
                                    true
                                } else {
                                    false
                                }
                            } else {
                                false
                            };
                            if !world_converted {
                                // Diagnostic: an outline-on-watermark stroke that
                                // can't convert to world coords falls back to the
                                // pixel path (no snap, re-scales from scratch).
                                // Surface WHY so the failure isn't silent.
                                if outline_on_watermark {
                                    let has_stroke = current.is_some();
                                    let has_bbox =
                                        canvas_transform(ber, current_storey_idx, rect, view, true)
                                            .is_some();
                                    eprintln!(
                                        "[new-storey] WARNING: outline-on-watermark stroke could \
                                         NOT convert to world coords (stroke present={has_stroke}, \
                                         fit bbox present={has_bbox}) — falling back to the pixel \
                                         path. If bbox is missing, the previous storey has no \
                                         committed floor outline to anchor the transform."
                                    );
                                }
                                on_event(UiEvent::Sketch(SketchEvent::EndStroke(local_pos)));
                            }
                        }
                    }

                    if let Some(stroke) = current {
                        draw_stroke(&painter, rect, stroke);
                    }
                }

                // Edit-mode primary input: click to select a wall, drag to
                // translate it. Suspended while a post-drag measurement
                // prompt is up — that prompt has to be Apply / Cycle /
                // Cancel first.
                if primary_available
                    && matches!(edit_tool, EditTools::Edit)
                    && edit_drag_prompt.is_none()
                    && vertex_drop_prompt.is_none()
                {
                    // One transform for both the double-click vertex-insert and
                    // the click/drag below (same storey, same frame).
                    if let Some(t) = canvas_transform(ber, current_storey_idx, rect, view, false) {
                        // World-coord tolerance corresponds to ~12 pixels
                        // on screen at the current zoom.
                        let tol_world = 12.0 / t.scale();
                        // Double-click first — egui fires this on the second
                        // press of a quick double-press. We insert the vertex
                        // and treat any subsequent drag movement as moving the
                        // just-inserted vertex.
                        if response.double_clicked_by(egui::PointerButton::Primary) {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let (wx, wy) = t.inverse(pos.x, pos.y);
                                on_event(UiEvent::Sketch(SketchEvent::EditDoubleClick {
                                    world_x: wx,
                                    world_y: wy,
                                    tol: tol_world,
                                }));
                            }
                        }
                        if response.drag_started_by(egui::PointerButton::Primary) {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let (wx, wy) = t.inverse(pos.x, pos.y);
                                on_event(UiEvent::Sketch(SketchEvent::EditMouseDown {
                                    world_x: wx,
                                    world_y: wy,
                                    tol: tol_world,
                                }));
                            }
                        }
                        if response.dragged_by(egui::PointerButton::Primary) {
                            if let Some(pos) = response.interact_pointer_pos() {
                                let (wx, wy) = t.inverse(pos.x, pos.y);
                                on_event(UiEvent::Sketch(SketchEvent::EditMouseDrag {
                                    world_x: wx,
                                    world_y: wy,
                                }));
                            }
                        }
                        if response.drag_stopped_by(egui::PointerButton::Primary) {
                            on_event(UiEvent::Sketch(SketchEvent::EditMouseUp));
                        }
                    }
                }

                ui.separator();

                // Bottom toolbar — priority: pending redraw > dimension prompt
                // > collinearity review > edit tools. The "Fit" button (reset
                // view) is always available alongside.
                //
                // The All-storeys tab (`current_storey_idx == None`) is read-only,
                // so drawing/edit tools are hidden — only the Fit button stays.
                ui.horizontal(|ui| {
                    // The ceiling-height prompt outranks everything — it's the
                    // blocked 3D request waiting on its height. Shown on the
                    // All-storeys tab too (full-house 3D gates there), so it
                    // sits before the read-only early-return below.
                    if ceiling_height_prompt {
                        Self::ceiling_height_prompt_toolbar(
                            ui,
                            ceiling_height_input,
                            ceiling_height_error,
                            &mut on_event,
                        );
                        return;
                    }
                    if current_storey_idx.is_none() {
                        ui.label(
                            egui::RichText::new("Pick a storey tab to edit.")
                                .color(egui::Color32::from_rgb(180, 180, 180)),
                        );
                        ui.separator();
                        Self::fit_button(ui, &mut on_event);
                        return;
                    }
                    if pending_redraw {
                        Self::pending_redraw_toolbar(ui, &mut on_event);
                    } else if let Some(bulge) = curve_bulge {
                        Self::curve_radius_toolbar(ui, bulge, &mut on_event);
                    } else if let Some(p) = dim_prompt {
                        Self::dimension_prompt_toolbar(
                            ui,
                            p,
                            dim_scale_set,
                            dim_input,
                            dim_error,
                            &mut on_event,
                        );
                    } else if let Some(p) = position_prompt {
                        Self::position_prompt_toolbar(
                            ui,
                            p,
                            position_input,
                            position_error,
                            &mut on_event,
                        );
                    } else if let Some(p) = edit_drag_prompt {
                        Self::edit_drag_prompt_toolbar(
                            ui,
                            p,
                            edit_drag_input,
                            edit_drag_error,
                            &mut on_event,
                        );
                    } else if let Some(vp) = &vertex_drop_prompt {
                        Self::vertex_drop_prompt_toolbar(
                            ui,
                            vp,
                            vertex_drop_inputs,
                            vertex_drop_error,
                            &mut on_event,
                        );
                    } else if reviewing {
                        Self::review_toolbar(ui, review_remaining, &mut on_event);
                    } else if exposure_mode {
                        Self::exposure_toolbar(
                            ui,
                            exposure_walls,
                            exposure_selection.as_ref(),
                            exposure_start_input,
                            exposure_end_input,
                            &mut on_event,
                        );
                    } else if let Some(new_tool) = Self::edit_toolbar(ui, edit_tool, has_floor_plan)
                    {
                        on_event(UiEvent::Sketch(SketchEvent::ChangeEditTool(new_tool)));
                    }
                    ui.separator();
                    // Living-area mode toggle: draw potential living spaces
                    // inside the storey. Only meaningful once an outline exists.
                    if has_floor_plan {
                        let mut la = living_area_mode;
                        if ui
                            .toggle_value(&mut la, "🏠 Living areas")
                            .on_hover_text(
                                "Draw potential living-space sub-areas inside the storey \
                                 (drawn & dimensioned the same way; largest chosen later)",
                            )
                            .changed()
                        {
                            on_event(UiEvent::Sketch(SketchEvent::SetLivingAreaMode(la)));
                        }
                        // In living-area mode, delete the active area (living
                        // areas are non-editable — redraw to change).
                        if living_area_mode {
                            if let Some(idx) = active_living_area {
                                if ui
                                    .button("🗑 Delete area")
                                    .on_hover_text("Delete this living area (redraw to change one)")
                                    .clicked()
                                {
                                    on_event(UiEvent::Sketch(SketchEvent::DeleteLivingArea(idx)));
                                }
                            }
                        }
                        // Wall-exposure mode toggle: classify the envelope walls
                        // for heat loss (exposed / semi-exposed / non-heat-loss).
                        let mut ex = exposure_mode;
                        if ui
                            .toggle_value(&mut ex, "🌡 Exposure")
                            .on_hover_text(
                                "Mark each storey wall's heat-loss exposure: tap a wall, \
                                 then pick exposed / semi-exposed / non-heat-loss",
                            )
                            .changed()
                        {
                            on_event(UiEvent::Sketch(SketchEvent::SetExposureMode(ex)));
                        }
                        // Construction-phase (year-of-construction) tagging:
                        // select an area, assign it one of the dates entered up
                        // front. Only useful once dates exist (picker non-empty).
                        if !phase_choices.is_empty() {
                            let mut ph = phase_mode;
                            if ui
                                .toggle_value(&mut ph, "📅 Construction year")
                                .on_hover_text(
                                    "Tag an area with its year of construction: draw an \
                                     area (like a living area), then pick the year",
                                )
                                .changed()
                            {
                                on_event(UiEvent::Sketch(SketchEvent::SetPhaseMode(ph)));
                            }
                            // Also shown when a pick is PENDING with the mode off
                            // (a restored mid-flight area after reopen): the pick
                            // blocks drawing, so its prompt must stay reachable.
                            if phase_mode || phase_pick_pending {
                                Self::phase_toolbar(
                                    ui,
                                    phase_choices,
                                    phase_region_polygons,
                                    phase_pick_pending,
                                    &mut on_event,
                                );
                            }
                        }
                        // Floor-step tagging: select an area of the floor and
                        // raise/lower it relative to the storey datum.
                        let mut st = step_mode;
                        if ui
                            .toggle_value(&mut st, "↕ Floor steps")
                            .on_hover_text(
                                "Mark a floor area as raised or lowered: draw an area \
                                 (like a living area), then enter the step amount",
                            )
                            .changed()
                        {
                            on_event(UiEvent::Sketch(SketchEvent::SetStepMode(st)));
                        }
                        // Same pending-with-mode-off rule as the phase toolbar.
                        if step_mode || step_pick_pending {
                            Self::step_toolbar(
                                ui,
                                step_region_polygons,
                                step_pick_pending,
                                step_amount_input,
                                &mut on_event,
                            );
                        }
                        // Ceiling-step tagging: draw a line across the plan,
                        // tap the side that steps, enter the amount. Disabled
                        // until the storey has a default ceiling height (the
                        // deltas are relative to it) — the controller setter
                        // re-enforces the gate.
                        let cs_resp = ui
                            .add_enabled(
                                ceiling_step_available,
                                egui::SelectableLabel::new(ceiling_step_mode, "⬆ Ceiling steps"),
                            )
                            .on_hover_text(
                                "Raise or lower part of the ceiling: draw a straight line \
                                 across the plan, tap the side that steps, then enter the amount",
                            )
                            .on_disabled_hover_text(
                                "Set the storey's default ceiling height first",
                            );
                        if cs_resp.clicked() {
                            on_event(UiEvent::Sketch(SketchEvent::SetCeilingStepMode(
                                !ceiling_step_mode,
                            )));
                        }
                        // Same pending-with-mode-off rule as the other tools.
                        if ceiling_step_mode || ceiling_step_pending {
                            Self::ceiling_step_toolbar(
                                ui,
                                ceiling_step_region_polygons,
                                ceiling_step_orientation_pending,
                                ceiling_step_side_pending,
                                ceiling_step_amount_pending,
                                ceiling_amount_input,
                                &mut on_event,
                            );
                        }
                    }
                    ui.separator();
                    Self::fit_button(ui, &mut on_event);
                    ui.separator();
                    Self::undo_redo_buttons(ui, can_undo, can_redo, &mut on_event);
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
            });
        });
    }

    fn undo_redo_buttons<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        can_undo: bool,
        can_redo: bool,
        on_event: &mut F,
    ) {
        if ui
            .add_enabled(can_undo, egui::Button::new("↶ Undo"))
            .on_hover_text("Undo (Ctrl+Z)")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::Undo));
        }
        if ui
            .add_enabled(can_redo, egui::Button::new("↷ Redo"))
            .on_hover_text("Redo (Ctrl+Shift+Z or Ctrl+Y)")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::Redo));
        }
        if ui
            .button("📌 Checkpoint")
            .on_hover_text(
                "DEV: save this plan as the checkpoint, reloaded on `BER_CHECKPOINT=1 cargo run`",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::SaveCheckpoint));
        }
        if ui
            .button("↩ Un-snap last")
            .on_hover_text("Replay the last drawn action with its elective snaps disabled")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::UnsnapLast));
        }
    }

    fn fit_button<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
        if ui
            .button("Fit")
            .on_hover_text("Reset view (zoom/pan)")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ResetView));
        }
    }

    fn dimension_prompt_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        prompt: DimensionPromptUiData,
        scale_set: bool,
        dim_input: &mut String,
        dim_error: Option<&str>,
        on_event: &mut F,
    ) {
        let units = if scale_set {
            "m"
        } else {
            "(first — sets scale)"
        };
        ui.label(
            egui::RichText::new(format!("Length of highlighted wall {}:", units))
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        // Show the current value for orientation. Useful especially for the
        // first dimension — the assessor sees "looks like ~3.something m" and
        // confirms with an exact measurement.
        ui.label(format!("(currently {:.2})", prompt.current_length));
        let resp = ui.add(
            egui::TextEdit::singleline(dim_input)
                .desired_width(80.0)
                .hint_text("e.g. 4.2"),
        );
        let submit_pressed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let apply_clicked = ui
            .button("Apply")
            .on_hover_text("Apply this dimension and move to the next")
            .clicked();
        if apply_clicked || submit_pressed {
            if let Some(value) = parse_dimension_expression(dim_input) {
                on_event(UiEvent::Sketch(SketchEvent::ApplyDimension(value)));
                dim_input.clear();
            }
        }
        if ui
            .button("Skip")
            .on_hover_text("Skip this dimension, ask about a different wall")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::SkipDimension));
            dim_input.clear();
        }
        // Surface the last error inline so the user can correct without
        // losing what they typed.
        if let Some(err) = dim_error {
            ui.label(
                egui::RichText::new(format!("⚠ {}", err))
                    .color(egui::Color32::from_rgb(255, 120, 90)),
            );
        }
    }

    /// Toolbar for the position prompt of a carved phase/step area: the
    /// distance (along the prompt's axis) from the highlighted outline
    /// reference wall to the highlighted area wall. The textbox arrives
    /// PREFILLED with the drawn distance (see `BerApp`'s prompt-identity
    /// refill); Apply pins it, Skip accepts the drawn position, and the
    /// cycle button moves to another reference wall.
    fn position_prompt_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        prompt: PositionPromptUiData,
        input: &mut String,
        error: Option<&str>,
        on_event: &mut F,
    ) {
        let direction = match prompt.axis {
            manip_2d_plan::ParameterAxis::X => "Horizontal",
            manip_2d_plan::ParameterAxis::Y => "Vertical",
        };
        ui.label(
            egui::RichText::new(format!(
                "{} distance from the highlighted wall (m):",
                direction
            ))
            .strong()
            .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        let resp = ui.add(
            egui::TextEdit::singleline(input)
                .desired_width(80.0)
                .hint_text("e.g. 3.5"),
        );
        let submit_pressed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let apply_clicked = ui
            .button("Apply")
            .on_hover_text("Pin the area at exactly this distance")
            .clicked();
        if apply_clicked || submit_pressed {
            if let Some(value) = parse_dimension_expression(input) {
                on_event(UiEvent::Sketch(SketchEvent::ApplyPositionDimension(value)));
            }
        }
        if ui
            .button("Skip")
            .on_hover_text("Accept the drawn position as-is")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::SkipPositionDimension));
        }
        // Only offered when there IS another wall to measure from.
        if prompt.option_count > 1
            && ui
                .button(format!(
                    "Measure from another wall ({}/{})",
                    prompt.option_idx + 1,
                    prompt.option_count,
                ))
                .on_hover_text("Measure the distance from a different outline wall")
                .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::CyclePositionReference));
        }
        if let Some(err) = error {
            ui.label(
                egui::RichText::new(format!("⚠ {}", err))
                    .color(egui::Color32::from_rgb(255, 120, 90)),
            );
        }
    }

    fn edit_drag_prompt_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        prompt: EditDragPromptUiData,
        input: &mut String,
        error: Option<&str>,
        on_event: &mut F,
    ) {
        ui.label(
            egui::RichText::new("Distance from highlighted reference wall (m):")
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        ui.label(format!(
            "(currently {:.2}, option {}/{})",
            prompt.current_distance,
            prompt.option_idx + 1,
            prompt.option_count,
        ));
        let resp = ui.add(
            egui::TextEdit::singleline(input)
                .desired_width(80.0)
                .hint_text("e.g. 3.5"),
        );
        let submit_pressed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let apply_clicked = ui
            .button("Apply")
            .on_hover_text("Apply this distance")
            .clicked();
        if apply_clicked || submit_pressed {
            if let Some(value) = parse_dimension_expression(input) {
                on_event(UiEvent::Sketch(SketchEvent::ApplyEditDrag(value)));
                input.clear();
            }
        }
        if ui
            .button("Different measurement")
            .on_hover_text("Try another reference wall")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::CycleEditDragMeasurement));
        }
        if ui
            .button("Cancel")
            .on_hover_text("Revert the wall to its position before this drag")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::CancelEditDrag));
            input.clear();
        }
        if let Some(err) = error {
            ui.label(
                egui::RichText::new(format!("⚠ {}", err))
                    .color(egui::Color32::from_rgb(255, 120, 90)),
            );
        }
    }

    fn vertex_drop_prompt_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        prompt: &VertexDropPromptUiData,
        inputs: &mut [String; 4],
        error: Option<&str>,
        on_event: &mut F,
    ) {
        use crate::controller::app_controller::VertexMeasurementKind;
        use manip_2d_plan::ParameterAxis;
        ui.label(
            egui::RichText::new("Place vertex — fill in any 2 distances (m):")
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        // One column per active measurement. Each shows: kind label,
        // current value for orientation, textbox, Apply button.
        for m in &prompt.active {
            let kind_label = match m.kind {
                VertexMeasurementKind::Axis(ParameterAxis::X) => "H",
                VertexMeasurementKind::Axis(ParameterAxis::Y) => "V",
                VertexMeasurementKind::WallLength => "W",
            };
            ui.label(format!("{} ({:.2})", kind_label, m.current_distance));
            // Use idx-based slot so inputs survive across frames + apply
            // targets the right measurement. Cap at 4 slots.
            let slot = m.idx.min(inputs.len() - 1);
            let resp = ui.add(
                egui::TextEdit::singleline(&mut inputs[slot])
                    .desired_width(60.0)
                    .hint_text("=?"),
            );
            let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let clicked = ui.button("✓").on_hover_text("Set this distance").clicked();
            if clicked || submitted {
                if let Some(v) = parse_dimension_expression(&inputs[slot]) {
                    on_event(UiEvent::Sketch(SketchEvent::ApplyVertexMeasurement {
                        idx: m.idx,
                        value: v,
                    }));
                    inputs[slot].clear();
                }
            }
            ui.separator();
        }
        if ui
            .button("Cancel")
            .on_hover_text("Revert the new vertex to its initial position")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::CancelVertexDrop));
            for s in inputs.iter_mut() {
                s.clear();
            }
        }
        if let Some(err) = error {
            ui.label(
                egui::RichText::new(format!("⚠ {}", err))
                    .color(egui::Color32::from_rgb(255, 120, 90)),
            );
        }
    }

    /// The first-3D ceiling-height prompt: a 3D view was requested but the
    /// current scope has no default ceiling height yet. One typed value (m,
    /// must be > 0) stamps the scope and opens the 3D view; Cancel stays 2D.
    fn ceiling_height_prompt_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        input: &mut String,
        error: Option<&str>,
        on_event: &mut F,
    ) {
        ui.label(
            egui::RichText::new("Default ceiling height (m):")
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        let resp = ui.add(
            egui::TextEdit::singleline(input)
                .desired_width(80.0)
                .hint_text("e.g. 2.4"),
        );
        let submit_pressed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let set_clicked = ui
            .button("Set")
            .on_hover_text("Set this height and open the 3D view")
            .clicked();
        if set_clicked || submit_pressed {
            // Validation (finite, > 0) lives in the controller — emit any
            // parsed value so its error message surfaces next to the box
            // instead of the button silently doing nothing. The dispatch arm
            // clears the input on success; on rejection the text stays put.
            if let Some(value) = parse_dimension_expression(input) {
                on_event(UiEvent::Sketch(SketchEvent::SetDefaultCeilingHeight(value)));
            }
        }
        if ui
            .button("Cancel")
            .on_hover_text("Stay in 2D without setting a height")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::CancelCeilingHeightPrompt));
            input.clear();
        }
        if let Some(err) = error {
            ui.label(
                egui::RichText::new(format!("⚠ {}", err))
                    .color(egui::Color32::from_rgb(255, 120, 90)),
            );
        }
    }

    /// Radius/bulge slider for a provisional curve whose attach lines are set.
    /// Dragging it reshapes the amber preview live; "Accept curve" bakes it in.
    /// `bulge` is the signed sagitta / chord (0 = flat, larger = more curved).
    fn curve_radius_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, bulge: f32, on_event: &mut F) {
        ui.label(
            egui::RichText::new("Curve radius:")
                .strong()
                .color(egui::Color32::from_rgb(255, 200, 60)),
        );
        let mut b = bulge;
        if ui
            .add(
                egui::Slider::new(&mut b, -1.5..=1.5)
                    .text("bulge")
                    .show_value(false),
            )
            .on_hover_text("Drag to flatten (toward 0) or deepen the curve; sign flips the side")
            .changed()
        {
            on_event(UiEvent::Sketch(SketchEvent::SetCurveBulge(b)));
        }
        if ui
            .button("Accept curve")
            .on_hover_text("Bake the curve into the plan at this radius")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::CommitCurve));
        }
    }

    fn pending_redraw_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
        ui.label(
            egui::RichText::new("A storey already exists. What should this new stroke do?")
                .strong()
                .color(egui::Color32::from_rgb(255, 200, 60)),
        );
        if ui
            .button("Replace this storey")
            .on_hover_text("Discard the current outline and replace it with this stroke")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ReplaceStorey));
        }
        if ui
            .button("New storey from this")
            .on_hover_text("Save current storey, start a new one from this stroke (coming soon)")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::AddStoreyFromCurrent));
        }
        if ui
            .button("Ignore")
            .on_hover_text("Discard this stroke, keep the current storey")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::IgnoreRedraw));
        }
    }

    fn review_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, remaining: usize, on_event: &mut F) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Same line? {} remaining", remaining))
                    .strong()
                    .color(egui::Color32::from_rgb(255, 200, 60)),
            );
            if ui.button("Yes").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::AcceptCollinearity));
            }
            if ui.button("No").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::RejectCollinearity));
            }
            if ui.button("Skip all").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::SkipAllCollinearity));
            }
        });
    }

    /// Wall-exposure mode toolbar. With a wall selected: a section selection
    /// `start–end` (live with the drag handles) plus typed start/end inputs, the
    /// three type buttons that paint the current selection, and "Clear wall".
    /// Always: "Set remaining → Exposed" bulk-marks every still-unset wall.
    /// Construction-phase tagging controls. While a carved area is pending,
    /// shows the year picker (one button per defined phase) + Cancel; otherwise
    /// lists the committed phase regions with delete buttons.
    fn phase_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        choices: &[(ConstructionPhase, String)],
        regions: &[(Vec<(f64, f64)>, String)],
        pick_pending: bool,
        on_event: &mut F,
    ) {
        if pick_pending {
            ui.label(egui::RichText::new("Assign year to area:").strong());
            ui.horizontal_wrapped(|ui| {
                for (phase, label) in choices {
                    if ui.button(label).clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::AssignPhase(phase.clone())));
                    }
                }
                if ui.button("Cancel").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::CancelPendingPhase));
                }
            });
        } else {
            ui.label("Draw an area to tag its year of construction.");
            for (i, (_ring, label)) in regions.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(label);
                    if ui
                        .button("🗑")
                        .on_hover_text("Delete this phase area")
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::DeletePhaseRegion(i)));
                    }
                });
            }
        }
    }

    /// Floor-step controls. While a carved area awaits its level, shows the
    /// amount field (metres) + Raise/Lower buttons (sign the amount) + Cancel;
    /// otherwise lists the committed step regions with delete buttons.
    fn step_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        regions: &[(Vec<(f64, f64)>, String)],
        pick_pending: bool,
        amount_input: &mut String,
        on_event: &mut F,
    ) {
        if pick_pending {
            ui.label(egui::RichText::new("Step amount (m):").strong());
            ui.add(
                egui::TextEdit::singleline(amount_input)
                    .desired_width(60.0)
                    .hint_text("e.g. 0.15"),
            );
            // Unparseable input is ignored — the buttons just do nothing. The
            // BUTTON owns the sign: an expression that evaluates negative
            // (e.g. "1-2") must not flip Lower into a raise, so take |v|.
            let amount = parse_dimension_expression(amount_input).map(f32::abs);
            if ui
                .button("Raise")
                .on_hover_text("Raise this area's floor by the amount")
                .clicked()
            {
                if let Some(v) = amount {
                    on_event(UiEvent::Sketch(SketchEvent::AssignStepLevel(v)));
                    amount_input.clear();
                }
            }
            if ui
                .button("Lower")
                .on_hover_text("Lower this area's floor by the amount")
                .clicked()
            {
                if let Some(v) = amount {
                    on_event(UiEvent::Sketch(SketchEvent::AssignStepLevel(-v)));
                    amount_input.clear();
                }
            }
            if ui.button("Cancel").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::CancelPendingStep));
                amount_input.clear();
            }
        } else {
            ui.label("Draw an area to raise or lower its floor.");
            for (i, (_ring, label)) in regions.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(label);
                    if ui
                        .button("🗑")
                        .on_hover_text("Delete this step area")
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::DeleteStepRegion(i)));
                    }
                });
            }
        }
    }

    /// Ceiling-step controls, stage by stage: the H/V picker for a tapped
    /// point; the "tap the side" hint while the side pick is pending; the
    /// amount field (metres) + Up/Down buttons (which own the sign) once the
    /// line is resolved; otherwise the committed-region list with deletes.
    /// (The position stage shows as the shared position-prompt panel instead.)
    #[allow(clippy::too_many_arguments)]
    fn ceiling_step_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        regions: &[(Vec<(f64, f64)>, String)],
        orientation_pending: bool,
        side_pending: bool,
        amount_pending: bool,
        amount_input: &mut String,
        on_event: &mut F,
    ) {
        if orientation_pending {
            ui.label(egui::RichText::new("Line direction:").strong());
            if ui
                .button("Horizontal")
                .on_hover_text("A horizontal step line through the tapped point")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ChooseCeilingStepOrientation {
                    horizontal: true,
                }));
            }
            if ui
                .button("Vertical")
                .on_hover_text("A vertical step line through the tapped point")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ChooseCeilingStepOrientation {
                    horizontal: false,
                }));
            }
            if ui.button("Cancel").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::CancelPendingCeilingStep));
            }
        } else if side_pending {
            ui.label(
                egui::RichText::new("Tap the side of the line where the ceiling steps")
                    .color(egui::Color32::from_rgb(255, 200, 60)),
            );
            if ui.button("Cancel").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::CancelPendingCeilingStep));
            }
        } else if amount_pending {
            ui.label(egui::RichText::new("Step (m):").strong());
            ui.add(
                egui::TextEdit::singleline(amount_input)
                    .desired_width(60.0)
                    .hint_text("e.g. 0.2"),
            );
            // Unparseable input is ignored — the buttons just do nothing. The
            // BUTTON owns the sign (an expression that evaluates negative must
            // not flip Down into a raise), so take |v|.
            let amount = parse_dimension_expression(amount_input).map(f32::abs);
            if ui
                .button("Up")
                .on_hover_text("Raise the ceiling on that side by the amount")
                .clicked()
            {
                if let Some(v) = amount {
                    on_event(UiEvent::Sketch(SketchEvent::AssignCeilingStepDelta(v)));
                    amount_input.clear();
                }
            }
            if ui
                .button("Down")
                .on_hover_text("Lower the ceiling on that side by the amount")
                .clicked()
            {
                if let Some(v) = amount {
                    on_event(UiEvent::Sketch(SketchEvent::AssignCeilingStepDelta(-v)));
                    amount_input.clear();
                }
            }
            if ui.button("Cancel").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::CancelPendingCeilingStep));
                amount_input.clear();
            }
        } else {
            ui.label("Draw a straight line across the plan to step the ceiling.");
            for (i, (_ring, label)) in regions.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(label);
                    if ui
                        .button("🗑")
                        .on_hover_text("Delete this ceiling step (re-flattens to the default)")
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::DeleteCeilingStepRegion(i)));
                    }
                });
            }
        }
    }

    fn exposure_toolbar<F: FnMut(UiEvent)>(
        ui: &mut egui::Ui,
        walls: &[WallExposureView],
        selection: Option<&ExposureSelection>,
        start_input: &mut String,
        end_input: &mut String,
        on_event: &mut F,
    ) {
        let unset = walls.iter().filter(|w| w.sections.is_empty()).count();
        ui.horizontal(|ui| {
            if let Some(sel) = selection {
                let (lo, hi) = (sel.start_m.min(sel.end_m), sel.start_m.max(sel.end_m));
                ui.label(
                    egui::RichText::new(format!(
                        "Section {lo:.2}–{hi:.2} m (wall {:.2} m):",
                        sel.length_m
                    ))
                    .strong(),
                );
                ui.label("start");
                let rs = ui.add(
                    egui::TextEdit::singleline(start_input)
                        .desired_width(46.0)
                        .hint_text(format!("{lo:.2}")),
                );
                ui.label("end");
                let re = ui.add(
                    egui::TextEdit::singleline(end_input)
                        .desired_width(46.0)
                        .hint_text(format!("{hi:.2}")),
                );
                let enter = (rs.lost_focus() || re.lost_focus())
                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("Set range").clicked() || enter {
                    let s = parse_dimension_expression(start_input).unwrap_or(lo);
                    let e = parse_dimension_expression(end_input).unwrap_or(hi);
                    on_event(UiEvent::Sketch(SketchEvent::SetExposureSectionRange {
                        start_m: s,
                        end_m: e,
                    }));
                    start_input.clear();
                    end_input.clear();
                }
                ui.separator();
                if ui.button("Exposed").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SetWallExposure(
                        WallExposure::Exposed,
                    )));
                }
                if ui.button("Semi-exposed").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SetWallExposure(
                        WallExposure::SemiExposed,
                    )));
                }
                if ui.button("Non-heat-loss").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SetWallExposure(
                        WallExposure::NonHeatLoss,
                    )));
                }
                if ui.button("Clear wall").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::ClearWallExposure));
                }
            } else {
                ui.label(
                    egui::RichText::new("Tap a wall to mark its exposure")
                        .color(egui::Color32::from_rgb(255, 200, 60)),
                );
            }
            ui.separator();
            if ui
                .add_enabled(
                    unset > 0,
                    egui::Button::new(format!("Set remaining {unset} → Exposed")),
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetAllRemainingExposed));
            }
        });
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
                    response = response
                        .on_hover_text("Sets the default ceiling height first — 3D needs it");
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

    fn edit_toolbar(ui: &mut egui::Ui, tool: EditTools, has_floor_plan: bool) -> Option<EditTools> {
        let mut change_tool: Option<EditTools> = None;
        ui.horizontal(|ui| {
            for tool_item in EditTools::iter() {
                let selected = tool == tool_item;
                // DrawPerpWalls is the prerequisite step — always available.
                // Every other tool operates on an existing shape and is gated
                // on `has_floor_plan` so the assessor can't pick them until
                // the orthogonal outline is drawn.
                let enabled = matches!(tool_item, EditTools::DrawPerpWalls) || has_floor_plan;
                let label = egui::SelectableLabel::new(selected, format!("{:?}", tool_item));
                let response = ui.add_enabled(enabled, label);
                if response.clicked() {
                    change_tool = Some(tool_item);
                }
            }
        });
        change_tool
    }
}

/// The current storey's screen↔world transform for the 2D canvas, or `None`
/// when there's no BER / storey / fittable bbox. `watermark = true` fits the
/// previous-storey watermark in too (used while capturing a new upper storey's
/// outline, matching what's drawn); `false` fits just the storey's own footprint
/// (the usual pointer hit-testing). Single source for the canvas's transform so
/// every interaction inverts the same mapping the renderer used.
fn canvas_transform(
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    rect: egui::Rect,
    view: ViewParams,
    watermark: bool,
) -> Option<render::Transform2d> {
    let b = ber?;
    let i = current_storey_idx?;
    let (min_xy, max_xy) = if watermark {
        render::fit_bbox_for_storey(b, i)?
    } else {
        b.storeys.get(i).and_then(render::storey_2d::bounding_box)?
    };
    Some(render::fit_transform(rect, min_xy, max_xy, view))
}

/// World coords of a canvas-pixel position via the storey's own fit transform.
fn screen_to_world(
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    rect: egui::Rect,
    view: ViewParams,
    pos: egui::Pos2,
) -> Option<(f64, f64)> {
    canvas_transform(ber, current_storey_idx, rect, view, false).map(|t| t.inverse(pos.x, pos.y))
}

fn draw_stroke(painter: &egui::Painter, rect: egui::Rect, stroke: EguiStroke) {
    if stroke.points.is_empty() {
        return;
    }

    let points: Vec<egui::Pos2> = stroke
        .points
        .iter()
        .map(|p| egui::pos2(rect.left() + p.x, rect.top() + p.y))
        .collect();

    if points.len() >= 2 {
        painter.add(PathShape::line(points.clone(), stroke.stroke.clone()));
    }

    // Endpoint markers — visible snap targets for continuing the stroke on the
    // next touch (handled by Storey2DPerpWalls::start_stroke).
    let snap_color = egui::Color32::from_rgb(255, 200, 60);
    let first = *points.first().expect("non-empty checked above");
    painter.circle_filled(first, 6.0, snap_color);
    if points.len() >= 2 {
        let last = *points.last().expect("non-empty checked above");
        painter.circle_filled(last, 6.0, snap_color);
    }
}
