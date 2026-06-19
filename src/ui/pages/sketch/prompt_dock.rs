//! The PROMPT DOCK: one bottom strip where every contextual flow panel of the
//! Sketch page renders, in both orientations — dimension/position prompts,
//! the post-drag and vertex-drop measurement prompts, the curve-radius
//! slider, the redraw decision, the collinearity review, the exposure
//! controls, and the phase / floor-step / ceiling-step flows. Visibility
//! logic is unchanged from the old bottom toolbar; only the location is
//! unified.

use super::{SketchEvent, SketchView};
use crate::controller::app_controller::{
    ExposureSelection, NewStoreyPromptUiData, OpeningPromptUiData, OpeningToolKind,
    VertexDropPromptUiData,
};
use crate::number_entry::parse_dimension_expression;
use crate::ui::pages::events::UiEvent;
use crate::ui::widgets::{metre_input, HintExt};
use datatypes::ber::common::ConstructionPhase;
use datatypes::ber::fenestration::{Frame, Glazing};
use datatypes::ber::structural::WallExposure;

/// Whether the dock has anything to show this frame. When false the bottom
/// panel is skipped entirely so the canvas gets the space.
pub(super) fn has_content(view: &SketchView<'_>) -> bool {
    // Single-value number prompts (dimension/position/edit-drag/thickness/step
    // & ceiling amounts/roof heights & lengths) are NOT listed here — they
    // render in the floating number-entry box, not the dock. Only the dock's
    // own content (mode lists, non-numeric stages, multi-field prompts) counts,
    // so the dock never draws an empty strip behind the box.
    view.current_storey_idx.is_none()
        || view
            .new_storey_prompt
            .as_ref()
            .is_some_and(|p| !p.thickness_pending)
        || view.pending_redraw
        || view.curve_bulge.is_some()
        || view.vertex_drop_prompt.is_some()
        || view.review.is_some()
        || view.exposure_mode
        || view.openings_mode
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
            if view.openings_mode {
                openings_toolbar(
                    ui,
                    view.openings_tool,
                    view.opening_prompt.as_ref(),
                    view.elevation_view.is_some(),
                    view.opening_width_input,
                    view.opening_height_input,
                    view.opening_sill_input,
                    view.opening_glazing,
                    view.opening_frame,
                    view.opening_year_input,
                    view.sidelight_left_input,
                    view.sidelight_right_input,
                    view.transom_input,
                    view.opening_error,
                    on_event,
                );
            }
        }
    });
}

/// The windows/doors toolbar: a type picker, then (once a wall/roof is tapped)
/// the size + glazing entry for the chosen type, or the elevation Done/Delete
/// controls when an elevation is open.
#[allow(clippy::too_many_arguments)]
fn openings_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    tool: OpeningToolKind,
    prompt: Option<&OpeningPromptUiData>,
    has_elevation: bool,
    width_input: &mut String,
    height_input: &mut String,
    sill_input: &mut String,
    glazing: &mut Glazing,
    frame: &mut Frame,
    year_input: &mut String,
    left_input: &mut String,
    right_input: &mut String,
    transom_input: &mut String,
    error: Option<&str>,
    on_event: &mut F,
) {
    use OpeningToolKind::*;
    ui.separator();
    // Type picker.
    let mut pick = |ui: &mut egui::Ui, k: OpeningToolKind, label: &str| {
        if ui.add(egui::Button::selectable(tool == k, label)).clicked() {
            on_event(UiEvent::Sketch(SketchEvent::SetOpeningTool(k)));
        }
    };
    pick(ui, Window, "🪟 Window");
    pick(ui, RoofWindow, "🔆 Roof window");
    pick(ui, Door, "🚪 Door");
    pick(ui, DoorSidelights, "🚪+ Sidelights");
    pick(ui, Edit, "✋ Edit on wall");

    if has_elevation {
        ui.separator();
        if ui.button("🗑 Delete selected").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::DeleteSelectedOpening));
        }
        if ui.button("Done").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::CloseElevation));
        }
        return;
    }

    let Some(prompt) = prompt else {
        ui.label(
            egui::RichText::new("  tap near a wall (or a roof for a roof window) to place")
                .color(egui::Color32::from_rgb(170, 170, 170)),
        );
        return;
    };
    ui.separator();
    // Size entry (door+sidelights labels its width/height as the DOOR's).
    let (w_label, h_label) = match prompt.tool {
        DoorSidelights => ("door W", "door H"),
        _ => ("W", "H"),
    };
    ui.label(w_label);
    metre_input(ui, width_input, 46.0, "=?");
    ui.label(h_label);
    metre_input(ui, height_input, 46.0, "=?");
    if matches!(prompt.tool, Window | RoofWindow) && !prompt.on_roof {
        ui.label("sill");
        metre_input(ui, sill_input, 46.0, "1.0");
    }
    // Door+sidelights: optional flanking + top lights.
    if matches!(prompt.tool, DoorSidelights) {
        ui.label("L light");
        metre_input(ui, left_input, 40.0, "—");
        ui.label("R light");
        metre_input(ui, right_input, 40.0, "—");
        ui.label("transom");
        metre_input(ui, transom_input, 40.0, "—");
    }
    // Glazing / frame / year for any window glass.
    if matches!(prompt.tool, Window | RoofWindow | DoorSidelights) {
        egui::ComboBox::from_id_salt("opening_glazing")
            .selected_text(glazing_label(glazing))
            .show_ui(ui, |ui| {
                ui.selectable_value(glazing, Glazing::Single, "Single");
                ui.selectable_value(glazing, Glazing::Double, "Double");
                ui.selectable_value(glazing, Glazing::Triple, "Triple");
            });
        egui::ComboBox::from_id_salt("opening_frame")
            .selected_text(frame_label(frame))
            .show_ui(ui, |ui| {
                ui.selectable_value(frame, Frame::WoodUPVC, "Wood/uPVC");
                ui.selectable_value(frame, Frame::Metal, "Metal");
            });
        ui.label("year");
        metre_input(ui, year_input, 50.0, "—");
    }
    if ui.button("Place").clicked() {
        let w = parse_dimension_expression(width_input);
        let h = parse_dimension_expression(height_input);
        if let (Some(width_m), Some(height_m)) = (w, h) {
            let year = year_input.trim().parse::<u32>().unwrap_or(0);
            let event = match prompt.tool {
                Window | RoofWindow => {
                    let sill_m = if prompt.on_roof {
                        0.0
                    } else {
                        parse_dimension_expression(sill_input).unwrap_or(1.0)
                    };
                    SketchEvent::SubmitWindow {
                        width_m,
                        height_m,
                        sill_m,
                        glazing: glazing.clone(),
                        frame: frame.clone(),
                        year,
                    }
                }
                Door | Edit => SketchEvent::SubmitDoor { width_m, height_m },
                DoorSidelights => SketchEvent::SubmitDoorSidelights {
                    door_w: width_m,
                    door_h: height_m,
                    left: parse_dimension_expression(left_input).filter(|v| *v > 0.0),
                    right: parse_dimension_expression(right_input).filter(|v| *v > 0.0),
                    transom: parse_dimension_expression(transom_input).filter(|v| *v > 0.0),
                    glazing: glazing.clone(),
                    frame: frame.clone(),
                    year,
                },
            };
            on_event(UiEvent::Sketch(event));
        }
    }
    if ui.button("Cancel").clicked() {
        on_event(UiEvent::Sketch(SketchEvent::CancelPendingOpening));
    }
    inline_error(ui, error);
}

fn glazing_label(g: &Glazing) -> &'static str {
    match g {
        Glazing::Single => "Single",
        Glazing::Double => "Double",
        Glazing::Triple => "Triple",
        Glazing::Other(_) => "Other",
    }
}

fn frame_label(f: &Frame) -> &'static str {
    match f {
        Frame::WoodUPVC => "Wood/uPVC",
        Frame::Metal => "Metal",
        Frame::Other(_) => "Other",
    }
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
