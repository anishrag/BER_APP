//! Orthogonal constraint enforcement (parallel/perpendicular lines)

use domain::points::Point2f32;

use crate::Error;
use std::f32::consts::PI;

/// Convert a simplified stroke to an orthogonal polygon and generate base constraints
pub fn make_orthogonal(points: &[Point2f32], tolerance: f32) -> Result<Vec<Point2f32>, Error> {
    make_orthogonal_at(points, tolerance, find_dominant_angle(points))
}

/// Like [`make_orthogonal`] but snaps to a grid aligned with a GIVEN
/// `base_angle` instead of the stroke's own dominant angle. Used for shapes
/// whose reference frame is fixed externally — e.g. a living area inside an
/// axis-aligned storey must snap to the storey's axes (`base_angle = 0`), so a
/// room drawn partly along a diagonal boundary doesn't tilt the whole shape to
/// 45°. (The boundary wall is then pulled onto the diagonal separately.)
pub fn make_orthogonal_at(
    points: &[Point2f32],
    tolerance: f32,
    base_angle: f32,
) -> Result<Vec<Point2f32>, Error> {
    if points.len() < 3 {
        return Err(Error::InsufficientPoints);
    }

    // Snap all points to horizontal/vertical grid aligned with the given angle.
    let orthogonal_points = snap_to_orthogonal(&points, base_angle, tolerance)?;

    // Remove duplicate consecutive points
    let cleaned = remove_duplicates(&orthogonal_points, tolerance);

    // Remove vertices whose two incident edges share the same axis (both
    // vertical at the same x, or both horizontal at the same y). These show
    // up when the user's stroke wobbled on a wall and `snap_to_orthogonal`
    // axis-aligned both halves of the wobble to the same coordinate but kept
    // them as separate vertices. Geometrically the middle vertex is redundant.
    let collapsed = remove_collinear(&cleaned, tolerance);

    if collapsed.len() < 3 {
        return Err(Error::OrthogonalError(
            "Too few points after orthogonalization".into(),
        ));
    }

    Ok(collapsed)
}

/// Snap nearby x-coordinates (and y-coordinates) together so walls the user
/// drew slightly offset land on the same coordinate. Catches typical sketch
/// wobble (e.g. the upper and lower bar of an L drawn with a few-px offset)
/// without affecting walls that are clearly separated.
///
/// Algorithm: sort each axis's values and form clusters where successive
/// sorted values are within `snap_radius`. Every value in a cluster is then
/// set to the cluster's mean. This is **single-linkage** clustering — a long
/// sequence of close values can chain into one cluster. For floor-plan-scale
/// input where walls are usually well-separated, this is fine; if you ever
/// have many walls close together, revisit.
pub fn snap_near_collinear_walls(mut points: Vec<Point2f32>, snap_radius: f32) -> Vec<Point2f32> {
    if points.len() < 3 {
        return points;
    }

    let x_reps = cluster_representatives(points.iter().map(|p| p.x), snap_radius);
    for p in &mut points {
        if let Some(rep) = x_reps.iter().find(|&&r| (r - p.x).abs() <= snap_radius) {
            p.x = *rep;
        }
    }

    let y_reps = cluster_representatives(points.iter().map(|p| p.y), snap_radius);
    for p in &mut points {
        if let Some(rep) = y_reps.iter().find(|&&r| (r - p.y).abs() <= snap_radius) {
            p.y = *rep;
        }
    }

    points
}

fn cluster_representatives(values: impl Iterator<Item = f32>, radius: f32) -> Vec<f32> {
    let mut sorted: Vec<f32> = values.collect();
    if sorted.is_empty() {
        return Vec::new();
    }
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let mut representatives = Vec::new();
    let mut cluster: Vec<f32> = vec![sorted[0]];
    for &v in &sorted[1..] {
        if (v - cluster.last().unwrap()).abs() <= radius {
            cluster.push(v);
        } else {
            representatives.push(cluster.iter().sum::<f32>() / cluster.len() as f32);
            cluster = vec![v];
        }
    }
    representatives.push(cluster.iter().sum::<f32>() / cluster.len() as f32);

    representatives
}

/// Snap the last vertex's coordinates so the implicit closing edge back to
/// the first vertex is exactly axis-aligned. Direction is dictated by the
/// last explicit edge: a vertical last edge means the closing edge must be
/// horizontal, so we set `last.y = first.y`; a horizontal last edge flips it.
/// Should only be called when the polygon was implicitly closed (i.e. the
/// `closed` flag is set in `from_points_to_orthogonal`).
pub fn snap_closing_edge(mut points: Vec<Point2f32>, tolerance: f32) -> Vec<Point2f32> {
    let n = points.len();
    if n < 3 {
        return points;
    }

    let last = points[n - 1];
    let prev = points[n - 2];
    let first = points[0];

    let last_edge_vertical = (prev.x - last.x).abs() < tolerance;
    let last_edge_horizontal = (prev.y - last.y).abs() < tolerance;

    if last_edge_vertical {
        // Closing edge runs horizontally — match y to first.
        points[n - 1].y = first.y;
    } else if last_edge_horizontal {
        // Closing edge runs vertically — match x to first.
        points[n - 1].x = first.x;
    }
    // If the last edge is neither (shouldn't happen post-orthogonal), leave alone.

    // Snapping might land `last` on top of `first` or `prev`; drop the duplicate.
    let n = points.len();
    let new_last = points[n - 1];
    if (new_last - first).norm() < tolerance {
        points.pop();
    } else if (new_last - points[n - 2]).norm() < tolerance {
        points.pop();
    }

    points
}

/// Rotate the vertex list so the first vertex sits at a real corner
/// (incident edges perpendicular). Necessary when the user started their
/// sketch mid-wall — `ParametricPlan` treats the first vertex as the constant
/// anchor, but a mid-wall start vertex is collinear with its neighbours and
/// shows up as a spurious "extra" vertex in the parametric representation.
///
/// Walks the polygon cyclically from index 0 and picks the first corner.
/// Returns the input unchanged if there are no corners (degenerate input).
pub fn rotate_to_corner_start(mut points: Vec<Point2f32>, tolerance: f32) -> Vec<Point2f32> {
    let n = points.len();
    if n < 3 {
        return points;
    }

    for offset in 0..n {
        let prev = points[(offset + n - 1) % n];
        let curr = points[offset];
        let next = points[(offset + 1) % n];

        let prev_horizontal = (prev.y - curr.y).abs() < tolerance;
        let prev_vertical = (prev.x - curr.x).abs() < tolerance;
        let next_horizontal = (curr.y - next.y).abs() < tolerance;
        let next_vertical = (curr.x - next.x).abs() < tolerance;

        let is_corner = (prev_horizontal && next_vertical) || (prev_vertical && next_horizontal);

        if is_corner {
            if offset > 0 {
                points.rotate_left(offset);
            }
            return points;
        }
    }

    // No corner found anywhere (degenerate). Leave alone.
    points
}

/// Tolerance for collinear removal over VERBATIM geometry — a ring that already
/// contains intended outline/curve vertices (the carve/cut sub-area path), where
/// only EXACTLY axis-aligned (≈0 off-axis) vertices should collapse. It must be
/// tight: a curve's first/last facet is nearly tangent to the adjoining straight
/// wall (a small off-axis delta), and a looser tolerance would mistake it for a
/// collinear continuation and drop the curve-start corner. This is distinct from
/// the freehand-orthogonalization callers (`make_orthogonal_at`,
/// `finalize_closed_ring`), which deliberately pass the looser RDP `tolerance` to
/// merge hand-drawn jitter while a stroke is being snapped to the axes.
pub(crate) const EXACT_COLLINEAR_TOL: f32 = 1e-3;

/// Remove vertices whose two incident edges share an axis. Treats the polygon
/// as cyclic so the implicit closing edge from the last vertex back to the
/// first counts. Iterates until stable — chains of 3+ collinear points
/// collapse fully.
pub fn remove_collinear(points: &[Point2f32], tolerance: f32) -> Vec<Point2f32> {
    if points.len() < 3 {
        return points.to_vec();
    }

    let mut result = points.to_vec();

    loop {
        if result.len() < 3 {
            break;
        }
        let n = result.len();
        let mut to_remove: Option<usize> = None;
        for i in 0..n {
            let prev = result[(i + n - 1) % n];
            let curr = result[i];
            let next = result[(i + 1) % n];

            let prev_horizontal = (prev.y - curr.y).abs() < tolerance;
            let prev_vertical = (prev.x - curr.x).abs() < tolerance;
            let next_horizontal = (curr.y - next.y).abs() < tolerance;
            let next_vertical = (curr.x - next.x).abs() < tolerance;

            if (prev_horizontal && next_horizontal) || (prev_vertical && next_vertical) {
                to_remove = Some(i);
                break;
            }
        }

        match to_remove {
            Some(i) => {
                result.remove(i);
            }
            None => break,
        }
    }

    result
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

/// Remove duplicate consecutive points, plus a trailing duplicate of the
/// first vertex if the polygon happens to close on itself exactly.
pub fn remove_duplicates(points: &[Point2f32], tolerance: f32) -> Vec<Point2f32> {
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
    fn snap_closing_edge_aligns_horizontal_close() {
        // Last edge is vertical (same x). Closing edge has y slip → snap y.
        let pts = vec![
            Point2f32::new(0.0, 0.0), // first
            Point2f32::new(0.0, 100.0),
            Point2f32::new(100.0, 100.0),
            Point2f32::new(100.0, 3.0), // last
        ];
        let snapped = snap_closing_edge(pts, 0.5);
        assert_eq!(snapped.len(), 4);
        assert_eq!(snapped[3].y, 0.0);
        assert_eq!(snapped[3].x, 100.0);
    }

    #[test]
    fn snap_closing_edge_aligns_vertical_close() {
        // Last edge is horizontal (same y). Closing edge has x slip → snap x.
        let pts = vec![
            Point2f32::new(0.0, 0.0), // first
            Point2f32::new(100.0, 0.0),
            Point2f32::new(100.0, 100.0),
            Point2f32::new(3.0, 100.0), // last
        ];
        let snapped = snap_closing_edge(pts, 0.5);
        assert_eq!(snapped.len(), 4);
        assert_eq!(snapped[3].x, 0.0);
        assert_eq!(snapped[3].y, 100.0);
    }

    #[test]
    fn snap_closing_edge_drops_duplicate_when_snap_lands_on_first() {
        // Last point already aligned with first; snap would put them on top of each other.
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(0.0, 100.0),
            Point2f32::new(100.0, 100.0),
            Point2f32::new(0.0, 3.0), // last edge horizontal (y=100→3? no, prev is (100,100))
                                      // Actually with prev = (100, 100) and last = (0, 3), prev→last isn't axis-aligned.
                                      // Adjust to make this test meaningful:
        ];
        // Reset with a case where snap creates a duplicate:
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(100.0, 0.0),
            Point2f32::new(0.0, 0.0), // already at first — last edge horizontal, snap x→0 is no-op
        ];
        let snapped = snap_closing_edge(pts, 0.5);
        // The last is already coincident with first; should be popped.
        assert!(snapped.len() < 3 || snapped.last().unwrap() != &Point2f32::new(0.0, 0.0));
    }

    #[test]
    fn snap_near_collinear_walls_merges_offset_walls() {
        // L where the upper bar's left wall (x=95) is 13px off from the lower
        // bar's left wall (x=108). Both should snap to a shared mean.
        let pts = vec![
            Point2f32::new(108.0, 124.0),
            Point2f32::new(108.0, 680.0),
            Point2f32::new(424.0, 680.0),
            Point2f32::new(424.0, 534.0),
            Point2f32::new(245.0, 534.0),
            Point2f32::new(245.0, 35.0),
            Point2f32::new(95.0, 35.0),
            Point2f32::new(95.0, 124.0),
        ];
        let snapped = snap_near_collinear_walls(pts, 20.0);
        // All four "left wall" x-values should now share the same coord.
        let left_xs: Vec<f32> = snapped
            .iter()
            .filter(|p| p.x < 200.0)
            .map(|p| p.x)
            .collect();
        let first = left_xs[0];
        assert!(left_xs.iter().all(|&x| (x - first).abs() < 0.1));
        // And they're between the originals (101.5 = midpoint of 95 and 108).
        assert!((first - 101.5).abs() < 0.5);
    }

    #[test]
    fn snap_near_collinear_walls_leaves_far_walls_alone() {
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(0.0, 100.0),
            Point2f32::new(100.0, 100.0), // 100 away from the left walls
            Point2f32::new(100.0, 0.0),
        ];
        let snapped = snap_near_collinear_walls(pts.clone(), 20.0);
        assert_eq!(snapped, pts);
    }

    #[test]
    fn rotate_to_corner_start_moves_mid_wall_start_to_corner() {
        // L shape where vertex 0 is mid-wall: between (67, -2) and (67, 588).
        let pts = vec![
            Point2f32::new(67.0, 205.0), // <- mid-wall start (collinear with neighbors)
            Point2f32::new(67.0, 588.0),
            Point2f32::new(440.0, 588.0),
            Point2f32::new(440.0, 420.0),
            Point2f32::new(278.0, 420.0),
            Point2f32::new(278.0, -2.0),
            Point2f32::new(67.0, -2.0),
        ];
        let rotated = rotate_to_corner_start(pts, 0.5);
        // Vertex 0 must now be a corner (perpendicular incident edges).
        let n = rotated.len();
        let prev = rotated[n - 1];
        let curr = rotated[0];
        let next = rotated[1];
        let prev_h = (prev.y - curr.y).abs() < 0.5;
        let prev_v = (prev.x - curr.x).abs() < 0.5;
        let next_h = (curr.y - next.y).abs() < 0.5;
        let next_v = (curr.x - next.x).abs() < 0.5;
        assert!(
            (prev_h && next_v) || (prev_v && next_h),
            "new start is a corner"
        );
        // Old start (67, 205) should still be present in the rotated list.
        assert!(rotated
            .iter()
            .any(|p| (p.x - 67.0).abs() < 0.5 && (p.y - 205.0).abs() < 0.5));
    }

    #[test]
    fn rotate_then_remove_collinear_drops_mid_wall_start() {
        // Full pipeline of the closure-finalization steps for the mid-wall start case.
        let pts = vec![
            Point2f32::new(67.0, 205.0), // mid-wall start
            Point2f32::new(67.0, 588.0),
            Point2f32::new(440.0, 588.0),
            Point2f32::new(440.0, 420.0),
            Point2f32::new(278.0, 420.0),
            Point2f32::new(278.0, -2.0),
            Point2f32::new(67.0, -2.0),
        ];
        let rotated = rotate_to_corner_start(pts, 0.5);
        let cleaned = remove_collinear(&rotated, 0.5);
        // The old mid-wall start should be gone, leaving the 6 real corners of the L.
        assert_eq!(cleaned.len(), 6);
        assert!(!cleaned
            .iter()
            .any(|p| (p.x - 67.0).abs() < 0.5 && (p.y - 205.0).abs() < 0.5));
    }

    #[test]
    fn rotate_to_corner_start_no_op_when_already_at_corner() {
        // Vertex 0 is already a corner — no rotation needed.
        let pts = vec![
            Point2f32::new(0.0, 0.0), // top-left corner
            Point2f32::new(100.0, 0.0),
            Point2f32::new(100.0, 100.0),
            Point2f32::new(0.0, 100.0),
        ];
        let rotated = rotate_to_corner_start(pts.clone(), 0.5);
        assert_eq!(rotated, pts);
    }

    #[test]
    fn remove_collinear_collapses_wall_wobble() {
        // Two consecutive vertical edges at x=10 with a redundant vertex between them.
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 5.0), // <- redundant, sits on the right wall
            Point2f32::new(10.0, 10.0),
            Point2f32::new(0.0, 10.0),
        ];
        let cleaned = remove_collinear(&pts, 0.5);
        assert_eq!(cleaned.len(), 4);
        assert!(!cleaned
            .iter()
            .any(|p| (p.x - 10.0).abs() < 0.5 && (p.y - 5.0).abs() < 0.5));
    }

    #[test]
    fn remove_collinear_preserves_l_shape() {
        // An L has no collinear interior vertices — every corner has perpendicular edges.
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(100.0, 0.0),
            Point2f32::new(100.0, 50.0),
            Point2f32::new(50.0, 50.0),
            Point2f32::new(50.0, 100.0),
            Point2f32::new(0.0, 100.0),
        ];
        let cleaned = remove_collinear(&pts, 0.5);
        assert_eq!(cleaned.len(), 6);
    }

    #[test]
    fn remove_collinear_collapses_long_chain() {
        // Five collinear points along the bottom — three are redundant.
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(25.0, 0.0),
            Point2f32::new(50.0, 0.0),
            Point2f32::new(75.0, 0.0),
            Point2f32::new(100.0, 0.0),
            Point2f32::new(100.0, 100.0),
            Point2f32::new(0.0, 100.0),
        ];
        let cleaned = remove_collinear(&pts, 0.5);
        assert_eq!(cleaned.len(), 4);
    }

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
