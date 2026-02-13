//! Snapping functionality for connecting new strokes to existing geometry

use crate::{Point, Line, Polygon, Error};
use rstar::{RTree, AABB, RTreeObject, PointDistance};

const DEFAULT_SNAP_DISTANCE: f64 = 5.0;

/// A snap target (point on an edge)
#[derive(Debug, Clone)]
pub struct SnapTarget {
    pub point: Point,
    pub edge_index: usize,
    pub distance_along_edge: f64, // 0.0 to 1.0
    pub vertex_snap: bool, // True if snapping to a vertex, false if on edge
}

/// Snap a point to the nearest location on a polygon
pub fn snap_to_polygon(
    point: &Point,
    polygon: &Polygon,
    max_distance: f64,
) -> Option<SnapTarget> {
    let mut best_target: Option<SnapTarget> = None;
    let mut best_distance = max_distance;
    
    // Check snapping to vertices first (higher priority)
    for (i, vertex) in polygon.vertices.iter().enumerate() {
        let dist = point.distance_to(vertex);
        if dist < best_distance {
            best_distance = dist;
            best_target = Some(SnapTarget {
                point: *vertex,
                edge_index: i,
                distance_along_edge: 0.0,
                vertex_snap: true,
            });
        }
    }
    
    // Check snapping to edges
    for (i, edge) in polygon.edges().iter().enumerate() {
        if let Some((snap_point, t)) = snap_to_line_segment(point, edge, max_distance) {
            let dist = point.distance_to(&snap_point);
            if dist < best_distance {
                best_distance = dist;
                best_target = Some(SnapTarget {
                    point: snap_point,
                    edge_index: i,
                    distance_along_edge: t,
                    vertex_snap: false,
                });
            }
        }
    }
    
    best_target
}

/// Snap a point to a line segment
fn snap_to_line_segment(
    point: &Point,
    line: &Line,
    max_distance: f64,
) -> Option<(Point, f64)> {
    let dx = line.end.x - line.start.x;
    let dy = line.end.y - line.start.y;
    let length_squared = dx * dx + dy * dy;
    
    if length_squared < 1e-10 {
        return None;
    }
    
    // Calculate projection parameter t
    let t = ((point.x - line.start.x) * dx + (point.y - line.start.y) * dy) / length_squared;
    
    // Clamp t to [0, 1] to stay on the segment
    let t = t.clamp(0.0, 1.0);
    
    // Calculate the point on the segment
    let snap_point = Point::new(
        line.start.x + t * dx,
        line.start.y + t * dy,
    );
    
    let distance = point.distance_to(&snap_point);
    
    if distance <= max_distance {
        Some((snap_point, t))
    } else {
        None
    }
}

/// Find where a stroke intersects with polygon edges
pub fn find_intersection_points(
    stroke_points: &[Point],
    polygon: &Polygon,
) -> Vec<(usize, Point, usize)> {
    let mut intersections = Vec::new();
    
    // Check each stroke segment against each polygon edge
    for (stroke_idx, window) in stroke_points.windows(2).enumerate() {
        let stroke_line = Line::new(window[0], window[1]);
        
        for (edge_idx, edge) in polygon.edges().iter().enumerate() {
            if let Some(intersection) = line_intersection(&stroke_line, edge) {
                intersections.push((stroke_idx, intersection, edge_idx));
            }
        }
    }
    
    intersections
}

/// Calculate intersection point between two line segments
fn line_intersection(line1: &Line, line2: &Line) -> Option<Point> {
    let x1 = line1.start.x;
    let y1 = line1.start.y;
    let x2 = line1.end.x;
    let y2 = line1.end.y;
    
    let x3 = line2.start.x;
    let y3 = line2.start.y;
    let x4 = line2.end.x;
    let y4 = line2.end.y;
    
    let denom = (x1 - x2) * (y3 - y4) - (y1 - y2) * (x3 - x4);
    
    if denom.abs() < 1e-10 {
        return None; // Lines are parallel
    }
    
    let t = ((x1 - x3) * (y3 - y4) - (y1 - y3) * (x3 - x4)) / denom;
    let u = -((x1 - x2) * (y1 - y3) - (y1 - y2) * (x1 - x3)) / denom;
    
    if t >= 0.0 && t <= 1.0 && u >= 0.0 && u <= 1.0 {
        let x = x1 + t * (x2 - x1);
        let y = y1 + t * (y2 - y1);
        Some(Point::new(x, y))
    } else {
        None
    }
}

/// Snap multiple points of a stroke to a polygon
pub fn snap_stroke_to_polygon(
    stroke_points: &[Point],
    polygon: &Polygon,
    snap_distance: f64,
) -> Vec<Point> {
    stroke_points.iter()
        .map(|point| {
            if let Some(target) = snap_to_polygon(point, polygon, snap_distance) {
                target.point
            } else {
                *point
            }
        })
        .collect()
}

/// Find the closest edge to a point
pub fn find_closest_edge(point: &Point, polygon: &Polygon) -> Option<(usize, f64)> {
    let mut closest_edge = None;
    let mut min_distance = f64::INFINITY;
    
    for (i, edge) in polygon.edges().iter().enumerate() {
        if let Some((_, t)) = snap_to_line_segment(point, &edge, f64::INFINITY) {
            let snap_point = Point::new(
                edge.start.x + t * (edge.end.x - edge.start.x),
                edge.start.y + t * (edge.end.y - edge.start.y),
            );
            let dist = point.distance_to(&snap_point);
            
            if dist < min_distance {
                min_distance = dist;
                closest_edge = Some((i, t));
            }
        }
    }
    
    closest_edge
}

/// Get the point at a specific distance along an edge
pub fn point_along_edge(polygon: &Polygon, edge_index: usize, t: f64) -> Option<Point> {
    let edge = polygon.edge(edge_index)?;
    let t = t.clamp(0.0, 1.0);
    
    Some(Point::new(
        edge.start.x + t * (edge.end.x - edge.start.x),
        edge.start.y + t * (edge.end.y - edge.start.y),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_snap_to_line_segment() {
        let line = Line::new(Point::new(0.0, 0.0), Point::new(10.0, 0.0));
        let point = Point::new(5.0, 1.0);
        
        let result = snap_to_line_segment(&point, &line, 2.0);
        assert!(result.is_some());
        
        let (snap_point, t) = result.unwrap();
        assert!((snap_point.x - 5.0).abs() < 0.01);
        assert!((snap_point.y - 0.0).abs() < 0.01);
        assert!((t - 0.5).abs() < 0.01);
    }
    
    #[test]
    fn test_line_intersection() {
        let line1 = Line::new(Point::new(0.0, 0.0), Point::new(10.0, 10.0));
        let line2 = Line::new(Point::new(0.0, 10.0), Point::new(10.0, 0.0));
        
        let intersection = line_intersection(&line1, &line2);
        assert!(intersection.is_some());
        
        let point = intersection.unwrap();
        assert!((point.x - 5.0).abs() < 0.01);
        assert!((point.y - 5.0).abs() < 0.01);
    }
}
