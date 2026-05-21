//! Axonometric 3D rendering of a single Storey.
//!
//! Uses isometric projection (see `super::project_iso`). The floor polygon is
//! extruded upward to the ceiling height; walls are the quads connecting floor
//! and ceiling rings. Roof line is the ceiling polygon at the top.
//!
//! `render_at_z_offset` is the workhorse — `full_house_3d` calls it per storey
//! with a cumulative z offset. The public `render` is just `render_at_z_offset`
//! with `z_offset = 0` and computes its own bounding box.

use datatypes::ber::structural::{FloorRegion, Storey};
use domain::sketch::DisplayMode;
use eframe::egui;

use super::{color_for, fit_transform, project_iso, style, Transform2d, ViewParams};

pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    storey: &Storey,
    display_mode: DisplayMode,
    view: ViewParams,
) {
    let Some((min_xy, max_xy)) = projected_bb_for_storey(storey, 0.0) else {
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy, view);
    render_at_z_offset(painter, &t, storey, display_mode, 0.0);
}

/// Render `storey` with the assumption that its floor sits at world-z `z_offset`.
/// Caller supplies the transform so multi-storey views can share one camera.
pub(crate) fn render_at_z_offset(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    display_mode: DisplayMode,
    z_offset: f64,
) {
    for region in &storey.floors.regions {
        draw_extruded_region(painter, t, region, z_offset, display_mode);
    }
}

/// Projected (post-isometric) 2D bounding box of every point in this storey,
/// including the ceiling ring (floor + height) at the given `z_offset`.
pub(crate) fn projected_bb_for_storey(
    storey: &Storey,
    z_offset: f64,
) -> Option<((f64, f64), (f64, f64))> {
    let mut iter = storey.floors.regions.iter().flat_map(move |r| {
        let z_top = z_offset + r.height;
        let floor = r
            .polygon
            .exterior()
            .coords()
            .map(move |c| project_iso(c.x, c.y, z_offset));
        let ceiling = r
            .polygon
            .exterior()
            .coords()
            .map(move |c| project_iso(c.x, c.y, z_top));
        floor.chain(ceiling)
    });
    let (mut min_x, mut min_y) = iter.next()?;
    let mut max_x = min_x;
    let mut max_y = min_y;
    for (x, y) in iter {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    Some(((min_x, min_y), (max_x, max_y)))
}

fn draw_extruded_region(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &FloorRegion,
    z_offset: f64,
    display_mode: DisplayMode,
) {
    let z_floor = z_offset;
    let z_ceiling = z_offset + region.height;

    let exterior: Vec<_> = region.polygon.exterior().coords().cloned().collect();
    if exterior.len() < 4 {
        return;
    }

    let floor_pts: Vec<egui::Pos2> = exterior
        .iter()
        .map(|c| {
            let (px, py) = project_iso(c.x, c.y, z_floor);
            t.apply(px, py)
        })
        .collect();
    let ceiling_pts: Vec<egui::Pos2> = exterior
        .iter()
        .map(|c| {
            let (px, py) = project_iso(c.x, c.y, z_ceiling);
            t.apply(px, py)
        })
        .collect();

    // Walls: a quad per floor edge, from (floor[i], floor[i+1], ceiling[i+1], ceiling[i]).
    let wall_stroke = egui::Stroke::new(
        1.5,
        color_for(DisplayMode::Walls, display_mode, style::WALL_LINE),
    );
    for i in 0..(exterior.len() - 1) {
        let quad = vec![
            floor_pts[i],
            floor_pts[i + 1],
            ceiling_pts[i + 1],
            ceiling_pts[i],
        ];
        painter.add(egui::Shape::closed_line(quad, wall_stroke));
    }

    // Floor outline (slightly thicker so it reads as the base).
    let floor_color = color_for(DisplayMode::Floors, display_mode, style::FLOOR_LINE);
    painter.add(egui::Shape::closed_line(
        floor_pts,
        egui::Stroke::new(2.0, floor_color),
    ));

    // Ceiling outline (roof line — flat ceiling for now).
    let roof_color = color_for(DisplayMode::Roofs, display_mode, style::ROOF_LINE);
    painter.add(egui::Shape::closed_line(
        ceiling_pts,
        egui::Stroke::new(1.5, roof_color),
    ));
}
