use egui::epaint::{PathShape, PathStroke};
use egui::Pos2;
use strum::IntoEnumIterator;

use crate::controller::app_controller::DimensionPromptUiData;
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

#[derive(Debug, Clone, Copy, PartialEq)]
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
                    view,
                );

                // Dimension-prompt overlay: a double-headed arrow on the wall
                // we're currently asking the assessor about.
                if let Some(p) = dim_prompt {
                    render::render_dimension_prompt(
                        &painter,
                        rect,
                        ber,
                        current_storey_idx,
                        mode,
                        view,
                        p.wall_a,
                        p.wall_b,
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

                // Stroke input: only in 2D mode, and only when we're not in
                // review or awaiting a redraw decision. (Pan/zoom still work.)
                // Use the *_by(Primary) variants so middle/right drags (which pan)
                // don't accidentally start a stroke.
                if matches!(mode, SketchMode::Storey2D)
                    && !reviewing
                    && !pending_redraw
                    && !dim_prompting
                {
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
                            on_event(UiEvent::Sketch(SketchEvent::EndStroke(local_pos)));
                        }
                    }

                    if let Some(stroke) = current {
                        draw_stroke(&painter, rect, stroke);
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
                });
            });
        });
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
