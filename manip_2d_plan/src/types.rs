//! Core geometric types used throughout the library

use geo_types::{Coord, LineString, Polygon as GeoPolygon};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A 2D point
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    pub fn distance_to(&self, other: &Point) -> f64 {
        let dx = self.x - other.x;
        let dy = self.y - other.y;
        (dx * dx + dy * dy).sqrt()
    }

    pub fn to_coord(&self) -> Coord {
        Coord {
            x: self.x,
            y: self.y,
        }
    }
}

impl From<Coord> for Point {
    fn from(coord: Coord) -> Self {
        Point::new(coord.x, coord.y)
    }
}

/// A line segment between two points
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Line {
    pub start: Point,
    pub end: Point,
}

impl Line {
    pub fn new(start: Point, end: Point) -> Self {
        Self { start, end }
    }

    pub fn length(&self) -> f64 {
        self.start.distance_to(&self.end)
    }

    pub fn angle(&self) -> f64 {
        (self.end.y - self.start.y).atan2(self.end.x - self.start.x)
    }

    /// Returns true if this line is approximately horizontal
    pub fn is_horizontal(&self, tolerance: f64) -> bool {
        (self.end.y - self.start.y).abs() < tolerance
    }

    /// Returns true if this line is approximately vertical
    pub fn is_vertical(&self, tolerance: f64) -> bool {
        (self.end.x - self.start.x).abs() < tolerance
    }

    /// Returns the direction vector (normalized)
    pub fn direction(&self) -> (f64, f64) {
        let dx = self.end.x - self.start.x;
        let dy = self.end.y - self.start.y;
        let len = self.length();
        if len < 1e-10 {
            (0.0, 0.0)
        } else {
            (dx / len, dy / len)
        }
    }
}

/// A closed polygon
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Polygon {
    pub vertices: Vec<Point>,
}

impl Polygon {
    pub fn new(vertices: Vec<Point>) -> Self {
        Self { vertices }
    }

    /// Get the number of vertices
    pub fn vertex_count(&self) -> usize {
        self.vertices.len()
    }

    /// Get an edge by index
    pub fn edge(&self, index: usize) -> Option<Line> {
        if index >= self.vertices.len() {
            return None;
        }
        let next = (index + 1) % self.vertices.len();
        Some(Line::new(self.vertices[index], self.vertices[next]))
    }

    /// Get all edges
    pub fn edges(&self) -> Vec<Line> {
        (0..self.vertices.len())
            .filter_map(|i| self.edge(i))
            .collect()
    }

    /// Convert to geo_types::Polygon for boolean operations
    pub fn to_geo_polygon(&self) -> GeoPolygon {
        let coords: Vec<Coord> = self.vertices.iter().map(|p| p.to_coord()).collect();

        let mut line_string_coords = coords;
        // Close the polygon if not already closed
        if let (Some(first), Some(last)) = (line_string_coords.first(), line_string_coords.last()) {
            if (first.x - last.x).abs() > 1e-10 || (first.y - last.y).abs() > 1e-10 {
                line_string_coords.push(*first);
            }
        }

        GeoPolygon::new(LineString::from(line_string_coords), vec![])
    }

    /// Create from geo_types::Polygon
    pub fn from_geo_polygon(poly: &GeoPolygon) -> Self {
        let vertices: Vec<Point> = poly
            .exterior()
            .points()
            .take(poly.exterior().coords_count() - 1) // Don't include the duplicate closing point
            .map(|p| Point::new(p.x(), p.y()))
            .collect();

        Self { vertices }
    }

    /// Calculate the area of the polygon
    pub fn area(&self) -> f64 {
        use geo::Area;
        self.to_geo_polygon().unsigned_area()
    }

    /// Check if the polygon is valid (non-self-intersecting, etc.)
    pub fn is_valid(&self) -> bool {
        if self.vertices.len() < 3 {
            return false;
        }
        // Could add more sophisticated checks here
        true
    }
}

/// Orientation of edges (for orthogonal constraints)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum Orientation {
    Horizontal,
    Vertical,
}
