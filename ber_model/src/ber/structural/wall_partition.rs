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
/// matching sample point falls back to [`base_exposure`].
///
/// [`base_exposure`]: WallFacePartition::base_exposure
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WallFacePartition {
    pub cuts: Vec<PartitionCut>,
    pub regions: Vec<(Point2f32, WallExposure)>, // (wall-local sample point, exposure)
    /// The wall's exposure WHEN IT WAS FIRST CUT, so every region starts out
    /// carrying it. A cut DIVIDES a wall, it does not un-answer it — but the
    /// first cut clears the wall's 1-D `wall_sections` (face partitioning
    /// supersedes along-wall sectioning), which used to lose the answer
    /// outright: partitioning a wall to give its halves different
    /// CONSTRUCTIONS silently dropped the exposure it already had.
    /// `#[serde(default)]` so older records load (`Unset`, the previous
    /// behaviour).
    #[serde(default = "unset_exposure")]
    pub base_exposure: WallExposure,
}

/// Durable, orientation-preserving identity of a perimeter wall: its base-edge
/// endpoints (start vertex → end vertex) quantized to the mm grid. The Layer-1
/// analogue of the editing `VertexId` key — a partition is reattached on load by
/// matching this back to a wall via `surface_ref_at`. Unlike `riser_key` it does
/// NOT sort the endpoints: the start must stay first so the partition's
/// wall-local `along_m` (measured from the start vertex) stays anchored.
impl Default for WallFacePartition {
    fn default() -> Self {
        Self {
            cuts: Vec::new(),
            regions: Vec::new(),
            base_exposure: unset_exposure(),
        }
    }
}

/// `WallExposure::Unset` — the serde/`Default` seed for
/// [`WallFacePartition::base_exposure`] (`WallExposure` has no `Default`).
fn unset_exposure() -> WallExposure {
    WallExposure::Unset
}

pub type WallFaceKey = (i64, i64, i64, i64);

/// Durable identity of ONE REGION of a partitioned wall face: the wall's
/// [`WallFaceKey`] plus a wall-local `(along_m, height_m)` sample on the mm grid.
///
/// A partition piece has no `SurfaceRef` of its own — `surface_ref_at` maps
/// EVERY piece of a wall to the same `SurfaceRef::wall(vid)` — so, exactly like
/// a riser's base edge, a geometric key is its durable home. Without one,
/// attributing a piece wrote the whole wall's entry and every piece read it
/// back: at 56 Elmwood Road, setting one half's construction (and its photos)
/// set both halves.
pub type FaceRegionKey = (WallFaceKey, i64, i64);

/// Quantize a wall-local `(along_m, height_m)` sample into a [`FaceRegionKey`]
/// on `face`.
pub fn face_region_key(face: WallFaceKey, sample: Point2f32) -> FaceRegionKey {
    let q = |v: f32| (v as f64 * 1000.0).round() as i64;
    (face, q(sample.x), q(sample.y))
}

/// Quantize a wall's base-edge endpoints `(start, end)` to a [`WallFaceKey`].
pub fn wall_face_key(start: Point2f32, end: Point2f32) -> WallFaceKey {
    let q = |v: f32| (v as f64 * 1000.0).round() as i64;
    (q(start.x), q(start.y), q(end.x), q(end.y))
}
