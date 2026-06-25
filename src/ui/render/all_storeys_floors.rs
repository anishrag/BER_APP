//! The OVERLAID all-storeys floor view for the Floor-exposure tool: every
//! storey's floors drawn on ONE shared transform (so footprints line up across
//! the stack), each storey in its own translucent palette shade, with each
//! region's [`FloorExposure`] overlaid as a thick coloured OUTLINE and a
//! brighter halo on the current selection. A small legend keys the storey
//! shades + the exposure colours.
//!
//! Independent of the active storey tab — it always shows the whole stack — so
//! the canvas renders this INSTEAD of the normal single-storey plan while the
//! tool is active (like the partition/elevation early-returns).

use eframe::egui;

use crate::controller::app_controller::FloorExposureStoreyView;
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::structural::FloorExposure;

use super::{fit_transform, storey_2d, style, union_bbox, ViewParams};

/// The outline colour for a region's exposure (Unset → grey).
fn exposure_color(e: FloorExposure) -> egui::Color32 {
    match e {
        FloorExposure::Ground => style::GROUND_WALL,
        FloorExposure::Exposed => style::EXPOSED_WALL,
        FloorExposure::SemiExposed => style::SEMI_EXPOSED_WALL,
        FloorExposure::NonHeatLoss => style::NON_HEAT_LOSS_WALL,
        FloorExposure::Unset => style::UNSET_WALL,
    }
}

/// Render every storey's floors overlaid on one transform fitted to the union
/// of all storey floor bboxes. `view` rolls in the user's pan/zoom.
pub(crate) fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: &Ber,
    view: ViewParams,
    storeys: &[FloorExposureStoreyView],
) {
    if storeys.iter().all(|s| s.regions.is_empty()) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No floors yet — draw and dimension the storeys first.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    }

    // ONE fit box: union of every storey's floor bbox, so footprints share a
    // common world→pixel mapping and stacked floors line up.
    let bb = ber
        .storeys
        .iter()
        .filter_map(storey_2d::bounding_box)
        .fold(None, |acc, b| union_bbox(acc, Some(b)));
    let Some((min_xy, max_xy)) = bb else {
        return;
    };
    let t = fit_transform(rect, min_xy, max_xy, view);

    for sv in storeys {
        let fill = style::storey_fill(sv.idx);
        for region in &sv.regions {
            if region.ring.len() < 3 {
                continue;
            }
            let screen_pts: Vec<egui::Pos2> =
                region.ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();

            // Translucent storey-shade fill (earcut so concave footprints fill
            // without fan artefacts — the floor-fill pattern).
            let flat: Vec<f64> = region.ring.iter().flat_map(|(x, y)| [*x, *y]).collect();
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

            // Selection halo UNDER the exposure outline so both read.
            if region.selected {
                painter.add(egui::Shape::closed_line(
                    screen_pts.clone(),
                    egui::Stroke::new(7.0, egui::Color32::WHITE),
                ));
            }

            // Exposure outline on top.
            painter.add(egui::Shape::closed_line(
                screen_pts,
                egui::Stroke::new(3.0, exposure_color(region.exposure)),
            ));
        }
    }

    draw_legend(painter, rect, storeys.len());
}

/// Storey-shade swatches + the exposure colour key, stacked top-left.
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
    for (label, color) in [
        ("Ground", style::GROUND_WALL),
        ("Exposed", style::EXPOSED_WALL),
        ("Semi-exposed", style::SEMI_EXPOSED_WALL),
        ("Non-heat-loss", style::NON_HEAT_LOSS_WALL),
        ("Unset", style::UNSET_WALL),
    ] {
        let p = egui::pos2(rect.left() + 12.0, y);
        painter.line_segment(
            [p + egui::vec2(0.0, 6.0), p + egui::vec2(18.0, 6.0)],
            egui::Stroke::new(4.0, color),
        );
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
