//! Top-down 2D plan rendering of a single Storey.
//!
//! Floor polygons are drawn as filled shapes with their outline; walls (when
//! populated) draw on top of the matching floor edges; roof regions overlay
//! as dashed lines; windows and doors render as small rectangles positioned
//! on the relevant wall. Dimension labels appear on each edge mid-point;
//! roof-height labels at each floor corner.

use std::collections::HashSet;

use ber_model::ber::common::ItemId;
use ber_model::ber::fenestration::{Door, Window};
use ber_model::ber::structural::{CeilingRegion, FloorRegion, Storey};
use domain::sketch::DisplayMode;
use eframe::egui;
use geo::Coord;
use manip_plan::CollinearitySuggestion;

use super::{color_for, draw_dashed_segment, fit_transform, style, Transform2d, ViewParams};

/// Render the storey. If `highlight` is set, the two edges named in the
/// suggestion are drawn on top in a contrasting style so the user can see
/// which walls are being asked about.
///
/// `watermark_ring` is an optional outline (the PREVIOUS storey's footprint)
/// drawn first in a faint grey dashed style — a drawing aid when starting a
/// new storey on top of an existing one. It also contributes to the fit
/// bounding box, so an empty current storey still gets a sensible auto-fit.
///
/// `fit_bbox_override`, when set, is used as the fit bounding box instead of the
/// storey's own (∪ watermark). The "All storeys" overview passes the GLOBAL
/// bbox (union of every storey) here so all cells share one world→pixel mapping
/// — same scale AND same anchor — making relative sizes and shared walls line
/// up across cells rather than each storey floating centered in its own cell.
#[allow(clippy::too_many_arguments)]
pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    storey: &Storey,
    display_mode: DisplayMode,
    highlight: Option<&CollinearitySuggestion>,
    dimensioned_walls: &[(usize, usize)],
    view: ViewParams,
    watermark_ring: Option<&[(f64, f64)]>,
    fit_bbox_override: Option<((f64, f64), (f64, f64))>,
    // Region ids the assessor has SELECTED in the Structure phase — these are
    // shaded; everything else draws at its normal style. Empty outside that
    // phase / with nothing selected.
    selected: &HashSet<ItemId>,
) {
    // Build the fit bbox from BOTH the storey and the watermark — a new (empty)
    // storey has no bbox of its own; the previous-storey watermark supplies one
    // so the canvas isn't blank-with-a-floating-watermark. An explicit override
    // (the All-storeys global bbox) wins over both.
    let bb = match fit_bbox_override {
        Some(o) => Some(o),
        None => {
            let storey_bb = bounding_box(storey);
            let watermark_bb = watermark_ring.and_then(ring_bbox);
            super::union_bbox(storey_bb, watermark_bb)
        }
    };
    let Some((min_xy, max_xy)) = bb else {
        // The mod-level dispatch already prints "no floor plan yet"; nothing to do.
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy, view);

    // Watermark FIRST so the live geometry paints on top.
    if let Some(ring) = watermark_ring {
        draw_watermark_ring(painter, &t, ring);
    }

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

    // Dimension labels on user-dimensioned floor edges. Walls whose length
    // is still derived from un-set pixel proportions get no label — the
    // canvas only shows real, user-confirmed measurements.
    if matches!(
        display_mode,
        DisplayMode::FloorMapDims | DisplayMode::AllSurfaces
    ) {
        for (region_idx, region) in storey.floors.regions.iter().enumerate() {
            // Edge indices in this region the user has dimensioned.
            let labelled_edges: Vec<usize> = dimensioned_walls
                .iter()
                .filter(|(r, _)| *r == region_idx)
                .map(|(_, e)| *e)
                .collect();
            if !labelled_edges.is_empty() {
                draw_dimensions(painter, &t, region, &labelled_edges);
            }
        }
    }

    // Ceiling-height labels at each roof-region corner, from the region's
    // per-vertex z (metres above the storey's floor datum). Roof regions only
    // exist once the assessor has entered a ceiling height — before that,
    // nothing is drawn (no made-up heights on the canvas). Corners are
    // DEDUPED by rounded (x, y) across regions: a fanned ceiling (roof apex)
    // splits into many triangles sharing vertices, which would otherwise
    // stack identical labels on top of each other. First region wins.
    if matches!(
        display_mode,
        DisplayMode::Roofs | DisplayMode::AllSurfaces | DisplayMode::FloorMapDims
    ) {
        let mut seen_corners: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
        for region in &storey.roofs.regions {
            draw_corner_height_labels(painter, &t, region, &mut seen_corners);
        }
    }

    // Collinearity-review highlight: paint the two candidate edges on top in
    // a bright contrasting color so the user can see what they're being asked.
    // Each edge lives in a (possibly different) ring → look up the region by
    // suggestion's ring index. Cross-ring suggestions highlight one edge in
    // each polygon.
    if let Some(s) = highlight {
        if let Some(region) = storey.floors.regions.get(s.ring_a) {
            draw_highlight_edge(painter, &t, region, s.edge_a);
        }
        if let Some(region) = storey.floors.regions.get(s.ring_b) {
            draw_highlight_edge(painter, &t, region, s.edge_b);
        }
    }

    // Structure pre-edit SELECTION (on TOP of everything): a bright marquee
    // around each selected surface's footprint — shows the selected AREA without
    // recolouring the surfaces. Empty (no-op) during an edit session, which
    // colours per group instead.
    draw_selection_overlay(painter, &t, storey, selected);
}

/// The pre-edit selection indicator: a bright gold dashed outline around every
/// SELECTED surface (floor/roof footprint, or a wall's base edge), drawn last so
/// it sits on top of all the surface layers. No fills — it marks the selected
/// AREA, it doesn't re-colour the surfaces.
fn draw_selection_overlay(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    selected: &HashSet<ItemId>,
) {
    if selected.is_empty() {
        return;
    }
    for region in &storey.floors.regions {
        if selected.contains(&region.id) {
            let mut pts: Vec<egui::Pos2> = region
                .polygon
                .exterior()
                .coords()
                .map(|c| t.apply(c.x, c.y))
                .collect();
            if pts.len() >= 2 && pts[0] == pts[pts.len() - 1] {
                pts.pop();
            }
            draw_selection_outline(painter, &pts);
        }
    }
    for region in &storey.roofs.regions {
        if selected.contains(&region.id) {
            let pts: Vec<egui::Pos2> = region
                .polygon
                .exterior
                .iter()
                .map(|p| t.apply(p.x, p.y))
                .collect();
            draw_selection_outline(painter, &pts);
        }
    }
    for wall in &storey.walls.regions {
        if selected.contains(&wall.id) {
            let v = &wall.polygon.exterior;
            if v.len() >= 2 {
                super::draw_dashed_segment(
                    painter,
                    t.apply(v[0].x, v[0].y),
                    t.apply(v[1].x, v[1].y),
                    egui::Stroke::new(4.0, style::SELECTION_LINE),
                );
            }
        }
    }
}

fn draw_highlight_edge(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &FloorRegion,
    edge_start: usize,
) {
    let coords: Vec<Coord<f64>> = region.polygon.exterior().coords().cloned().collect();
    // The polygon's exterior may or may not include the closing duplicate;
    // for indexing, we want the unique vertex count.
    let n_unique = if coords.len() >= 2 && coords[0] == coords[coords.len() - 1] {
        coords.len() - 1
    } else {
        coords.len()
    };
    if n_unique < 3 {
        return;
    }

    let highlight = egui::Color32::from_rgb(255, 140, 30); // warm orange
    let stroke = egui::Stroke::new(4.0, highlight);

    let a = coords[edge_start % n_unique];
    let b = coords[(edge_start + 1) % n_unique];
    let pa = t.apply(a.x, a.y);
    let pb = t.apply(b.x, b.y);
    painter.line_segment([pa, pb], stroke);
}

pub(crate) fn ring_bbox(ring: &[(f64, f64)]) -> Option<((f64, f64), (f64, f64))> {
    let mut it = ring.iter();
    let first = *it.next()?;
    let (mut min_x, mut min_y) = first;
    let (mut max_x, mut max_y) = first;
    for &(x, y) in it {
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    Some(((min_x, min_y), (max_x, max_y)))
}

/// Faint grey dashed outline of the previous storey, drawn behind the live
/// geometry as a drawing aid when starting a new storey on top.
fn draw_watermark_ring(painter: &egui::Painter, t: &Transform2d, ring: &[(f64, f64)]) {
    if ring.len() < 2 {
        return;
    }
    let grey = egui::Color32::from_rgb(150, 150, 150);
    let stroke = egui::Stroke::new(2.0, grey);
    let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
    let n = pts.len();
    for k in 0..n {
        draw_dashed_segment(painter, pts[k], pts[(k + 1) % n], stroke);
    }
}

pub(crate) fn bounding_box(storey: &Storey) -> Option<((f64, f64), (f64, f64))> {
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
    // The usual faint fill (faded when another surface kind is the focus). The
    // SELECTION is shown by the gold marquee overlay (`draw_selection_overlay`),
    // not by recolouring the surface fill.
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

/// A bright dashed marquee around a SELECTED region's screen ring — the
/// temporary "this is what's selected" outline shown before "Edit attributes"
/// opens (the edit session has its own per-group colouring, so this only paints
/// pre-edit, where `selected` is non-empty).
fn draw_selection_outline(painter: &egui::Painter, ring: &[egui::Pos2]) {
    let n = ring.len();
    if n < 2 {
        return;
    }
    // Inset so the marquee sits just inside the surface edges (it'd otherwise
    // coincide with the perimeter outline of a whole-outline floor and vanish).
    let ring = super::inset_ring(ring, 5.0);
    let stroke = egui::Stroke::new(3.0, style::SELECTION_LINE);
    for i in 0..n {
        super::draw_dashed_segment(painter, ring[i], ring[(i + 1) % n], stroke);
    }
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
    // Only draw the dedicated wall stroke when it should be visible; otherwise
    // the floor outline already shows the edges and we don't want to double-stroke.
    if !matches!(display_mode, DisplayMode::Walls | DisplayMode::AllSurfaces) {
        return;
    }
    let wall_color = color_for(DisplayMode::Walls, display_mode, style::WALL_LINE);
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
    // The SELECTION is shown by the gold marquee overlay, not a fill here.
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
    let color = color_for(DisplayMode::Walls, display_mode, style::WINDOW_FILL);
    draw_opening_footprint(painter, t, &window.vertices, color);
}

fn draw_door(painter: &egui::Painter, t: &Transform2d, door: &Door, display_mode: DisplayMode) {
    if door.vertices.len() < 4 {
        return;
    }
    let color = color_for(DisplayMode::Walls, display_mode, style::DOOR_FILL);
    draw_opening_footprint(painter, t, &door.vertices, color);
}

/// Draw an opening's XY footprint. A ROOF opening projects to a real rectangle
/// (filled); a WALL opening lies in the vertical wall plane, so its footprint
/// collapses to a line along the wall — drawn as a THICK coloured span so it
/// reads as a marked opening on the plan (a degenerate polygon would be
/// invisible under the wall edge).
fn draw_opening_footprint(
    painter: &egui::Painter,
    t: &Transform2d,
    vertices: &[(f32, f32, f32)],
    color: egui::Color32,
) {
    let pts: Vec<egui::Pos2> = vertices
        .iter()
        .map(|(x, y, _z)| t.apply(*x as f64, *y as f64))
        .collect();
    // Screen-space area: a real rectangle (roof window) has positive area; a
    // wall opening's collinear footprint is ~0.
    let n = pts.len();
    let mut area2 = 0.0_f32;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        area2 += a.x * b.y - b.x * a.y;
    }
    if area2.abs() * 0.5 > 4.0 {
        painter.add(egui::Shape::convex_polygon(
            pts,
            color,
            egui::Stroke::new(1.0, color),
        ));
    } else {
        // Wall opening: a thick span between the base endpoints (exterior[0..1]).
        painter.line_segment([pts[0], pts[1]], egui::Stroke::new(5.0, color));
    }
}

fn draw_dimensions(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &FloorRegion,
    edges_to_label: &[usize],
) {
    let coords: Vec<Coord<f64>> = region.polygon.exterior().coords().cloned().collect();
    let n = coords.len();
    if n < 2 {
        return;
    }
    // `geo::Polygon::exterior` returns vertices with the first one repeated at
    // the end to close the ring, so the actual edge count is `n - 1`.
    let edge_count = n - 1;
    for &edge_idx in edges_to_label {
        if edge_idx >= edge_count {
            continue;
        }
        let a = coords[edge_idx];
        let b = coords[edge_idx + 1];
        let dx = b.x - a.x;
        let dy = b.y - a.y;
        let length = (dx * dx + dy * dy).sqrt();
        if length < 1e-3 {
            continue;
        }
        let mid_x = (a.x + b.x) * 0.5;
        let mid_y = (a.y + b.y) * 0.5;
        let mid = t.apply(mid_x, mid_y);
        // Polygon is in meters after the first user dimension; before that,
        // no walls are in `edges_to_label` so the unit label is always meters here.
        painter.text(
            mid,
            egui::Align2::CENTER_CENTER,
            format!("{:.2} m", length),
            egui::FontId::monospace(11.0),
            style::DIMENSION_TEXT,
        );
    }
}

fn draw_corner_height_labels(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &CeilingRegion,
    seen: &mut std::collections::HashSet<(i64, i64)>,
) {
    // Each roof vertex carries its own height as the polygon's z. `seen`
    // dedupes shared corners across regions by (x, y) rounded to the mm —
    // fan triangles and step pieces meet at common vertices.
    for v in &region.polygon.exterior {
        let key = ((v.x * 1000.0).round() as i64, (v.y * 1000.0).round() as i64);
        if !seen.insert(key) {
            continue;
        }
        let pos = t.apply(v.x, v.y) + egui::Vec2::new(0.0, -12.0);
        painter.text(
            pos,
            egui::Align2::CENTER_BOTTOM,
            format!("h={:.2}m", v.z),
            egui::FontId::proportional(9.0),
            style::ROOF_LINE,
        );
    }
}
