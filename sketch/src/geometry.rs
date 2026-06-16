// ===== sketch/src/geometry.rs =====
use crate::model::Stroke;
#[cfg(test)]
use domain::points::Point2f32;

impl Stroke {
    /// If the stroke's lift point is within `epsilon` of its starting point,
    /// mark the stroke closed and pop any trailing points still inside the
    /// snap zone (the homing-in wiggle the assessor leaves as they curl back).
    ///
    /// The polyline is left **open** — `is_closed` is the signal that the
    /// closing edge should be treated as implicit. Downstream code can then
    /// run RDP to collapse collinear closing-edge drag points and drop the
    /// final trailing point before orthogonalizing, so the closing edge
    /// doesn't introduce a spurious vertex.
    ///
    /// Stops popping once we hit a point outside the zone or ≥3 points remain.
    pub fn check_closed(&mut self, epsilon: f32) -> bool {
        if self.points.len() < 3 {
            return false;
        }

        let first = self.points.first().copied().unwrap();
        let last = self.points.last().copied().unwrap();

        let dx = first.x - last.x;
        let dy = first.y - last.y;
        let eps2 = epsilon * epsilon;

        if dx * dx + dy * dy > eps2 {
            return false;
        }

        self.is_closed = true;

        // The lift point is inside the snap zone — pop it.
        self.points.pop();

        // Pop further trailing points still inside the zone (the homing-in wiggle).
        // Keep ≥3 points so a tiny shape doesn't collapse to nothing.
        while self.points.len() > 3 {
            let p = *self.points.last().unwrap();
            let pdx = p.x - first.x;
            let pdy = p.y - first.y;
            if pdx * pdx + pdy * pdy > eps2 {
                break;
            }
            self.points.pop();
        }

        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domain::sketch::EditTools;

    fn stroke_from(points: &[(f32, f32)]) -> Stroke {
        let mut s = Stroke::new(
            EditTools::DrawPerpWalls,
            Point2f32::new(points[0].0, points[0].1),
        );
        for &(x, y) in &points[1..] {
            s.push(Point2f32::new(x, y));
        }
        s
    }

    #[test]
    fn check_closed_returns_false_when_lift_far_from_start() {
        let mut s = stroke_from(&[(0.0, 0.0), (100.0, 0.0), (100.0, 100.0), (0.0, 100.0)]);
        assert!(!s.check_closed(30.0));
        assert!(!s.is_closed);
        assert_eq!(s.points.len(), 4);
    }

    #[test]
    fn check_closed_pops_lift_point() {
        let mut s = stroke_from(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (5.0, 5.0), // lifted within close_epsilon of (0,0)
        ]);
        assert!(s.check_closed(30.0));
        assert!(s.is_closed);
        // Lift point gone; polyline left open (last != first).
        assert_eq!(s.points.len(), 4);
        assert_eq!(*s.points.last().unwrap(), Point2f32::new(0.0, 100.0));
    }

    #[test]
    fn check_closed_absorbs_homing_in_wiggle() {
        // The last three points are inside the snap zone (radius 30 around origin);
        // they should all collapse so we don't carry wiggle into orthogonalization.
        let mut s = stroke_from(&[
            (0.0, 0.0),
            (200.0, 0.0),
            (200.0, 200.0),
            (0.0, 200.0),
            (20.0, 50.0), // outside snap zone (53 from start)
            (10.0, 20.0), // inside
            (5.0, 10.0),  // inside
            (2.0, 3.0),   // inside (lift point)
        ]);
        assert!(s.check_closed(30.0));
        // Expected: (0,0), (200,0), (200,200), (0,200), (20,50). Polyline is open.
        assert_eq!(s.points.len(), 5);
        assert_eq!(*s.points.last().unwrap(), Point2f32::new(20.0, 50.0));
    }

    #[test]
    fn check_closed_preserves_minimum_polygon() {
        // Tiny square where every corner is inside the snap zone. Must keep ≥3
        // points so the shape doesn't disappear.
        let mut s = stroke_from(&[
            (0.0, 0.0),
            (10.0, 0.0),
            (10.0, 10.0),
            (0.0, 10.0),
            (1.0, 1.0), // lift inside zone
        ]);
        assert!(s.check_closed(30.0));
        assert!(s.points.len() >= 3);
        assert_eq!(*s.points.first().unwrap(), Point2f32::new(0.0, 0.0));
    }
}
