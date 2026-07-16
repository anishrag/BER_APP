//! Shared 3D-view constants: the default orbit angles used by both the
//! controller's view state and the render crates' `ViewParams`. They live in
//! `domain` (not `ber_render` / `src/ui`) so the UI-agnostic controller and
//! the egui-based renderers agree on one definition without the controller
//! importing a UI crate.

/// Default 3D orbit yaw (degrees). With the default pitch this reproduces the
/// historical fixed isometric projection (`px = (x − y)·cos30°`,
/// `py = (x + y)·sin30° − z`) exactly, up to a uniform scale the auto-fit
/// cancels: yaw 45° rotates the plan so +x and +y recede symmetrically.
pub const DEFAULT_YAW_DEG: f32 = 45.0;

/// Default 3D orbit pitch (degrees above the horizon): `atan(1/√2)` ≈
/// 35.264° — the true-isometric elevation matching the
/// historical fixed projection (see [`DEFAULT_YAW_DEG`]). 90° = top-down plan.
pub const DEFAULT_PITCH_DEG: f32 = 35.264_39;

/// A committed construction-phase zone projected for DISPLAY (tint + label). It
/// is HOLES-AWARE: a zone shrunk to a donut by a newer OVERLAPPING phase (the
/// latest-wins partition) keeps its `holes`, so the tint strokes the void's edge
/// without filling it, and `label_pos` is a point guaranteed inside the material
/// (outside every hole). All world-metre. Reused for the simple, hole-free
/// step / structure-split / pending overlays via [`Self::from_ring`].
///
/// Lives in `domain` because both the controller (which builds them) and the
/// `ber_render` drawing crate (which paints them) need the type.
#[derive(Debug, Clone)]
pub struct PhaseDisplayRegion {
    pub exterior: Vec<(f64, f64)>,
    pub holes: Vec<Vec<(f64, f64)>>,
    pub label_pos: (f64, f64),
    pub label: String,
}

impl PhaseDisplayRegion {
    /// A simple (hole-free) display region from one ring + label, the label
    /// anchored at the ring's vertex-mean. Used by the step / structure-split /
    /// pending overlays, which never carry a hole. Under the affine world→screen
    /// transform the world vertex-mean maps to the screen vertex-mean, so the
    /// label lands exactly where the old per-screen centroid placed it.
    pub fn from_ring(exterior: Vec<(f64, f64)>, label: String) -> Self {
        let n = exterior.len().max(1) as f64;
        let (sx, sy) = exterior
            .iter()
            .fold((0.0, 0.0), |(ax, ay), (x, y)| (ax + x, ay + y));
        Self {
            label_pos: (sx / n, sy / n),
            exterior,
            holes: Vec::new(),
            label,
        }
    }
}
