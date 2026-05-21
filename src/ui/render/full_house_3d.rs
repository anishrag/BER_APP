//! Axonometric 3D rendering of every Storey in the BER, stacked vertically.
//!
//! Storeys are placed at cumulative z offsets equal to the sum of heights of
//! storeys below. The transform is fitted across all storeys' projected
//! bounding boxes so the whole building sits inside the canvas.

use datatypes::ber::ber_struct::Ber;
use domain::sketch::DisplayMode;
use eframe::egui;

use super::{fit_transform, storey_3d, style};

pub fn render(painter: &egui::Painter, rect: egui::Rect, ber: &Ber, display_mode: DisplayMode) {
    if ber.storeys.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No storeys yet.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    }

    // First pass: precompute z-offsets per storey and the combined projected bbox.
    let mut z_offsets = Vec::with_capacity(ber.storeys.len());
    let mut z = 0.0_f64;
    let mut combined: Option<((f64, f64), (f64, f64))> = None;

    for storey in &ber.storeys {
        z_offsets.push(z);
        if let Some(bb) = storey_3d::projected_bb_for_storey(storey, z) {
            combined = Some(match combined {
                None => bb,
                Some((cur_min, cur_max)) => (
                    (cur_min.0.min(bb.0 .0), cur_min.1.min(bb.0 .1)),
                    (cur_max.0.max(bb.1 .0), cur_max.1.max(bb.1 .1)),
                ),
            });
        }
        let storey_height = storey
            .floors
            .regions
            .first()
            .map(|r| r.height)
            .unwrap_or(2.4);
        z += storey_height;
    }

    let Some((min_xy, max_xy)) = combined else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Storeys present but no geometry yet.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    };

    let t = fit_transform(rect, min_xy, max_xy);

    // Second pass: render each storey using the shared transform.
    // Iterate top-down so lower storeys' floors don't paint over upper-storey walls
    // (closer-to-camera in isometric is drawn last).
    for (i, storey) in ber.storeys.iter().enumerate().rev() {
        storey_3d::render_at_z_offset(painter, &t, storey, display_mode, z_offsets[i]);
    }
}
