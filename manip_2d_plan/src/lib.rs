//! # Floorplan Geometry
//!
//! A library for processing hand-drawn floor plan strokes into precise geometric plans.
//!
//! This crate provides tools for:
//! - Simplifying rough hand-drawn strokes into clean polygons
//! - Creating orthogonal (parallel/perpendicular) layouts
//! - Adding and trimming areas with automatic snapping
//! - Managing geometric constraints
//! - Calculating required dimensions
//! - Room detection and labeling
//!
//! ## Example
//!
//! ```ignore
//! // Note: doctest disabled — `add_area_orthogonal` and other ops below are
//! // commented out pending re-implementation. See the commented blocks in this file.
//! use floorplan_geometry::*;
//!
//! // Create a stroke from user input
//! let points = vec![
//!     Point::new(0.0, 0.0),
//!     Point::new(10.2, 0.3),
//!     Point::new(10.1, 10.1),
//!     Point::new(0.1, 9.9),
//! ];
//! let stroke = Stroke::new(points);
//!
//! // Simplify to orthogonal polygon
//! let plan = FloorPlan::from_stroke_orthogonal(&stroke, 0.5)?;
//!
//! // Add an area
//! let addition_stroke = Stroke::new(vec![/* ... */]);
//! let updated_plan = plan.add_area_orthogonal(&addition_stroke)?;
//! # Ok::<(), floorplan_geometry::Error>(())
//! ```

mod error;
mod orthogonal;
mod parametric;
//mod rooms;
mod simplification;

// mod dimensions;

// mod measurements;
// mod operations;
//
//
// mod snapping;

// mod utils;

pub use error::Error;

use domain::points::{Point2f32, Point2f64};
use geo_types::{LineString, Point as GeoPoint, Polygon as GeoPolygon};

/// A simplified floor plan polygon
#[derive(Debug, Clone, Default)]
pub struct FloorPlan {
    /// The parametric representation of the floor plan
    // n
    pub parametric: parametric::ParametricPlan,
    //pub rooms: Vec<rooms::Room>,
}

impl FloorPlan {
    /// Create a floor plan from a hand-drawn stroke with orthogonal constraint.
    /// Automatically creates a parametric representation with H/V constraints applied.
    ///
    /// `closed` indicates whether the stroke is an implicitly-closed polygon
    /// (the assessor lifted near the start and `check_closed` fired). When
    /// closed, RDP runs first to collapse collinear closing-edge drag points,
    /// then we drop the final trailing vertex so the closing edge becomes
    /// implicit and orthogonalization doesn't introduce a spurious corner.
    pub fn from_points_to_orthogonal(
        points: &[Point2f32],
        tolerance: f32,
        closed: bool,
    ) -> Result<Self, Error> {
        let mut simplified = simplification::simplify_stroke(points, tolerance)?;

        if closed && simplified.len() > 3 {
            // Drop the trailing drag point. RDP just collapsed everything along
            // the closing edge into one segment; the remaining endpoint is
            // along that edge, not a corner. Removing it lets the implicit
            // edge from the new last vertex to first be axis-aligned by
            // virtue of the user's drawing, with no spurious L-corner added
            // during orthogonalization.
            simplified.pop();
        }

        let orthog_points = orthogonal::make_orthogonal(&simplified, tolerance)?;

        // For a closed polygon, finalize the closing structure before the
        // parametric layer sees it:
        //   1. snap the implicit closing edge to be exactly axis-aligned
        //   2. snap nearby walls onto the same coord (catches drawing wobble
        //      where the user meant to align two walls but drew them slightly
        //      offset — e.g. the upper bar of an L 12px off from the lower bar)
        //   3. dedup any vertices the snap landed on top of each other
        //   4. rotate so the first vertex sits at a real corner (handles the
        //      "user started mid-wall" case — otherwise the start is collinear
        //      and shows up as a spurious extra vertex)
        //   5. run remove_collinear one more time to drop the previously-
        //      anchored start, now demoted to a mid-list vertex
        let final_points = if closed {
            let snapped = orthogonal::snap_closing_edge(orthog_points, tolerance);
            let aligned = orthogonal::snap_near_collinear_walls(snapped, 20.0);
            let deduped = orthogonal::remove_duplicates(&aligned, tolerance);
            let rotated = orthogonal::rotate_to_corner_start(deduped, tolerance);
            orthogonal::remove_collinear(&rotated, tolerance)
        } else {
            orthog_points
        };

        // Create parametric representation
        let mut parametric = parametric::ParametricPlan::from_points(&final_points);

        // Apply orthogonal constraints to reduce parameters
        parametric.apply_orthogonal_constraints()?;

        Ok(Self {
            parametric,
            //rooms: Vec::new(),
        })
    }
    //
    //     /// Create a floor plan from a hand-drawn stroke without orthogonal constraint
    //     pub fn from_stroke_freeform(stroke: &Stroke, tolerance: f64) -> Result<Self, Error> {
    //         let polygon = simplification::simplify_to_polygon(stroke, tolerance)?;
    //         let parametric = parametric::ParametricPlan::from_polygon(&polygon);
    //
    //         Ok(Self {
    //             parametric,
    //             constraints: constraints::ConstraintSet::new(),
    //             rooms: Vec::new(),
    //         })
    //     }
    //
    //
    //     /// Get collinear edge suggestions for user confirmation
    //     /// This should be called after the initial orthogonal plan is created
    //     pub fn suggest_collinear_constraints(
    //         &self,
    //         tolerance: f64,
    //     ) -> Vec<constraints::CollinearSuggestion> {
    //         let polygon = self.polygon();
    //         let temp_plan = FloorPlan {
    //             parametric: self.parametric.clone(),
    //             constraints: self.constraints.clone(),
    //             rooms: Vec::new(),
    //         };
    //         constraints::suggest_collinear(&temp_plan, tolerance)
    //     }
    //
    //     /// Apply a collinear constraint between two edges (user has confirmed they should be on same line)
    //     pub fn add_collinear_constraint(&mut self, edge1: usize, edge2: usize) -> Result<(), Error> {
    //         self.constraints.add_collinear(edge1, edge2)?;
    //         self.parametric.apply_collinear_constraint(edge1, edge2)?;
    //         Ok(())
    //     }
    //
    //     /// Calculate which dimensions (edge lengths) are needed to fully constrain the plan
    //     /// Call this AFTER collinear constraints have been set
    //     /// Returns parameter IDs that need values
    //     pub fn calculate_required_parameters(&self) -> Vec<parametric::ParameterId> {
    //         self.parametric.get_unset_parameters()
    //     }
    //
    //     /// Get all active parameters
    //     pub fn get_active_parameters(&self) -> Vec<parametric::ParameterId> {
    //         self.parametric.get_active_parameters()
    //     }
    //
    //     /// Set a parameter value (e.g., an edge length or coordinate)
    //     pub fn set_parameter(&mut self, param_id: parametric::ParameterId, value: f64) {
    //         self.parametric.set_parameter(param_id, value);
    //     }
    //
    //     /// Get a parameter value
    //     pub fn get_parameter(&self, param_id: parametric::ParameterId) -> Option<f64> {
    //         self.parametric.get_parameter(param_id)
    //     }
    //
    //     /// Get which parameter controls a specific edge's length
    //     /// Returns None if the edge length is derived from multiple parameters
    //     pub fn get_edge_parameter(&self, edge_index: usize) -> Option<parametric::ParameterId> {
    //         self.parametric.get_edge_length_parameter(edge_index)
    //     }
    //
    //     /// Get smart measurement suggestions (what the user should actually measure)
    //     /// This is better than just asking for raw parameter values
    //     pub fn suggest_measurements(&self) -> Vec<measurements::MeasurementSuggestion> {
    //         measurements::suggest_measurements(self)
    //     }
    //
    //     /// Apply a measurement (convert user's measurement to parameter value)
    //     pub fn apply_measurement(
    //         &mut self,
    //         suggestion: &measurements::MeasurementSuggestion,
    //         measured_value: f64,
    //     ) -> Result<(), Error> {
    //         // For simple cases, just set the parameter directly
    //         if suggestion.target_parameters.len() == 1 {
    //             let param_id = suggestion.target_parameters[0];
    //
    //             match suggestion.measurement_type {
    //                 measurements::MeasurementType::EdgeLength
    //                 | measurements::MeasurementType::RoomDimension
    //                 | measurements::MeasurementType::OverallDimension
    //                 | measurements::MeasurementType::AbsoluteFromOrigin => {
    //                     self.set_parameter(param_id, measured_value);
    //                     Ok(())
    //                 }
    //                 measurements::MeasurementType::IncrementalOffset => {
    //                     // For incremental, the measured value might need to be combined with another parameter
    //                     // For now, just set it directly (can be enhanced later)
    //                     self.set_parameter(param_id, measured_value);
    //                     Ok(())
    //                 }
    //             }
    //         } else {
    //             // Multiple parameters - more complex logic would go here
    //             Ok(())
    //         }
    //     }
    //
    //     /// Get a human-readable description of what needs to be measured
    //     pub fn get_measurement_instructions(&self) -> Vec<String> {
    //         self.suggest_measurements()
    //             .iter()
    //             .map(|s| s.description.clone())
    //             .collect()
    //     }
    //
    //     /// Add an area to the floor plan using an orthogonal stroke
    //     pub fn add_area_orthogonal(&self, stroke: &Stroke) -> Result<Self, Error> {
    //         operations::add_area(self, stroke, true)
    //     }
    //
    //     /// Add an area to the floor plan without orthogonal constraint
    //     pub fn add_area_freeform(&self, stroke: &Stroke) -> Result<Self, Error> {
    //         operations::add_area(self, stroke, false)
    //     }
    //
    //     /// Trim an area from the floor plan using an orthogonal stroke
    //     pub fn trim_area_orthogonal(&self, stroke: &Stroke) -> Result<Self, Error> {
    //         operations::trim_area(self, stroke, true)
    //     }
    //
    //     /// Trim the floor plan by a line, keeping the largest part
    //     pub fn trim_by_line(&self, line: &Line) -> Result<Self, Error> {
    //         operations::trim_by_line(self, line)
    //     }
    //
    //     /// Shift a wall while maintaining connections
    //     pub fn shift_wall(&mut self, edge_index: usize, offset: f64) -> Result<(), Error> {
    //         operations::shift_wall(self, edge_index, offset)
    //     }
    //
    //     /// Detect and identify rooms within the floor plan
    //     pub fn detect_rooms(&mut self) -> Result<(), Error> {
    //         self.rooms = rooms::detect_rooms(&self.polygon)?;
    //         Ok(())
    //     }
    //
    //     /// Apply all active constraints to update the geometry
    //     fn apply_constraints(&mut self) -> Result<(), Error> {
    //         constraints::apply_all_constraints(self)
    //     }
}
