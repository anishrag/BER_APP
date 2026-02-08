/// Module for processing sketch strokes (simplification, straightening, etc.)

/// Represents a simplified/processed floor plan
#[derive(Debug, Clone)]
pub struct ProcessedFloorPlan {
    pub original_points: Vec<(f32, f32)>,
    pub simplified_points: Vec<(f32, f32)>,
    pub is_closed: bool,
}

pub struct SketchProcessor {
    // Configuration for processing
    pub simplification_tolerance: f32,
    pub angle_snap_threshold: f32, // Degrees - snap to horizontal/vertical
}

impl Default for SketchProcessor {
    fn default() -> Self {
        Self {
            simplification_tolerance: 5.0,
            angle_snap_threshold: 10.0,
        }
    }
}

impl SketchProcessor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Process a closed floor plan stroke
    /// Simplifies the stroke and straightens lines to horizontal/vertical where appropriate
    pub fn process_floor_plan(&self, points: Vec<(f32, f32)>) -> ProcessedFloorPlan {
        println!("Processing floor plan with {} points", points.len());

        // Step 1: Simplify using Ramer-Douglas-Peucker algorithm
        let simplified = self.simplify_stroke(&points, self.simplification_tolerance);
        println!("  Simplified to {} points", simplified.len());

        // Step 2: Straighten lines (snap to horizontal/vertical)
        let straightened = self.straighten_lines(&simplified, self.angle_snap_threshold);
        println!("  Straightened {} segments", straightened.len() - 1);

        ProcessedFloorPlan {
            original_points: points,
            simplified_points: straightened,
            is_closed: true,
        }
    }

    /// Simplify a stroke using Ramer-Douglas-Peucker algorithm
    fn simplify_stroke(&self, points: &[(f32, f32)], tolerance: f32) -> Vec<(f32, f32)> {
        if points.len() <= 2 {
            return points.to_vec();
        }

        self.rdp_simplify(points, tolerance)
    }

    /// Ramer-Douglas-Peucker line simplification algorithm
    fn rdp_simplify(&self, points: &[(f32, f32)], epsilon: f32) -> Vec<(f32, f32)> {
        if points.len() <= 2 {
            return points.to_vec();
        }

        // Find the point with maximum distance from line between first and last
        let (max_dist, max_idx) = self.find_farthest_point(points);

        // If max distance is greater than epsilon, recursively simplify
        if max_dist > epsilon {
            // Recursively simplify the two segments
            let mut left = self.rdp_simplify(&points[..=max_idx], epsilon);
            let right = self.rdp_simplify(&points[max_idx..], epsilon);

            // Remove duplicate point at junction
            left.pop();
            left.extend(right);
            left
        } else {
            // All points between first and last can be removed
            vec![points[0], points[points.len() - 1]]
        }
    }

    /// Find the point farthest from the line between first and last points
    fn find_farthest_point(&self, points: &[(f32, f32)]) -> (f32, usize) {
        if points.len() <= 2 {
            return (0.0, 0);
        }

        let first = points[0];
        let last = points[points.len() - 1];

        let mut max_dist = 0.0;
        let mut max_idx = 0;

        for (i, &point) in points.iter().enumerate().skip(1).take(points.len() - 2) {
            let dist = self.perpendicular_distance(point, first, last);
            if dist > max_dist {
                max_dist = dist;
                max_idx = i;
            }
        }

        (max_dist, max_idx)
    }

    /// Calculate perpendicular distance from point to line segment
    fn perpendicular_distance(
        &self,
        point: (f32, f32),
        line_start: (f32, f32),
        line_end: (f32, f32),
    ) -> f32 {
        let (px, py) = point;
        let (x1, y1) = line_start;
        let (x2, y2) = line_end;

        let dx = x2 - x1;
        let dy = y2 - y1;

        if dx == 0.0 && dy == 0.0 {
            // Line start and end are the same point
            return ((px - x1).powi(2) + (py - y1).powi(2)).sqrt();
        }

        let numerator = ((dy * px - dx * py + x2 * y1 - y2 * x1).abs());
        let denominator = (dx.powi(2) + dy.powi(2)).sqrt();

        numerator / denominator
    }

    /// Straighten lines by snapping to horizontal/vertical where appropriate
    fn straighten_lines(
        &self,
        points: &[(f32, f32)],
        angle_threshold_degrees: f32,
    ) -> Vec<(f32, f32)> {
        if points.len() <= 2 {
            return points.to_vec();
        }

        let mut result = vec![points[0]];

        for i in 1..points.len() {
            let prev = result[result.len() - 1];
            let curr = points[i];

            let dx = curr.0 - prev.0;
            let dy = curr.1 - prev.1;

            // Calculate angle from horizontal
            let angle_rad = dy.atan2(dx);
            let angle_deg = angle_rad.to_degrees();

            // Snap to horizontal (0° or 180°)
            if angle_deg.abs() < angle_threshold_degrees
                || (180.0 - angle_deg.abs()) < angle_threshold_degrees
            {
                result.push((curr.0, prev.1)); // Keep x, use prev y
            }
            // Snap to vertical (90° or -90°)
            else if (90.0 - angle_deg.abs()).abs() < angle_threshold_degrees {
                result.push((prev.0, curr.1)); // Keep y, use prev x
            }
            // Keep as-is (diagonal)
            else {
                result.push(curr);
            }
        }

        result
    }

    /// Calculate the total length of a stroke
    pub fn calculate_length(&self, points: &[(f32, f32)]) -> f32 {
        let mut total = 0.0;
        for window in points.windows(2) {
            let dx = window[1].0 - window[0].0;
            let dy = window[1].1 - window[0].1;
            total += (dx.powi(2) + dy.powi(2)).sqrt();
        }
        total
    }

    /// Calculate the area of a closed polygon
    pub fn calculate_area(&self, points: &[(f32, f32)]) -> f32 {
        if points.len() < 3 {
            return 0.0;
        }

        let mut area = 0.0;
        for i in 0..points.len() {
            let j = (i + 1) % points.len();
            area += points[i].0 * points[j].1;
            area -= points[j].0 * points[i].1;
        }
        area.abs() / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simplify_line() {
        let processor = SketchProcessor::new();
        let points = vec![(0.0, 0.0), (1.0, 0.1), (2.0, -0.1), (3.0, 0.0)];

        let simplified = processor.simplify_stroke(&points, 0.5);
        assert_eq!(simplified.len(), 2); // Should simplify to just start and end
    }

    #[test]
    fn test_straighten_horizontal() {
        let processor = SketchProcessor::new();
        let points = vec![
            (0.0, 0.0),
            (100.0, 5.0), // Almost horizontal
        ];

        let straightened = processor.straighten_lines(&points, 10.0);
        assert_eq!(straightened[1].1, 0.0); // Should snap to y=0
    }

    #[test]
    fn test_area_calculation() {
        let processor = SketchProcessor::new();
        let square = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];

        let area = processor.calculate_area(&square);
        assert_eq!(area, 100.0);
    }
}
