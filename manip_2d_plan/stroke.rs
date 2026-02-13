//! Stroke handling for user input

use crate::{Point, Error};

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A stroke drawn by the user
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Stroke {
    pub points: Vec<Point>,
}

impl Stroke {
    /// Create a new stroke from a list of points
    pub fn new(points: Vec<Point>) -> Self {
        Self { points }
    }
    
    /// Get the number of points in the stroke
    pub fn len(&self) -> usize {
        self.points.len()
    }
    
    /// Check if the stroke is empty
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
    
    /// Check if the stroke forms a closed loop (start and end are close)
    pub fn is_closed(&self, tolerance: f64) -> bool {
        if self.points.len() < 3 {
            return false;
        }
        
        let first = self.points.first().unwrap();
        let last = self.points.last().unwrap();
        
        first.distance_to(last) < tolerance
    }
    
    /// Force close the stroke by adding the first point at the end if needed
    pub fn force_close(&mut self, tolerance: f64) {
        if !self.is_closed(tolerance) && !self.points.is_empty() {
            let first = self.points[0];
            self.points.push(first);
        }
    }
    
    /// Calculate the approximate bounding box
    pub fn bounds(&self) -> Option<(Point, Point)> {
        if self.points.is_empty() {
            return None;
        }
        
        let mut min_x = f64::INFINITY;
        let mut min_y = f64::INFINITY;
        let mut max_x = f64::NEG_INFINITY;
        let mut max_y = f64::NEG_INFINITY;
        
        for point in &self.points {
            min_x = min_x.min(point.x);
            min_y = min_y.min(point.y);
            max_x = max_x.max(point.x);
            max_y = max_y.max(point.y);
        }
        
        Some((Point::new(min_x, min_y), Point::new(max_x, max_y)))
    }
    
    /// Resample the stroke to have approximately evenly spaced points
    pub fn resample(&self, spacing: f64) -> Result<Self, Error> {
        if self.points.len() < 2 {
            return Err(Error::InsufficientPoints);
        }
        
        let mut resampled = vec![self.points[0]];
        let mut accumulated = 0.0;
        
        for i in 0..self.points.len() - 1 {
            let current = self.points[i];
            let next = self.points[i + 1];
            let segment_length = current.distance_to(&next);
            
            if segment_length < 1e-10 {
                continue;
            }
            
            let dx = (next.x - current.x) / segment_length;
            let dy = (next.y - current.y) / segment_length;
            
            accumulated += segment_length;
            
            while accumulated >= spacing {
                let prev = resampled.last().unwrap();
                let new_point = Point::new(
                    prev.x + dx * spacing,
                    prev.y + dy * spacing,
                );
                resampled.push(new_point);
                accumulated -= spacing;
            }
        }
        
        // Always include the last point
        resampled.push(*self.points.last().unwrap());
        
        Ok(Stroke::new(resampled))
    }
    
    /// Calculate the total length of the stroke
    pub fn total_length(&self) -> f64 {
        if self.points.len() < 2 {
            return 0.0;
        }
        
        self.points.windows(2)
            .map(|w| w[0].distance_to(&w[1]))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_stroke_closed() {
        let points = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 0.0),
        ];
        let stroke = Stroke::new(points);
        assert!(stroke.is_closed(0.1));
    }
    
    #[test]
    fn test_stroke_not_closed() {
        let points = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
        ];
        let stroke = Stroke::new(points);
        assert!(!stroke.is_closed(0.1));
    }
}
