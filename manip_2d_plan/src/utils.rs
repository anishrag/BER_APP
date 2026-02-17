//! Utility functions for geometric calculations

use crate::{Point, Line, Polygon};

/// Calculate the angle between three points (in radians)
pub fn angle_between_points(p1: &Point, vertex: &Point, p2: &Point) -> f64 {
    let dx1 = p1.x - vertex.x;
    let dy1 = p1.y - vertex.y;
    let dx2 = p2.x - vertex.x;
    let dy2 = p2.y - vertex.y;
    
    let angle1 = dy1.atan2(dx1);
    let angle2 = dy2.atan2(dx2);
    
    let mut angle = angle2 - angle1;
    
    // Normalize to [0, 2π]
    while angle < 0.0 {
        angle += 2.0 * std::f64::consts::PI;
    }
    while angle >= 2.0 * std::f64::consts::PI {
        angle -= 2.0 * std::f64::consts::PI;
    }
    
    angle
}

/// Calculate the signed area of a polygon (positive if CCW, negative if CW)
pub fn signed_area(polygon: &Polygon) -> f64 {
    let mut area = 0.0;
    let n = polygon.vertices.len();
    
    for i in 0..n {
        let j = (i + 1) % n;
        area += polygon.vertices[i].x * polygon.vertices[j].y;
        area -= polygon.vertices[j].x * polygon.vertices[i].y;
    }
    
    area / 2.0
}

/// Check if a polygon is counter-clockwise
pub fn is_ccw(polygon: &Polygon) -> bool {
    signed_area(polygon) > 0.0
}

/// Reverse the order of vertices in a polygon
pub fn reverse_polygon(polygon: &Polygon) -> Polygon {
    let mut reversed = polygon.vertices.clone();
    reversed.reverse();
    Polygon::new(reversed)
}

/// Ensure a polygon is counter-clockwise
pub fn ensure_ccw(polygon: &Polygon) -> Polygon {
    if is_ccw(polygon) {
        polygon.clone()
    } else {
        reverse_polygon(polygon)
    }
}

/// Calculate the perimeter of a polygon
pub fn perimeter(polygon: &Polygon) -> f64 {
    polygon.edges().iter().map(|e| e.length()).sum()
}

/// Check if two lines are approximately parallel
pub fn are_parallel(line1: &Line, line2: &Line, angle_tolerance: f64) -> bool {
    let angle1 = line1.angle();
    let angle2 = line2.angle();
    
    let diff = (angle1 - angle2).abs();
    let normalized_diff = diff.min(std::f64::consts::PI - diff);
    
    normalized_diff < angle_tolerance
}

/// Check if two lines are approximately perpendicular
pub fn are_perpendicular(line1: &Line, line2: &Line, angle_tolerance: f64) -> bool {
    let angle1 = line1.angle();
    let angle2 = line2.angle();
    
    let diff = (angle1 - angle2).abs();
    let perpendicular_diff = (diff - std::f64::consts::PI / 2.0).abs();
    
    perpendicular_diff < angle_tolerance
}

/// Calculate the distance from a point to a line segment
pub fn distance_to_segment(point: &Point, line: &Line) -> f64 {
    let dx = line.end.x - line.start.x;
    let dy = line.end.y - line.start.y;
    let length_squared = dx * dx + dy * dy;
    
    if length_squared < 1e-10 {
        return point.distance_to(&line.start);
    }
    
    let t = ((point.x - line.start.x) * dx + (point.y - line.start.y) * dy) / length_squared;
    let t_clamped = t.clamp(0.0, 1.0);
    
    let closest = Point::new(
        line.start.x + t_clamped * dx,
        line.start.y + t_clamped * dy,
    );
    
    point.distance_to(&closest)
}

/// Find the closest point on a line segment to a given point
pub fn closest_point_on_segment(point: &Point, line: &Line) -> Point {
    let dx = line.end.x - line.start.x;
    let dy = line.end.y - line.start.y;
    let length_squared = dx * dx + dy * dy;
    
    if length_squared < 1e-10 {
        return line.start;
    }
    
    let t = ((point.x - line.start.x) * dx + (point.y - line.start.y) * dy) / length_squared;
    let t_clamped = t.clamp(0.0, 1.0);
    
    Point::new(
        line.start.x + t_clamped * dx,
        line.start.y + t_clamped * dy,
    )
}

/// Interpolate between two points
pub fn lerp(p1: &Point, p2: &Point, t: f64) -> Point {
    Point::new(
        p1.x + t * (p2.x - p1.x),
        p1.y + t * (p2.y - p1.y),
    )
}

/// Calculate the center of a bounding box
pub fn bounds_center(min: &Point, max: &Point) -> Point {
    Point::new(
        (min.x + max.x) / 2.0,
        (min.y + max.y) / 2.0,
    )
}

/// Rotate a point around an origin
pub fn rotate_point(point: &Point, origin: &Point, angle: f64) -> Point {
    let cos_a = angle.cos();
    let sin_a = angle.sin();
    
    let dx = point.x - origin.x;
    let dy = point.y - origin.y;
    
    Point::new(
        origin.x + dx * cos_a - dy * sin_a,
        origin.y + dx * sin_a + dy * cos_a,
    )
}

/// Scale a point relative to an origin
pub fn scale_point(point: &Point, origin: &Point, scale: f64) -> Point {
    Point::new(
        origin.x + (point.x - origin.x) * scale,
        origin.y + (point.y - origin.y) * scale,
    )
}

/// Calculate the convex hull of a set of points (Gift wrapping algorithm)
pub fn convex_hull(points: &[Point]) -> Vec<Point> {
    if points.len() < 3 {
        return points.to_vec();
    }
    
    // Find the leftmost point
    let mut leftmost_idx = 0;
    for i in 1..points.len() {
        if points[i].x < points[leftmost_idx].x ||
           (points[i].x == points[leftmost_idx].x && points[i].y < points[leftmost_idx].y) {
            leftmost_idx = i;
        }
    }
    
    let mut hull = Vec::new();
    let mut current = leftmost_idx;
    
    loop {
        hull.push(points[current]);
        let mut next = (current + 1) % points.len();
        
        for i in 0..points.len() {
            if orientation(&points[current], &points[i], &points[next]) == 2 {
                next = i;
            }
        }
        
        current = next;
        
        if current == leftmost_idx {
            break;
        }
    }
    
    hull
}

/// Calculate orientation of ordered triplet (p, q, r)
/// Returns: 0 = collinear, 1 = clockwise, 2 = counterclockwise
fn orientation(p: &Point, q: &Point, r: &Point) -> i32 {
    let val = (q.y - p.y) * (r.x - q.x) - (q.x - p.x) * (r.y - q.y);
    
    if val.abs() < 1e-10 {
        0 // Collinear
    } else if val > 0.0 {
        1 // Clockwise
    } else {
        2 // Counterclockwise
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_signed_area() {
        let polygon = Polygon::new(vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ]);
        
        let area = signed_area(&polygon);
        assert!((area - 100.0).abs() < 0.01);
    }
    
    #[test]
    fn test_are_parallel() {
        let line1 = Line::new(Point::new(0.0, 0.0), Point::new(10.0, 0.0));
        let line2 = Line::new(Point::new(0.0, 5.0), Point::new(10.0, 5.0));
        
        assert!(are_parallel(&line1, &line2, 0.1));
    }
    
    #[test]
    fn test_are_perpendicular() {
        let line1 = Line::new(Point::new(0.0, 0.0), Point::new(10.0, 0.0));
        let line2 = Line::new(Point::new(5.0, 0.0), Point::new(5.0, 10.0));
        
        assert!(are_perpendicular(&line1, &line2, 0.1));
    }
}
