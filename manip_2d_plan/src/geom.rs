//! Small pure-geometry helpers for the dimensioning solver and the
//! direction-flip guard. No floor-plan state — just coordinate math over
//! `(f32, f32)` points, plus edge-sign comparison.

use crate::ParameterAxis;
use domain::points::Point2f32;

/// Fit a straight line to a (hand-drawn, possibly wavy) point set by total
/// least squares: returns the centroid and a unit direction vector along the
/// line's principal axis (the direction of greatest spread). Used by Trim to
/// turn a freehand cut stroke into a clean straight cut line.
pub fn fit_line(points: &[Point2f32]) -> (Point2f32, (f32, f32)) {
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
pub fn stroke_sagitta(stroke: &[Point2f32]) -> f32 {
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
pub fn circular_arc_points(
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
