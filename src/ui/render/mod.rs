//! Renders the persisted BER state as 2D / 3D views in the sketch canvas.
//!
//! Three rendering paths, selected by [`SketchMode`]:
//!   - `Storey2D`  — top-down 2D plan of the current storey
//!   - `Storey3D`  — axonometric 3D of the current storey
//!   - `FullHouse` — axonometric 3D of all storeys stacked
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

pub mod full_house_3d;
pub mod storey_2d;
pub mod storey_3d;

use datatypes::ber::ber_struct::Ber;
use domain::sketch::{DisplayMode, SketchMode};
use eframe::egui;
use manip_2d_plan::CollinearitySuggestion;

/// User-controlled view transform passed in from the controller. `zoom = 1.0`
/// and zero offsets = pure auto-fit. Higher zoom enlarges; positive offset_x
/// pans the view right.
#[derive(Debug, Clone, Copy, Default)]
pub struct ViewParams {
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

impl ViewParams {
    pub fn identity() -> Self {
        Self {
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

/// Render the appropriate BER view into `rect`. Silent no-op if `ber` is None.
/// `highlight` is the current collinearity-review suggestion (if any) — its
/// two candidate edges get a bright overlay so the user sees what they're
/// being asked about. Only applies to the 2D view.
pub fn render_ber(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: usize,
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

    match sketch_mode {
        SketchMode::Storey2D => {
            if let Some(storey) = ber.storeys.get(current_storey_idx) {
                storey_2d::render(
                    painter,
                    rect,
                    storey,
                    display_mode,
                    highlight,
                    dimensioned_walls,
                    view,
                );
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        SketchMode::Storey3D => {
            if let Some(storey) = ber.storeys.get(current_storey_idx) {
                storey_3d::render(painter, rect, storey, display_mode, view);
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        SketchMode::FullHouse => {
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
    current_storey_idx: usize,
    sketch_mode: SketchMode,
    view: ViewParams,
    wall_a: (f64, f64),
    wall_b: (f64, f64),
) {
    if !matches!(sketch_mode, SketchMode::Storey2D) {
        return;
    }
    let Some(ber) = ber else {
        return;
    };
    let Some(storey) = ber.storeys.get(current_storey_idx) else {
        return;
    };
    let Some((min_xy, max_xy)) = storey_2d::bounding_box(storey) else {
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy, view);

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
fn draw_dashed_segment(
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

    pub fn scale(&self) -> f64 {
        self.scale
    }
}

/// Compute a transform that fits a source bounding box inside `rect` with
/// padding, then applies the user's `view` transform (zoom + pan) on top.
pub(crate) fn fit_transform(
    rect: egui::Rect,
    min_xy: (f64, f64),
    max_xy: (f64, f64),
    view: ViewParams,
) -> Transform2d {
    let pad = 30.0_f32;
    let target_w = (rect.width() - 2.0 * pad).max(1.0);
    let target_h = (rect.height() - 2.0 * pad).max(1.0);
    let src_w = (max_xy.0 - min_xy.0).max(1e-6);
    let src_h = (max_xy.1 - min_xy.1).max(1e-6);
    let auto_scale = (target_w as f64 / src_w).min(target_h as f64 / src_h);

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

/// True isometric projection: rotate around vertical, tilt back, z is up.
/// World (x, y, z) → 2D plane (x', y'). Used by `storey_3d` and `full_house_3d`.
pub(crate) fn project_iso(x: f64, y: f64, z: f64) -> (f64, f64) {
    const COS30: f64 = 0.866_025_403_784_438_6;
    const SIN30: f64 = 0.5;
    let px = (x - y) * COS30;
    let py = (x + y) * SIN30 - z;
    (px, py)
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
