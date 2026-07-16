//! The canvas view transform (pan/zoom on top of auto-fit, plus the 3D orbit).
use super::*;

pub use domain::view::{DEFAULT_PITCH_DEG, DEFAULT_YAW_DEG};

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

    /// Record the diagonal (metres) of the currently-VISIBLE world rectangle —
    /// the sketch UI computes it from the live pan/zoom transform and pushes it
    /// each frame. A plain `f32`, so no egui type crosses into the controller;
    /// the snap-tolerance helpers ([`snap_view_diag`](Self::snap_view_diag))
    /// read it to size viewport-relative snap radii.
    pub fn set_view_world_diagonal_m(&mut self, d: f32) {
        self.view_world_diagonal_m = Some(d);
    }

    /// The world diagonal (metres) that sizes viewport-relative snap tolerances:
    /// the value the UI last pushed ([`set_view_world_diagonal_m`](Self::set_view_world_diagonal_m)),
    /// else a plan-bbox diagonal — so headless/standalone runs, tests, and the
    /// pre-first-frame case are always defined. The fallback tries, in order:
    /// the active storey outline; the live editing buffer; the PREVIOUS storey's
    /// ring (so a fresh upper storey being traced — whose own outline is still
    /// empty — is sized by the footprint it's drawn over, the scale the real
    /// app's viewport would show); then a 1 m floor (the clamp still yields the
    /// per-tool `min_m` on a truly empty session).
    fn snap_view_diag(&self) -> f32 {
        if let Some(d) = self
            .view_world_diagonal_m
            .filter(|d| d.is_finite() && *d > 0.0)
        {
            return d;
        }
        let outline = self.document.outline.floor_plan.bounding_box_diagonal();
        if outline > 0.0 {
            return outline;
        }
        let buffer = self.document.floor_plan.bounding_box_diagonal();
        if buffer > 0.0 {
            return buffer;
        }
        if let Some(prev) = self.previous_storey_ring() {
            let d = manip_plan::ring_diagonal(&prev);
            if d > 0.0 {
                return d;
            }
        }
        1.0
    }

    /// Dormer edge-snap tolerance (metres), viewport-relative.
    pub(crate) fn snap_tol_dormer(&self) -> f32 {
        self.tuning.snap.dormer.tol(self.snap_view_diag())
    }

    /// Ceiling-exposure carve pre-snap tolerance (metres), viewport-relative.
    pub(crate) fn snap_tol_ceiling_exposure(&self) -> f32 {
        self.tuning.snap.ceiling_exposure.tol(self.snap_view_diag())
    }

    /// Ridge + ceiling-step LINE snap tolerance (metres), viewport-relative.
    pub(crate) fn snap_tol_ceiling_line(&self) -> f32 {
        self.tuning.snap.ceiling_line.tol(self.snap_view_diag())
    }

    /// Phase / floor-step / living-area carve wall-leveling tolerance (metres),
    /// viewport-relative.
    pub(crate) fn snap_tol_carve(&self) -> f32 {
        self.tuning.snap.carve.tol(self.snap_view_diag())
    }

    /// Extend + new-storey stroke snap tolerance (metres), viewport-relative.
    pub(crate) fn snap_tol_extend(&self) -> f32 {
        self.tuning.snap.extend.tol(self.snap_view_diag())
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
