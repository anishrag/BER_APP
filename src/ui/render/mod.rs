//! Renders the persisted BER state as 2D / 3D views in the sketch canvas.
//!
//! Three rendering paths, selected by [`SketchMode`]:
//!   - `Storey2D`  — top-down 2D plan of the current storey
//!   - `Storey3D`  — axonometric 3D of the current storey
//!   - `FullHouse` — axonometric 3D of all storeys stacked
//!
//! Each view honours [`DisplayMode`] by highlighting one surface category and
//! fading the others:
//!   - `FloorMapDims` — show dimensions labels (default — best with `Storey2D`)
//!   - `Floors`      — highlight floor polygons
//!   - `Walls`       — highlight wall edges
//!   - `Roofs`       — highlight ceiling regions / roof-height labels
//!   - `AllSurfaces` — render every category at full opacity
//!
//! Rendering reads from a `Ber` clone supplied by the controller, so the user
//! sees what's actually persisted to SQLite, not in-flight sketch state.
//!
//! **Units caveat:** geometry is currently stored in canvas pixel coordinates
//! (the sketch input is pixels and there's no scale-to-meters step yet). The
//! dimension labels print the raw polygon distance — they'll become meters
//! once the assessor sets a scale.

pub mod full_house_3d;
pub mod storey_2d;
pub mod storey_3d;

use datatypes::ber::ber_struct::Ber;
use domain::sketch::{DisplayMode, SketchMode};
use eframe::egui;
use manip_2d_plan::CollinearitySuggestion;

/// Render the appropriate BER view into `rect`. Silent no-op if `ber` is None.
/// `highlight` is the current collinearity-review suggestion (if any) — its
/// two candidate edges get a bright overlay so the user sees what they're
/// being asked about. Only applies to the 2D view.
pub fn render_ber(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: usize,
    sketch_mode: SketchMode,
    display_mode: DisplayMode,
    highlight: Option<&CollinearitySuggestion>,
) {
    let Some(ber) = ber else {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No BER loaded — create one from the Home page.",
            egui::FontId::default(),
            style::HINT_TEXT,
        );
        return;
    };

    match sketch_mode {
        SketchMode::Storey2D => {
            if let Some(storey) = ber.storeys.get(current_storey_idx) {
                storey_2d::render(painter, rect, storey, display_mode, highlight);
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        SketchMode::Storey3D => {
            if let Some(storey) = ber.storeys.get(current_storey_idx) {
                storey_3d::render(painter, rect, storey, display_mode);
            } else {
                empty_storey_hint(painter, rect);
            }
        }
        SketchMode::FullHouse => {
            full_house_3d::render(painter, rect, ber, display_mode);
        }
    }
}

fn empty_storey_hint(painter: &egui::Painter, rect: egui::Rect) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "No floor plan yet. Draw a shape on this canvas to begin.",
        egui::FontId::default(),
        style::HINT_TEXT,
    );
}

/// Shared color palette + opacity constants for all renderers.
pub(crate) mod style {
    use eframe::egui::Color32;

    pub const FLOOR_FILL: Color32 = Color32::from_rgba_premultiplied(40, 70, 110, 50);
    pub const FLOOR_LINE: Color32 = Color32::from_rgb(120, 170, 240);
    pub const WALL_LINE: Color32 = Color32::from_rgb(230, 230, 230);
    pub const ROOF_LINE: Color32 = Color32::from_rgb(220, 160, 80);
    pub const WINDOW_FILL: Color32 = Color32::from_rgba_premultiplied(130, 200, 230, 180);
    pub const DOOR_FILL: Color32 = Color32::from_rgba_premultiplied(180, 130, 80, 200);
    pub const DIMENSION_TEXT: Color32 = Color32::from_rgb(200, 200, 200);
    pub const HINT_TEXT: Color32 = Color32::from_rgb(140, 140, 140);
    pub const FADED_OPACITY: u8 = 50;

    pub fn faded(c: Color32) -> Color32 {
        let [r, g, b, _] = c.to_array();
        Color32::from_rgba_premultiplied(r, g, b, FADED_OPACITY)
    }
}

/// Linear 2D transform: source-space (meters/pixels) → screen-space (px).
/// Preserves aspect ratio; centres the source bounding box inside the dest rect.
pub(crate) struct Transform2d {
    scale: f64,
    cx: f64, // dest centre x (px)
    cy: f64, // dest centre y (px)
    sx: f64, // source centre x
    sy: f64, // source centre y
}

impl Transform2d {
    pub fn apply(&self, x: f64, y: f64) -> egui::Pos2 {
        egui::pos2(
            (self.cx + (x - self.sx) * self.scale) as f32,
            (self.cy + (y - self.sy) * self.scale) as f32,
        )
    }

    pub fn scale(&self) -> f64 {
        self.scale
    }
}

/// Compute a transform that fits a source bounding box inside `rect` with padding.
pub(crate) fn fit_transform(
    rect: egui::Rect,
    min_xy: (f64, f64),
    max_xy: (f64, f64),
) -> Transform2d {
    let pad = 30.0_f32;
    let target_w = (rect.width() - 2.0 * pad).max(1.0);
    let target_h = (rect.height() - 2.0 * pad).max(1.0);
    let src_w = (max_xy.0 - min_xy.0).max(1e-6);
    let src_h = (max_xy.1 - min_xy.1).max(1e-6);
    let scale = (target_w as f64 / src_w).min(target_h as f64 / src_h);

    Transform2d {
        scale,
        cx: rect.center().x as f64,
        cy: rect.center().y as f64,
        sx: (min_xy.0 + max_xy.0) * 0.5,
        sy: (min_xy.1 + max_xy.1) * 0.5,
    }
}

/// True isometric projection: rotate around vertical, tilt back, z is up.
/// World (x, y, z) → 2D plane (x', y'). Used by `storey_3d` and `full_house_3d`.
pub(crate) fn project_iso(x: f64, y: f64, z: f64) -> (f64, f64) {
    const COS30: f64 = 0.866_025_403_784_438_6;
    const SIN30: f64 = 0.5;
    let px = (x - y) * COS30;
    let py = (x + y) * SIN30 - z;
    (px, py)
}

/// Returns `base` if `target` should be highlighted, else a faded variant.
pub(crate) fn color_for(
    target: DisplayMode,
    current: DisplayMode,
    base: egui::Color32,
) -> egui::Color32 {
    match current {
        DisplayMode::AllSurfaces | DisplayMode::FloorMapDims => base,
        other if other == target => base,
        _ => style::faded(base),
    }
}
