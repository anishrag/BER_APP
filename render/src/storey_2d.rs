//! Top-down 2D plan rendering of a single Storey.
//!
//! Floor polygons are drawn as filled shapes with their outline; walls (when
//! populated) draw on top of the matching floor edges; roof regions overlay
//! as dashed lines; windows and doors render as small rectangles positioned
//! on the relevant wall. Dimension labels appear on each edge mid-point;
//! roof-height labels at each floor corner.

use std::collections::HashSet;

use ber_model::ber::common::ItemId;
use ber_model::ber::fenestration::{Door, OpeningPlacement, Window};
use ber_model::ber::structural::{CeilingRegion, FloorRegion, Storey};
use domain::points::{Point2f32, Point2f64};
use domain::sketch::DisplayMode;
use geo::Coord;

use domain::view::PhaseDisplayRegion;

use crate::{color_for, draw_dashed_segment, fit_transform, style, Transform2d, ViewParams};

/// Render the storey. If `highlight_segments` is set (the collinearity
/// review's two candidate edges, resolved by the CONTROLLER from the live
/// parametric plan), they are drawn on top in a contrasting style so the user
/// can see which walls are being asked about.
///
/// `watermark_ring` is an optional outline (the PREVIOUS storey's footprint)
/// drawn first in a faint grey dashed style — a drawing aid when starting a
/// new storey on top of an existing one. It also contributes to the fit
/// bounding box, so an empty current storey still gets a sensible auto-fit.
///
/// `phase_watermarks` are the previous storey's construction-phase zones
/// (`(ring, label)` pairs), drawn as faint dashed PURPLE rings over the grey
/// outline watermark while the fresh upper storey is being traced — so the
/// assessor can follow (and the stroke can snap to) the phase boundaries
/// below. They lie within the outline watermark's footprint, so they do NOT
/// contribute to the fit bbox. Empty outside the fresh-storey trace.
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
    highlight_segments: Option<[(Point2f32, Point2f32); 2]>,
    // The ACTIVE storey's LIVE outline rings while it's mid-dimensioning on a
    // BUILT storey (controller-supplied): drawn over the committed regions in
    // the same faint floor fill a fresh not-yet-dimensioned outline shows, and
    // included in the fit bbox so the not-yet-dimensioned geometry (an
    // extend's bump at its drawn position) is on screen. None otherwise.
    provisional_rings: Option<&[Vec<(f64, f64)>]>,
    dimensioned_walls: &[(usize, usize)],
    view: ViewParams,
    watermark_ring: Option<&[(f64, f64)]>,
    phase_watermarks: &[PhaseDisplayRegion],
    fit_bbox_override: Option<((f64, f64), (f64, f64))>,
    // Region ids the assessor has SELECTED in the Structure phase — these are
    // shaded; everything else draws at its normal style. Empty outside that
    // phase / with nothing selected.
    selected: &HashSet<ItemId>,
    // Objects-phase plan: a REDUCED view (walls + wall-windows + floors, plus the
    // living-area overlay the canvas adds) — roof outlines, roof windows and
    // ceiling-height labels are suppressed so the plan just gives a sense of place.
    objects_view: bool,
    // Also draw INTERIOR wall regions (risers / step walls) as dashed lines. The
    // single-storey view leaves this OFF — `render_wall_exposure` draws risers
    // there (dashed), so drawing them here too would double them. The all-storeys
    // OVERVIEW (which never calls `render_wall_exposure`) turns it ON so a
    // split-level storey's interior step edges still show.
    draw_interior_walls: bool,
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
            // Mid-dimensioning: the live provisional rings are the
            // authoritative preview — their bbox arrives on the VIEW
            // (`fit_include_bbox`) so this base fit and every overlay pass
            // (which recompute the fit independently via `storey_transform`)
            // union the SAME box and land on one transform. Deriving it here
            // from `provisional_rings` instead would leave the overlays on a
            // smaller box — every prompt/highlight then paints displaced.
            crate::union_bbox(
                crate::union_bbox(storey_bb, watermark_bb),
                view.fit_include_bbox,
            )
        }
    };
    let Some((min_xy, max_xy)) = bb else {
        // The mod-level dispatch already prints "no floor plan yet"; nothing to do.
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy, view);

    // Watermark FIRST so the live geometry paints on top — the grey outline,
    // then the purple phase zones over it.
    if let Some(ring) = watermark_ring {
        draw_watermark_ring(painter, &t, ring);
    }
    draw_phase_watermark_rings(painter, &t, phase_watermarks);

    // Floors (background FILL only). The wall LINES are NOT derived from
    // floor-region edges any more — that re-derivation mis-drew a solid line
    // across a dormer opening (a phase-seam floor region keeps a full-width
    // edge). Walls are drawn from the authoritative `storey.walls.regions`
    // below, which already open at the dormer (flanks only, matching 3D).
    for region in &storey.floors.regions {
        draw_floor(painter, &t, region, display_mode);
    }

    // Walls: the resolved wall regions are the single source of truth (derived
    // from the ONE dimensioned perimeter), so a wall a dormer opened shows only
    // its flanks — no full-width line across the opening, immune to floor-region
    // seams/drift. RISERS (interior step walls) are drawn separately (dashed) by
    // `render_wall_exposure`, so only PERIMETER walls are drawn here: a wall
    // whose base edge lies on the floor outline boundary (a riser's is interior).
    // The 0.05 m boundary tolerance survives the few-cm drift between
    // separately-dimensioned regions. Empty boundary → draw every wall (fallback).
    let boundary = storey.floors.boundary_rings();
    let wall_color = color_for(DisplayMode::Floors, display_mode, style::FLOOR_LINE);
    let wall_stroke = egui::Stroke::new(2.0, wall_color);
    for wall in &storey.walls.regions {
        let Some((a, b)) = wall.base_edge() else {
            continue;
        };
        let (pa, pb) = (t.apply(a.x, a.y), t.apply(b.x, b.y));
        let perimeter = boundary.is_empty() || wall_on_perimeter((a.x, a.y), (b.x, b.y), &boundary);
        if perimeter {
            painter.add(egui::Shape::line_segment([pa, pb], wall_stroke));
        } else if draw_interior_walls {
            // Interior riser/step wall (drawn only in the overview — the
            // single-storey view's exposure overlay owns risers). Dashed to read
            // as an interior division, not an outline wall.
            painter.add(egui::Shape::dashed_line(&[pa, pb], wall_stroke, 6.0, 4.0));
        }
    }

    // PROVISIONAL live outline (mid-dimensioning on a built storey): the live
    // parametric rings at their drawn/estimated positions, filled in the same
    // faint floor style a fresh not-yet-dimensioned outline shows (resolve's
    // bare-floor projection), OVER the committed regions — which stay visible
    // as context. The plan re-solves as each dimension lands, so redrawing per
    // frame gives the "shape amends itself as I dimension" behaviour. Drawn
    // before the review highlight so the questioned walls paint on top of it.
    if let Some(rings) = provisional_rings {
        draw_provisional_rings(painter, &t, rings, display_mode);
    }

    // Roof regions (when populated) — drawn as dashed outlines above the floor.
    // Today Storey.ceilings.regions is empty by default; this loop renders nothing
    // until the controller populates ceilings. Suppressed in the Objects view.
    if !storey.ceilings.regions.is_empty() && !objects_view {
        draw_roof_overlay(painter, &t, storey, display_mode);
    }

    // Windows + doors. Roof windows (windows whose vertices form a horizontal quad)
    // are drawn as translucent rectangles since they're visible looking down. In
    // the Objects view, roof windows are skipped (only wall windows give a sense
    // of place).
    for window in &storey.windows {
        if objects_view && matches!(window.placement, Some(OpeningPlacement::Roof { .. })) {
            continue;
        }
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
        DisplayMode::Ceilings | DisplayMode::AllSurfaces | DisplayMode::FloorMapDims
    ) {
        let mut seen_corners: std::collections::HashSet<(i64, i64)> =
            std::collections::HashSet::new();
        for region in &storey.ceilings.regions {
            draw_corner_height_labels(painter, &t, region, &mut seen_corners);
        }
    }

    // Collinearity-review highlight: paint the two candidate edges on top in
    // a bright contrasting color so the user can see what they're being asked.
    // The segments arrive pre-resolved from the LIVE parametric plan (the
    // controller owns that lookup) — the committed floor regions must NOT be
    // consulted here: they're empty/stale during the deferred-derivation
    // window and their ring/vertex layout differs from the live plan's.
    if let Some(segments) = highlight_segments {
        let highlight = egui::Color32::from_rgb(255, 140, 30); // warm orange
        let stroke = egui::Stroke::new(4.0, highlight);
        for (a, b) in segments {
            let pa = t.apply(a.x as f64, a.y as f64);
            let pb = t.apply(b.x as f64, b.y as f64);
            painter.line_segment([pa, pb], stroke);
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
    for region in &storey.ceilings.regions {
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
    // Perimeter AND porch inner walls — a selected porch wall shows the same
    // dashed pre-edit line as any other wall.
    for wall in storey
        .walls
        .regions
        .iter()
        .chain(storey.porch_walls.regions.iter())
    {
        if selected.contains(&wall.id) {
            // The wall's FULL plan extent, not its base EDGE — a merged wall's
            // edge is a stub, so drawing it showed the selection over only part
            // of the wall while 3D (drawing the real polygon) showed all of it.
            if let Some((wa, wb)) = wall.base_extent() {
                let (pa, pb) = (t.apply(wa.x, wa.y), t.apply(wb.x, wb.y));
                // A wall is only a LINE in plan, drawn over the outline it sits
                // on — a dashed gold line alone is easy to lose among the other
                // lines. Give it a dark HALO to separate it from whatever is
                // under it, and END CAPS so the extent of the selection reads at
                // a glance (the exposure tool's wall selection does the same).
                painter.line_segment(
                    [pa, pb],
                    egui::Stroke::new(9.0, egui::Color32::from_rgba_premultiplied(0, 0, 0, 160)),
                );
                super::draw_dashed_segment(
                    painter,
                    pa,
                    pb,
                    egui::Stroke::new(5.0, style::SELECTION_LINE),
                );
                for p in [pa, pb] {
                    painter.circle_filled(p, 5.0, style::SELECTION_LINE);
                    painter.circle_stroke(
                        p,
                        5.0,
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(40, 40, 40)),
                    );
                }
            }
        }
    }
}

/// Fill the live parametric rings in the same faint floor style the resolve
/// layer's bare-floor projection gives a fresh not-yet-dimensioned outline —
/// so the mid-dimensioning preview of a built storey looks identical to the
/// fresh-draw flow. Earcut per ring (rings are separate sections, not holes).
fn draw_provisional_rings(
    painter: &egui::Painter,
    t: &Transform2d,
    rings: &[Vec<(f64, f64)>],
    display_mode: DisplayMode,
) {
    let fill = color_for(DisplayMode::Floors, display_mode, style::FLOOR_FILL);
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let screen_pts: Vec<egui::Pos2> = ring.iter().map(|&(x, y)| t.apply(x, y)).collect();
        let flat: Vec<f64> = ring.iter().flat_map(|&(x, y)| [x, y]).collect();
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
    }
}

pub fn ring_bbox(ring: &[(f64, f64)]) -> Option<((f64, f64), (f64, f64))> {
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

/// Faint dashed PURPLE rings of the previous storey's construction-phase
/// zones, each labelled at its centroid — drawn with the outline watermark
/// while a fresh upper storey is traced. Echoes the committed-phase overlay's
/// purple (`render_phase_regions`) at watermark strength so the two read as
/// the same concept, one live and one ghosted.
fn draw_phase_watermark_rings(
    painter: &egui::Painter,
    t: &Transform2d,
    zones: &[PhaseDisplayRegion],
) {
    let purple = egui::Color32::from_rgba_unmultiplied(200, 120, 255, 130);
    let stroke = egui::Stroke::new(2.0, purple);
    let dash_ring = |ring: &[(f64, f64)]| {
        if ring.len() < 3 {
            return;
        }
        let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let n = pts.len();
        for k in 0..n {
            draw_dashed_segment(painter, pts[k], pts[(k + 1) % n], stroke);
        }
    };
    for zone in zones {
        if zone.exterior.len() < 3 {
            continue;
        }
        dash_ring(&zone.exterior);
        for hole in &zone.holes {
            dash_ring(hole);
        }
        let (lx, ly) = zone.label_pos;
        painter.text(
            t.apply(lx, ly),
            egui::Align2::CENTER_CENTER,
            &zone.label,
            egui::FontId::proportional(12.0),
            purple,
        );
    }
}

pub fn bounding_box(storey: &Storey) -> Option<((f64, f64), (f64, f64))> {
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
    // not by recolouring the surface fill. FILL ONLY — the wall lines are drawn
    // from `storey.walls.regions` by the caller (a floor-region edge is NOT a
    // reliable wall: a phase-seam region keeps an edge across a dormer opening).
    let fill = color_for(DisplayMode::Floors, display_mode, style::FLOOR_FILL);

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

/// Whether a wall's base edge `a`→`b` lies on the storey floor `boundary` — a
/// PERIMETER wall (drawn as a plan wall line) vs an interior riser/step wall
/// (drawn separately, dashed, by the exposure overlay). Tests the base-edge
/// MIDPOINT against every boundary ring edge with a generous 0.05 m tolerance,
/// so the classification survives the few-cm drift between separately-dimensioned
/// regions: a riser is well inside the boundary; a perimeter wall is on it.
fn wall_on_perimeter(a: (f64, f64), b: (f64, f64), boundary: &[Vec<Point2f64>]) -> bool {
    const TOL: f64 = 0.05;
    let mid = ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5);
    boundary.iter().any(|ring| {
        let n = ring.len();
        n >= 2
            && (0..n).any(|i| {
                let (p, q) = (ring[i], ring[(i + 1) % n]);
                domain::points::point_to_segment_distance(mid.0, mid.1, p.x, p.y, q.x, q.y) <= TOL
            })
    })
}

fn draw_roof_overlay(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    display_mode: DisplayMode,
) {
    let color = color_for(DisplayMode::Ceilings, display_mode, style::ROOF_LINE);
    let stroke = egui::Stroke::new(1.0, color);
    // The SELECTION is shown by the gold marquee overlay, not a fill here.
    for region in &storey.ceilings.regions {
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
        // Detail tier: the report trims dimension strings to declutter the
        // figure (hover / checkbox restores; print always shows them).
        crate::detail_marker(painter, true);
        painter.text(
            mid,
            egui::Align2::CENTER_CENTER,
            format!("{:.2} m", length),
            egui::FontId::monospace(11.0),
            style::DIMENSION_TEXT,
        );
        crate::detail_marker(painter, false);
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
        // Detail tier — see the dimension labels above.
        crate::detail_marker(painter, true);
        painter.text(
            pos,
            egui::Align2::CENTER_BOTTOM,
            format!("h={:.2}m", v.z),
            egui::FontId::proportional(9.0),
            style::ROOF_LINE,
        );
        crate::detail_marker(painter, false);
    }
}

#[cfg(test)]
mod tests {
    use super::wall_on_perimeter;
    use ber_model::ber::common::ItemId;
    use ber_model::ber::structural::FloorRegions;

    /// The dormer render fix: 2D walls are drawn from the resolved wall regions'
    /// base edges, filtered to PERIMETER walls (base edge on the floor boundary).
    /// A dormer's opened wall resolves to its two FLANK segments — both classify
    /// as perimeter (drawn) — while a would-be FULL-WIDTH segment spanning the
    /// opening does NOT (its midpoint is in the opening, off the boundary), and
    /// an INTERIOR riser (drawn separately by the exposure overlay) does not.
    #[test]
    fn dormer_flanks_classify_as_walls_not_the_full_width() {
        // Box x[0,5] y[0,3] + a centred bump x[2,4] y[3,4] on top (the dormer).
        let p = domain::points::Point2f64::new;
        let regions = FloorRegions::from_vertex_lists(
            vec![
                vec![p(0.0, 0.0), p(5.0, 0.0), p(5.0, 3.0), p(0.0, 3.0)],
                vec![p(2.0, 3.0), p(4.0, 3.0), p(4.0, 4.0), p(2.0, 4.0)],
            ],
            ItemId::default(),
        );
        let boundary = regions.boundary_rings();
        assert!(!boundary.is_empty(), "the union yields a boundary");

        // The two flanks of the opened wall (y=3) are perimeter → drawn.
        assert!(
            wall_on_perimeter((0.0, 3.0), (2.0, 3.0), &boundary),
            "left flank is a perimeter wall"
        );
        assert!(
            wall_on_perimeter((4.0, 3.0), (5.0, 3.0), &boundary),
            "right flank is a perimeter wall"
        );
        // A full-width segment across the opening is NOT a perimeter wall — its
        // midpoint (2.5, 3) sits in the dormer opening, off the boundary. (The
        // resolved walls never contain this, but the classifier must reject it.)
        assert!(
            !wall_on_perimeter((0.0, 3.0), (5.0, 3.0), &boundary),
            "a full-width line across the opening is NOT a wall"
        );
        // The bump's own side wall is perimeter; an interior riser is not.
        assert!(
            wall_on_perimeter((2.0, 3.0), (2.0, 4.0), &boundary),
            "the bump side is a perimeter wall"
        );
        assert!(
            !wall_on_perimeter((1.5, 1.0), (3.5, 1.0), &boundary),
            "an interior (riser) edge is NOT a perimeter wall"
        );
    }
}
