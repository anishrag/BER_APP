//! Parametric constraint system for an entire storey's floor geometry.
//!
//! A `ParametricPlan` holds one or more **rings** (each ring is a closed
//! polygon). All rings share a single parameter namespace, so collinearity
//! constraints can tie walls together across disjoint polygons (e.g. an
//! upper-floor extension whose walls line up with another section).
//!
//! ## Coordinate model
//!
//! Each vertex coordinate is either a `Constant(f32)` (fixed at a literal
//! value — used for the very first vertex of the first ring, the global
//! origin) or a `Parameter(ParameterId)` (lookup into the shared
//! `parameters` map).
//!
//! Two vertices that share a `Parameter` reference share that coordinate by
//! construction — moving the parameter moves both. Collinearity merges are
//! implemented by rewriting one vertex's coordinate expression to match the
//! other's.

use crate::Error;
use std::collections::{HashMap, HashSet};

use domain::points::Point2f32;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

pub type ParameterId = usize;

/// A stable identifier for a vertex. Survives `apply_orthogonal_constraints`
/// rebuilds and (eventually) other geometric edits — annotations attached
/// to a vertex stay attached even as its position or coordinate expression
/// changes. Assigned monotonically by the plan; never reused.
pub type VertexId = u64;

/// Axis along which a collinearity decision is being made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Axis {
    /// Edge runs horizontal — its perpendicular coordinate is `y`.
    Horizontal,
    /// Edge runs vertical — its perpendicular coordinate is `x`.
    Vertical,
}

/// A suggestion that two edges (possibly in different rings of the same plan)
/// are close enough to be considered the same line. The caller asks the user,
/// then either applies (`apply_collinear_suggestion`) or discards.
#[derive(Debug, Clone)]
pub struct CollinearitySuggestion {
    /// Which ring edge A belongs to.
    pub ring_a: usize,
    /// Starting-vertex index of edge A within `ring_a`. Edge A is the segment
    /// from `rings[ring_a][edge_a]` to `rings[ring_a][edge_a + 1]`.
    pub edge_a: usize,
    pub ring_b: usize,
    pub edge_b: usize,
    pub axis: Axis,
    /// Current perpendicular coord of edge A (`y` for horizontal, `x` for vertical).
    pub coord_a: f32,
    pub coord_b: f32,
}

#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum CoordinateExpression {
    Constant(f32),
    Parameter(ParameterId),
}

impl CoordinateExpression {
    pub fn evaluate(&self, params: &HashMap<ParameterId, f32>) -> f32 {
        match self {
            CoordinateExpression::Constant(v) => *v,
            CoordinateExpression::Parameter(id) => *params.get(id).unwrap_or(&0.0),
        }
    }
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ParametricVertex {
    /// Stable identity. Persists across `apply_orthogonal_constraints`,
    /// collinearity merges, and (future) ring-edit operations. The `(ring,
    /// index)` position can shift; the `id` doesn't.
    pub id: VertexId,
    pub x_expr: CoordinateExpression,
    pub y_expr: CoordinateExpression,
}

impl ParametricVertex {
    pub fn constant(id: VertexId, x: f32, y: f32) -> Self {
        Self {
            id,
            x_expr: CoordinateExpression::Constant(x),
            y_expr: CoordinateExpression::Constant(y),
        }
    }

    pub fn evaluate(&self, params: &HashMap<ParameterId, f32>) -> Point2f32 {
        Point2f32::new(self.x_expr.evaluate(params), self.y_expr.evaluate(params))
    }
}

/// Parametric model of a storey's floor geometry. One or more rings (closed
/// polygons) sharing a single parameter namespace.
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct ParametricPlan {
    /// Each ring is one closed polygon. `rings[0]` is the primary; additional
    /// rings represent disjoint sections (e.g. extensions on upper floors).
    pub rings: Vec<Vec<ParametricVertex>>,
    /// Shared parameter values, keyed by `ParameterId`.
    pub parameters: HashMap<ParameterId, f32>,
    /// Number of distinct parameter IDs currently referenced across all rings.
    pub active_parameter_count: usize,
    next_param_id: ParameterId,
    /// Next vertex ID to hand out. Monotonically increasing — IDs are never
    /// reused even after vertices are removed (when remove operations exist).
    next_vertex_id: VertexId,
}

impl ParametricPlan {
    /// Create a plan with a single ring from a set of points. The first point
    /// of the first ring becomes the global constant origin.
    pub fn from_points(points: &[Point2f32]) -> Self {
        let mut plan = Self::default();
        plan.add_ring_internal(points, true);
        plan
    }

    /// Add a ring to the existing plan. Treats every vertex as a free
    /// parameter (no constant). To unify shared coordinates across rings,
    /// call `apply_orthogonal_constraints` afterwards.
    pub fn add_ring(&mut self, points: &[Point2f32]) {
        self.add_ring_internal(points, false);
    }

    fn add_ring_internal(&mut self, points: &[Point2f32], first_ring: bool) {
        if points.is_empty() {
            return;
        }

        let mut ring = Vec::with_capacity(points.len());

        let start = if first_ring && self.rings.is_empty() {
            // First-ever vertex of the first ring is the constant origin.
            let id = self.alloc_vertex_id();
            ring.push(ParametricVertex::constant(id, points[0].x, points[0].y));
            1
        } else {
            0
        };

        for p in &points[start..] {
            let x_param = self.next_param_id;
            self.next_param_id += 1;
            let y_param = self.next_param_id;
            self.next_param_id += 1;

            self.parameters.insert(x_param, p.x);
            self.parameters.insert(y_param, p.y);

            let id = self.alloc_vertex_id();
            ring.push(ParametricVertex {
                id,
                x_expr: CoordinateExpression::Parameter(x_param),
                y_expr: CoordinateExpression::Parameter(y_param),
            });
        }

        self.rings.push(ring);
        self.recompute_active_parameter_count();
    }

    fn alloc_vertex_id(&mut self) -> VertexId {
        let id = self.next_vertex_id;
        self.next_vertex_id += 1;
        id
    }

    /// Look up a vertex by its stable ID. Returns `(ring_index, vertex_index)`
    /// or `None` if the vertex no longer exists.
    pub fn find_vertex(&self, id: VertexId) -> Option<(usize, usize)> {
        for (ring_idx, ring) in self.rings.iter().enumerate() {
            for (vert_idx, v) in ring.iter().enumerate() {
                if v.id == id {
                    return Some((ring_idx, vert_idx));
                }
            }
        }
        None
    }

    /// Get the stable ID of the vertex at `(ring, vertex_idx)`, if it exists.
    pub fn vertex_id_at(&self, ring: usize, vertex_idx: usize) -> Option<VertexId> {
        self.rings
            .get(ring)
            .and_then(|r| r.get(vertex_idx))
            .map(|v| v.id)
    }

    /// Number of rings (closed polygons) in this plan.
    pub fn ring_count(&self) -> usize {
        self.rings.len()
    }

    /// Concrete vertex positions of a single ring.
    pub fn ring_points(&self, ring: usize) -> Vec<Point2f32> {
        self.rings
            .get(ring)
            .map(|r| r.iter().map(|v| v.evaluate(&self.parameters)).collect())
            .unwrap_or_default()
    }

    /// All rings' vertex positions, in ring order.
    pub fn all_ring_points(&self) -> Vec<Vec<Point2f32>> {
        self.rings
            .iter()
            .map(|r| r.iter().map(|v| v.evaluate(&self.parameters)).collect())
            .collect()
    }

    /// Set a parameter value (used by tests today; the wall-drag UI will use
    /// this when implemented).
    pub fn set_parameter(&mut self, param_id: ParameterId, value: f32) {
        self.parameters.insert(param_id, value);
    }

    /// Walk every vertex across every ring, dedupe coordinates that match
    /// within tolerance, and rebuild the parameter namespace from scratch.
    /// Vertices that match `rings[0][0]`'s constants collapse to constants;
    /// other near-equal coordinates share a parameter.
    ///
    /// Assumes the first ring's V0 is `Constant(_)` (the global origin). If
    /// that ever becomes parameterized (e.g. to translate the whole plan),
    /// this dedup needs to treat it like any other vertex.
    pub fn apply_orthogonal_constraints(&mut self) -> Result<(), Error> {
        if self.rings.is_empty() {
            return Ok(());
        }
        let tolerance = 0.5;

        // V0 of ring 0 stays as-is (the global origin constant).
        let v0_const_x = match &self.rings[0][0].x_expr {
            CoordinateExpression::Constant(v) => Some(*v),
            _ => None,
        };
        let v0_const_y = match &self.rings[0][0].y_expr {
            CoordinateExpression::Constant(v) => Some(*v),
            _ => None,
        };

        // Snapshot positions so we can rebuild expressions while reading values.
        let snapshot: Vec<Vec<Point2f32>> = self.all_ring_points();

        let mut unique_x: Vec<(f32, ParameterId)> = Vec::new();
        let mut unique_y: Vec<(f32, ParameterId)> = Vec::new();
        let mut new_rings: Vec<Vec<ParametricVertex>> = Vec::with_capacity(self.rings.len());
        let mut param_id: ParameterId = 0;
        let mut new_parameters: HashMap<ParameterId, f32> = HashMap::new();

        for (ring_idx, ring_positions) in snapshot.iter().enumerate() {
            let mut new_ring: Vec<ParametricVertex> = Vec::with_capacity(ring_positions.len());

            let start_index = if ring_idx == 0 {
                // First ring: preserve V0 (constant origin) verbatim — its
                // id, x_expr, y_expr all carry through unchanged.
                new_ring.push(self.rings[0][0].clone());
                1
            } else {
                0
            };

            for i in start_index..ring_positions.len() {
                let pos = &ring_positions[i];
                // Preserve the existing vertex's stable id through the rebuild.
                let old_id = self.rings[ring_idx][i].id;

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

                new_ring.push(ParametricVertex {
                    id: old_id,
                    x_expr,
                    y_expr,
                });
            }

            new_rings.push(new_ring);
        }

        self.rings = new_rings;
        self.parameters = new_parameters;
        self.active_parameter_count = param_id;
        self.next_param_id = param_id;

        Ok(())
    }

    /// Detect pairs of axis-aligned edges — possibly in different rings —
    /// whose perpendicular coordinates differ by `(min_separation, max_separation]`.
    /// Pairs already sharing a coordinate expression are skipped (already collinear).
    pub fn suggest_collinear(
        &self,
        min_separation: f32,
        max_separation: f32,
    ) -> Vec<CollinearitySuggestion> {
        let positions = self.all_ring_points();
        let mut horizontal: Vec<EdgeInfo> = Vec::new();
        let mut vertical: Vec<EdgeInfo> = Vec::new();

        for (ring_idx, ring) in self.rings.iter().enumerate() {
            let n = ring.len();
            if n < 4 {
                continue;
            }
            for i in 0..n {
                let next_i = (i + 1) % n;
                let v1 = &ring[i];
                let v2 = &ring[next_i];
                if v1.y_expr == v2.y_expr {
                    horizontal.push(EdgeInfo {
                        ring: ring_idx,
                        start: i,
                        coord: positions[ring_idx][i].y,
                        expr: v1.y_expr.clone(),
                    });
                } else if v1.x_expr == v2.x_expr {
                    vertical.push(EdgeInfo {
                        ring: ring_idx,
                        start: i,
                        coord: positions[ring_idx][i].x,
                        expr: v1.x_expr.clone(),
                    });
                }
            }
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

    /// Apply a collinearity decision: rewrite edge B's coordinate expression
    /// to match edge A's. Cross-ring is supported.
    pub fn apply_collinear_suggestion(&mut self, suggestion: &CollinearitySuggestion) {
        let Some(ring_a) = self.rings.get(suggestion.ring_a) else {
            return;
        };
        let n_a = ring_a.len();
        let edge_a_start = suggestion.edge_a;
        if edge_a_start >= n_a {
            return;
        }

        let target_expr = match suggestion.axis {
            Axis::Horizontal => ring_a[edge_a_start].y_expr.clone(),
            Axis::Vertical => ring_a[edge_a_start].x_expr.clone(),
        };

        let Some(ring_b) = self.rings.get_mut(suggestion.ring_b) else {
            return;
        };
        let n_b = ring_b.len();
        let edge_b_start = suggestion.edge_b;
        if edge_b_start >= n_b {
            return;
        }
        let edge_b_end = (edge_b_start + 1) % n_b;

        match suggestion.axis {
            Axis::Horizontal => {
                ring_b[edge_b_start].y_expr = target_expr.clone();
                ring_b[edge_b_end].y_expr = target_expr;
            }
            Axis::Vertical => {
                ring_b[edge_b_start].x_expr = target_expr.clone();
                ring_b[edge_b_end].x_expr = target_expr;
            }
        }

        self.recompute_active_parameter_count();
    }

    /// Are these two edges (possibly in different rings) collinear along
    /// the given axis? True when their coordinate expressions match.
    pub fn edges_collinear(
        &self,
        ring_a: usize,
        edge_a: usize,
        ring_b: usize,
        edge_b: usize,
        axis: &Axis,
    ) -> bool {
        let Some(va) = self.rings.get(ring_a).and_then(|r| r.get(edge_a)) else {
            return false;
        };
        let Some(vb) = self.rings.get(ring_b).and_then(|r| r.get(edge_b)) else {
            return false;
        };
        match axis {
            Axis::Horizontal => va.y_expr == vb.y_expr,
            Axis::Vertical => va.x_expr == vb.x_expr,
        }
    }

    fn recompute_active_parameter_count(&mut self) {
        let mut used: HashSet<ParameterId> = HashSet::new();
        for ring in &self.rings {
            for v in ring {
                if let CoordinateExpression::Parameter(id) = &v.x_expr {
                    used.insert(*id);
                }
                if let CoordinateExpression::Parameter(id) = &v.y_expr {
                    used.insert(*id);
                }
            }
        }
        self.active_parameter_count = used.len();
    }
}

struct EdgeInfo {
    ring: usize,
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
                continue; // already collinear
            }
            let diff = (edges[i].coord - edges[j].coord).abs();
            if diff > min_separation && diff <= max_separation {
                out.push(CollinearitySuggestion {
                    ring_a: edges[i].ring,
                    edge_a: edges[i].start,
                    ring_b: edges[j].ring,
                    edge_b: edges[j].start,
                    axis: axis.clone(),
                    coord_a: edges[i].coord,
                    coord_b: edges[j].coord,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_plan(points: &[(f32, f32)]) -> ParametricPlan {
        let pts: Vec<Point2f32> = points.iter().map(|(x, y)| Point2f32::new(*x, *y)).collect();
        let mut plan = ParametricPlan::from_points(&pts);
        plan.apply_orthogonal_constraints().unwrap();
        plan
    }

    #[test]
    fn suggest_collinear_returns_empty_for_clean_l() {
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
        let plan = build_plan(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 50.0),
            (200.0, 50.0),
            (200.0, 80.0),
            (0.0, 80.0),
        ]);
        let suggestions = plan.suggest_collinear(20.0, 60.0);
        assert!(!suggestions.is_empty());
    }

    #[test]
    fn apply_collinear_suggestion_reduces_parameter_count() {
        let mut plan = build_plan(&[
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
        plan.apply_collinear_suggestion(&suggestions[0]);
        assert!(
            plan.active_parameter_count < initial,
            "applying a collinear suggestion should reduce the parameter count (was {}, now {})",
            initial,
            plan.active_parameter_count,
        );
    }

    #[test]
    fn edges_collinear_after_apply_returns_true() {
        let mut plan = build_plan(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 50.0),
            (200.0, 50.0),
            (200.0, 80.0),
            (0.0, 80.0),
        ]);
        let suggestions = plan.suggest_collinear(20.0, 60.0);
        let sug = suggestions.first().expect("at least one suggestion");

        assert!(!plan.edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis));
        plan.apply_collinear_suggestion(sug);
        assert!(plan.edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis));
    }

    #[test]
    fn test_parametric_rectangle() {
        let points = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(10.0, 0.0),
            Point2f32::new(10.0, 5.0),
            Point2f32::new(0.0, 5.0),
        ];

        let mut plan = ParametricPlan::from_points(&points);

        assert_eq!(plan.active_parameter_count, 6);

        plan.apply_orthogonal_constraints().unwrap();

        assert_eq!(plan.active_parameter_count, 2);

        plan.set_parameter(0, 15.0);
        let result = plan.ring_points(0);
        assert!((result[1].x - 15.0).abs() < 0.01);
    }

    // -------- Multi-ring tests --------

    #[test]
    fn add_ring_increases_ring_count() {
        let mut plan = build_plan(&[(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 5.0)]);
        assert_eq!(plan.ring_count(), 1);
        let other = [
            Point2f32::new(50.0, 50.0),
            Point2f32::new(60.0, 50.0),
            Point2f32::new(60.0, 55.0),
            Point2f32::new(50.0, 55.0),
        ];
        plan.add_ring(&other);
        plan.apply_orthogonal_constraints().unwrap();
        assert_eq!(plan.ring_count(), 2);
        assert_eq!(plan.ring_points(1).len(), 4);
    }

    // -------- Stable-ID tests --------

    #[test]
    fn vertices_get_unique_monotonic_ids() {
        let plan = build_plan(&[(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 5.0)]);
        let ids: Vec<_> = plan.rings[0].iter().map(|v| v.id).collect();
        assert_eq!(ids, vec![0, 1, 2, 3]);
        let unique: HashSet<_> = ids.iter().copied().collect();
        assert_eq!(unique.len(), 4);
    }

    #[test]
    fn add_ring_continues_id_sequence() {
        let mut plan = build_plan(&[(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 5.0)]);
        plan.add_ring(&[
            Point2f32::new(50.0, 50.0),
            Point2f32::new(60.0, 50.0),
            Point2f32::new(60.0, 55.0),
            Point2f32::new(50.0, 55.0),
        ]);
        plan.apply_orthogonal_constraints().unwrap();

        let r0: Vec<_> = plan.rings[0].iter().map(|v| v.id).collect();
        let r1: Vec<_> = plan.rings[1].iter().map(|v| v.id).collect();
        assert_eq!(r0, vec![0, 1, 2, 3]);
        assert_eq!(r1, vec![4, 5, 6, 7]);
        // All distinct across rings — the namespace is shared and monotonic.
        let union: HashSet<_> = r0.iter().chain(r1.iter()).copied().collect();
        assert_eq!(union.len(), 8);
    }

    #[test]
    fn vertex_ids_survive_apply_orthogonal_constraints() {
        // Capture ids before the rebuild, run it, verify ids match position-by-position.
        let mut plan = ParametricPlan::from_points(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(100.0, 0.0),
            Point2f32::new(100.0, 50.0),
            Point2f32::new(0.0, 50.0),
        ]);
        let before: Vec<_> = plan.rings[0].iter().map(|v| v.id).collect();
        plan.apply_orthogonal_constraints().unwrap();
        let after: Vec<_> = plan.rings[0].iter().map(|v| v.id).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn vertex_ids_survive_collinearity_apply() {
        let mut plan = build_plan(&[
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 50.0),
            (200.0, 50.0),
            (200.0, 80.0),
            (0.0, 80.0),
        ]);
        let before: Vec<_> = plan.rings[0].iter().map(|v| v.id).collect();
        let sug = plan
            .suggest_collinear(20.0, 60.0)
            .into_iter()
            .next()
            .expect("suggestion");
        plan.apply_collinear_suggestion(&sug);
        let after: Vec<_> = plan.rings[0].iter().map(|v| v.id).collect();
        assert_eq!(
            before, after,
            "apply_collinear_suggestion must not mint new vertex IDs"
        );
    }

    #[test]
    fn find_vertex_round_trips() {
        let plan = build_plan(&[(0.0, 0.0), (10.0, 0.0), (10.0, 5.0), (0.0, 5.0)]);
        for (expected_ring, expected_idx) in [(0, 0), (0, 1), (0, 2), (0, 3)] {
            let id = plan.vertex_id_at(expected_ring, expected_idx).unwrap();
            let (r, v) = plan.find_vertex(id).unwrap();
            assert_eq!((r, v), (expected_ring, expected_idx));
        }
        assert!(plan.find_vertex(99999).is_none());
    }

    #[test]
    fn cross_ring_collinearity_can_be_suggested_and_applied() {
        // Two rectangles whose top edges differ by 30px in y (5 vs 35).
        // Their tops should be flagged as near-collinear across rings.
        let mut plan = ParametricPlan::from_points(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(100.0, 0.0),
            Point2f32::new(100.0, 5.0),
            Point2f32::new(0.0, 5.0),
        ]);
        plan.add_ring(&[
            Point2f32::new(200.0, 30.0),
            Point2f32::new(300.0, 30.0),
            Point2f32::new(300.0, 35.0),
            Point2f32::new(200.0, 35.0),
        ]);
        plan.apply_orthogonal_constraints().unwrap();

        let suggestions = plan.suggest_collinear(20.0, 60.0);
        // Expect at least one cross-ring suggestion (the two close edges).
        let cross_ring = suggestions.iter().find(|s| s.ring_a != s.ring_b);
        assert!(
            cross_ring.is_some(),
            "expected at least one cross-ring suggestion, got {:?}",
            suggestions
        );

        let sug = cross_ring.unwrap().clone();
        let before = plan.active_parameter_count;
        plan.apply_collinear_suggestion(&sug);
        assert!(
            plan.active_parameter_count < before,
            "cross-ring apply should reduce active params (was {}, now {})",
            before,
            plan.active_parameter_count,
        );
        assert!(plan.edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis));
    }
}
