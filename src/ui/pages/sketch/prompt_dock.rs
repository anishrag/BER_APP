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

/// Whether the floating flow panel has a REAL interactive control to show this
/// frame — a button to press or a value to type. It is NOT enough for a mode to
/// merely be active: a draw-on-canvas mode with nothing pending (e.g. floor
/// steps before any area is drawn, with no committed regions) shows NO box — the
/// bottom bar carries its instruction instead. Single-value number prompts and
/// openings placement also don't count (they use the number box / top-right).
pub(super) fn has_controls(view: &SketchView<'_>) -> bool {
    // Always-has-a-control flows (buttons / inputs / slider).
    if view
        .new_storey_prompt
        .as_ref()
        .is_some_and(|p| !p.thickness_pending)
        || view.pending_redraw
        || view.curve_bulge.is_some()
        || view.vertex_drop_prompt.is_some()
        || view.review.is_some()
        || view.partition_mode
        || view.floor_exposure_mode
        || view.ceiling_exposure_mode
    {
        return true;
    }
    // Wall exposure: only when there's a target to set (a selected section /
    // riser) or unset coverage the bulk button can paint.
    if view.exposure_mode {
        let riser_selected = view.riser_views.iter().any(|r| r.selected);
        let unset = view
            .exposure_walls
            .iter()
            .filter(|w| {
                w.partition_exposures.is_empty()
                    && document::wall_coverage(w.length_m, &w.sections)
                        .iter()
                        .any(|(_, _, e)| *e == WallExposure::Unset)
            })
            .count();
        return view.exposure_selection.is_some() || riser_selected || unset > 0;
    }
    if view.has_floor_plan {
        // Construction year: the picker (pending) or the "Set rest to Original"
        // Construction year: the year picker (pending) or the always-available
        // "Set rest to Original" shortcut. (Committed-area deletes → Delete panel.)
        if !view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending) {
            return true;
        }
        // Floor steps: NO floating control — amount in the number box, deletes in
        // the Delete panel. (Nothing here.)
        //
        // Ceiling steps: only the direction / side STAGE buttons.
        if view.ceiling_step_orientation_pending || view.ceiling_step_side_pending {
            return true;
        }
        // Roof: the which-area-slopes buttons or a position-stage Cancel.
        if !view.roof_ridge_area_options.is_empty()
            || view.roof_point_position_pending
            || view.roof_ridge_position_pending
            || view.roof_rect_position_pending
        {
            return true;
        }
    }
    false
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

/// A token identifying the CURRENT panel activation — what's selected / which
/// stage. The floating panel closes when a button is pressed and stays closed
/// until this token changes (a new selection, stage, or target reopens it). It
/// deliberately excludes terminal VALUES (e.g. the exposure picked), so applying
/// one keeps the panel closed; changing the selection reopens it.
pub(super) fn panel_token(view: &SketchView<'_>) -> String {
    if view.floor_exposure_mode {
        return format!("fe-{}", view.floor_exposure_gate_pending);
    }
    if view.ceiling_exposure_mode {
        return format!("ce-{}", view.ceiling_exposure_gate_pending);
    }
    if let Some(p) = &view.new_storey_prompt {
        return if p.method_pending {
            "ns-method".into()
        } else {
            format!("ns-{}", p.suggestion.as_ref().map(|s| s.2).unwrap_or(0))
        };
    }
    if view.pending_redraw {
        return "redraw".into();
    }
    if view.curve_bulge.is_some() {
        return "curve".into();
    }
    if let Some(vp) = &view.vertex_drop_prompt {
        return format!("vd-{}", vp.active.len());
    }
    if view.review.is_some() {
        return format!("rev-{}", view.review_remaining);
    }
    if view.partition_mode {
        let sel = view
            .partition_view
            .and_then(|pv| pv.regions.iter().position(|r| r.selected));
        return format!("part-{sel:?}");
    }
    if view.exposure_mode {
        return match view.exposure_selection.as_ref() {
            Some(s) => format!("exp-{:.3}-{:.3}", s.start_m, s.end_m),
            None => "exp-idle".into(),
        };
    }
    if view.has_floor_plan {
        if !view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending) {
            return format!(
                "phase-{}-{}",
                view.phase_pick_pending,
                view.phase_region_polygons.len()
            );
        }
        if view.step_mode {
            return format!("step-{}", view.step_region_polygons.len());
        }
        if view.ceiling_step_mode
            || view.ceiling_step_orientation_pending
            || view.ceiling_step_side_pending
        {
            return format!(
                "cs-{}-{}-{}",
                view.ceiling_step_orientation_pending,
                view.ceiling_step_side_pending,
                view.ceiling_step_region_polygons.len()
            );
        }
        if !view.roof_ridge_area_options.is_empty() {
            return "ridge-area".into();
        }
        if view.roof_point_mode
            || view.roof_point_position_pending
            || view.roof_ridge_position_pending
            || view.roof_rect_position_pending
        {
            return format!(
                "rp-{}-{}-{}-{}",
                view.roof_point_position_pending,
                view.roof_ridge_position_pending,
                view.roof_rect_position_pending,
                view.roof_apex_views.len()
            );
        }
    }
    "none".into()
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
        return Some(if view.floor_exposure_gate_pending {
            "Have you finished adding all storeys? (needed before the auto-exposure pass)"
        } else {
            "Floor exposures — select floors, then pick their heat-loss exposure."
        });
    }
    if view.ceiling_exposure_mode {
        return Some(if view.ceiling_exposure_gate_pending {
            "Have you finished adding all storeys? (needed before the auto-exposure pass)"
        } else {
            "Ceiling exposures — select ceilings, then pick their heat-loss exposure."
        });
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
        return Some("Wall partition — draw a cut line, then tap a part and set its exposure.");
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
        if view.ceiling_step_orientation_pending {
            return Some("Ceiling step — choose the step line's direction.");
        }
        if view.ceiling_step_side_pending {
            return Some("Ceiling step — tap the side of the line where the ceiling steps.");
        }
        if view.ceiling_step_mode {
            return Some("Ceiling steps — draw the step line, then set the level change.");
        }
        if !view.roof_ridge_area_options.is_empty() {
            return Some("Choose which area this ridge should slope.");
        }
        if view.roof_point_position_pending
            || view.roof_ridge_position_pending
            || view.roof_rect_position_pending
        {
            return Some("Pin the position on the plan — the distance is entered in the box.");
        }
        if view.roof_point_mode {
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
                phase_toolbar(ui, view.phase_choices, view.phase_pick_pending, on_event);
            }
            // Floor steps have NO floating control: the amount is in the number
            // box, and the committed-region deletes live in the Delete panel.
            // (Nothing to render here.)
            //
            // The ceiling-step AMOUNT (Up/Down) is entered in the number box; the
            // floating box keeps only the orientation / side STAGE buttons (the
            // committed-region deletes live in the Delete panel).
            if view.ceiling_step_orientation_pending || view.ceiling_step_side_pending {
                ceiling_step_toolbar(
                    ui,
                    view.ceiling_step_orientation_pending,
                    view.ceiling_step_side_pending,
                    on_event,
                );
            }
            // Same pending-with-mode-off rule again. A pending RIDGE or
            // RECT (the stroke flows sharing roof-point mode) keeps it
            // reachable too.
            if !view.roof_ridge_area_options.is_empty() {
                // Which-area-slopes buttons (the question is in the bottom bar).
                for (i, label) in view.roof_ridge_area_options.iter().enumerate() {
                    if ui.button(label).clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::ChooseRoofRidgeArea(i)));
                    }
                }
                if ui.button("Cancel").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::CancelPendingRoofRidge));
                }
            } else if view.roof_point_position_pending
                || view.roof_ridge_position_pending
                || view.roof_rect_position_pending
            {
                // Only the position-stage Cancel buttons live here; heights/
                // lengths are in the number box and the committed-apex deletes
                // are in the Delete panel.
                roof_point_toolbar(
                    ui,
                    view.roof_point_position_pending,
                    view.roof_ridge_position_pending,
                    view.roof_rect_position_pending,
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

/// Whether there's anything deletable to list in the Delete panel.
pub(super) fn has_deletables(view: &SketchView<'_>) -> bool {
    !view.phase_region_polygons.is_empty()
        || !view.step_region_polygons.is_empty()
        || !view.ceiling_step_region_polygons.is_empty()
        || !view.roof_apex_views.is_empty()
}

/// The "Delete" panel — a single home for removing things you've added
/// (construction-year areas, floor steps, ceiling steps, roof points), grouped
/// into clearly-labelled SECTIONS. These deletes used to clutter each tool's
/// floating control box; they live here instead so the control boxes stay
/// input-only. FIXED at the canvas bottom-left (just above the instruction bar),
/// a collapsing "🗑 Delete" header that expands UPWARD — the left-side mirror of
/// the bottom-right "Add external pictures" button. Renders nothing when there's
/// nothing to delete.
pub(super) fn render_delete_panel<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    view: &SketchView<'_>,
    on_event: &mut F,
) {
    if !has_deletables(view) {
        return;
    }
    // One section header grouping its 🗑 rows.
    fn section(ui: &mut egui::Ui, title: &str) {
        ui.add_space(2.0);
        ui.label(egui::RichText::new(title).strong().size(13.0));
    }
    // Bottom-left pivot → the panel grows UP into the canvas as the header
    // expands (mirror of the bottom-right external-photos button).
    egui::Area::new(egui::Id::new("sketch_delete_panel"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(rect.left_bottom() + egui::vec2(8.0, -8.0))
        .show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                egui::CollapsingHeader::new("🗑 Delete")
                    .default_open(false)
                    .show(ui, |ui| {
                        ui.set_max_width(240.0);
                        if !view.phase_region_polygons.is_empty() {
                            section(ui, "Construction years");
                            for (i, (_ring, label)) in view.phase_region_polygons.iter().enumerate()
                            {
                                ui.horizontal(|ui| {
                                    if ui.button("🗑").clicked() {
                                        on_event(UiEvent::Sketch(SketchEvent::DeletePhaseRegion(
                                            i,
                                        )));
                                    }
                                    ui.label(label);
                                });
                            }
                        }
                        if !view.step_region_polygons.is_empty() {
                            section(ui, "Floor steps");
                            for (i, (_ring, label)) in view.step_region_polygons.iter().enumerate()
                            {
                                ui.horizontal(|ui| {
                                    if ui.button("🗑").clicked() {
                                        on_event(UiEvent::Sketch(SketchEvent::DeleteStepRegion(i)));
                                    }
                                    ui.label(label);
                                });
                            }
                        }
                        if !view.ceiling_step_region_polygons.is_empty() {
                            section(ui, "Ceiling steps");
                            for (ring, label) in view.ceiling_step_region_polygons.iter() {
                                ui.horizontal(|ui| {
                                    if ui.button("🗑").clicked() {
                                        on_event(UiEvent::Sketch(
                                            SketchEvent::DeleteCeilingStepRegion(ring.clone()),
                                        ));
                                    }
                                    ui.label(label);
                                });
                            }
                        }
                        if !view.roof_apex_views.is_empty() {
                            section(ui, "Roof points");
                            for (i, (_pos, label)) in view.roof_apex_views.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui.button("🗑").clicked() {
                                        on_event(UiEvent::Sketch(SketchEvent::DeleteRoofApex(i)));
                                    }
                                    ui.label(label);
                                });
                            }
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
    // (The "fill in any 2 distances" instruction is in the bottom bar.)
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
    // (The "drag to set the curve radius" instruction is in the bottom bar.)
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
        // (The "how should the outline start?" instruction is in the bottom bar.)
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
        // The suggestion name + count are functional read-outs (what you're
        // browsing), not instructions, so they stay with the controls.
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
    // (The "what should this stroke do?" instruction is in the bottom bar.)
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
        // "Same line?" instruction is in the bottom bar; keep the count read-out.
        ui.label(format!("{remaining} left"));
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

/// Construction-phase tagging controls. While a carved area is pending, shows
/// the year picker (one button per defined phase) + Cancel; otherwise the
/// "Set rest to Original" shortcut. Committed-region deletes live in the Delete
/// panel.
fn phase_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    choices: &[(ConstructionPhase, String)],
    pick_pending: bool,
    on_event: &mut F,
) {
    if pick_pending {
        // (The "assign a year" instruction is in the bottom bar.)
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
        // (The "draw an area" instruction is in the bottom bar; the committed
        // regions' deletes live in the Delete panel.)
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

/// Ceiling-step STAGE buttons: the H/V direction picker for a tapped point, or
/// just a Cancel while the side pick is pending. The AMOUNT (Up/Down) is in the
/// number box; the committed-region deletes are in the Delete panel.
fn ceiling_step_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    orientation_pending: bool,
    side_pending: bool,
    on_event: &mut F,
) {
    if orientation_pending {
        // (The "line direction" instruction is in the bottom bar.)
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
        // (The "tap the side" instruction is in the bottom bar.)
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::CancelPendingCeilingStep));
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
    point_position_pending: bool,
    ridge_position_pending: bool,
    rect_position_pending: bool,
    on_event: &mut F,
) {
    // Each position stage keeps only its Cancel (the instruction is in the
    // bottom bar; the distance is typed in the box; apex deletes are in the
    // Delete panel).
    let position_cancel = |ui: &mut egui::Ui, ev: SketchEvent, on_event: &mut F| {
        if ui.button("Cancel").clicked() {
            on_event(UiEvent::Sketch(ev));
        }
    };
    if rect_position_pending {
        position_cancel(ui, SketchEvent::CancelPendingRoofRect, on_event);
    } else if ridge_position_pending {
        position_cancel(ui, SketchEvent::CancelPendingRoofRidge, on_event);
    } else if point_position_pending {
        position_cancel(ui, SketchEvent::CancelPendingRoofPoint, on_event);
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
        }
        // (The "tap a wall to mark its exposure" instruction is in the bottom
        // bar.) The bulk "set remaining" button is always available:
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
    // (The "finished adding all storeys?" question is in the bottom bar.)
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
    // (The "finished adding all storeys?" question is in the bottom bar.)
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
            // (The "tap a part / set the selected part" instruction is in the
            // bottom bar.)
            let has_selection = pv.regions.iter().any(|r| r.selected);
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
