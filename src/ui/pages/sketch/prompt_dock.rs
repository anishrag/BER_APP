//! The PROMPT DOCK: one bottom strip where every contextual flow panel of the
//! Sketch page renders, in both orientations — dimension/position prompts,
//! the post-drag and vertex-drop measurement prompts, the curve-radius
//! slider, the redraw decision, the collinearity review, the exposure
//! controls, and the phase / floor-step / ceiling-step flows. Visibility
//! logic is unchanged from the old bottom toolbar; only the location is
//! unified.

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::{
    ExposureSelection, NewStoreyPromptUiData, PartitionView, SelectMode, VertexDropPromptUiData,
};
use crate::number_entry::parse_dimension_expression;
use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::{metre_input, HintExt};
use ber_model::ber::common::ConstructionPhase;
use ber_model::ber::structural::WallExposure;
use ber_model::ber::structural::{CeilingExposure, FloorExposure};

/// Whether the floating flow panel has interactive controls to show this frame.
/// When false the panel is skipped so it doesn't draw an empty window. Pure
/// instruction-only states (no storey, openings placement) are NOT listed — they
/// surface as the bottom instruction bar / top-right actions, not the panel.
pub(super) fn has_controls(view: &SketchView<'_>) -> bool {
    // Single-value number prompts (dimension/position/edit-drag/thickness/step
    // & ceiling amounts/roof heights & lengths) are NOT listed here — they
    // render in the floating number-entry box. Openings placement is NOT here
    // either (its measurements use the number box; its edit actions are the
    // top-right floating buttons).
    view.new_storey_prompt
        .as_ref()
        .is_some_and(|p| !p.thickness_pending)
        || view.pending_redraw
        || view.curve_bulge.is_some()
        || view.vertex_drop_prompt.is_some()
        || view.review.is_some()
        || view.exposure_mode
        || view.floor_exposure_mode
        || view.ceiling_exposure_mode
        || view.partition_mode
        || (view.has_floor_plan
            && ((!view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending))
                || view.step_mode
                || view.ceiling_step_mode
                || view.ceiling_step_orientation_pending
                || view.ceiling_step_side_pending
                || view.roof_point_mode
                || view.roof_point_position_pending
                || view.roof_ridge_position_pending
                || view.roof_rect_position_pending
                || !view.roof_ridge_area_options.is_empty()))
}

/// A short title for the floating flow panel's title bar (also its drag handle).
/// Matches whichever control set [`render_controls`] shows; approximate when
/// several are active at once (picks the primary).
pub(super) fn panel_title(view: &SketchView<'_>) -> &'static str {
    if view.floor_exposure_mode {
        return "Floor exposures";
    }
    if view.ceiling_exposure_mode {
        return "Ceiling exposures";
    }
    if view
        .new_storey_prompt
        .as_ref()
        .is_some_and(|p| !p.thickness_pending)
    {
        return "New storey";
    }
    if view.pending_redraw {
        return "New stroke";
    }
    if view.curve_bulge.is_some() {
        return "Curve";
    }
    if view.vertex_drop_prompt.is_some() {
        return "Place vertex";
    }
    if view.review.is_some() {
        return "Review";
    }
    if view.partition_mode {
        return "Wall partition";
    }
    if view.exposure_mode {
        return "Wall exposures";
    }
    if view.has_floor_plan {
        if !view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending) {
            return "Construction year";
        }
        if view.step_mode {
            return "Floor steps";
        }
        if view.ceiling_step_mode
            || view.ceiling_step_orientation_pending
            || view.ceiling_step_side_pending
        {
            return "Ceiling steps";
        }
        if !view.roof_ridge_area_options.is_empty() {
            return "Ridge";
        }
    }
    "Roof shape"
}

/// The contextual one-line INSTRUCTION for the bottom bar — what the assessor
/// should do in the current mode/flow. The controls themselves live in the
/// floating flow panel (and the top-right action buttons); this strip is purely
/// the "what to do" hint. `None` hides the bar.
pub(super) fn instruction(view: &SketchView<'_>) -> Option<&'static str> {
    if view.current_storey_idx.is_none() {
        return Some("Pick a storey tab to edit.");
    }
    if view.floor_exposure_mode {
        return Some("Floor exposures — select floors, then pick their heat-loss exposure.");
    }
    if view.ceiling_exposure_mode {
        return Some("Ceiling exposures — select ceilings, then pick their heat-loss exposure.");
    }
    if view
        .new_storey_prompt
        .as_ref()
        .is_some_and(|p| !p.thickness_pending)
    {
        return Some("New storey — choose how its outline starts, or draw it.");
    }
    if view.pending_redraw {
        return Some("A storey already exists — choose what this stroke should do.");
    }
    if view.curve_bulge.is_some() {
        return Some("Drag the slider to set the curve radius, then accept it.");
    }
    if view.vertex_drop_prompt.is_some() {
        return Some("Place the vertex — enter any two distances.");
    }
    if view.review.is_some() {
        return Some("Review the suggested straightening.");
    }
    if view.partition_mode {
        return Some("Split the wall face — draw a cut line across it.");
    }
    if view.exposure_mode {
        return Some("Wall exposures — select a wall, then pick its heat-loss exposure.");
    }
    if view.openings_mode {
        return Some(if view.elevation_view.is_some() {
            "Tap a window to select; drag to move. Use the buttons (top-right) to delete or finish."
        } else if view.roof_window_selected.is_some() {
            "Roof window selected — drag in the plan to move it, or delete it (top-right)."
        } else {
            "Windows & doors — pick a tool, tap the plan, then enter the size in the box."
        });
    }
    if view.has_floor_plan {
        if !view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending) {
            return Some("Construction year — draw an area, then tag its build period.");
        }
        if view.step_mode {
            return Some("Floor steps — draw the step line across the area.");
        }
        if view.ceiling_step_mode
            || view.ceiling_step_orientation_pending
            || view.ceiling_step_side_pending
        {
            return Some("Ceiling steps — draw the step line, then set the level change.");
        }
        if !view.roof_ridge_area_options.is_empty() {
            return Some("Choose which area this ridge should slope.");
        }
        if view.roof_point_mode
            || view.roof_point_position_pending
            || view.roof_ridge_position_pending
            || view.roof_rect_position_pending
        {
            return Some("Roof shape — tap an apex, or draw a ridge / flat-top, then set heights.");
        }
    }
    None
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
pub(super) fn render_controls<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    view: &mut SketchView<'_>,
    on_event: &mut F,
) {
    // The floor-exposure tool spans the whole storey stack — it works on the
    // All tab too, so its toolbar renders BEFORE the "pick a storey" notice and
    // owns the dock while active.
    if view.floor_exposure_mode {
        ui.horizontal_wrapped(|ui| {
            if view.floor_exposure_gate_pending {
                floor_exposure_gate_toolbar(ui, on_event);
            } else {
                floor_exposure_toolbar(ui, view.floor_exposure_select, on_event);
            }
        });
        return;
    }
    // The ceiling-exposure tool likewise spans the whole stack and owns the dock.
    if view.ceiling_exposure_mode {
        ui.horizontal_wrapped(|ui| {
            if view.ceiling_exposure_gate_pending {
                ceiling_exposure_gate_toolbar(ui, on_event);
            } else {
                ceiling_exposure_toolbar(ui, view.ceiling_exposure_select, on_event);
            }
        });
        return;
    }
    ui.horizontal_wrapped(|ui| {
        if let Some(p) = &view.new_storey_prompt {
            new_storey_toolbar(ui, p, view.new_storey_error, on_event);
        } else if view.pending_redraw {
            pending_redraw_toolbar(ui, on_event);
        } else if let Some(bulge) = view.curve_bulge {
            curve_radius_toolbar(ui, bulge, on_event);
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
        } else if view.partition_mode {
            if let Some(pv) = view.partition_view {
                partition_toolbar(ui, pv, on_event);
            }
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
            // The step AMOUNT (Raise/Lower) is entered in the floating box; the
            // dock keeps only the region list (and the draw hint when idle).
            if view.step_mode {
                step_toolbar(
                    ui,
                    view.step_region_polygons,
                    view.step_pick_pending,
                    on_event,
                );
            }
            // The ceiling-step AMOUNT (Up/Down) is entered in the floating box;
            // the dock keeps the orientation/side stages, the region list, and
            // the draw hint (suppressed while the amount box is active).
            if view.ceiling_step_mode
                || view.ceiling_step_orientation_pending
                || view.ceiling_step_side_pending
            {
                ceiling_step_toolbar(
                    ui,
                    view.ceiling_step_region_polygons,
                    view.ceiling_step_orientation_pending,
                    view.ceiling_step_side_pending,
                    view.ceiling_step_amount_pending,
                    on_event,
                );
            }
            // Same pending-with-mode-off rule again. A pending RIDGE or
            // RECT (the stroke flows sharing roof-point mode) keeps it
            // reachable too.
            if !view.roof_ridge_area_options.is_empty() {
                // The ridge sits on an internal boundary — ask which area slopes.
                ui.label(
                    egui::RichText::new(
                        "Which area should this ridge slope? (the other gets a step)",
                    )
                    .strong(),
                );
                for (i, label) in view.roof_ridge_area_options.iter().enumerate() {
                    if ui.button(label).clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::ChooseRoofRidgeArea(i)));
                    }
                }
                if ui.button("Cancel").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::CancelPendingRoofRidge));
                }
            } else if view.roof_point_mode
                || view.roof_point_position_pending
                || view.roof_ridge_position_pending
                || view.roof_rect_position_pending
            {
                // Heights and lengths are entered in the floating box; the dock
                // keeps the position-stage Cancels, the apex list, and the
                // tap/stroke hint (suppressed while a box stage is active).
                let number_pending = view.roof_point_height_pending
                    || view.roof_ridge_height_pending
                    || view.roof_ridge_length_pending
                    || view.roof_rect_height_pending
                    || view.roof_rect_length_axis.is_some();
                roof_point_toolbar(
                    ui,
                    view.roof_apex_views,
                    view.roof_point_position_pending,
                    view.roof_ridge_position_pending,
                    view.roof_rect_position_pending,
                    number_pending,
                    on_event,
                );
            }
        }
    });
}

/// Top-right floating ACTION buttons for the active selection — the openings
/// edit-on-surface controls (Delete, with a confirm step, and the elevation
/// Done). These are dismiss/confirm actions, so they sit as floating buttons in
/// the top-right corner of the canvas rather than in the bottom bar. Renders
/// nothing when there's no actionable selection.
pub(super) fn render_corner_actions<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    view: &SketchView<'_>,
    on_event: &mut F,
) {
    if !view.openings_mode {
        return;
    }
    let has_elevation = view.elevation_view.is_some();
    let has_selection = has_elevation || view.roof_window_selected.is_some();
    if !has_selection {
        return;
    }
    let delete_confirm = view.delete_confirm_pending;
    egui::Area::new(egui::Id::new("sketch_corner_actions"))
        .order(egui::Order::Foreground)
        // Down the right edge, below the floating canvas view buttons.
        .fixed_pos(rect.right_top() + egui::vec2(-8.0, 64.0))
        .pivot(egui::Align2::RIGHT_TOP)
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.vertical(|ui| {
                    if delete_confirm {
                        ui.label("Delete this window?");
                        ui.horizontal(|ui| {
                            if ui.button("🗑 Delete").clicked() {
                                on_event(UiEvent::Sketch(SketchEvent::ConfirmDeleteOpening));
                            }
                            if ui.button("Cancel").clicked() {
                                on_event(UiEvent::Sketch(SketchEvent::CancelDeleteOpening));
                            }
                        });
                    } else if ui.button("🗑 Delete selected").clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::DeleteSelectedOpening));
                    }
                    if has_elevation && ui.button("Done").clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::CloseElevation));
                    }
                });
            });
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

fn vertex_drop_prompt_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    prompt: &VertexDropPromptUiData,
    inputs: &mut [String; 4],
    error: Option<&str>,
    on_event: &mut F,
) {
    use crate::controller::app_controller::VertexMeasurementKind;
    use manip_plan::ParameterAxis;
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
        let submitted = metre_input(ui, &mut inputs[slot], 60.0, "=?").submitted;
        let clicked = ui.button("✓").hint("Set this distance").clicked();
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
        .hint("Revert the new vertex to its initial position")
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
        .hint("Drag to flatten (toward 0) or deepen the curve; sign flips the side")
        .changed()
    {
        on_event(UiEvent::Sketch(SketchEvent::SetCurveBulge(b)));
    }
    if ui
        .button("Accept curve")
        .hint("Bake the curve into the plan at this radius")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::CommitCurve));
    }
}

/// The NEW-STOREY flow toolbar, stage by stage:
///
/// - **Thickness**: the slab-thickness question asked FIRST on a fresh upper
///   storey is now entered in the floating number box, not here.
/// - **ChooseMethod**: "Draw outline" vs "Suggest outlines" (the latter
///   gated on `can_suggest` — the storey below needs its ceiling height and
///   a flat ceiling area). A canvas stroke is an implicit "Draw outline".
/// - **Browse**: the current suggestion's label, ◀ ▶ cycling with a "2/5"
///   count, Accept (adopts it fully finalised — no review, no prompts), and
///   "Draw instead". The canvas tints the suggestion's ring meanwhile.
fn new_storey_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    prompt: &NewStoreyPromptUiData,
    error: Option<&str>,
    on_event: &mut F,
) {
    if prompt.method_pending {
        ui.label(
            egui::RichText::new("How should this storey's outline start?")
                .strong()
                .color(egui::Color32::from_rgb(60, 220, 255)),
        );
        if ui
            .button("Draw outline")
            .hint("Draw it over the faint previous storey (just drawing a stroke works too)")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ChooseDrawOutline));
        }
        if ui
            .add_enabled(prompt.can_suggest, egui::Button::new("Suggest outlines"))
            .hint("Browse outlines derived from the storey below's roof shape")
            .disabled_hint(
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
        if ui.button("◀").hint("Previous suggestion").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::PrevSuggestion));
        }
        ui.label(format!("{}/{}", idx + 1, count));
        if ui.button("▶").hint("Next suggestion").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::NextSuggestion));
        }
        if ui
            .button("Accept")
            .hint(
                "Use this outline as the storey's footprint — finalised, \
                 no dimension questions",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::AcceptSuggestion));
        }
        if ui
            .button("Draw instead")
            .hint("Abandon the suggestions and draw the outline yourself")
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
        .hint("Discard the current outline and replace it with this stroke")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::ReplaceStorey));
    }
    if ui
        .button("New storey from this")
        .hint("Save current storey, start a new one from this stroke (coming soon)")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::AddStoreyFromCurrent));
    }
    if ui
        .button("Ignore")
        .hint("Discard this stroke, keep the current storey")
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
                if ui.button("🗑").hint("Delete this phase area").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::DeletePhaseRegion(i)));
                }
            });
        }
        // One-tap "everything else is the original build": stamps the leftover
        // fabric Original so the Construction-phase checklist row goes green
        // without drawing an area for the bulk of the storey.
        if ui
            .button("Set rest to Original")
            .hint(
                "Tag all untagged fabric (the outline minus the areas above) as the original build",
            )
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::AssignRestOriginal));
        }
    }
}

/// Floor-step region list + draw hint. The step AMOUNT (the Raise/Lower entry
/// for a pending area) is typed in the floating number box; the dock keeps the
/// committed-region list with deletes, plus the "draw an area" hint while no
/// pick is in flight.
fn step_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    regions: &[(Vec<(f64, f64)>, String)],
    pick_pending: bool,
    on_event: &mut F,
) {
    if !pick_pending {
        ui.label("Draw an area to raise or lower its floor.");
    }
    for (i, (_ring, label)) in regions.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(label);
            if ui.button("🗑").hint("Delete this step area").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::DeleteStepRegion(i)));
            }
        });
    }
}

/// Ceiling-step controls, stage by stage: the H/V picker for a tapped point;
/// the "tap the side" hint while the side pick is pending; otherwise the
/// committed-region list with deletes. The AMOUNT (Up/Down) once the line is
/// resolved is typed in the floating number box, and the position stage shows
/// in that box too — so the "draw a line" hint is suppressed while an amount
/// entry is open.
fn ceiling_step_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    regions: &[(Vec<(f64, f64)>, String)],
    orientation_pending: bool,
    side_pending: bool,
    amount_pending: bool,
    on_event: &mut F,
) {
    if orientation_pending {
        ui.label(egui::RichText::new("Line direction:").strong());
        if ui
            .button("Horizontal")
            .hint("A horizontal step line through the tapped point")
            .clicked()
        {
            on_event(UiEvent::Sketch(SketchEvent::ChooseCeilingStepOrientation {
                horizontal: true,
            }));
        }
        if ui
            .button("Vertical")
            .hint("A vertical step line through the tapped point")
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
    } else {
        if !amount_pending {
            ui.label(
                "Draw a straight line across the plan — or a closed area — to step the ceiling.",
            );
        }
        for (ring, label) in regions.iter() {
            ui.horizontal(|ui| {
                ui.label(label);
                if ui
                    .button("🗑")
                    .hint("Delete this ceiling step (re-flattens to the default)")
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::DeleteCeilingStepRegion(
                        ring.clone(),
                    )));
                }
            });
        }
    }
}

/// Roof-point (and roof-RIDGE / roof-RECT) NON-numeric controls. The heights
/// and lengths are entered in the floating number box; the dock keeps the
/// position-stage Cancels (the distance itself is typed in the box), the
/// committed-apex list with deletes, and the tap/stroke hint — suppressed
/// while a number stage is active (`number_pending`) so the box has the floor.
fn roof_point_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    apexes: &[((f64, f64), String)],
    point_position_pending: bool,
    ridge_position_pending: bool,
    rect_position_pending: bool,
    number_pending: bool,
    on_event: &mut F,
) {
    // The three position stages each keep their own Cancel reachable while the
    // distance is typed in the box.
    let position_cancel = |ui: &mut egui::Ui, text: &str, ev: SketchEvent, on_event: &mut F| {
        ui.label(egui::RichText::new(text).color(egui::Color32::from_rgb(255, 200, 60)));
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(ev));
        }
    };
    if rect_position_pending {
        position_cancel(
            ui,
            "Pin the flat top's position (the distance question is in the box)",
            SketchEvent::CancelPendingRoofRect,
            on_event,
        );
    } else if ridge_position_pending {
        position_cancel(
            ui,
            "Pin the ridge's position (the distance question is in the box)",
            SketchEvent::CancelPendingRoofRidge,
            on_event,
        );
    } else if point_position_pending {
        position_cancel(
            ui,
            "Pin the point's position (the distance question is in the box)",
            SketchEvent::CancelPendingRoofPoint,
            on_event,
        );
    } else {
        if !number_pending {
            ui.label(
                "Tap a high point inside a ceiling area to slope the roof, draw a \
                 straight stroke along a ridge line, or a closed rectangle for a \
                 flat-topped roof.",
            );
        }
        for (i, (_pos, label)) in apexes.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.label(label);
                if ui
                    .button("🗑")
                    .hint("Delete this roof point (the area re-flattens)")
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
            // Face-partitioned walls are classified in the partition view, not by
            // this bulk button — exclude them (their empty `sections` would
            // otherwise read as a full Unset wall and over-count).
            w.partition_exposures.is_empty()
                && document::wall_coverage(w.length_m, &w.sections)
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
                .hint("In contact with the ground (below grade)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetWallExposure(
                    WallExposure::Ground,
                )));
            }
            if ui
                .button("Clear")
                .hint("Back to the derived default (unset)")
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
            let rs = metre_input(ui, start_input, 46.0, &format!("{lo:.2}"));
            ui.label("end");
            let re = metre_input(ui, end_input, 46.0, &format!("{hi:.2}"));
            let enter = rs.submitted || re.submitted;
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
                .hint("Remove this wall's marks — back to unset")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ClearWallExposure));
            }
            if ui
                .button("⊟ Partition & set")
                .hint("Divide this wall's face and set each part")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::OpenWallPartition));
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

/// The floor-exposure storey-count GATE: confirm every storey is added before
/// the auto-exposure pass runs and the overlaid floor view appears.
fn floor_exposure_gate_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
    ui.label(
        egui::RichText::new("Have you finished adding all storeys?")
            .strong()
            .color(egui::Color32::from_rgb(60, 220, 255)),
    );
    if ui
        .button("Yes")
        .hint("Run the inter-storey party auto-exposure pass, then mark floors")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::ConfirmAllStoreysAdded));
    }
    if ui
        .button("Not yet")
        .hint("Exit the tool so you can add the remaining storeys first")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::DeferStoreyCount));
    }
}

/// The floor-exposure SELECT + APPLY toolbar (post-gate): the Select subtools
/// (Set all / Click / Carve / Remaining) plus Deselect, then the exposure
/// buttons that paint the current floor-region selection across the stack.
fn floor_exposure_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    select: Option<SelectMode>,
    on_event: &mut F,
) {
    let mut subtool = |ui: &mut egui::Ui, mode: SelectMode, label: &str| {
        let on = select == Some(mode);
        if ui.add(egui::Button::selectable(on, label)).clicked() {
            // Re-clicking the active subtool turns it off.
            let next = if on { None } else { Some(mode) };
            on_event(UiEvent::Sketch(SketchEvent::SetFloorExposureSelectMode(
                next,
            )));
        }
    };
    subtool(ui, SelectMode::All, "Set all");
    subtool(ui, SelectMode::Click, "Click");
    subtool(ui, SelectMode::Carve, "Carve");
    subtool(ui, SelectMode::Remaining, "Remaining");
    if ui
        .button("Deselect all")
        .hint("Clear the floor selection")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::DeselectAllFloorExposure));
    }
    ui.separator();
    for (label, exposure) in [
        ("Ground", FloorExposure::Ground),
        ("Exposed", FloorExposure::Exposed),
        ("Semi-exposed", FloorExposure::SemiExposed),
        ("Non-heat-loss", FloorExposure::NonHeatLoss),
        ("Unset", FloorExposure::Unset),
    ] {
        if ui.button(label).clicked() {
            on_event(UiEvent::Sketch(SketchEvent::AssignFloorExposure(exposure)));
        }
    }
}

/// The ceiling-exposure storey-count GATE (mirror of
/// [`floor_exposure_gate_toolbar`]): confirm every storey is added before the
/// auto-exposure pass runs and the overlaid ceiling view appears.
fn ceiling_exposure_gate_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
    ui.label(
        egui::RichText::new("Have you finished adding all storeys?")
            .strong()
            .color(egui::Color32::from_rgb(60, 220, 255)),
    );
    if ui
        .button("Yes")
        .hint("Run the inter-storey party auto-exposure pass, then mark ceilings")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::ConfirmAllStoreysAddedCeiling));
    }
    if ui
        .button("Not yet")
        .hint("Exit the tool so you can add the remaining storeys first")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::DeferStoreyCountCeiling));
    }
}

/// The ceiling-exposure SELECT + APPLY toolbar (post-gate), mirror of
/// [`floor_exposure_toolbar`]: the Select subtools (Set all / Click / Carve /
/// Remaining) plus Deselect, then the exposure buttons that paint the current
/// ceiling-region selection across the stack.
fn ceiling_exposure_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    select: Option<SelectMode>,
    on_event: &mut F,
) {
    let mut subtool = |ui: &mut egui::Ui, mode: SelectMode, label: &str| {
        let on = select == Some(mode);
        if ui.add(egui::Button::selectable(on, label)).clicked() {
            // Re-clicking the active subtool turns it off.
            let next = if on { None } else { Some(mode) };
            on_event(UiEvent::Sketch(SketchEvent::SetCeilingExposureSelectMode(
                next,
            )));
        }
    };
    subtool(ui, SelectMode::All, "Set all");
    subtool(ui, SelectMode::Click, "Click");
    subtool(ui, SelectMode::Carve, "Carve");
    subtool(ui, SelectMode::Remaining, "Remaining");
    if ui
        .button("Deselect all")
        .hint("Clear the ceiling selection")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::DeselectAllCeilingExposure));
    }
    ui.separator();
    for (label, exposure) in [
        ("Exposed roof", CeilingExposure::ExposedRoof),
        ("Party", CeilingExposure::Party),
        ("Unheated", CeilingExposure::Unheated),
        ("Unset", CeilingExposure::Unset),
    ] {
        if ui.button(label).clicked() {
            on_event(UiEvent::Sketch(SketchEvent::AssignCeilingExposure(
                exposure,
            )));
        }
    }
}

/// Wall-face partition toolbar: while a face is open, divide it (drag on the
/// canvas) and set each part's exposure. When a region is selected the three
/// exposure buttons apply to it; when a cut is mid-dimensioning the prompt's
/// metre entry + Skip resolve the next free coordinate. Always: "Remove
/// partition" (back to one face) and "Done" (close the overlay).
fn partition_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, pv: &PartitionView, on_event: &mut F) {
    ui.vertical(|ui| {
        // The cut's dimension is entered in the floating number box (same
        // keypad / laser as plan dimensioning — see `number_box`); the dock
        // keeps only the region exposure controls.
        ui.horizontal_wrapped(|ui| {
            let has_selection = pv.regions.iter().any(|r| r.selected);
            ui.label(
                egui::RichText::new(if has_selection {
                    "Set the selected part:"
                } else {
                    "Tap a part to select it"
                })
                .color(egui::Color32::from_rgb(255, 200, 60)),
            );
            for (label, exposure) in [
                ("Exposed", WallExposure::Exposed),
                ("Semi-exposed", WallExposure::SemiExposed),
                ("Non-heat-loss", WallExposure::NonHeatLoss),
                ("Ground", WallExposure::Ground),
                ("Unset", WallExposure::Unset),
            ] {
                if ui
                    .add_enabled(has_selection, egui::Button::new(label))
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::SetPartitionRegionExposure(
                        exposure,
                    )));
                }
            }
            ui.separator();
            if ui
                .button("Remove partition")
                .hint("Back to a single face")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::ClearWallPartition));
            }
            if ui.button("Done").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::CloseWallPartition));
            }
        });
    });
}
