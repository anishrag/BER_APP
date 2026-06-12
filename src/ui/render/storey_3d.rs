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
//!   walls genuinely follow raised/ridged roofs — the old faint volume-hint
//!   band is gone. Riser walls between stepped sections are deliberately not
//!   derived yet — deferred work.
//!
//! The default ceiling height comes from `storey.ceiling_height_m` — the
//! controller's first-3D gate guarantees it's set before this view is
//! reachable; an unset storey (defensive: a storey-tab switch while already in
//! 3D can land on one) paints a hint instead of guessing a height.
//!
//! `render_at_z_offset` is the workhorse — `full_house_3d` calls it per storey
//! with a cumulative z offset. The public `render` is just `render_at_z_offset`
//! with `z_offset = 0` and computes its own bounding box.

use datatypes::ber::structural::{CeilingRegion, FloorRegion, Storey, WallExposure, WallRegion};
use domain::sketch::DisplayMode;
use eframe::egui;

use super::{color_for, fit_transform, style, Transform2d, ViewParams};

pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    storey: &Storey,
    display_mode: DisplayMode,
    view: ViewParams,
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
    render_at_z_offset(painter, &t, storey, display_mode, 0.0, view);
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
pub(crate) fn render_at_z_offset(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    display_mode: DisplayMode,
    z_offset: f64,
    view: ViewParams,
) {
    let Some(height) = storey.ceiling_height_m else {
        return;
    };
    let wall_top = z_offset + height;
    for region in &storey.floors.regions {
        draw_floor_region(
            painter,
            t,
            region,
            z_offset + region.level_m,
            display_mode,
            view,
        );
    }
    // Real wall regions: the exact resolved polygons (tops trace the roof —
    // jumps at ceiling steps, slopes under fans/ridges), coloured by exposure.
    for wall in &storey.walls.regions {
        draw_wall_region(painter, t, wall, z_offset, display_mode, view);
    }
    // Roofs at their REAL per-vertex z (ceiling steps). Fallback when the
    // resolved roofs are missing (defensive: height set but the resolve
    // projection not yet run): flat rings over each floor region at the
    // default height, matching the old uniform extrusion.
    if storey.roofs.regions.is_empty() {
        let roof_color = color_for(DisplayMode::Roofs, display_mode, style::ROOF_LINE);
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
                    egui::Stroke::new(1.5, roof_color),
                ));
            }
        }
    } else {
        for roof in &storey.roofs.regions {
            draw_roof_region(painter, t, roof, z_offset, display_mode, view);
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
    for region in &storey.roofs.regions {
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
fn draw_floor_region(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &FloorRegion,
    floor_z: f64,
    display_mode: DisplayMode,
    view: ViewParams,
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
    painter.add(egui::Shape::closed_line(
        floor_pts,
        egui::Stroke::new(2.0, floor_color),
    ));
}

/// One WALL region's real polygon (base edge at the bottom, top tracing the
/// roof — jumps at ceiling steps, slopes under fans/ridges), coloured by its
/// exposure class with the shared 2D scheme so a wall reads the same in both
/// views. Unset walls draw grey — present, just not classified yet.
fn draw_wall_region(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &WallRegion,
    z_offset: f64,
    display_mode: DisplayMode,
    view: ViewParams,
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
    };
    let color = color_for(DisplayMode::Walls, display_mode, base);
    painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(2.0, color)));
}

/// One roof region's outline at its own per-vertex z (`z_offset` + the stored
/// vertex z, which is metres above the storey's floor datum).
fn draw_roof_region(
    painter: &egui::Painter,
    t: &Transform2d,
    region: &CeilingRegion,
    z_offset: f64,
    display_mode: DisplayMode,
    view: ViewParams,
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
    let roof_color = color_for(DisplayMode::Roofs, display_mode, style::ROOF_LINE);
    painter.add(egui::Shape::closed_line(
        pts,
        egui::Stroke::new(1.5, roof_color),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use datatypes::ber::common::ItemId;
    use datatypes::ber::structural::{FloorRegions, Opes, Polygon3D, WallRegions};
    use domain::points::{Point2f64, Point3f64};

    /// The fitted view must include WALL vertices: a wall whose top rises
    /// above the default ceiling height (under a ridge) extends the projected
    /// bounding box beyond the floors+roofs extent.
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
