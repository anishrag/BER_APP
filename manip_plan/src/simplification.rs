//! Stroke simplification algorithms
use crate::Error;
use domain::points::Point2f32;
use geo::Simplify;
use geo::{Coord, LineString};

/// RDP-simplify an OPEN polyline (e.g. one interior wall of a carve), allowing a
/// 2-point result. Unlike [`simplify_stroke`] — which is for CLOSED rings and
/// errors below 3 points — a straight wall here MUST be allowed to collapse to
/// its two endpoints; otherwise the caller keeps the raw wander and staircases
/// it. Returns the input unchanged when there's nothing to simplify (<3 points).
pub(crate) fn simplify_open(points: &[Point2f32], tolerance: f32) -> Vec<Point2f32> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let coords: Vec<Coord> = points
        .iter()
        .map(|p| Coord {
            x: p.x as f64,
            y: p.y as f64,
        })
        .collect();
    LineString::from(coords)
        .simplify(tolerance as f64)
        .points()
        .map(|c| Point2f32::from([c.x() as f32, c.y() as f32]))
        .collect()
}

/// Reduce a shaky freehand "partition cut" stroke to a single straight cut
/// segment. The stroke is RDP-simplified ([`simplify_open`], which tolerates a
/// 2-point result) and the simplified polyline's FIRST and LAST points are
/// taken as the cut endpoints — for a near-straight stroke RDP collapses to
/// exactly those two, and even when residual wobble leaves intermediate points
/// the wall-face model only cares about the infinite line through the ends.
/// `None` for a stroke that can't define a line (< 2 points, or both endpoints
/// coincident after simplification).
pub fn simplify_to_cut(stroke: &[Point2f32], tolerance: f32) -> Option<[Point2f32; 2]> {
    if stroke.len() < 2 {
        return None;
    }
    let simplified = simplify_open(stroke, tolerance);
    let a = *simplified.first()?;
    let b = *simplified.last()?;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    if dx * dx + dy * dy < 1e-12 {
        return None;
    }
    Some([a, b])
}

/// Simplify a stroke using the Ramer-Douglas-Peucker algorithm
pub(crate) fn simplify_stroke(
    points: &[Point2f32],
    tolerance: f32,
) -> Result<Vec<Point2f32>, Error> {
    if points.len() < 3 {
        return Err(Error::InsufficientPoints);
    }

    let coords: Vec<Coord> = points
        .iter()
        .map(|p| Coord {
            x: p.x as f64,
            y: p.y as f64,
        })
        .collect();

    let line_string = LineString::from(coords);

    // Apply RDP algorithm
    let simplified = line_string.simplify(tolerance as f64);

    // Convert back
    let new_points: Vec<Point2f32> = simplified
        .points()
        .map(|c| Point2f32::from([c.x() as f32, c.y() as f32]))
        .collect();

    if new_points.len() < 3 {
        return Err(Error::InsufficientPoints);
    }

    Ok(new_points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simplify_stroke() {
        // A noisy rectangle: each side has an extra point that should be removed.
        // Use polygon-shaped data — simplify_stroke requires ≥3 points in the output
        // because the downstream consumer (orthogonalize → parametric) is polygonal.
        let points = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(5.0, 0.05),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 5.0),
            Point2f32::new(5.0, 5.05),
            Point2f32::new(0.0, 5.0),
        ];
        let simplified = simplify_stroke(&points, 0.5).unwrap();

        assert!(simplified.len() < points.len());
        assert!(simplified.len() >= 3);
    }

    #[test]
    fn test_simplify_stroke_rejects_degenerate() {
        // Nearly-collinear points collapse to a 2-point line, which is rejected.
        let points = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(1.0, 0.1),
            Point2f32::new(2.0, 0.0),
            Point2f32::new(10.0, 0.0),
        ];
        assert!(matches!(
            simplify_stroke(&points, 0.5),
            Err(Error::InsufficientPoints)
        ));
    }
}
