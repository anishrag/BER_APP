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
        || view.roof_mode
        || view.wall_inclusion_prompt.is_some()
    {
        return true;
    }
    // Wall exposure: only when there's a target to set (a selected section /
    // riser) or unset coverage the bulk button can paint (riser faces count —
    // the bulk button classifies them too).
    if view.exposure_mode {
        let riser_selected = view.riser_views.iter().any(|r| r.selected);
        let riser_unset = view
            .riser_views
            .iter()
            .any(|r| r.exposure == WallExposure::Unset);
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
        return view.exposure_selection.is_some() || riser_selected || riser_unset || unset > 0;
    }
    if view.has_floor_plan {
        // Construction year: the year picker (pending) or the always-available
        // "Set rest to Original" shortcut. (Committed-area deletes → Delete panel.)
        if !view.phase_choices.is_empty() && (view.phase_mode || view.phase_pick_pending) {
            return true;
        }
        // Floor steps: NO floating control — amount in the number box, deletes in
        // the Delete panel.
        //
        // Ceiling steps: only the DIRECTION choice (Horizontal / Vertical) is a
        // real in-box decision. "Tap the side" is a canvas action — its Cancel is
        // a top-right corner action, not a box.
        if view.ceiling_step_orientation_pending {
            return true;
        }
        // Roof: only the which-area-slopes choice is an in-box decision. Pinning a
        // position is a canvas action (distance in the number box) — its Cancel is
        // a top-right corner action, not a box.
        if !view.ceiling_ridge_area_options.is_empty() {
            return true;
        }
    }
    false
}

/// Whether the floating panel is a PERSISTENT select-then-set tool — you pick a
/// target on the canvas, then set it in the box, and may repeat for several
/// targets. For these a button press must NOT close the box (you still need to
/// set the selection, and to set more); only the box's own "Done" closes it (by
/// exiting the mode). Other panels keep the press-to-close behaviour.
pub(super) fn panel_persists(view: &SketchView<'_>) -> bool {
    view.floor_exposure_mode
        || view.ceiling_exposure_mode
        || view.roof_mode
        || view.exposure_mode
        || view.partition_mode
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
    if view.roof_mode {
        return "Roof";
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
    if view.wall_inclusion_prompt.is_some() {
        return "Include wall?";
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
        if !view.ceiling_ridge_area_options.is_empty() {
            return "Ridge";
        }
    }
    "Ceiling shape"
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
    if let Some(wi) = &view.wall_inclusion_prompt {
        // Keyed by the cursor so each ambiguous edge reopens the panel.
        return format!("wi-{}-{}", wi.index, wi.total);
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
        if !view.ceiling_ridge_area_options.is_empty() {
            return "ridge-area".into();
        }
        if view.ceiling_point_mode
            || view.ceiling_point_position_pending
            || view.ceiling_ridge_position_pending
            || view.ceiling_rect_position_pending
        {
            return format!(
                "rp-{}-{}-{}-{}",
                view.ceiling_point_position_pending,
                view.ceiling_ridge_position_pending,
                view.ceiling_rect_position_pending,
                view.ceiling_apex_views.len()
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
            "Have you finished adding all storeys? (inter-storey surfaces derive from the full stack)"
        } else {
            "Floor exposures — select floors, then pick their heat-loss exposure."
        });
    }
    if view.ceiling_exposure_mode {
        return Some(if view.ceiling_exposure_gate_pending {
            "Have you finished adding all storeys? (inter-storey surfaces derive from the full stack)"
        } else {
            "Ceiling exposures — select ceilings, then pick their heat-loss exposure."
        });
    }
    if view.roof_mode {
        return Some(if view.roof_gate_pending {
            "Have you finished adding all storeys? (the roof covers the top storey everywhere)"
        } else if view.roof_area_pitch_pending {
            "Roof — type the pitch (degrees) for the ridge(s)."
        } else if view.roof_area_ridge_pending {
            if view.roof_area_hip_ridge_count.is_some() {
                "Roof — draw ALL the hip's ridge lines, then Finish. Near an edge snaps onto it."
            } else {
                "Roof — draw the ridge line across the grouped area (near an edge snaps onto it)."
            }
        } else if view.roof_area_can_pick_shape {
            if view.roof_area_ceiling_sloped {
                "Roof — pick the shape (Follow ceiling / Flat / Gable / Hip)."
            } else {
                "Roof — pick the shape for the grouped area (Flat / Gable / Hip)."
            }
        } else {
            "Roof — tap the ceiling areas (shaded by top storey) to group them into one roof area."
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
    if view.wall_inclusion_prompt.is_some() {
        return Some(
            "The house continues past the highlighted edge — include the wall along it in the selection?",
        );
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
        if !view.ceiling_ridge_area_options.is_empty() {
            return Some("Choose which area this ridge should slope.");
        }
        if view.ceiling_point_position_pending
            || view.ceiling_ridge_position_pending
            || view.ceiling_rect_position_pending
        {
            return Some("Pin the position on the plan — the distance is entered in the box.");
        }
        if view.ceiling_point_mode {
            return Some(
                "Ceiling shape — tap an apex, or draw a ridge / flat-top, then set heights.",
            );
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
    // The roof step likewise spans the whole house and owns the dock.
    if view.roof_mode {
        if view.roof_gate_pending {
            ui.horizontal_wrapped(|ui| roof_gate_toolbar(ui, on_event));
        } else {
            roof_toolbar(ui, view, on_event);
        }
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
        } else if let Some(wi) = &view.wall_inclusion_prompt {
            wall_inclusion_toolbar(ui, wi, on_event);
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
            // Ceiling steps: only the DIRECTION choice is an in-box decision; the
            // amount is in the number box, the "tap a side" Cancel is top-right,
            // and the deletes are in the Delete panel.
            if view.ceiling_step_orientation_pending {
                ceiling_step_toolbar(ui, on_event);
            }
            // Roof: the which-area-slopes choice is the only in-box decision; the
            // position-stage Cancel is top-right, heights/lengths in the number
            // box, and apex deletes in the Delete panel.
            if !view.ceiling_ridge_area_options.is_empty() {
                // Which-area-slopes buttons (the question is in the bottom bar).
                for (i, label) in view.ceiling_ridge_area_options.iter().enumerate() {
                    if ui.button(label).clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::ChooseCeilingRidgeArea(i)));
                    }
                }
                if ui.button("Cancel").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::CancelPendingCeilingRidge));
                }
            }
        }
    });
}

/// Top-right floating ACTION buttons — dismiss/confirm actions that don't belong
/// in a control box or the bottom bar: the openings edit-on-surface Delete/Done,
/// and the Cancel for a canvas-positioning stage (ceiling-step side pick, roof
/// position pin) whose actual input is a tap on the canvas. Renders nothing when
/// none apply.
pub(super) fn render_corner_actions<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
    rect: egui::Rect,
    view: &SketchView<'_>,
    on_event: &mut F,
) {
    let has_elevation = view.elevation_view.is_some();
    let openings_selection =
        view.openings_mode && (has_elevation || view.roof_window_selected.is_some());
    // A canvas-positioning stage in progress → offer just its Cancel here.
    let stage_cancel = if view.ceiling_step_side_pending {
        Some(SketchEvent::CancelPendingCeilingStep)
    } else if view.ceiling_rect_position_pending {
        Some(SketchEvent::CancelPendingCeilingRect)
    } else if view.ceiling_ridge_position_pending {
        Some(SketchEvent::CancelPendingCeilingRidge)
    } else if view.ceiling_point_position_pending {
        Some(SketchEvent::CancelPendingCeilingPoint)
    } else {
        None
    };
    if !openings_selection && stage_cancel.is_none() {
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
                    if openings_selection {
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
                    }
                    if let Some(ev) = stage_cancel {
                        if ui.button("Cancel").clicked() {
                            on_event(UiEvent::Sketch(ev));
                        }
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
        || !view.ceiling_apex_views.is_empty()
}

/// The "Delete" panel — a single home for removing things you've added
/// (construction-year areas, floor steps, ceiling steps, roof points), grouped
/// into clearly-labelled SECTIONS. These deletes used to clutter each tool's
/// floating control box; they live here instead so the control boxes stay
/// input-only. FIXED at the very bottom-left of the WINDOW — the same level as
/// the workspace "Continue →" bar — as a collapsing "🗑 Delete" header that
/// expands UPWARD. Renders nothing when there's nothing to delete.
pub(super) fn render_delete_panel<F: FnMut(UiEvent)>(
    ctx: &egui::Context,
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
    // Anchor to the WINDOW bottom-left (over the workspace Continue bar's empty
    // left side), bottom pivot → the list grows UP as the header expands.
    let bl = ctx.content_rect().left_bottom();
    egui::Area::new(egui::Id::new("sketch_delete_panel"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::LEFT_BOTTOM)
        .fixed_pos(bl + egui::vec2(8.0, -6.0))
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
                        if !view.ceiling_apex_views.is_empty() {
                            section(ui, "Roof points");
                            for (i, (_pos, label)) in view.ceiling_apex_views.iter().enumerate() {
                                ui.horizontal(|ui| {
                                    if ui.button("🗑").clicked() {
                                        on_event(UiEvent::Sketch(SketchEvent::DeleteCeilingApex(
                                            i,
                                        )));
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

/// The structure-carve wall-inclusion ask: one ambiguous carve edge at a time
/// (highlighted on the canvas), answered Include / Exclude. The label carries
/// the "Wall N of M" cursor so a multi-edge carve reads as a short sequence.
fn wall_inclusion_toolbar<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    wi: &crate::controller::app_controller::WallInclusionPromptView,
    on_event: &mut F,
) {
    if wi.total > 1 {
        ui.label(format!("Wall {} of {}:", wi.index + 1, wi.total));
    }
    if ui
        .button("Include")
        .hint("Keep the wall along the highlighted edge in the selection")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::ResolveWallInclusion {
            include: true,
        }));
    }
    if ui
        .button("Exclude")
        .hint("Leave the wall along the highlighted edge out of the selection")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::ResolveWallInclusion {
            include: false,
        }));
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

/// Ceiling-step DIRECTION picker (Horizontal / Vertical) for a tapped point — the
/// only in-box decision of the ceiling-step flow. The amount is in the number
/// box; "tap a side" is a canvas action (its Cancel is a corner action); deletes
/// are in the Delete panel.
fn ceiling_step_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
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
        .count()
        // Riser faces count too — the bulk button classifies them alongside
        // the outline walls.
        + risers
            .iter()
            .filter(|r| r.exposure == WallExposure::Unset)
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
            // Ground (below-grade contact) — DEAP treats it separately and a tall
            // ground wall is reclassified as a floor at export. Offered for both
            // risers and outline walls (e.g. a basement perimeter / deep step).
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
                .button("Ground")
                .hint(
                    "In contact with the ground (below grade — e.g. a basement / \
                     deep step wall). DEAP reclassifies a tall ground wall as a floor.",
                )
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::SetWallExposure(
                    WallExposure::Ground,
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
        // Persistent tool: stays open while you select + set walls; Done finishes.
        if ui.button("Done").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::SetExposureMode(false)));
        }
    });
}

/// The floor-exposure storey-count GATE: confirm every storey is added before
/// the overlaid floor view appears (inter-storey surfaces derive from the
/// full stack, so labelling earlier would waste work).
fn floor_exposure_gate_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
    // (The "finished adding all storeys?" question is in the bottom bar.)
    if ui
        .button("Yes")
        .hint("Open the all-storeys floor view and mark the floors")
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
    ui.separator();
    // Persistent tool: stays open while you select + set floors; Done finishes.
    if ui.button("Done").clicked() {
        on_event(UiEvent::Sketch(SketchEvent::SetFloorExposureMode(false)));
    }
}

/// The ceiling-exposure storey-count GATE (mirror of
/// [`floor_exposure_gate_toolbar`]): confirm every storey is added before the
/// overlaid ceiling view appears (inter-storey surfaces derive from the full
/// stack, so labelling earlier would waste work).
fn ceiling_exposure_gate_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
    // (The "finished adding all storeys?" question is in the bottom bar.)
    if ui
        .button("Yes")
        .hint("Open the all-storeys ceiling view and mark the ceilings")
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

/// The roof-step storey-count GATE toolbar: confirm all storeys are added (the
/// roof covers the top storey everywhere) or defer to add the rest first.
fn roof_gate_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, on_event: &mut F) {
    if ui
        .button("Yes")
        .hint("Show the whole-house outline shaded by top storey, ready to draw the roof")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::ConfirmAllStoreysAddedRoof));
    }
    if ui
        .button("Not yet")
        .hint("Exit the roof step so you can add the remaining storeys first")
        .clicked()
    {
        on_event(UiEvent::Sketch(SketchEvent::DeferStoreyCountRoof));
    }
}

/// The roof-step toolbar (post-gate): drives the per-area drawing flow —
/// group areas (tap on the canvas) → pick Flat/Gable/Hip → draw a ridge → type
/// the pitch. Which controls show depends on the in-flight stage.
fn roof_toolbar<F: FnMut(UiEvent)>(ui: &mut egui::Ui, view: &mut SketchView<'_>, on_event: &mut F) {
    use crate::controller::app_controller::RoofShape;
    ui.horizontal_wrapped(|ui| {
        if view.roof_area_pitch_pending {
            ui.label("Pitch:");
            let resp = ui.add(
                egui::TextEdit::singleline(view.roof_pitch_input)
                    .desired_width(54.0)
                    .hint_text("°"),
            );
            let submit = (resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)))
                | ui.button("Set").clicked();
            if submit {
                if let Ok(deg) = view.roof_pitch_input.trim().parse::<f32>() {
                    on_event(UiEvent::Sketch(SketchEvent::AssignRoofPitch(deg)));
                }
            }
            // A gable's rough stroke can be perfected: recentre it between the
            // eaves (the drawn position stays available by just not pressing it).
            if view.roof_area_can_centre_ridge
                && ui
                    .button("Centre on roof")
                    .hint("Move the ridge to the middle of the area it spans")
                    .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::CentreRoofRidge));
            }
            if ui.button("Cancel").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::CancelRoofArea));
            }
        } else if view.roof_area_ridge_pending {
            if let Some(count) = view.roof_area_hip_ridge_count {
                // HIP: several ridges (an L-shaped roof has one per wing) —
                // keep drawing, then Finish moves to the pitch.
                ui.label(if count == 0 {
                    "Draw the hip's ridge lines.".to_string()
                } else {
                    format!("{count} ridge(s) drawn — draw the rest, then Finish.")
                });
                if count > 0
                    && ui
                        .button("Finish ridges")
                        .hint("All ridges drawn — set the pitch next")
                        .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::FinishRoofRidges));
                }
            } else {
                ui.label("Draw the ridge line across the area.");
            }
            if ui.button("Cancel").clicked() {
                on_event(UiEvent::Sketch(SketchEvent::CancelRoofArea));
            }
        } else if view.roof_area_can_pick_shape {
            ui.label("Shape:");
            // Converted attic: the ceiling is already sloped — offer to adopt it
            // (the rafters ARE the ceiling), so no ridge/pitch is re-asked.
            if view.roof_area_ceiling_sloped
                && ui
                    .button("Follow ceiling")
                    .hint("Converted attic — the roof follows the already-sloped ceiling")
                    .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::PickRoofShape(
                    RoofShape::FollowCeiling,
                )));
            }
            if ui
                .button("Flat")
                .hint("Flat roof — keeps the ceiling level")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::PickRoofShape(RoofShape::Flat)));
            }
            if ui
                .button("Gable")
                .hint("Ridge to the end walls (vertical gable ends)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::PickRoofShape(
                    RoofShape::Gable,
                )));
            }
            if ui
                .button("Hip")
                .hint("Ridge inset from the ends (the ends slope down too)")
                .clicked()
            {
                on_event(UiEvent::Sketch(SketchEvent::PickRoofShape(RoofShape::Hip)));
            }
        }
        if ui.button("Done").hint("Leave the roof step").clicked() {
            on_event(UiEvent::Sketch(SketchEvent::SetRoofMode(false)));
        }
        if let Some(err) = view.roof_error {
            ui.colored_label(egui::Color32::from_rgb(200, 60, 60), err);
        }
    });
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
    ui.separator();
    // Persistent tool: stays open while you select + set ceilings; Done finishes.
    if ui.button("Done").clicked() {
        on_event(UiEvent::Sketch(SketchEvent::SetCeilingExposureMode(false)));
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
