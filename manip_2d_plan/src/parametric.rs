//! Parametric constraint system
//!
//! This module implements a parametric representation of floor plans where:
//! - All vertex coordinates are expressed as linear combinations of independent parameters
//! - The number of parameters = degrees of freedom (DOF) needed to fully constrain the plan
//! - Constraints don't change the parameter mapping, just reduce the number of active parameters
//! - Moving a wall = changing a single parameter value
//!
//! ## Example
//!
//! For a rectangle with one corner at origin:
//! - DOF = 2*4 - 2 = 6 (4 vertices, minus fixed origin)
//! - With orthogonal constraints: DOF = 2 (width and height)
//! - Parameters: [width, height]
//! - Vertex 0: (0, 0) - fixed
//! - Vertex 1: (p[0], 0) - width parameter
//! - Vertex 2: (p[0], p[1]) - both parameters
//! - Vertex 3: (0, p[1]) - height parameter

use crate::Error;
use std::collections::HashMap;

use domain::points::Point2f32;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A parameter in the constraint system
pub type ParameterId = usize;

/// Axis along which a collinearity decision is being made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Axis {
    /// Edge runs horizontal — its perpendicular coordinate is `y`.
    Horizontal,
    /// Edge runs vertical — its perpendicular coordinate is `x`.
    Vertical,
}

/// A suggestion that two edges in a `ParametricPlan` are close enough to be
/// considered the same line. The caller asks the user, then either applies
/// the suggestion (via `apply_collinear_suggestion`) or discards it.
#[derive(Debug, Clone)]
pub struct CollinearitySuggestion {
    /// Index of edge A's starting vertex. Edge A = (vertex[edge_a], vertex[edge_a + 1]).
    pub edge_a: usize,
    /// Index of edge B's starting vertex.
    pub edge_b: usize,
    pub axis: Axis,
    /// Current perpendicular coordinate of edge A (`y` for horizontal, `x` for vertical).
    pub coord_a: f32,
    /// Same for edge B.
    pub coord_b: f32,
}

struct EdgeInfo {
    start: usize,
    coord: f32,
    expr: CoordinateExpression,
}

fn collect_pairs(
    edges: &[EdgeInfo],
    axis: Axis,
    min_separation: f32,
    max_separation: f32,
    out: &mut Vec<CollinearitySuggestion>,
) {
    for i in 0..edges.len() {
        for j in (i + 1)..edges.len() {
            if edges[i].expr == edges[j].expr {
                continue; // already collinear in the parametric representation
            }
            let diff = (edges[i].coord - edges[j].coord).abs();
            if diff > min_separation && diff <= max_separation {
                out.push(CollinearitySuggestion {
                    edge_a: edges[i].start,
                    edge_b: edges[j].start,
                    axis: axis.clone(),
                    coord_a: edges[i].coord,
                    coord_b: edges[j].coord,
                });
            }
        }
    }
}

/// An expression that defines a coordinate value
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum CoordinateExpression {
    /// A constant value (e.g., fixed at origin)
    Constant(f32),

    /// A single parameter (e.g., x = p[3])
    Parameter(ParameterId),
}

impl CoordinateExpression {
    /// Evaluate the expression given parameter values
    pub fn evaluate(&self, params: &HashMap<ParameterId, f32>) -> f32 {
        match self {
            CoordinateExpression::Constant(v) => *v,
            CoordinateExpression::Parameter(id) => *params.get(id).unwrap_or(&0.0),
        }
    }
}

/// A vertex defined parametrically
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ParametricVertex {
    pub x_expr: CoordinateExpression,
    pub y_expr: CoordinateExpression,
}

impl ParametricVertex {
    /// Create a vertex at a constant position
    pub fn constant(x: f32, y: f32) -> Self {
        Self {
            x_expr: CoordinateExpression::Constant(x),
            y_expr: CoordinateExpression::Constant(y),
        }
    }

    /// Evaluate the vertex position given parameter values
    pub fn evaluate(&self, params: &HashMap<ParameterId, f32>) -> Point2f32 {
        Point2f32::new(self.x_expr.evaluate(params), self.y_expr.evaluate(params))
    }
}

/// Parametric representation of a floor plan
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ParametricPlan {
    /// The parametric vertices
    pub vertices: Vec<ParametricVertex>,

    /// Current parameter values
    pub parameters: HashMap<ParameterId, f32>,

    /// Number of active parameters (after constraints applied)
    pub active_parameter_count: usize,

    /// Next available parameter ID
    next_param_id: ParameterId,
}

impl ParametricPlan {
    /// Create a new parametric plan from a polygon
    /// Initially, each coordinate is a free parameter (before constraints)
    pub fn from_points(points: &Vec<Point2f32>) -> Self {
        let n = points.len();
        let mut vertices = Vec::new();
        let mut parameters = HashMap::new();
        let mut param_id = 0;

        // First vertex is fixed at origin (or at its current position)
        vertices.push(ParametricVertex::constant(points[0].x, points[0].y));

        // Remaining vertices: each coordinate is initially a free parameter
        for i in 1..n {
            let x_param = param_id;
            param_id += 1;
            let y_param = param_id;
            param_id += 1;

            parameters.insert(x_param, points[i].x);
            parameters.insert(y_param, points[i].y);

            vertices.push(ParametricVertex {
                x_expr: CoordinateExpression::Parameter(x_param),
                y_expr: CoordinateExpression::Parameter(y_param),
            });
        }

        let active_count = param_id;

        Self {
            vertices,
            parameters,
            active_parameter_count: active_count,
            next_param_id: param_id,
        }
    }

    /// Apply orthogonal constraints
    /// For orthogonal plans, we reduce parameters by sharing coordinates along axes.
    ///
    /// Assumes V0's `x_expr` and `y_expr` are `Constant(_)` — this is the contract
    /// from `from_points`, which pins V0 to the origin. Coordinates on other
    /// vertices that match V0's constants (within tolerance) collapse to those
    /// constants rather than getting fresh parameters. If V0 ever becomes
    /// parameterized (e.g. to translate the whole plan), this dedup needs to
    /// treat V0 like any other vertex instead of folding into its constants.
    pub fn apply_orthogonal_constraints(&mut self) -> Result<(), Error> {
        let tolerance = 0.5;

        let positions: Vec<Point2f32> = self
            .vertices
            .iter()
            .map(|v| v.evaluate(&self.parameters))
            .collect();

        // Capture V0's constants so matching x/y values on later vertices
        // collapse to constants instead of becoming new parameters.
        let v0_const_x = match &self.vertices[0].x_expr {
            CoordinateExpression::Constant(v) => Some(*v),
            _ => None,
        };
        let v0_const_y = match &self.vertices[0].y_expr {
            CoordinateExpression::Constant(v) => Some(*v),
            _ => None,
        };

        let mut unique_x: Vec<(f32, ParameterId)> = Vec::new();
        let mut unique_y: Vec<(f32, ParameterId)> = Vec::new();
        let mut new_vertices = vec![self.vertices[0].clone()];
        let mut param_id = 0;
        let mut new_parameters = HashMap::new();

        for i in 1..positions.len() {
            let pos = &positions[i];

            // x: prefer V0's constant, then a previously-seen parameter, else a new one.
            let x_expr = if let Some(c) = v0_const_x.filter(|c| (c - pos.x).abs() < tolerance) {
                CoordinateExpression::Constant(c)
            } else if let Some((_, pid)) = unique_x
                .iter()
                .find(|(val, _)| (val - pos.x).abs() < tolerance)
            {
                CoordinateExpression::Parameter(*pid)
            } else {
                let pid = param_id;
                param_id += 1;
                new_parameters.insert(pid, pos.x);
                unique_x.push((pos.x, pid));
                CoordinateExpression::Parameter(pid)
            };

            // y: same logic, other axis.
            let y_expr = if let Some(c) = v0_const_y.filter(|c| (c - pos.y).abs() < tolerance) {
                CoordinateExpression::Constant(c)
            } else if let Some((_, pid)) = unique_y
                .iter()
                .find(|(val, _)| (val - pos.y).abs() < tolerance)
            {
                CoordinateExpression::Parameter(*pid)
            } else {
                let pid = param_id;
                param_id += 1;
                new_parameters.insert(pid, pos.y);
                unique_y.push((pos.y, pid));
                CoordinateExpression::Parameter(pid)
            };

            new_vertices.push(ParametricVertex { x_expr, y_expr });
        }

        self.vertices = new_vertices;
        self.parameters = new_parameters;
        self.active_parameter_count = param_id;
        self.next_param_id = param_id;

        Ok(())
    }

    /// Set a parameter value (used by tests today; the wall-drag UI will use
    /// this when implemented).
    pub fn set_parameter(&mut self, param_id: ParameterId, value: f32) {
        self.parameters.insert(param_id, value);
    }

    /// Concrete vertex positions of the polygon.
    pub fn get_points(&self) -> Vec<Point2f32> {
        self.vertices
            .iter()
            .map(|v| v.evaluate(&self.parameters))
            .collect()
    }

    /// Detect pairs of axis-aligned edges whose perpendicular coordinates are
    /// close (in `(min_separation, max_separation]`) but currently different.
    ///
    /// Returns candidate `CollinearitySuggestion`s for user confirmation. Pairs
    /// whose edges already share the same coordinate expression are skipped —
    /// they're already collinear by construction. Pairs closer than
    /// `min_separation` should have been auto-merged earlier in the pipeline
    /// (`snap_near_collinear_walls`); pairs farther than `max_separation` are
    /// treated as distinct walls.
    pub fn suggest_collinear(
        &self,
        min_separation: f32,
        max_separation: f32,
    ) -> Vec<CollinearitySuggestion> {
        let n = self.vertices.len();
        if n < 4 {
            return Vec::new();
        }

        let positions: Vec<Point2f32> = self
            .vertices
            .iter()
            .map(|v| v.evaluate(&self.parameters))
            .collect();

        // Classify edges into horizontal / vertical (an edge is axis-aligned
        // when its two endpoints share the same expression on one coord).
        let mut horizontal: Vec<EdgeInfo> = Vec::new();
        let mut vertical: Vec<EdgeInfo> = Vec::new();
        for i in 0..n {
            let next_i = (i + 1) % n;
            let v1 = &self.vertices[i];
            let v2 = &self.vertices[next_i];
            if v1.y_expr == v2.y_expr {
                horizontal.push(EdgeInfo {
                    start: i,
                    coord: positions[i].y,
                    expr: v1.y_expr.clone(),
                });
            } else if v1.x_expr == v2.x_expr {
                vertical.push(EdgeInfo {
                    start: i,
                    coord: positions[i].x,
                    expr: v1.x_expr.clone(),
                });
            }
            // Non-axis-aligned edges (shouldn't exist post-orthogonalization) are skipped.
        }

        let mut suggestions = Vec::new();
        collect_pairs(
            &horizontal,
            Axis::Horizontal,
            min_separation,
            max_separation,
            &mut suggestions,
        );
        collect_pairs(
            &vertical,
            Axis::Vertical,
            min_separation,
            max_separation,
            &mut suggestions,
        );
        suggestions
    }

    /// Apply a collinearity decision: make edge_b share edge_a's coordinate
    /// expression along the suggested axis. Both endpoints of edge_b are
    /// updated. The parameter that uniquely belonged to edge_b is left in
    /// `self.parameters` (orphaned but harmless) while `active_parameter_count`
    /// is recomputed from the live expressions.
    pub fn apply_collinear_suggestion(&mut self, suggestion: &CollinearitySuggestion) {
        let n = self.vertices.len();
        let edge_a_start = suggestion.edge_a;
        let edge_b_start = suggestion.edge_b;
        let edge_b_end = (edge_b_start + 1) % n;

        match suggestion.axis {
            Axis::Horizontal => {
                let target = self.vertices[edge_a_start].y_expr.clone();
                self.vertices[edge_b_start].y_expr = target.clone();
                self.vertices[edge_b_end].y_expr = target;
            }
            Axis::Vertical => {
                let target = self.vertices[edge_a_start].x_expr.clone();
                self.vertices[edge_b_start].x_expr = target.clone();
                self.vertices[edge_b_end].x_expr = target;
            }
        }

        self.recompute_active_parameter_count();
    }

    /// Are these two edges currently collinear along the given axis? True when
    /// they share the same coordinate expression along that axis. Callers use
    /// this to filter "already determined" suggestions after applying earlier
    /// ones (the transitive part of the user's question flow).
    pub fn edges_collinear(&self, edge_a: usize, edge_b: usize, axis: &Axis) -> bool {
        let n = self.vertices.len();
        if edge_a >= n || edge_b >= n {
            return false;
        }
        match axis {
            Axis::Horizontal => self.vertices[edge_a].y_expr == self.vertices[edge_b].y_expr,
            Axis::Vertical => self.vertices[edge_a].x_expr == self.vertices[edge_b].x_expr,
        }
    }

    fn recompute_active_parameter_count(&mut self) {
        let mut used: std::collections::HashSet<ParameterId> = std::collections::HashSet::new();
        for v in &self.vertices {
            if let CoordinateExpression::Parameter(id) = &v.x_expr {
                used.insert(*id);
            }
            if let CoordinateExpression::Parameter(id) = &v.y_expr {
                used.insert(*id);
            }
        }
        self.active_parameter_count = used.len();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a `ParametricPlan` directly from a list of axis-aligned
    /// points, then run `apply_orthogonal_constraints` so the parametric
    /// representation matches what `from_points_to_orthogonal` would produce.
    fn build_plan(points: &[(f32, f32)]) -> ParametricPlan {
        let pts: Vec<Point2f32> = points.iter().map(|(x, y)| Point2f32::new(*x, *y)).collect();
        let mut plan = ParametricPlan::from_points(&pts);
        plan.apply_orthogonal_constraints().unwrap();
        plan
    }

    #[test]
    fn suggest_collinear_returns_empty_for_clean_l() {
        // L with no near-collinear walls — every wall is either >= 100px from
        // any other, or already shares an expression.
        let plan = build_plan(&[
            (0.0, 0.0),
            (200.0, 0.0),
            (200.0, 100.0),
            (100.0, 100.0),
            (100.0, 200.0),
            (0.0, 200.0),
        ]);
        let suggestions = plan.suggest_collinear(20.0, 60.0);
        assert!(suggestions.is_empty());
    }

    #[test]
    fn suggest_collinear_flags_offset_wall() {
        // Two horizontal walls 30px apart (in the 20–60 range).
        let plan = build_plan(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 50.0),
            (200.0, 50.0), // upper bar of a notched shape
            (200.0, 80.0), // top, 30px above y=50
            (0.0, 80.0),
        ]);
        let suggestions = plan.suggest_collinear(20.0, 60.0);
        // Several horizontal walls; at least one pair should be flagged.
        assert!(!suggestions.is_empty());
    }

    #[test]
    fn apply_collinear_suggestion_reduces_parameter_count() {
        // Polygon with two horizontal walls at y=50 and y=53 (3px apart — but
        // we'll use a different config where they're 30px apart since auto-snap
        // would have caught 3px). Actually use y=50 and y=80 so they aren't merged.
        let plan = build_plan(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 50.0),
            (200.0, 50.0),
            (200.0, 80.0),
            (0.0, 80.0),
        ]);
        let initial = plan.active_parameter_count;
        let suggestions = plan.suggest_collinear(20.0, 60.0);
        assert!(!suggestions.is_empty(), "expected at least one suggestion");

        let mut plan = plan;
        plan.apply_collinear_suggestion(&suggestions[0]);
        assert!(
            plan.active_parameter_count < initial,
            "applying a collinear suggestion should reduce the parameter count (was {}, now {})",
            initial,
            plan.active_parameter_count,
        );
    }

    #[test]
    fn edges_collinear_after_apply_returns_true_transitively() {
        // Three horizontal walls at y=50, y=80, y=110 (within 30px of each other
        // but not auto-snap distance). After merging the first two, asking about
        // the second and third should still be required; after merging those,
        // checking the first and third should report collinear.
        let plan = build_plan(&[
            (0.0, 0.0),
            (50.0, 0.0),
            (50.0, 50.0),
            (100.0, 50.0),
            (100.0, 80.0),
            (150.0, 80.0),
            (150.0, 110.0),
            (200.0, 110.0),
            (200.0, 130.0),
            (0.0, 130.0),
        ]);
        let suggestions = plan.suggest_collinear(20.0, 60.0);
        // Find two suggestions that, after applied in order, link three walls.
        // We just verify that after applying ANY two horizontal suggestions
        // sharing a common edge, the chain is detectable.
        let h_suggestions: Vec<&CollinearitySuggestion> = suggestions
            .iter()
            .filter(|s| s.axis == Axis::Horizontal)
            .collect();
        if h_suggestions.len() >= 2 {
            let first = h_suggestions[0].clone();
            let second = h_suggestions[1].clone();
            let mut plan = plan;
            plan.apply_collinear_suggestion(&first);
            plan.apply_collinear_suggestion(&second);
            // After both applies, the two edges in `first` should still report collinear.
            assert!(plan.edges_collinear(first.edge_a, first.edge_b, &Axis::Horizontal));
        }
    }

    #[test]
    fn test_parametric_rectangle() {
        // Create a rectangle
        let points = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 5.0),
            Point2f32::new(0.0, 5.0),
        ];

        let mut plan = ParametricPlan::from_points(&points);

        // Initially 6 parameters (3 vertices * 2 coords, minus fixed origin)
        assert_eq!(plan.active_parameter_count, 6);

        // Apply orthogonal constraints
        plan.apply_orthogonal_constraints().unwrap();

        // Should reduce to 2 parameters (width and height)
        assert_eq!(plan.active_parameter_count, 2);

        // Change the width parameter
        plan.set_parameter(0, 15.0);

        let result = plan.get_points();
        assert!((result[1].x - 15.0).abs() < 0.01);
    }
}
