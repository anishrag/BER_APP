use egui::epaint::{PathShape, PathStroke};
use egui::Pos2;
use strum::IntoEnumIterator;

use crate::controller::app_controller::{
    DimensionPromptUiData, EditDragPromptUiData, VertexDropPromptUiData,
};
use crate::ui::pages::events::UiEvent;
use crate::ui::render;
use crate::ui::render::ViewParams;

/// Parse a dimension expression. Supports:
/// - A single positive number: `"4.2"`, `"0.5"`
/// - Sums and differences of numbers: `"3.2 + 1.5"`, `"5 - 1.8 + 0.3"`
/// - Optional leading `+`; a leading `-` is rejected since dimensions must
///   be positive.
///
/// Whitespace is ignored. Returns `None` for malformed input or non-positive
/// results.
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
    if total.is_finite() && total > 0.0 {
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
        assert!(parse_dimension_expression("3 - 3").is_none()); // zero result
        assert!(parse_dimension_expression("3 - 5").is_none()); // negative result
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
    /// Extend-mode stroke completed: the user's captured stroke,
    /// converted to *world* coords via the current fit transform so the
    /// controller can compare it against the polygon (which is in real
    /// units post-dim or in pixel-derived units pre-dim).
    ExtendStrokeWorld {
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
        current_storey_idx: usize,
        review: Option<&CollinearitySuggestion>,
        review_remaining: usize,
        has_floor_plan: bool,
        pending_redraw: bool,
        dim_prompt: Option<DimensionPromptUiData>,
        dim_input: &mut String,
        dim_scale_set: bool,
        dim_error: Option<&str>,
        dimensioned_walls: &[(usize, usize)],
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
        mut on_event: F,
    ) where
        F: FnMut(UiEvent),
    {
        let reviewing = review.is_some();
        let dim_prompting = dim_prompt.is_some();

        egui::CentralPanel::default().show_inside(ui, |ui| {
            ui.vertical(|ui| {
                // --- Top toolbar: SketchMode ---
                if let Some(new_mode) = Self::mode_toolbar(ui, mode) {
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

                // Primary-button input: stroke drawing OR Edit-mode
                // click/drag, depending on `edit_tool`. Both gated on
                // 2D mode and "no overlay is open" (review / pending
                // redraw / dim prompt take priority).
                let primary_available = matches!(mode, SketchMode::Storey2D)
                    && !reviewing
                    && !pending_redraw
                    && !dim_prompting;

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
                            let needs_world =
                                matches!(edit_tool, EditTools::Extend | EditTools::Trim);
                            let world_converted = if needs_world {
                                if let (Some(stroke), Some((min_xy, max_xy))) = (
                                    current.as_ref(),
                                    ber.and_then(|b| {
                                        b.storeys
                                            .get(current_storey_idx)
                                            .and_then(crate::ui::render::storey_2d::bounding_box)
                                    }),
                                ) {
                                    let t = crate::ui::render::fit_transform(
                                        rect, min_xy, max_xy, view,
                                    );
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
                                    let event = if matches!(edit_tool, EditTools::Trim) {
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
                    // Double-click first — egui fires this on the second
                    // press of a quick double-press. We insert the vertex
                    // and treat any subsequent drag movement as moving
                    // the just-inserted vertex.
                    if response.double_clicked_by(egui::PointerButton::Primary) {
                        if let (Some(b), Some((min_xy, max_xy))) = (
                            ber,
                            ber.and_then(|b| {
                                b.storeys
                                    .get(current_storey_idx)
                                    .and_then(crate::ui::render::storey_2d::bounding_box)
                            }),
                        ) {
                            let _ = b;
                            let t = crate::ui::render::fit_transform(rect, min_xy, max_xy, view);
                            let tol_world = 12.0 / t.scale();
                            if let Some(pos) = response.interact_pointer_pos() {
                                let (wx, wy) = t.inverse(pos.x, pos.y);
                                on_event(UiEvent::Sketch(SketchEvent::EditDoubleClick {
                                    world_x: wx,
                                    world_y: wy,
                                    tol: tol_world,
                                }));
                            }
                        }
                    }
                    if let (Some(b), Some((min_xy, max_xy))) = (
                        ber,
                        ber.and_then(|b| {
                            b.storeys
                                .get(current_storey_idx)
                                .and_then(crate::ui::render::storey_2d::bounding_box)
                        }),
                    ) {
                        let _ = b; // keep `ber` referenced; binding-only.
                        let t = crate::ui::render::fit_transform(rect, min_xy, max_xy, view);
                        // World-coord tolerance corresponds to ~12 pixels
                        // on screen at the current zoom.
                        let tol_world = 12.0 / t.scale();
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
                ui.horizontal(|ui| {
                    if pending_redraw {
                        Self::pending_redraw_toolbar(ui, &mut on_event);
                    } else if let Some(p) = dim_prompt {
                        Self::dimension_prompt_toolbar(
                            ui,
                            p,
                            dim_scale_set,
                            dim_input,
                            dim_error,
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
                    } else if let Some(new_tool) = Self::edit_toolbar(ui, edit_tool, has_floor_plan)
                    {
                        on_event(UiEvent::Sketch(SketchEvent::ChangeEditTool(new_tool)));
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

    fn mode_toolbar(ui: &mut egui::Ui, mode: SketchMode) -> Option<SketchMode> {
        let mut change_mode: Option<SketchMode> = None;
        ui.horizontal(|ui| {
            for mode_item in SketchMode::iter() {
                let selected = mode == mode_item;
                if ui
                    .selectable_label(selected, format!("{:?}", mode_item))
                    .clicked()
                {
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
