//! Orthogonal constraint enforcement (parallel/perpendicular lines)

use domain::points::Point2f32;

use crate::Error;
use std::f32::consts::PI;

/// Convert a simplified stroke to an orthogonal polygon and generate base constraints
pub fn make_orthogonal(points: &[Point2f32], tolerance: f32) -> Result<Vec<Point2f32>, Error> {
    if points.len() < 3 {
        return Err(Error::InsufficientPoints);
    }

    // First, determine the dominant directions
    let dominant_angle = find_dominant_angle(points);

    // Snap all points to horizontal/vertical grid aligned with dominant angle
    let orthogonal_points = snap_to_orthogonal(&points, dominant_angle, tolerance)?;

    // Remove duplicate consecutive points
    let cleaned = remove_duplicates(&orthogonal_points, tolerance);

    if cleaned.len() < 3 {
        return Err(Error::OrthogonalError(
            "Too few points after orthogonalization".into(),
        ));
    }

    Ok(cleaned)
}

/// Find the dominant angle in a set of points
fn find_dominant_angle(points: &[Point2f32]) -> f32 {
    if points.len() < 2 {
        return 0.0;
    }

    // Collect all edge angles
    let mut angles = Vec::new();
    for i in 0..points.len() - 1 {
        let dx = points[i + 1].x - points[i].x;
        let dy = points[i + 1].y - points[i].y;
        let angle = dy.atan2(dx);
        angles.push(angle);
    }

    // Normalize angles to [0, PI/2] quadrant
    let normalized: Vec<f32> = angles
        .iter()
        .map(|&a| {
            let mut norm = a.abs();
            while norm > PI / 2.0 {
                norm -= PI / 2.0;
            }
            norm
        })
        .collect();

    // Find the angle with the longest total length
    let mut best_angle = 0.0;
    let mut best_weight = 0.0;

    for (i, &angle) in normalized.iter().enumerate() {
        let dx = points[i + 1].x - points[i].x;
        let dy = points[i + 1].y - points[i].y;
        let length = (dx * dx + dy * dy).sqrt();

        let weight = length;
        if weight > best_weight {
            best_weight = weight;
            best_angle = angles[i];
        }
    }

    // Round to nearest 0, 90, 180, or 270 degrees if close
    let degrees = best_angle.to_degrees();
    let rounded = (degrees / 90.0).round() * 90.0;
    if (degrees - rounded).abs() < 15.0 {
        return rounded.to_radians();
    }

    best_angle
}

/// Snap points to an orthogonal grid
fn snap_to_orthogonal(
    points: &[Point2f32],
    base_angle: f32,
    tolerance: f32,
) -> Result<Vec<Point2f32>, Error> {
    if points.len() < 2 {
        return Err(Error::InsufficientPoints);
    }

    let cos_angle = base_angle.cos();
    let sin_angle = base_angle.sin();

    // Rotate points to align with axes
    let rotated: Vec<(f32, f32)> = points
        .iter()
        .map(|p| {
            let x = p.x * cos_angle + p.y * sin_angle;
            let y = -p.x * sin_angle + p.y * cos_angle;
            (x, y)
        })
        .collect();

    // Snap to grid
    let mut snapped = Vec::new();
    let mut current = rotated[0];
    snapped.push(current);

    for i in 1..rotated.len() {
        let next = rotated[i];
        let dx = next.0 - current.0;
        let dy = next.1 - current.1;

        // Determine if this should be horizontal or vertical
        if dx.abs() > dy.abs() {
            // Horizontal move
            snapped.push((next.0, current.1));
        } else {
            // Vertical move
            snapped.push((current.0, next.1));
        }

        current = *snapped.last().unwrap();
    }

    // If stroke should be closed, ensure last point connects properly to first
    if (points.first().unwrap() - points.last().unwrap()).norm() < tolerance * 10.0 {
        let first = snapped[0];
        let last = *snapped.last().unwrap();

        // Add intermediate point if needed to close properly
        if (last.0 - first.0).abs() > tolerance && (last.1 - first.1).abs() > tolerance {
            // Need a corner point
            snapped.push((first.0, last.1));
        }
    }

    // Rotate back
    let result: Vec<Point2f32> = snapped
        .iter()
        .map(|(x, y)| {
            let px = x * cos_angle - y * sin_angle;
            let py = x * sin_angle + y * cos_angle;
            Point2f32::new(px, py)
        })
        .collect();

    Ok(result)
}

/// Remove duplicate consecutive points
fn remove_duplicates(points: &[Point2f32], tolerance: f32) -> Vec<Point2f32> {
    if points.is_empty() {
        return Vec::new();
    }

    let mut result = vec![points[0]];

    for i in 1..points.len() {
        let last = result.last().unwrap();
        if (last - points[i]).norm() > tolerance {
            result.push(points[i]);
        }
    }

    // Check if first and last are duplicates
    if result.len() > 1 {
        let first = result[0];
        let last = *result.last().unwrap();
        if (first - last).norm() < tolerance {
            result.pop();
        }
    }

    result
}

// /// Classify edges as horizontal or vertical
// pub fn classify_edges(polygon: &Polygon, tolerance: f64) -> Vec<Orientation> {
//     polygon
//         .edges()
//         .iter()
//         .map(|edge| {
//             if edge.is_horizontal(tolerance) {
//                 Orientation::Horizontal
//             } else {
//                 Orientation::Vertical
//             }
//         })
//         .collect()
// }

// /// Make an existing polygon more orthogonal by adjusting vertices
// pub fn orthogonalize_polygon(polygon: &Polygon, tolerance: f64) -> Result<Polygon, Error> {
//     let mut vertices = polygon.vertices.clone();
//
//     if vertices.len() < 3 {
//         return Err(Error::InsufficientPoints);
//     }
//
//     // Iteratively adjust vertices to make edges horizontal or vertical
//     let max_iterations = 10;
//     for _ in 0..max_iterations {
//         let mut adjusted = false;
//
//         for i in 0..vertices.len() {
//             let prev = if i == 0 { vertices.len() - 1 } else { i - 1 };
//             let next = (i + 1) % vertices.len();
//
//             let v_prev = vertices[prev];
//             let v_curr = vertices[i];
//             let v_next = vertices[next];
//
//             // Try to align with previous edge
//             let edge_to_prev = Line::new(v_prev, v_curr);
//             if !edge_to_prev.is_horizontal(tolerance) && !edge_to_prev.is_vertical(tolerance) {
//                 // Try making it horizontal or vertical
//                 let dx = (v_curr.x - v_prev.x).abs();
//                 let dy = (v_curr.y - v_prev.y).abs();
//
//                 if dx > dy {
//                     // Make horizontal
//                     vertices[i].y = v_prev.y;
//                     adjusted = true;
//                 } else {
//                     // Make vertical
//                     vertices[i].x = v_prev.x;
//                     adjusted = true;
//                 }
//             }
//         }
//
//         if !adjusted {
//             break;
//         }
//     }
//
//     Ok(Polygon::new(vertices))
// }
//
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_find_dominant_angle() {
        let points = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.2),
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
        ];

        let angle = find_dominant_angle(&points);
        // Should be close to 0 (horizontal)
        assert!((angle.abs()) < 0.2);
    }
}
