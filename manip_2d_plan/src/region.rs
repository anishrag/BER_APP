//! The outline polygon as a REGION: containment tests, clipping a freehand
//! stroke inside it, and boolean subtraction. Pure geometry over a ring; the
//! `carve` pipeline and the app shell call these. (Carving itself is `carve.rs`.)

use crate::{geom, FloorPlan};
use domain::points::Point2f32;

/// A ring as a `geo` polygon (f64), for boolean ops.
fn to_poly(pts: &[Point2f32]) -> geo::Polygon<f64> {
    geo::Polygon::new(
        geo::LineString::from(
            pts.iter()
                .map(|p| (p.x as f64, p.y as f64))
                .collect::<Vec<_>>(),
        ),
        vec![],
    )
}

/// Closest point on the ring to `p`, and the distance to it.
fn nearest_on_ring(ring: &[Point2f32], p: Point2f32) -> (Point2f32, f32) {
    let n = ring.len();
    let mut best = p;
    let mut best_d = f32::MAX;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let (q, _t, d) = geom::project_point_to_segment(p, a, b);
        if d < best_d {
            best_d = d;
            best = q;
        }
    }
    (best, best_d)
}

/// Clamp each point that lies OUTSIDE `outline` back onto its boundary (nearest
/// point on any edge); points already inside are left as drawn. A containment
/// guardrail: bring a stroke that strays out back to the boundary rather than
/// producing geometry outside the house.
pub(crate) fn clamp_inside(outline: &[Point2f32], points: &[Point2f32]) -> Vec<Point2f32> {
    if outline.len() < 3 {
        return points.to_vec();
    }
    points
        .iter()
        .map(|&p| {
            if geom::ring_contains_point(outline, p.x, p.y) {
                p
            } else {
                nearest_on_ring(outline, p).0
            }
        })
        .collect()
}

/// First-stage clip of a freehand sub-area stroke ONTO the storey, run before
/// any simplification. A CLOSED loop is intersected with the outline polygon, so
/// the loop is routed AROUND concavities (a trimmed-out notch is removed from the
/// area, not bridged) — unlike the per-point [`clamp_inside`], which collapses a
/// stroke drawn across a notch onto its corners. An OPEN path (or a
/// failed/degenerate clip) falls back to `clamp_inside`. `close_gap_m` is the
/// closed-loop threshold (see [`crate::SubareaTuning::close_gap_m`]).
pub(crate) fn clip_stroke_inside(
    outline: &[Point2f32],
    points: &[Point2f32],
    close_gap_m: f32,
) -> Vec<Point2f32> {
    let closed = points.len() >= 3
        && points
            .first()
            .zip(points.last())
            .map(|(a, b)| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt() < close_gap_m)
            .unwrap_or(false);
    if closed {
        if let Some(clipped) = clip_loop_to_outline(outline, points) {
            return clipped;
        }
    }
    clamp_inside(outline, points)
}

/// Intersect a closed freehand loop with the outline polygon and return the
/// largest resulting piece's boundary (the loop clipped to the storey, voids
/// removed). `None` if the intersection is empty or degenerate.
fn clip_loop_to_outline(outline: &[Point2f32], loop_pts: &[Point2f32]) -> Option<Vec<Point2f32>> {
    use geo::bool_ops::BooleanOps;
    use geo::Area;
    let clipped = to_poly(loop_pts).intersection(&to_poly(outline));
    let best = clipped.0.into_iter().max_by(|a, b| {
        a.unsigned_area()
            .partial_cmp(&b.unsigned_area())
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;
    let ring: Vec<Point2f32> = best
        .exterior()
        .points()
        .map(|c| Point2f32::new(c.x() as f32, c.y() as f32))
        .collect();
    (ring.len() >= 4).then_some(ring)
}

/// `outline` MINUS the union of `holes`, as the largest remaining piece's
/// boundary (a fresh `FloorPlan`). Carving a sub-area against this — instead of
/// the raw outline — makes it snap to the holes' shared edges AND keeps it from
/// overlapping them, so e.g. a new construction phase tiles cleanly against the
/// phases already drawn. `None` when nothing meaningful remains. NB: a
/// subtraction that punches an interior HOLE keeps only the exterior boundary
/// (phase areas tile from the edges in practice; an enclosed leftover is rare).
pub(crate) fn subtract(outline: &[Point2f32], holes: &[Vec<Point2f32>]) -> Option<FloorPlan> {
    use geo::bool_ops::BooleanOps;
    use geo::Area;
    let mut region: geo::MultiPolygon<f64> = geo::MultiPolygon::new(vec![to_poly(outline)]);
    for h in holes {
        if h.len() >= 3 {
            region = region.difference(&geo::MultiPolygon::new(vec![to_poly(h)]));
        }
    }
    let best = region.0.into_iter().max_by(|a, b| {
        a.unsigned_area()
            .partial_cmp(&b.unsigned_area())
            .unwrap_or(std::cmp::Ordering::Equal)
    })?;
    let mut ring: Vec<Point2f32> = best
        .exterior()
        .points()
        .map(|c| Point2f32::new(c.x() as f32, c.y() as f32))
        .collect();
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    (ring.len() >= 3)
        .then(|| FloorPlan::from_clean_ring(&ring).ok())
        .flatten()
}

/// Whether any vertex or edge-midpoint of `area` lies clearly OUTSIDE `outline`
/// — farther than `tol` beyond the boundary. Points on a shared wall (distance
/// ~0) don't count. Used as a dimensioning guardrail: reject a dimension that
/// would push a living area out of the storey.
pub(crate) fn any_point_outside(outline: &[Point2f32], area: &[Point2f32], tol: f32) -> bool {
    if outline.len() < 3 || area.len() < 3 {
        return false;
    }
    let dist_outside = |p: Point2f32| -> f32 {
        if geom::ring_contains_point(outline, p.x, p.y) {
            0.0
        } else {
            nearest_on_ring(outline, p).1
        }
    };
    let n = area.len();
    for i in 0..n {
        let a = area[i];
        let b = area[(i + 1) % n];
        let mid = Point2f32::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
        if dist_outside(a) > tol || dist_outside(mid) > tol {
            return true;
        }
    }
    false
}

/// `FloorPlan`'s region API — thin facades over the free functions above, each
/// handing the call `self`'s outline ring.
impl FloorPlan {
    /// Whether `(x, y)` lies inside the primary ring (ray-cast). Used to
    /// hit-test which candidate region a tap landed in.
    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        geom::ring_contains_point(&self.parametric.ring_points(0), x, y)
    }

    /// Clamp points that stray outside the outline back onto it — see
    /// [`clamp_inside`].
    pub fn clamp_inside(&self, points: &[Point2f32]) -> Vec<Point2f32> {
        clamp_inside(&self.parametric.ring_points(0), points)
    }

    /// First-stage clip of a freehand sub-area stroke onto the storey — see
    /// [`clip_stroke_inside`].
    pub fn clip_stroke_inside(&self, points: &[Point2f32], close_gap_m: f32) -> Vec<Point2f32> {
        clip_stroke_inside(&self.parametric.ring_points(0), points, close_gap_m)
    }

    /// This plan MINUS the union of `holes`, as a fresh `FloorPlan` — see
    /// [`subtract`].
    pub fn subtract(&self, holes: &[Vec<Point2f32>]) -> Option<FloorPlan> {
        subtract(&self.parametric.ring_points(0), holes)
    }

    /// Whether any vertex/edge-midpoint of `area` lies clearly outside this
    /// plan's outline — see [`any_point_outside`].
    pub fn any_point_outside(&self, area: &FloorPlan, tol: f32) -> bool {
        any_point_outside(&self.parametric.ring_points(0), &area.get_points(), tol)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// `subtract` (the "available region" for a new phase): the outline minus an
    /// existing phase leaves the remaining space, whose boundary is the shared
    /// edge — so a new phase carved against it snaps to that edge and can't
    /// overlap the old phase.
    #[test]
    fn subtract_leaves_the_available_region_with_the_shared_edge() {
        // 10×10 storey minus a 10×4 strip along the bottom → a 10×6 top region.
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap();
        let phase = vec![p(0., 0.), p(10., 0.), p(10., 4.), p(0., 4.)];
        let avail = outline.subtract(&[phase]).expect("a region remains");
        let pts = avail.get_points();
        assert!(
            geom::ring_contains_point(&pts, 5.0, 7.0),
            "top strip kept: {:?}",
            pts
        );
        assert!(
            !geom::ring_contains_point(&pts, 5.0, 2.0),
            "the existing phase (bottom strip) is excluded: {:?}",
            pts
        );
        // The shared edge y=4 is now part of the available region's boundary.
        assert!(
            pts.iter().any(|p| (p.y - 4.0).abs() < 1e-3),
            "shared edge y=4 present: {:?}",
            pts
        );
    }

    /// First-stage clip: a closed loop drawn STRAIGHT across a trimmed-out notch
    /// must be routed AROUND the void (the notch stays excluded), not bridged.
    #[test]
    fn clip_stroke_inside_routes_a_loop_around_a_notch() {
        // 6×4 storey with a notch bitten out of the top middle: x[2,4] y[2,4] is
        // OUTSIDE the storey.
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(6., 0.),
            p(6., 4.),
            p(4., 4.),
            p(4., 2.),
            p(2., 2.),
            p(2., 4.),
            p(0., 4.),
        ])
        .unwrap();
        // A loop drawn straight across the top, over the notch opening.
        let loop_pts = vec![p(1., 1.), p(5., 1.), p(5., 3.9), p(1., 3.9), p(1., 1.)];
        // The raw loop wrongly encloses the notch interior...
        assert!(geom::ring_contains_point(&loop_pts, 3.0, 3.0));
        // ...but clipped to the storey it does NOT, while keeping the real interior.
        let clipped = outline.clip_stroke_inside(&loop_pts, 0.7);
        assert!(
            !geom::ring_contains_point(&clipped, 3.0, 3.0),
            "notch stays excluded: {:?}",
            clipped
        );
        assert!(
            geom::ring_contains_point(&clipped, 1.5, 1.5),
            "storey interior kept: {:?}",
            clipped
        );
    }

    #[test]
    fn clamp_inside_pulls_outside_points_to_the_boundary() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0.2, 0.2)],
            0.5,
            true,
        )
        .unwrap();
        let clamped = outline.clamp_inside(&[p(5., 5.), p(15., 5.), p(5., -3.)]);
        assert_eq!(clamped[0], p(5., 5.), "inside point unchanged");
        assert!(
            (clamped[1].x - 10.0).abs() < 0.05 && (clamped[1].y - 5.0).abs() < 0.05,
            "right overshoot snaps to (10,5): {:?}",
            clamped[1]
        );
        assert!(
            clamped[2].y.abs() < 0.05 && (clamped[2].x - 5.0).abs() < 0.05,
            "bottom overshoot snaps to (5,0): {:?}",
            clamped[2]
        );
    }

    #[test]
    fn any_point_outside_flags_a_room_poking_out() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0.2, 0.2)],
            0.5,
            true,
        )
        .unwrap();
        let inside =
            FloorPlan::from_clean_ring(&[p(2., 2.), p(6., 2.), p(6., 6.), p(2., 6.)]).unwrap();
        assert!(
            !outline.any_point_outside(&inside, 0.05),
            "fully-inside room is fine"
        );
        let poking =
            FloorPlan::from_clean_ring(&[p(8., 2.), p(13., 2.), p(13., 6.), p(8., 6.)]).unwrap();
        assert!(
            outline.any_point_outside(&poking, 0.05),
            "room past x=10 is flagged"
        );
    }
}
