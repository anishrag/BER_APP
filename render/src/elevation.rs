//! Side-elevation drawing: the wall panels + openings of one external side
//! (`Ber::side_elevation`), as shown in the Stage-1 Mapping REVIEW takeover.
//! Extracted from the tablet's canvas so the report renders the exact same
//! picture. The interactive shell (photo panel, nav buttons, opening drag)
//! stays in `src/ui`; this module is pure painting over a `SideElevation`.

use ber_model::ber::openings::OpeningRef;
use ber_model::ber::structural::SideElevation;

use crate::{fit_transform, Transform2d, ViewParams};

/// Draw the elevation into `draw_rect`: every wall panel's base edge + top
/// polyline + end verticals (clipped around openings so no wall line cuts
/// across a window), then every opening (windows blue, doors brown; the
/// `selection`, if any, highlighted white). The world frame is x = composite
/// x, y = −z, so height grows UP on screen. Returns the fit transform so
/// interactive callers can invert pointer positions (and the report can
/// place labels) in the same frame the panels were drawn with.
///
/// The caller is responsible for the `elev.panels.is_empty()` case — this
/// draws nothing (but still returns a transform) when there are no panels.
pub fn render_side_elevation(
    painter: &egui::Painter,
    draw_rect: egui::Rect,
    elev: &SideElevation,
    selection: Option<OpeningRef>,
    vp: ViewParams,
) -> Transform2d {
    // Fit the composite (x, z) bounds into the elevation sub-rect; world frame
    // is x = composite x, y = −z (height grows UP on screen).
    let mx = (elev.max_x - elev.min_x).max(0.5) * 0.06;
    let mz = (elev.max_z - elev.min_z).max(0.5) * 0.10;
    let t = fit_transform(
        draw_rect,
        (elev.min_x - mx, -(elev.max_z + mz)),
        (elev.max_x + mx, -(elev.min_z - mz)),
        vp,
    );
    let p = |x: f64, z: f64| t.apply(x, -z);
    let wall_col = egui::Color32::from_gray(150);
    // Opening rectangles in the composite (x, z) frame — wall-outline lines are
    // clipped around these so a wall edge (e.g. the floor-slab / inter-storey
    // boundary at a fixed base+thickness offset) never draws across a window.
    let open_rects: Vec<(f64, f64, f64, f64)> = elev
        .openings
        .iter()
        .map(|o| {
            (
                o.x_center - o.width_m / 2.0,
                o.x_center + o.width_m / 2.0,
                o.sill_z,
                o.sill_z + o.height_m,
            )
        })
        .collect();
    let wline = |a: (f64, f64), b: (f64, f64), w: f32, c: egui::Color32| {
        draw_clipped_wall_line(painter, &p, a, b, &open_rects, w, c);
    };
    for panel in &elev.panels {
        // Base edge + the top polyline; close the two ends down to the base.
        wline(
            (panel.x0, panel.base_z),
            (panel.x1, panel.base_z),
            2.0,
            wall_col,
        );
        for w in panel.top.windows(2) {
            wline((w[0].0, w[0].1), (w[1].0, w[1].1), 2.0, wall_col);
        }
        if let (Some(first), Some(last)) = (panel.top.first(), panel.top.last()) {
            wline((first.0, first.1), (first.0, panel.base_z), 2.0, wall_col);
            wline((last.0, last.1), (last.0, panel.base_z), 2.0, wall_col);
        }
    }
    for o in &elev.openings {
        let l = o.x_center - o.width_m / 2.0;
        let r = o.x_center + o.width_m / 2.0;
        let (b, tp) = (o.sill_z, o.sill_z + o.height_m);
        let selected = selection == Some(o.opening);
        let base = if o.is_door {
            egui::Color32::from_rgb(180, 130, 80)
        } else {
            egui::Color32::from_rgb(90, 160, 220)
        };
        let a = if selected { 130 } else { 70 };
        let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a);
        let (sw, sc) = if selected {
            (2.5, egui::Color32::WHITE)
        } else {
            (1.5, base)
        };
        draw_elevation_opening(painter, &p, l, r, b, tp, o.shape, fill, sc, sw);
    }
    t
}

/// Draw a wall-outline segment (in the elevation `(x, z)` frame) CLIPPED
/// around the given opening rects: the sub-spans strictly inside any opening
/// are skipped, the rest drawn. `openings` are `(l, r, bottom,
/// top)` rects in the same `(x, z)` frame; `p` maps `(x, z)` → screen.
///
/// This clips exactly the artifact the assessor hit: on a wall whose lower part
/// is an inter-storey band, the resolve leaves a horizontal wall edge at the
/// fixed floor-slab offset (base + slab thickness) that runs straight across the
/// heat-loss section's window — a "floor thickness line" cutting the window in
/// two. A segment that only TOUCHES an opening edge (e.g. the base line meeting
/// a floor-sill window) is kept — only strictly-interior crossings are removed —
/// so every legitimate wall/floor outline elsewhere is untouched. A segment
/// crossing no opening (the common case) draws whole.
pub fn draw_clipped_wall_line(
    painter: &egui::Painter,
    p: &impl Fn(f64, f64) -> egui::Pos2,
    a: (f64, f64),
    b: (f64, f64),
    openings: &[(f64, f64, f64, f64)],
    w: f32,
    c: egui::Color32,
) {
    let (dx, dz) = (b.0 - a.0, b.1 - a.1);
    let stroke = egui::Stroke::new(w, c);
    for (ta, tb) in wall_line_visible_spans(a, b, openings) {
        painter.line_segment(
            [
                p(a.0 + dx * ta, a.1 + dz * ta),
                p(a.0 + dx * tb, a.1 + dz * tb),
            ],
            stroke,
        );
    }
}

/// The visible sub-spans (as `t` sub-intervals of `a`→`b`, `t ∈ [0, 1]`) of a
/// wall-outline segment once the parts STRICTLY inside any opening rect are
/// removed — the pure geometry behind [`draw_clipped_wall_line`]. `openings` are
/// `(l, r, bottom, top)` rects in the same `(x, z)` frame. A segment crossing no
/// opening returns the whole `[(0.0, 1.0)]`; one fully covered returns empty.
pub fn wall_line_visible_spans(
    a: (f64, f64),
    b: (f64, f64),
    openings: &[(f64, f64, f64, f64)],
) -> Vec<(f64, f64)> {
    let (dx, dz) = (b.0 - a.0, b.1 - a.1);
    // The t-interval (along a→b) STRICTLY inside each opening rect, via slab
    // clipping on x and z. A component parallel to a slab (d≈0) is "inside" only
    // when its fixed coordinate is strictly within that slab's open interval, so
    // a line lying exactly on a sill/head is never treated as crossing.
    let mut cuts: Vec<(f64, f64)> = Vec::new();
    for &(l, r, bo, tp) in openings {
        let mut t0 = 0.0_f64;
        let mut t1 = 1.0_f64;
        let slab = |lo: f64, hi: f64, o: f64, d: f64, t0: &mut f64, t1: &mut f64| -> bool {
            if d.abs() < 1e-12 {
                return o > lo + 1e-9 && o < hi - 1e-9; // parallel: fixed coord in the open slab
            }
            let (mut ta, mut tb) = ((lo - o) / d, (hi - o) / d);
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            *t0 = t0.max(ta);
            *t1 = t1.min(tb);
            true
        };
        if slab(l, r, a.0, dx, &mut t0, &mut t1)
            && slab(bo, tp, a.1, dz, &mut t0, &mut t1)
            && t1 - t0 > 1e-6
        {
            cuts.push((t0.clamp(0.0, 1.0), t1.clamp(0.0, 1.0)));
        }
    }
    if cuts.is_empty() {
        return vec![(0.0, 1.0)];
    }
    cuts.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let mut spans = Vec::new();
    let mut cursor = 0.0_f64;
    for (cl, ch) in cuts {
        if cl > cursor + 1e-6 {
            spans.push((cursor, cl));
        }
        cursor = cursor.max(ch);
    }
    if cursor < 1.0 - 1e-6 {
        spans.push((cursor, 1.0));
    }
    spans
}

/// Draw one opening in an elevation, given its bounds in the elevation `(x, z)`
/// frame and a mapping `p(x, z) → screen`. Rectangles get a crisp box; every
/// other shape is filled + outlined from the shared [`window_outline`] silhouette
/// (so it matches the derived 3D `vertices` exactly). Shared by BOTH the
/// edit-on-surface and Mapping review elevations.
///
/// [`window_outline`]: ber_model::ber::fenestration::window_outline
#[allow(clippy::too_many_arguments)]
pub fn draw_elevation_opening(
    painter: &egui::Painter,
    p: &impl Fn(f64, f64) -> egui::Pos2,
    l: f64,
    r: f64,
    b: f64,
    tp: f64,
    shape: ber_model::ber::fenestration::WindowShape,
    fill: egui::Color32,
    stroke: egui::Color32,
    sw: f32,
) {
    use ber_model::ber::fenestration::{window_outline, WindowShape};
    let line =
        |a: egui::Pos2, c: egui::Pos2| painter.line_segment([a, c], egui::Stroke::new(sw, stroke));
    let (w, h) = (r - l, tp - b);
    if matches!(shape, WindowShape::Rectangular) {
        let ro = egui::Rect::from_two_pos(p(l, b), p(r, tp));
        painter.rect_filled(ro, 1.0, fill);
        line(ro.left_top(), ro.right_top());
        line(ro.right_top(), ro.right_bottom());
        line(ro.right_bottom(), ro.left_bottom());
        line(ro.left_bottom(), ro.left_top());
        return;
    }
    // The tessellated silhouette (local metres → elevation x/z → screen).
    let pts: Vec<egui::Pos2> = window_outline(shape, w as f32, h as f32)
        .iter()
        .map(|&(u, v)| p(l + u as f64, b + v as f64))
        .collect();
    // Fill: convex shapes as one polygon; the (concave) T as its two rects.
    if let WindowShape::TShape { band_m, stem_w_m } = shape {
        let stem_h = (h - band_m as f64).max(0.0);
        let (x0, x1) = (
            l + (w - stem_w_m as f64) / 2.0,
            l + (w + stem_w_m as f64) / 2.0,
        );
        painter.rect_filled(
            egui::Rect::from_two_pos(p(l, b + stem_h), p(r, tp)),
            1.0,
            fill,
        );
        painter.rect_filled(
            egui::Rect::from_two_pos(p(x0, b), p(x1, b + stem_h)),
            1.0,
            fill,
        );
    } else {
        painter.add(egui::Shape::convex_polygon(
            pts.clone(),
            fill,
            egui::Stroke::NONE,
        ));
    }
    for wnd in pts.windows(2) {
        line(wnd[0], wnd[1]);
    }
    if let (Some(&first), Some(&last)) = (pts.first(), pts.last()) {
        line(last, first);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A wall-outline line crossing no opening draws whole.
    #[test]
    fn wall_line_unobstructed_draws_whole() {
        let spans = wall_line_visible_spans((0.0, 2.4), (4.0, 2.4), &[]);
        assert_eq!(spans, vec![(0.0, 1.0)]);
    }

    /// THE report 7c4b1558 artifact: a horizontal wall edge at the fixed
    /// floor-slab offset (base + thickness) runs across the heat-loss section's
    /// window — a "floor thickness line" cutting the window. The clip removes
    /// exactly the window-spanning middle, keeping the wall line either side.
    #[test]
    fn floor_thickness_line_is_clipped_across_the_window() {
        // Wall 6 m long; a window along [3.4, 4.3], sill 0.3 → 1.5 (z-band). A
        // horizontal wall edge at z = 0.55 (base 0.3 + 0.25 slab) crosses it.
        let win = (3.4_f64, 4.3, 0.3, 1.5);
        let spans = wall_line_visible_spans((0.0, 0.55), (6.0, 0.55), &[win]);
        // Two pieces: [0, 3.4] and [4.3, 6.0] in t (÷6).
        assert_eq!(spans.len(), 2, "line split around the window: {spans:?}");
        assert!((spans[0].0 - 0.0).abs() < 1e-3 && (spans[0].1 - 3.4 / 6.0).abs() < 1e-3);
        assert!((spans[1].0 - 4.3 / 6.0).abs() < 1e-3 && (spans[1].1 - 1.0).abs() < 1e-3);
    }

    /// A line lying EXACTLY on the sill (window bottom) is NOT clipped — the
    /// window legitimately sits on the wall base / a real edge there, so only
    /// strictly-interior crossings are removed.
    #[test]
    fn line_on_the_sill_is_not_clipped() {
        let win = (1.0_f64, 3.0, 0.3, 1.5);
        // z = 0.3 is the window's bottom edge → kept whole.
        assert_eq!(
            wall_line_visible_spans((0.0, 0.3), (4.0, 0.3), &[win]),
            vec![(0.0, 1.0)]
        );
        // z = 1.5 is the window's top edge → also kept whole.
        assert_eq!(
            wall_line_visible_spans((0.0, 1.5), (4.0, 1.5), &[win]),
            vec![(0.0, 1.0)]
        );
    }

    /// A vertical side edge at a wall end never crosses an interior opening, so
    /// it draws whole; and a line above the window's head is untouched.
    #[test]
    fn side_edge_and_above_head_are_untouched() {
        let win = (1.0_f64, 3.0, 0.3, 1.5);
        // Vertical edge at x = 0 (wall end), full height.
        assert_eq!(
            wall_line_visible_spans((0.0, 0.0), (0.0, 2.4), &[win]),
            vec![(0.0, 1.0)]
        );
        // Horizontal line at z = 2.0, above the 1.5 head → untouched.
        assert_eq!(
            wall_line_visible_spans((0.0, 2.0), (4.0, 2.0), &[win]),
            vec![(0.0, 1.0)]
        );
    }
}
