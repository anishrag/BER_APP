//! Top-down 2D plan of every storey in the BER, drawn side-by-side as a row of
//! equal-width sub-canvases. Each storey auto-fits inside its own sub-rect via
//! `storey_2d::render`, with a small label above so the user can tell which is
//! which. This is the read-only "All storeys" overview — overlays (dimension
//! prompts, exposure, living areas, edit selection) are suppressed at the
//! caller because they're per-edited-storey.
//!
//! A 2×2 grid for >2 storeys is a follow-up; today we just lay them out in a row.

use datatypes::ber::ber_struct::Ber;
use domain::sketch::DisplayMode;
use eframe::egui;

use super::{storey_2d, style, ViewParams};

pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: &Ber,
    display_mode: DisplayMode,
    view: ViewParams,
) {
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

    let n = ber.storeys.len() as f32;
    let label_h = 18.0_f32;
    let sub_w = rect.width() / n;
    let plan_h = rect.height() - label_h;

    // One shared fit box for every cell: the GLOBAL bbox (union of all storeys).
    // Fitting each cell to this common frame — rather than to each storey's own
    // footprint — gives every cell the SAME world→pixel scale AND the SAME world
    // anchor. So relative sizes read true (a half-size storey is half-size) and
    // shared geometry lines up across cells (e.g. each storey's base sits at the
    // same height) instead of every footprint floating centered in its own cell.
    let global_bbox = ber
        .storeys
        .iter()
        .filter_map(|s| storey_2d::bounding_box(s))
        .fold(None, |acc, bb| super::union_bbox(acc, Some(bb)));

    for (i, storey) in ber.storeys.iter().enumerate() {
        let left = rect.left() + sub_w * i as f32;
        let label_rect =
            egui::Rect::from_min_size(egui::pos2(left, rect.top()), egui::vec2(sub_w, label_h));
        let plan_rect = egui::Rect::from_min_size(
            egui::pos2(left, rect.top() + label_h),
            egui::vec2(sub_w, plan_h),
        );
        let label = if storey.name.is_empty() {
            format!("Storey {}", storey.storey_number + 1)
        } else {
            storey.name.clone()
        };
        painter.text(
            label_rect.center(),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(12.0),
            style::DIMENSION_TEXT,
        );
        // No collinearity highlight + no dimension labels in the overview —
        // those are edit-time signals tied to the active storey's controller
        // state, which isn't meaningful here. The global bbox override gives the
        // shared scale + alignment.
        storey_2d::render(
            painter,
            plan_rect,
            storey,
            display_mode,
            None,
            &[],
            view,
            None,
            global_bbox,
        );
    }
}
