//! Top-down 2D plan of every storey in the BER, drawn side-by-side as a row of
//! equal-width sub-canvases. Each storey auto-fits inside its own sub-rect via
//! `storey_2d::render`, with a small label above so the user can tell which is
//! which. This is the "All storeys" overview — read-only for the drawing
//! tools, but the STRUCTURE phase selects/lassoes across it, so the
//! Structure-phase selection shades here too and the per-cell transforms are
//! exposed ([`cell_transforms`]) for the canvas to invert taps/strokes.
//!
//! A 2×2 grid for >2 storeys is a follow-up; today we just lay them out in a row.

use std::collections::HashSet;

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use domain::sketch::DisplayMode;
use eframe::egui;

use super::{storey_2d, style, Transform2d, ViewParams};

/// Height of the storey-name label strip above each plan cell.
const LABEL_H: f32 = 18.0;

/// The plan sub-rect of cell `i` of `n` — the SINGLE source for the layout,
/// shared by [`render`] and [`cell_transforms`] so interaction inverts exactly
/// the mapping the renderer used.
fn plan_rect(rect: egui::Rect, n: usize, i: usize) -> egui::Rect {
    let sub_w = rect.width() / n.max(1) as f32;
    egui::Rect::from_min_size(
        egui::pos2(rect.left() + sub_w * i as f32, rect.top() + LABEL_H),
        egui::vec2(sub_w, rect.height() - LABEL_H),
    )
}

/// One shared fit box for every cell: the GLOBAL bbox (union of all storeys).
/// Fitting each cell to this common frame — rather than to each storey's own
/// footprint — gives every cell the SAME world→pixel scale AND the SAME world
/// anchor. So relative sizes read true (a half-size storey is half-size) and
/// shared geometry lines up across cells (e.g. each storey's base sits at the
/// same height) instead of every footprint floating centered in its own cell.
fn global_bbox(ber: &Ber) -> Option<((f64, f64), (f64, f64))> {
    ber.storeys
        .iter()
        .filter_map(|s| storey_2d::bounding_box(s))
        .fold(None, |acc, bb| super::union_bbox(acc, Some(bb)))
}

/// The per-cell screen↔world transform for every storey (index = storey
/// index), matching [`render`]'s layout exactly — the canvas inverts these to
/// map an all-storeys tap/lasso stroke into each storey's own world frame,
/// and applies them to draw per-storey overlays into the right cell. Empty
/// when there are no storeys / no fittable bbox.
pub(crate) fn cell_transforms(rect: egui::Rect, ber: &Ber, view: ViewParams) -> Vec<Transform2d> {
    let Some((min_xy, max_xy)) = global_bbox(ber) else {
        return Vec::new();
    };
    (0..ber.storeys.len())
        .map(|i| super::fit_transform(plan_rect(rect, ber.storeys.len(), i), min_xy, max_xy, view))
        .collect()
}

/// Which storey's CELL a screen point falls in, if any — the inverse of the
/// row layout, for the all-storeys canvas to route a tap to the right storey
/// before inverting that cell's transform. `None` off every plan cell (e.g.
/// on the label strip) or with no storeys.
pub(crate) fn cell_index_at(rect: egui::Rect, ber: &Ber, pos: egui::Pos2) -> Option<usize> {
    (0..ber.storeys.len()).find(|i| plan_rect(rect, ber.storeys.len(), *i).contains(pos))
}

pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: &Ber,
    display_mode: DisplayMode,
    view: ViewParams,
    // Structure-phase selected region ids — region ids are unique house-wide,
    // so one shared set shades correctly in every cell (each cell only finds
    // its own storey's regions in it).
    selected: &HashSet<ItemId>,
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

    let n = ber.storeys.len();
    let bbox = global_bbox(ber);

    for (i, storey) in ber.storeys.iter().enumerate() {
        let cell = plan_rect(rect, n, i);
        let label_rect = egui::Rect::from_min_size(
            egui::pos2(cell.left(), rect.top()),
            egui::vec2(cell.width(), LABEL_H),
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
            cell,
            storey,
            display_mode,
            None,
            &[],
            view,
            None,
            // No phase-zone watermark either — that's a fresh-storey trace aid.
            &[],
            bbox,
            selected,
            // Full plan (not the reduced Objects view) in the overview.
            false,
            // Overview never calls `render_wall_exposure`, so draw interior
            // riser/step walls here (dashed) — otherwise they'd vanish.
            true,
        );
    }
}
