//! Top-down 2D plan rendering of a single Storey.
//!
//! Floor polygons are drawn as filled shapes with their outline; walls (when
//! populated) draw on top of the matching floor edges; roof regions overlay
//! as dashed lines; windows and doors render as small rectangles positioned
//! on the relevant wall. Dimension labels appear on each edge mid-point;
//! roof-height labels at each floor corner.

use datatypes::ber::fenestration::{Door, Window};
use datatypes::ber::structural::{FloorRegion, Storey};
use domain::sketch::DisplayMode;
use eframe::egui;
use geo::Coord;

use super::{color_for, fit_transform, style, Transform2d};

pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    storey: &Storey,
    display_mode: DisplayMode,
) {
    let Some((min_xy, max_xy)) = bounding_box(storey) else {
        // The mod-level dispatch already prints "no floor plan yet"; nothing to do.
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy);

    // Floors (background layer).
    for region in &storey.floors.regions {
        draw_floor(painter, &t, region, display_mode);
    }

    // Walls (when populated). For now we treat each floor edge as an implied wall
    // and highlight them when DisplayMode::Walls is active.
    for region in &storey.floors.regions {
        draw_wall_edges(painter, &t, region, display_mode);
    }

    // Roof regions (when populated) — drawn as dashed outlines above the floor.
    // Today Storey.roofs.regions is empty by default; this loop renders nothing
    // until the controller populates ceilings.
    if !storey.roofs.regions.is_empty() {
        draw_roof_overlay(painter, &t, storey, display_mode);
    }

    // Windows + doors. Roof windows (windows whose vertices form a horizontal quad)
    // are drawn as translucent rectangles since they're visible looking down.
    for window in &storey.windows {
        draw_window(painter, &t, window, display_mode);
    }
    for door in &storey.doors {
        draw_door(painter, &t, door, display_mode);
    }

    // Dimension labels on each floor edge.
    if matches!(
        display_mode,
        DisplayMode::FloorMapDims | DisplayMode::AllSurfaces
    ) {
        for region in &storey.floors.regions {
            draw_dimensions(painter, &t, region);
        }
    }

    // Roof-height labels at each floor corner.
    //
    // TODO: when CeilingRegion data is populated, look up the matching ceiling
    // vertex's z-coordinate per corner instead of using the floor's bulk
    // `height` field (which represents floor-to-ceiling distance, flat).
    if matches!(
        display_mode,
        DisplayMode::Roofs | DisplayMode::AllSurfaces | DisplayMode::FloorMapDims
    ) {
        for region in &storey.floors.regions {
            draw_corner_height_labels(painter, &t, region);
        }
    }
}

fn bounding_box(storey: &Storey) -> Option<((f64, f64), (f64, f64))> {
    let mut iter = storey
        .floors
        .regions
        .iter()
        .flat_map(|r| r.polygon.exterior().coords());
    let first = iter.next()?;
    let (mut min_x, mut min_y) = (first.x, first.y);
    let (mut max_x, mut max_y) = (first.x, first.y);
    for c in iter {
        min_x = min_x.min(c.x);
        max_x = max_x.max(c.x);
        min_y = min_y.min(c.y);
        max_y = max_y.max(c.y);
    }
    Some(((min_x, min_y), (max_x, max_y)))
}

fn draw_floor(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &FloorRegion,
    display_mode: DisplayMode,
) {
    // Pull unique exterior vertices (drop a trailing closing duplicate if geo
    // included one — `earcutr` expects each vertex once around the ring).
    let mut coords: Vec<_> = region.polygon.exterior().coords().cloned().collect();
    if coords.len() >= 2 && coords[0] == coords[coords.len() - 1] {
        coords.pop();
    }
    if coords.len() < 3 {
        return;
    }

    let screen_pts: Vec<egui::Pos2> = coords.iter().map(|c| t.apply(c.x, c.y)).collect();
    let fill = color_for(DisplayMode::Floors, display_mode, style::FLOOR_FILL);
    let line_color = color_for(DisplayMode::Floors, display_mode, style::FLOOR_LINE);

    // Triangulate with earcutr so concave shapes (L, U, etc.) fill correctly.
    // egui's `convex_polygon` does a fan triangulation that cuts across the
    // concave region and produces visible triangle artifacts on non-convex
    // polygons.
    let flat: Vec<f64> = coords.iter().flat_map(|c| [c.x, c.y]).collect();
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

    // Outline drawn separately so the stroke stays crisp regardless of fill mode.
    painter.add(egui::Shape::closed_line(
        screen_pts,
        egui::Stroke::new(2.0, line_color),
    ));
}

fn draw_wall_edges(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &FloorRegion,
    display_mode: DisplayMode,
) {
    let coords: Vec<Coord<f64>> = region.polygon.exterior().coords().cloned().collect();
    if coords.len() < 2 {
        return;
    }
    let wall_color = color_for(DisplayMode::Walls, display_mode, style::WALL_LINE);
    // Only draw the dedicated wall stroke when it should be visible; otherwise
    // the floor outline already shows the edges and we don't want to double-stroke.
    if !matches!(display_mode, DisplayMode::Walls | DisplayMode::AllSurfaces) {
        return;
    }
    let stroke = egui::Stroke::new(2.5, wall_color);
    for i in 0..(coords.len() - 1) {
        let a = t.apply(coords[i].x, coords[i].y);
        let b = t.apply(coords[i + 1].x, coords[i + 1].y);
        painter.line_segment([a, b], stroke);
    }
}

fn draw_roof_overlay(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    display_mode: DisplayMode,
) {
    let color = color_for(DisplayMode::Roofs, display_mode, style::ROOF_LINE);
    let stroke = egui::Stroke::new(1.0, color);
    for region in &storey.roofs.regions {
        let pts: Vec<egui::Pos2> = region
            .polygon
            .exterior
            .iter()
            .map(|p| t.apply(p.x, p.y))
            .collect();
        if pts.len() >= 3 {
            painter.add(egui::Shape::closed_line(pts, stroke));
        }
    }
}

fn draw_window(
    painter: &egui::Painter,
    t: &Transform2d,
    window: &Window,
    display_mode: DisplayMode,
) {
    if window.vertices.len() < 4 {
        return;
    }
    // Project the quad's XY footprint (works whether it's a wall window or a
    // roof window — for wall windows the footprint degenerates to a thin line,
    // for roof windows it's a real rectangle).
    let pts: Vec<egui::Pos2> = window
        .vertices
        .iter()
        .map(|(x, y, _z)| t.apply(*x as f64, *y as f64))
        .collect();
    let fill = color_for(DisplayMode::Walls, display_mode, style::WINDOW_FILL);
    painter.add(egui::Shape::convex_polygon(
        pts,
        fill,
        egui::Stroke::new(1.0, style::WINDOW_FILL),
    ));
}

fn draw_door(painter: &egui::Painter, t: &Transform2d, door: &Door, display_mode: DisplayMode) {
    if door.vertices.len() < 4 {
        return;
    }
    let pts: Vec<egui::Pos2> = door
        .vertices
        .iter()
        .map(|(x, y, _z)| t.apply(*x as f64, *y as f64))
        .collect();
    let fill = color_for(DisplayMode::Walls, display_mode, style::DOOR_FILL);
    painter.add(egui::Shape::convex_polygon(
        pts,
        fill,
        egui::Stroke::new(1.0, style::DOOR_FILL),
    ));
}

fn draw_dimensions(painter: &egui::Painter, t: &Transform2d, region: &FloorRegion) {
    let coords: Vec<Coord<f64>> = region.polygon.exterior().coords().cloned().collect();
    let n = coords.len();
    if n < 2 {
        return;
    }
    for i in 0..(n - 1) {
        let a = coords[i];
        let b = coords[i + 1];
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let length = (dx * dx + dy * dy).sqrt();
        if length < 1.0 {
            continue;
        }
        let mid_x = (a.x + b.x) * 0.5;
        let mid_y = (a.y + b.y) * 0.5;
        let mid = t.apply(mid_x, mid_y);
        // TODO: format as meters once a pixel→meter scale is set on the controller.
        painter.text(
            mid,
            egui::Align2::CENTER_CENTER,
            format!("{:.1}", length),
            egui::FontId::monospace(10.0),
            style::DIMENSION_TEXT,
        );
    }
}

fn draw_corner_height_labels(painter: &egui::Painter, t: &Transform2d, region: &FloorRegion) {
    let h = region.height;
    if h.abs() < 1e-6 {
        return;
    }
    let coords: Vec<Coord<f64>> = region.polygon.exterior().coords().cloned().collect();
    // Exterior closes (last point == first); skip the duplicate.
    let unique = &coords[..coords.len().saturating_sub(1)];
    for c in unique {
        let pos = t.apply(c.x, c.y) + egui::Vec2::new(0.0, -12.0);
        painter.text(
            pos,
            egui::Align2::CENTER_BOTTOM,
            format!("h={:.2}m", h),
            egui::FontId::proportional(9.0),
            style::ROOF_LINE,
        );
    }
}
