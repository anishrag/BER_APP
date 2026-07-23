//! The parametric model query/edit API of `FloorPlan`.
//!
//! Every method here delegates to `self.parametric` (the `ParametricPlan`) —
//! reading or mutating parameters, vertices, walls, collinearity. They ARE
//! `FloorPlan`'s behaviour over its own model (not pure `ring → FloorPlan`
//! geometry like `carve`), so they stay methods; this split is organisational.
use crate::parametric::{Axis, CollinearitySuggestion, ParameterAxis};
use crate::{geom, orthogonal, parametric};
use crate::{Error, FloorPlan, ParameterId, VertexId};
use domain::points::{point_on_ring_boundary, point_to_segment_distance, to_f64, Point2f32, Point2f64};
use std::collections::HashSet;

impl FloorPlan {
    /// The interior edges of a dimensioned cut `ring` against this outline —
    /// every ring edge whose midpoint lies strictly inside the outline (the
    /// edges that DON'T run along it). Each is a fixed cut line stored as a wall
    /// split: a straight bisecting cut yields one, a corner/notch carve yields
    /// several (each crossing a DIFFERENT wall). Returning them all is safe —
    /// `WallRegions::apply_splits` only divides a wall a cut actually crosses
    /// mid-span, so an interior edge that reaches no wall is a harmless no-op.
    ///
    /// A boundary edge must be EXCLUDED explicitly: `ring_contains_point` is
    /// unreliable for a point sitting exactly on the ring (it can report a
    /// boundary-edge midpoint as "inside"), which would otherwise pick a wall
    /// edge — collinear with that wall, so it splits nothing. The
    /// strictly-interior test is therefore "inside AND not on the boundary".
    pub fn interior_cut_edges(&self, ring: &[Point2f32]) -> Vec<[Point2f64; 2]> {
        /// How close (m) a midpoint must be to an outline edge to count as "on it".
        const ON_BOUNDARY_TOL_M: f64 = 1e-3;
        let op = self.get_points();
        let op64: Vec<Point2f64> = op.iter().map(|p| to_f64(*p)).collect();
        let n = ring.len();
        let m = op64.len();
        let mut edges = Vec::new();
        for i in 0..n {
            let a64 = to_f64(ring[i]);
            let b64 = to_f64(ring[(i + 1) % n]);
            // A MIXED edge — partly along the outline, partly interior (a
            // porch drawn so its inner edge continues into an outline jog) —
            // must SPLIT, not classify whole: collect the outline-covered
            // t-intervals along a→b, cut there, then midpoint-test each
            // sub-segment as before.
            let (dx, dy) = (b64.x - a64.x, b64.y - a64.y);
            let len2 = dx * dx + dy * dy;
            if len2 < 1e-12 {
                continue;
            }
            let mut ts = vec![0.0_f64, 1.0];
            for j in 0..m {
                let (p, q) = (op64[j], op64[(j + 1) % m]);
                let tp = (((p.x - a64.x) * dx + (p.y - a64.y) * dy) / len2).clamp(0.0, 1.0);
                let tq = (((q.x - a64.x) * dx + (q.y - a64.y) * dy) / len2).clamp(0.0, 1.0);
                let (lo, hi) = if tp <= tq { (tp, tq) } else { (tq, tp) };
                if hi - lo < 1e-9 {
                    continue;
                }
                // Real coverage only: the candidate sub-interval's midpoint
                // must actually lie on THIS outline segment.
                let tm = (lo + hi) / 2.0;
                let (mx, my) = (a64.x + tm * dx, a64.y + tm * dy);
                if point_to_segment_distance(mx, my, p.x, p.y, q.x, q.y) <= ON_BOUNDARY_TOL_M {
                    ts.push(lo);
                    ts.push(hi);
                }
            }
            ts.sort_by(|x, y| x.partial_cmp(y).unwrap());
            ts.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
            for w in ts.windows(2) {
                let (t0, t1) = (w[0], w[1]);
                if (t1 - t0) * len2.sqrt() < ON_BOUNDARY_TOL_M {
                    continue; // sliver
                }
                let tm = (t0 + t1) / 2.0;
                let (mx, my) = (a64.x + tm * dx, a64.y + tm * dy);
                if !point_on_ring_boundary(&op64, mx, my, ON_BOUNDARY_TOL_M)
                    && geom::ring_contains_point(&op, mx as f32, my as f32)
                {
                    edges.push([
                        Point2f64::new(a64.x + t0 * dx, a64.y + t0 * dy),
                        Point2f64::new(a64.x + t1 * dx, a64.y + t1 * dy),
                    ]);
                }
            }
        }
        edges
    }

    /// The world-coord endpoints `(a, b)` of the wall starting at vertex `vid`
    /// — the ring edge from `vid` to its successor. `None` when the vertex is
    /// gone (a stale prompt) or its ring is degenerate (< 2 vertices).
    pub fn wall_endpoints(&self, vid: VertexId) -> Option<(Point2f32, Point2f32)> {
        let (ring, edge_start) = self.find_vertex(vid)?;
        let polygons = self.polygons();
        let pts = polygons.get(ring)?;
        let n = pts.len();
        if n < 2 {
            return None;
        }
        let a = *pts.get(edge_start)?;
        let b = *pts.get((edge_start + 1) % n)?;
        Some((a, b))
    }

    /// The normalized position (0.0 = start, 1.0 = end) of `point` projected
    /// onto the wall starting at vertex `start`. This is the durable along-key
    /// for items attached to a wall (openings, split points): it's invariant
    /// under the wall translating or rescaling, because it's a fraction of the
    /// wall's own length. Clamped to `[0, 1]`. `None` when the vertex is gone or
    /// its edge is degenerate.
    pub fn wall_along_fraction(&self, start: VertexId, point: Point2f32) -> Option<f32> {
        let (a, b) = self.wall_endpoints(start)?;
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len2 = dx * dx + dy * dy;
        if len2 <= f32::EPSILON {
            return Some(0.0);
        }
        let t = ((point.x - a.x) * dx + (point.y - a.y) * dy) / len2;
        Some(t.clamp(0.0, 1.0))
    }

    /// The world point at along-`fraction` of the wall starting at vertex
    /// `start` — the inverse of [`wall_along_fraction`](Self::wall_along_fraction).
    /// `fraction` is clamped to `[0, 1]`. `None` when the vertex is gone or its
    /// edge is degenerate.
    pub fn wall_point_at_fraction(&self, start: VertexId, fraction: f32) -> Option<Point2f32> {
        let (a, b) = self.wall_endpoints(start)?;
        let t = fraction.clamp(0.0, 1.0);
        Some(Point2f32::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t))
    }

    /// The ring-0 perimeter wall nearest to `point` (within `tol`), as a
    /// [`SurfaceRef::Wall`](crate::SurfaceRef) keyed by its stable start
    /// `VertexId`. Used to anchor an opening placed on a resolved wall back onto
    /// the durable parametric outline. `None` when no perimeter edge is within
    /// `tol`.
    pub fn surface_ref_at(&self, point: Point2f32, tol: f32) -> Option<crate::SurfaceRef> {
        let mut best: Option<(f64, VertexId)> = None;
        for (vid, a, b) in self.perimeter_walls() {
            let d = domain::points::point_to_segment_distance(
                point.x as f64,
                point.y as f64,
                a.x as f64,
                a.y as f64,
                b.x as f64,
                b.y as f64,
            );
            if d <= tol as f64 && best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, vid));
            }
        }
        best.map(|(_, vid)| crate::SurfaceRef::wall(vid))
    }

    /// For each wall in ring 0 whose length is KNOWN (its parameter is in
    /// `known_params`), its endpoints and length: `((ax, ay), (bx, by), len)`
    /// with coordinates in f64. The label set the UI draws on dimensioned
    /// living-area walls. Empty for a degenerate ring (< 3 vertices).
    pub fn known_length_wall_segments(
        &self,
        known_params: &HashSet<ParameterId>,
    ) -> Vec<((f64, f64), (f64, f64), f32)> {
        let pts = self.get_points();
        let n = pts.len();
        if n < 3 {
            return Vec::new();
        }
        self.walls_with_known_length(known_params)
            .into_iter()
            .filter(|(ring, _)| *ring == 0)
            .filter_map(|(ring, e)| {
                let a = pts[e % n];
                let b = pts[(e + 1) % n];
                let len = self.wall_length(ring, e)?;
                Some(((a.x as f64, a.y as f64), (b.x as f64, b.y as f64), len))
            })
            .collect()
    }

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

    /// For the axis-aligned wall `(ring, edge_start)`, the moved wall's CROSS
    /// axis plus every OTHER wall PARALLEL to it (same orientation), each as
    /// `(reference wall's start vid, reference coordinate, sign, distance)` —
    /// the moved wall's perpendicular coordinate measured from that reference.
    /// Coincident walls (within 1 mm) and the moved wall itself are skipped;
    /// order is ring/edge traversal order (stable, so the same wall shows first
    /// across recomputes). `None` when the wall is degenerate (< 2 vertices).
    ///
    /// This is the discovery walk behind the shell's post-drag measurement
    /// prompt — plan topology + coordinate math, so it lives on `FloorPlan`; the
    /// shell only assembles its `MeasurementOption`s from the result.
    pub fn parallel_walls_to(
        &self,
        ring: usize,
        edge_start: usize,
    ) -> Option<(ParameterAxis, Vec<(VertexId, f32, f32, f32)>)> {
        let polygons = self.polygons();
        let r0 = polygons.get(ring)?;
        let n = r0.len();
        if n < 2 {
            return None;
        }
        let a = r0[edge_start];
        let b = r0[(edge_start + 1) % n];
        // Orientation of the moved wall: vertical (its X is the cross coord) vs
        // horizontal (its Y is). Tolerance-free abs compare, like the old walk.
        let is_vertical = (b.y - a.y).abs() > (b.x - a.x).abs();
        let axis = if is_vertical {
            ParameterAxis::X
        } else {
            ParameterAxis::Y
        };
        let moved_value = if is_vertical { a.x } else { a.y };
        let mut out: Vec<(VertexId, f32, f32, f32)> = Vec::new();
        for (ri, r) in polygons.iter().enumerate() {
            for i in 0..r.len() {
                if ri == ring && i == edge_start {
                    continue;
                }
                let (p, q) = (r[i], r[(i + 1) % r.len()]);
                let other_is_vertical = (q.y - p.y).abs() > (q.x - p.x).abs();
                if other_is_vertical != is_vertical {
                    continue; // not parallel to the moved wall
                }
                let ref_value = if is_vertical { p.x } else { p.y };
                let delta = moved_value - ref_value;
                let distance = delta.abs();
                if distance < 1e-3 {
                    continue; // coincident — no measurement to form
                }
                if let Some(ref_vid) = self.vertex_id_at(ri, i) {
                    out.push((ref_vid, ref_value, delta.signum(), distance));
                }
            }
        }
        Some((axis, out))
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
    /// Test-only: not on any app path (vertex removal goes via trim/splice).
    #[cfg(test)]
    pub(crate) fn remove_vertex(&mut self, ring: usize, idx: usize) -> bool {
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

#[cfg(test)]
mod along_fraction_tests {
    use super::*;
    use crate::SurfaceRef;

    fn square() -> FloorPlan {
        let pts = [
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 3.0),
            Point2f32::new(0.0, 3.0),
        ];
        FloorPlan::from_points_to_orthogonal(&pts, 0.01, true).unwrap()
    }

    #[test]
    fn midpoint_is_half() {
        let plan = square();
        let walls = plan.perimeter_walls();
        assert_eq!(walls.len(), 4);
        for (vid, a, b) in walls {
            let mid = Point2f32::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            let f = plan.wall_along_fraction(vid, mid).unwrap();
            assert!((f - 0.5).abs() < 1e-4, "mid fraction {f}");
        }
    }

    #[test]
    fn round_trips_point_and_fraction() {
        let plan = square();
        for (vid, _a, _b) in plan.perimeter_walls() {
            for frac in [0.0_f32, 0.25, 0.5, 0.75, 1.0] {
                let p = plan.wall_point_at_fraction(vid, frac).unwrap();
                let back = plan.wall_along_fraction(vid, p).unwrap();
                assert!((back - frac).abs() < 1e-4, "round trip {frac} -> {back}");
            }
        }
    }

    #[test]
    fn endpoints_are_zero_and_one() {
        let plan = square();
        for (vid, a, b) in plan.perimeter_walls() {
            assert!(plan.wall_along_fraction(vid, a).unwrap() < 1e-4);
            assert!((plan.wall_along_fraction(vid, b).unwrap() - 1.0).abs() < 1e-4);
        }
    }

    #[test]
    fn missing_vertex_is_none() {
        let plan = square();
        let p = Point2f32::new(0.0, 0.0);
        assert!(plan.wall_along_fraction(u64::MAX, p).is_none());
        assert!(plan.wall_point_at_fraction(u64::MAX, 0.5).is_none());
    }

    #[test]
    fn surface_ref_wall_accessor() {
        assert_eq!(SurfaceRef::wall(7).wall_start(), Some(7));
    }
}
