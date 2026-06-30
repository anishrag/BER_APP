//! The "fanned-out" canvas view for the Structure-phase **Edit attributes**
//! session. When a [`StructureEdit`] is open, the plan switches from the normal
//! storey render to this exploded view: the storey is drawn faint as a ghost,
//! and each construction GROUP's surfaces are pulled radially OUT from the
//! storey's bounding-box centre, colour-coded per group, with a dashed connector
//! back to where they really sit. The canvas (`canvas.rs`) places one small
//! popup per group on top — clicking it makes that group active.
//!
//! Group separation is by INDEX angle (each group gets its own spoke of the
//! fan), not the group's natural position, so groups never overlap regardless of
//! where their surfaces are. The painter here and the popup/hit-test code in
//! `canvas.rs` share [`fan_transform`] + [`group_fan_layout`] so both agree on
//! where every fanned surface lands.

use ber_model::ber::structural::Storey;
use eframe::egui;

use crate::controller::app_controller::{StructureEdit, SurfaceKind};

use super::{fit_transform, storey_2d::bounding_box, style, Transform2d, ViewParams};

/// One group's placement in the fan: the world-space translation pulling its
/// surfaces out of the ghost, the group's true (un-offset) centroid, its colour,
/// and how many surfaces it holds (for the popup label).
pub(crate) struct GroupFan {
    pub group_index: usize,
    /// World-space translation applied to every surface in the group.
    pub offset: (f64, f64),
    /// The group's true centroid (un-offset) — the connector's anchor.
    pub centroid: (f64, f64),
    pub color: egui::Color32,
    pub count: usize,
}

impl GroupFan {
    /// The group's centroid in the FANNED (pulled-out) position — where the
    /// popup sits and the connector ends.
    pub fn fanned_centroid(&self) -> (f64, f64) {
        (
            self.centroid.0 + self.offset.0,
            self.centroid.1 + self.offset.1,
        )
    }
}

/// Distinct per-group colours, cycled by group index.
const PALETTE: &[egui::Color32] = &[
    egui::Color32::from_rgb(120, 170, 240), // blue
    egui::Color32::from_rgb(245, 150, 70),  // orange
    egui::Color32::from_rgb(120, 210, 130), // green
    egui::Color32::from_rgb(225, 120, 200), // magenta
    egui::Color32::from_rgb(235, 210, 80),  // yellow
    egui::Color32::from_rgb(150, 130, 230), // violet
    egui::Color32::from_rgb(110, 215, 215), // teal
    egui::Color32::from_rgb(225, 110, 110), // red
];

pub(crate) fn group_color(i: usize) -> egui::Color32 {
    PALETTE[i % PALETTE.len()]
}

/// The fan's geometry for `storey`: bounding-box centre and the radial pull-out
/// distance. `None` when the storey has no footprint to centre on.
fn fan_geometry(storey: &Storey) -> Option<((f64, f64), f64)> {
    let ((min_x, min_y), (max_x, max_y)) = bounding_box(storey)?;
    let centre = ((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);
    // Pull groups out by ~40% of the smaller footprint dimension — enough to
    // clear the ghost, small enough that the expanded fit keeps them on-canvas.
    let r = (max_x - min_x).min(max_y - min_y).max(1.0) * 0.4;
    Some((centre, r))
}

/// The screen↔world transform for the fan view: the storey's bbox GROWN by the
/// fan radius on every side so the pulled-out groups stay in frame, then fitted
/// to `rect` with the user's pan/zoom (same `fit_transform` the normal render
/// uses). Shared by the painter and the canvas popup/hit-test code so both place
/// fanned surfaces identically. `None` when the storey has no footprint.
pub(crate) fn fan_transform(
    rect: egui::Rect,
    storey: &Storey,
    view: ViewParams,
) -> Option<Transform2d> {
    let ((min_x, min_y), (max_x, max_y)) = bounding_box(storey)?;
    let (_, r) = fan_geometry(storey)?;
    let min_xy = (min_x - r, min_y - r);
    let max_xy = (max_x + r, max_y + r);
    Some(fit_transform(rect, min_xy, max_xy, view))
}

/// The per-group fan placement: each group gets its own spoke (index angle), so
/// groups fan into a ring around the bbox centre and never overlap. Empty when
/// there's no footprint or no groups.
pub(crate) fn group_fan_layout(storey: &Storey, edit: &StructureEdit) -> Vec<GroupFan> {
    let Some((centre, r)) = fan_geometry(storey) else {
        return Vec::new();
    };
    let n = edit.groups.len();
    if n == 0 {
        return Vec::new();
    }
    edit.groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            // One spoke per group, starting at the top and going clockwise. A
            // lone group stays put (nothing to disambiguate).
            let offset = if n <= 1 {
                (0.0, 0.0)
            } else {
                let angle =
                    -std::f64::consts::FRAC_PI_2 + std::f64::consts::TAU * (i as f64) / (n as f64);
                (r * angle.cos(), r * angle.sin())
            };
            let centroid = group_centroid(storey, g.ty.kind(), g).unwrap_or(centre);
            GroupFan {
                group_index: i,
                offset,
                centroid,
                color: group_color(i),
                count: g.ids.len(),
            }
        })
        .collect()
}

/// The true (un-offset) centroid of a group's surfaces, averaged over its
/// regions of `kind`. `None` when none of the ids resolve to a region.
fn group_centroid(
    storey: &Storey,
    kind: SurfaceKind,
    group: &crate::controller::app_controller::EditGroup,
) -> Option<(f64, f64)> {
    let mut sum = (0.0, 0.0);
    let mut n = 0u32;
    for id in &group.ids {
        if let Some(c) = region_centroid(storey, kind, *id) {
            sum.0 += c.0;
            sum.1 += c.1;
            n += 1;
        }
    }
    (n > 0).then(|| (sum.0 / n as f64, sum.1 / n as f64))
}

/// The point a group's in-place connector LINE should target. Floors/ceilings use
/// the group `centroid`; WALLS use the base midpoint of the wall (in the group)
/// NEAREST that centroid — pointing at an actual selected wall reads clearer
/// than at the average position, which can land in empty space (e.g. an
/// L-shaped wall run). Falls back to the centroid if no wall resolves.
pub(crate) fn group_connector_anchor(
    storey: &Storey,
    group: &crate::controller::app_controller::EditGroup,
    centroid: (f64, f64),
) -> (f64, f64) {
    let kind = group.ty.kind();
    if kind != SurfaceKind::Walls {
        return centroid;
    }
    let dist2 = |p: &(f64, f64)| (p.0 - centroid.0).powi(2) + (p.1 - centroid.1).powi(2);
    group
        .ids
        .iter()
        .filter_map(|id| region_centroid(storey, kind, *id))
        .min_by(|a, b| {
            dist2(a)
                .partial_cmp(&dist2(b))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(centroid)
}

/// A single region's plan centroid for `kind` (wall → base midpoint; floor /
/// roof → region centroid).
fn region_centroid(
    storey: &Storey,
    kind: SurfaceKind,
    id: ber_model::ber::common::ItemId,
) -> Option<(f64, f64)> {
    match kind {
        SurfaceKind::Walls => storey
            .walls
            .regions
            .iter()
            .find(|r| r.id == id)
            .and_then(|r| r.base_midpoint())
            .map(|m| (m.x, m.y)),
        SurfaceKind::Floors => storey.floors.regions.iter().find(|r| r.id == id).map(|r| {
            let c = r.centroid();
            (c.x, c.y)
        }),
        SurfaceKind::Ceilings => storey.ceilings.regions.iter().find(|r| r.id == id).map(|r| {
            let c = r.centroid();
            (c.x, c.y)
        }),
    }
}

/// Paint the fanned view: the faint storey ghost, then each group's surfaces
/// pulled out along its spoke, colour-coded (the active group full-strength, the
/// rest dimmer), each joined to its true spot by a dashed connector.
pub(crate) fn render_structure_fan(
    painter: &egui::Painter,
    rect: egui::Rect,
    storey: &Storey,
    edit: &StructureEdit,
    view: ViewParams,
) {
    let Some(t) = fan_transform(rect, storey, view) else {
        return;
    };

    // Ghost: every floor outline faint, so the pulled-out groups read as
    // lifting off the real plan.
    for region in &storey.floors.regions {
        let pts: Vec<egui::Pos2> = ring_screen(&floor_ring(region), &t, (0.0, 0.0));
        if pts.len() >= 3 {
            painter.add(egui::Shape::closed_line(
                pts,
                egui::Stroke::new(1.0, style::faded(style::FLOOR_LINE)),
            ));
        }
    }

    let layout = group_fan_layout(storey, edit);
    for fan in &layout {
        let active = fan.group_index == edit.active;
        // Dashed connector from the true centroid to the fanned centroid.
        if fan.offset != (0.0, 0.0) {
            let a = t.apply(fan.centroid.0, fan.centroid.1);
            let b = t.apply(fan.centroid.0 + fan.offset.0, fan.centroid.1 + fan.offset.1);
            super::draw_dashed_segment(
                painter,
                a,
                b,
                egui::Stroke::new(1.0, style::faded(fan.color)),
            );
        }
        let group = &edit.groups[fan.group_index];
        let kind = group.ty.kind();
        for id in &group.ids {
            draw_fanned_surface(
                painter, &t, storey, kind, *id, fan.offset, fan.color, active,
            );
        }
    }
}

/// Paint each construction GROUP's surfaces in its group colour, IN PLACE (no
/// fan offset) — the default Edit-attributes view, where the plan stays where it
/// sits (zoomed out by the controller) and the selection is recoloured per group
/// under the floating attribute windows. The base plan is drawn first by
/// `storey_2d`; this only overlays the group colours. `t` is that same plan
/// transform so the overlay registers exactly. The exploded fan
/// ([`render_structure_fan`]) is the alternative view.
pub(crate) fn render_groups_inplace(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    edit: &StructureEdit,
) {
    for (i, group) in edit.groups.iter().enumerate() {
        let color = group_color(i);
        let kind = group.ty.kind();
        for id in &group.ids {
            // `active = true` for every group: in place, all groups are shown
            // full-strength (each has its own editable window — none is dimmed).
            draw_fanned_surface(painter, t, storey, kind, *id, (0.0, 0.0), color, true);
        }
    }
}

/// Draw one surface of `kind` translated by `offset`, in `color` (full-strength
/// for the active group, dimmed otherwise).
fn draw_fanned_surface(
    painter: &egui::Painter,
    t: &Transform2d,
    storey: &Storey,
    kind: SurfaceKind,
    id: ber_model::ber::common::ItemId,
    offset: (f64, f64),
    color: egui::Color32,
    active: bool,
) {
    let line = if active { color } else { style::faded(color) };
    // A LIGHT translucent tint (proper non-premultiplied alpha) so the group
    // reads as a coloured wash with a solid coloured boundary — not the near-
    // white, washed-out fill the premultiplied `highlight_fill` produced.
    let [r, g, b, _] = line.to_array();
    let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, 55);
    match kind {
        SurfaceKind::Walls => {
            let Some(r) = storey.walls.regions.iter().find(|r| r.id == id) else {
                return;
            };
            let v = &r.polygon.exterior;
            if v.len() < 2 {
                return;
            }
            let a = t.apply(v[0].x + offset.0, v[0].y + offset.1);
            let b = t.apply(v[1].x + offset.0, v[1].y + offset.1);
            // A dark casing under the coloured core so the group highlight reads
            // clearly even when painted over the wall-exposure colours — the
            // selection has to be unmistakable on top of everything below it.
            let (casing, core) = if active { (9.0, 5.5) } else { (6.0, 3.0) };
            painter.line_segment(
                [a, b],
                egui::Stroke::new(casing, egui::Color32::from_black_alpha(180)),
            );
            painter.line_segment([a, b], egui::Stroke::new(core, line));
        }
        SurfaceKind::Floors => {
            let Some(r) = storey.floors.regions.iter().find(|r| r.id == id) else {
                return;
            };
            fill_outline(painter, t, &floor_ring(r), offset, fill, line, active);
        }
        SurfaceKind::Ceilings => {
            let Some(r) = storey.ceilings.regions.iter().find(|r| r.id == id) else {
                return;
            };
            let ring: Vec<(f64, f64)> = r.polygon.exterior.iter().map(|p| (p.x, p.y)).collect();
            fill_outline(painter, t, &ring, offset, fill, line, active);
        }
    }
}

/// A floor region's exterior ring as `(x, y)` (drops the closing duplicate).
fn floor_ring(region: &ber_model::ber::structural::FloorRegion) -> Vec<(f64, f64)> {
    let mut ring: Vec<(f64, f64)> = region
        .polygon
        .exterior()
        .coords()
        .map(|c| (c.x, c.y))
        .collect();
    if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
        ring.pop();
    }
    ring
}

/// Screen points of `ring` translated by `offset`.
fn ring_screen(ring: &[(f64, f64)], t: &Transform2d, offset: (f64, f64)) -> Vec<egui::Pos2> {
    ring.iter()
        .map(|(x, y)| t.apply(x + offset.0, y + offset.1))
        .collect()
}

/// Fill a translated area ring (earcut, like `draw_floor`) and stroke its
/// outline.
fn fill_outline(
    painter: &egui::Painter,
    t: &Transform2d,
    ring: &[(f64, f64)],
    offset: (f64, f64),
    fill: egui::Color32,
    line: egui::Color32,
    active: bool,
) {
    if ring.len() < 3 {
        return;
    }
    let screen: Vec<egui::Pos2> = ring_screen(ring, t, offset);
    let flat: Vec<f64> = ring
        .iter()
        .flat_map(|(x, y)| [x + offset.0, y + offset.1])
        .collect();
    if let Ok(triangles) = earcutr::earcut(&flat, &[], 2) {
        let mut mesh = egui::Mesh::default();
        for &p in &screen {
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
    painter.add(egui::Shape::closed_line(
        screen,
        egui::Stroke::new(if active { 2.5 } else { 1.5 }, line),
    ));
}
