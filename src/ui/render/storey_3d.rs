//! Axonometric 3D rendering of a single Storey.
//!
//! Uses the orbitable orthographic projection (see `super::project_3d`,
//! parameterized by the view's yaw/pitch — drag-to-orbit; the defaults give
//! the classic isometric). The storey's REAL resolved geometry is drawn, not
//! a uniform extrusion:
//!
//! - each floor region's outline at its own `level_m` (floor steps show as
//!   raised/lowered rings);
//! - each roof region at its own per-vertex z (ceiling steps as offset rings,
//!   fans/ridges as sloped faces);
//! - each WALL REGION's real polygon — sloped/jumped tops tracing the roof —
//!   coloured by its exposure class (the 2D scheme). EVERY perimeter wall
//!   resolves to a region now (unmarked walls come through as `Unset`, drawn
//!   grey), so the walls themselves carry the building's volume and Unset
//!   walls genuinely follow raised/ridged ceilings — the old faint volume-hint
//!   band is gone. RISER faces between stepped sections are derived too
//!   (defaulting `Unset` like other walls; `Ground` is the riser-only
//!   assessor choice, earth-toned) and draw through the same wall-region
//!   path.
//!
//! The default ceiling height comes from `storey.ceiling_height_m` — the
//! controller's first-3D gate guarantees it's set before this view is
//! reachable; an unset storey (defensive: a storey-tab switch while already in
//! 3D can land on one) paints a hint instead of guessing a height.
//!
//! `render_at_z_offset` is the workhorse — `full_house_3d` calls it per storey
//! with a cumulative z offset. The public `render` is just `render_at_z_offset`
//! with `z_offset = 0` and computes its own bounding box.

use std::collections::HashSet;

use ber_model::ber::common::ItemId;
use ber_model::ber::structural::{
    CeilingExposure, CeilingRegion, FloorExposure, FloorRegion, Storey, WallExposure, WallRegion,
};
use domain::sketch::DisplayMode;
use eframe::egui;

use super::{color_for, fill_polygon, fit_transform, style, Transform2d, ViewParams};

#[allow(clippy::too_many_arguments)]
pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    storey: &Storey,
    display_mode: DisplayMode,
    view: ViewParams,
    // Region ids the assessor has SELECTED in the Structure phase — these faces
    // are shaded. Empty outside that phase / with nothing selected.
    selected: &HashSet<ItemId>,
    // World-3D points (storey-local) to mark with a bright dot — the Objects
    // phase's room pins. Empty otherwise.
    hotspots: &[(f64, f64, f64)],
) {
    if storey.ceiling_height_m.is_none() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Set a default ceiling height to view in 3D.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    }
    let Some((min_xy, max_xy)) = projected_bb_for_storey(storey, 0.0, view) else {
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy, view);
    // The single-storey view draws every surface — the assessor
    // reviews/paints exposure here, so nothing is faded or hidden (a derived
    // InterStorey wall draws as a ghost, but it draws).
    render_at_z_offset(
        painter,
        &t,
        storey,
        display_mode,
        0.0,
        view,
        selected,
        false,
    );
    for &(x, y, z) in hotspots {
        let (px, py) = view.project_3d(x, y, z);
        let sp = t.apply(px, py);
        painter.circle_filled(sp, 7.0, egui::Color32::from_rgb(255, 210, 70));
        painter.circle_stroke(sp, 7.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
    }
}

/// The lowest floor level in the storey (metres relative to its datum) — the
/// bottom of the storey's vertical extent for view fitting. `0.0` when there
/// are no floor regions.
fn min_floor_level(storey: &Storey) -> f64 {
    storey
        .floors
        .regions
        .iter()
        .map(|r| r.level_m)
        .reduce(f64::min)
        .unwrap_or(0.0)
}

/// Render `storey` with the assumption that its floor DATUM sits at world-z
/// `z_offset` (region `level_m`s offset from there).
/// Caller supplies the transform so multi-storey views can share one camera;
/// `view` carries the orbit angles the points are projected with (must match
/// the angles the transform's fit bbox was computed at).
/// Defensive no-op when the storey's ceiling height is unset (`render` and
/// `full_house_3d` both hint-and-return before getting here).
///
/// `fade_internal` (the all-storeys stack sets it): non-heat-loss walls,
/// floors, and Party ceilings draw as FAINT TRANSLUCENT fills with a faded
/// outline — "external boundary to another dwelling, no heat loss" — while a
/// derived `InterStorey` region (this dwelling's own storey junction, stamped
/// by `derive_inter_storey`) doesn't draw at all: it would double-draw the
/// same plane at every junction and says nothing about heat loss. A SELECTED
/// region always draws highlighted (the Structure review must be able to
/// shade any surface). The single-storey view passes `false` — the assessor
/// reviews/paints exposure there, so every surface draws (InterStorey walls
/// as ghosts).
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_at_z_offset(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    display_mode: DisplayMode,
    z_offset: f64,
    view: ViewParams,
    selected: &HashSet<ItemId>,
    fade_internal: bool,
) {
    let Some(height) = storey.ceiling_height_m else {
        return;
    };
    let wall_top = z_offset + height;
    for region in &storey.floors.regions {
        if fade_internal
            && region.exposure == FloorExposure::InterStorey
            && !selected.contains(&region.id)
        {
            continue; // own storey-junction plane — hidden in the stack view
        }
        draw_floor_region(
            painter,
            t,
            region,
            z_offset + region.level_m,
            display_mode,
            view,
            selected,
            fade_internal,
        );
    }
    // Real wall regions: the exact resolved polygons (tops trace the roof —
    // jumps at ceiling steps, slopes under fans/ridges), coloured by exposure.
    for wall in &storey.walls.regions {
        if fade_internal
            && wall.exposure == WallExposure::InterStorey
            && !selected.contains(&wall.id)
        {
            continue; // buried behind the neighbour storey — hidden in the stack
        }
        draw_wall_region(
            painter,
            t,
            wall,
            z_offset,
            display_mode,
            view,
            selected,
            fade_internal,
        );
    }
    // Openings (windows blue, doors brown) — each opening's 4 vertices are a
    // real quad in 3D (in the wall plane, or on the roof plane for roof
    // windows), so they fill cleanly even though a wall window's PLAN footprint
    // is degenerate. Drawn after the walls so they sit on the face.
    for window in &storey.windows {
        let c = color_for(DisplayMode::Walls, display_mode, style::WINDOW_FILL);
        draw_opening_3d(
            painter,
            t,
            &window.vertices,
            z_offset,
            view,
            c,
            selected.contains(&window.id),
        );
    }
    for door in &storey.doors {
        let c = color_for(DisplayMode::Walls, display_mode, style::DOOR_FILL);
        draw_opening_3d(
            painter,
            t,
            &door.vertices,
            z_offset,
            view,
            c,
            selected.contains(&door.id),
        );
    }
    // Ceilings at their REAL per-vertex z (ceiling steps). Fallback when the
    // resolved ceilings are missing (defensive: height set but the resolve
    // projection not yet run): flat rings over each floor region at the
    // default height, matching the old uniform extrusion.
    if storey.ceilings.regions.is_empty() {
        let ceiling_color = color_for(DisplayMode::Ceilings, display_mode, style::ROOF_LINE);
        for region in &storey.floors.regions {
            let pts: Vec<egui::Pos2> = region
                .polygon
                .exterior()
                .coords()
                .map(|c| {
                    let (px, py) = view.project_3d(c.x, c.y, wall_top);
                    t.apply(px, py)
                })
                .collect();
            if pts.len() >= 3 {
                painter.add(egui::Shape::closed_line(
                    pts,
                    egui::Stroke::new(1.5, ceiling_color),
                ));
            }
        }
    } else {
        for roof in &storey.ceilings.regions {
            if fade_internal
                && roof.exposure == CeilingExposure::InterStorey
                && !selected.contains(&roof.id)
            {
                continue; // own storey-junction plane — hidden in the stack view
            }
            draw_roof_region(
                painter,
                t,
                roof,
                z_offset,
                display_mode,
                view,
                selected,
                fade_internal,
            );
        }
    }
}

/// Projected (post-projection, at `view`'s orbit angles) 2D bounding box of
/// every point in this storey: each floor region's ring at the storey's
/// lowest level and default ceiling height AND at its own `level_m`, plus
/// every roof vertex at its own z — so lowered floors, raised ceilings, and
/// any orbit angle never clip. `None` when there's no geometry or no ceiling
/// height to extrude to.
pub(crate) fn projected_bb_for_storey(
    storey: &Storey,
    z_offset: f64,
    view: ViewParams,
) -> Option<((f64, f64), (f64, f64))> {
    let height = storey.ceiling_height_m?;
    let wall_bottom = z_offset + min_floor_level(storey);
    let wall_top = z_offset + height;
    let mut bb: Option<((f64, f64), (f64, f64))> = None;
    let mut add = |px: f64, py: f64| {
        bb = Some(match bb {
            None => ((px, py), (px, py)),
            Some(((min_x, min_y), (max_x, max_y))) => (
                (min_x.min(px), min_y.min(py)),
                (max_x.max(px), max_y.max(py)),
            ),
        });
    };
    for region in &storey.floors.regions {
        for c in region.polygon.exterior().coords() {
            for z in [wall_bottom, z_offset + region.level_m, wall_top] {
                let (px, py) = view.project_3d(c.x, c.y, z);
                add(px, py);
            }
        }
    }
    for region in &storey.ceilings.regions {
        for v in &region.polygon.exterior {
            let (px, py) = view.project_3d(v.x, v.y, z_offset + v.z);
            add(px, py);
        }
    }
    // Wall regions: their tops can exceed the default height (under a ridge)
    // or drop below the band (lowered floors) — fold every vertex in.
    for region in &storey.walls.regions {
        for v in &region.polygon.exterior {
            let (px, py) = view.project_3d(v.x, v.y, z_offset + v.z);
            add(px, py);
        }
    }
    bb
}

/// One floor region's outline at its own level (`floor_z`), so floor steps
/// read as offset rings. The building's vertical extent is carried by the
/// wall regions themselves (every wall resolves, Unset included) — no volume
/// band is drawn here.
#[allow(clippy::too_many_arguments)]
fn draw_floor_region(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &FloorRegion,
    floor_z: f64,
    display_mode: DisplayMode,
    view: ViewParams,
    selected: &HashSet<ItemId>,
    fade_internal: bool,
) {
    let floor_pts: Vec<egui::Pos2> = region
        .polygon
        .exterior()
        .coords()
        .map(|c| {
            let (px, py) = view.project_3d(c.x, c.y, floor_z);
            t.apply(px, py)
        })
        .collect();
    if floor_pts.len() < 4 {
        return;
    }
    let floor_color = color_for(DisplayMode::Floors, display_mode, style::FLOOR_LINE);
    // A SELECTED floor region's face is SHADED so the surfaces we mean are
    // unmistakable in 3D; in the all-storeys stack (`fade_internal`) a
    // NON-HEAT-LOSS floor (a dwelling below — an external fact; own storey
    // junctions are InterStorey and skipped before this fn) gets a FAINT
    // translucent shade + faded outline so it reads as "not part of the
    // envelope". The rest stay wireframe.
    let internal = fade_internal
        && region.exposure == FloorExposure::NonHeatLoss
        && !selected.contains(&region.id);
    if selected.contains(&region.id) {
        fill_polygon(
            painter,
            &floor_pts,
            style::highlight_fill(style::FLOOR_LINE),
        );
    } else if internal {
        fill_polygon(painter, &floor_pts, style::internal_fill(style::FLOOR_LINE));
    }
    let stroke = if internal {
        egui::Stroke::new(1.0, style::faded(floor_color))
    } else {
        egui::Stroke::new(2.0, floor_color)
    };
    painter.add(egui::Shape::closed_line(floor_pts, stroke));
}

/// One WALL region's real polygon (base edge at the bottom, top tracing the
/// roof — jumps at ceiling steps, slopes under fans/ridges), coloured by its
/// exposure class with the shared 2D scheme so a wall reads the same in both
/// views. Unset walls draw grey — present, just not classified yet.
#[allow(clippy::too_many_arguments)]
fn draw_wall_region(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &WallRegion,
    z_offset: f64,
    display_mode: DisplayMode,
    view: ViewParams,
    selected: &HashSet<ItemId>,
    fade_internal: bool,
) {
    if region.polygon.exterior.len() < 3 {
        return;
    }
    let pts: Vec<egui::Pos2> = region
        .polygon
        .exterior
        .iter()
        .map(|v| {
            let (px, py) = view.project_3d(v.x, v.y, z_offset + v.z);
            t.apply(px, py)
        })
        .collect();
    let base = match region.exposure {
        WallExposure::Exposed => style::EXPOSED_WALL,
        WallExposure::SemiExposed => style::SEMI_EXPOSED_WALL,
        WallExposure::NonHeatLoss => style::NON_HEAT_LOSS_WALL,
        WallExposure::Unset => style::UNSET_WALL,
        WallExposure::Ground => style::GROUND_WALL,
        // Derived storey junction — grey base; the ghost branch below fades
        // it further. Only reachable in the SINGLE-storey view (the stack view
        // skips InterStorey regions before this fn).
        WallExposure::InterStorey => style::UNSET_WALL,
    };
    let color = color_for(DisplayMode::Walls, display_mode, base);
    // A SELECTED wall region's face is SHADED (keeping its exposure colour) so
    // it reads as a solid surface, not just an edge; in the all-storeys stack
    // (`fade_internal`) a NON-HEAT-LOSS wall (an external boundary to another
    // dwelling) gets a FAINT translucent shade + faded outline so it reads as
    // "not part of the envelope". A derived InterStorey band draws as the same
    // ghost in EVERY view it reaches (the single-storey view — the stack skips
    // it): present-but-inert, never a paint target.
    let internal = ((fade_internal && region.exposure == WallExposure::NonHeatLoss)
        || region.exposure == WallExposure::InterStorey)
        && !selected.contains(&region.id);
    if selected.contains(&region.id) {
        fill_polygon(painter, &pts, style::highlight_fill(base));
    } else if internal {
        fill_polygon(painter, &pts, style::internal_fill(base));
    }
    let stroke = if internal {
        egui::Stroke::new(1.0, style::faded(color))
    } else {
        egui::Stroke::new(2.0, color)
    };
    painter.add(egui::Shape::closed_line(pts, stroke));
}

/// One roof region's outline at its own per-vertex z (`z_offset` + the stored
/// vertex z, which is metres above the storey's floor datum).
#[allow(clippy::too_many_arguments)]
fn draw_roof_region(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &CeilingRegion,
    z_offset: f64,
    display_mode: DisplayMode,
    view: ViewParams,
    selected: &HashSet<ItemId>,
    fade_internal: bool,
) {
    if region.polygon.exterior.len() < 3 {
        return;
    }
    let pts: Vec<egui::Pos2> = region
        .polygon
        .exterior
        .iter()
        .map(|v| {
            let (px, py) = view.project_3d(v.x, v.y, z_offset + v.z);
            t.apply(px, py)
        })
        .collect();
    let ceiling_color = color_for(DisplayMode::Ceilings, display_mode, style::ROOF_LINE);
    // A SELECTED roof region's face is SHADED so sloped fans/ridges read as
    // surfaces, not just outlines; in the all-storeys stack (`fade_internal`)
    // a PARTY ceiling (a dwelling above — no heat loss; own storey junctions
    // are InterStorey and skipped before this fn) gets a FAINT translucent
    // shade + faded outline so it reads as "not part of the envelope".
    let internal = fade_internal
        && region.exposure == CeilingExposure::Party
        && !selected.contains(&region.id);
    if selected.contains(&region.id) {
        fill_polygon(painter, &pts, style::highlight_fill(style::ROOF_LINE));
    } else if internal {
        fill_polygon(painter, &pts, style::internal_fill(style::ROOF_LINE));
    }
    let stroke = if internal {
        egui::Stroke::new(1.0, style::faded(ceiling_color))
    } else {
        egui::Stroke::new(1.5, ceiling_color)
    };
    painter.add(egui::Shape::closed_line(pts, stroke));
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::common::ItemId;
    use ber_model::ber::structural::{FloorRegions, Opes, Polygon3D, WallRegions};
    use domain::points::{Point2f64, Point3f64};

    /// The fitted view must include WALL vertices: a wall whose top rises
    /// above the default ceiling height (under a ridge) extends the projected
    /// bounding box beyond the floors+ceilings extent.
    #[test]
    fn projected_bb_includes_wall_tops_above_the_default_height() {
        let square = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let mut storey = Storey {
            floors: FloorRegions::from_vertex_lists(vec![square], ItemId::default()),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        let view = ViewParams::default();
        let without_walls = projected_bb_for_storey(&storey, 0.0, view).unwrap();

        // A wall peaking at 4.2 m — well above the 2.4 m band top.
        storey.walls = WallRegions {
            regions: vec![WallRegion {
                id: ItemId::default(),
                polygon: Polygon3D {
                    exterior: vec![
                        Point3f64::new(0.0, 0.0, 0.0),
                        Point3f64::new(4.0, 0.0, 0.0),
                        Point3f64::new(2.0, 0.0, 4.2),
                    ],
                },
                exposure: WallExposure::Exposed,
                opes: Opes::default(),
                construction_id: ItemId::default(),
            }],
        };
        let with_walls = projected_bb_for_storey(&storey, 0.0, view).unwrap();
        // Higher z projects to smaller screen-y at the default pitch — the
        // bb's min-y must extend; the rest must not shrink.
        assert!(
            with_walls.0 .1 < without_walls.0 .1,
            "wall peak extends the bb: {:?} vs {:?}",
            with_walls,
            without_walls
        );
        assert!(with_walls.1 .1 >= without_walls.1 .1 - 1e-9);
    }
}

/// Draw one opening as a filled quad in 3D: project its 4 vertices at the
/// storey's `z_offset` and fill. Works for wall openings (a vertical rectangle
/// on the wall face) and roof windows (a tilted quad on the roof plane) alike.
fn draw_opening_3d(
    painter: &egui::Painter,
    t: &Transform2d,
    vertices: &[(f32, f32, f32)],
    z_offset: f64,
    view: ViewParams,
    color: egui::Color32,
    // When true (the surface is in the `selected` set — e.g. the Structure-review
    // group being inspected), the opening gets a bold white outline so it reads
    // as highlighted against the rest.
    highlight: bool,
) {
    if vertices.len() < 4 {
        return;
    }
    let pts: Vec<egui::Pos2> = vertices
        .iter()
        .map(|(x, y, z)| {
            let (px, py) = view.project_3d(*x as f64, *y as f64, z_offset + *z as f64);
            t.apply(px, py)
        })
        .collect();
    let stroke = if highlight {
        egui::Stroke::new(3.0, egui::Color32::WHITE)
    } else {
        egui::Stroke::new(1.0, color)
    };
    painter.add(egui::Shape::convex_polygon(pts, color, stroke));
}
