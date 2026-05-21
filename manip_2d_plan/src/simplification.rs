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

// /// Simplify a stroke directly to a polygon
// pub fn simplify_to_polygon(points: &mut [Point2f64], tolerance: f64) -> Result<Polygon, Error> {
//     let simplified = simplify_stroke(stroke, tolerance)?;
//
//     // Ensure it's closed
//     if let (Some(first), Some(last)) = (points.first(), points.last()) {
//         if first.distance_to(last) > tolerance {
//             points.push(*first);
//         }
//     }
//
//     // Remove the duplicate closing point for the polygon
//     if points.len() > 1 {
//         let first = points[0];
//         let last = *points.last().unwrap();
//         if first.distance_to(&last) < 1e-10 {
//             points.pop();
//         }
//     }
//
//     if points.len() < 3 {
//         return Err(Error::InsufficientPoints);
//     }
//
//     Ok(Polygon::new(points))
// }
//
// /// Simplify by merging nearly collinear segments
// pub fn merge_collinear_segments(polygon: &Polygon, angle_tolerance: f64) -> Polygon {
//     if polygon.vertices.len() < 3 {
//         return polygon.clone();
//     }
//
//     let mut simplified = Vec::new();
//     let n = polygon.vertices.len();
//
//     let mut i = 0;
//     while i < n {
//         let curr = polygon.vertices[i];
//         let next = polygon.vertices[(i + 1) % n];
//         let next_next = polygon.vertices[(i + 2) % n];
//
//         // Calculate angle between consecutive segments
//         let v1x = next.x - curr.x;
//         let v1y = next.y - curr.y;
//         let v2x = next_next.x - next.x;
//         let v2y = next_next.y - next.y;
//
//         let len1 = (v1x * v1x + v1y * v1y).sqrt();
//         let len2 = (v2x * v2x + v2y * v2y).sqrt();
//
//         if len1 > 1e-10 && len2 > 1e-10 {
//             let dot = (v1x * v2x + v1y * v2y) / (len1 * len2);
//             let angle = dot.acos();
//
//             if angle < angle_tolerance {
//                 // Skip the middle point (merge segments)
//                 simplified.push(curr);
//                 i += 2; // Skip next point
//                 continue;
//             }
//         }
//
//         simplified.push(curr);
//         i += 1;
//     }
//
//     if simplified.len() < 3 {
//         return polygon.clone();
//     }
//
//     Polygon::new(simplified)
// }
//
// /// Remove short edges that are likely digitization artifacts
// pub fn remove_short_edges(polygon: &Polygon, min_length: f64) -> Polygon {
//     if polygon.vertices.len() < 4 {
//         return polygon.clone();
//     }
//
//     let mut keep = vec![true; polygon.vertices.len()];
//
//     for i in 0..polygon.vertices.len() {
//         if let Some(edge) = polygon.edge(i) {
//             if edge.length() < min_length {
//                 // Mark this vertex for removal
//                 keep[i] = false;
//             }
//         }
//     }
//
//     let filtered: Vec<Point> = polygon
//         .vertices
//         .iter()
//         .enumerate()
//         .filter(|(i, _)| keep[*i])
//         .map(|(_, p)| *p)
//         .collect();
//
//     if filtered.len() < 3 {
//         return polygon.clone();
//     }
//
//     Polygon::new(filtered)
// }

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
