//! The Sketch CANVAS: paints the plan + every overlay, routes pointer input
//! (strokes, taps, drags, pan/zoom) into [`SketchEvent`]s, and hosts the
//! floating view buttons (2D/3D toggle + display-mode cycler) in its
//! top-right corner.

use egui::epaint::PathShape;
use strum::IntoEnumIterator;

use super::{EguiStroke, SketchEvent, SketchView};
use crate::ui::pages::events::UiEvent;
use crate::ui::render;
use crate::ui::render::ViewParams;
use datatypes::ber::ber_struct::Ber;
use domain::sketch::{DisplayMode, EditTools, SketchMode};

pub(super) fn render<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    view: &SketchView<'_>,
    on_event: &mut F,
) {
    let reviewing = view.review.is_some();
    let dim_prompting = view.dim_prompt.is_some();
    let ber = view.ber;
    let current_storey_idx = view.current_storey_idx;
    let mode = view.mode;
    let display = view.display;
    let edit_tool = view.edit_tool;
    let vp = view.view_params;

    // Wall-length labels gated on the Dimensions-category toggle: passing
    // empty label lists leaves the geometry untouched and just drops the
    // text.
    let dimensioned_walls: &[(usize, usize)] = if view.cat_state.show_wall_lengths {
        view.dimensioned_walls
    } else {
        &[]
    };
    let living_area_wall_labels: &[Vec<((f64, f64), (f64, f64), f32)>] =
        if view.cat_state.show_wall_lengths {
            view.living_area_wall_labels
        } else {
            &[]
        };

    // `click_and_drag` so middle/secondary drags register too
    // (plain `drag` is biased toward primary-button drag).
    let (rect, response) =
        ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(30));

    // Floating view buttons in the canvas's top-right corner (an egui Area on
    // the Foreground order, so they sit above the canvas and own their
    // clicks — same pattern as the snap-tag menus).
    view_buttons(ui, rect, view, on_event);

    // BER view layer with optional collinearity highlight overlay.
    render::render_ber(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        display,
        view.review,
        dimensioned_walls,
        vp,
    );

    // New-storey browse stage: tint the current outline suggestion's ring
    // translucently over the previous-storey watermark so the assessor sees
    // the candidate footprint in place while cycling ◀ ▶ in the dock.
    if let Some((ring, _label, _idx, _count)) = view
        .new_storey_prompt
        .as_ref()
        .and_then(|p| p.suggestion.as_ref())
    {
        render::render_suggestion_ring(&painter, rect, ber, current_storey_idx, mode, vp, ring);
    }

    // Living-area sub-shapes overlaid in green (active brighter),
    // with length labels on their known walls (like the storey).
    render::render_living_areas(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        display,
        vp,
        view.living_area_polygons,
        living_area_wall_labels,
        view.active_living_area,
    );

    // Pending cut: show both candidate regions to tap between.
    render::render_cut_candidates(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.pending_cut_regions,
    );

    // DEBUG: splice-pipeline phases of the last extend/trim.
    render::render_splice_debug(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.splice_debug,
    );

    // Elective-snap tags of the last action (orange markers + kind).
    render::render_snap_tags(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.snap_tags,
    );

    // Left-click menu per snap tag: pick a different candidate, or
    // "Don't snap", to replay the action with just that snap changed.
    if !view.snap_tags.is_empty() {
        if let Some(t) = render::active_storey_transform(rect, ber, current_storey_idx, mode, vp) {
            for (ti, tag) in view.snap_tags.iter().enumerate() {
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
                                    on_event(UiEvent::Sketch(SketchEvent::ChooseSnap {
                                        from: tag.from,
                                        choice: Some(*idx),
                                    }));
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
        vp,
        view.phase_region_polygons,
        phase_tint,
    );
    render::render_cut_candidates(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.pending_phase_cut_regions,
    );
    if view.pending_phase_polygon.len() >= 3 {
        render::render_phase_regions(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            std::slice::from_ref(&(view.pending_phase_polygon.to_vec(), "?".to_string())),
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
        vp,
        view.step_region_polygons,
        step_tint,
    );
    render::render_cut_candidates(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.pending_step_cut_regions,
    );
    if view.pending_step_polygon.len() >= 3 {
        render::render_phase_regions(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            std::slice::from_ref(&(view.pending_step_polygon.to_vec(), "?".to_string())),
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
        vp,
        view.ceiling_step_region_polygons,
        ceiling_step_tint,
    );
    render::render_cut_candidates(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.pending_ceiling_step_side_rects,
    );
    if let Some((a, b)) = view.pending_ceiling_step_line {
        render::render_ceiling_step_line(&painter, rect, ber, current_storey_idx, mode, vp, a, b);
    }

    // The pending line's wall snap, drawn through the SAME snap-tag
    // renderer as the stroke snaps — but its menu is a separate
    // widget routed to UnsnapCeilingStepLine, never ChooseSnap:
    // the generic replay rewinds history, and the pending line
    // lives outside it. With no alternative candidates the menu
    // offers exactly "Don't snap".
    if let Some(tag) = view.ceiling_step_snap_tag {
        render::render_snap_tags(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            std::slice::from_ref(tag),
        );
        if let Some(t) = render::active_storey_transform(rect, ber, current_storey_idx, mode, vp) {
            let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
            let label = match tag.current_kind {
                Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                None => "no snap ▾".to_string(),
            };
            egui::Area::new(ui.id().with("ceiling_step_snap_tag"))
                .fixed_pos(sp + egui::vec2(7.0, -7.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    ui.menu_button(label, |ui| {
                        if tag.skippable && ui.button("Don't snap").clicked() {
                            on_event(UiEvent::Sketch(SketchEvent::UnsnapCeilingStepLine));
                            ui.close_menu();
                        }
                    });
                });
        }
    }

    // Roof apexes: committed high points ("▲ 4.20 m" markers) plus the
    // pending tapped point as a hollow dot, in a distinct rose-magenta.
    render::render_roof_apexes(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.roof_apex_views,
        view.pending_roof_point,
    );

    // The pending roof point's wall snap(s) as ONE tag, mirroring the
    // ceiling-step line's: drawn through the same snap-tag renderer, but
    // its menu is a separate widget routed to UnsnapRoofPoint, never
    // ChooseSnap — the generic replay rewinds history, and the pending
    // point lives outside it. The menu offers exactly "Don't snap".
    if let Some(tag) = view.roof_point_snap_tag {
        render::render_snap_tags(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            std::slice::from_ref(tag),
        );
        if let Some(t) = render::active_storey_transform(rect, ber, current_storey_idx, mode, vp) {
            let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
            let label = match tag.current_kind {
                Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                None => "no snap ▾".to_string(),
            };
            egui::Area::new(ui.id().with("roof_point_snap_tag"))
                .fixed_pos(sp + egui::vec2(7.0, -7.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    ui.menu_button(label, |ui| {
                        if tag.skippable && ui.button("Don't snap").clicked() {
                            on_event(UiEvent::Sketch(SketchEvent::UnsnapRoofPoint));
                            ui.close_menu();
                        }
                    });
                });
        }
    }

    // Roof ridges: committed two-apex groups as solid labelled segments
    // ("⌃ 4.20 m"), plus the pending ridge as a dashed SEGMENT between its
    // own endpoints (the stroke's extent, not bbox-spanning). Violet-blue,
    // distinct from the apex rose-magenta.
    render::render_roof_ridges(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.roof_ridge_views,
    );
    if let Some((a, b)) = view.pending_roof_ridge_line {
        render::render_roof_ridge_line(&painter, rect, ber, current_storey_idx, mode, vp, a, b);
    }

    // The pending ridge's wall snap, mirroring the ceiling-step line's:
    // drawn through the same snap-tag renderer, but its menu is a separate
    // widget routed to UnsnapRoofRidge, never ChooseSnap — the generic
    // replay rewinds history, and the pending ridge lives outside it. The
    // menu offers exactly "Don't snap".
    if let Some(tag) = view.roof_ridge_snap_tag {
        render::render_snap_tags(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            std::slice::from_ref(tag),
        );
        if let Some(t) = render::active_storey_transform(rect, ber, current_storey_idx, mode, vp) {
            let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
            let label = match tag.current_kind {
                Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                None => "no snap ▾".to_string(),
            };
            egui::Area::new(ui.id().with("roof_ridge_snap_tag"))
                .fixed_pos(sp + egui::vec2(7.0, -7.0))
                .order(egui::Order::Foreground)
                .show(ui.ctx(), |ui| {
                    ui.menu_button(label, |ui| {
                        if tag.skippable && ui.button("Don't snap").clicked() {
                            on_event(UiEvent::Sketch(SketchEvent::UnsnapRoofRidge));
                            ui.close_menu();
                        }
                    });
                });
        }
    }

    // Wall exposure: colour each outline wall by its heat-loss class
    // (brighter in exposure mode; the selected wall is haloed). Derived
    // riser walls draw dashed alongside, selectable in exposure mode.
    render::render_wall_exposure(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.exposure_walls,
        view.riser_views,
        view.exposure_selection.as_ref(),
        view.exposure_mode,
    );

    // Move-mode selection highlight: paint the selected wall
    // in red so the user sees which wall their drag is moving.
    // The inserted-vertex marker (if any) takes precedence —
    // both can't be selected at once.
    if matches!(edit_tool, EditTools::Move) {
        if let Some((wa, wb)) = view.edit_selected_wall {
            render::render_equivalent_wall(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                wa,
                wb,
            );
        }
        if let Some((vx, vy)) = view.edit_selected_vertex {
            render::render_vertex_marker(&painter, rect, ber, current_storey_idx, mode, vp, vx, vy);
        }
    }

    // Vertex-drop prompt: draw a perpendicular distance arrow
    // for each still-active measurement (axis not yet pinned),
    // so the user sees what each text box refers to.
    if let Some(vdp) = &view.vertex_drop_prompt {
        for m in &vdp.active {
            use crate::controller::app_controller::VertexMeasurementKind;
            use manip_2d_plan::ParameterAxis;
            // Axis measurements get a perpendicular arrow at
            // the vertex's row/column. WallLength measurements
            // draw the arrow directly along the diagonal from
            // neighbour → vertex (= the new wall itself).
            let (arrow_a, arrow_b) = match m.kind {
                VertexMeasurementKind::Axis(ParameterAxis::X) => (
                    (m.neighbor_pos.0, vdp.vertex_pos.1),
                    (vdp.vertex_pos.0, vdp.vertex_pos.1),
                ),
                VertexMeasurementKind::Axis(ParameterAxis::Y) => (
                    (vdp.vertex_pos.0, m.neighbor_pos.1),
                    (vdp.vertex_pos.0, vdp.vertex_pos.1),
                ),
                VertexMeasurementKind::WallLength => (m.neighbor_pos, vdp.vertex_pos),
            };
            render::render_dimension_prompt(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
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
    if let Some(ep) = &view.edit_drag_prompt {
        render::render_equivalent_wall(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
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
            vp,
            arrow_a,
            arrow_b,
        );
    }

    // Dimension-prompt overlay. Every wall the user can
    // equivalently measure (including the primary) is highlighted
    // in red. The primary additionally gets the cyan interior
    // arrow + length label, drawn last so it sits on top.
    if let Some(p) = &view.dim_prompt {
        for wall in &p.walls {
            render::render_equivalent_wall(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
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
                vp,
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
    if let Some(pp) = &view.position_prompt {
        render::render_wall_highlight(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
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
            vp,
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
            vp,
            arrow_a,
            arrow_b,
        );
    }

    // Provisional curved-wall preview (amber arc) drawn on top, so
    // the user sees the curve while dimensioning its attach lines.
    if let Some(pts) = view.arc_preview {
        render::render_arc_preview(&painter, rect, ber, current_storey_idx, mode, vp, pts);
    }

    // Pan / zoom / orbit on the canvas.
    //   - Scroll wheel  → zoom about the cursor (2D and 3D)
    //   - Middle/right-drag → pan (2D and 3D)
    //   - Primary drag in 3D → orbit (Google-Earth style; primary is the
    //     stroke-drawing button in 2D, but draws nothing in 3D)
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
    // In 3D the primary drag ORBITS the camera (it draws strokes only in
    // 2D — see `primary_available` below). Trackball feel: the near side of
    // the model follows the cursor — drag right swings it right (yaw),
    // drag down pulls it down revealing the top (pitch up, toward plan).
    // Reset via the Drawing category's Fit button.
    if matches!(mode, SketchMode::Storey3D) && response.dragged_by(egui::PointerButton::Primary) {
        const ORBIT_DEG_PER_PIXEL: f32 = 0.4;
        let d = response.drag_delta();
        if d.x.abs() > 0.0 || d.y.abs() > 0.0 {
            on_event(UiEvent::Sketch(SketchEvent::OrbitView {
                dyaw: -d.x * ORBIT_DEG_PER_PIXEL,
                dpitch: d.y * ORBIT_DEG_PER_PIXEL,
            }));
        }
    }

    // Pending cut: a primary click picks which candidate region to
    // keep as the living area. Takes priority over stroke/edit input.
    let cut_pending = !view.pending_cut_regions.is_empty();
    if cut_pending {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::ChooseCutRegion {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Pending PHASE cut: a primary click picks which candidate region
    // to tag with a construction phase. Mirrors the living-area cut.
    let phase_cut_pending = !view.pending_phase_cut_regions.is_empty();
    if phase_cut_pending {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::ChoosePhaseCutRegion {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Pending STEP cut: a primary click picks which candidate region
    // gets the floor level. Mirrors the phase cut.
    let step_cut_pending = !view.pending_step_cut_regions.is_empty();
    if step_cut_pending {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::ChooseStepCutRegion {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Ceiling-step taps: in mode with nothing pending, a tap places
    // the line's point (H/V picked next) — but only under the draw
    // tool, so Move/Trim clicks in-mode don't spawn phantom points.
    // While the side pick is pending — including with the mode
    // toggled off — the tap picks the stepping side. A pending
    // cut-pick owns the tap instead (those handlers above have no
    // mode guard, so one tap would otherwise fire both). Other
    // stages ignore taps (the controller's stage dispatch owns
    // that), so this can stay coarse.
    if (view.ceiling_step_side_pending
        || (view.ceiling_step_mode && matches!(edit_tool, EditTools::DrawPerpWalls)))
        && !cut_pending
        && !phase_cut_pending
        && !step_cut_pending
    {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::CeilingStepTap {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Roof-point tap: in mode with nothing pending, a tap places the
    // apex's point — but only under the draw tool, so Move/Trim clicks
    // in-mode don't spawn phantom points (the ceiling-step gating). A
    // pending cut-pick owns the tap instead. Once a stage is pending —
    // a point's OR a ridge's (stroke = ridge shares the mode) — taps do
    // nothing (the prompts are toolbar/panel-driven).
    if view.roof_point_mode
        && !view.roof_point_pending
        && !view.roof_ridge_pending
        && matches!(edit_tool, EditTools::DrawPerpWalls)
        && !cut_pending
        && !phase_cut_pending
        && !step_cut_pending
    {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::RoofPointTap {
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
    if view.exposure_mode && !cut_pending {
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, false) {
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

    // Primary-button input: stroke drawing OR Move-mode
    // click/drag, depending on `edit_tool`. Both gated on
    // 2D mode and "no overlay is open" (review / pending
    // redraw / dim prompt / cut-pick take priority).
    let primary_available = matches!(mode, SketchMode::Storey2D)
        && !reviewing
        && !view.pending_redraw
        && !dim_prompting
        && !cut_pending
        && !phase_cut_pending
        && !view.phase_pick_pending
        && !step_cut_pending
        && !view.step_pick_pending
        && !view.ceiling_step_pending
        && !view.roof_point_pending
        && !view.roof_ridge_pending
        && !view.exposure_mode;

    if primary_available && !matches!(edit_tool, EditTools::Move) {
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
                    view.living_area_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // Construction-phase tagging draws in the storey's
                // metre space too (carve an area to assign a year).
                let phase_draw = view.phase_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // Floor-step tagging draws in the storey's metre
                // space too (carve an area to raise/lower its floor).
                let step_draw = view.step_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // A ceiling-step stroke is the LINE the ceiling
                // steps along — drawn in metre space as well.
                let ceiling_step_draw =
                    view.ceiling_step_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // In roof-point mode a STROKE draws a RIDGE (taps stay
                // the point input — the ceiling-step stroke/tap split),
                // in metre space too.
                let roof_ridge_draw =
                    view.roof_point_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // First outline of a fresh upper storey: drawn over
                // the previous-storey watermark, so it must be
                // captured in that storey's metre space (mirrors the
                // watermark predicate in render::render_ber).
                let outline_on_watermark = matches!(edit_tool, EditTools::DrawPerpWalls)
                    && !view.living_area_mode
                    && !view.phase_mode
                    && !view.step_mode
                    && !view.ceiling_step_mode
                    && !view.roof_point_mode
                    && matches!(current_storey_idx, Some(i) if i > 0)
                    && ber
                        .and_then(|b| b.storeys.get(current_storey_idx?))
                        .is_some_and(|s| s.floors.regions.is_empty());
                let needs_world = living_area_draw
                    || phase_draw
                    || step_draw
                    || ceiling_step_draw
                    || roof_ridge_draw
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
                        view.current.as_ref(),
                        canvas_transform(ber, current_storey_idx, rect, vp, true),
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
                        } else if roof_ridge_draw {
                            SketchEvent::RoofRidgeStrokeWorld {
                                points: world_points,
                            }
                        } else if outline_on_watermark {
                            SketchEvent::OutlineStrokeWorld {
                                points: world_points,
                            }
                        } else if matches!(
                            edit_tool,
                            EditTools::Trim | EditTools::TrimOrtho | EditTools::TrimCurve
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
                        let has_stroke = view.current.is_some();
                        let has_bbox =
                            canvas_transform(ber, current_storey_idx, rect, vp, true).is_some();
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

        if let Some(stroke) = &view.current {
            draw_stroke(&painter, rect, stroke);
        }
    }

    // Move-mode primary input: click to select a wall, drag to
    // translate it. Suspended while a post-drag measurement
    // prompt is up — that prompt has to be Apply / Cycle /
    // Cancel first.
    if primary_available
        && matches!(edit_tool, EditTools::Move)
        && view.edit_drag_prompt.is_none()
        && view.vertex_drop_prompt.is_none()
    {
        // One transform for both the double-click vertex-insert and
        // the click/drag below (same storey, same frame).
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, false) {
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
}

/// The two floating canvas buttons, anchored to the top-right corner and
/// stacked vertically. Plain text labels for now — visual design deferred.
///
/// - **2D/3D toggle**: shows the current view ("2D"/"3D"); a click switches
///   to the other. Disabled until the current scope has its ceiling
///   height(s) — heights are entered via the Heights category, and the
///   controller's `set_sketch_mode` re-enforces the gate as a no-op.
/// - **Display mode**: shows the current [`DisplayMode`]'s name; a click
///   CYCLES to the next variant (wrapping).
fn view_buttons<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view: &SketchView<'_>,
    on_event: &mut F,
) {
    egui::Area::new(ui.id().with("canvas_view_buttons"))
        .fixed_pos(rect.right_top() + egui::vec2(-110.0, 8.0))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            ui.vertical(|ui| {
                let (label, other) = match view.mode {
                    SketchMode::Storey2D => ("2D", SketchMode::Storey3D),
                    SketchMode::Storey3D => ("3D", SketchMode::Storey2D),
                };
                // The gate is on ENTERING 3D only — going back to 2D is
                // always allowed (otherwise switching onto a fresh storey
                // while in 3D would trap the user there). Mirrors the
                // controller guard, which only refuses Storey3D.
                let enabled = matches!(view.mode, SketchMode::Storey3D) || view.ceiling_heights_set;
                if ui
                    .add_enabled(enabled, egui::Button::new(label))
                    .on_hover_text("Toggle between the 2D plan and the 3D view")
                    .on_disabled_hover_text("Set a default ceiling height (Heights) first")
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeMode(other)));
                }
                // Cycle to the variant after the current one, wrapping.
                let next_display = {
                    let mut iter = DisplayMode::iter().cycle();
                    iter.find(|d| *d == view.display);
                    iter.next().expect("cycle() never ends")
                };
                if ui
                    .button(format!("{:?}", view.display))
                    .on_hover_text("Cycle what the canvas shows (dims / roofs / walls / floors)")
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeDisplay(next_display)));
                }
            });
        });
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

fn draw_stroke(painter: &egui::Painter, rect: egui::Rect, stroke: &EguiStroke) {
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
