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

/// Whether the point `(x, y)` lies within `tol` of the ring's boundary (any
/// edge), the ring taken as implicitly closed. Used to match a boundary surface
/// (a wall, which collapses to its base edge in plan) to a carved region. A ring
/// of fewer than two vertices has no boundary. Shared 2D vector math.
pub fn point_on_ring_boundary(ring: &[Point2f64], x: f64, y: f64, tol: f64) -> bool {
    let n = ring.len();
    if n < 2 {
        return false;
    }
    (0..n).any(|i| {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        point_to_segment_distance(x, y, a.x, a.y, b.x, b.y) <= tol
    })
}

/// If segment `a→b` crosses segment `p→q` at a single point, the parameter
/// `t ∈ [0, 1]` along `a→b` of that crossing (the point is `a + t·(b−a)`).
/// `None` when the segments are parallel/degenerate or don't meet within both
/// spans. Shared 2D vector math (used to find where a cut line crosses a wall's
/// base edge).
pub fn segment_cross_param(a: Point2f64, b: Point2f64, p: Point2f64, q: Point2f64) -> Option<f64> {
    let (rx, ry) = (b.x - a.x, b.y - a.y);
    let (sx, sy) = (q.x - p.x, q.y - p.y);
    let denom = rx * sy - ry * sx;
    if denom.abs() < 1e-12 {
        return None; // parallel or a degenerate segment
    }
    let (qpx, qpy) = (p.x - a.x, p.y - a.y);
    let t = (qpx * sy - qpy * sx) / denom;
    let u = (qpx * ry - qpy * rx) / denom;
    ((0.0..=1.0).contains(&t) && (0.0..=1.0).contains(&u)).then_some(t)
}

/// The average of a ring's vertices (a closing-duplicate vertex is dropped) — a
/// cheap interior-ish centre for region-membership tests (good enough for the
/// simple region footprints carve selection deals with; a wall's degenerate ring
/// collapses to its base-edge midpoint). The origin for an empty ring.
pub fn ring_centroid(ring: &[Point2f64]) -> Point2f64 {
    let mut pts = ring;
    if pts.len() >= 2 && pts[0] == pts[pts.len() - 1] {
        pts = &pts[..pts.len() - 1];
    }
    if pts.is_empty() {
        return Point2f64::new(0.0, 0.0);
    }
    let n = pts.len() as f64;
    let sx: f64 = pts.iter().map(|p| p.x).sum();
    let sy: f64 = pts.iter().map(|p| p.y).sum();
    Point2f64::new(sx / n, sy / n)
}
