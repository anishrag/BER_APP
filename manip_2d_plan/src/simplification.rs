//! Stroke simplification algorithms
use crate::Error;
use domain::points::Point2f32;
use geo::Simplify;
use geo::{Coord, LineString};

/// Simplify a stroke using the Ramer-Douglas-Peucker algorithm
pub fn simplify_stroke(points: &[Point2f32], tolerance: f32) -> Result<Vec<Point2f32>, Error> {
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
