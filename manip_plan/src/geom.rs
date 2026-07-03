//! Small pure-geometry helpers for the dimensioning solver and the
//! direction-flip guard. No floor-plan state — just coordinate math over
//! `(f32, f32)` points, plus edge-sign comparison.

use crate::ParameterAxis;
use domain::points::{Point2f32, Point2f64};

/// Fit a straight line to a (hand-drawn, possibly wavy) point set by total
/// least squares: returns the centroid and a unit direction vector along the
/// line's principal axis (the direction of greatest spread). Used by Trim to
/// turn a freehand cut stroke into a clean straight cut line.
pub(crate) fn fit_line(points: &[Point2f32]) -> (Point2f32, (f32, f32)) {
    let n = points.len().max(1) as f32;
    let cx = points.iter().map(|p| p.x).sum::<f32>() / n;
    let cy = points.iter().map(|p| p.y).sum::<f32>() / n;
    let (mut sxx, mut sxy, mut syy) = (0.0_f32, 0.0_f32, 0.0_f32);
    for p in points {
        let dx = p.x - cx;
        let dy = p.y - cy;
        sxx += dx * dx;
        sxy += dx * dy;
        syy += dy * dy;
    }
    // Principal eigenvector direction of the 2x2 covariance matrix.
    let theta = 0.5 * (2.0 * sxy).atan2(sxx - syy);
    (Point2f32::new(cx, cy), (theta.cos(), theta.sin()))
}

/// A step-line stroke must span at least this many METRES along its fitted
/// direction (strokes arrive in world/metre space; anything shorter is a tap
/// or a stray mark, not a line).
const AXIS_LINE_MIN_SPAN_M: f32 = 0.05;

/// A stroke is "line-like" only when no point deviates perpendicular from the
/// fitted line by more than this fraction of the stroke's span — a closed loop
/// (~50%) or an L (~30%) blows well past it; freehand waviness stays under.
const AXIS_LINE_MAX_DEV_RATIO: f32 = 0.15;

/// Fit a FREE-DIRECTION segment to a near-straight stroke: the TLS line
/// ([`fit_line`]) with endpoints at the stroke's extreme projections onto it,
/// in draw order (first endpoint nearest the stroke's start). Same
/// line-likeness guards as [`fit_axis_segment`], but the orientation is kept
/// AS DRAWN — for lines that are legitimately diagonal (a hip roof's corner
/// ridges), where the axis snap would be wrong. `None` when the stroke is too
/// short or not line-like.
pub fn fit_segment(points: &[Point2f32]) -> Option<(Point2f32, Point2f32)> {
    if points.len() < 2 {
        return None;
    }
    let (centroid, (dx, dy)) = fit_line(points);
    let (mut min_t, mut max_t) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut max_dev = 0.0_f32;
    for p in points {
        let rx = p.x - centroid.x;
        let ry = p.y - centroid.y;
        let t = rx * dx + ry * dy;
        min_t = min_t.min(t);
        max_t = max_t.max(t);
        max_dev = max_dev.max((rx * dy - ry * dx).abs());
    }
    let span = max_t - min_t;
    if span < AXIS_LINE_MIN_SPAN_M || max_dev > span * AXIS_LINE_MAX_DEV_RATIO {
        return None;
    }
    let at = |t: f32| Point2f32::new(centroid.x + t * dx, centroid.y + t * dy);
    // Draw order: the stroke's first point projects nearer one extreme.
    let t0 = (points[0].x - centroid.x) * dx + (points[0].y - centroid.y) * dy;
    if (t0 - min_t).abs() <= (t0 - max_t).abs() {
        Some((at(min_t), at(max_t)))
    } else {
        Some((at(max_t), at(min_t)))
    }
}

/// Fit an AXIS-ALIGNED segment to a near-straight stroke: TLS fit
/// ([`fit_line`]), orientation snapped to the DOMINANT axis (the step/ridge
/// line is orthogonal by construction). Returns `(axis, coord, (lo, hi))`:
///
/// - `axis` is the axis the line's POSITION varies on (X for a vertical
///   line — the position-prompt convention) and `coord` the centroid's
///   coordinate on it;
/// - `(lo, hi)` is the stroke's projected EXTENT along the fitted line —
///   which runs along the OTHER axis from `axis`. For a vertical line
///   (`axis` = X, `coord` = the line's x) the extent is the stroke's
///   y-range; for a horizontal line (`axis` = Y) it is the x-range.
///   `lo <= hi` always, regardless of the stroke's draw direction. A ridge
///   needs endpoints, not an infinite line — the extent is taken AS DRAWN
///   (min/max over the stroke's points on the line's axis), with no snapping.
///
/// `None` when the stroke is too short (< 2 points, or span under
/// [`AXIS_LINE_MIN_SPAN_M`]) or not line-like (max perpendicular deviation
/// over [`AXIS_LINE_MAX_DEV_RATIO`] of its span — a loop or an L is not a
/// line). A ~45° stroke snaps to whichever axis dominates; there is no
/// diagonal rejection.
pub fn fit_axis_segment(points: &[Point2f32]) -> Option<(ParameterAxis, f32, (f32, f32))> {
    if points.len() < 2 {
        return None;
    }
    let (centroid, (dx, dy)) = fit_line(points);
    // Span = extent of the projections onto the fitted direction; deviation =
    // largest perpendicular offset from the fitted line.
    let (mut min_t, mut max_t) = (f32::INFINITY, f32::NEG_INFINITY);
    let mut max_dev = 0.0_f32;
    for p in points {
        let rx = p.x - centroid.x;
        let ry = p.y - centroid.y;
        let t = rx * dx + ry * dy;
        min_t = min_t.min(t);
        max_t = max_t.max(t);
        max_dev = max_dev.max((rx * dy - ry * dx).abs());
    }
    let span = max_t - min_t;
    if span < AXIS_LINE_MIN_SPAN_M {
        return None;
    }
    if max_dev > span * AXIS_LINE_MAX_DEV_RATIO {
        return None;
    }
    // Snap to the dominant axis of the fitted direction. A mostly-vertical
    // stroke (|dy| > |dx|) is a vertical line, whose position varies on X —
    // and whose extent runs along Y (the other axis).
    let extent = |get: fn(&Point2f32) -> f32| {
        points
            .iter()
            .map(get)
            .fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v), hi.max(v))
            })
    };
    if dy.abs() > dx.abs() {
        Some((ParameterAxis::X, centroid.x, extent(|p| p.y)))
    } else {
        Some((ParameterAxis::Y, centroid.y, extent(|p| p.x)))
    }
}

/// [`fit_axis_segment`] without the extent — the infinite-line view a
/// ceiling-step stroke needs (position only; the line spans the footprint).
pub fn fit_axis_line(points: &[Point2f32]) -> Option<(ParameterAxis, f32)> {
    fit_axis_segment(points).map(|(axis, coord, _)| (axis, coord))
}

/// A stroke is "rectangle-like" only when no point sits further from the
/// stroke bbox's BOUNDARY than this fraction of the bbox's SHORTER side —
/// the rect analogue of [`AXIS_LINE_MAX_DEV_RATIO`]: a diagonal stroke's
/// middle sits half the short side from every edge (ratio 0.5), a filled
/// blob worse; freehand waviness along the sides stays under.
const AXIS_RECT_MAX_DEV_RATIO: f32 = 0.25;

/// Fit a CLOSED stroke to an AXIS-ALIGNED rectangle — the rect analogue of
/// [`fit_axis_segment`]'s straightness heuristic. The fitted rectangle is
/// the stroke's bounding box, accepted when every point sits near the bbox
/// boundary (within [`AXIS_RECT_MAX_DEV_RATIO`] of the shorter side — a
/// diagonal stroke or a filled blob has points deep inside and is
/// rejected). Both sides must span at least [`AXIS_LINE_MIN_SPAN_M`] (a
/// straight line is not a rectangle). Returns `((min_x, min_y), (max_x,
/// max_y))`. CLOSURE IS THE CALLER'S TEST — an open U-shaped stroke passes
/// the near-boundary heuristic; the caller routes only closed strokes here.
pub fn fit_axis_rect(points: &[Point2f32]) -> Option<((f32, f32), (f32, f32))> {
    if points.len() < 4 {
        return None;
    }
    let (mut min_x, mut min_y) = (f32::INFINITY, f32::INFINITY);
    let (mut max_x, mut max_y) = (f32::NEG_INFINITY, f32::NEG_INFINITY);
    for p in points {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    let (w, h) = (max_x - min_x, max_y - min_y);
    if w < AXIS_LINE_MIN_SPAN_M || h < AXIS_LINE_MIN_SPAN_M {
        return None; // a line (or a tap), not a rectangle
    }
    let tol = w.min(h) * AXIS_RECT_MAX_DEV_RATIO;
    let near_boundary = |p: &Point2f32| {
        let dx = (p.x - min_x).min(max_x - p.x);
        let dy = (p.y - min_y).min(max_y - p.y);
        dx.min(dy) <= tol
    };
    points
        .iter()
        .all(near_boundary)
        .then_some(((min_x, min_y), (max_x, max_y)))
}

/// Project `cur` onto a circle of radius `r` around `n`.
pub fn project_to_circle(cur: (f32, f32), n: (f32, f32), r: f32) -> (f32, f32) {
    let dx = cur.0 - n.0;
    let dy = cur.1 - n.1;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        (n.0 + r, n.1)
    } else {
        let scale = r / len;
        (n.0 + dx * scale, n.1 + dy * scale)
    }
}

/// Solve V given one axis constraint + one circle constraint.
/// `axis_sign * axis_dist` pins V's coord on the named axis (from
/// `axis_neighbor`), then we solve the circle (centred at `circle_n`,
/// radius `circle_r`) for the other coord. Two solutions in general;
/// pick the one matching the current position's side.
pub fn solve_axis_wall(
    axis: ParameterAxis,
    axis_sign: f32,
    axis_dist: f32,
    axis_neighbor: (f32, f32),
    circle_n: (f32, f32),
    circle_r: f32,
    cur: (f32, f32),
) -> (f32, f32) {
    match axis {
        ParameterAxis::X => {
            let new_x = axis_neighbor.0 + axis_sign * axis_dist;
            let dx_to_n = new_x - circle_n.0;
            let dy_sq = circle_r * circle_r - dx_to_n * dx_to_n;
            if dy_sq < 0.0 {
                // Circle doesn't reach this X line — fall back to projection.
                project_to_circle(cur, circle_n, circle_r)
            } else {
                let dy_abs = dy_sq.sqrt();
                let sign_y = (cur.1 - circle_n.1).signum();
                let sign_y = if sign_y == 0.0 { 1.0 } else { sign_y };
                (new_x, circle_n.1 + sign_y * dy_abs)
            }
        }
        ParameterAxis::Y => {
            let new_y = axis_neighbor.1 + axis_sign * axis_dist;
            let dy_to_n = new_y - circle_n.1;
            let dx_sq = circle_r * circle_r - dy_to_n * dy_to_n;
            if dx_sq < 0.0 {
                project_to_circle(cur, circle_n, circle_r)
            } else {
                let dx_abs = dx_sq.sqrt();
                let sign_x = (cur.0 - circle_n.0).signum();
                let sign_x = if sign_x == 0.0 { 1.0 } else { sign_x };
                (circle_n.0 + sign_x * dx_abs, new_y)
            }
        }
    }
}

/// Intersect two circles (centres `n1`, `n2`; radii `r1`, `r2`). Returns
/// the intersection point closest to `cur`. When circles don't intersect,
/// falls back to `cur`.
pub fn intersect_circles(
    n1: (f32, f32),
    r1: f32,
    n2: (f32, f32),
    r2: f32,
    cur: (f32, f32),
) -> (f32, f32) {
    let dx = n2.0 - n1.0;
    let dy = n2.1 - n1.1;
    let d = (dx * dx + dy * dy).sqrt();
    if d < 1e-6 || d > r1 + r2 + 1e-3 || d < (r1 - r2).abs() - 1e-3 {
        return cur;
    }
    let a = (r1 * r1 - r2 * r2 + d * d) / (2.0 * d);
    let h_sq = (r1 * r1 - a * a).max(0.0);
    let h = h_sq.sqrt();
    let mid_x = n1.0 + a * dx / d;
    let mid_y = n1.1 + a * dy / d;
    let perp_x = -dy / d * h;
    let perp_y = dx / d * h;
    let p1 = (mid_x + perp_x, mid_y + perp_y);
    let p2 = (mid_x - perp_x, mid_y - perp_y);
    let d1_sq = (p1.0 - cur.0).powi(2) + (p1.1 - cur.1).powi(2);
    let d2_sq = (p2.0 - cur.0).powi(2) + (p2.1 - cur.1).powi(2);
    if d1_sq < d2_sq {
        p1
    } else {
        p2
    }
}

/// Returns the `(ring_idx, edge_idx)` of the first edge whose direction sign
/// flipped between `before` and `after` (positive → negative or vice versa
/// along either axis), per [`FloorPlan::edge_signs`](crate::FloorPlan::edge_signs).
/// Zero signs are ignored — they represent edges already degenerate in one
/// direction, so a sign change there isn't a "flip"; genuinely-collapsed edges
/// are caught separately by `validate_simple`. An inversion of an un-set wall
/// is a signal to *repair* (redistribute the un-set walls), not necessarily to
/// reject — see `AppController::apply_dimension`.
pub fn find_flipped_edge(
    before: &[Vec<(i8, i8)>],
    after: &[Vec<(i8, i8)>],
) -> Option<(usize, usize)> {
    for (ring_idx, (b_ring, a_ring)) in before.iter().zip(after.iter()).enumerate() {
        if b_ring.len() != a_ring.len() {
            return Some((ring_idx, 0));
        }
        for (edge_idx, (b_sign, a_sign)) in b_ring.iter().zip(a_ring.iter()).enumerate() {
            if b_sign.0 != 0 && a_sign.0 != 0 && b_sign.0 != a_sign.0 {
                return Some((ring_idx, edge_idx));
            }
            if b_sign.1 != 0 && a_sign.1 != 0 && b_sign.1 != a_sign.1 {
                return Some((ring_idx, edge_idx));
            }
        }
    }
    None
}

/// Signed "sagitta" (bulge height) of a hand-drawn arc stroke: the largest
/// perpendicular deviation of any interior point from the straight chord
/// joining the first and last point. Sign is positive when the bulge is to the
/// LEFT of the chord direction (first → last), negative to the right — so it
/// records which way the curve bows. Used to capture a freehand arc as a clean
/// circular segment (chord endpoints + this one bulge value) instead of the
/// noisy raw polyline. Returns 0 for a degenerate (zero-length) chord.
pub(crate) fn stroke_sagitta(stroke: &[Point2f32]) -> f32 {
    if stroke.len() < 3 {
        return 0.0;
    }
    let p1 = stroke[0];
    let p2 = stroke[stroke.len() - 1];
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-6 {
        return 0.0;
    }
    // Left normal of the chord direction (unit).
    let nx = -dy / len;
    let ny = dx / len;
    let mut best = 0.0_f32;
    for p in &stroke[1..stroke.len() - 1] {
        let perp = (p.x - p1.x) * nx + (p.y - p1.y) * ny;
        if perp.abs() > best.abs() {
            best = perp;
        }
    }
    best
}

/// Facet a circular arc joining `p1`→`p2` that bulges by `sagitta` (signed, as
/// returned by [`stroke_sagitta`] — positive bulges to the left of p1→p2). The
/// arc is the unique circular segment through both endpoints with that bulge
/// height; it's sampled into a polyline at roughly `step_deg`-degree increments
/// (clamped to a sane count), endpoints pinned exactly to `p1`/`p2`. A
/// near-zero sagitta or zero-length chord degenerates to `[p1, p2]` (a straight
/// segment). This is how a provisional arc becomes model points once its attach
/// dimensions are set.
pub(crate) fn circular_arc_points(
    p1: Point2f32,
    p2: Point2f32,
    sagitta: f32,
    step_deg: f32,
) -> Vec<Point2f32> {
    let dx = p2.x - p1.x;
    let dy = p2.y - p1.y;
    let chord = (dx * dx + dy * dy).sqrt();
    if chord < 1e-6 || sagitta.abs() < 1e-4 {
        return vec![p1, p2];
    }
    // Chord-local frame: origin at the chord midpoint, u along the chord, n the
    // left normal. p1=(-a,0), p2=(a,0), apex=(0, sagitta).
    let ux = dx / chord;
    let uy = dy / chord;
    let nx = -uy;
    let ny = ux;
    let mx = (p1.x + p2.x) * 0.5;
    let my = (p1.y + p2.y) * 0.5;
    let a = chord * 0.5;
    let s = sagitta;
    // Circle centre lies on the normal axis at local y = k (through the 3 pts).
    let k = (s * s - a * a) / (2.0 * s);
    let radius = (a * a + k * k).sqrt();

    // The three points in arc order are p1, apex, p2 — monotonic in angle about
    // the centre, so sweep θ1 → apex → θ2 by accumulating wrapped deltas.
    let ang = |lx: f32, ly: f32| (ly - k).atan2(lx);
    let theta1 = ang(-a, 0.0);
    let theta_apex = ang(0.0, s);
    let theta2 = ang(a, 0.0);
    let wrap = |d: f32| {
        let mut d = d;
        while d > std::f32::consts::PI {
            d -= 2.0 * std::f32::consts::PI;
        }
        while d < -std::f32::consts::PI {
            d += 2.0 * std::f32::consts::PI;
        }
        d
    };
    let sweep = wrap(theta_apex - theta1) + wrap(theta2 - theta_apex);

    let step = step_deg.max(1.0).to_radians();
    let n = ((sweep.abs() / step).ceil() as usize).clamp(2, 64);
    let mut pts = Vec::with_capacity(n + 1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let theta = theta1 + sweep * t;
        let lx = radius * theta.cos();
        let ly = k + radius * theta.sin();
        pts.push(Point2f32::new(
            mx + lx * ux + ly * nx,
            my + lx * uy + ly * ny,
        ));
    }
    // Pin endpoints exactly (kill rounding so they sit on their attach edges).
    pts[0] = p1;
    let last = pts.len() - 1;
    pts[last] = p2;
    pts
}

/// Axis-aligned bounding box of a point set, as `((min_x,min_y),(max_x,max_y))`.
/// `None` for an empty slice. The single source for every bbox computation over
/// `Point2f32` in this crate.
pub(crate) fn bbox(points: &[Point2f32]) -> Option<((f32, f32), (f32, f32))> {
    let mut it = points.iter();
    let first = it.next()?;
    let (mut min_x, mut min_y) = (first.x, first.y);
    let (mut max_x, mut max_y) = (first.x, first.y);
    for p in it {
        min_x = min_x.min(p.x);
        min_y = min_y.min(p.y);
        max_x = max_x.max(p.x);
        max_y = max_y.max(p.y);
    }
    Some(((min_x, min_y), (max_x, max_y)))
}

/// Diagonal length of a point set's bounding box. `0.0` for fewer than two
/// points. Used throughout to scale tolerances to a shape's overall size.
pub fn bbox_diagonal(points: &[Point2f32]) -> f32 {
    match bbox(points) {
        Some(((mnx, mny), (mxx, mxy))) => ((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt(),
        None => 0.0,
    }
}

/// The interior shared edge (world coords) between a sub-rectangle `region`
/// (an axis-aligned partition piece) and the rest of a footprint whose bounding
/// box is `outline_bbox` = `((ox0,oy0),(ox1,oy1))` — the faint-divider cut for a
/// same-height ceiling split. Returns the region's bbox edge on the SPLIT axis
/// that lies INTERIOR to the outline (not flush with the outline bbox):
/// a vertical edge when the region is narrower than the outline in X, a
/// horizontal edge when narrower in Y, else `None` (degenerate — the region
/// fills the footprint on both axes, or has no points). Pure f64 coordinate
/// math; no model state.
pub fn interior_split_edge(
    region: &[(f64, f64)],
    outline_bbox: ((f64, f64), (f64, f64)),
) -> Option<((f64, f64), (f64, f64))> {
    let ((ox0, oy0), (ox1, oy1)) = outline_bbox;
    let mut it = region.iter();
    let &(fx, fy) = it.next()?;
    let (mut rx0, mut ry0, mut rx1, mut ry1) = (fx, fy, fx, fy);
    for &(x, y) in it {
        rx0 = rx0.min(x);
        ry0 = ry0.min(y);
        rx1 = rx1.max(x);
        ry1 = ry1.max(y);
    }
    const TOL: f64 = 1e-6;
    if (rx1 - rx0) + TOL < (ox1 - ox0) {
        // Vertical cut — the full-height edge at rx0 or rx1 not flush with the
        // outline's X extent.
        let cut_x = if (rx0 - ox0).abs() <= TOL { rx1 } else { rx0 };
        Some(((cut_x, ry0), (cut_x, ry1)))
    } else if (ry1 - ry0) + TOL < (oy1 - oy0) {
        // Horizontal cut — the full-width edge at ry0 or ry1.
        let cut_y = if (ry0 - oy0).abs() <= TOL { ry1 } else { ry0 };
        Some(((rx0, cut_y), (rx1, cut_y)))
    } else {
        None
    }
}

/// Nearest point on segment `a→b` to `p`. Returns the clamped on-segment point,
/// the UNCLAMPED parameter `t` (so callers can tell whether the foot fell beyond
/// an endpoint), and the distance from `p` to that on-segment point. A
/// degenerate (zero-length) segment returns `a` at `t = 0`.
pub(crate) fn project_point_to_segment(
    p: Point2f32,
    a: Point2f32,
    b: Point2f32,
) -> (Point2f32, f32, f32) {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let l2 = dx * dx + dy * dy;
    if l2 < 1e-9 {
        let d = ((p.x - a.x).powi(2) + (p.y - a.y).powi(2)).sqrt();
        return (a, 0.0, d);
    }
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / l2;
    let tc = t.clamp(0.0, 1.0);
    let q = Point2f32::new(a.x + tc * dx, a.y + tc * dy);
    let d = ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
    (q, t, d)
}

/// Euclidean length of segment `a→b`.
pub fn segment_len(a: Point2f32, b: Point2f32) -> f32 {
    ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
}

/// Point at distance `d` along the segment `a→b` of length `len`. A degenerate
/// (zero-length) segment returns `a`.
pub fn lerp_along(a: Point2f32, b: Point2f32, len: f32, d: f32) -> (f32, f32) {
    let t = if len > 1e-6 { d / len } else { 0.0 };
    (a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
}

/// Distance from `a` of the projection of `(px, py)` onto the segment `a→b` of
/// length `len`, clamped to `[0, len]`. A degenerate segment returns `0.0`.
pub fn project_distance_along(a: Point2f32, b: Point2f32, len: f32, px: f32, py: f32) -> f32 {
    if len <= 1e-6 {
        return 0.0;
    }
    let dot = (px - a.x) * (b.x - a.x) + (py - a.y) * (b.y - a.y);
    (dot / len).clamp(0.0, len)
}

/// Along-wall distances (metres from `a`) where reference segments CROSS or
/// TOUCH the wall segment `a→b` of length `len` — the snap candidates for a
/// point sliding along the wall (e.g. an exposure section handle). A proper
/// crossing contributes its intersection point; an endpoint graze within
/// `eps` metres counts as a touch; a COLLINEAR reference contributes its two
/// endpoints' projections (the overlap's extent on the wall). Results are
/// clamped to `[0, len]`, deduped within `eps`, and sorted ascending. A
/// degenerate wall yields nothing.
pub fn crossing_distances_along(
    a: Point2f32,
    b: Point2f32,
    len: f32,
    refs: &[[Point2f32; 2]],
    eps: f32,
) -> Vec<f32> {
    if len <= 1e-6 {
        return Vec::new();
    }
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let mut out: Vec<f32> = Vec::new();
    for [p, q] in refs {
        let (ex, ey) = (q.x - p.x, q.y - p.y);
        let elen = (ex * ex + ey * ey).sqrt();
        if elen <= 1e-6 {
            continue; // degenerate reference
        }
        let (wx, wy) = (p.x - a.x, p.y - a.y);
        // denom = len·elen·sin(angle between) — scale the parallel test by the
        // segment lengths so it's unit-consistent.
        let denom = dx * ey - dy * ex;
        if denom.abs() > 1e-4 * len * elen {
            // Non-parallel lines: one intersection; keep it if it lies on both
            // SEGMENTS (allowing an endpoint graze of `eps` metres on each).
            let t = (wx * ey - wy * ex) / denom; // along the wall, 0..1
            let u = (wx * dy - wy * dx) / denom; // along the reference, 0..1
            let (tol_t, tol_u) = (eps / len, eps / elen);
            if (-tol_t..=1.0 + tol_t).contains(&t) && (-tol_u..=1.0 + tol_u).contains(&u) {
                out.push((t * len).clamp(0.0, len));
            }
        } else {
            // Parallel: collinear only if the reference lies on the wall's
            // LINE (perpendicular offset within eps); then each of its
            // endpoints inside the wall's extent marks the overlap.
            let off = (wx * dy - wy * dx).abs() / len;
            if off <= eps {
                for r in [p, q] {
                    let d = ((r.x - a.x) * dx + (r.y - a.y) * dy) / len;
                    if (-eps..=len + eps).contains(&d) {
                        out.push(d.clamp(0.0, len));
                    }
                }
            }
        }
    }
    out.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    out.dedup_by(|x, y| (*x - *y).abs() <= eps);
    out
}

/// Distance from `coord` to the nearest edge of `ring` that is PARALLEL to
/// `axis` (a `Horizontal` axis compares against horizontal edges' y, a
/// `Vertical` one against vertical edges' x). `None` when the ring has no
/// edge on that axis. Coordinates are whatever space `ring` is in (the
/// callers pass metres). Used to pick which of a near-collinear pair lies on
/// a reference ring (the new-storey collinearity review).
pub fn nearest_parallel_edge_distance(
    ring: &[Point2f32],
    axis: &crate::Axis,
    coord: f32,
) -> Option<f32> {
    let n = ring.len();
    let mut best: Option<f32> = None;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let d = match axis {
            crate::Axis::Horizontal if (a.y - b.y).abs() < 1e-3 => (coord - a.y).abs(),
            crate::Axis::Vertical if (a.x - b.x).abs() < 1e-3 => (coord - a.x).abs(),
            _ => continue,
        };
        best = Some(best.map_or(d, |bst: f32| bst.min(d)));
    }
    best
}

/// Twice the signed shoelace area of a ring (sign = winding); callers that
/// only compare magnitudes use `.abs()`.
pub(crate) fn ring_signed_area2(pts: &[Point2f32]) -> f32 {
    let n = pts.len();
    if n < 3 {
        return 0.0;
    }
    let mut s = 0.0;
    for i in 0..n {
        let a = pts[i];
        let b = pts[(i + 1) % n];
        s += a.x * b.y - b.x * a.y;
    }
    s
}

/// The perpendicular offset `(dx, dy)` that moves an axis-aligned wall toward
/// the middle of the local interior of ring `polygon` — for placing a dimension
/// arrow inside the room rather than on the wall. Casts a perpendicular from the
/// wall midpoint toward the ring centroid and returns half the distance to the
/// nearest opposing parallel edge. `None` if the wall isn't axis-aligned, the
/// ring has < 4 vertices, or no opposing edge is found.
pub fn interior_perpendicular_offset(
    polygon: &[(f64, f64)],
    wall_a: (f64, f64),
    wall_b: (f64, f64),
) -> Option<(f64, f64)> {
    let n = polygon.len();
    if n < 4 {
        return None;
    }
    let mid_x = (wall_a.0 + wall_b.0) * 0.5;
    let mid_y = (wall_a.1 + wall_b.1) * 0.5;

    let centroid_x: f64 = polygon.iter().map(|p| p.0).sum::<f64>() / n as f64;
    let centroid_y: f64 = polygon.iter().map(|p| p.1).sum::<f64>() / n as f64;

    let eps = 1e-4_f64;
    let dx_w = wall_b.0 - wall_a.0;
    let dy_w = wall_b.1 - wall_a.1;
    let is_horizontal = dy_w.abs() < eps;
    let is_vertical = dx_w.abs() < eps;
    if !(is_horizontal ^ is_vertical) {
        // Diagonal or degenerate wall — no axis-aligned interior offset.
        return None;
    }

    if is_horizontal {
        let direction = (centroid_y - mid_y).signum();
        if direction == 0.0 {
            return None;
        }
        let mut closest: Option<f64> = None;
        for i in 0..n {
            let a = polygon[i];
            let b = polygon[(i + 1) % n];
            if (a.1 - b.1).abs() > eps {
                continue; // not horizontal
            }
            let x_min = a.0.min(b.0);
            let x_max = a.0.max(b.0);
            if mid_x < x_min - eps || mid_x > x_max + eps {
                continue;
            }
            let edge_dy = a.1 - mid_y;
            if edge_dy.abs() < eps {
                continue; // the wall itself
            }
            if edge_dy.signum() != direction {
                continue;
            }
            let dist = edge_dy.abs();
            if closest.map_or(true, |d| dist < d) {
                closest = Some(dist);
            }
        }
        let depth = closest?;
        Some((0.0, direction * depth * 0.5))
    } else {
        let direction = (centroid_x - mid_x).signum();
        if direction == 0.0 {
            return None;
        }
        let mut closest: Option<f64> = None;
        for i in 0..n {
            let a = polygon[i];
            let b = polygon[(i + 1) % n];
            if (a.0 - b.0).abs() > eps {
                continue;
            }
            let y_min = a.1.min(b.1);
            let y_max = a.1.max(b.1);
            if mid_y < y_min - eps || mid_y > y_max + eps {
                continue;
            }
            let edge_dx = a.0 - mid_x;
            if edge_dx.abs() < eps {
                continue;
            }
            if edge_dx.signum() != direction {
                continue;
            }
            let dist = edge_dx.abs();
            if closest.map_or(true, |d| dist < d) {
                closest = Some(dist);
            }
        }
        let depth = closest?;
        Some((direction * depth * 0.5, 0.0))
    }
}

/// Whether `(x, y)` lies inside the closed ring `pts` (ray-casting / even-odd
/// rule). The ring is taken as implicitly closed (last vertex joins first); a
/// repeated closing vertex is harmless. Degenerate rings (<3 vertices) contain
/// nothing.
pub fn ring_contains_point(pts: &[Point2f32], x: f32, y: f32) -> bool {
    let n = pts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > y) != (b.y > y) {
            let xc = a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x);
            if x < xc {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Whether the closed ring `inner` is FULLY contained within the closed ring
/// `outer` (a conservative containment test, world metres). Every `inner`
/// vertex must lie inside `outer` OR within `tol` of an `outer` edge (so a
/// shared/coincident boundary counts as contained, not outside), AND no `inner`
/// edge may properly cross an `outer` edge (which would mean `inner` pokes out
/// between two sampled vertices). Degenerate rings (< 3 vertices) are never
/// contained. Used by horizontal-exposure auto-derivation: a floor/ceiling
/// region only auto-classifies as Party/NonHeatLoss when it sits ENTIRELY over
/// the adjacent storey's footprint — a partial overlap (cantilever / setback)
/// stays ambiguous for manual marking.
pub fn ring_within_ring(inner: &[Point2f64], outer: &[Point2f64], tol: f64) -> bool {
    if inner.len() < 3 || outer.len() < 3 {
        return false;
    }
    let on_or_inside = |p: Point2f64| -> bool {
        if point_in_ring_f64(outer, p.x, p.y) {
            return true;
        }
        // Just outside by float noise, or exactly on the boundary → on an edge.
        let n = outer.len();
        for i in 0..n {
            let a = outer[i];
            let b = outer[(i + 1) % n];
            if domain::points::point_to_segment_distance(p.x, p.y, a.x, a.y, b.x, b.y) <= tol {
                return true;
            }
        }
        false
    };
    if !inner.iter().all(|p| on_or_inside(*p)) {
        return false;
    }
    // No inner edge may properly cross an outer edge (a poke-out the
    // vertex-sampling above can't see).
    let m = inner.len();
    let n = outer.len();
    for i in 0..m {
        let a1 = inner[i];
        let a2 = inner[(i + 1) % m];
        for j in 0..n {
            let b1 = outer[j];
            let b2 = outer[(j + 1) % n];
            if segments_cross_f64(a1, a2, b1, b2) {
                return false;
            }
        }
    }
    true
}

/// Whether `p` lies inside the closed polygon `poly` (even-odd ray cast). A
/// thin `Point2f32` wrapper over [`ring_contains_point`] — the single
/// point-in-polygon primitive in this crate — for the wall-face partition
/// helpers (which work in `Point2f32` wall-local space). Degenerate polygons
/// (< 3 vertices) contain nothing.
pub fn point_in_polygon(poly: &[Point2f32], p: Point2f32) -> bool {
    ring_contains_point(poly, p.x, p.y)
}

/// Result of [`snap_on_face`]: the (possibly adjusted) point plus which of its
/// coordinates were snapped to a face feature. An un-fixed coordinate is left
/// untouched (a higher layer prompts the user for it).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FaceSnap {
    pub point: Point2f32,
    pub x_fixed: bool,
    pub y_fixed: bool,
}

/// Snap a wall-face point `p` to the face's existing features, each axis
/// independently. The wall face is the rectangle `[(0,0), (len,0), (len,
/// height), (0,height)]` in wall-local coordinates (`x` = distance along the
/// wall, `y` = height off the floor).
///
/// - `p.x` snaps to the nearest of `{0.0, len}` ∪ {x of every cut endpoint}
///   within `eps`; on a hit, `x_fixed = true`.
/// - `p.y` snaps to the nearest of `{0.0, height}` ∪ {y of every cut endpoint}
///   within `eps`; on a hit, `y_fixed = true`.
///
/// The two axes are resolved separately, so a point can snap in x (to a cut
/// endpoint's column) while its y stays free at mid-height. An un-snapped
/// coordinate is returned unchanged.
pub fn snap_on_face(
    p: Point2f32,
    len: f32,
    height: f32,
    cuts: &[[Point2f32; 2]],
    eps: f32,
) -> FaceSnap {
    // Nearest candidate to `v` within `eps`, returning `(value, snapped)`.
    let snap_axis = |v: f32, edges: [f32; 2], pick: fn(&Point2f32) -> f32| -> (f32, bool) {
        let mut best: Option<f32> = None;
        let mut consider = |cand: f32| {
            let d = (cand - v).abs();
            if d <= eps && best.map_or(true, |b: f32| d < (b - v).abs()) {
                best = Some(cand);
            }
        };
        consider(edges[0]);
        consider(edges[1]);
        for c in cuts {
            consider(pick(&c[0]));
            consider(pick(&c[1]));
        }
        match best {
            Some(c) => (c, true),
            None => (v, false),
        }
    };
    let (x, x_fixed) = snap_axis(p.x, [0.0, len], |q| q.x);
    let (y, y_fixed) = snap_axis(p.y, [0.0, height], |q| q.y);
    FaceSnap {
        point: Point2f32::new(x, y),
        x_fixed,
        y_fixed,
    }
}

/// Subdivide a wall face into regions by a sequence of straight cuts. The face
/// starts as the rectangle `[(0,0), (len,0), (len,height), (0,height)]` (CCW)
/// in wall-local coordinates. Each cut is treated as an INFINITE straight line
/// through its two endpoints; it splits every current region into its two
/// half-polygons (a region the line misses passes through unchanged), via a
/// robust half-plane split — the 2-D analogue of [`Polygon3D::split_by_line`]
/// (`poly3d.rs`): classify vertices by signed distance to the line, interpolate
/// the crossing points, drop degenerate (< 3-vertex) halves. Cuts may be
/// diagonal; the model is a convex rectangle subdivided by lines, so every
/// region stays convex. A zero-length cut (both endpoints equal) is skipped.
pub fn subdivide_face(len: f32, height: f32, cuts: &[[Point2f32; 2]]) -> Vec<Vec<Point2f32>> {
    let mut regions = vec![vec![
        Point2f32::new(0.0, 0.0),
        Point2f32::new(len, 0.0),
        Point2f32::new(len, height),
        Point2f32::new(0.0, height),
    ]];
    for cut in cuts {
        let (a, b) = (cut[0], cut[1]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        if dx * dx + dy * dy < 1e-12 {
            continue; // degenerate cut — no line to split by
        }
        // Left normal of the cut direction; signed distance of a point to the
        // infinite line through `a` is the dot of (p - a) with this normal.
        let (nx, ny) = (-dy, dx);
        let signed = |p: Point2f32| (p.x - a.x) * nx + (p.y - a.y) * ny;
        let mut next = Vec::with_capacity(regions.len() * 2);
        for region in &regions {
            let (left, right) = split_polygon_by_line(region, signed);
            if left.len() >= 3 {
                next.push(left);
            }
            if right.len() >= 3 {
                next.push(right);
            }
        }
        regions = next;
    }
    regions
}

/// Split a convex polygon by an infinite line, given a `signed`-distance
/// function for that line (positive on one side, negative on the other). Walks
/// the ring like [`Polygon3D::split_by_line`]: a vertex on the `>= 0` side
/// joins `left`, a vertex on the `<= 0` side joins `right` (a vertex on the
/// line joins both), and every edge that strictly straddles the line
/// contributes its interpolated crossing point to both halves. Either half may
/// come back with fewer than 3 vertices when the line misses or merely grazes
/// the polygon — the caller drops those.
fn split_polygon_by_line(
    poly: &[Point2f32],
    signed: impl Fn(Point2f32) -> f32,
) -> (Vec<Point2f32>, Vec<Point2f32>) {
    let n = poly.len();
    let mut left = Vec::new();
    let mut right = Vec::new();
    for i in 0..n {
        let a = poly[i];
        let b = poly[(i + 1) % n];
        let da = signed(a);
        let db = signed(b);
        if da >= 0.0 {
            left.push(a);
        }
        if da <= 0.0 {
            right.push(a);
        }
        if da * db < 0.0 {
            let t = da / (da - db);
            let p = Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
            left.push(p);
            right.push(p);
        }
    }
    (left, right)
}

/// Even-odd point-in-ring test for an f64 ring (the f64 analogue of
/// [`ring_contains_point`]; implicitly closed). `< 3` vertices contain nothing.
fn point_in_ring_f64(pts: &[Point2f64], x: f64, y: f64) -> bool {
    let n = pts.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (a, b) = (pts[i], pts[j]);
        if (a.y > y) != (b.y > y) {
            let xc = a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x);
            if x < xc {
                inside = !inside;
            }
        }
        j = i;
    }
    inside
}

/// Whether f64 segments `a1→a2` and `b1→b2` cross at a single strictly-interior
/// point (the f64, point-discarding analogue of [`segment_crossing`]): excludes
/// shared endpoints and collinear touches, so a coincident/shared boundary edge
/// is NOT reported as a crossing.
fn segments_cross_f64(a1: Point2f64, a2: Point2f64, b1: Point2f64, b2: Point2f64) -> bool {
    let (rx, ry) = (a2.x - a1.x, a2.y - a1.y);
    let (sx, sy) = (b2.x - b1.x, b2.y - b1.y);
    let denom = rx * sy - ry * sx;
    if denom.abs() < 1e-12 {
        return false; // parallel or collinear
    }
    let (qpx, qpy) = (b1.x - a1.x, b1.y - a1.y);
    let t = (qpx * sy - qpy * sx) / denom;
    let u = (qpx * ry - qpy * rx) / denom;
    let m = 1e-9;
    t > m && t < 1.0 - m && u > m && u < 1.0 - m
}

/// If segments `a1→a2` and `b1→b2` cross at a single interior point (not at a
/// shared endpoint, not collinear), return that point. This is the
/// proper-crossing half of [`segments_properly_intersect`] (which ALSO flags
/// collinear / T-junction touches, for validation) — but here we yield the point
/// so [`untangle_ring`] can resolve the crossing.
fn segment_crossing(
    a1: Point2f32,
    a2: Point2f32,
    b1: Point2f32,
    b2: Point2f32,
) -> Option<Point2f32> {
    let rx = a2.x - a1.x;
    let ry = a2.y - a1.y;
    let sx = b2.x - b1.x;
    let sy = b2.y - b1.y;
    let denom = rx * sy - ry * sx;
    if denom.abs() < 1e-9 {
        return None; // parallel or collinear
    }
    let qpx = b1.x - a1.x;
    let qpy = b1.y - a1.y;
    let t = (qpx * sy - qpy * sx) / denom;
    let u = (qpx * ry - qpy * rx) / denom;
    let m = 1e-6;
    if t > m && t < 1.0 - m && u > m && u < 1.0 - m {
        Some(Point2f32::new(a1.x + t * rx, a1.y + t * ry))
    } else {
        None
    }
}

/// Remove self-intersections from a closed ring, returning a simple
/// (non-self-crossing) ring. When two non-adjacent edges cross, the ring folds
/// Replace any TILTED ring edge that does NOT lie on an outline edge with an
/// orthogonal L (a corner aligned with the edge ENTERING it). A sub-area's only
/// legitimate non-axis edges are the house walls it reproduces VERBATIM (a
/// diagonal or curved storey wall); every other diagonal is an artifact — a
/// boundary-follow that hopped between two non-adjacent edges, or an interior
/// wall meeting the boundary slightly off-level — and the assessor wants those
/// squared off. An edge whose BOTH ends lie within `tol` of one outline edge is
/// such a verbatim wall and is left untouched; anything else tilted gets the
/// corner. `tol` is small (a few mm) so a real off-level join is squared but
/// float noise on a verbatim edge isn't.
pub(crate) fn orthogonalize_off_boundary(
    ring: &[Point2f32],
    outline: &[Point2f32],
    tol: f32,
) -> Vec<Point2f32> {
    let n = ring.len();
    if n < 3 {
        return ring.to_vec();
    }
    let eps = 1e-3;
    let on_one_outline_edge = |a: Point2f32, b: Point2f32| -> bool {
        let m = outline.len();
        (0..m).any(|i| {
            let (p, q) = (outline[i], outline[(i + 1) % m]);
            project_point_to_segment(a, p, q).2 < tol && project_point_to_segment(b, p, q).2 < tol
        })
    };
    let mut out: Vec<Point2f32> = Vec::with_capacity(n + 4);
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        out.push(a);
        let tilted = (a.x - b.x).abs() > eps && (a.y - b.y).abs() > eps;
        if tilted && !on_one_outline_edge(a, b) {
            // Insert a corner aligned with the edge ENTERING `a`: if that edge is
            // vertical (shares a.x) keep a.x for the first leg, else keep a.y.
            let prev = ring[(i + n - 1) % n];
            let c = if (prev.x - a.x).abs() <= (prev.y - a.y).abs() {
                Point2f32::new(a.x, b.y)
            } else {
                Point2f32::new(b.x, a.y)
            };
            out.push(c);
        }
    }
    out
}

/// into two loops sharing the crossing point; we keep the larger-area loop and
/// discard the smaller "ear" the fold created, repeating until simple. Returns
/// the ring unchanged when it's already simple (or can't be reduced).
///
/// Used to repair a sub-area whose assembly placed a vertex past an adjacent
/// wall (e.g. a boundary-hugging loop whose diagonal endpoint landed outside
/// the room's own side wall), instead of emitting the crossed shape.
pub(crate) fn untangle_ring(points: &[Point2f32]) -> Vec<Point2f32> {
    let mut ring: Vec<Point2f32> = points.to_vec();
    // Each pass resolves one crossing (shrinking the ring), so the loop is
    // bounded by the vertex count.
    for _ in 0..points.len().max(4) {
        let n = ring.len();
        if n < 4 {
            return ring;
        }
        let mut found: Option<(usize, usize, Point2f32)> = None;
        'scan: for i in 0..n {
            let a1 = ring[i];
            let a2 = ring[(i + 1) % n];
            for j in (i + 2)..n {
                // The closing edge (n-1 → 0) is adjacent to edge 0; skip it.
                if i == 0 && j == n - 1 {
                    continue;
                }
                if let Some(x) = segment_crossing(a1, a2, ring[j], ring[(j + 1) % n]) {
                    found = Some((i, j, x));
                    break 'scan;
                }
            }
        }
        let Some((i, j, x)) = found else {
            return ring; // already simple
        };
        // Split at X into two loops, both starting at the crossing:
        //   loop A = X, ring[i+1 ..= j]
        //   loop B = X, ring[j+1 ..], ring[0 ..= i]
        let mut loop_a = vec![x];
        loop_a.extend_from_slice(&ring[i + 1..=j]);
        let mut loop_b = vec![x];
        loop_b.extend_from_slice(&ring[j + 1..n]);
        loop_b.extend_from_slice(&ring[0..=i]);
        ring = if ring_signed_area2(&loop_a).abs() >= ring_signed_area2(&loop_b).abs() {
            loop_a
        } else {
            loop_b
        };
    }
    ring
}

// ── Polygon-simplicity / collinearity predicates ───────────────────────────
// Pure point/ring predicates (no floor-plan state) — siblings of
// `segment_crossing` / `untangle_ring` above. `ParametricPlan::validate_simple`
// runs these over its rings; they used to live in `parametric.rs`.

/// Is every edge of `points` (as a closed ring) axis-aligned (horizontal or
/// vertical) within a small tolerance? Picks which simple-polygon validator to
/// run; a single diagonal edge (e.g. from Trim) makes this false.
pub(crate) fn ring_is_axis_aligned(points: &[Point2f32]) -> bool {
    let n = points.len();
    if n < 2 {
        return true;
    }
    let eps = 1e-3_f32;
    (0..n).all(|i| {
        let a = points[i];
        let b = points[(i + 1) % n];
        (a.x - b.x).abs() < eps || (a.y - b.y).abs() < eps
    })
}

/// Whether `cur` lies on — and strictly between the ends of — the straight line
/// from `prev` to `next`, so removing it wouldn't change the outline. Uses the
/// normalized cross product (≈ |sin| of the turn angle), so it's scale-free and
/// rejects a genuine curve facet (which turns by several degrees); the positive
/// dot product excludes a spike/fold doubling back.
pub(crate) fn is_collinear_vertex(prev: Point2f32, cur: Point2f32, next: Point2f32) -> bool {
    let (ux, uy) = (cur.x - prev.x, cur.y - prev.y);
    let (vx, vy) = (next.x - cur.x, next.y - cur.y);
    let lu = (ux * ux + uy * uy).sqrt();
    let lv = (vx * vx + vy * vy).sqrt();
    if lu < 1e-9 || lv < 1e-9 {
        return false;
    }
    let sin_turn = (ux * vy - uy * vx).abs() / (lu * lv);
    let dot = ux * vx + uy * vy;
    sin_turn <= 1e-3 && dot > 0.0
}

/// Shared simplicity check: at least `min_verts` vertices, every edge longer
/// than ~1mm, and no pair of non-adjacent edges crossing per `crosses`. The
/// `min_len_sq` floor sits below any real wall AND below the short facets of a
/// faceted curve, yet still catches a genuinely collapsed (near-coincident)
/// edge. The two validators differ only in `min_verts` + the predicate.
fn check_simple_with(
    points: &[Point2f32],
    min_verts: usize,
    crosses: impl Fn(Point2f32, Point2f32, Point2f32, Point2f32) -> bool,
) -> Result<(), String> {
    let n = points.len();
    if n < min_verts {
        return Err(format!("only {} vertices (need at least {})", n, min_verts));
    }
    let min_len_sq = 1e-6_f32;
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        let dx = a.x - b.x;
        let dy = a.y - b.y;
        if dx * dx + dy * dy < min_len_sq {
            return Err(format!("edge {} collapsed to zero length", i));
        }
    }
    for i in 0..n {
        let a1 = points[i];
        let a2 = points[(i + 1) % n];
        for j in (i + 2)..n {
            // The closing edge (n-1 → 0) is adjacent to edge 0; skip that pair.
            if i == 0 && j == n - 1 {
                continue;
            }
            let b1 = points[j];
            let b2 = points[(j + 1) % n];
            if crosses(a1, a2, b1, b2) {
                return Err(format!("edges {} and {} intersect", i, j));
            }
        }
    }
    Ok(())
}

/// Validate that `points` form a simple polygon TOLERATING diagonal edges:
/// ≥3 vertices, every edge positive-length, no two non-adjacent edges properly
/// intersect. Returns the first failure reason.
pub(crate) fn check_simple_general(points: &[Point2f32]) -> Result<(), String> {
    check_simple_with(points, 3, segments_properly_intersect)
}

/// Do segments `(a1,a2)` and `(b1,b2)` intersect anywhere other than at a shared
/// endpoint? Treats a vertex lying in the other segment's interior (T-junction)
/// as an intersection — so this catches MORE than [`segment_crossing`], which
/// finds only the proper-crossing point and ignores collinear/T-junction
/// touches. General orientation test; used by [`check_simple_general`].
fn segments_properly_intersect(a1: Point2f32, a2: Point2f32, b1: Point2f32, b2: Point2f32) -> bool {
    let eps = 1e-6_f32;
    let cross = |ox: f32, oy: f32, px: f32, py: f32, qx: f32, qy: f32| -> f32 {
        (px - ox) * (qy - oy) - (py - oy) * (qx - ox)
    };
    let d1 = cross(b1.x, b1.y, b2.x, b2.y, a1.x, a1.y);
    let d2 = cross(b1.x, b1.y, b2.x, b2.y, a2.x, a2.y);
    let d3 = cross(a1.x, a1.y, a2.x, a2.y, b1.x, b1.y);
    let d4 = cross(a1.x, a1.y, a2.x, a2.y, b2.x, b2.y);

    // Proper crossing: each segment straddles the other's supporting line.
    if ((d1 > eps && d2 < -eps) || (d1 < -eps && d2 > eps))
        && ((d3 > eps && d4 < -eps) || (d3 < -eps && d4 > eps))
    {
        return true;
    }

    // Collinear/touching: an endpoint lying within the other segment's span.
    let on_seg = |px: f32, py: f32, qx: f32, qy: f32, rx: f32, ry: f32| -> bool {
        // r is colinear with p→q (caller ensures) and lies strictly between.
        let min_x = px.min(qx) - eps;
        let max_x = px.max(qx) + eps;
        let min_y = py.min(qy) - eps;
        let max_y = py.max(qy) + eps;
        rx > min_x && rx < max_x && ry > min_y && ry < max_y
    };
    if d1.abs() < eps && on_seg(b1.x, b1.y, b2.x, b2.y, a1.x, a1.y) {
        return true;
    }
    if d2.abs() < eps && on_seg(b1.x, b1.y, b2.x, b2.y, a2.x, a2.y) {
        return true;
    }
    if d3.abs() < eps && on_seg(a1.x, a1.y, a2.x, a2.y, b1.x, b1.y) {
        return true;
    }
    if d4.abs() < eps && on_seg(a1.x, a1.y, a2.x, a2.y, b2.x, b2.y) {
        return true;
    }
    false
}

/// Validate that `points` form a simple AXIS-ALIGNED polygon: ≥4 vertices, every
/// edge positive-length, no two non-adjacent edges cross or overlap. A stricter,
/// faster special case of [`check_simple_general`] for orthogonal rings.
pub(crate) fn check_simple_axis_aligned(points: &[Point2f32]) -> Result<(), String> {
    check_simple_with(points, 4, axis_aligned_segments_cross)
}

fn axis_aligned_segments_cross(a1: Point2f32, a2: Point2f32, b1: Point2f32, b2: Point2f32) -> bool {
    let eps = 1e-4_f32;
    let a_horiz = (a1.y - a2.y).abs() < eps;
    let b_horiz = (b1.y - b2.y).abs() < eps;

    if a_horiz != b_horiz {
        // Perpendicular pair. Cross iff the vertical's x lies strictly inside
        // the horizontal's x range AND the horizontal's y lies strictly inside
        // the vertical's y range. (Strict — shared endpoints on adjacent edges
        // are already filtered by the caller, but T-junctions where one edge's
        // endpoint touches the other's interior count as crossing.)
        let (h1, h2, v1, v2) = if a_horiz {
            (a1, a2, b1, b2)
        } else {
            (b1, b2, a1, a2)
        };
        let h_y = h1.y;
        let h_min = h1.x.min(h2.x);
        let h_max = h1.x.max(h2.x);
        let v_x = v1.x;
        let v_min = v1.y.min(v2.y);
        let v_max = v1.y.max(v2.y);
        return h_min < v_x && v_x < h_max && v_min < h_y && h_y < v_max;
    }

    // Parallel pair. Overlap iff collinear and ranges intersect.
    let same_line = if a_horiz {
        (a1.y - b1.y).abs() < eps
    } else {
        (a1.x - b1.x).abs() < eps
    };
    if !same_line {
        return false;
    }
    let (a_min, a_max, b_min, b_max) = if a_horiz {
        (
            a1.x.min(a2.x),
            a1.x.max(a2.x),
            b1.x.min(b2.x),
            b1.x.max(b2.x),
        )
    } else {
        (
            a1.y.min(a2.y),
            a1.y.max(a2.y),
            b1.y.min(b2.y),
            b1.y.max(b2.y),
        )
    };
    a_min < b_max && b_min < a_max
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring64(pts: &[(f64, f64)]) -> Vec<Point2f64> {
        pts.iter().map(|&(x, y)| Point2f64::new(x, y)).collect()
    }

    /// `ring_within_ring`: a fully-enclosed inner ring is contained; a coincident
    /// (equal) ring counts as contained (shared boundary within tolerance); an
    /// inner ring that POKES OUT past the outer is not; degenerate rings never are.
    #[test]
    fn ring_within_ring_is_conservative_full_containment() {
        let outer = ring64(&[(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        // Strictly inside.
        let inside = ring64(&[(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]);
        assert!(ring_within_ring(&inside, &outer, 0.05));
        // Coincident / shared boundary → contained (within tolerance).
        assert!(ring_within_ring(&outer, &outer, 0.05));
        // Pokes out to the right (x up to 6) → NOT contained.
        let pokes = ring64(&[(1.0, 1.0), (6.0, 1.0), (6.0, 3.0), (1.0, 3.0)]);
        assert!(!ring_within_ring(&pokes, &outer, 0.05));
        // Fully outside → not contained.
        let outside = ring64(&[(5.0, 5.0), (6.0, 5.0), (6.0, 6.0), (5.0, 6.0)]);
        assert!(!ring_within_ring(&outside, &outer, 0.05));
        // Degenerate inner → never contained.
        assert!(!ring_within_ring(
            &ring64(&[(1.0, 1.0), (2.0, 2.0)]),
            &outer,
            0.05
        ));
    }

    // A vertical split: a region flush with the outline's left edge yields the
    // shared cut at its RIGHT edge; the complementary half yields the same x.
    #[test]
    fn interior_split_edge_finds_the_shared_cut() {
        let outline = ((0.0, 0.0), (10.0, 10.0));
        // Left half x ∈ 0..4 → cut at x = 4 (its right edge, interior).
        let left = [(0.0, 0.0), (4.0, 0.0), (4.0, 10.0), (0.0, 10.0)];
        assert_eq!(
            interior_split_edge(&left, outline),
            Some(((4.0, 0.0), (4.0, 10.0)))
        );
        // Right half x ∈ 4..10 → cut at x = 4 (its left edge, interior).
        let right = [(4.0, 0.0), (10.0, 0.0), (10.0, 10.0), (4.0, 10.0)];
        assert_eq!(
            interior_split_edge(&right, outline),
            Some(((4.0, 0.0), (4.0, 10.0)))
        );
        // Horizontal split: bottom band y ∈ 0..3 → cut at y = 3.
        let bottom = [(0.0, 0.0), (10.0, 0.0), (10.0, 3.0), (0.0, 3.0)];
        assert_eq!(
            interior_split_edge(&bottom, outline),
            Some(((0.0, 3.0), (10.0, 3.0)))
        );
        // A region filling the footprint on both axes has no interior cut.
        let full = [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        assert_eq!(interior_split_edge(&full, outline), None);
        // Empty ring → None.
        assert_eq!(interior_split_edge(&[], outline), None);
    }

    // Both validators must accept a clean square and reject too-few vertices, a
    // collapsed edge, and a self-crossing "bow-tie".
    #[test]
    fn check_simple_validators_accept_and_reject() {
        let pt = |x: f32, y: f32| Point2f32::new(x, y);
        let square = vec![pt(0., 0.), pt(4., 0.), pt(4., 4.), pt(0., 4.)];
        assert!(check_simple_axis_aligned(&square).is_ok());
        assert!(check_simple_general(&square).is_ok());

        // Too few vertices: axis-aligned needs 4, general needs 3.
        let tri = vec![pt(0., 0.), pt(4., 0.), pt(4., 4.)];
        assert!(
            check_simple_axis_aligned(&tri).is_err(),
            "axis-aligned needs ≥4"
        );
        assert!(
            check_simple_general(&tri).is_ok(),
            "general accepts a triangle"
        );
        assert!(check_simple_general(&[pt(0., 0.), pt(1., 0.)]).is_err());

        // A collapsed (zero-length) edge is rejected by both.
        let collapsed = vec![pt(0., 0.), pt(0., 0.), pt(4., 0.), pt(4., 4.), pt(0., 4.)];
        assert!(check_simple_axis_aligned(&collapsed).is_err());
        assert!(check_simple_general(&collapsed).is_err());

        // A bow-tie (edges 0–1 and 2–3 cross) is rejected by the general check.
        let bowtie = vec![pt(0., 0.), pt(4., 4.), pt(4., 0.), pt(0., 4.)];
        assert!(
            check_simple_general(&bowtie).is_err(),
            "self-crossing rejected"
        );
    }

    #[test]
    fn untangle_removes_a_self_crossing() {
        let p = |x, y| Point2f32::new(x, y);
        // The reported fold: the diagonal's bottom corner (x=2.75) sits LEFT of
        // the room's left wall (x=2.781), so the diagonal (edge 0) crosses the
        // left wall (edge 3).
        let crossed = vec![
            p(2.750228, 1.0471573),
            p(3.650228, 2.0471573),
            p(3.650228, 3.0471573),
            p(2.7811332, 3.0471573),
            p(2.7811332, 1.0471573),
        ];
        let fixed = untangle_ring(&crossed);
        // No non-adjacent edge pair crosses any more.
        let n = fixed.len();
        assert!(n >= 3, "kept a polygon: {fixed:?}");
        for i in 0..n {
            for j in (i + 2)..n {
                if i == 0 && j == n - 1 {
                    continue;
                }
                assert!(
                    segment_crossing(fixed[i], fixed[(i + 1) % n], fixed[j], fixed[(j + 1) % n])
                        .is_none(),
                    "edges {i} and {j} still cross in {fixed:?}"
                );
            }
        }
        // Kept the big room body (the three outer corners + the crossing point),
        // dropped the tiny bottom ear the fold created.
        assert!(fixed.len() <= 4, "kept the larger loop: {fixed:?}");
    }

    #[test]
    fn untangle_leaves_a_simple_ring_untouched() {
        let p = |x, y| Point2f32::new(x, y);
        let square = vec![p(0.0, 0.0), p(4.0, 0.0), p(4.0, 4.0), p(0.0, 4.0)];
        assert_eq!(untangle_ring(&square), square);
    }

    #[test]
    fn bbox_and_diagonal() {
        let pts = vec![p(1.0, 2.0), p(4.0, 2.0), p(4.0, 6.0), p(1.0, 6.0)];
        assert_eq!(bbox(&pts), Some(((1.0, 2.0), (4.0, 6.0))));
        // 3x4 box → diagonal 5.
        assert!((bbox_diagonal(&pts) - 5.0).abs() < 1e-5);
        assert_eq!(bbox(&[]), None);
        assert_eq!(bbox_diagonal(&[]), 0.0);
        assert_eq!(bbox_diagonal(&[p(3.0, 3.0)]), 0.0);
    }

    #[test]
    fn project_point_to_segment_interior_endpoints_and_degenerate() {
        let a = p(0.0, 0.0);
        let b = p(10.0, 0.0);
        // Interior foot: perpendicular distance, t in (0,1), point on the segment.
        let (q, t, d) = project_point_to_segment(p(3.0, 4.0), a, b);
        assert!((q.x - 3.0).abs() < 1e-5 && q.y.abs() < 1e-5);
        assert!((t - 0.3).abs() < 1e-5);
        assert!((d - 4.0).abs() < 1e-5);
        // Beyond b: clamps to b, but t reports the overshoot (>1).
        let (q2, t2, _) = project_point_to_segment(p(15.0, 0.0), a, b);
        assert!((q2.x - 10.0).abs() < 1e-5 && t2 > 1.0);
        // Before a: clamps to a, t < 0.
        let (q3, t3, _) = project_point_to_segment(p(-5.0, 1.0), a, b);
        assert!(q3.x.abs() < 1e-5 && t3 < 0.0);
        // Degenerate segment: returns a at t=0.
        let (q4, t4, d4) = project_point_to_segment(p(2.0, 0.0), a, a);
        assert!(q4 == a && t4 == 0.0 && (d4 - 2.0).abs() < 1e-5);
    }

    #[test]
    fn ring_signed_area2_is_twice_shoelace() {
        // 3x4 rectangle, area 12 → 2x = 24 (CCW positive).
        let r = vec![p(0.0, 0.0), p(3.0, 0.0), p(3.0, 4.0), p(0.0, 4.0)];
        assert!((ring_signed_area2(&r).abs() - 24.0).abs() < 1e-4);
        assert_eq!(ring_signed_area2(&[p(0.0, 0.0), p(1.0, 1.0)]), 0.0);
    }

    #[test]
    fn project_to_circle_lands_on_radius() {
        let (x, y) = project_to_circle((3.0, 0.0), (0.0, 0.0), 5.0);
        assert!((x - 5.0).abs() < 1e-4 && y.abs() < 1e-4);
    }

    #[test]
    fn intersect_circles_picks_nearest() {
        // Two unit circles at (0,0) and (2,0) meet at (1, 0). cur biases side.
        let p = intersect_circles((0.0, 0.0), 1.414, (2.0, 0.0), 1.414, (1.0, 5.0));
        assert!((p.0 - 1.0).abs() < 1e-2 && p.1 > 0.0);
    }

    #[test]
    fn find_flipped_edge_detects_x_inversion() {
        let before = vec![vec![(1i8, 0i8), (0, 1), (-1, 0), (0, -1)]];
        let after = vec![vec![(-1i8, 0i8), (0, 1), (-1, 0), (0, -1)]];
        assert_eq!(find_flipped_edge(&before, &after), Some((0, 0)));
    }

    #[test]
    fn find_flipped_edge_ignores_zero_signs() {
        let before = vec![vec![(0i8, 1i8)]];
        let after = vec![vec![(1i8, 1i8)]]; // x went 0 → 1: not a flip
        assert_eq!(find_flipped_edge(&before, &after), None);
    }

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    #[test]
    fn stroke_sagitta_measures_signed_bulge() {
        // Stroke from (0,0) to (10,0) bowing UP (left of +x is +y). Apex at y=3.
        let up = vec![p(0.0, 0.0), p(5.0, 3.0), p(10.0, 0.0)];
        assert!((stroke_sagitta(&up) - 3.0).abs() < 1e-4);
        // Same chord bowing DOWN → negative.
        let down = vec![p(0.0, 0.0), p(5.0, -2.5), p(10.0, 0.0)];
        assert!((stroke_sagitta(&down) + 2.5).abs() < 1e-4);
        // Noisy: max deviation wins, sign preserved.
        let noisy = vec![
            p(0.0, 0.0),
            p(3.0, 1.0),
            p(5.0, 4.0),
            p(7.0, 1.2),
            p(10.0, 0.0),
        ];
        assert!((stroke_sagitta(&noisy) - 4.0).abs() < 1e-4);
    }

    #[test]
    fn circular_arc_passes_through_endpoints_and_apex() {
        let pts = circular_arc_points(p(0.0, 0.0), p(10.0, 0.0), 3.0, 5.0);
        assert!(pts.len() >= 3);
        // Endpoints pinned exactly.
        assert!((pts[0].x).abs() < 1e-6 && (pts[0].y).abs() < 1e-6);
        let last = pts[pts.len() - 1];
        assert!((last.x - 10.0).abs() < 1e-6 && last.y.abs() < 1e-6);
        // Apex (midpoint of the arc) bulges to the chord's left (+y) by ~sagitta.
        let mid = pts[pts.len() / 2];
        assert!(
            (mid.x - 5.0).abs() < 0.3,
            "apex near mid-chord x, got {}",
            mid.x
        );
        assert!(
            (mid.y - 3.0).abs() < 0.2,
            "apex bulge ≈ sagitta, got {}",
            mid.y
        );
        // Every sample sits on the circle (equal radius about the fitted centre).
        // Centre is on the +y axis at k = (s²-a²)/2s with a=5, s=3 → k=-8/3.
        let (cx, cy) = (5.0_f32, -8.0 / 3.0);
        let r = ((5.0_f32).powi(2) + (8.0_f32 / 3.0).powi(2)).sqrt();
        for q in &pts {
            let d = ((q.x - cx).powi(2) + (q.y - cy).powi(2)).sqrt();
            assert!((d - r).abs() < 1e-2, "point off circle: d={d}, r={r}");
        }
    }

    #[test]
    fn circular_arc_negative_sagitta_bows_the_other_way() {
        let pts = circular_arc_points(p(0.0, 0.0), p(10.0, 0.0), -3.0, 5.0);
        let mid = pts[pts.len() / 2];
        assert!(
            mid.y < -2.5,
            "negative sagitta should bow to -y, got {}",
            mid.y
        );
    }

    #[test]
    fn circular_arc_degenerate_is_a_straight_segment() {
        assert_eq!(
            circular_arc_points(p(0.0, 0.0), p(10.0, 0.0), 0.0, 5.0),
            vec![p(0.0, 0.0), p(10.0, 0.0)]
        );
    }

    #[test]
    fn segment_len_is_euclidean() {
        assert_eq!(segment_len(p(0.0, 0.0), p(3.0, 4.0)), 5.0);
        assert_eq!(segment_len(p(2.0, 2.0), p(2.0, 2.0)), 0.0);
    }

    #[test]
    fn lerp_along_degenerate_segment_stays_at_start() {
        assert_eq!(lerp_along(p(1.0, 1.0), p(1.0, 1.0), 0.0, 5.0), (1.0, 1.0));
        assert_eq!(lerp_along(p(0.0, 0.0), p(10.0, 0.0), 10.0, 2.5), (2.5, 0.0));
    }

    #[test]
    fn fit_axis_line_near_vertical_snaps_to_x() {
        // Wavy but near-vertical stroke at x ≈ 3: position varies on X.
        let stroke = vec![p(3.02, 0.0), p(2.97, 1.0), p(3.03, 2.0), p(2.99, 3.0)];
        let (axis, coord) = fit_axis_line(&stroke).expect("near-vertical fits");
        assert_eq!(axis, ParameterAxis::X);
        assert!((coord - 3.0).abs() < 0.05, "coord ≈ 3, got {coord}");
    }

    #[test]
    fn fit_axis_line_near_horizontal_snaps_to_y() {
        let stroke = vec![p(0.0, 2.01), p(1.5, 1.98), p(3.0, 2.02), p(4.5, 2.0)];
        let (axis, coord) = fit_axis_line(&stroke).expect("near-horizontal fits");
        assert_eq!(axis, ParameterAxis::Y);
        assert!((coord - 2.0).abs() < 0.05, "coord ≈ 2, got {coord}");
    }

    #[test]
    fn fit_axis_line_diagonal_snaps_to_dominant_axis() {
        // Steeper than 45° (rises 3 in y per 1 in x): the vertical component
        // dominates, so it snaps to a VERTICAL line → position axis X.
        let stroke = vec![p(0.0, 0.0), p(0.5, 1.5), p(1.0, 3.0)];
        let (axis, coord) = fit_axis_line(&stroke).expect("diagonal snaps, no rejection");
        assert_eq!(axis, ParameterAxis::X);
        // Coordinate is the centroid's x.
        assert!((coord - 0.5).abs() < 1e-5, "centroid x, got {coord}");
    }

    #[test]
    fn fit_axis_line_rejects_a_closed_loop() {
        // A square loop: max perpendicular deviation ≈ half the span — far
        // over the 15% line-likeness ratio.
        let loop_stroke = vec![
            p(0.0, 0.0),
            p(2.0, 0.0),
            p(2.0, 2.0),
            p(0.0, 2.0),
            p(0.0, 0.1),
        ];
        assert_eq!(fit_axis_line(&loop_stroke), None);
    }

    #[test]
    fn fit_axis_line_rejects_an_l_shape() {
        // Two equal arms at a right angle: the corner sits well off any single
        // fitted line.
        let l_stroke = vec![
            p(0.0, 0.0),
            p(1.0, 0.0),
            p(2.0, 0.0),
            p(2.0, 1.0),
            p(2.0, 2.0),
        ];
        assert_eq!(fit_axis_line(&l_stroke), None);
    }

    #[test]
    fn fit_axis_segment_near_vertical_extent_is_the_y_range() {
        // Vertical line (position on X) — the extent runs along Y, taken as
        // drawn: y spans 0.2 → 3.1 over the wavy stroke.
        let stroke = vec![p(3.02, 0.2), p(2.97, 1.0), p(3.03, 3.1), p(2.99, 2.0)];
        let (axis, coord, (lo, hi)) = fit_axis_segment(&stroke).expect("near-vertical fits");
        assert_eq!(axis, ParameterAxis::X);
        assert!((coord - 3.0).abs() < 0.05, "coord ≈ 3, got {coord}");
        assert_eq!((lo, hi), (0.2, 3.1), "extent = y-range as drawn");
    }

    #[test]
    fn fit_axis_segment_near_horizontal_extent_is_the_x_range() {
        // Drawn right-to-left: lo/hi still come out ordered.
        let stroke = vec![p(4.5, 2.0), p(3.0, 2.02), p(1.5, 1.98), p(0.4, 2.01)];
        let (axis, coord, (lo, hi)) = fit_axis_segment(&stroke).expect("near-horizontal fits");
        assert_eq!(axis, ParameterAxis::Y);
        assert!((coord - 2.0).abs() < 0.05, "coord ≈ 2, got {coord}");
        assert_eq!(
            (lo, hi),
            (0.4, 4.5),
            "extent = x-range, draw-direction-free"
        );
    }

    #[test]
    fn fit_axis_line_is_fit_axis_segment_without_the_extent() {
        let strokes: Vec<Vec<Point2f32>> = vec![
            vec![p(3.02, 0.0), p(2.97, 1.0), p(3.03, 2.0), p(2.99, 3.0)],
            vec![p(0.0, 2.01), p(1.5, 1.98), p(3.0, 2.02), p(4.5, 2.0)],
            vec![p(0.0, 0.0), p(0.5, 1.5), p(1.0, 3.0)],
            vec![p(0.0, 0.0), p(0.0, 0.02)], // rejected: both must be None
            vec![],
        ];
        for stroke in &strokes {
            assert_eq!(
                fit_axis_line(stroke),
                fit_axis_segment(stroke).map(|(a, c, _)| (a, c)),
                "wrapper equivalence for {stroke:?}"
            );
        }
    }

    #[test]
    fn fit_axis_line_rejects_short_strokes() {
        // Two points 2 cm apart: under the 0.05 m span floor.
        assert_eq!(fit_axis_line(&[p(0.0, 0.0), p(0.0, 0.02)]), None);
        // Fewer than 2 points.
        assert_eq!(fit_axis_line(&[]), None);
        assert_eq!(fit_axis_line(&[p(1.0, 1.0)]), None);
    }

    /// A hand-wavy closed rectangle fits to its bbox; a diagonal stroke, a
    /// filled blob, a bare line (one degenerate side), and a too-short point
    /// set are rejected. Closure itself is the CALLER's test — an open
    /// three-sided U still fits (documented behaviour).
    #[test]
    fn fit_axis_rect_accepts_rectangles_and_rejects_lines_diagonals_blobs() {
        // Wavy rectangle ~ (1,1)..(5,3), drawn around the boundary.
        let rect = vec![
            p(1.0, 1.05),
            p(3.0, 0.95),
            p(5.0, 1.0),
            p(5.05, 2.0),
            p(4.95, 3.0),
            p(3.0, 3.05),
            p(1.0, 2.95),
            p(0.95, 2.0),
            p(1.05, 1.1),
        ];
        let ((x0, y0), (x1, y1)) = fit_axis_rect(&rect).expect("rectangle fits");
        assert!((x0 - 0.95).abs() < 1e-6 && (x1 - 5.05).abs() < 1e-6);
        assert!((y0 - 0.95).abs() < 1e-6 && (y1 - 3.05).abs() < 1e-6);

        // A diagonal stroke: its middle sits half the short side from every
        // bbox edge — rejected.
        let diagonal = vec![p(0.0, 0.0), p(1.0, 1.0), p(2.0, 2.0), p(3.0, 3.0)];
        assert_eq!(fit_axis_rect(&diagonal), None);
        // A shallow diagonal is no better (the tolerance scales with the
        // SHORT side, so flattening the slope doesn't sneak it through).
        let shallow = vec![p(0.0, 0.0), p(1.0, 0.2), p(2.0, 0.4), p(3.0, 0.6)];
        assert_eq!(fit_axis_rect(&shallow), None);

        // A filled blob (points across the interior) is rejected.
        let blob = vec![
            p(0.0, 0.0),
            p(2.0, 0.0),
            p(1.0, 1.0),
            p(2.0, 2.0),
            p(0.0, 2.0),
            p(1.2, 0.9),
        ];
        assert_eq!(fit_axis_rect(&blob), None);

        // A straight line has a degenerate side — not a rectangle.
        let line = vec![p(0.0, 0.0), p(1.0, 0.01), p(2.0, 0.0), p(3.0, 0.01)];
        assert_eq!(fit_axis_rect(&line), None);
        // Fewer than 4 points never fit.
        assert_eq!(
            fit_axis_rect(&[p(0.0, 0.0), p(2.0, 0.0), p(2.0, 2.0)]),
            None
        );
    }

    #[test]
    fn project_distance_along_clamps_to_segment() {
        let (a, b) = (p(0.0, 0.0), p(10.0, 0.0));
        assert_eq!(project_distance_along(a, b, 10.0, 4.0, 3.0), 4.0);
        assert_eq!(project_distance_along(a, b, 10.0, -5.0, 0.0), 0.0);
        assert_eq!(project_distance_along(a, b, 10.0, 15.0, 0.0), 10.0);
        assert_eq!(project_distance_along(a, a, 0.0, 5.0, 5.0), 0.0);
    }

    // ── Wall-face partition helpers ─────────────────────────────────────────

    /// Unsigned polygon area via the shoelace (half of `ring_signed_area2`).
    fn area(poly: &[Point2f32]) -> f32 {
        ring_signed_area2(poly).abs() * 0.5
    }

    #[test]
    fn point_in_polygon_basic_in_out() {
        let square = vec![p(0.0, 0.0), p(4.0, 0.0), p(4.0, 4.0), p(0.0, 4.0)];
        assert!(point_in_polygon(&square, p(2.0, 2.0)));
        assert!(!point_in_polygon(&square, p(5.0, 2.0)));
        assert!(!point_in_polygon(&square, p(-1.0, 2.0)));
        // Degenerate polygon contains nothing.
        assert!(!point_in_polygon(&[p(0.0, 0.0), p(1.0, 1.0)], p(0.5, 0.5)));
    }

    #[test]
    fn subdivide_face_single_diagonal_yields_two_regions() {
        let (len, height) = (4.0_f32, 3.0_f32);
        // Diagonal corner-to-corner cut.
        let cuts = [[p(0.0, 0.0), p(len, height)]];
        let regions = subdivide_face(len, height, &cuts);
        assert_eq!(regions.len(), 2, "one cut → two regions: {regions:?}");
        let total: f32 = regions.iter().map(|r| area(r)).sum();
        assert!(
            (total - len * height).abs() < 1e-3,
            "areas sum to len*height, got {total}"
        );
    }

    #[test]
    fn subdivide_face_two_crossing_cuts_yield_four_regions() {
        let (len, height) = (4.0_f32, 2.0_f32);
        // A vertical-ish and a horizontal-ish cut, both crossing the interior.
        let cuts = [
            [p(2.0, -1.0), p(2.0, 3.0)], // infinite vertical line at x=2
            [p(-1.0, 1.0), p(5.0, 1.0)], // infinite horizontal line at y=1
        ];
        let regions = subdivide_face(len, height, &cuts);
        assert_eq!(regions.len(), 4, "two crossing cuts → four regions");
        let total: f32 = regions.iter().map(|r| area(r)).sum();
        assert!(
            (total - len * height).abs() < 1e-3,
            "areas still sum to len*height, got {total}"
        );
        // Each quadrant is a 2x1 rectangle.
        for r in &regions {
            assert!(
                (area(r) - 2.0).abs() < 1e-3,
                "quadrant area 2, got {}",
                area(r)
            );
        }
    }

    #[test]
    fn subdivide_face_line_that_misses_passes_region_through() {
        let (len, height) = (4.0_f32, 3.0_f32);
        // A line entirely below the face (y = -1) misses the rectangle.
        let cuts = [[p(0.0, -1.0), p(len, -1.0)]];
        let regions = subdivide_face(len, height, &cuts);
        assert_eq!(regions.len(), 1, "missing line leaves one region");
        assert!((area(&regions[0]) - len * height).abs() < 1e-3);
    }

    #[test]
    fn subdivide_face_skips_degenerate_cut() {
        let (len, height) = (4.0_f32, 3.0_f32);
        let cuts = [[p(1.0, 1.0), p(1.0, 1.0)]]; // zero-length
        let regions = subdivide_face(len, height, &cuts);
        assert_eq!(regions.len(), 1);
        assert!((area(&regions[0]) - len * height).abs() < 1e-3);
    }

    #[test]
    fn snap_on_face_fixes_x_to_edge_and_leaves_mid_height_y_free() {
        let (len, height) = (5.0_f32, 2.5_f32);
        // x near the right edge (5.0); y at mid-height (free).
        let res = snap_on_face(p(4.98, 1.25), len, height, &[], 0.05);
        assert!(res.x_fixed, "x snaps to the near edge");
        assert!((res.point.x - 5.0).abs() < 1e-6, "x → len");
        assert!(!res.y_fixed, "mid-height y stays free");
        assert_eq!(res.point.y, 1.25, "free y unchanged");
    }

    #[test]
    fn snap_on_face_snaps_to_cut_endpoints_independently() {
        let (len, height) = (6.0_f32, 3.0_f32);
        // An existing cut whose endpoints introduce snap columns/rows.
        let cuts = [[p(2.0, 0.0), p(2.0, 3.0)]];
        // Point near the cut's x=2 column, y mid (free).
        let res = snap_on_face(p(2.02, 1.5), len, height, &cuts, 0.05);
        assert!(res.x_fixed && (res.point.x - 2.0).abs() < 1e-6);
        assert!(!res.y_fixed);
        // y near the cut endpoint at y=0 → snaps to floor (0 also an edge).
        let res2 = snap_on_face(p(3.5, 0.03), len, height, &cuts, 0.05);
        assert!(res2.y_fixed && res2.point.y.abs() < 1e-6);
        assert!(!res2.x_fixed, "x mid-wall stays free");
        // Nothing within eps → neither fixed, point untouched.
        let res3 = snap_on_face(p(3.5, 1.5), len, height, &cuts, 0.05);
        assert!(!res3.x_fixed && !res3.y_fixed);
        assert_eq!(res3.point, p(3.5, 1.5));
    }

    #[test]
    fn simplify_to_cut_takes_first_and_last_of_a_shaky_stroke() {
        // A shaky near-straight diagonal stroke.
        let stroke = vec![p(0.0, 0.0), p(1.02, 0.98), p(1.98, 2.05), p(3.0, 3.0)];
        let cut = crate::simplify_to_cut(&stroke, 0.3).expect("near-straight stroke yields a cut");
        assert!(
            (cut[0].x).abs() < 1e-6 && (cut[0].y).abs() < 1e-6,
            "first point"
        );
        assert!(
            (cut[1].x - 3.0).abs() < 1e-6 && (cut[1].y - 3.0).abs() < 1e-6,
            "last point"
        );
        // Too few points → None.
        assert_eq!(crate::simplify_to_cut(&[p(0.0, 0.0)], 0.3), None);
        // Coincident endpoints (a tap) → None.
        assert_eq!(
            crate::simplify_to_cut(&[p(1.0, 1.0), p(1.0, 1.0)], 0.3),
            None
        );
    }

    /// `crossing_distances_along`: proper crossings, endpoint touches, and
    /// collinear overlaps all land as along-wall distances; duplicates within
    /// eps collapse; the result is sorted; misses contribute nothing.
    #[test]
    fn crossing_distances_along_finds_crossings_touches_and_overlaps() {
        let (a, b, len) = (p(0.0, 0.0), p(10.0, 0.0), 10.0);
        let refs = [
            [p(4.0, -1.0), p(4.0, 1.0)],     // proper crossing at 4.0
            [p(7.0, 0.0), p(7.0, 5.0)],      // endpoint TOUCHES the wall at 7.0
            [p(2.0, 0.0), p(5.0, 0.0)],      // collinear overlap → 2.0 and 5.0
            [p(4.003, -2.0), p(4.003, 2.0)], // dup of 4.0 within eps → collapses
            [p(3.0, 1.0), p(6.0, 1.0)],      // parallel, OFF the wall line → nothing
            [p(1.0, 0.5), p(1.0, 3.0)],      // stops short of the wall → nothing
            [p(12.0, -1.0), p(12.0, 1.0)],   // crosses the LINE past the wall end → nothing
        ];
        let ds = crossing_distances_along(a, b, len, &refs, 0.01);
        assert_eq!(ds.len(), 4, "4 distinct candidates: {ds:?}");
        for (got, want) in ds.iter().zip([2.0, 4.0, 5.0, 7.0]) {
            assert!((got - want).abs() < 0.01, "sorted candidates: {ds:?}");
        }
        // Degenerate wall → nothing.
        assert!(crossing_distances_along(a, a, 0.0, &refs, 0.01).is_empty());
    }
}
