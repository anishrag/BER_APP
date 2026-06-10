//! The parametric model query/edit API of `FloorPlan`.
//!
//! Every method here delegates to `self.parametric` (the `ParametricPlan`) —
//! reading or mutating parameters, vertices, walls, collinearity. They ARE
//! `FloorPlan`'s behaviour over its own model (not pure `ring → FloorPlan`
//! geometry like `carve`), so they stay methods; this split is organisational.
use crate::parametric::{Axis, CollinearitySuggestion, ParameterAxis};
use crate::{geom, orthogonal, parametric};
use crate::{Error, FloorPlan, ParameterId, VertexId};
use domain::points::Point2f32;
use std::collections::HashSet;

impl FloorPlan {
    /// Edges that are neither horizontal nor vertical (off by more than 1mm on
    /// BOTH axes) — i.e. visibly slanted "not straight" walls. Returns
    /// `(ring, edge_start, a, b)` for each. Empty when the plan is fully
    /// orthogonal. A diagnostic for confirming orthogonality.
    pub fn non_orthogonal_edges(&self) -> Vec<(usize, usize, Point2f32, Point2f32)> {
        let eps = 1e-3_f32;
        let mut out = Vec::new();
        for (r, ring) in self.parametric.all_ring_points().into_iter().enumerate() {
            let n = ring.len();
            if n < 3 {
                continue;
            }
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                if (a.x - b.x).abs() >= eps && (a.y - b.y).abs() >= eps {
                    out.push((r, i, a, b));
                }
            }
        }
        out
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

    /// Collinearity suggestions that involve at least one of `new_params` —
    /// the walls a just-applied extend/trim added. Lets the caller
    /// ask whether a freshly-drawn wall lines up with an existing one, WITHOUT
    /// re-surfacing pairs among the pre-existing walls. Separation thresholds
    /// scale with the bounding-box diagonal, so it behaves the same in pixel
    /// units (pre-dimensioning) and metres (post-dimensioning).
    pub fn collinear_suggestions_for_params(
        &self,
        new_params: &[ParameterId],
    ) -> Vec<CollinearitySuggestion> {
        let diag = self.bounding_box_diagonal();
        if diag <= 0.0 {
            return Vec::new();
        }
        // No lower bound: freehand drawing isn't precise, so two walls landing
        // close together — even exactly coincident — is chance, not intent.
        // Those are still independent parameters, so we ask rather than assume
        // (`suggest_collinear` only auto-skips walls that already SHARE an
        // expression, i.e. are already the same parameter). Beyond ~12% of the
        // diagonal the walls are clearly distinct — that stays the upper cutoff.
        let min_sep = 0.0;
        let max_sep = (diag * 0.12).max(1e-3);
        let new: std::collections::HashSet<ParameterId> = new_params.iter().copied().collect();
        self.parametric
            .suggest_collinear(min_sep, max_sep)
            .into_iter()
            .filter(|s| {
                let involves = |ring: usize, edge: usize| {
                    self.parametric
                        .wall_movable_param(ring, edge)
                        .is_some_and(|(p, _)| new.contains(&p))
                };
                involves(s.ring_a, s.edge_a) || involves(s.ring_b, s.edge_b)
            })
            .collect()
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

    /// Get the stable ID of the vertex at `(ring, vertex_idx)`. Use this when
    /// you need a reference that survives later edits (e.g. for dimension
    /// prompts that should outlive intervening reflows).
    pub fn vertex_id_at(&self, ring: usize, vertex_idx: usize) -> Option<VertexId> {
        self.parametric.vertex_id_at(ring, vertex_idx)
    }

    /// Look up a vertex by stable ID. Returns `(ring_index, vertex_index)`.
    pub fn find_vertex(&self, id: VertexId) -> Option<(usize, usize)> {
        self.parametric.find_vertex(id)
    }

    /// For an edge connecting vertices `a` and `b` (e.g. an arc's placeholder
    /// chord), the RING-START vertex of that edge — the one prompt logic keys the
    /// wall by. `None` if the two aren't adjacent on a single ring.
    pub fn adjacent_edge_start(&self, a: VertexId, b: VertexId) -> Option<VertexId> {
        let (ra, ia) = self.find_vertex(a)?;
        let (rb, ib) = self.find_vertex(b)?;
        if ra != rb {
            return None;
        }
        let n = self.parametric.ring_points(ra).len();
        if n == 0 {
            return None;
        }
        if (ia + 1) % n == ib {
            Some(a)
        } else if (ib + 1) % n == ia {
            Some(b)
        } else {
            None
        }
    }

    /// The outline perimeter as walls: for ring 0, each edge's stable start
    /// `VertexId` together with its two endpoint coordinates `(a, b)`, in ring
    /// order. The editor uses this to derive heat-loss wall regions and to key
    /// per-wall annotations (e.g. exposure) by the stable id, which survives
    /// edits.
    pub fn perimeter_walls(&self) -> Vec<(VertexId, Point2f32, Point2f32)> {
        let ring = self.get_points();
        let n = ring.len();
        if n < 2 {
            return Vec::new();
        }
        (0..n)
            .filter_map(|i| {
                self.vertex_id_at(0, i)
                    .map(|vid| (vid, ring[i], ring[(i + 1) % n]))
            })
            .collect()
    }

    /// Current length of the wall starting at `(ring, edge_start)` in
    /// whatever units the plan is currently in (pixels before the first
    /// dimension, real units after).
    pub fn wall_length(&self, ring: usize, edge_start: usize) -> Option<f32> {
        self.parametric.wall_length(ring, edge_start)
    }

    /// Parameter IDs the wall's length depends on. For an axis-aligned wall,
    /// usually one (e.g. a wall whose start is at a constant and end is at
    /// a parameter) or two (a wall between two parameter coordinates).
    pub fn wall_param_deps(&self, ring: usize, edge_start: usize) -> Vec<ParameterId> {
        self.parametric.wall_param_deps(ring, edge_start)
    }

    /// All parameter IDs currently referenced by any vertex.
    pub fn all_active_params(&self) -> std::collections::HashSet<ParameterId> {
        self.parametric.all_active_params()
    }

    /// Multiply every parameter value (and every constant) by `factor`. Used
    /// when the first user-entered dimension converts the pixel-scale polygon
    /// into real-world units uniformly.
    pub fn scale_all(&mut self, factor: f32) {
        self.parametric.scale_all(factor);
    }

    /// Translate every coordinate in the plan by `(dx, dy)`: each X/Y
    /// parameter value and each X/Y Constant. Wall lengths are invariant
    /// (pure rigid translation); parameter IDs and expressions are untouched.
    /// Used to pin a position-dimensioned area's free axis by rigid-moving
    /// the whole area so a measured wall lands exactly.
    pub fn translate_all(&mut self, dx: f32, dy: f32) {
        self.parametric.translate_all(dx, dy);
    }

    /// Set the value of a single parameter directly. Used to apply a user-
    /// entered dimension to the unknown parameter a wall constrains.
    pub fn set_parameter(&mut self, param_id: ParameterId, value: f32) {
        self.parametric.set_parameter(param_id, value);
    }

    /// Change `unknown_param` so the wall has the given length. Returns an
    /// error if the wall isn't axis-aligned or doesn't reference the param.
    pub fn set_wall_length(
        &mut self,
        ring: usize,
        edge_start: usize,
        new_length: f32,
        unknown_param: ParameterId,
    ) -> Result<(), Error> {
        self.parametric
            .set_wall_length(ring, edge_start, new_length, unknown_param)
    }

    /// Validate that every ring is a simple axis-aligned polygon. Used after
    /// applying a dimension to confirm the user's input is consistent with the
    /// shape — e.g. for an E, the right-side vertical segments must collectively
    /// span the same height as the spine.
    pub fn validate_simple(&self) -> Result<(), Error> {
        self.parametric.validate_simple()
    }

    /// Which Cartesian axis the given parameter controls.
    pub fn param_axis(&self, p: ParameterId) -> Option<ParameterAxis> {
        self.parametric.param_axis(p)
    }

    /// All walls in the plan whose length expression matches the wall at
    /// `(ring, edge_start)`. Returns `(ring, edge_start)` tuples including
    /// the input wall. The UI uses this to show the user every wall they
    /// can equivalently measure to satisfy a dimension prompt.
    pub fn equivalent_length_walls(&self, ring: usize, edge_start: usize) -> Vec<(usize, usize)> {
        self.parametric.equivalent_length_walls(ring, edge_start)
    }

    /// For an axis-aligned wall, the parameter that translates it under a
    /// perpendicular drag and the axis that parameter controls. None if the
    /// wall's perpendicular coord is a constant (anchored to V0).
    pub fn wall_movable_param(
        &self,
        ring: usize,
        edge_start: usize,
    ) -> Option<(ParameterId, ParameterAxis)> {
        self.parametric.wall_movable_param(ring, edge_start)
    }

    /// Insert a new vertex on the wall at `(ring, edge_start)`, placing it
    /// at world position `(world_x, world_y)`. Splits one edge into two
    /// and allocates two fresh parameters (one per axis) for the new
    /// vertex. Returns its new `VertexId`, or `None` if the ring/edge
    /// indices are out of range.
    pub fn insert_vertex_on_wall(
        &mut self,
        ring: usize,
        edge_start: usize,
        world_x: f32,
        world_y: f32,
    ) -> Option<VertexId> {
        self.parametric
            .insert_vertex_on_wall(ring, edge_start, world_x, world_y)
    }

    /// Whether the wall starting at `(ring, edge_start)` is horizontal,
    /// vertical, or non-axis-aligned (`None`). See
    /// [`ParametricPlan::wall_axis`](parametric::ParametricPlan::wall_axis).
    pub fn wall_axis(&self, ring: usize, edge_start: usize) -> Option<Axis> {
        self.parametric.wall_axis(ring, edge_start)
    }

    /// Remove the vertex at `(ring, idx)`, collapsing the walls around it. See
    /// [`ParametricPlan::remove_vertex`](parametric::ParametricPlan::remove_vertex).
    pub fn remove_vertex(&mut self, ring: usize, idx: usize) -> bool {
        self.parametric.remove_vertex(ring, idx)
    }

    /// Drop spurious collinear vertices the outline doesn't need (e.g. the
    /// corner a flush porch leaves mid-wall), keeping any in `preserve` (marked
    /// subdivision points such as a flooring-type boundary). Surviving vertices
    /// keep their IDs/params; returns the removed vertex IDs. See
    /// [`ParametricPlan::remove_collinear_vertices`](parametric::ParametricPlan::remove_collinear_vertices).
    pub fn remove_collinear_vertices(&mut self, preserve: &HashSet<VertexId>) -> Vec<VertexId> {
        self.parametric.remove_collinear_vertices(preserve)
    }

    /// Collapse a trim cut wall to zero (the wall identified by `edge_start` /
    /// `controlled_param`). See
    /// [`ParametricPlan::collapse_trim_wall`](parametric::ParametricPlan::collapse_trim_wall).
    pub fn collapse_trim_wall(
        &mut self,
        ring: usize,
        edge_start: usize,
        controlled_param: ParameterId,
    ) -> Result<(), Error> {
        self.parametric
            .collapse_trim_wall(ring, edge_start, controlled_param)
    }

    /// Every wall whose length is fully determined by `known` parameters — all
    /// the parameters in its length expression are known. A superset of the
    /// user-dimensioned walls (also catches lengths implied by shared
    /// parameters, e.g. the opposite side of a rectangle). Walls with no
    /// parameter deps (pre-dimensioning pixel artefacts) are excluded.
    pub fn walls_with_known_length(&self, known: &HashSet<ParameterId>) -> Vec<(usize, usize)> {
        let polygons = self.polygons();
        let mut result = Vec::new();
        for (ri, ring) in polygons.iter().enumerate() {
            for i in 0..ring.len() {
                let deps = self.wall_param_deps(ri, i);
                if !deps.is_empty() && deps.iter().all(|p| known.contains(p)) {
                    result.push((ri, i));
                }
            }
        }
        result
    }

    /// Pick the next wall to ask the user to dimension: the first wall (in ring
    /// order) whose length depends on exactly ONE not-yet-`known` parameter
    /// (the rest known) and whose start vertex isn't in `skipped`. Returns
    /// `(start vertex id, the one unknown parameter)`, or `None` when every
    /// active parameter is known or no clean single-unknown wall remains. The
    /// caller decides what to do with the result (the dimensioning workflow).
    pub fn next_promptable_wall(
        &self,
        known: &HashSet<ParameterId>,
        skipped: &HashSet<VertexId>,
    ) -> Option<(VertexId, ParameterId)> {
        let to_ask: HashSet<ParameterId> = self
            .all_active_params()
            .into_iter()
            .filter(|p| !known.contains(p))
            .collect();
        if to_ask.is_empty() {
            return None;
        }
        let polygons = self.polygons();
        for (ring_idx, ring_points) in polygons.iter().enumerate() {
            for edge_start in 0..ring_points.len() {
                let Some(vid) = self.vertex_id_at(ring_idx, edge_start) else {
                    continue;
                };
                if skipped.contains(&vid) {
                    continue;
                }
                // Don't prompt facet walls of a curve. A non-axis-aligned wall
                // is dimensionable (a Trim diagonal) only when it's ISOLATED —
                // both neighbours axis-aligned. A curve is a RUN of consecutive
                // non-axis-aligned facets, so each facet has a non-aligned
                // neighbour; those aren't individually dimensioned.
                if self.wall_axis(ring_idx, edge_start).is_none() {
                    let len = ring_points.len();
                    let prev = (edge_start + len - 1) % len;
                    let next = (edge_start + 1) % len;
                    if self.wall_axis(ring_idx, prev).is_none()
                        || self.wall_axis(ring_idx, next).is_none()
                    {
                        continue;
                    }
                }
                // Promptable iff exactly one dep is in `to_ask` and the rest are
                // known. A dep that's neither blocks this wall (its only setter
                // was skipped / never set).
                let mut to_ask_count = 0usize;
                let mut to_ask_param: Option<ParameterId> = None;
                let mut blocked = false;
                for d in self.wall_param_deps(ring_idx, edge_start) {
                    if to_ask.contains(&d) {
                        to_ask_count += 1;
                        to_ask_param = Some(d);
                    } else if !known.contains(&d) {
                        blocked = true;
                        break;
                    }
                }
                if !blocked && to_ask_count == 1 {
                    return Some((vid, to_ask_param.unwrap()));
                }
            }
        }
        None
    }

    /// For a wall whose perpendicular coord is a constant (V0's anchor or
    /// similar), the current constant value and the axis it lives on.
    /// Edit-mode drag uses this together with `shift_constant` to translate
    /// the polygon's anchor walls (leftmost / bottommost on an E, etc.).
    pub fn wall_anchor_constant(
        &self,
        ring: usize,
        edge_start: usize,
    ) -> Option<(f32, ParameterAxis)> {
        self.parametric.wall_anchor_constant(ring, edge_start)
    }

    /// Shift every vertex whose `axis`-coord equals `old_value` (within a
    /// small tolerance) to `new_value`. Companion to `wall_anchor_constant`.
    pub fn shift_constant(&mut self, old_value: f32, axis: ParameterAxis, new_value: f32) {
        self.parametric.shift_constant(old_value, axis, new_value);
    }

    /// Hit-test a world-coord point against the polygon's walls. Returns
    /// the `(ring, edge_start)` of the closest wall within `tol` (same
    /// units as the polygon coords), or `None` if every wall is farther.
    pub fn hit_test_wall(&self, world_x: f64, world_y: f64, tol: f64) -> Option<(usize, usize)> {
        let polygons = self.polygons();
        let mut best: Option<(f64, usize, usize)> = None;
        for (ri, ring) in polygons.iter().enumerate() {
            let n = ring.len();
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let d = domain::points::point_to_segment_distance(
                    world_x, world_y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                );
                if d <= tol && (best.is_none() || d < best.unwrap().0) {
                    best = Some((d, ri, i));
                }
            }
        }
        best.map(|(_, r, e)| (r, e))
    }

    /// The ParameterId controlling vertex `(ring, idx)`'s x coord, or None.
    pub fn vertex_x_param(&self, ring: usize, idx: usize) -> Option<ParameterId> {
        self.parametric.vertex_x_param(ring, idx)
    }

    /// The ParameterId controlling vertex `(ring, idx)`'s y coord, or None.
    pub fn vertex_y_param(&self, ring: usize, idx: usize) -> Option<ParameterId> {
        self.parametric.vertex_y_param(ring, idx)
    }

    /// Current value of a parameter (in the plan's current units), or None.
    pub fn parameter_value(&self, p: ParameterId) -> Option<f32> {
        self.parametric.parameter_value(p)
    }

    /// Multiply a single parameter's value by `factor`.
    pub fn scale_parameter(&mut self, param: ParameterId, factor: f32) {
        self.parametric.scale_parameter(param, factor);
    }

    /// Rebuild every ring with zero-length edges and collinear middle vertices
    /// removed, then re-run the orthogonal-constraints pass so coordinate
    /// expressions are clean. Used after the collinearity review has merged
    /// edges that were "the same line" — the merges can leave duplicate
    /// vertices (zero-length edges) and three-on-a-line vertices that
    /// shouldn't appear in dimensioning prompts or downstream area calcs.
    ///
    /// Side effect: vertex IDs are reassigned (the parametric layer is rebuilt
    /// from scratch). Only safe to call before dimensioning has started —
    /// otherwise the controller's `known_params` set would dangle. Caller
    /// is responsible for committing the simplified plan to the BER.
    pub fn simplify_degenerate(&mut self) {
        let tolerance: f32 = 0.5;
        let new_rings: Vec<Vec<Point2f32>> = self
            .polygons()
            .into_iter()
            .map(|ring| {
                let deduped = orthogonal::remove_duplicates(&ring, tolerance);
                orthogonal::remove_collinear(&deduped, tolerance)
            })
            .filter(|r| r.len() >= 3)
            .collect();

        if new_rings.is_empty() {
            self.parametric = parametric::ParametricPlan::default();
            return;
        }

        let mut new_parametric = parametric::ParametricPlan::from_points(&new_rings[0]);
        for ring in new_rings.iter().skip(1) {
            new_parametric.add_ring(ring);
        }
        let _ = new_parametric.apply_orthogonal_constraints();
        self.parametric = new_parametric;
    }

    /// Bounding box of the entire plan (across all rings) in current units.
    pub fn bounding_box(&self) -> Option<((f32, f32), (f32, f32))> {
        let pts: Vec<Point2f32> = self.polygons().into_iter().flatten().collect();
        geom::bbox(&pts)
    }

    /// Diagonal of this plan's bounding box (0.0 when empty). The canonical
    /// "overall size" used to scale tolerances; callers in the app layer reach
    /// it via this method or the free [`ring_diagonal`] over a raw ring.
    pub fn bounding_box_diagonal(&self) -> f32 {
        let pts: Vec<Point2f32> = self.polygons().into_iter().flatten().collect();
        geom::bbox_diagonal(&pts)
    }

    /// Per-edge direction signs, for the direction-flip guard. See
    /// [`ParametricPlan::edge_signs`](parametric::ParametricPlan::edge_signs).
    pub fn edge_signs(&self) -> Vec<Vec<(i8, i8)>> {
        self.parametric.edge_signs()
    }

    /// Repair an invalid polygon by redistributing the parameters not in
    /// `known` along `axis`. See
    /// [`ParametricPlan::repair_unset_params`](parametric::ParametricPlan::repair_unset_params).
    pub fn repair_unset_params(
        &mut self,
        axis: ParameterAxis,
        known: &HashSet<ParameterId>,
        snapshot: &FloorPlan,
    ) -> bool {
        self.parametric
            .repair_unset_params(axis, known, &snapshot.parametric)
    }

    /// Nudge an extreme aspect ratio back toward ~3:1 by scaling un-set
    /// parameters. See
    /// [`ParametricPlan::aesthetic_rescale`](parametric::ParametricPlan::aesthetic_rescale).
    pub fn aesthetic_rescale(&mut self, known: &HashSet<ParameterId>) {
        self.parametric.aesthetic_rescale(known)
    }
}
