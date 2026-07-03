//! The OVERLAID all-storeys CEILING view for the Ceiling-exposure tool: every
//! storey's ceilings drawn on ONE shared transform (so footprints line up across
//! the stack), each storey in its own translucent palette shade, with each
//! region's [`CeilingExposure`] overlaid as a thick coloured OUTLINE and a
//! brighter halo on the current selection. A small legend keys the storey
//! shades + the exposure colours. The ceiling mirror of
//! [`all_storeys_floors`](super::all_storeys_floors).
//!
//! Independent of the active storey tab — it always shows the whole stack — so
//! the canvas renders this INSTEAD of the normal single-storey plan while the
//! tool is active.

use eframe::egui;

use crate::controller::app_controller::CeilingExposureStoreyView;
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::structural::CeilingExposure;

use super::{fit_transform, storey_2d, style, union_bbox, ViewParams};

/// The outline colour for a region's ceiling exposure (Unset → grey). Reuses the
/// shared wall-exposure palette: ExposedRoof → exposed, Party → non-heat-loss,
/// Unheated → semi-exposed.
fn exposure_color(e: CeilingExposure) -> egui::Color32 {
    match e {
        CeilingExposure::ExposedRoof => style::EXPOSED_WALL,
        CeilingExposure::Party => style::NON_HEAT_LOSS_WALL,
        CeilingExposure::Unheated => style::SEMI_EXPOSED_WALL,
        CeilingExposure::Unset => style::UNSET_WALL,
        // UNREACHABLE in practice: the controller's `ceiling_exposure_view`
        // filters derived InterStorey ceilings out of this worklist. Kept
        // exhaustive — grey, the ghost/unclassified read.
        CeilingExposure::InterStorey => style::UNSET_WALL,
    }
}

/// Render every storey's ceilings overlaid on one transform fitted to the union
/// of all storey footprint bboxes. `view` rolls in the user's pan/zoom.
pub(crate) fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: &Ber,
    view: ViewParams,
    storeys: &[CeilingExposureStoreyView],
) {
    if storeys.iter().all(|s| s.regions.is_empty()) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No ceilings yet — set a ceiling height on the storeys first.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    }

    // ONE fit box: union of every storey's bbox, so footprints share a common
    // world→pixel mapping and stacked ceilings line up.
    let bb = ber
        .storeys
        .iter()
        .filter_map(storey_2d::bounding_box)
        .fold(None, |acc, b| union_bbox(acc, Some(b)));
    let Some((min_xy, max_xy)) = bb else {
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy, view);

    // A filled mesh of `ring` (screen coords) in `color`, earcut-triangulated so
    // concave footprints fill cleanly. `None` if triangulation fails.
    let fill_mesh = |screen_pts: &[egui::Pos2], flat: &[f64], color: egui::Color32| {
        earcutr::earcut(flat, &[], 2).ok().map(|triangles| {
            let mut mesh = egui::Mesh::default();
            for &p in screen_pts {
                mesh.vertices.push(egui::epaint::Vertex {
                    pos: p,
                    uv: egui::epaint::WHITE_UV,
                    color,
                });
            }
            for idx in triangles {
                mesh.indices.push(idx as u32);
            }
            egui::Shape::mesh(mesh)
        })
    };

    for sv in storeys {
        let shade = style::storey_fill(sv.idx);
        for region in &sv.regions {
            if region.ring.len() < 3 {
                continue;
            }
            let screen_pts: Vec<egui::Pos2> =
                region.ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
            let flat: Vec<f64> = region.ring.iter().flat_map(|(x, y)| [*x, *y]).collect();
            let ec = exposure_color(region.exposure);
            // SET = the assessor (or auto) has classified it; UNSET = still to do.
            let is_set = region.exposure != CeilingExposure::Unset;

            // Base storey-shade fill (keeps storey identity across the stack).
            if let Some(m) = fill_mesh(&screen_pts, &flat, shade) {
                painter.add(m);
            }
            // SET ceilings get an exposure-colour tint on top, so "done" reads at
            // a glance and you can tell what it was set to.
            if is_set {
                let tint = egui::Color32::from_rgba_unmultiplied(ec.r(), ec.g(), ec.b(), 70);
                if let Some(m) = fill_mesh(&screen_pts, &flat, tint) {
                    painter.add(m);
                }
            }

            // Selection halo UNDER the outline so both read.
            if region.selected {
                painter.add(egui::Shape::closed_line(
                    screen_pts.clone(),
                    egui::Stroke::new(7.0, egui::Color32::WHITE),
                ));
            }

            // Outline: SET → solid exposure colour (thick, "done"); UNSET →
            // dashed grey ("still to be set").
            if is_set {
                painter.add(egui::Shape::closed_line(
                    screen_pts,
                    egui::Stroke::new(3.5, ec),
                ));
            } else {
                let mut ring = screen_pts;
                ring.push(ring[0]); // close it for the dashed run
                painter.extend(egui::Shape::dashed_line(
                    &ring,
                    egui::Stroke::new(2.0, style::UNSET_WALL),
                    8.0,
                    6.0,
                ));
            }
        }
    }

    draw_legend(painter, rect, storeys.len());
}

/// Storey-shade swatches + the ceiling-exposure colour key, stacked top-left.
fn draw_legend(painter: &egui::Painter, rect: egui::Rect, n_storeys: usize) {
    let mut y = rect.top() + 10.0;
    let swatch = |painter: &egui::Painter, p: egui::Pos2, color: egui::Color32, label: &str| {
        painter.rect_filled(
            egui::Rect::from_min_size(p, egui::vec2(18.0, 12.0)),
            2.0,
            color,
        );
        painter.text(
            egui::pos2(p.x + 24.0, p.y + 6.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            egui::Color32::from_gray(210),
        );
    };
    for i in 0..n_storeys {
        swatch(
            painter,
            egui::pos2(rect.left() + 12.0, y),
            style::storey_fill(i),
            &format!("Storey {}", i + 1),
        );
        y += 18.0;
    }
    y += 6.0;
    // Set exposures draw as a SOLID outline; still-to-set (Unset) draws dashed —
    // mirror that in the key so the two read the same way as on the canvas.
    for (label, color, dashed) in [
        ("Exposed roof", style::EXPOSED_WALL, false),
        ("Party", style::NON_HEAT_LOSS_WALL, false),
        ("Unheated", style::SEMI_EXPOSED_WALL, false),
        ("To set", style::UNSET_WALL, true),
    ] {
        let p = egui::pos2(rect.left() + 12.0, y);
        let a = p + egui::vec2(0.0, 6.0);
        let b = p + egui::vec2(18.0, 6.0);
        if dashed {
            painter.extend(egui::Shape::dashed_line(
                &[a, b],
                egui::Stroke::new(2.5, color),
                4.0,
                3.0,
            ));
        } else {
            painter.line_segment([a, b], egui::Stroke::new(4.0, color));
        }
        painter.text(
            egui::pos2(p.x + 24.0, p.y + 6.0),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(12.0),
            color,
        );
        y += 18.0;
    }
}
