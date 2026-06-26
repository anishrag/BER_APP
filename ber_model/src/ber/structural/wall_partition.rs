//! Wall-face partition recipe — a durable Layer-1 input.
//!
//! A face partition divides an outline wall's internal elevation (a flat
//! wall-local rectangle, x = along_m / y = height_m) into regions by drawn cut
//! lines, each carrying its own [`WallExposure`]. `apply_face_partitions`
//! (in the `document` resolve pass) consumes this to SPLIT the wall into
//! per-region [`WallRegion`](super::WallRegion)s — but the cut lines and the
//! per-region sample points are NOT recoverable from the split result, so the
//! recipe must be stored durably here rather than reconstructed.
//!
//! The editing layer keeps a `VertexId`-keyed working copy; this Layer-1 store
//! keys instead by the wall's base-edge geometry ([`WallFaceKey`]) so it stays
//! free of the parametric `manip_plan::VertexId`. The two are reconciled on
//! load by matching the geometry back to a wall (`surface_ref_at`).

use super::WallExposure;
use domain::points::Point2f32;
use serde::{Deserialize, Serialize};

/// One straight partition cut on a wall's internal elevation, in WALL-LOCAL
/// coords: x = along_m (0..wall_len from the wall's start vertex), y = height_m
/// (0 = floor).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PartitionCut {
    pub a: Point2f32,
    pub b: Point2f32,
}

/// A partitioned wall face: ordered `cuts` (each a full cross-cut of the face)
/// plus per-region exposure attribution by a CONTAINED sample point (durable
/// across re-draws, like the floor/ceiling centroid keys). A region with no
/// matching sample point resolves `Unset`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct WallFacePartition {
    pub cuts: Vec<PartitionCut>,
    pub regions: Vec<(Point2f32, WallExposure)>, // (wall-local sample point, exposure)
}

/// Durable, orientation-preserving identity of a perimeter wall: its base-edge
/// endpoints (start vertex → end vertex) quantized to the mm grid. The Layer-1
/// analogue of the editing `VertexId` key — a partition is reattached on load by
/// matching this back to a wall via `surface_ref_at`. Unlike `riser_key` it does
/// NOT sort the endpoints: the start must stay first so the partition's
/// wall-local `along_m` (measured from the start vertex) stays anchored.
pub type WallFaceKey = (i64, i64, i64, i64);

/// Quantize a wall's base-edge endpoints `(start, end)` to a [`WallFaceKey`].
pub fn wall_face_key(start: Point2f32, end: Point2f32) -> WallFaceKey {
    let q = |v: f32| (v as f64 * 1000.0).round() as i64;
    (q(start.x), q(start.y), q(end.x), q(end.y))
}
