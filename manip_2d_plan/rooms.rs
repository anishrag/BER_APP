//! Room detection and labeling within floor plans

use crate::{Polygon, Point, Line, Error};
use geo::{Area, Contains};
use geo_types::{Coord, MultiPolygon};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A detected room within the floor plan
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Room {
    /// The boundary of the room
    pub boundary: Polygon,
    
    /// Optional label for the room
    pub label: Option<String>,
    
    /// Area of the room
    pub area: f64,
    
    /// Center point of the room
    pub center: Point,
}

impl Room {
    pub fn new(boundary: Polygon) -> Self {
        let area = boundary.area();
        let center = calculate_centroid(&boundary);
        
        Self {
            boundary,
            label: None,
            area,
            center,
        }
    }
    
    pub fn with_label(mut self, label: String) -> Self {
        self.label = Some(label);
        self
    }
}

/// Detect rooms within a floor plan polygon
/// This is a simplified version - a real implementation would need internal walls
pub fn detect_rooms(polygon: &Polygon) -> Result<Vec<Room>, Error> {
    // For now, we'll just create a single room from the entire polygon
    // In a real implementation, this would:
    // 1. Take internal wall segments as input
    // 2. Subdivide the polygon into rooms
    // 3. Use connectivity analysis to find separate spaces
    
    let room = Room::new(polygon.clone());
    Ok(vec![room])
}

/// Detect rooms given internal walls
pub fn detect_rooms_with_walls(
    exterior: &Polygon,
    internal_walls: &[Line],
) -> Result<Vec<Room>, Error> {
    // This would implement a proper room segmentation algorithm
    // For now, return a placeholder
    let room = Room::new(exterior.clone());
    Ok(vec![room])
}

/// Calculate the centroid of a polygon
fn calculate_centroid(polygon: &Polygon) -> Point {
    if polygon.vertices.is_empty() {
        return Point::new(0.0, 0.0);
    }
    
    let sum_x: f64 = polygon.vertices.iter().map(|p| p.x).sum();
    let sum_y: f64 = polygon.vertices.iter().map(|p| p.y).sum();
    let count = polygon.vertices.len() as f64;
    
    Point::new(sum_x / count, sum_y / count)
}

/// Check if a point is inside a room
pub fn point_in_room(point: &Point, room: &Room) -> bool {
    use geo::Contains;
    let geo_poly = room.boundary.to_geo_polygon();
    let geo_point = geo_types::Point::new(point.x, point.y);
    geo_poly.contains(&geo_point)
}

/// Find which room contains a given point
pub fn find_room_containing_point(point: &Point, rooms: &[Room]) -> Option<usize> {
    rooms.iter()
        .position(|room| point_in_room(point, room))
}

/// Calculate the minimum bounding rectangle for a room
pub fn room_bounds(room: &Room) -> (Point, Point) {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    
    for vertex in &room.boundary.vertices {
        min_x = min_x.min(vertex.x);
        min_y = min_y.min(vertex.y);
        max_x = max_x.max(vertex.x);
        max_y = max_y.max(vertex.y);
    }
    
    (Point::new(min_x, min_y), Point::new(max_x, max_y))
}

/// Suggest room types based on dimensions and connectivity
pub fn suggest_room_type(room: &Room) -> Vec<String> {
    let (min, max) = room_bounds(room);
    let width = max.x - min.x;
    let height = max.y - min.y;
    let area = room.area;
    let aspect_ratio = width.max(height) / width.min(height);
    
    let mut suggestions = Vec::new();
    
    // Simple heuristics for room type suggestions
    if area < 10.0 {
        suggestions.push("Closet".to_string());
        suggestions.push("Bathroom".to_string());
    } else if area < 30.0 {
        suggestions.push("Bathroom".to_string());
        suggestions.push("Office".to_string());
        suggestions.push("Bedroom".to_string());
    } else if area < 50.0 {
        suggestions.push("Bedroom".to_string());
        suggestions.push("Office".to_string());
        suggestions.push("Dining Room".to_string());
    } else {
        suggestions.push("Living Room".to_string());
        suggestions.push("Great Room".to_string());
        suggestions.push("Master Bedroom".to_string());
    }
    
    // Consider aspect ratio
    if aspect_ratio > 2.0 {
        suggestions.push("Hallway".to_string());
        suggestions.push("Corridor".to_string());
    }
    
    suggestions
}

/// Calculate adjacency between rooms (which rooms share walls)
pub fn calculate_room_adjacency(rooms: &[Room]) -> Vec<Vec<usize>> {
    let mut adjacency = vec![Vec::new(); rooms.len()];
    
    for i in 0..rooms.len() {
        for j in (i + 1)..rooms.len() {
            if rooms_are_adjacent(&rooms[i], &rooms[j]) {
                adjacency[i].push(j);
                adjacency[j].push(i);
            }
        }
    }
    
    adjacency
}

/// Check if two rooms share a wall
fn rooms_are_adjacent(room1: &Room, room2: &Room) -> bool {
    // Check if any edges are shared or very close
    let tolerance = 1.0;
    
    for edge1 in room1.boundary.edges() {
        for edge2 in room2.boundary.edges() {
            if edges_overlap(&edge1, &edge2, tolerance) {
                return true;
            }
        }
    }
    
    false
}

/// Check if two edges overlap (share a portion)
fn edges_overlap(edge1: &Line, edge2: &Line, tolerance: f64) -> bool {
    // Check if edges are nearly collinear
    let dist1 = point_to_line_distance(&edge2.start, edge1);
    let dist2 = point_to_line_distance(&edge2.end, edge1);
    
    if dist1 > tolerance || dist2 > tolerance {
        return false;
    }
    
    // Check if they overlap along the line
    // This is a simplified check
    let min1_x = edge1.start.x.min(edge1.end.x);
    let max1_x = edge1.start.x.max(edge1.end.x);
    let min2_x = edge2.start.x.min(edge2.end.x);
    let max2_x = edge2.start.x.max(edge2.end.x);
    
    let x_overlap = min1_x <= max2_x && min2_x <= max1_x;
    
    let min1_y = edge1.start.y.min(edge1.end.y);
    let max1_y = edge1.start.y.max(edge1.end.y);
    let min2_y = edge2.start.y.min(edge2.end.y);
    let max2_y = edge2.start.y.max(edge2.end.y);
    
    let y_overlap = min1_y <= max2_y && min2_y <= max1_y;
    
    x_overlap && y_overlap
}

fn point_to_line_distance(point: &Point, line: &Line) -> f64 {
    let dx = line.end.x - line.start.x;
    let dy = line.end.y - line.start.y;
    let length = (dx * dx + dy * dy).sqrt();
    
    if length < 1e-10 {
        return point.distance_to(&line.start);
    }
    
    let num = ((point.x - line.start.x) * dy - (point.y - line.start.y) * dx).abs();
    num / length
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_calculate_centroid() {
        let polygon = Polygon::new(vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ]);
        
        let centroid = calculate_centroid(&polygon);
        assert!((centroid.x - 5.0).abs() < 0.01);
        assert!((centroid.y - 5.0).abs() < 0.01);
    }
    
    #[test]
    fn test_room_bounds() {
        let polygon = Polygon::new(vec![
            Point::new(1.0, 2.0),
            Point::new(11.0, 2.0),
            Point::new(11.0, 12.0),
            Point::new(1.0, 12.0),
        ]);
        
        let room = Room::new(polygon);
        let (min, max) = room_bounds(&room);
        
        assert_eq!(min.x, 1.0);
        assert_eq!(min.y, 2.0);
        assert_eq!(max.x, 11.0);
        assert_eq!(max.y, 12.0);
    }
}
