//! The shared drawing core, extracted from `src/ui/render` so the PC report
//! generator (BER_deap) renders the house with the SAME tested code as the
//! tablet: the orbitable 3D house (`storey_3d`, `full_house_3d`), the side
//! elevations (`elevation`), and the projection / fit-transform / palette
//! machinery they share. Everything paints through `egui::Painter`, which
//! works headlessly — run a frame on a bare `egui::Context`, collect the
//! `FullOutput` shapes, and serialize them to any target (the tablet's GPU
//! or the report's SVG).
//!
//! Depends on `egui` (not `eframe`) so consumers don't pull a window stack.

pub mod elevation;
pub mod full_house_3d;
pub mod pick_3d;
pub mod storey_2d;
pub mod storey_3d;

use ber_model::ber::ber_struct::Ber;
use domain::sketch::DisplayMode;

/// User-controlled view transform passed in from the controller. `zoom = 1.0`
/// and zero offsets = pure auto-fit. Higher zoom enlarges; positive offset_x
/// pans the view right. `yaw_deg`/`pitch_deg` are the 3D orbit angles
/// (ignored by the 2D views); their defaults reproduce the historical fixed
/// isometric projection.
#[derive(Debug, Clone, Copy)]
pub struct ViewParams {
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    pub yaw_deg: f32,
    pub pitch_deg: f32,
}

impl Default for ViewParams {
    fn default() -> Self {
        Self::identity()
    }
}

impl ViewParams {
    pub fn identity() -> Self {
        Self {
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            yaw_deg: domain::view::DEFAULT_YAW_DEG,
            pitch_deg: domain::view::DEFAULT_PITCH_DEG,
        }
    }

    /// Orbit-parameterized orthographic projection of a world point (z up)
    /// onto the 2D view plane (y down) — see [`project_3d`].
    pub fn project_3d(&self, x: f64, y: f64, z: f64) -> (f64, f64) {
        project_3d(x, y, z, self.yaw_deg, self.pitch_deg)
    }
}

/// Shared color palette + opacity constants for all renderers.
pub mod style {
    use egui::Color32;

    pub const FLOOR_FILL: Color32 = Color32::from_rgba_premultiplied(40, 70, 110, 50);
    pub const FLOOR_LINE: Color32 = Color32::from_rgb(120, 170, 240);
    pub const ROOF_LINE: Color32 = Color32::from_rgb(220, 160, 80);
    /// Exposure-class colours — shared between the 2D exposure overlay and the
    /// 3D wall-region rendering so a wall reads the same in both views.
    pub const EXPOSED_WALL: Color32 = Color32::from_rgb(255, 140, 40);
    pub const SEMI_EXPOSED_WALL: Color32 = Color32::from_rgb(240, 205, 40);
    pub const NON_HEAT_LOSS_WALL: Color32 = Color32::from_rgb(95, 145, 215);
    /// Walls with no exposure marked yet — neutral grey in both views.
    pub const UNSET_WALL: Color32 = Color32::from_rgb(150, 150, 150);
    /// Ground-contact walls (a riser-only assessor choice) — earth tone.
    pub const GROUND_WALL: Color32 = Color32::from_rgb(150, 105, 60);
    /// Porch INNER walls (`Storey::porch_walls`) — the 2D porch overlay's
    /// violet, so the openings-placeable porch boundary reads as porch (not
    /// envelope) in the 3D views too.
    pub const PORCH_WALL: Color32 = Color32::from_rgb(150, 95, 185);
    pub const WINDOW_FILL: Color32 = Color32::from_rgba_premultiplied(130, 200, 230, 180);
    pub const DOOR_FILL: Color32 = Color32::from_rgba_premultiplied(180, 130, 80, 200);
    /// Bright marquee colour for the Structure pre-edit SELECTION outline — a
    /// vivid gold that pops against the floor-blue / roof-orange fills.
    pub const SELECTION_LINE: Color32 = Color32::from_rgb(255, 215, 0);
    pub const DIMENSION_TEXT: Color32 = Color32::from_rgb(200, 200, 200);
    pub const HINT_TEXT: Color32 = Color32::from_rgb(140, 140, 140);
    pub const FADED_OPACITY: u8 = 50;
    /// Alpha for a highlighted surface's shaded fill — opaque enough to read as
    /// "these are the surfaces we mean", translucent enough to see geometry
    /// (and stacked faces in 3D) through it.
    pub const HIGHLIGHT_FILL_OPACITY: u8 = 95;

    pub fn faded(c: Color32) -> Color32 {
        let [r, g, b, _] = c.to_array();
        Color32::from_rgba_premultiplied(r, g, b, FADED_OPACITY)
    }

    /// `c` as a translucent shade for filling a highlighted surface region.
    pub fn highlight_fill(c: Color32) -> Color32 {
        let [r, g, b, _] = c.to_array();
        Color32::from_rgba_premultiplied(r, g, b, HIGHLIGHT_FILL_OPACITY)
    }

    /// Alpha for a NON-heat-loss surface's shaded fill in 3D — much fainter
    /// than a selection highlight: enough to read as "internal, not part of
    /// the envelope" without hiding the geometry behind it.
    pub const INTERNAL_FILL_OPACITY: u8 = 30;

    /// `c` as the faint translucent shade for a non-heat-loss (internal)
    /// surface in the 3D views.
    pub fn internal_fill(c: Color32) -> Color32 {
        let [r, g, b, _] = c.to_array();
        Color32::from_rgba_premultiplied(r, g, b, INTERNAL_FILL_OPACITY)
    }

    /// Per-storey distinct translucent fill hues for the overlaid all-storeys
    /// floor-exposure view, cycled for many storeys. Translucent so overlapping
    /// floors from different storeys stay readable through each other; the
    /// exposure is overlaid as a separate coloured OUTLINE on top.
    const STOREY_FILL_HUES: &[(u8, u8, u8)] = &[
        (80, 140, 230),  // blue
        (230, 150, 70),  // orange
        (110, 200, 120), // green
        (200, 110, 200), // magenta
        (210, 200, 90),  // gold
        (90, 200, 210),  // teal
    ];

    /// The translucent fill colour for storey `idx` in the overlaid floor view
    /// (cycles through [`STOREY_FILL_HUES`]).
    pub fn storey_fill(idx: usize) -> Color32 {
        let (r, g, b) = STOREY_FILL_HUES[idx % STOREY_FILL_HUES.len()];
        Color32::from_rgba_unmultiplied(r, g, b, 55)
    }
}

/// Linear 2D transform: source-space (meters/pixels) → screen-space (px).
/// Preserves aspect ratio; centres the source bounding box inside the dest
/// rect. The user's pan/zoom (`ViewParams`) is folded in: `zoom` multiplies
/// the auto-fit scale, and `offset_x/y` shift the final screen position.
pub struct Transform2d {
    scale: f64,
    cx: f64, // dest centre x (px)
    cy: f64, // dest centre y (px)
    sx: f64, // source centre x
    sy: f64, // source centre y
    user_offset_x: f64,
    user_offset_y: f64,
}

impl Transform2d {
    pub fn apply(&self, x: f64, y: f64) -> egui::Pos2 {
        egui::pos2(
            (self.cx + (x - self.sx) * self.scale + self.user_offset_x) as f32,
            (self.cy + (y - self.sy) * self.scale + self.user_offset_y) as f32,
        )
    }

    /// Inverse of `apply`: take a canvas-pixel point and return the
    /// corresponding world-coord point. Used by Edit-mode hit-testing.
    pub fn inverse(&self, px: f32, py: f32) -> (f64, f64) {
        let x = (px as f64 - self.cx - self.user_offset_x) / self.scale + self.sx;
        let y = (py as f64 - self.cy - self.user_offset_y) / self.scale + self.sy;
        (x, y)
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }
}

/// The pre-zoom world→pixel scale that fits `min_xy..max_xy` inside `rect` with
/// padding (aspect-ratio preserving). Exposed so callers that need several
/// sub-views to share one scale — e.g. the "All storeys" overview, where a
/// common scale lets the reader compare storey sizes — can compute the shared
/// value before building each transform.
pub fn auto_fit_scale(rect: egui::Rect, min_xy: (f64, f64), max_xy: (f64, f64)) -> f64 {
    // Padding proportional to the smaller canvas dimension (~8%), so the fitted
    // geometry keeps a comfortable margin instead of pressing against the edges.
    // A fixed 30 px was a sliver on a ~900 px-tall tablet canvas — the first
    // storey outline (fit by height, in raw pixel space) landed edge-to-edge and
    // read as "too big", so the assessor had to zoom out and pan. Floored at the
    // historical 30 px so small desktop windows and the "All storeys" overview
    // cells keep at least the previous margin (their shared-scale fit math is
    // then bit-for-bit unchanged, since a fraction of a common rect cancels).
    let pad = (rect.width().min(rect.height()) * 0.08).max(30.0);
    let target_w = (rect.width() - 2.0 * pad).max(1.0);
    let target_h = (rect.height() - 2.0 * pad).max(1.0);
    let src_w = (max_xy.0 - min_xy.0).max(1e-6);
    let src_h = (max_xy.1 - min_xy.1).max(1e-6);
    (target_w as f64 / src_w).min(target_h as f64 / src_h)
}

/// Compute a transform that fits a source bounding box inside `rect` with
/// padding, then applies the user's `view` transform (zoom + pan) on top.
pub fn fit_transform(
    rect: egui::Rect,
    min_xy: (f64, f64),
    max_xy: (f64, f64),
    view: ViewParams,
) -> Transform2d {
    let auto_scale = auto_fit_scale(rect, min_xy, max_xy);

    Transform2d {
        scale: auto_scale * view.zoom as f64,
        cx: rect.center().x as f64,
        cy: rect.center().y as f64,
        sx: (min_xy.0 + max_xy.0) * 0.5,
        sy: (min_xy.1 + max_xy.1) * 0.5,
        user_offset_x: view.offset_x as f64,
        user_offset_y: view.offset_y as f64,
    }
}

/// Orbit-parameterized orthographic projection (the 3D camera): rotate the
/// world by `yaw_deg` about the vertical (z-up) axis, tilt by `pitch_deg`
/// (camera elevation above the horizon — 90° = top-down plan), drop depth.
/// World (x, y, z) → 2D plane (x', y'), y' down. Used by `storey_3d` and
/// `full_house_3d`.
///
/// At the default angles (yaw 45°, pitch atan(1/√2) ≈ 35.264°) this equals
/// the historical fixed isometric `((x − y)·cos30°, (x + y)·sin30° − z)`
/// scaled by a uniform √(3/2), which the auto-fit cancels — so the default
/// view renders identically to the old fixed projection.
///
/// Rotation happens about the world origin; the resulting uniform translation
/// of the projected bbox is absorbed by `fit_transform` centring on that
/// bbox, so no explicit centroid pivot is needed.
pub fn project_3d(x: f64, y: f64, z: f64, yaw_deg: f32, pitch_deg: f32) -> (f64, f64) {
    let yaw = (yaw_deg as f64).to_radians();
    let pitch = (pitch_deg as f64).to_radians();
    // Yaw about the vertical axis.
    let u = x * yaw.cos() - y * yaw.sin();
    let v = x * yaw.sin() + y * yaw.cos();
    // Pitch: foreshorten the receding axis, mix in height.
    (u, v * pitch.sin() - z * pitch.cos())
}

/// Returns `base` if `target` should be highlighted, else a faded variant.
pub fn color_for(target: DisplayMode, current: DisplayMode, base: egui::Color32) -> egui::Color32 {
    match current {
        DisplayMode::AllSurfaces | DisplayMode::FloorMapDims => base,
        other if other == target => base,
        _ => style::faded(base),
    }
}

/// Fill a projected (screen-space) polygon with `color`, triangulating with
/// earcut so concave rings (L / U footprints) fill correctly rather than
/// fanning across the concavity. A trailing closing-duplicate vertex is dropped
/// (geo rings carry one); no-op below 3 unique points.
pub fn fill_polygon(painter: &egui::Painter, pts: &[egui::Pos2], color: egui::Color32) {
    let mut pts = pts.to_vec();
    if pts.len() >= 2 && pts[0] == pts[pts.len() - 1] {
        pts.pop();
    }
    if pts.len() < 3 {
        return;
    }
    let flat: Vec<f64> = pts.iter().flat_map(|p| [p.x as f64, p.y as f64]).collect();
    if let Ok(triangles) = earcutr::earcut(&flat, &[], 2) {
        let mut mesh = egui::Mesh::default();
        for p in &pts {
            mesh.vertices.push(egui::epaint::Vertex {
                pos: *p,
                uv: egui::epaint::WHITE_UV,
                color,
            });
        }
        for idx in triangles {
            mesh.indices.push(idx as u32);
        }
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// Draw a dashed line between two screen points. ~6px dash, ~4px gap.
pub fn draw_dashed_segment(
    painter: &egui::Painter,
    from: egui::Pos2,
    to: egui::Pos2,
    stroke: egui::Stroke,
) {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length < 1.0 {
        return;
    }
    let dash = 6.0_f32;
    let gap = 4.0_f32;
    let cycle = dash + gap;
    let n_cycles = (length / cycle).ceil() as usize;
    let ux = dx / length;
    let uy = dy / length;
    for i in 0..n_cycles {
        let t0 = (i as f32 * cycle).min(length);
        let t1 = (t0 + dash).min(length);
        if t1 <= t0 {
            break;
        }
        let p0 = egui::pos2(from.x + ux * t0, from.y + uy * t0);
        let p1 = egui::pos2(from.x + ux * t1, from.y + uy * t1);
        painter.line_segment([p0, p1], stroke);
    }
}

pub fn union_bbox(
    a: Option<((f64, f64), (f64, f64))>,
    b: Option<((f64, f64), (f64, f64))>,
) -> Option<((f64, f64), (f64, f64))> {
    match (a, b) {
        (Some(a), Some(b)) => Some((
            (a.0 .0.min(b.0 .0), a.0 .1.min(b.0 .1)),
            (a.1 .0.max(b.1 .0), a.1 .1.max(b.1 .1)),
        )),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Move each screen-space ring vertex `px` toward the ring's centroid — an
/// approximate inward inset so a selection outline sits just INSIDE a region's
/// edges instead of coinciding with (and disappearing into) the surface outline.
pub fn inset_ring(pts: &[egui::Pos2], px: f32) -> Vec<egui::Pos2> {
    let n = pts.len();
    if n < 3 {
        return pts.to_vec();
    }
    let (mut cx, mut cy) = (0.0f32, 0.0f32);
    for p in pts {
        cx += p.x;
        cy += p.y;
    }
    let c = egui::pos2(cx / n as f32, cy / n as f32);
    pts.iter()
        .map(|p| {
            let d = egui::vec2(c.x - p.x, c.y - p.y);
            let len = d.length();
            if len < 1.0 {
                *p
            } else {
                *p + d * (px / len)
            }
        })
        .collect()
}

/// The fit transform `full_house_3d::render` draws with: every storey's
/// projected bounding box (at its floor datum) combined, fitted into `rect`.
/// Exposed so callers that annotate ON TOP of the 3D view (hotspot labels in
/// the report, hit-testing on the tablet) can map world points to the same
/// screen positions the renderer used. `None` when no storey has geometry or
/// a ceiling height is missing.
pub fn full_house_transform(rect: egui::Rect, ber: &Ber, view: ViewParams) -> Option<Transform2d> {
    if ber.storeys.is_empty() || ber.storeys.iter().any(|s| s.ceiling_height_m.is_none()) {
        return None;
    }
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
    let (min_xy, max_xy) = combined?;
    Some(fit_transform(rect, min_xy, max_xy, view))
}
