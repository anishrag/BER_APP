//! Axonometric 3D rendering of every Storey in the BER, stacked vertically.
//!
//! Storeys are placed at the floor datums from `Ber::storey_floor_datums()`:
//! slab-thickness-aware (anchor on the lowest covered flat ceiling below +
//! the slab) where a thickness is recorded, and the legacy
//! sum-of-default-ceiling-heights stacking where it isn't. Within its band
//! each storey renders its real per-region floor levels and roof z via
//! `storey_3d::render_at_z_offset`. The transform is fitted across all
//! storeys' projected bounding boxes so the whole building sits inside the
//! canvas.

use std::collections::HashSet;

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use domain::sketch::DisplayMode;

use crate::{fit_transform, storey_3d, style, ViewParams};

pub fn render(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: &Ber,
    display_mode: DisplayMode,
    view: ViewParams,
    // Surface / opening ids to shade across the whole stack — empty for the plain
    // all-storeys overview, the current group's surfaces in the Structure review.
    selected: &HashSet<ItemId>,
    // World-3D points to mark with a bright dot (the Objects review's hotspots
    // for the selected kind). Empty otherwise.
    hotspots: &[(f64, f64, f64)],
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

    // The stack's z-step per storey IS its default ceiling height — every storey needs
    // one before the full house can stack. The controller's first-3D gate fills
    // them from the All-storeys tab; an unset one here (e.g. a storey added
    // while already in 3D) paints the hint instead of guessing a height.
    if ber.storeys.iter().any(|s| s.ceiling_height_m.is_none()) {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "Set a default ceiling height to view in 3D.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    }

    // First pass: per-storey floor datums (the slab-thickness-aware datum
    // rule — legacy records without thicknesses stack on the ceiling heights
    // exactly as before) and the combined projected bbox. The heights gate
    // above guarantees the datums' ceiling-height inputs are all set.
    let z_offsets = ber.storey_floor_datums();
    let mut combined: Option<((f64, f64), (f64, f64))> = None;

    for (storey, &z) in ber.storeys.iter().zip(&z_offsets) {
        if let Some(bb) = storey_3d::projected_bb_for_storey(storey, z, view) {
            combined = Some(match combined {
                None => bb,
                Some((cur_min, cur_max)) => (
                    (cur_min.0.min(bb.0 .0), cur_min.1.min(bb.0 .1)),
                    (cur_max.0.max(bb.1 .0), cur_max.1.max(bb.1 .1)),
                ),
            });
        }
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

    let t = fit_transform(rect, min_xy, max_xy, view);

    // Second pass: render each storey using the shared transform.
    // Iterate top-down so lower storeys' floors don't paint over upper-storey walls
    // (closer-to-camera in the default view is drawn last; with a free orbit
    // this ordering is approximate — wireframe rendering keeps it readable).
    // This dwelling's own storey junctions carry the derived `InterStorey`
    // exposure and are skipped per region inside `render_at_z_offset` (they'd
    // double-draw the same plane at every junction and say nothing about heat
    // loss). Non-heat-loss/party surfaces are unambiguously external-dwelling
    // boundaries now and draw as faint translucent fills with a faded outline.
    for (i, storey) in ber.storeys.iter().enumerate().rev() {
        storey_3d::render_at_z_offset(
            painter,
            &t,
            storey,
            display_mode,
            z_offsets[i],
            view,
            selected,
            true,
        );
    }

    // The dwelling ROOF: a SEPARATE structure above the ceiling (not the ceiling
    // itself). Each roof area's planes are derived from its shape/pitch/ridge/
    // footprint, with z LOCAL to its covered storey — add that storey's floor
    // datum to place them in world z, then draw each plane as a wireframe.
    let roof_stroke = egui::Stroke::new(1.5, egui::Color32::from_rgb(150, 90, 60));
    for area in &ber.roof.areas {
        let Some(&datum) = z_offsets.get(area.storey_idx) else {
            continue;
        };
        // Roof lines only carry information where the roof DEPARTS from the
        // ceiling (a pitched roof over a flat ceiling). A roof that coincides
        // with the ceiling beneath it (flat-on-flat, or FollowCeiling) would
        // just double the ceiling's lines — skip it.
        if ber
            .storeys
            .get(area.storey_idx)
            .is_some_and(|s| area.follows_ceiling(&s.ceilings, &area.dormer_patches(ber)))
        {
            continue;
        }
        // Built via the SAME apex/ridge fan the ceiling uses, over the grouped
        // ceiling footprint — so the roof shares the ceiling's plan vertices.
        // Covered storeys' dormers are composed in: the roof joins each
        // dormer's flat top instead of draping its plane over the dormer.
        for region in area
            .regions_with_dormers(
                ber,
                ber_model::ber::roof::DEFAULT_OVERHANG_M,
                ItemId::default(),
            )
            .regions
        {
            let ring = &region.polygon.exterior;
            if ring.len() < 3 {
                continue;
            }
            let pts: Vec<egui::Pos2> = ring
                .iter()
                .map(|v| {
                    let (px, py) = view.project_3d(v.x, v.y, datum + v.z);
                    t.apply(px, py)
                })
                .collect();
            // A sloped face's BASE edges (both ends at the eave) lie along the
            // wall tops / ceiling outline already drawn below — repeating them
            // just thickens the clutter, so only the RISING edges (rakes,
            // hips, the ridge) are drawn. An all-at-eave (flat) face keeps its
            // full outline — skipping its base would erase it entirely.
            let eave = area.eave_z;
            let sloped = ring.iter().any(|v| v.z > eave + 1e-3);
            for i in 0..pts.len() {
                let (a, b) = (&ring[i], &ring[(i + 1) % ring.len()]);
                if sloped && (a.z - eave).abs() < 1e-3 && (b.z - eave).abs() < 1e-3 {
                    continue;
                }
                painter.line_segment([pts[i], pts[(i + 1) % pts.len()]], roof_stroke);
            }
        }
    }

    // Hotspot markers (Objects review): a bright dot at each world point.
    for &(x, y, z) in hotspots {
        let (px, py) = view.project_3d(x, y, z);
        let sp = t.apply(px, py);
        painter.circle_filled(sp, 7.0, egui::Color32::from_rgb(255, 210, 70));
        painter.circle_stroke(sp, 7.0, egui::Stroke::new(1.5, egui::Color32::BLACK));
    }
}
