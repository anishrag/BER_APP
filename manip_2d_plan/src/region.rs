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
fn clamp_inside(outline: &[Point2f32], points: &[Point2f32]) -> Vec<Point2f32> {
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
    let ends_meet = points
        .first()
        .zip(points.last())
        .map(|(a, b)| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt() < close_gap_m)
        .unwrap_or(false);
    // A genuine CLOSED loop both meets its ends AND encloses real area. A short
    // straight CUT whose ends merely fall within `close_gap_m` is NOT a loop —
    // clipping it as one collapses it to a degenerate sliver and the cut is then
    // lost. Keep it OPEN so the cut solver sees the real line.
    let closed = points.len() >= 3 && ends_meet && encloses_area(points);
    if closed {
        if let Some(clipped) = clip_loop_to_outline(outline, points) {
            return clipped;
        }
    }
    clamp_inside(outline, points)
}

/// Whether `pts` enclose real area (a drawn ROOM) rather than being a near-
/// straight line whose ends happen to fall close together (a short CUT). The
/// enclosed (shoelace) area must be a real fraction of the stroke's extent.
fn encloses_area(pts: &[Point2f32]) -> bool {
    let Some(((x0, y0), (x1, y1))) = geom::bbox(pts) else {
        return false;
    };
    let span = (x1 - x0).max(y1 - y0);
    let area = geom::ring_signed_area2(pts).abs() * 0.5;
    area > span * span * 0.05
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

/// `outline` MINUS the union of `holes`, as EVERY remaining piece (each a fresh
/// `FloorPlan`). Carving a sub-area against the relevant piece — instead of the
/// raw outline — makes it snap to the holes' shared edges AND keeps it from
/// overlapping them, so a new phase/living area tiles cleanly against the ones
/// already drawn. The result can be DISCONNECTED (a hole spanning the region
/// leaves separate pieces), so the caller picks the one the cut is in. NB: a
/// subtraction that punches an interior HOLE keeps only the exterior boundary.
fn subtract_all(outline: &[Point2f32], holes: &[Vec<Point2f32>]) -> Vec<FloorPlan> {
    use geo::bool_ops::BooleanOps;
    let mut region: geo::MultiPolygon<f64> = geo::MultiPolygon::new(vec![to_poly(outline)]);
    for h in holes {
        if h.len() >= 3 {
            region = region.difference(&geo::MultiPolygon::new(vec![to_poly(h)]));
        }
    }
    region
        .0
        .into_iter()
        .filter_map(|poly| {
            let mut ring: Vec<Point2f32> = poly
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
        })
        .collect()
}

/// The single LARGEST piece of `outline` minus `holes` — see [`subtract_all`].
fn subtract(outline: &[Point2f32], holes: &[Vec<Point2f32>]) -> Option<FloorPlan> {
    subtract_all(outline, holes).into_iter().max_by(|a, b| {
        geom::ring_signed_area2(&a.get_points())
            .abs()
            .total_cmp(&geom::ring_signed_area2(&b.get_points()).abs())
    })
}

/// Whether any vertex or edge-midpoint of `area` lies clearly OUTSIDE `outline`
/// — farther than `tol` beyond the boundary. Points on a shared wall (distance
/// ~0) don't count. Used as a dimensioning guardrail: reject a dimension that
/// would push a living area out of the storey.
fn any_point_outside(outline: &[Point2f32], area: &[Point2f32], tol: f32) -> bool {
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

    /// EVERY piece of this plan minus `holes` — the available region can be
    /// disconnected; see [`subtract_all`].
    pub fn subtract_all(&self, holes: &[Vec<Point2f32>]) -> Vec<FloorPlan> {
        subtract_all(&self.parametric.ring_points(0), holes)
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

    /// `subtract_all`: when an existing region splits the available space into
    /// DISCONNECTED pieces, every piece comes back (not just the largest, as
    /// `subtract` returns) — the caller picks the one its cut is in.
    #[test]
    fn subtract_all_returns_every_disconnected_piece() {
        // A 10×2 strip with a vertical bar through the middle → two separate pieces.
        let outline = vec![p(0., 0.), p(10., 0.), p(10., 2.), p(0., 2.)];
        let holes = [vec![p(4., -1.), p(6., -1.), p(6., 3.), p(4., 3.)]];
        let pieces = subtract_all(&outline, &holes);
        assert_eq!(pieces.len(), 2, "the bar splits the strip into two pieces");
        // a left piece and a right piece, on opposite sides of the bar
        assert!(pieces
            .iter()
            .any(|fp| geom::ring_contains_point(&fp.get_points(), 2.0, 1.0)));
        assert!(pieces
            .iter()
            .any(|fp| geom::ring_contains_point(&fp.get_points(), 8.0, 1.0)));
        // `subtract` (largest only) still returns a single piece.
        assert!(subtract(&outline, &holes).is_some());
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

#[cfg(test)]
mod overlap_repro {
    use super::*;
    fn pt(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }
    fn outline() -> Vec<Point2f32> {
        vec![
            pt(2.95, 0.571),
            pt(2.45, 0.571),
            pt(2.45, 1.571),
            pt(1.55, 1.571),
            pt(1.55, 2.571),
            pt(1.552, 2.653),
            pt(1.562, 2.735),
            pt(1.578, 2.815),
            pt(1.601, 2.894),
            pt(1.63, 2.971),
            pt(1.665, 3.045),
            pt(1.706, 3.116),
            pt(1.753, 3.183),
            pt(1.805, 3.246),
            pt(1.863, 3.305),
            pt(1.925, 3.359),
            pt(1.991, 3.407),
            pt(2.061, 3.45),
            pt(2.134, 3.487),
            pt(2.21, 3.518),
            pt(2.289, 3.542),
            pt(2.369, 3.56),
            pt(2.45, 3.571),
            pt(3.45, 3.571),
            pt(3.45, 3.071),
            pt(4.15, 3.071),
            pt(4.15, 3.571),
            pt(5.45, 3.571),
            pt(5.45, 2.871),
            pt(4.85, 2.571),
            pt(3.45, 2.571),
            pt(3.45, 1.271),
            pt(3.455, 1.219),
            pt(3.456, 1.166),
            pt(3.453, 1.113),
            pt(3.445, 1.061),
            pt(3.433, 1.01),
            pt(3.416, 0.959),
            pt(3.395, 0.911),
            pt(3.37, 0.864),
            pt(3.341, 0.82),
            pt(3.309, 0.778),
            pt(3.273, 0.74),
            pt(3.234, 0.704),
            pt(3.192, 0.672),
            pt(3.147, 0.644),
            pt(3.1, 0.619),
            pt(3.052, 0.599),
            pt(3.001, 0.583),
        ]
    }
    #[test]
    fn subtract_excludes_edge_touching_area() {
        let o = outline();
        // the right-room living area (touches the right walls) from the log
        let area = vec![
            pt(3.4498644, 2.571196),
            pt(4.8498645, 2.571196),
            pt(5.4498644, 2.871196),
            pt(5.4498644, 3.571196),
            pt(4.149864, 3.571196),
            pt(4.149864, 3.071196),
            pt(3.4498644, 3.071196),
        ];
        let pieces = subtract_all(&o, &[area]);
        // a point well inside the right room must NOT be in any available piece
        assert!(
            !pieces
                .iter()
                .any(|p| geom::ring_contains_point(&p.get_points(), 5.0, 3.2)),
            "the existing right-room area must be excluded from the available region"
        );
        // the left part is still available
        assert!(pieces
            .iter()
            .any(|p| geom::ring_contains_point(&p.get_points(), 2.5, 2.0)));
    }
}
