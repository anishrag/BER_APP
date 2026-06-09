// points.rs
pub type Point3f32 = nalgebra::Point3<f32>;
pub type Vector3f32 = nalgebra::Vector3<f32>;

pub type Point3f64 = nalgebra::Point3<f64>;
pub type Vector3f64 = nalgebra::Vector3<f64>;

pub type Point2f64 = nalgebra::Point2<f64>;
pub type Vector2f64 = nalgebra::Vector2<f64>;

pub type Point2f32 = nalgebra::Point2<f32>;
pub type Vector2f32 = nalgebra::Vector2<f32>;

/// Widen a 2D point to `f64` — the `f32` editing space (`FloorPlan`) → the `f64`
/// BER/datatypes space. (`Point2*` are nalgebra aliases, so this is a free fn.)
pub fn to_f64(p: Point2f32) -> Point2f64 {
    Point2f64::new(p.x as f64, p.y as f64)
}

/// Narrow a 2D point to `f32` — the `f64` BER/datatypes space → the `f32` editing
/// space. Inverse of [`to_f64`].
pub fn to_f32(p: Point2f64) -> Point2f32 {
    Point2f32::new(p.x as f32, p.y as f32)
}

/// Distance from point `(px, py)` to the segment `(ax, ay)→(bx, by)`. Returns
/// the nearest-endpoint distance when the point projects outside the segment
/// (so it doubles as an on-segment test: `... < tol` is true only for points
/// within `tol` of the segment AND inside its span). Shared 2D vector math.
pub fn point_to_segment_distance(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let abx = bx - ax;
    let aby = by - ay;
    let apx = px - ax;
    let apy = py - ay;
    let ab_len_sq = abx * abx + aby * aby;
    if ab_len_sq < 1e-12 {
        return (apx * apx + apy * apy).sqrt();
    }
    let t = ((apx * abx + apy * aby) / ab_len_sq).clamp(0.0, 1.0);
    let proj_x = ax + t * abx;
    let proj_y = ay + t * aby;
    let dx = px - proj_x;
    let dy = py - proj_y;
    (dx * dx + dy * dy).sqrt()
}
