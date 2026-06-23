//! The Sketch CANVAS: paints the plan + every overlay, routes pointer input
//! (strokes, taps, drags, pan/zoom) into [`SketchEvent`]s, and hosts the
//! floating 2D/3D toggle in its top-right corner.

use egui::epaint::PathShape;

use super::touch::TouchNav;
use super::{ArmedSurfaceExposure, EguiStroke, SketchEvent, SketchView};
use crate::controller::app_controller::{AssessmentPhase, SelectMode};
use crate::ui::pages::events::UiEvent;
use crate::ui::render;
use crate::ui::render::ViewParams;
use crate::ui::widgets::HintExt;
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::structural::{CeilingExposure, FloorExposure};
use domain::sketch::{EditTools, SketchMode};

/// Faint fill tint for a floor region by its boundary condition (Unset = grey,
/// so unmarked surfaces read as "to do").
fn floor_exposure_tint(e: FloorExposure) -> egui::Color32 {
    match e {
        FloorExposure::Unset => egui::Color32::from_rgba_unmultiplied(140, 140, 140, 70),
        FloorExposure::Ground => egui::Color32::from_rgba_unmultiplied(150, 110, 60, 90),
        FloorExposure::Exposed => egui::Color32::from_rgba_unmultiplied(220, 80, 60, 90),
        FloorExposure::SemiExposed => egui::Color32::from_rgba_unmultiplied(220, 160, 60, 90),
        FloorExposure::NonHeatLoss => egui::Color32::from_rgba_unmultiplied(80, 160, 220, 90),
    }
}

/// Outline tint for a ceiling region by its boundary condition.
fn ceiling_exposure_tint(e: CeilingExposure) -> egui::Color32 {
    match e {
        CeilingExposure::Unset => egui::Color32::from_rgb(150, 150, 150),
        CeilingExposure::ExposedRoof => egui::Color32::from_rgb(230, 90, 70),
        CeilingExposure::Party => egui::Color32::from_rgb(90, 170, 230),
        CeilingExposure::Unheated => egui::Color32::from_rgb(230, 170, 70),
    }
}

/// Estimated width an in-place "Edit attributes" window occupies, and the
/// vertical slot each is stacked into. egui sizes the windows lazily, so the
/// layout reserves space from these estimates; the slot is generous enough that
/// stacked windows don't overlap, and the window's own content is capped to
/// `EDIT_BOX_W` so it stays narrow (construction type is a dropdown).
const EDIT_BOX_W: f32 = 165.0;
const EDIT_SLOT_H: f32 = 155.0;
/// Gap between a window column and the canvas edge.
const EDIT_COL_PAD: f32 = 10.0;
/// Clear gap kept BETWEEN adjacent floaties along an edge, so the de-clutter
/// pitch exceeds the window box and they never touch/overlap.
const EDIT_GAP: f32 = 12.0;
/// Top/bottom inset reserved for the window columns.
const EDIT_TOP_PAD: f32 = 12.0;
/// Clearance kept below the canvas top for the phase signpost + surface banner
/// (the floating bar at `center_top + 8` and the banner just under it), so the
/// top row of attribute windows and the side columns don't sit on top of them.
const EDIT_TOP_SAFE: f32 = 84.0;

/// One in-place attribute window's placement (see [`compute_structure_edit_layout`]).
struct EditWindow {
    group_index: usize,
    /// Top-left of the floating `Area` (screen px).
    pos: egui::Pos2,
    /// Where the connector leaves the window — its inner-edge midpoint (screen px).
    inner_anchor: egui::Pos2,
    /// The group's true plan centroid (world) — the connector's target.
    centroid: (f64, f64),
    color: egui::Color32,
}

/// The in-place "Edit attributes" layout: the per-group windows distributed to
/// the canvas margins (nearest edge to each group), plus the view that fits the
/// plan into the gap left in the middle.
struct StructureEditLayout {
    view: ViewParams,
    windows: Vec<EditWindow>,
}

/// Place items (window top-left coords, given in their PREFERRED order) along one
/// axis with no overlap: a forward pass pushes each to at least `spacing` past
/// the previous (and ≥ `lo`), a backward pass pulls any that overran `hi` back
/// (keeping `spacing`). The result keeps each near its preferred spot while
/// staying inside `[lo, hi]`; if there are too many to fit they compress evenly.
fn declutter_axis(prefs: &mut [f32], spacing: f32, lo: f32, hi: f32) {
    let n = prefs.len();
    for i in 0..n {
        let floor = if i == 0 { lo } else { prefs[i - 1] + spacing };
        if prefs[i] < floor {
            prefs[i] = floor;
        }
    }
    for i in (0..n).rev() {
        let ceil = if i + 1 == n {
            hi
        } else {
            prefs[i + 1] - spacing
        };
        if prefs[i] > ceil {
            prefs[i] = ceil;
        }
    }
    // The backward pass can push the leftmost items below `lo` when there are
    // more than fit; clamp so nothing ever lands off-screen (they overlap-compress
    // at the `lo` end instead of running off the canvas).
    for p in prefs.iter_mut() {
        *p = p.clamp(lo, hi);
    }
}

/// Lay the in-place attribute windows into the canvas MARGINS, then fit the plan
/// into the gap left over — "place the windows first, then zoom the plan to fill
/// what's left". Each group's window goes to whichever canvas EDGE its group is
/// nearest (top / bottom / left / right) and is positioned along that edge close
/// to the group's own spot (de-cluttered so windows on the same edge don't
/// overlap). Spreading across all four margins — rather than forcing every window
/// into one pair of gutters — keeps each connector short and roughly radial and
/// keeps the plan large in the centre. Corners are kept clear by confining the
/// side columns to the un-reserved height and the top/bottom rows to the
/// un-reserved width. `None` when there's no fittable plan or the canvas is too
/// small, leaving the caller on the normal view.
fn compute_structure_edit_layout(
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    storey: &ber_model::ber::structural::Storey,
    edit: &crate::controller::app_controller::StructureEdit,
    base: ViewParams,
) -> Option<StructureEditLayout> {
    let i = current_storey_idx?;
    let ber = ber?;
    let (min_xy, max_xy) = render::fit_bbox_for_storey(ber, i)?;
    let fans = render::structure_fan::group_fan_layout(storey, edit);
    if fans.is_empty() {
        return None;
    }

    let col_w = EDIT_BOX_W + 2.0 * EDIT_COL_PAD;
    let band_h = EDIT_SLOT_H + 2.0 * EDIT_TOP_PAD;

    // Each group's on-screen position under the plain full-rect fit — picks which
    // side of the plan each window goes to and where along that edge it sits.
    let t_full = render::fit_transform(rect, min_xy, max_xy, base);
    let screen: Vec<egui::Pos2> = fans
        .iter()
        .map(|f| t_full.apply(f.centroid.0, f.centroid.1))
        .collect();
    // The connector TARGET per group (walls → nearest selected wall, else the
    // centroid). Indexed by fan; assignment/ordering still uses the centroid.
    let anchors: Vec<(f64, f64)> = fans
        .iter()
        .map(|f| {
            render::structure_fan::group_connector_anchor(
                storey,
                &edit.groups[f.group_index],
                f.centroid,
            )
        })
        .collect();

    // Assign each group to a side by WHERE IT SITS RELATIVE TO THE PLAN'S OWN
    // CENTRE (its quadrant), not the nearest canvas edge — otherwise a
    // letterboxed plan (wide → tall side gaps, or tall → wide top/bottom gaps)
    // pushes every group onto the same axis and we're back to two-sided.
    //
    // But the quadrant is only a PREFERENCE: an edge that can't fit all the
    // groups that want it would just overlap-compress them while other edges sit
    // empty. So each edge has a capacity, and groups are placed onto their most
    // preferred edge that still has room — decisive groups (clearly on one side)
    // claim their edge first, and ones near a corner spill to a perpendicular
    // edge that's free. Edge codes: 0 = left, 1 = right, 2 = top, 3 = bottom.
    let plan_centre = t_full.apply((min_xy.0 + max_xy.0) * 0.5, (min_xy.1 + max_xy.1) * 0.5);
    // Rough per-edge capacities from the full canvas (the exact reserved margins
    // depend on the assignment we're computing, so estimate from the whole rect).
    let col_pitch = EDIT_SLOT_H + EDIT_GAP;
    let row_pitch = EDIT_BOX_W + EDIT_GAP;
    let cap_v =
        (((rect.height() - EDIT_TOP_SAFE - EDIT_TOP_PAD) / col_pitch).floor() as usize).max(1);
    let cap_h = (((rect.width() - 2.0 * EDIT_COL_PAD) / row_pitch).floor() as usize).max(1);
    let cap = [cap_v, cap_v, cap_h, cap_h];

    // Each group's edge preference order (most→least wanted) + how decisively it
    // belongs to its first choice (bigger = more one-sided, less corner-like).
    let pref_and_decisiveness = |p: egui::Pos2| -> ([usize; 4], f32) {
        let (dx, dy) = (p.x - plan_centre.x, p.y - plan_centre.y);
        let (h, opp_h) = if dx < 0.0 { (0, 1) } else { (1, 0) };
        let (v, opp_v) = if dy < 0.0 { (2, 3) } else { (3, 2) };
        let prefs = if dx.abs() >= dy.abs() {
            [h, v, opp_v, opp_h]
        } else {
            [v, h, opp_h, opp_v]
        };
        (prefs, (dx.abs() - dy.abs()).abs())
    };

    // Decisive groups first, so corner-ish ones are the ones that spill over.
    let mut order: Vec<usize> = (0..fans.len()).collect();
    order.sort_by(|&a, &b| {
        let da = pref_and_decisiveness(screen[a]).1;
        let db = pref_and_decisiveness(screen[b]).1;
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });

    let (mut left, mut right, mut top, mut bottom): (
        Vec<usize>,
        Vec<usize>,
        Vec<usize>,
        Vec<usize>,
    ) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut counts = [0usize; 4];
    for k in order {
        let (prefs, _) = pref_and_decisiveness(screen[k]);
        // First preferred edge with spare room; if all are full, fall back to the
        // first choice (an unavoidable overlap, but only once every edge is full).
        let edge = prefs
            .into_iter()
            .find(|&e| counts[e] < cap[e])
            .unwrap_or(prefs[0]);
        counts[edge] += 1;
        match edge {
            0 => left.push(k),
            1 => right.push(k),
            2 => top.push(k),
            _ => bottom.push(k),
        }
    }

    // Reserve a margin only on edges that actually hold a window.
    let left_w = if left.is_empty() { EDIT_COL_PAD } else { col_w };
    let right_w = if right.is_empty() {
        EDIT_COL_PAD
    } else {
        col_w
    };
    // A top row sits BELOW the signpost, so the plan must clear the signpost band
    // PLUS the row height; the bottom has no overlay, so a normal band suffices.
    let top_h = if top.is_empty() {
        EDIT_TOP_PAD
    } else {
        EDIT_TOP_SAFE + EDIT_SLOT_H + EDIT_TOP_PAD
    };
    let bottom_h = if bottom.is_empty() {
        EDIT_TOP_PAD
    } else {
        band_h
    };
    let inner = egui::Rect::from_min_max(
        egui::pos2(rect.left() + left_w, rect.top() + top_h),
        egui::pos2(rect.right() - right_w, rect.bottom() - bottom_h),
    );
    if inner.width() < 60.0 || inner.height() < 60.0 {
        return None;
    }

    // Fit the plan into `inner`: zoom relative to the full-rect auto-fit, offset
    // to recentre on the gap between the reserved margins.
    let base_scale = render::auto_fit_scale(rect, min_xy, max_xy);
    let inner_scale = render::auto_fit_scale(inner, min_xy, max_xy);
    let view = ViewParams {
        zoom: (inner_scale / base_scale) as f32,
        offset_x: inner.center().x - rect.center().x,
        offset_y: inner.center().y - rect.center().y,
        yaw_deg: base.yaw_deg,
        pitch_deg: base.pitch_deg,
    };

    let mut windows = Vec::with_capacity(fans.len());

    // LEFT / RIGHT columns: window y tracks the group's screen y. Columns flank
    // the plan for the full height (below the signpost), and never collide with
    // the top/bottom rows because those are confined to the inner WIDTH — see
    // `row_lo`/`row_hi` below — which excludes the column x-ranges.
    let col_lo = rect.top() + EDIT_TOP_SAFE;
    let col_hi = (rect.bottom() - EDIT_TOP_PAD - EDIT_SLOT_H).max(col_lo);
    for (col, left_side) in [(&mut left, true), (&mut right, false)] {
        col.sort_by(|&a, &b| {
            screen[a]
                .y
                .partial_cmp(&screen[b].y)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut ys: Vec<f32> = col
            .iter()
            .map(|&k| screen[k].y - EDIT_SLOT_H * 0.5)
            .collect();
        declutter_axis(&mut ys, col_pitch, col_lo, col_hi);
        let x = if left_side {
            rect.left() + EDIT_COL_PAD
        } else {
            rect.right() - EDIT_COL_PAD - EDIT_BOX_W
        };
        for (slot, &k) in col.iter().enumerate() {
            let y = ys[slot];
            let inner_anchor = if left_side {
                egui::pos2(x + EDIT_BOX_W, y + EDIT_SLOT_H * 0.5)
            } else {
                egui::pos2(x, y + EDIT_SLOT_H * 0.5)
            };
            windows.push(EditWindow {
                group_index: fans[k].group_index,
                pos: egui::pos2(x, y),
                inner_anchor,
                centroid: anchors[k],
                color: fans[k].color,
            });
        }
    }

    // TOP / BOTTOM rows: window x tracks the group's screen x, confined to the
    // width not reserved by the left/right columns.
    let row_lo = rect.left() + left_w;
    let row_hi = (rect.right() - right_w - EDIT_BOX_W).max(row_lo);
    for (row, top_side) in [(&mut top, true), (&mut bottom, false)] {
        row.sort_by(|&a, &b| {
            screen[a]
                .x
                .partial_cmp(&screen[b].x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut xs: Vec<f32> = row
            .iter()
            .map(|&k| screen[k].x - EDIT_BOX_W * 0.5)
            .collect();
        declutter_axis(&mut xs, row_pitch, row_lo, row_hi);
        let y = if top_side {
            rect.top() + EDIT_TOP_SAFE // clear the phase signpost / banner
        } else {
            rect.bottom() - EDIT_TOP_PAD - EDIT_SLOT_H
        };
        for (slot, &k) in row.iter().enumerate() {
            let x = xs[slot];
            let inner_anchor = if top_side {
                egui::pos2(x + EDIT_BOX_W * 0.5, y + EDIT_SLOT_H)
            } else {
                egui::pos2(x + EDIT_BOX_W * 0.5, y)
            };
            windows.push(EditWindow {
                group_index: fans[k].group_index,
                pos: egui::pos2(x, y),
                inner_anchor,
                centroid: anchors[k],
                color: fans[k].color,
            });
        }
    }

    Some(StructureEditLayout { view, windows })
}

pub(super) fn render<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    view: &SketchView<'_>,
    touch: &mut TouchNav,
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

    // Elevation editor: when a wall's elevation is open it REPLACES the plan
    // canvas — the unrolled wall with draggable openings. Owns its own input.
    if let Some(elev) = view.elevation_view.as_ref() {
        elevation_overlay(ui, rect, &response, elev, vp, on_event);
        return;
    }

    // Structure "Edit attributes" (in-place view): lay the per-group windows out
    // in the margins (each on the side nearest its group), then fit the plan into
    // the gap they leave.
    // The fitted view REPLACES the normal pan/zoom for the whole render so the
    // plan, its overlays, and the windows all share one transform. `None` (no
    // session, the exploded view, 3D, or a canvas too small) leaves `vp` as-is.
    let structure_edit_layout = view
        .structure_edit
        .filter(|e| !e.exploded && matches!(mode, SketchMode::Storey2D))
        .and_then(|edit| {
            let storey = ber.and_then(|b| current_storey_idx.and_then(|i| b.storeys.get(i)))?;
            compute_structure_edit_layout(rect, ber, current_storey_idx, storey, edit, vp)
        });
    let vp = structure_edit_layout.as_ref().map(|l| l.view).unwrap_or(vp);

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(30));

    // Floating view buttons in the canvas's top-right corner (an egui Area on
    // the Foreground order, so they sit above the canvas and own their
    // clicks — same pattern as the snap-tag menus).
    view_buttons(ui, rect, view, on_event);

    // Floating phase signpost (top-centre) + the live per-phase completion
    // checklist (a draggable window), both painted over the canvas. The
    // signpost is clickable: clicking a reachable box switches the workspace
    // phase. Structure unlocks once Mapping is complete (every checklist item
    // green on all storeys); Objects isn't built yet.
    if let Some(phase) =
        super::overlays::phase_bar(ui, rect, view.current_phase, view.mapping_complete)
    {
        on_event(UiEvent::Sketch(SketchEvent::SetAssessmentPhase(phase)));
    }
    super::overlays::checklist_window(ui, rect, view.current_phase, &view.checklist);

    // Structure phase: when a surface kind is selected for highlighting, name it
    // (+ its count) in a banner under the signpost so it's explicit which
    // surfaces the shaded faces on the canvas are.
    super::overlays::highlight_caption(ui, rect, view.display, view.surface_counts);

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
        view.structure_selected,
        view.structure_edit,
    );

    // Structure "Edit attributes" — EXPLODED view only: place a small group-picker
    // popup at each group's pulled-out position (clicking makes that group active;
    // the side panel edits it). The DEFAULT in-place windows are drawn later, on
    // top of the wall-exposure layer, by `structure_edit_overlay`.
    if let (Some(edit), Some(storey)) = (
        view.structure_edit,
        ber.and_then(|b| current_storey_idx.and_then(|i| b.storeys.get(i))),
    ) {
        if edit.exploded && matches!(mode, SketchMode::Storey2D) {
            if let Some(t) = render::structure_fan::fan_transform(rect, storey, vp) {
                for fan in render::structure_fan::group_fan_layout(storey, edit) {
                    let (wx, wy) = fan.fanned_centroid();
                    let at = t.apply(wx, wy);
                    let active = fan.group_index == edit.active;
                    egui::Area::new(ui.id().with(("structure_fan", fan.group_index)))
                        .order(egui::Order::Foreground)
                        .fixed_pos(at)
                        .show(ui.ctx(), |ui| {
                            egui::Frame::popup(ui.style())
                                .fill(if active {
                                    fan.color.gamma_multiply(0.5)
                                } else {
                                    ui.style().visuals.window_fill
                                })
                                .show(ui, |ui| {
                                    let label =
                                        format!("Group {}  ·  {}", fan.group_index + 1, fan.count);
                                    if ui.add(egui::Button::selectable(active, label)).clicked() {
                                        on_event(UiEvent::Sketch(
                                            SketchEvent::SetActiveStructureGroup(fan.group_index),
                                        ));
                                    }
                                });
                        });
                }
            }
        }
    }

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
    // Structure-phase carve: the same two-candidate highlight while picking
    // which region's surfaces to select.
    render::render_cut_candidates(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.structure_cut_regions,
    );
    // Structure split: the in-flight ring, highlighted while it's being
    // dimensioned (same treatment as the construction-phase pending area).
    if view.pending_structure_split_polygon.len() >= 3 {
        render::render_phase_regions(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            std::slice::from_ref(&(
                view.pending_structure_split_polygon.to_vec(),
                "?".to_string(),
            )),
            egui::Color32::from_rgb(120, 200, 255), // blue: structure split
        );
    }
    // Staged carve rings: the pending SELECTION (not yet split). Shaded green so
    // the assessor sees what's selected; the geometry only splits at "Edit
    // attributes".
    if !view.pending_structure_carves.is_empty() {
        let labelled: Vec<(Vec<(f64, f64)>, String)> = view
            .pending_structure_carves
            .iter()
            .map(|ring| (ring.clone(), String::new()))
            .collect();
        render::render_phase_regions(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            &labelled,
            egui::Color32::from_rgb(120, 210, 130), // green: pending selection
        );
    }

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
                                    ui.close();
                                }
                            }
                            if tag.skippable && ui.button("Don't snap").clicked() {
                                on_event(UiEvent::Sketch(SketchEvent::ChooseSnap {
                                    from: tag.from,
                                    choice: None,
                                }));
                                ui.close();
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
    // Same-height dividers: a faint dashed pass, distinct from the amber
    // height steps (divider regions are filtered out of the amber feed above).
    render::render_ceiling_dividers(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.ceiling_divider_lines,
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
                            ui.close();
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
                            ui.close();
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

    // Roof RECTS (flat tops): committed four-apex rect groups as solid
    // labelled outlines, plus the pending rect as a dashed outline — the
    // ridge family's closed-stroke sibling, same violet-blue.
    render::render_roof_rects(
        &painter,
        rect,
        ber,
        current_storey_idx,
        mode,
        vp,
        view.roof_rect_views,
        view.pending_roof_rect_outline,
    );

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
                            ui.close();
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

    // Horizontal-surface exposure: tint each floor/ceiling region by its current
    // boundary condition so the assessor sees which surfaces still need marking.
    // Floors fill solid (faint); ceilings outline (so a region shows both).
    if view.surface_exposure_mode && !view.surface_exposure_views.is_empty() {
        if let Some((_s, t)) = render::storey_transform(rect, ber, current_storey_idx, mode, vp) {
            for sv in view.surface_exposure_views {
                if sv.ring.len() < 3 {
                    continue;
                }
                let pts: Vec<egui::Pos2> = sv.ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
                if let Some(e) = sv.floor {
                    let c = floor_exposure_tint(e);
                    painter.add(egui::Shape::convex_polygon(
                        pts.clone(),
                        c,
                        egui::Stroke::NONE,
                    ));
                }
                if let Some(e) = sv.ceiling {
                    let c = ceiling_exposure_tint(e);
                    painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(2.0, c)));
                }
            }
        }
    }

    // Structure "Edit attributes" (in-place): now the wall-exposure colours are
    // down, recolour each group's surfaces in its group colour ON TOP (so the
    // selection is unmistakable over exposure), draw a connector from each group
    // to its window, and float the per-group attribute editors in their columns.
    if let (Some(layout), Some(edit), Some(storey)) = (
        structure_edit_layout.as_ref(),
        view.structure_edit,
        ber.and_then(|b| current_storey_idx.and_then(|i| b.storeys.get(i))),
    ) {
        if let Some((_s, t)) = render::storey_transform(rect, ber, current_storey_idx, mode, vp) {
            render::structure_fan::render_groups_inplace(&painter, &t, storey, edit);
            let editable = !reviewing && !dim_prompting;
            for w in &layout.windows {
                let i = w.group_index;
                let Some(group) = edit.groups.get(i) else {
                    continue;
                };
                // Connector from the window's inner edge to the group's true
                // centroid, ending in a dot on the recoloured surfaces.
                let target = t.apply(w.centroid.0, w.centroid.1);
                painter.line_segment([w.inner_anchor, target], egui::Stroke::new(2.0, w.color));
                painter.circle_filled(target, 3.5, w.color);
                egui::Area::new(ui.id().with(("structure_attr", i)))
                    .order(egui::Order::Foreground)
                    .fixed_pos(w.pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(w.color.gamma_multiply(0.22))
                            .stroke(egui::Stroke::new(2.0, w.color))
                            .show(ui, |ui| {
                                ui.set_max_width(EDIT_BOX_W - 16.0);
                                // The two-level heading (kind + editable name) is
                                // rendered by `group_editor_ui` itself now.
                                super::structure::group_editor_ui(
                                    ui,
                                    group.ty.kind(),
                                    i,
                                    group,
                                    view.phase_choices,
                                    editable,
                                    on_event,
                                );
                            });
                    });
            }
        }
    }

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
            use manip_plan::ParameterAxis;
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
            manip_plan::ParameterAxis::X => {
                // Horizontal arrow at the area wall's midpoint y.
                let mid_y = (pp.area_wall.wall_a.1 + pp.area_wall.wall_b.1) * 0.5;
                (
                    (pp.reference_wall.wall_a.0, mid_y),
                    (pp.area_wall.wall_a.0, mid_y),
                )
            }
            manip_plan::ParameterAxis::Y => {
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

    // Two-finger navigation (touch): pinch zoom + two-finger pan, with
    // retraction of a stroke begun before the second finger landed. While a
    // gesture is active (until EVERY finger lifts) all primary-drag input
    // below — strokes, Move drags, exposure drags, 3D orbit — is suppressed.
    let touch_nav_active = touch.handle(ui, rect, view.current.is_some(), on_event);

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
    if matches!(mode, SketchMode::Storey3D)
        && !touch_nav_active
        && response.dragged_by(egui::PointerButton::Primary)
    {
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
        && !view.roof_rect_pending
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

    // Structure phase: the canvas has its OWN tools. In the CLICK select mode a
    // primary tap (2D) toggles the surface under it in/out of the selection;
    // the CARVE mode instead captures a stroke (handled in the stroke path
    // below). All Mapping primary input is gated off (`primary_available`).
    let structure_phase = view.current_phase == AssessmentPhase::Structure;
    let structure_click =
        structure_phase && matches!(view.structure_select, Some(SelectMode::Click));
    let structure_carve =
        structure_phase && matches!(view.structure_select, Some(SelectMode::Carve));
    if structure_click && matches!(mode, SketchMode::Storey2D) && !touch_nav_active {
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, false) {
            let tol_world = (12.0 / t.scale()) as f64;
            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::StructureSurfaceTap {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
        }
    }

    // Structure CARVE mode (2D): while a cut is awaiting a pick, a tap chooses
    // which region's surfaces to select; otherwise a primary drag draws the
    // carve stroke (captured in pixels, converted to world on release — the
    // same path living-area cuts use), then routed to the carve handler.
    let structure_cut_picking = !view.structure_cut_regions.is_empty();
    if structure_carve && matches!(mode, SketchMode::Storey2D) && !touch_nav_active {
        if structure_cut_picking {
            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some((wx, wy)) = response
                    .interact_pointer_pos()
                    .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
                {
                    on_event(UiEvent::Sketch(SketchEvent::ChooseStructureCutRegion {
                        world_x: wx,
                        world_y: wy,
                    }));
                }
            }
        } else {
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    on_event(UiEvent::Sketch(SketchEvent::StartStroke(
                        pos - rect.left_top().to_vec2(),
                    )));
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    on_event(UiEvent::Sketch(SketchEvent::AddPoint(
                        pos - rect.left_top().to_vec2(),
                    )));
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                if let (Some(stroke), Some(t)) = (
                    view.current.as_ref(),
                    canvas_transform(ber, current_storey_idx, rect, vp, false),
                ) {
                    let topleft = rect.left_top();
                    let world_points: Vec<(f32, f32)> = stroke
                        .points
                        .iter()
                        .map(|p| {
                            let (wx, wy) = t.inverse(p.x + topleft.x, p.y + topleft.y);
                            (wx as f32, wy as f32)
                        })
                        .collect();
                    on_event(UiEvent::Sketch(SketchEvent::StructureCarveStrokeWorld {
                        points: world_points,
                    }));
                } else {
                    on_event(UiEvent::Sketch(SketchEvent::CancelStroke));
                }
            }
            if let Some(stroke) = &view.current {
                draw_stroke(&painter, rect, stroke);
            }
        }
    }

    // Exposure mode: a tap selects the wall under the pointer; a
    // primary drag near a selection handle slides that handle along
    // the wall. Takes priority over stroke/edit input (suppressed via
    // `primary_available` above).
    if view.exposure_mode && !cut_pending && !touch_nav_active {
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

    // Surface-exposure mode: a tap applies the SINGLE armed boundary condition
    // (floor XOR ceiling) to ONLY the matching surface kind — never both — so the
    // tap can't clobber the other surface's (auto-derived) value. The region
    // under the tap is resolved controller-side
    // (`set_floor_exposure_at` / `set_ceiling_exposure_at`); a no-op if no region
    // of that kind is under the point.
    if view.surface_exposure_mode && !cut_pending && !touch_nav_active {
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, false) {
            let tap = response.clicked_by(egui::PointerButton::Primary)
                || response.drag_started_by(egui::PointerButton::Primary);
            if tap {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    let ev = match *view.armed_surface_exposure {
                        ArmedSurfaceExposure::Floor(exposure) => SketchEvent::SetFloorExposure {
                            world_x: wx,
                            world_y: wy,
                            exposure,
                        },
                        ArmedSurfaceExposure::Ceiling(exposure) => {
                            SketchEvent::SetCeilingExposure {
                                world_x: wx,
                                world_y: wy,
                                exposure,
                            }
                        }
                    };
                    on_event(UiEvent::Sketch(ev));
                }
            }
        }
    }

    // Openings mode (no elevation open): a tap near a wall drops the selected
    // opening type on the nearest wall (or opens the wall's elevation in Edit).
    // The elevation overlay, when open, captures its own input (see the
    // elevation render path), so plan taps are gated on it being closed.
    if view.openings_mode && view.elevation_view.is_none() && !cut_pending && !touch_nav_active {
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, false) {
            let tol_world = (14.0 / t.scale()) as f64;
            let tap = response.clicked_by(egui::PointerButton::Primary)
                || response.drag_started_by(egui::PointerButton::Primary);
            if tap {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::OpeningsPointerDown {
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
        && !view.roof_rect_pending
        && !view.exposure_mode
        && !view.surface_exposure_mode
        && !view.openings_mode
        // Structure phase owns the canvas with its own selection tools.
        && !structure_phase;

    if primary_available && !touch_nav_active && !matches!(edit_tool, EditTools::Move) {
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
                        log::warn!(
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
        && !touch_nav_active
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

/// The floating 2D/3D toggle, anchored to the canvas's top-right corner. Plain
/// text label for now — visual design deferred. Shows the current view
/// ("2D"/"3D"); a click switches to the other. Disabled until the current
/// scope has its ceiling height(s) — heights are entered via the Heights
/// category, and the controller's `set_sketch_mode` re-enforces the gate as a
/// no-op.
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
                    .hint("Toggle between the 2D plan and the 3D view")
                    .disabled_hint("Set a default ceiling height (Heights) first")
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeMode(other)));
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

/// Render the per-wall ELEVATION editor: the unrolled wall (base line + roof
/// top profile) with each opening as a draggable rectangle. Drags reposition
/// (horizontal → along, vertical → sill); sizes are fixed. The panel owns its
/// own primary-button input, emitting `Elevation*` events.
fn elevation_overlay<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    elev: &crate::controller::app_controller::ElevationView,
    vp: render::ViewParams,
    on_event: &mut F,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(24));

    let len = (elev.wall_len_m as f64).max(0.1);
    let base = elev.base_z as f64;
    let top_max = elev
        .top_profile
        .iter()
        .map(|(_, z)| *z as f64)
        .fold(base + 0.5, f64::max);
    // World frame: x = along (0..len), y = -z (so height grows UP on screen),
    // with a small margin so the wall sits off the panel edges.
    let margin = (top_max - base).max(0.5) * 0.12;
    let t = render::fit_transform(
        rect,
        (-len * 0.05, -(top_max + margin)),
        (len * 1.05, -(base - margin)),
        vp,
    );
    let p = |along: f64, z: f64| t.apply(along, -z);
    let wall_col = egui::Color32::from_gray(150);
    let line = |a: egui::Pos2, b: egui::Pos2, w: f32, c: egui::Color32| {
        painter.line_segment([a, b], egui::Stroke::new(w, c));
    };
    // Base, top profile, and the two side edges connecting them.
    line(p(0.0, base), p(len, base), 2.0, wall_col);
    if elev.top_profile.len() >= 2 {
        for w in elev.top_profile.windows(2) {
            line(
                p(w[0].0 as f64, w[0].1 as f64),
                p(w[1].0 as f64, w[1].1 as f64),
                2.0,
                wall_col,
            );
        }
        // The profile runs b→a (along len→0); join the base ends to it.
        let first = elev.top_profile.first().unwrap();
        let last = elev.top_profile.last().unwrap();
        line(
            p(len, base),
            p(first.0 as f64, first.1 as f64),
            2.0,
            wall_col,
        );
        line(p(0.0, base), p(last.0 as f64, last.1 as f64), 2.0, wall_col);
    } else {
        line(p(0.0, base), p(0.0, top_max), 2.0, wall_col);
        line(p(len, base), p(len, top_max), 2.0, wall_col);
        line(p(0.0, top_max), p(len, top_max), 2.0, wall_col);
    }
    // Openings as filled rectangles with a (heavier when selected) border.
    for o in &elev.openings {
        let l = (o.along_m - o.width_m / 2.0) as f64;
        let r = (o.along_m + o.width_m / 2.0) as f64;
        let b = base + o.sill_m as f64;
        let tp = b + o.height_m as f64;
        let ro = egui::Rect::from_two_pos(p(l, b), p(r, tp));
        let (fill, stroke) = elevation_colors(o.kind, o.selected);
        painter.rect_filled(ro, 1.0, fill);
        let sw = if o.selected { 2.5 } else { 1.5 };
        line(ro.left_top(), ro.right_top(), sw, stroke);
        line(ro.right_top(), ro.right_bottom(), sw, stroke);
        line(ro.right_bottom(), ro.left_bottom(), sw, stroke);
        line(ro.left_bottom(), ro.left_top(), sw, stroke);
    }
    painter.text(
        rect.left_top() + egui::vec2(8.0, 8.0),
        egui::Align2::LEFT_TOP,
        "Elevation — drag openings to reposition (sizes locked). Done in the toolbar.",
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(160),
    );
    // Primary input → wall-local (along, height-above-base).
    let to_local = |pos: egui::Pos2| {
        let (wx, wy) = t.inverse(pos.x, pos.y);
        (wx as f32, ((-wy) - base) as f32)
    };
    let emit_down = |on_event: &mut F, pos: egui::Pos2| {
        let (a, h) = to_local(pos);
        on_event(UiEvent::Sketch(SketchEvent::ElevationPointerDown {
            along_m: a,
            height_m: h,
        }));
    };
    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            emit_down(on_event, pos);
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            let (a, h) = to_local(pos);
            on_event(UiEvent::Sketch(SketchEvent::ElevationPointerDrag {
                along_m: a,
                height_m: h,
            }));
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        on_event(UiEvent::Sketch(SketchEvent::ElevationPointerUp));
    }
    if response.clicked_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            emit_down(on_event, pos);
        }
    }
}

/// Fill + border colour for an opening rectangle in the elevation (glass blue /
/// door brown; brighter + white-bordered when selected).
fn elevation_colors(
    kind: crate::controller::app_controller::OpeningViewKind,
    selected: bool,
) -> (egui::Color32, egui::Color32) {
    use crate::controller::app_controller::OpeningViewKind::*;
    let base = match kind {
        Window | RoofWindow | SidelightWindow => egui::Color32::from_rgb(90, 160, 220),
        Door | DoorPanel => egui::Color32::from_rgb(180, 130, 80),
    };
    let a = if selected { 130 } else { 70 };
    let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a);
    let stroke = if selected { egui::Color32::WHITE } else { base };
    (fill, stroke)
}
