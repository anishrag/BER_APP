//! Renders the persisted BER state as 2D / 3D views in the sketch canvas.
//!
//! Four rendering paths, selected by [`SketchMode`] × current-storey:
//!   - `Storey2D`, `Some(i)`  — top-down 2D plan of storey `i`
//!   - `Storey3D`, `Some(i)`  — axonometric 3D of storey `i`
//!   - `Storey2D`, `None`     — side-by-side row of all storeys in 2D
//!   - `Storey3D`, `None`     — axonometric 3D of all storeys stacked
//!
//! Each view honours [`DisplayMode`] by highlighting one surface category and
//! fading the others:
//!   - `FloorMapDims` — show dimensions labels (default — best with `Storey2D`)
//!   - `Floors`      — highlight floor polygons
//!   - `Walls`       — highlight wall edges
//!   - `Ceilings`       — highlight ceiling regions / roof-height labels
//!   - `AllSurfaces` — render every category at full opacity
//!
//! Rendering reads from a `Ber` clone supplied by the controller, so the user
//! sees what's actually persisted to SQLite, not in-flight sketch state.
//!
//! **Units caveat:** geometry is currently stored in canvas pixel coordinates
//! (the sketch input is pixels and there's no scale-to-meters step yet). The
//! dimension labels print the raw polygon distance — they'll become meters
//! once the assessor sets a scale.

pub mod all_storeys_2d;
pub mod all_storeys_ceilings;
pub mod all_storeys_floors;
pub mod structure_fan;

// The drawing core — the 3D house/elevation renderers plus the projection,
// fit-transform, and palette machinery — lives in the `ber_render` workspace
// crate so BER_deap's report generator drives the SAME code headlessly.
// Re-exported here so in-app callers keep their `render::*` paths.
pub use ber_render::{
    auto_fit_scale, color_for, draw_dashed_segment, elevation, fill_polygon, fit_transform,
    full_house_3d, inset_ring, pick_3d, project_3d, storey_2d, storey_3d, style, union_bbox,
    Transform2d, ViewParams,
};

use std::collections::HashSet;

use crate::controller::app_controller::{
    ExposureSelection, PhaseDisplayRegion, StructureEdit, WallExposureView,
};
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::WallExposure;
use domain::sketch::{DisplayMode, SketchMode};
use eframe::egui;
use manip_plan::CollinearitySuggestion;

/// Render the appropriate BER view into `rect`. Silent no-op if `ber` is None.
/// `highlight` is the current collinearity-review suggestion (if any) — its
/// two candidate edges get a bright overlay so the user sees what they're
/// being asked about. Only applies to a single-storey 2D view.
///
/// `current_storey_idx == None` → All-storeys overview (2D row, or 3D stack).
pub fn render_ber(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    display_mode: DisplayMode,
    highlight: Option<&CollinearitySuggestion>,
    // Walls the user has explicitly dimensioned, as (ring_idx, edge_start)
    // tuples for the current storey. Other walls' length labels are suppressed.
    dimensioned_walls: &[(usize, usize)],
    view: ViewParams,
    // Structure-phase selected region ids — shaded on the current storey.
    selected: &HashSet<ItemId>,
    // The in-flight "Edit attributes" session. When set, the 2D storey switches
    // to the fanned group view instead of the normal shaded render.
    structure_edit: Option<&StructureEdit>,
    // Objects phase: render the REDUCED 2D plan (no ceilings / roof windows) and the
    // 3D hotspot markers below.
    objects_view: bool,
    // World-3D points to mark as hotspots in the 3D views (the Objects phase's
    // room pins). Empty otherwise.
    hotspots: &[(f64, f64, f64)],
) {
    let Some(ber) = ber else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No BER loaded — create one from the Home page.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    };

    match (sketch_mode, current_storey_idx) {
        (SketchMode::Storey2D, Some(i)) => {
            if let Some(storey) = ber.storeys.get(i) {
                // Watermark: on any upper storey, draw the previous storey's
                // outline as a faint guide — both while tracing the new outline
                // and afterwards while dimensioning it, so the assessor keeps
                // the storey-below footprint as a reference throughout (method 2).
                let watermark = if i > 0 {
                    ber.storeys.get(i - 1).and_then(previous_outline_ring)
                } else {
                    None
                };
                // While the fresh upper storey's outline is still being TRACED
                // (no floor regions yet — the same freshness condition as the
                // canvas's world-space outline capture), also ghost the storey
                // below's construction-phase zones: faint dashed purple rings
                // with the phase label, so the assessor can follow the "year"
                // divisions the stroke snaps to. Gone once the outline lands —
                // the dimensioning-phase watermark stays outline-only.
                let phase_watermarks: Vec<PhaseDisplayRegion> =
                    if i > 0 && storey.floors.regions.is_empty() {
                        // HOLES-AWARE (like the committed overlay): a donut zone
                        // below ghosts with its hole, its label anchored inside
                        // the material.
                        ber.storey_phase_zone_polygons(i - 1)
                            .into_iter()
                            .map(|z| PhaseDisplayRegion {
                                exterior: z.exterior.iter().map(|p| (p.x, p.y)).collect(),
                                holes: z
                                    .holes
                                    .iter()
                                    .map(|h| h.iter().map(|p| (p.x, p.y)).collect())
                                    .collect(),
                                label_pos: (z.label_anchor.x, z.label_anchor.y),
                                label: ber.construction_phases.display_label(&z.phase),
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                // An open "Edit attributes" session takes over the canvas. The
                // default (in-place) view keeps the plan where it sits — the
                // controller has zoomed it out — and recolours each group on top
                // of the normal render. The exploded `fan` view pulls each group
                // radially out instead (wired but not the default).
                match structure_edit {
                    Some(edit) if edit.exploded => {
                        structure_fan::render_structure_fan(painter, rect, storey, edit, view);
                    }
                    Some(_) => {
                        // In-place edit: draw the base plan with the selection
                        // highlight suppressed. The per-group colour overlay is
                        // painted by the canvas AFTER the wall-exposure layer (so
                        // it sits on top and stays prominent) — see
                        // `structure_fan::render_groups_inplace`.
                        let no_selection = std::collections::HashSet::new();
                        storey_2d::render(
                            painter,
                            rect,
                            storey,
                            display_mode,
                            highlight,
                            dimensioned_walls,
                            view,
                            watermark.as_deref(),
                            &phase_watermarks,
                            None,
                            &no_selection,
                            objects_view,
                            // Single-storey view: risers are drawn by
                            // `render_wall_exposure`, so don't double them here.
                            false,
                        );
                    }
                    None => {
                        storey_2d::render(
                            painter,
                            rect,
                            storey,
                            display_mode,
                            highlight,
                            dimensioned_walls,
                            view,
                            watermark.as_deref(),
                            &phase_watermarks,
                            None,
                            selected,
                            objects_view,
                            // Single-storey view: risers drawn by the exposure
                            // overlay — don't double them here.
                            false,
                        );
                    }
                }
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        (SketchMode::Storey3D, Some(i)) => {
            if let Some(storey) = ber.storeys.get(i) {
                storey_3d::render(
                    painter,
                    rect,
                    storey,
                    display_mode,
                    view,
                    selected,
                    hotspots,
                );
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        (SketchMode::Storey2D, None) => {
            // Mirror the single-storey in-place arm: while an in-place "Edit
            // attributes" session is open the gold selection shade is
            // suppressed — the canvas paints the per-group colour overlay on
            // top instead (`structure_fan::render_groups_inplace` per cell).
            let no_selection = std::collections::HashSet::new();
            let selected = match structure_edit {
                Some(e) if !e.exploded => &no_selection,
                _ => selected,
            };
            all_storeys_2d::render(painter, rect, ber, display_mode, view, selected);
        }
        (SketchMode::Storey3D, None) => {
            full_house_3d::render(painter, rect, ber, display_mode, view, selected, hotspots);
        }
    }
}

/// Overlay a double-headed arrow that represents the wall the user is being
/// asked to dimension. The arrow is placed in the polygon's *interior*,
/// parallel to and offset from the wall (by ~half the interior depth), so
/// it's clearly visible against the rendered geometry. Dashed extension
/// lines connect the arrow's endpoints to the wall's endpoints, so the user
/// sees exactly which wall the prompt refers to. Uses the same auto-fit +
/// view transform as `render_ber`, so the arrow tracks pan/zoom.
pub fn render_dimension_prompt(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    wall_a: (f64, f64),
    wall_b: (f64, f64),
) {
    let Some((storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };

    // Find the polygon ring that contains the wall's endpoints, then compute
    // a perpendicular interior offset so the arrow lands in the middle of
    // the local interior. Falls back to drawing on the wall if no containing
    // ring or interior depth is found.
    let polygon_points = storey.floor_ring_containing_wall(wall_a, wall_b);
    let offset = polygon_points
        .as_deref()
        .and_then(|pts| manip_plan::interior_perpendicular_offset(pts, wall_a, wall_b))
        .unwrap_or((0.0, 0.0));

    let shifted_a = (wall_a.0 + offset.0, wall_a.1 + offset.1);
    let shifted_b = (wall_b.0 + offset.0, wall_b.1 + offset.1);

    let a_wall = t.apply(wall_a.0, wall_a.1);
    let b_wall = t.apply(wall_b.0, wall_b.1);
    let a = t.apply(shifted_a.0, shifted_a.1);
    let b = t.apply(shifted_b.0, shifted_b.1);

    let color = egui::Color32::from_rgb(60, 220, 255);
    let arrow_stroke = egui::Stroke::new(3.0, color);

    // Dashed extension lines from each end of the wall to the arrow's
    // matching endpoint. Only drawn when the offset is non-trivial — if the
    // arrow's already on the wall there's nothing to extend.
    if offset.0.abs() > 1e-3 || offset.1.abs() > 1e-3 {
        let dash_color = egui::Color32::from_rgba_unmultiplied(60, 220, 255, 140);
        let dash_stroke = egui::Stroke::new(1.5, dash_color);
        draw_dashed_segment(painter, a_wall, a, dash_stroke);
        draw_dashed_segment(painter, b_wall, b, dash_stroke);
    }

    // The arrow body.
    painter.line_segment([a, b], arrow_stroke);

    // Arrowheads at both ends — short lines at ±25° back along the arrow.
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len = (dx * dx + dy * dy).sqrt().max(1.0);
    let ux = dx / len;
    let uy = dy / len;
    let head_len = 12.0_f32.min(len * 0.25);
    let (sin25, cos25) = (0.4226_f32, 0.9063_f32);
    let h1 = egui::pos2(
        b.x + head_len * (-ux * cos25 + uy * sin25),
        b.y + head_len * (-ux * sin25 - uy * cos25),
    );
    let h2 = egui::pos2(
        b.x + head_len * (-ux * cos25 - uy * sin25),
        b.y + head_len * (ux * sin25 - uy * cos25),
    );
    painter.line_segment([b, h1], arrow_stroke);
    painter.line_segment([b, h2], arrow_stroke);
    let h3 = egui::pos2(
        a.x + head_len * (ux * cos25 - uy * sin25),
        a.y + head_len * (ux * sin25 + uy * cos25),
    );
    let h4 = egui::pos2(
        a.x + head_len * (ux * cos25 + uy * sin25),
        a.y + head_len * (-ux * sin25 + uy * cos25),
    );
    painter.line_segment([a, h3], arrow_stroke);
    painter.line_segment([a, h4], arrow_stroke);
}

/// Find the floor ring that contains both endpoints of the dimension wall.
/// Returns the ring's vertices as `(x, y)` tuples, without the closing
/// duplicate that `geo::LineString` keeps at the end.

/// Highlight a wall that's length-equivalent to the primary dimension-prompt
/// wall — a red line drawn directly on the wall, no arrowheads, no label.
/// The primary's cyan interior arrow still owns the "this is the
/// measurement" signal; this is just "these other walls would work too."
pub fn render_equivalent_wall(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    wall_a: (f64, f64),
    wall_b: (f64, f64),
) {
    render_wall_highlight(
        painter,
        rect,
        ber,
        current_storey_idx,
        sketch_mode,
        view,
        wall_a,
        wall_b,
        egui::Color32::from_rgb(255, 80, 80),
    );
}

/// Tap-to-copy wall-length labels while a dimension prompt is open: one
/// `"{len:.2} m"` label at each wall's midpoint, from the generous label set
/// (`AppController::copyable_wall_labels` — the settled outline plus the
/// active in-flight shape, minus the prompted wall). Same look as the
/// confirmed dimension labels, and drawn for EVERY entry — no declutter,
/// each label doubles as a tap-to-copy input key.
pub fn render_copy_wall_labels(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    labels: &[((f64, f64), (f64, f64), f32)],
) {
    if labels.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for (a, b, len) in labels {
        if *len < 1e-3 {
            continue;
        }
        let mid = t.apply((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
        painter.text(
            mid,
            egui::Align2::CENTER_CENTER,
            format!("{len:.2} m"),
            egui::FontId::monospace(11.0),
            style::DIMENSION_TEXT,
        );
    }
}

/// Highlight one wall in a caller-chosen colour — the same thick on-wall line
/// as [`render_equivalent_wall`], for prompts that need to distinguish two
/// walls (e.g. the position prompt's area wall vs its outline reference wall).
#[allow(clippy::too_many_arguments)]
pub fn render_wall_highlight(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    wall_a: (f64, f64),
    wall_b: (f64, f64),
    color: egui::Color32,
) {
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };

    let a = t.apply(wall_a.0, wall_a.1);
    let b = t.apply(wall_b.0, wall_b.1);

    let stroke = egui::Stroke::new(4.0, color);
    painter.line_segment([a, b], stroke);
}

/// Draw a PROVISIONAL arc preview: a polyline through `points` (world coords)
/// in a bright amber, with endpoint markers where it attaches to the wall.
/// Used while an `ExtendCurve`/`TrimCurve` arc is held pending its two attach
/// dimensions — the model still holds a straight placeholder edge underneath,
/// so this overlay is the only hint of the curve to come.
pub fn render_arc_preview(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    points: &[(f64, f64)],
) {
    if points.len() < 2 {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let screen: Vec<egui::Pos2> = points.iter().map(|(x, y)| t.apply(*x, *y)).collect();
    let amber = egui::Color32::from_rgb(255, 200, 60);
    let stroke = egui::Stroke::new(3.0, amber);
    for w in screen.windows(2) {
        painter.line_segment([w[0], w[1]], stroke);
    }
    painter.circle_filled(screen[0], 4.0, amber);
    painter.circle_filled(screen[screen.len() - 1], 4.0, amber);
}

/// Draw the living-area sub-shapes over the storey in a distinct green, each as
/// a closed outline. The `active` one (the most-recent, currently edited) is
/// drawn brighter and thicker so it's clear which the tools act on.
/// Draw the two candidate regions of a pending cut — distinct thick colours
/// with a label at each centroid — so the user can see which side is which and
/// tap the one they want to keep as the living area.
pub fn render_cut_candidates(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    regions: &[Vec<(f64, f64)>],
) {
    if regions.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let colors = [
        egui::Color32::from_rgb(255, 180, 40), // amber
        egui::Color32::from_rgb(40, 190, 255), // cyan
    ];
    let labels = ["tap ①", "tap ②"];
    for (i, region) in regions.iter().enumerate() {
        if region.len() < 3 {
            continue;
        }
        let pts: Vec<egui::Pos2> = region.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let color = colors[i % colors.len()];
        let n = pts.len();
        for k in 0..n {
            painter.line_segment([pts[k], pts[(k + 1) % n]], egui::Stroke::new(4.0, color));
        }
        // Label at the centroid (the two centroids sit on opposite sides of the
        // cut, so they disambiguate the overlapping shared boundary).
        let (mut cx, mut cy) = (0.0f32, 0.0f32);
        for p in &pts {
            cx += p.x;
            cy += p.y;
        }
        let c = egui::pos2(cx / n as f32, cy / n as f32);
        painter.text(
            c,
            egui::Align2::CENTER_CENTER,
            labels[i % labels.len()],
            egui::FontId::proportional(14.0),
            color,
        );
    }
}

/// Draw the PENDING ceiling-step line as a dashed amber segment across the
/// storey footprint. Dashed because the line is provisional input UX — it's
/// still awaiting its side / position / amount; committed ceiling-step
/// regions render as tinted rings instead (`render_phase_regions`).
pub fn render_ceiling_step_line(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    line_a: (f64, f64),
    line_b: (f64, f64),
) {
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let a = t.apply(line_a.0, line_a.1);
    let b = t.apply(line_b.0, line_b.1);
    draw_dashed_segment(
        painter,
        a,
        b,
        egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 170, 60)),
    );
}

/// Draw the storey's COMMITTED same-height ceiling DIVIDERS — each a faint
/// dashed cut segment (low-alpha grey), visually distinct from the
/// amber-solid height steps. These partition a ceiling region without a riser
/// (to bound a ridge's own area); the divider regions are excluded from the
/// amber tint/label feed, so this is the only place they appear.
pub fn render_ceiling_dividers(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    lines: &[((f64, f64), (f64, f64))],
) {
    if lines.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let stroke = egui::Stroke::new(
        2.0,
        egui::Color32::from_rgba_unmultiplied(120, 120, 120, 90),
    );
    for (a, b) in lines {
        draw_dashed_segment(painter, t.apply(a.0, a.1), t.apply(b.0, b.1), stroke);
    }
}

/// Draw the storey's roof apexes (committed high points) plus the PENDING
/// tapped point. Committed apexes render as a filled dot with their height
/// label above ("▲ 4.20 m"); the pending point is a hollow dot — provisional,
/// like the dashed ceiling-step line. Rose-magenta, distinct from the phase
/// purple / step teal / ceiling-step amber tints.
pub fn render_roof_apexes(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    apexes: &[((f64, f64), String)],
    pending: Option<(f64, f64)>,
) {
    if apexes.is_empty() && pending.is_none() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let color = egui::Color32::from_rgb(255, 80, 170); // rose-magenta
    for ((x, y), label) in apexes {
        let p = t.apply(*x, *y);
        painter.circle_filled(p, 5.0, color);
        painter.circle_stroke(p, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
        painter.text(
            p + egui::vec2(0.0, -10.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(13.0),
            color,
        );
    }
    if let Some((x, y)) = pending {
        let p = t.apply(x, y);
        painter.circle_stroke(p, 6.0, egui::Stroke::new(2.5, color));
    }
}

/// Draw the fan-vertex markers (roof-point mode only — the controller
/// returns them empty otherwise): an ANCHOR vertex (a gable side's current
/// pitch anchor — "this corner sets the pitch") is a larger DOUBLE-RING dot
/// (tapping clears its override); a FANNED vertex is a small filled dot
/// with a white ring (hip/fan: tapping unfans it); an EMBEDDED/unfanned one
/// is a hollow dot with a strike (hip/fan: tapping re-fans it; gable:
/// tapping makes it the side's anchor). Orange, distinct from the apex
/// rose-magenta and the ridge violet-blue.
pub fn render_ceiling_fan_markers(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    markers: &[((f64, f64), bool, bool)],
) {
    if markers.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let color = egui::Color32::from_rgb(255, 165, 40); // fan-marker orange
    for ((x, y), fanned, anchor) in markers {
        let p = t.apply(*x, *y);
        if *anchor {
            painter.circle_filled(p, 5.0, color);
            painter.circle_stroke(p, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
            painter.circle_stroke(p, 8.5, egui::Stroke::new(2.0, color));
        } else if *fanned {
            painter.circle_filled(p, 5.0, color);
            painter.circle_stroke(p, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
        } else {
            painter.circle_stroke(p, 5.0, egui::Stroke::new(2.0, color));
            // The strike: this corner is currently embedded / excluded.
            let d = egui::vec2(4.0, -4.0);
            painter.line_segment([p - d, p + d], egui::Stroke::new(2.0, color));
        }
    }
}

/// The roof-ridge hue: violet-blue, distinct from the apex rose-magenta, the
/// phase purple, the floor-step teal and the ceiling-step amber.
const ROOF_RIDGE_COLOR: egui::Color32 = egui::Color32::from_rgb(130, 150, 255);
/// Committed-dormer outline + label colour — a green distinct from the
/// ridge/rect family's violet-blue (a dormer is a non-resloping flat patch).
const DORMER_COLOR: egui::Color32 = egui::Color32::from_rgb(70, 190, 120);

/// Draw the PENDING roof-ridge line as a dashed segment between its OWN
/// endpoints (the stroke's extent — a ridge has endpoints, unlike the
/// bbox-spanning ceiling-step line). Dashed because it's provisional input
/// UX, still awaiting its position / height.
pub fn render_roof_ridge_line(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    line_a: (f64, f64),
    line_b: (f64, f64),
) {
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let a = t.apply(line_a.0, line_a.1);
    let b = t.apply(line_b.0, line_b.1);
    draw_dashed_segment(painter, a, b, egui::Stroke::new(3.0, ROOF_RIDGE_COLOR));
}

/// Draw the storey's COMMITTED roof ridges: a solid segment between the two
/// apex points of each two-apex group, labelled at the midpoint
/// ("⌃ 4.20 m", or "4.20→4.50 m" for a sloping ridge). The apex dots
/// themselves come from [`render_roof_apexes`].
/// The "this area was continued" outline: each CONTINUED target region's
/// plan ring, drawn as a closed dashed teal polygon while a
/// CONTINUE-ceiling tool is open — so an applied continuation stays
/// visible (not just tiny corner labels — field report deb23cbf feedback).
pub fn render_continued_regions(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    rings: &[Vec<(f64, f64)>],
) {
    if rings.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    const TEAL: egui::Color32 = egui::Color32::from_rgb(60, 175, 160);
    for ring in rings {
        if ring.len() < 2 {
            continue;
        }
        let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let n = pts.len();
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            painter.line_segment([a, b], egui::Stroke::new(2.5, TEAL));
        }
    }
}

pub fn render_roof_ridges(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    ridges: &[((f64, f64), (f64, f64), String)],
) {
    if ridges.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for ((ax, ay), (bx, by), label) in ridges {
        let a = t.apply(*ax, *ay);
        let b = t.apply(*bx, *by);
        painter.line_segment([a, b], egui::Stroke::new(3.0, ROOF_RIDGE_COLOR));
        let mid = egui::pos2((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        painter.text(
            mid + egui::vec2(0.0, -8.0),
            egui::Align2::CENTER_BOTTOM,
            label,
            egui::FontId::proportional(13.0),
            ROOF_RIDGE_COLOR,
        );
    }
}

/// Draw the storey's COMMITTED flat tops (rect ridges): a solid outline
/// around each four-apex rect group, labelled at its centre ("⌃ 4.20 m"),
/// plus the PENDING rect as a dashed outline — provisional, like the
/// pending ridge segment. Shares the ridge violet-blue (one roof-stroke
/// family); the corner dots come from [`render_roof_apexes`].
pub fn render_roof_rects(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    rects: &[([(f64, f64); 4], String)],
    pending: Option<[(f64, f64); 4]>,
) {
    if rects.is_empty() && pending.is_none() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for (corners, label) in rects {
        let pts: Vec<egui::Pos2> = corners.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        for k in 0..4 {
            painter.line_segment(
                [pts[k], pts[(k + 1) % 4]],
                egui::Stroke::new(3.0, ROOF_RIDGE_COLOR),
            );
        }
        let cx = pts.iter().map(|p| p.x).sum::<f32>() / 4.0;
        let cy = pts.iter().map(|p| p.y).sum::<f32>() / 4.0;
        painter.text(
            egui::pos2(cx, cy),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(13.0),
            ROOF_RIDGE_COLOR,
        );
    }
    if let Some(corners) = pending {
        let pts: Vec<egui::Pos2> = corners.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        for k in 0..4 {
            draw_dashed_segment(
                painter,
                pts[k],
                pts[(k + 1) % 4],
                egui::Stroke::new(3.0, ROOF_RIDGE_COLOR),
            );
        }
    }
}

/// Draw the storey's COMMITTED dormers: a solid outline around each dormer
/// footprint ring, labelled at its centroid ("⌐ 2.60 m"). A distinct green
/// from the ridge/rect family's violet-blue — a dormer is a non-resloping flat
/// patch carved into the sloped attic, not a ridge. The footprint is a general
/// ring (v1 is rectangular, but a future non-rectangular dormer draws
/// unchanged).
pub fn render_dormers(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    dormers: &[(Vec<(f64, f64)>, String)],
) {
    if dormers.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for (ring, label) in dormers {
        if ring.len() < 2 {
            continue;
        }
        let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let n = pts.len();
        for k in 0..n {
            painter.line_segment(
                [pts[k], pts[(k + 1) % n]],
                egui::Stroke::new(3.0, DORMER_COLOR),
            );
        }
        let cx = pts.iter().map(|p| p.x).sum::<f32>() / n as f32;
        let cy = pts.iter().map(|p| p.y).sum::<f32>() / n as f32;
        painter.text(
            egui::pos2(cx, cy),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(13.0),
            DORMER_COLOR,
        );
    }
}

/// DEBUG: draw the splice-pipeline phases of the last extend/trim. Each phase is
/// a distinctly-coloured polyline (with dotted vertices) plus a colour-keyed
/// legend entry top-left, so the stages — raw stroke → orthogonalized → pinned →
/// spliced ring — can be compared visually. `phases` is `(label, world pts, closed)`.
pub fn render_splice_debug(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    phases: &[(String, Vec<(f32, f32)>, bool)],
) {
    if phases.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let colors = [
        egui::Color32::from_rgb(255, 70, 70),  // raw — red
        egui::Color32::from_rgb(255, 170, 30), // simplified — orange
        egui::Color32::from_rgb(230, 220, 40), // snapped to H/V — yellow
        egui::Color32::from_rgb(60, 200, 90),  // orthogonalized — green
        egui::Color32::from_rgb(80, 150, 255), // pinned — blue
        egui::Color32::from_rgb(210, 90, 230), // spliced — magenta
    ];
    for (i, (label, pts_w, closed)) in phases.iter().enumerate() {
        if pts_w.len() < 2 {
            continue;
        }
        let color = colors[i % colors.len()];
        let pts: Vec<egui::Pos2> = pts_w
            .iter()
            .map(|(x, y)| t.apply(*x as f64, *y as f64))
            .collect();
        let n = pts.len();
        let segs = if *closed { n } else { n - 1 };
        for k in 0..segs {
            painter.line_segment([pts[k], pts[(k + 1) % n]], egui::Stroke::new(2.0, color));
        }
        for p in &pts {
            painter.circle_filled(*p, 3.0, color);
        }
        // Colour-keyed legend, stacked top-left so labels never overlap the
        // shape; numbered by pipeline order.
        let ly = rect.top() + 8.0 + i as f32 * 16.0;
        painter.text(
            egui::pos2(rect.left() + 10.0, ly),
            egui::Align2::LEFT_TOP,
            format!("{}. {}", i + 1, label),
            egui::FontId::proportional(13.0),
            color,
        );
    }
}

/// Short human label for an elective snap kind.
pub(crate) fn snap_kind_label(kind: manip_plan::SnapKind) -> &'static str {
    match kind {
        manip_plan::SnapKind::ToVertex | manip_plan::SnapKind::ToCorner => "corner",
        manip_plan::SnapKind::ToEdge => "edge",
        manip_plan::SnapKind::ToPreviousOutline => "prev outline",
        manip_plan::SnapKind::ToParallelWall => "parallel wall",
        manip_plan::SnapKind::ToReferenceSegment => "ref edge",
        manip_plan::SnapKind::ToPhaseBoundary => "phase line",
    }
}

/// Whether a snap kind aligns a whole WALL onto an existing wall (a parametric
/// pin — the wall shares that wall's coordinate and is skipped by
/// dimensioning), as opposed to moving a drawn POINT onto a nearby feature.
/// Drives the tag's two visual classes: violet square (wall pin) vs orange
/// circle (point snap).
pub(crate) fn snap_kind_is_wall(kind: manip_plan::SnapKind) -> bool {
    matches!(kind, manip_plan::SnapKind::ToParallelWall)
}

/// One-line consequence of an elective snap, for the tag menu — says what the
/// snap DID (point moved vs wall pinned) so the assessor knows what "Don't
/// snap" gives back. A pinned wall's dimension question is skipped; un-snapping
/// reopens it.
pub(crate) fn snap_kind_hint(kind: manip_plan::SnapKind) -> &'static str {
    match kind {
        manip_plan::SnapKind::ToVertex => "endpoint attached to the corner (reuses that vertex)",
        manip_plan::SnapKind::ToCorner => "point moved onto the nearby corner",
        manip_plan::SnapKind::ToEdge => "endpoint attached along the wall",
        manip_plan::SnapKind::ToPreviousOutline => "point moved onto the storey below's outline",
        manip_plan::SnapKind::ToParallelWall => {
            "wall pinned onto the parallel wall — its dimension isn't asked; un-snap reopens it"
        }
        manip_plan::SnapKind::ToReferenceSegment => {
            "point moved onto a reference edge (roof division / wall base)"
        }
        manip_plan::SnapKind::ToPhaseBoundary => "point moved onto a construction-phase line",
    }
}

/// The world→screen transform for the active storey, exposed so the interaction
/// layer can place clickable widgets (e.g. snap tags) over the canvas.
pub(crate) fn active_storey_transform(
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
) -> Option<Transform2d> {
    storey_transform(rect, ber, current_storey_idx, sketch_mode, view).map(|(_storey, t)| t)
}

/// Render a marker at each elective snap of the last action, in the tag's
/// visual class: an orange CIRCLE for a point snap, a violet SQUARE for a wall
/// pin ([`snap_kind_is_wall`]); grey (in the same shape) when currently
/// un-snapped. The clickable label/menu is an overlay widget (see the sketch
/// page) placed at the same point.
pub fn render_snap_tags(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    tags: &[crate::controller::app_controller::SnapTagView],
) {
    if tags.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for tag in tags {
        let p = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
        // The shape follows the snap's class even while un-snapped (the
        // most-aggressive candidate stands in), so a grey tag still says what
        // KIND of snap it would re-apply.
        let kind = tag
            .current_kind
            .or_else(|| tag.options.first().map(|(k, _)| *k));
        let wall = kind.is_some_and(snap_kind_is_wall);
        let color = if tag.current_kind.is_some() {
            if wall {
                egui::Color32::from_rgb(150, 90, 255) // violet — wall pinned
            } else {
                egui::Color32::from_rgb(255, 140, 0) // orange — point applied
            }
        } else {
            egui::Color32::from_gray(150) // grey — un-snapped
        };
        if wall {
            // White ring drawn as a slightly larger backing square.
            painter.rect_filled(
                egui::Rect::from_center_size(p, egui::vec2(13.0, 13.0)),
                2.0,
                egui::Color32::WHITE,
            );
            painter.rect_filled(
                egui::Rect::from_center_size(p, egui::vec2(10.0, 10.0)),
                1.5,
                color,
            );
        } else {
            painter.circle_filled(p, 5.0, color);
            painter.circle_stroke(p, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
        }
    }
}

/// Render committed tagged areas (construction-phase or floor-step regions):
/// each zone's EXTERIOR and every HOLE ring tinted `color`, with ONE label at
/// the zone's `label_pos` (a point guaranteed inside the material, outside every
/// hole), so the assessor sees which footprint carries which tag — and a donut
/// zone (an older phase a newer one overlaps) reads correctly, its label never
/// falling in the hole. Hole-free overlays (steps / structure splits / pending
/// carves) build their [`PhaseDisplayRegion`] via `PhaseDisplayRegion::from_ring`.
#[allow(clippy::too_many_arguments)]
pub fn render_phase_regions(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    regions: &[PhaseDisplayRegion],
    color: egui::Color32,
) {
    if regions.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let stroke_ring = |ring: &[(f64, f64)]| {
        if ring.len() < 3 {
            return;
        }
        let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let n = pts.len();
        for k in 0..n {
            painter.line_segment([pts[k], pts[(k + 1) % n]], egui::Stroke::new(3.0, color));
        }
    };
    for region in regions {
        if region.exterior.len() < 3 {
            continue;
        }
        stroke_ring(&region.exterior);
        for hole in &region.holes {
            stroke_ring(hole);
        }
        let (lx, ly) = region.label_pos;
        painter.text(
            t.apply(lx, ly),
            egui::Align2::CENTER_CENTER,
            &region.label,
            egui::FontId::proportional(14.0),
            color,
        );
    }
}

/// Draw a bright gold DASHED outline (inset just inside each ring) around the
/// given world-space `rings` — the pre-edit "selected area" marquee for carve
/// selections, painted on top so it reads clearly even where a ring's edges run
/// along the surface perimeter. Outline only, no fill.
pub fn render_selection_area(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    rings: &[Vec<(f64, f64)>],
) {
    if rings.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let stroke = egui::Stroke::new(3.0, style::SELECTION_LINE);
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let inset = inset_ring(&pts, 5.0);
        let n = inset.len();
        for k in 0..n {
            draw_dashed_segment(painter, inset[k], inset[(k + 1) % n], stroke);
        }
    }
}

/// Highlight ONE carve edge (world coords) — the wall-inclusion ask's current
/// question — unmissably: a wide translucent halo under a solid core line in
/// the selection gold, plus a white-rimmed gold "?" BADGE at the edge
/// midpoint (the snap-tag dot idiom, but round + larger so it reads as a
/// question, not a snap). The badge and the panel's description always refer
/// to the SAME edge — the caller passes the prompt's current one. Steady
/// strokes only (no animation machinery); the solid core over the wide band
/// keeps it distinct from the staged-ring dashed marquee that stays visible
/// underneath during the ask.
pub fn render_wall_inclusion_edge(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    edge: ((f64, f64), (f64, f64)),
) {
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let a = t.apply(edge.0 .0, edge.0 .1);
    let b = t.apply(edge.1 .0, edge.1 .1);
    painter.line_segment(
        [a, b],
        egui::Stroke::new(14.0, style::SELECTION_LINE.gamma_multiply(0.35)),
    );
    painter.line_segment([a, b], egui::Stroke::new(4.5, style::SELECTION_LINE));
    // The "?" badge: white rim → gold disc → dark glyph, centred on the edge.
    let mid = egui::pos2((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
    painter.circle_filled(mid, 11.0, egui::Color32::WHITE);
    painter.circle_filled(mid, 9.0, style::SELECTION_LINE);
    painter.text(
        mid,
        egui::Align2::CENTER_CENTER,
        "?",
        egui::FontId::proportional(13.0),
        egui::Color32::BLACK,
    );
}

/// Tint the CURRENT new-storey outline suggestion's ring translucently over
/// the previous-storey watermark (the browse stage of the new-storey flow):
/// a sky-blue fill — a hue none of the other overlays use — plus a crisp
/// outline. Filled via earcutr so concave suggestions (L, U, …) tint
/// correctly (the floor-fill pattern; egui's `convex_polygon` fan-cuts
/// across concavities).
pub fn render_suggestion_ring(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    ring: &[(f64, f64)],
) {
    if ring.len() < 3 {
        return;
    }
    // The storey is fresh (no outline yet), so the fit transform is driven
    // by the watermark half of `fit_bbox_for_storey` — the same box the
    // watermark was drawn with, putting the tint exactly over it.
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let screen_pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
    let fill = egui::Color32::from_rgba_unmultiplied(80, 180, 255, 60); // translucent sky blue
    let outline = egui::Color32::from_rgb(80, 180, 255);
    let flat: Vec<f64> = ring.iter().flat_map(|(x, y)| [*x, *y]).collect();
    if let Ok(triangles) = earcutr::earcut(&flat, &[], 2) {
        let mut mesh = egui::Mesh::default();
        for &p in &screen_pts {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: p,
                uv: egui::epaint::WHITE_UV,
                color: fill,
            });
        }
        for idx in triangles {
            mesh.indices.push(idx as u32);
        }
        painter.add(egui::Shape::mesh(mesh));
    }
    painter.add(egui::Shape::closed_line(
        screen_pts,
        egui::Stroke::new(2.5, outline),
    ));
}

#[allow(clippy::too_many_arguments)]
pub fn render_living_areas(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    display_mode: DisplayMode,
    view: ViewParams,
    polygons: &[Vec<(f64, f64)>],
    // Per area, the known-length walls as (start, end, length) in world coords.
    wall_labels: &[Vec<((f64, f64), (f64, f64), f32)>],
    active: Option<usize>,
) {
    if polygons.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for (i, poly) in polygons.iter().enumerate() {
        if poly.len() < 3 {
            continue;
        }
        let is_active = active == Some(i);
        let pts: Vec<egui::Pos2> = poly.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let (color, width) = if is_active {
            (egui::Color32::from_rgb(80, 230, 130), 3.0)
        } else {
            (egui::Color32::from_rgb(60, 170, 100), 2.0)
        };
        let stroke = egui::Stroke::new(width, color);
        let n = pts.len();
        for k in 0..n {
            painter.line_segment([pts[k], pts[(k + 1) % n]], stroke);
        }
    }

    // Length labels on each living area's known walls — same gating and look as
    // the storey's dimension labels.
    if matches!(
        display_mode,
        DisplayMode::FloorMapDims | DisplayMode::AllSurfaces
    ) {
        for area in wall_labels {
            for (a, b, len) in area {
                if *len < 1e-3 {
                    continue;
                }
                let mid = t.apply((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
                painter.text(
                    mid,
                    egui::Align2::CENTER_CENTER,
                    format!("{len:.2} m"),
                    egui::FontId::monospace(11.0),
                    egui::Color32::from_rgb(120, 240, 160),
                );
            }
        }
    }
}

/// Porch regions overlaid like living areas (`render_living_areas`), in a
/// distinct violet with a "Porch" text label at each region's centroid so the
/// region reads as a porch at a glance. Same active-brighter highlight and the
/// same known-wall length labels.
#[allow(clippy::too_many_arguments)]
pub fn render_porches(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    display_mode: DisplayMode,
    view: ViewParams,
    polygons: &[Vec<(f64, f64)>],
    // Per porch, the known-length walls as (start, end, length) in world coords.
    wall_labels: &[Vec<((f64, f64), (f64, f64), f32)>],
    active: Option<usize>,
) {
    if polygons.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for (i, poly) in polygons.iter().enumerate() {
        if poly.len() < 3 {
            continue;
        }
        let is_active = active == Some(i);
        let pts: Vec<egui::Pos2> = poly.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let (color, width) = if is_active {
            (egui::Color32::from_rgb(200, 130, 240), 3.0)
        } else {
            (egui::Color32::from_rgb(150, 95, 185), 2.0)
        };
        let stroke = egui::Stroke::new(width, color);
        let n = pts.len();
        for k in 0..n {
            painter.line_segment([pts[k], pts[(k + 1) % n]], stroke);
        }
        // "Porch" label at the region's centroid — the porch records a
        // labelled region, so the label is part of the rendering.
        let (cx, cy) = poly.iter().fold((0.0, 0.0), |(sx, sy), (x, y)| {
            (sx + *x / poly.len() as f64, sy + *y / poly.len() as f64)
        });
        painter.text(
            t.apply(cx, cy),
            egui::Align2::CENTER_CENTER,
            "Porch",
            egui::FontId::proportional(12.0),
            color,
        );
    }

    // Length labels on each porch's known walls — same gating and look as the
    // living areas' (and the storey's) dimension labels.
    if matches!(
        display_mode,
        DisplayMode::FloorMapDims | DisplayMode::AllSurfaces
    ) {
        for area in wall_labels {
            for (a, b, len) in area {
                if *len < 1e-3 {
                    continue;
                }
                let mid = t.apply((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
                painter.text(
                    mid,
                    egui::Align2::CENTER_CENTER,
                    format!("{len:.2} m"),
                    egui::FontId::monospace(11.0),
                    egui::Color32::from_rgb(215, 165, 245),
                );
            }
        }
    }
}

/// Colour each outline wall's exposure SECTIONS: orange = exposed, amber =
/// semi-exposed, slate-blue = non-heat-loss. In exposure mode every wall is
/// shown faint grey first (so un-marked gaps read as grey), the selected wall's
/// two section handles are drawn, and a legend is shown. Outside the mode only
/// the coloured sections are drawn.
///
/// Derived RISER walls draw as DASHED lines along their plan base edges
/// (dashed ≠ outline wall), coloured by exposure class — full-strength in
/// exposure mode (where they're tappable), faded otherwise like the other
/// non-active categories. The selected riser gets endpoint handles, mirroring
/// the wall-selection visual.
#[allow(clippy::too_many_arguments)]
pub fn render_wall_exposure(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    walls: &[WallExposureView],
    risers: &[crate::controller::app_controller::RiserView],
    selection: Option<&ExposureSelection>,
    exposure_mode: bool,
) {
    if walls.is_empty() && risers.is_empty() {
        return;
    }
    let any_set = walls
        .iter()
        .any(|w| !w.sections.is_empty() || !w.partition_exposures.is_empty());
    if !exposure_mode && !any_set && risers.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };

    let exposed = style::EXPOSED_WALL;
    let semi = style::SEMI_EXPOSED_WALL;
    let non = style::NON_HEAT_LOSS_WALL;
    let grey = style::UNSET_WALL;
    let width = if exposure_mode { 5.0 } else { 3.5 };
    let color_of = |e: WallExposure| match e {
        WallExposure::Exposed => exposed,
        WallExposure::SemiExposed => semi,
        WallExposure::NonHeatLoss => non,
        // Sections store PAINTED exposures only: Unset is derived coverage
        // and Ground is riser-only (the riser toolbar's Ground button), so
        // neither reaches a section — but the match stays exhaustive (and
        // risers, which CAN be Unset or Ground, colour through here too).
        WallExposure::Unset => grey,
        WallExposure::Ground => style::GROUND_WALL,
        // UNREACHABLE in practice: sections never recover InterStorey
        // (`sections_from_wall_regions` skips it) and the controller's riser
        // views exclude InterStorey risers. Kept exhaustive — grey, the
        // ghost/unclassified read.
        WallExposure::InterStorey | WallExposure::Irrelevant => grey,
    };
    // Point at distance `d` (metres) along wall a→b of length `len`, in screen.
    let along = |a: (f64, f64), b: (f64, f64), len: f32, d: f32| {
        let f = if len > 1e-6 { (d / len) as f64 } else { 0.0 };
        t.apply(a.0 + f * (b.0 - a.0), a.1 + f * (b.1 - a.1))
    };

    for w in walls {
        // A FACE-PARTITIONED wall (2-D regions, no 1-D sections): draw the whole
        // wall as an INTERLEAVED line cycling its distinct region exposures, so a
        // mixed wall reads as mixed in the plan. Dense enough that each colour
        // recurs several times along the wall.
        if !w.partition_exposures.is_empty() {
            let segs = (w.partition_exposures.len() * 6).max(12);
            for k in 0..segs {
                let d0 = w.length_m * (k as f32) / (segs as f32);
                let d1 = w.length_m * ((k + 1) as f32) / (segs as f32);
                let c = color_of(w.partition_exposures[k % w.partition_exposures.len()]);
                let pa = along(w.a, w.b, w.length_m, d0);
                let pb = along(w.a, w.b, w.length_m, d1);
                painter.line_segment([pa, pb], egui::Stroke::new(width, c));
            }
            continue;
        }
        // Faint full wall in mode, so un-marked gaps stay visible under the
        // coloured sections drawn on top.
        if exposure_mode {
            painter.line_segment(
                [t.apply(w.a.0, w.a.1), t.apply(w.b.0, w.b.1)],
                egui::Stroke::new(width, grey),
            );
        }
        for s in &w.sections {
            let pa = along(w.a, w.b, w.length_m, s.start_m);
            let pb = along(w.a, w.b, w.length_m, s.end_m);
            painter.line_segment([pa, pb], egui::Stroke::new(width, color_of(s.exposure)));
        }
    }

    // RISER walls: dashed along the base edge, exposure-coloured. Faded
    // outside exposure mode (they're context there, not the active tool).
    for r in risers {
        let pa = t.apply(r.a.0, r.a.1);
        let pb = t.apply(r.b.0, r.b.1);
        let color = if exposure_mode {
            color_of(r.exposure)
        } else {
            style::faded(color_of(r.exposure))
        };
        let w = if r.selected { width + 2.0 } else { width };
        painter.add(egui::Shape::dashed_line(
            &[pa, pb],
            egui::Stroke::new(w, color),
            8.0,
            6.0,
        ));
        if r.selected {
            // Whole-face selection: handles at both ends (the wall-selection
            // visual), but they don't drag — risers have no section range.
            for p in [pa, pb] {
                painter.circle_filled(p, 6.0, egui::Color32::WHITE);
                painter.circle_stroke(
                    p,
                    6.0,
                    egui::Stroke::new(2.0, egui::Color32::from_rgb(40, 40, 40)),
                );
            }
        }
    }

    // Selected wall's section handles.
    if let Some(sel) = selection {
        let hs = along(sel.a, sel.b, sel.length_m, sel.start_m);
        let he = along(sel.a, sel.b, sel.length_m, sel.end_m);
        for p in [hs, he] {
            painter.circle_filled(p, 6.0, egui::Color32::WHITE);
            painter.circle_stroke(
                p,
                6.0,
                egui::Stroke::new(2.0, egui::Color32::from_rgb(40, 40, 40)),
            );
        }
    }

    if exposure_mode {
        let mut legend = vec![
            ("Exposed", exposed),
            ("Semi-exposed", semi),
            ("Non-heat-loss", non),
            ("Unset", grey),
        ];
        if !risers.is_empty() {
            // Ground is a riser-only CHOICE (never a default now) — the
            // earth swatch only means something once a dashed riser line is
            // on screen.
            legend.push(("Ground", style::GROUND_WALL));
        }
        let mut y = rect.top() + 10.0;
        for (label, color) in legend {
            let p = egui::pos2(rect.left() + 12.0, y);
            painter.line_segment(
                [p, egui::pos2(p.x + 22.0, p.y)],
                egui::Stroke::new(4.0, color),
            );
            painter.text(
                egui::pos2(p.x + 28.0, p.y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(12.0),
                color,
            );
            y += 18.0;
        }
        if !risers.is_empty() {
            let p = egui::pos2(rect.left() + 12.0, y);
            painter.add(egui::Shape::dashed_line(
                &[p, egui::pos2(p.x + 22.0, p.y)],
                egui::Stroke::new(4.0, grey),
                5.0,
                4.0,
            ));
            painter.text(
                egui::pos2(p.x + 28.0, p.y),
                egui::Align2::LEFT_CENTER,
                "Riser (tap to mark)",
                egui::FontId::proportional(12.0),
                grey,
            );
        }
    }
}

/// Draw a small filled marker at a world-coord point — used to highlight a
/// vertex the user has just inserted in Edit mode (double-click) and is
/// currently dragging. Red filled circle with a thin white outline so it's
/// visible against either the polygon fill or the dark canvas background.
pub fn render_vertex_marker(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    world_x: f64,
    world_y: f64,
) {
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    let p = t.apply(world_x, world_y);
    let radius = 6.0_f32;
    painter.circle_filled(p, radius, egui::Color32::from_rgb(255, 80, 80));
    painter.circle_stroke(
        p,
        radius,
        egui::Stroke::new(1.5, egui::Color32::from_rgb(240, 240, 240)),
    );
}

/// Extract the primary floor ring of `storey` as `(x, y)` tuples (no closing
/// duplicate). Used by `render_ber` to supply a previous-storey watermark to
/// `storey_2d::render`. `None` if the storey has no floor regions yet.
fn previous_outline_ring(storey: &ber_model::ber::structural::Storey) -> Option<Vec<(f64, f64)>> {
    // When the floor is split into phase zones, merge them back to the full
    // footprint; otherwise the single region IS the footprint.
    if storey.floors.regions.len() > 1 {
        let ring = storey.floors.merged_outline();
        if ring.len() < 3 {
            return None;
        }
        return Some(ring.into_iter().map(|p| (p.x, p.y)).collect());
    }
    let region = storey.floors.regions.first()?;
    let mut coords: Vec<(f64, f64)> = region
        .polygon
        .exterior()
        .coords()
        .map(|c| (c.x, c.y))
        .collect();
    let eps = 1e-9;
    if coords.len() >= 2 {
        let first = coords[0];
        let last = coords[coords.len() - 1];
        if (first.0 - last.0).abs() < eps && (first.1 - last.1).abs() < eps {
            coords.pop();
        }
    }
    if coords.len() < 3 {
        return None;
    }
    Some(coords)
}

/// Union of two optional bounding boxes (min/max corner pairs).

/// The bounding box used to auto-fit storey `idx` on the canvas: the storey's
/// own floor bbox unioned with the previous-storey watermark (drawn only while
/// this storey is still empty). Mirrors the union in [`storey_2d::render`] so
/// non-render callers that need the on-screen transform — notably the
/// stroke→world conversion when drawing a new storey over the watermark — agree
/// with what was actually drawn. `None` if there's nothing to fit.
pub(crate) fn fit_bbox_for_storey(ber: &Ber, idx: usize) -> Option<((f64, f64), (f64, f64))> {
    let storey = ber.storeys.get(idx)?;
    let storey_bb = storey_2d::bounding_box(storey);
    // Match render_ber: the previous-storey watermark is part of the fit on any
    // upper storey (both empty and during dimensioning), so callers that invert
    // the on-screen transform agree with what was drawn.
    let watermark_bb = if idx > 0 {
        ber.storeys
            .get(idx - 1)
            .and_then(previous_outline_ring)
            .and_then(|r| storey_2d::ring_bbox(&r))
    } else {
        None
    };
    union_bbox(storey_bb, watermark_bb)
}

fn empty_storey_hint(painter: &egui::Painter, rect: egui::Rect) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "No floor plan yet. Draw a shape on this canvas to begin.",
        egui::FontId::default(),
        style::HINT_TEXT,
    );
}

/// Shared preamble for the single-storey `render_*` overlays: requires 2D mode,
/// a loaded BER, a selected storey, and a fittable bbox — then returns that
/// storey together with its fit transform. Each overlay adds its own
/// collection-empty early-out on top. `None` means "nothing to draw, return".
pub(crate) fn storey_transform(
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
) -> Option<(&ber_model::ber::structural::Storey, Transform2d)> {
    if !matches!(sketch_mode, SketchMode::Storey2D) {
        return None;
    }
    let ber = ber?;
    let idx = current_storey_idx?;
    let storey = ber.storeys.get(idx)?;
    // Use the SAME fit box the plan was drawn with — storey ∪ previous-storey
    // watermark on any upper storey. Fitting the storey alone here (while the
    // plan fit includes the watermark) gives the dimension-prompt arrow a
    // different scale/origin than the geometry, so it lands off the wall.
    let (min_xy, max_xy) = fit_bbox_for_storey(ber, idx)?;
    Some((storey, fit_transform(rect, min_xy, max_xy, view)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fit scale is inversely proportional to footprint size, so a storey
    /// twice as large fits at half the scale. This is what lets the "All
    /// storeys" overview share one scale and still show relative sizes: taking
    /// the min (largest storey's) scale renders the big one full-cell and a
    /// half-size storey at half the pixels.
    #[test]
    fn auto_fit_scale_is_inverse_to_footprint_size() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let small = auto_fit_scale(rect, (0.0, 0.0), (10.0, 10.0));
        let big = auto_fit_scale(rect, (0.0, 0.0), (20.0, 20.0));
        assert!(
            small > big,
            "the larger footprint must fit at a smaller scale"
        );
        assert!(
            (small - 2.0 * big).abs() < 1e-6,
            "doubling the footprint halves the fit scale (small={small}, big={big})"
        );
    }

    /// Reproduces the session figures in the "All storeys" overview: storey 0 is
    /// a 1×1 m square; storey 1 is a larger L (bbox 3×1 m, 6 vertices, sharing
    /// the base at y=0) reflecting the 3 m / 0.5 m walls drawn over it. Both
    /// cells are driven by the GLOBAL bbox, so the overview must:
    ///   1. render both at ONE shared scale (relative sizes read true),
    ///   2. align them — a point on the shared base lands at the same pixel Y
    ///      in each cell (no more per-cell vertical centering).
    #[test]
    fn all_storeys_overview_shares_scale_and_aligns_base() {
        use ber_model::ber::common::ItemId;
        use ber_model::ber::structural::{FloorRegions, Storey};
        use domain::points::Point2f64;

        let pt = |x: f64, y: f64| Point2f64::new(x, y);
        let floors =
            |verts: Vec<Point2f64>| FloorRegions::from_vertex_lists(vec![verts], ItemId::default());
        let storeys = [
            Storey {
                floors: floors(vec![pt(0., 0.), pt(1., 0.), pt(1., 1.), pt(0., 1.)]),
                ..Default::default()
            },
            Storey {
                // L-shape: 3 m bottom wall, a 0.5 m step, back to the 1×1 corner.
                floors: floors(vec![
                    pt(0., 0.),
                    pt(3., 0.),
                    pt(3., 0.5),
                    pt(1., 0.5),
                    pt(1., 1.),
                    pt(0., 1.),
                ]),
                ..Default::default()
            },
        ];

        // Same fit math as `all_storeys_2d::render`: one global bbox per cell.
        let global = storeys
            .iter()
            .filter_map(storey_2d::bounding_box)
            .fold(None, |acc, bb| union_bbox(acc, Some(bb)))
            .expect("non-empty");
        assert_eq!(
            global,
            ((0.0, 0.0), (3.0, 1.0)),
            "global bbox spans both storeys"
        );

        let cell0 = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 200.0));
        let cell1 = egui::Rect::from_min_size(egui::pos2(200.0, 0.0), egui::vec2(200.0, 200.0));
        let view = ViewParams::identity();
        let t0 = fit_transform(cell0, global.0, global.1, view);
        let t1 = fit_transform(cell1, global.0, global.1, view);

        // (1) Shared scale: 1 m is the same number of pixels in both cells.
        assert!(
            (t0.scale() - t1.scale()).abs() < 1e-9,
            "cells share one scale (t0={}, t1={})",
            t0.scale(),
            t1.scale()
        );

        // (2) Aligned: a point on the shared base (y=0) lands at the same pixel Y
        // in both cells — storeys line up by their base, not floating centered.
        let base0 = t0.apply(0.5, 0.0);
        let base1 = t1.apply(0.5, 0.0);
        assert!(
            (base0.y - base1.y).abs() < 1e-3,
            "shared base aligns across cells (y0={}, y1={})",
            base0.y,
            base1.y
        );

        // (3) Relative sizes read true: storey 1's 3 m wall spans 3× the pixels
        // of storey 0's 1 m wall at the shared scale.
        let span = |t: &Transform2d, w: f64| (t.apply(w, 0.0).x - t.apply(0.0, 0.0).x).abs();
        assert!(
            (span(&t0, 3.0) - 3.0 * span(&t0, 1.0)).abs() < 1e-3,
            "3 m reads as 3× the pixels of 1 m"
        );
    }

    /// At the default orbit angles, `project_3d` reproduces the historical
    /// fixed isometric `((x − y)·cos30°, (x + y)·sin30° − z)` exactly, up to
    /// one uniform scale factor √(3/2) on both axes — which the auto-fit
    /// cancels, so the default 3D view renders pixel-identically to before
    /// orbiting existed.
    #[test]
    fn default_orbit_angles_reproduce_the_fixed_isometric() {
        let v = ViewParams::identity();
        let cos30 = 0.866_025_403_784_438_6_f64;
        let s = 1.5_f64.sqrt();
        for &(x, y, z) in &[
            (1.0, 0.0, 0.0),
            (0.0, 1.0, 0.0),
            (0.0, 0.0, 1.0),
            (3.2, -1.7, 2.4),
            (-5.0, 8.5, 0.3),
        ] {
            let (px, py) = v.project_3d(x, y, z);
            let (ix, iy) = ((x - y) * cos30, (x + y) * 0.5 - z);
            assert!(
                (px * s - ix).abs() < 1e-4 && (py * s - iy).abs() < 1e-4,
                "default orbit of ({x},{y},{z}) → ({px},{py})·√(3/2) ≠ iso ({ix},{iy})"
            );
        }
    }
}
