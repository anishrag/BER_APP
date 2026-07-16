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
