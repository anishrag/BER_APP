//! Processing hand-drawn floor plan strokes into a precise parametric model.
//!
//! Pipeline (`FloorPlan::from_points_to_orthogonal`):
//!   1. RDP simplification (geo's `Simplify`)
//!   2. Orthogonalize each segment to horizontal or vertical
//!   3. For closed strokes: snap the implicit closing edge, rotate the start
//!      vertex to a real corner, remove collinear vertices
//!   4. Build a `ParametricPlan` and apply orthogonal constraints
//!
//! After the plan is built, callers can ask for collinearity suggestions
//! (`suggest_collinear`) and apply user decisions (`apply_collinear_suggestion`).

mod error;
mod orthogonal;
mod parametric;
mod simplification;

pub use error::Error;
pub use parametric::{Axis, CollinearitySuggestion};

use domain::points::Point2f32;

/// A processed floor plan: orthogonal polygon as a parametric representation
/// (shared coordinate expressions for collinear walls, one parameter per
/// independent dimension).
#[derive(Debug, Clone, Default)]
pub struct FloorPlan {
    parametric: parametric::ParametricPlan,
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
        //   2. rotate so the first vertex sits at a real corner (handles the
        //      "user started mid-wall" case — otherwise the start is collinear
        //      and shows up as a spurious extra vertex)
        //   3. run remove_collinear one more time to drop the previously-
        //      anchored start, now demoted to a mid-list vertex
        //
        // Note: we deliberately do NOT auto-merge near-collinear walls here.
        // Walls that are close but not adjacent in the polygon are left
        // as separate, and the user gets asked about them via the
        // `suggest_collinear` review. This keeps the parametric model honest
        // about what the user actually drew. `snap_near_collinear_walls`
        // remains available as a function for callers that want auto-merge
        // back (e.g. a future "auto-clean" mode).
        let final_points = if closed {
            let snapped = orthogonal::snap_closing_edge(orthog_points, tolerance);
            let rotated = orthogonal::rotate_to_corner_start(snapped, tolerance);
            orthogonal::remove_collinear(&rotated, tolerance)
        } else {
            orthog_points
        };

        // Create parametric representation
        let mut parametric = parametric::ParametricPlan::from_points(&final_points);

        // Apply orthogonal constraints to reduce parameters
        parametric.apply_orthogonal_constraints()?;

        Ok(Self { parametric })
    }

    /// Add another disjoint polygon to this floor plan (e.g. an extension
    /// section on an upper floor). The new polygon shares the same parameter
    /// namespace as existing ones, so cross-polygon collinearity suggestions
    /// and merges work uniformly across the whole plan.
    pub fn add_polygon(
        &mut self,
        points: &[Point2f32],
        tolerance: f32,
        closed: bool,
    ) -> Result<(), Error> {
        let mut simplified = simplification::simplify_stroke(points, tolerance)?;
        if closed && simplified.len() > 3 {
            simplified.pop();
        }
        let orthog_points = orthogonal::make_orthogonal(&simplified, tolerance)?;
        let final_points = if closed {
            let snapped = orthogonal::snap_closing_edge(orthog_points, tolerance);
            let rotated = orthogonal::rotate_to_corner_start(snapped, tolerance);
            orthogonal::remove_collinear(&rotated, tolerance)
        } else {
            orthog_points
        };

        self.parametric.add_ring(&final_points);
        self.parametric.apply_orthogonal_constraints()?;
        Ok(())
    }

    /// Concrete vertex positions of the first polygon (backwards-compatible
    /// for single-polygon callers). For multi-polygon access, use `polygons()`.
    pub fn get_points(&self) -> Vec<Point2f32> {
        self.parametric.ring_points(0)
    }

    /// All polygons' vertex positions, in ring order.
    pub fn polygons(&self) -> Vec<Vec<Point2f32>> {
        self.parametric.all_ring_points()
    }

    /// Number of polygons (rings) in this floor plan.
    pub fn ring_count(&self) -> usize {
        self.parametric.ring_count()
    }

    /// Number of independent parameters controlling the plan's geometry.
    /// Drops when collinearity merges reduce the degrees of freedom.
    pub fn active_parameter_count(&self) -> usize {
        self.parametric.active_parameter_count
    }

    /// Detect pairs of axis-aligned edges that are close to being collinear
    /// but currently aren't. Suggestions can span rings (cross-polygon).
    /// See `ParametricPlan::suggest_collinear` for details.
    pub fn suggest_collinear(
        &self,
        min_separation: f32,
        max_separation: f32,
    ) -> Vec<CollinearitySuggestion> {
        self.parametric
            .suggest_collinear(min_separation, max_separation)
    }

    /// Apply a collinearity decision returned from `suggest_collinear`.
    /// Merges the two edges so they share a single coordinate expression.
    pub fn apply_collinear_suggestion(&mut self, suggestion: &CollinearitySuggestion) {
        self.parametric.apply_collinear_suggestion(suggestion);
    }

    /// Are these two edges currently collinear along the given axis?
    /// Takes ring indices so cross-polygon queries are possible.
    pub fn edges_collinear(
        &self,
        ring_a: usize,
        edge_a: usize,
        ring_b: usize,
        edge_b: usize,
        axis: &Axis,
    ) -> bool {
        self.parametric
            .edges_collinear(ring_a, edge_a, ring_b, edge_b, axis)
    }
}
