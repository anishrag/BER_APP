//! The canvas view transform (pan/zoom on top of auto-fit, plus the 3D orbit).
use super::*;

/// Default 3D orbit yaw (degrees). With the default pitch this reproduces the
/// historical fixed isometric projection (`px = (x − y)·cos30°,
/// `py = (x + y)·sin30° − z`) exactly, up to a uniform scale the auto-fit
/// cancels: yaw 45° rotates the plan so +x and +y recede symmetrically.
pub const DEFAULT_YAW_DEG: f32 = 45.0;

/// Default 3D orbit pitch (degrees above the horizon): `atan(1/√2)` ≈
/// 35.264°, the true-isometric elevation — the second half of matching the
/// historical fixed projection (see [`DEFAULT_YAW_DEG`]). 90° = top-down plan.
pub const DEFAULT_PITCH_DEG: f32 = 35.264_39;

/// Pitch clamp range (degrees): 5° keeps the view just above the horizon
/// (a 0° edge-on view degenerates the plan to a line); 90° is top-down.
const PITCH_RANGE_DEG: std::ops::RangeInclusive<f32> = 5.0..=90.0;

/// User-controlled view transform on the sketch canvas, on top of the
/// auto-fit. `zoom = 1.0` and zero offset = pure auto-fit. The assessor uses
/// this to zoom out (giving room to draw a disjoint section) or pan around.
/// `yaw_deg`/`pitch_deg` are the 3D orbit camera angles (drag-to-orbit in the
/// 3D view, Google-Earth style); they're ignored by the 2D views, and their
/// defaults reproduce the historical fixed isometric projection.
#[derive(Debug, Clone, Copy)]
pub struct ViewTransform {
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
    /// 3D orbit: rotation about the vertical axis, degrees (wraps mod 360).
    pub yaw_deg: f32,
    /// 3D orbit: camera elevation above the horizon, degrees (5..=90;
    /// 90 = top-down plan).
    pub pitch_deg: f32,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
            yaw_deg: DEFAULT_YAW_DEG,
            pitch_deg: DEFAULT_PITCH_DEG,
        }
    }
}

impl AppController {
    /// Current view transform (auto-fit + user pan/zoom).
    pub fn view(&self) -> ViewTransform {
        self.view
    }

    /// Multiply current zoom by `factor`, keeping `anchor` (canvas pixel pos)
    /// stationary. Clamped to a reasonable range.
    pub fn zoom_view(&mut self, factor: f32, anchor: (f32, f32), canvas_center: (f32, f32)) {
        let old = self.view.zoom;
        let new = (old * factor).clamp(0.1, 10.0);
        let actual_factor = new / old;
        // Anchor's pre-zoom position relative to center, scaled by the actual factor
        // we applied, then re-anchored. Offset shifts so the anchor stays put.
        let rel_x = anchor.0 - canvas_center.0 - self.view.offset_x;
        let rel_y = anchor.1 - canvas_center.1 - self.view.offset_y;
        self.view.offset_x += rel_x * (1.0 - actual_factor);
        self.view.offset_y += rel_y * (1.0 - actual_factor);
        self.view.zoom = new;
    }

    /// Translate the view by a canvas-pixel delta (positive = pan right/down).
    pub fn pan_view(&mut self, delta_x: f32, delta_y: f32) {
        self.view.offset_x += delta_x;
        self.view.offset_y += delta_y;
    }

    /// Orbit the 3D camera by a yaw/pitch delta (degrees). Yaw wraps mod
    /// 360; pitch is clamped to keep the view between just-above-horizon
    /// and top-down. Presentation-only, like pan/zoom — nothing is marked
    /// dirty. The 2D views ignore the angles.
    pub fn orbit_view(&mut self, dyaw_deg: f32, dpitch_deg: f32) {
        self.view.yaw_deg = (self.view.yaw_deg + dyaw_deg).rem_euclid(360.0);
        self.view.pitch_deg = (self.view.pitch_deg + dpitch_deg)
            .clamp(*PITCH_RANGE_DEG.start(), *PITCH_RANGE_DEG.end());
    }

    /// Reset the view to pure auto-fit (and the orbit to the default
    /// isometric angles).
    pub fn reset_view(&mut self) {
        self.view = ViewTransform::default();
    }
}
