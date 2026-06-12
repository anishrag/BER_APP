//! The PROMPT DOCK: one bottom strip where every contextual flow panel of the
//! Sketch page renders, in both orientations — dimension/position prompts,
//! the post-drag and vertex-drop measurement prompts, the curve-radius
//! slider, the redraw decision, the collinearity review, the exposure
//! controls, and the phase / floor-step / ceiling-step flows. Visibility
//! logic is unchanged from the old bottom toolbar; only the location is
//! unified.

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::{
    DimensionPromptUiData, EditDragPromptUiData, ExposureSelection, NewStoreyPromptUiData,
    PositionPromptUiData, VertexDropPromptUiData,
};
use crate::ui::pages::events::UiEvent;
use datatypes::ber::common::ConstructionPhase;
use datatypes::ber::structural::WallExposure;

/// Whether the dock has anything to show this frame. When false the bottom
/// panel is skipped entirely so the canvas gets the space.
pub(super) fn has_content(view: &SketchView<'_>) -> bool {
    view.current_storey_idx.is_none()
        || view.new_storey_prompt.is_some()
        || view.pending_redraw
        || view.curve_bulge.is_some()
        || view.dim_prompt.is_some()
        || view.position_prompt.is_some()
        || view.edit_drag_prompt.is_some()
        || view.vertex_drop_prompt.is_some()
        || view.review.is_some()
        || view.exposure_mode
        || (view.has_floor_plan
            && ((!view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending))
                || view.step_mode
                || view.step_pick_pending
                || view.ceiling_step_mode
                || view.ceiling_step_pending
                || view.roof_point_mode
                || view.roof_point_pending
                || view.roof_ridge_pending))
}

/// Render the dock. Priority of the prompt chain: read-only notice >
/// new-storey flow > pending redraw > curve radius > dimension prompt >
/// position prompt > edit-drag prompt > vertex-drop prompt > collinearity
/// review > exposure controls. The new-storey flow sits at the TOP: it only
/// exists on a fresh upper storey, where none of the others can coexist
/// (notably not `pending_redraw` — the flow precedes any stroke). The
/// phase / step / ceiling-step flow toolbars render ALONGSIDE the chain
/// (as before): e.g. a position prompt can be up while a step amount is
/// still pending.
pub(super) fn render<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    view: &mut SketchView<'_>,
    on_event: &mut F,
) {
    ui.horizontal_wrapped(|ui| {
        if view.current_storey_idx.is_none() {
            ui.label(
                egui::RichText::new("Pick a storey tab to edit.")
                    .color(egui::Color32::from_rgb(180, 180, 180)),
            );
            return;
        }
        if let Some(p) = &view.new_storey_prompt {
            new_storey_toolbar(
                ui,
                p,
                view.new_storey_thickness_input,
                view.new_storey_error,
                on_event,
            );
        } else if view.pending_redraw {
            pending_redraw_toolbar(ui, on_event);
        } else if let Some(bulge) = view.curve_bulge {
            curve_radius_toolbar(ui, bulge, on_event);
        } else if let Some(p) = &view.dim_prompt {
            dimension_prompt_toolbar(
                ui,
                p,
                view.dim_scale_set,
                view.dim_input,
                view.dim_error,
                on_event,
            );
        } else if let Some(p) = &view.position_prompt {
            position_prompt_toolbar(ui, p, view.position_input, view.position_error, on_event);
        } else if let Some(p) = &view.edit_drag_prompt {
            edit_drag_prompt_toolbar(ui, p, view.edit_drag_input, view.edit_drag_error, on_event);
        } else if let Some(vp) = &view.vertex_drop_prompt {
            vertex_drop_prompt_toolbar(
                ui,
                vp,
                view.vertex_drop_inputs,
                view.vertex_drop_error,
                on_event,
            );
        } else if view.review.is_some() {
            review_toolbar(ui, view.review_remaining, on_event);
        } else if view.exposure_mode {
            exposure_toolbar(
                ui,
                view.exposure_walls,
                view.riser_views,
                view.exposure_selection.as_ref(),
                view.exposure_start_input,
                view.exposure_end_input,
                on_event,
            );
        }

        if view.has_floor_plan {
            // Construction-phase picker / region list. Only useful once
            // construction years exist (picker non-empty). Also shown when a
            // pick is PENDING with the mode off (a restored mid-flight area
            // after reopen): the pick blocks drawing, so its prompt must stay
            // reachable.
            if !view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending) {
                phase_toolbar(
                    ui,
                    view.phase_choices,
                    view.phase_region_polygons,
                    view.phase_pick_pending,
                    on_event,
                );
            }
            // Same pending-with-mode-off rule as the phase toolbar.
            if view.step_mode || view.step_pick_pending {
                step_toolbar(
                    ui,
                    view.step_region_polygons,
                    view.step_pick_pending,
                    view.step_amount_input,
                    view.step_error,
                    on_event,
                );
            }
            // Same pending-with-mode-off rule as the other tools.
            if view.ceiling_step_mode || view.ceiling_step_pending {
                ceiling_step_toolbar(
                    ui,
                    view.ceiling_step_region_polygons,
                    view.ceiling_step_orientation_pending,
                    view.ceiling_step_side_pending,
                    view.ceiling_step_amount_pending,
                    view.ceiling_amount_input,
                    view.ceiling_step_error,
                    on_event,
                );
            }
            // Same pending-with-mode-off rule again. A pending RIDGE (the
            // stroke flow sharing roof-point mode) keeps it reachable too.
            if view.roof_point_mode || view.roof_point_pending || view.roof_ridge_pending {
                roof_point_toolbar(
                    ui,
                    view.roof_apex_views,
                    view.roof_point_height_pending,
                    view.roof_point_position_pending,
                    view.roof_ridge_height_pending,
                    view.roof_ridge_length_pending,
                    view.roof_ridge_length_anchored,
                    view.roof_ridge_position_pending,
                    view.roof_apex_input,
                    view.roof_ridge_length_input,
                    view.roof_error,
                    on_event,
                );
            }
        }
    });
}

/// Surface a rejected entry inline, next to the input that produced it, so
/// the user sees WHY nothing happened and can correct without losing what
/// they typed. Shared by every prompt panel that validates input.
fn inline_error(ui: &mut egui::Ui, error: Option<&str>) {
    if let Some(err) = error {
        ui.label(
            egui::RichText::new(format!("⚠ {}", err)).color(egui::Color32::from_rgb(255, 120, 90)),
        );
    }
}

/// Parse a dimension expression. Supports:
/// - A single non-negative number: `"4.2"`, `"0.5"`, `"0"`
/// - Sums and differences of numbers: `"3.2 + 1.5"`, `"5 - 1.8 + 0.3"`
/// - Optional leading `+`; a leading `-` is rejected since dimensions can't
///   be negative.
///
/// Zero is accepted — it collapses the wall (used to zero a trim remnant so a
/// cut snaps onto a corner). Whitespace is ignored. Returns `None` for
/// malformed input or a negative result.
///
/// `pub(super)` so the Heights category's ceiling-height field shares the
/// same entry surface as the dock's prompts.
pub(super) fn parse_dimension_expression(input: &str) -> Option<f32> {
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

fn dimension_prompt_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    prompt: &DimensionPromptUiData,
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
    inline_error(ui, dim_error);
}

/// Toolbar for the position prompt of a carved phase/step area: the
/// distance (along the prompt's axis) from the highlighted outline
/// reference wall to the highlighted area wall. The textbox arrives
/// PREFILLED with the drawn distance (see `BerApp`'s prompt-identity
/// refill); Apply pins it, Skip accepts the drawn position, and the
/// cycle button moves to another reference wall.
fn position_prompt_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    prompt: &PositionPromptUiData,
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
    // The roof location stages offer a computed CENTRE suggestion, applied
    // through the same path as a typed distance.
    if prompt.offers_centre
        && ui
            .button("Centre")
            .on_hover_text("Centre it within its ceiling area")
            .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::CentreRoofPosition));
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
    inline_error(ui, error);
}

fn edit_drag_prompt_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    prompt: &EditDragPromptUiData,
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
    inline_error(ui, error);
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
    inline_error(ui, error);
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

/// The NEW-STOREY flow toolbar, stage by stage:
///
/// - **Thickness**: the slab-thickness question asked FIRST on a fresh upper
///   storey — input (arrives prefilled "0.30") + Set + "Use 0.30" (= submit
///   the default), plus the hint that drawing stays paused until it lands
///   (the controller ignores outline strokes during this stage).
/// - **ChooseMethod**: "Draw outline" vs "Suggest outlines" (the latter
///   gated on `can_suggest` — the storey below needs its ceiling height and
///   a flat ceiling area). A canvas stroke is an implicit "Draw outline".
/// - **Browse**: the current suggestion's label, ◀ ▶ cycling with a "2/5"
///   count, Accept (adopts it fully finalised — no review, no prompts), and
///   "Draw instead". The canvas tints the suggestion's ring meanwhile.
fn new_storey_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    prompt: &NewStoreyPromptUiData,
    thickness_input: &mut String,
    error: Option<&str>,
    on_event: &mut F,
) {
    if prompt.thickness_pending {
        ui.label(
            egui::RichText::new("Floor thickness to the storey below (m):")
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        let resp = ui.add(
            egui::TextEdit::singleline(thickness_input)
                .desired_width(60.0)
                .hint_text("e.g. 0.30"),
        );
        let submitted = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let set_clicked = ui
            .button("Set")
            .on_hover_text("Use this slab thickness (it sets the storey's floor level)")
            .clicked();
        if set_clicked || submitted {
            if let Some(v) = parse_dimension_expression(thickness_input) {
                on_event(UiEvent::Sketch(SketchEvent::AssignFloorThickness(v as f64)));
            }
        }
        if ui
            .button("Use 0.30")
            .on_hover_text("Accept the typical 0.30 m slab")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::AssignFloorThickness(0.30)));
        }
        ui.label(
            egui::RichText::new("Drawing is paused until the thickness is set.")
                .color(egui::Color32::from_rgb(180, 180, 180)),
        );
    } else if prompt.method_pending {
        ui.label(
            egui::RichText::new("How should this storey's outline start?")
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        if ui
            .button("Draw outline")
            .on_hover_text(
                "Draw it over the faint previous storey (just drawing a stroke works too)",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ChooseDrawOutline));
        }
        if ui
            .add_enabled(prompt.can_suggest, egui::Button::new("Suggest outlines"))
            .on_hover_text("Browse outlines derived from the storey below's roof shape")
            .on_disabled_hover_text(
                "Needs the storey below to have its ceiling height set and a flat ceiling area",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ChooseSuggestOutlines));
        }
    } else if let Some((_ring, label, idx, count)) = &prompt.suggestion {
        ui.label(
            egui::RichText::new("Suggested outline:")
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        ui.label(label);
        if ui
            .button("◀")
            .on_hover_text("Previous suggestion")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::PrevSuggestion));
        }
        ui.label(format!("{}/{}", idx + 1, count));
        if ui.button("▶").on_hover_text("Next suggestion").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::NextSuggestion));
        }
        if ui
            .button("Accept")
            .on_hover_text(
                "Use this outline as the storey's footprint — finalised, \
                 no dimension questions",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::AcceptSuggestion));
        }
        if ui
            .button("Draw instead")
            .on_hover_text("Abandon the suggestions and draw the outline yourself")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ChooseDrawOutline));
        }
    }
    inline_error(ui, error);
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
    error: Option<&str>,
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
        inline_error(ui, error);
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
fn ceiling_step_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    regions: &[(Vec<(f64, f64)>, String)],
    orientation_pending: bool,
    side_pending: bool,
    amount_pending: bool,
    amount_input: &mut String,
    error: Option<&str>,
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
        inline_error(ui, error);
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

/// Roof-point (and roof-RIDGE) controls, stage by stage: the height field +
/// Set/Cancel once the point's/ridge's position is resolved; the ridge's
/// LENGTH field (its own buffer, prefilled with the drawn length) + Set/Skip
/// between its position questions and its height; a "answer the distance
/// question" hint while a position stage is up (it shows as the shared
/// position-prompt panel); otherwise a tap/stroke hint plus the
/// committed-apex list with deletes. The ridge stages mirror the apex ones
/// and share the height input buffer (the two flows are never pending at
/// once — same mode, mutually suppressing).
#[allow(clippy::too_many_arguments)] // one flag/buffer per dock stage
fn roof_point_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    apexes: &[((f64, f64), String)],
    height_pending: bool,
    position_pending: bool,
    ridge_height_pending: bool,
    ridge_length_pending: bool,
    ridge_length_anchored: bool,
    ridge_position_pending: bool,
    height_input: &mut String,
    ridge_length_input: &mut String,
    error: Option<&str>,
    on_event: &mut F,
) {
    if ridge_height_pending {
        ui.label(egui::RichText::new("Ridge height (m):").strong());
        ui.add(
            egui::TextEdit::singleline(height_input)
                .desired_width(60.0)
                .hint_text("e.g. 4.2"),
        );
        // The height is ABSOLUTE (above the storey floor datum) and applies
        // to BOTH ridge ends; the controller validates it.
        if ui
            .button("Set")
            .on_hover_text("Raise (or lower) the ceiling area to a ridge at this height")
            .clicked()
        {
            if let Some(v) = parse_dimension_expression(height_input) {
                on_event(UiEvent::Sketch(SketchEvent::AssignRoofRidgeHeight(v)));
                height_input.clear();
            }
        }
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::CancelPendingRoofRidge));
            height_input.clear();
        }
        inline_error(ui, error);
    } else if ridge_length_pending {
        ui.label(egui::RichText::new("Ridge length (m):").strong());
        // Arrives PREFILLED with the drawn length (the stroke is approximate
        // — the typed value is the real dimension; Skip accepts the drawn).
        ui.add(
            egui::TextEdit::singleline(ridge_length_input)
                .desired_width(60.0)
                .hint_text("e.g. 6.0"),
        );
        // "(from the highlighted end)" only when an end is ANCHORED — an
        // extent end snapped onto a wall, so the typed length extends from
        // (or, for a snapped far end, back from) that exact coordinate.
        let set_hover = if ridge_length_anchored {
            "Use exactly this ridge length (from the highlighted end)"
        } else {
            "Use exactly this ridge length"
        };
        if ui.button("Set").on_hover_text(set_hover).clicked() {
            if let Some(v) = parse_dimension_expression(ridge_length_input) {
                on_event(UiEvent::Sketch(SketchEvent::AssignRoofRidgeLength(v)));
            }
        }
        if ui
            .button("Skip")
            .on_hover_text("Accept the drawn length as-is")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::SkipRoofRidgeLength));
        }
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::CancelPendingRoofRidge));
            ridge_length_input.clear();
        }
        inline_error(ui, error);
    } else if ridge_position_pending {
        // The ridge's cross-coordinate question — the shared position panel
        // above owns the entry; this hint keeps Cancel reachable alongside it.
        ui.label(
            egui::RichText::new("Pin the ridge's position (the distance question above)")
                .color(egui::Color32::from_rgb(255, 200, 60)),
        );
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::CancelPendingRoofRidge));
        }
    } else if height_pending {
        ui.label(egui::RichText::new("Apex height (m):").strong());
        ui.add(
            egui::TextEdit::singleline(height_input)
                .desired_width(60.0)
                .hint_text("e.g. 4.2"),
        );
        // Unparseable input is ignored — the button just does nothing. The
        // height is ABSOLUTE (above the storey floor datum), so the value
        // goes through as-is; the controller validates it.
        if ui
            .button("Set")
            .on_hover_text("Slope the ceiling area up (or down) to this height at the point")
            .clicked()
        {
            if let Some(v) = parse_dimension_expression(height_input) {
                on_event(UiEvent::Sketch(SketchEvent::AssignRoofApexHeight(v)));
                height_input.clear();
            }
        }
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::CancelPendingRoofPoint));
            height_input.clear();
        }
        inline_error(ui, error);
    } else if position_pending {
        // A position stage — the shared position panel above owns the entry;
        // this hint keeps Cancel reachable alongside it.
        ui.label(
            egui::RichText::new("Pin the point's position (the distance question above)")
                .color(egui::Color32::from_rgb(255, 200, 60)),
        );
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::CancelPendingRoofPoint));
        }
    } else {
        ui.label(
            "Tap a high point inside a ceiling area to slope the roof, \
             or draw a straight stroke along a ridge line.",
        );
        for (i, (_pos, label)) in apexes.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(label);
                if ui
                    .button("🗑")
                    .on_hover_text("Delete this roof point (the area re-flattens)")
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::DeleteRoofApex(i)));
                }
            });
        }
    }
}

/// Wall-exposure mode toolbar. With a wall selected: a section selection
/// `start–end` (live with the drag handles) plus typed start/end inputs, the
/// three type buttons that paint the current selection, and "Clear wall".
/// With a RISER selected (a dashed line tapped): a kind label, the same type
/// buttons plus a riser-only "Ground" button (they route to the riser), and
/// "Clear" back to the derived default (`Unset`) — NO range controls, risers
/// are whole-face only (v1).
/// Always: "Set remaining → Exposed" bulk-marks every still-unset coverage
/// range — whole unmarked walls and the unset gaps around painted sections.
fn exposure_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    walls: &[crate::controller::app_controller::WallExposureView],
    risers: &[crate::controller::app_controller::RiserView],
    selection: Option<&ExposureSelection>,
    start_input: &mut String,
    end_input: &mut String,
    on_event: &mut F,
) {
    use crate::controller::app_controller::RiserKind;
    let selected_riser = risers.iter().find(|r| r.selected);
    // Count walls with ANY unset coverage (no sections at all, or unset gaps
    // around painted sections) — the same ranges SetAllRemainingExposed paints.
    let unset = walls
        .iter()
        .filter(|w| {
            document::wall_coverage(w.length_m, &w.sections)
                .iter()
                .any(|(_, _, e)| *e == WallExposure::Unset)
        })
        .count();
    ui.horizontal(|ui| {
        if let Some(r) = selected_riser {
            // Riser context: whole-face only (v1) — no start/end range
            // controls; the type buttons route to the riser in the
            // controller (`set_wall_exposure` → `set_riser_exposure`).
            let label = match r.kind {
                RiserKind::Floor => "Riser: floor step",
                RiserKind::Ceiling => "Riser: ceiling step",
            };
            ui.label(egui::RichText::new(label).strong());
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
            // Ground is riser-only (outline walls keep their three buttons):
            // DEAP's ground-contact category applies to below-grade faces,
            // which today only risers (floor/ceiling step faces) can be.
            if ui
                .button("Ground")
                .on_hover_text("In contact with the ground (below grade)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetWallExposure(
                    WallExposure::Ground,
                )));
            }
            if ui
                .button("Clear")
                .on_hover_text("Back to the derived default (unset)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ClearWallExposure));
            }
        } else if let Some(sel) = selection {
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
            if ui
                .button("Clear wall")
                .on_hover_text("Remove this wall's marks — back to unset")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ClearWallExposure));
            }
        } else {
            let hint = if risers.is_empty() {
                "Tap a wall to mark its exposure"
            } else {
                "Tap a wall — or a dashed riser line — to mark its exposure"
            };
            ui.label(egui::RichText::new(hint).color(egui::Color32::from_rgb(255, 200, 60)));
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
