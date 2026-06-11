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
//!   - `Roofs`       — highlight ceiling regions / roof-height labels
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
pub mod full_house_3d;
pub mod storey_2d;
pub mod storey_3d;

use crate::controller::app_controller::{ExposureSelection, WallExposureView};
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::structural::WallExposure;
use domain::sketch::{DisplayMode, SketchMode};
use eframe::egui;
use manip_2d_plan::CollinearitySuggestion;

/// User-controlled view transform passed in from the controller. `zoom = 1.0`
/// and zero offsets = pure auto-fit. Higher zoom enlarges; positive offset_x
/// pans the view right. `yaw_deg`/`pitch_deg` are the 3D orbit angles
/// (ignored by the 2D views); their defaults reproduce the historical fixed
/// isometric projection.
#[derive(Debug, Clone, Copy)]
pub struct ViewParams {
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
}

impl Default for ViewParams {
    fn default() -> Self {
        Self::identity()
    }
}

impl ViewParams {
    pub fn identity() -> Self {
        Self {
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            yaw_deg: crate::controller::app_controller::DEFAULT_YAW_DEG,
            pitch_deg: crate::controller::app_controller::DEFAULT_PITCH_DEG,
        }
    }

    /// Orbit-parameterized orthographic projection of a world point (z up)
    /// onto the 2D view plane (y down) — see [`project_3d`].
    pub(crate) fn project_3d(&self, x: f64, y: f64, z: f64) -> (f64, f64) {
        project_3d(x, y, z, self.yaw_deg, self.pitch_deg)
    }
}

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
                storey_2d::render(
                    painter,
                    rect,
                    storey,
                    display_mode,
                    highlight,
                    dimensioned_walls,
                    view,
                    watermark.as_deref(),
                    None,
                );
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        (SketchMode::Storey3D, Some(i)) => {
            if let Some(storey) = ber.storeys.get(i) {
                storey_3d::render(painter, rect, storey, display_mode, view);
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        (SketchMode::Storey2D, None) => {
            all_storeys_2d::render(painter, rect, ber, display_mode, view);
        }
        (SketchMode::Storey3D, None) => {
            full_house_3d::render(painter, rect, ber, display_mode, view);
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
    let polygon_points = ring_containing_wall(storey, wall_a, wall_b);
    let offset = polygon_points
        .as_deref()
        .and_then(|pts| interior_perpendicular_offset(pts, wall_a, wall_b))
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
fn ring_containing_wall(
    storey: &datatypes::ber::structural::Storey,
    wall_a: (f64, f64),
    wall_b: (f64, f64),
) -> Option<Vec<(f64, f64)>> {
    let eps = 1e-3_f64;
    for region in &storey.floors.regions {
        let mut coords: Vec<(f64, f64)> = region
            .polygon
            .exterior()
            .coords()
            .map(|c| (c.x, c.y))
            .collect();
        // Drop the closing duplicate that geo adds.
        if coords.len() >= 2 && {
            let first = coords[0];
            let last = coords[coords.len() - 1];
            (first.0 - last.0).abs() < eps && (first.1 - last.1).abs() < eps
        } {
            coords.pop();
        }
        let has = |p: (f64, f64)| {
            coords
                .iter()
                .any(|q| (q.0 - p.0).abs() < eps && (q.1 - p.1).abs() < eps)
        };
        if has(wall_a) && has(wall_b) {
            return Some(coords);
        }
    }
    None
}

/// Compute the perpendicular offset (`dx`, `dy`) to apply to the wall so the
/// arrow lands roughly in the middle of the local interior. Casts a
/// perpendicular ray from the wall's midpoint toward the polygon's centroid
/// and uses half the distance to the next perpendicular edge as the offset.
/// Returns `None` if the wall isn't axis-aligned or no opposing edge is found.
fn interior_perpendicular_offset(
    polygon: &[(f64, f64)],
    wall_a: (f64, f64),
    wall_b: (f64, f64),
) -> Option<(f64, f64)> {
    let n = polygon.len();
    if n < 4 {
        return None;
    }
    let mid_x = (wall_a.0 + wall_b.0) * 0.5;
    let mid_y = (wall_a.1 + wall_b.1) * 0.5;

    let centroid_x: f64 = polygon.iter().map(|p| p.0).sum::<f64>() / n as f64;
    let centroid_y: f64 = polygon.iter().map(|p| p.1).sum::<f64>() / n as f64;

    let eps = 1e-4_f64;
    let dx_w = wall_b.0 - wall_a.0;
    let dy_w = wall_b.1 - wall_a.1;
    let is_horizontal = dy_w.abs() < eps;
    let is_vertical = dx_w.abs() < eps;
    if !(is_horizontal ^ is_vertical) {
        // Diagonal or degenerate wall — no axis-aligned interior offset.
        return None;
    }

    if is_horizontal {
        let direction = (centroid_y - mid_y).signum();
        if direction == 0.0 {
            return None;
        }
        let mut closest: Option<f64> = None;
        for i in 0..n {
            let a = polygon[i];
            let b = polygon[(i + 1) % n];
            if (a.1 - b.1).abs() > eps {
                continue; // not horizontal
            }
            let x_min = a.0.min(b.0);
            let x_max = a.0.max(b.0);
            if mid_x < x_min - eps || mid_x > x_max + eps {
                continue;
            }
            let edge_dy = a.1 - mid_y;
            if edge_dy.abs() < eps {
                continue; // the wall itself
            }
            if edge_dy.signum() != direction {
                continue;
            }
            let dist = edge_dy.abs();
            if closest.map_or(true, |d| dist < d) {
                closest = Some(dist);
            }
        }
        let depth = closest?;
        Some((0.0, direction * depth * 0.5))
    } else {
        let direction = (centroid_x - mid_x).signum();
        if direction == 0.0 {
            return None;
        }
        let mut closest: Option<f64> = None;
        for i in 0..n {
            let a = polygon[i];
            let b = polygon[(i + 1) % n];
            if (a.0 - b.0).abs() > eps {
                continue;
            }
            let y_min = a.1.min(b.1);
            let y_max = a.1.max(b.1);
            if mid_y < y_min - eps || mid_y > y_max + eps {
                continue;
            }
            let edge_dx = a.0 - mid_x;
            if edge_dx.abs() < eps {
                continue;
            }
            if edge_dx.signum() != direction {
                continue;
            }
            let dist = edge_dx.abs();
            if closest.map_or(true, |d| dist < d) {
                closest = Some(dist);
            }
        }
        let depth = closest?;
        Some((direction * depth * 0.5, 0.0))
    }
}

/// Draw a dashed line between two screen points. ~6px dash, ~4px gap.
pub(crate) fn draw_dashed_segment(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    stroke: egui::Stroke,
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 1.0 {
        return;
    }
    let dash = 6.0_f32;
    let gap = 4.0_f32;
    let cycle = dash + gap;
    let n_cycles = (length / cycle).ceil() as usize;
    let ux = dx / length;
    let uy = dy / length;
    for i in 0..n_cycles {
        let t0 = (i as f32 * cycle).min(length);
        let t1 = (t0 + dash).min(length);
        if t1 <= t0 {
            break;
        }
        let p0 = egui::pos2(from.x + ux * t0, from.y + uy * t0);
        let p1 = egui::pos2(from.x + ux * t1, from.y + uy * t1);
        painter.line_segment([p0, p1], stroke);
    }
}

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
pub(crate) fn snap_kind_label(kind: manip_2d_plan::SnapKind) -> &'static str {
    match kind {
        manip_2d_plan::SnapKind::ToVertex | manip_2d_plan::SnapKind::ToCorner => "corner",
        manip_2d_plan::SnapKind::ToEdge => "edge",
        manip_2d_plan::SnapKind::ToPreviousOutline => "prev outline",
        manip_2d_plan::SnapKind::ToParallelWall => "parallel wall",
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

/// Render a marker at each elective snap of the last action: orange when applied,
/// grey when currently un-snapped. The clickable label/menu is an overlay widget
/// (see the sketch page) placed at the same point.
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
        let color = if tag.current_kind.is_some() {
            egui::Color32::from_rgb(255, 140, 0) // orange — applied
        } else {
            egui::Color32::from_gray(150) // grey — un-snapped
        };
        painter.circle_filled(p, 5.0, color);
        painter.circle_stroke(p, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
    }
}

/// Render committed tagged areas (construction-phase or floor-step regions):
/// each ring tinted `color` with its label at the centroid, so the assessor
/// sees which footprint carries which tag. `regions` is
/// `(ring_world_coords, label)`.
#[allow(clippy::too_many_arguments)]
pub fn render_phase_regions(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    regions: &[(Vec<(f64, f64)>, String)],
    color: egui::Color32,
) {
    if regions.is_empty() {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };
    for (region, label) in regions {
        if region.len() < 3 {
            continue;
        }
        let pts: Vec<egui::Pos2> = region.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let n = pts.len();
        for k in 0..n {
            painter.line_segment([pts[k], pts[(k + 1) % n]], egui::Stroke::new(3.0, color));
        }
        let (mut cx, mut cy) = (0.0f32, 0.0f32);
        for p in &pts {
            cx += p.x;
            cy += p.y;
        }
        let c = egui::pos2(cx / n as f32, cy / n as f32);
        painter.text(
            c,
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(14.0),
            color,
        );
    }
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

/// Colour each outline wall's exposure SECTIONS: orange = exposed, amber =
/// semi-exposed, slate-blue = non-heat-loss. In exposure mode every wall is
/// shown faint grey first (so un-marked gaps read as grey), the selected wall's
/// two section handles are drawn, and a legend is shown. Outside the mode only
/// the coloured sections are drawn.
#[allow(clippy::too_many_arguments)]
pub fn render_wall_exposure(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
    walls: &[WallExposureView],
    selection: Option<&ExposureSelection>,
    exposure_mode: bool,
) {
    if walls.is_empty() {
        return;
    }
    let any_set = walls.iter().any(|w| !w.sections.is_empty());
    if !exposure_mode && !any_set {
        return;
    }
    let Some((_storey, t)) = storey_transform(rect, ber, current_storey_idx, sketch_mode, view)
    else {
        return;
    };

    let exposed = egui::Color32::from_rgb(255, 140, 40);
    let semi = egui::Color32::from_rgb(240, 205, 40);
    let non = egui::Color32::from_rgb(95, 145, 215);
    let grey = egui::Color32::from_rgb(150, 150, 150);
    let width = if exposure_mode { 5.0 } else { 3.5 };
    let color_of = |e: WallExposure| match e {
        WallExposure::Exposed => exposed,
        WallExposure::SemiExposed => semi,
        WallExposure::NonHeatLoss => non,
    };
    // Point at distance `d` (metres) along wall a→b of length `len`, in screen.
    let along = |a: (f64, f64), b: (f64, f64), len: f32, d: f32| {
        let f = if len > 1e-6 { (d / len) as f64 } else { 0.0 };
        t.apply(a.0 + f * (b.0 - a.0), a.1 + f * (b.1 - a.1))
    };

    for w in walls {
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
        let legend = [
            ("Exposed", exposed),
            ("Semi-exposed", semi),
            ("Non-heat-loss", non),
            ("Unset", grey),
        ];
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
fn previous_outline_ring(storey: &datatypes::ber::structural::Storey) -> Option<Vec<(f64, f64)>> {
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
pub(crate) fn union_bbox(
    a: Option<((f64, f64), (f64, f64))>,
    b: Option<((f64, f64), (f64, f64))>,
) -> Option<((f64, f64), (f64, f64))> {
    match (a, b) {
        (Some(a), Some(b)) => Some((
            (a.0 .0.min(b.0 .0), a.0 .1.min(b.0 .1)),
            (a.1 .0.max(b.1 .0), a.1 .1.max(b.1 .1)),
        )),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

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

/// Shared color palette + opacity constants for all renderers.
pub(crate) mod style {
    use eframe::egui::Color32;

    pub const FLOOR_FILL: Color32 = Color32::from_rgba_premultiplied(40, 70, 110, 50);
    pub const FLOOR_LINE: Color32 = Color32::from_rgb(120, 170, 240);
    pub const WALL_LINE: Color32 = Color32::from_rgb(230, 230, 230);
    pub const ROOF_LINE: Color32 = Color32::from_rgb(220, 160, 80);
    pub const WINDOW_FILL: Color32 = Color32::from_rgba_premultiplied(130, 200, 230, 180);
    pub const DOOR_FILL: Color32 = Color32::from_rgba_premultiplied(180, 130, 80, 200);
    pub const DIMENSION_TEXT: Color32 = Color32::from_rgb(200, 200, 200);
    pub const HINT_TEXT: Color32 = Color32::from_rgb(140, 140, 140);
    pub const FADED_OPACITY: u8 = 50;

    pub fn faded(c: Color32) -> Color32 {
        let [r, g, b, _] = c.to_array();
        Color32::from_rgba_premultiplied(r, g, b, FADED_OPACITY)
    }
}

/// Linear 2D transform: source-space (meters/pixels) → screen-space (px).
/// Preserves aspect ratio; centres the source bounding box inside the dest
/// rect. The user's pan/zoom (`ViewParams`) is folded in: `zoom` multiplies
/// the auto-fit scale, and `offset_x/y` shift the final screen position.
pub(crate) struct Transform2d {
    scale: f64,
    cx: f64, // dest centre x (px)
    cy: f64, // dest centre y (px)
    sx: f64, // source centre x
    sy: f64, // source centre y
    user_offset_x: f64,
    user_offset_y: f64,
}

impl Transform2d {
    pub fn apply(&self, x: f64, y: f64) -> egui::Pos2 {
        egui::pos2(
            (self.cx + (x - self.sx) * self.scale + self.user_offset_x) as f32,
            (self.cy + (y - self.sy) * self.scale + self.user_offset_y) as f32,
        )
    }

    /// Inverse of `apply`: take a canvas-pixel point and return the
    /// corresponding world-coord point. Used by Edit-mode hit-testing.
    pub fn inverse(&self, px: f32, py: f32) -> (f64, f64) {
        let x = (px as f64 - self.cx - self.user_offset_x) / self.scale + self.sx;
        let y = (py as f64 - self.cy - self.user_offset_y) / self.scale + self.sy;
        (x, y)
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }
}

/// The pre-zoom world→pixel scale that fits `min_xy..max_xy` inside `rect` with
/// padding (aspect-ratio preserving). Exposed so callers that need several
/// sub-views to share one scale — e.g. the "All storeys" overview, where a
/// common scale lets the reader compare storey sizes — can compute the shared
/// value before building each transform.
pub(crate) fn auto_fit_scale(rect: egui::Rect, min_xy: (f64, f64), max_xy: (f64, f64)) -> f64 {
    let pad = 30.0_f32;
    let target_w = (rect.width() - 2.0 * pad).max(1.0);
    let target_h = (rect.height() - 2.0 * pad).max(1.0);
    let src_w = (max_xy.0 - min_xy.0).max(1e-6);
    let src_h = (max_xy.1 - min_xy.1).max(1e-6);
    (target_w as f64 / src_w).min(target_h as f64 / src_h)
}

/// Compute a transform that fits a source bounding box inside `rect` with
/// padding, then applies the user's `view` transform (zoom + pan) on top.
pub(crate) fn fit_transform(
    rect: egui::Rect,
    min_xy: (f64, f64),
    max_xy: (f64, f64),
    view: ViewParams,
) -> Transform2d {
    let auto_scale = auto_fit_scale(rect, min_xy, max_xy);

    Transform2d {
        scale: auto_scale * view.zoom as f64,
        cx: rect.center().x as f64,
        cy: rect.center().y as f64,
        sx: (min_xy.0 + max_xy.0) * 0.5,
        sy: (min_xy.1 + max_xy.1) * 0.5,
        user_offset_x: view.offset_x as f64,
        user_offset_y: view.offset_y as f64,
    }
}

/// Shared preamble for the single-storey `render_*` overlays: requires 2D mode,
/// a loaded BER, a selected storey, and a fittable bbox — then returns that
/// storey together with its fit transform. Each overlay adds its own
/// collection-empty early-out on top. `None` means "nothing to draw, return".
fn storey_transform(
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    sketch_mode: SketchMode,
    view: ViewParams,
) -> Option<(&datatypes::ber::structural::Storey, Transform2d)> {
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

/// Orbit-parameterized orthographic projection (the 3D camera): rotate the
/// world by `yaw_deg` about the vertical (z-up) axis, tilt by `pitch_deg`
/// (camera elevation above the horizon — 90° = top-down plan), drop depth.
/// World (x, y, z) → 2D plane (x', y'), y' down. Used by `storey_3d` and
/// `full_house_3d`.
///
/// At the default angles (yaw 45°, pitch atan(1/√2) ≈ 35.264°) this equals
/// the historical fixed isometric `((x − y)·cos30°, (x + y)·sin30° − z)`
/// scaled by a uniform √(3/2), which the auto-fit cancels — so the default
/// view renders identically to the old fixed projection.
///
/// Rotation happens about the world origin; the resulting uniform translation
/// of the projected bbox is absorbed by `fit_transform` centring on that
/// bbox, so no explicit centroid pivot is needed.
pub(crate) fn project_3d(x: f64, y: f64, z: f64, yaw_deg: f32, pitch_deg: f32) -> (f64, f64) {
    let yaw = (yaw_deg as f64).to_radians();
    let pitch = (pitch_deg as f64).to_radians();
    // Yaw about the vertical axis.
    let u = x * yaw.cos() - y * yaw.sin();
    let v = x * yaw.sin() + y * yaw.cos();
    // Pitch: foreshorten the receding axis, mix in height.
    (u, v * pitch.sin() - z * pitch.cos())
}

/// Returns `base` if `target` should be highlighted, else a faded variant.
pub(crate) fn color_for(
    target: DisplayMode,
    current: DisplayMode,
    base: egui::Color32,
) -> egui::Color32 {
    match current {
        DisplayMode::AllSurfaces | DisplayMode::FloorMapDims => base,
        other if other == target => base,
        _ => style::faded(base),
    }
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
        use datatypes::ber::common::ItemId;
        use datatypes::ber::structural::{FloorRegions, Storey};
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
