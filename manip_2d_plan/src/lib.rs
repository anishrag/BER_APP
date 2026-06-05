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

mod arc;
mod boundary_follow;
mod carve;
mod error;
mod extend;
mod geom;
mod orthogonal;
mod parametric;
mod query;
mod simplification;
mod trim;

pub use error::Error;
pub use extend::snap_stroke_to_ring;
pub use geom::{
    bbox_diagonal as ring_diagonal, find_flipped_edge, intersect_circles, project_to_circle,
    ring_contains_point, solve_axis_wall,
};
pub use parametric::{Axis, CollinearitySuggestion, ParameterAxis, ParameterId, VertexId};

use domain::points::Point2f32;

/// Dimensionless tuning knobs for [`FloorPlan::outline_following_trace`] (the
/// new-storey splice). Defaults reproduce the values used before these were
/// extracted; the app overrides them from `tuning.yaml`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct SpliceTuning {
    /// A STRAIGHT boundary wall is "followed" only within `follow_tol * this`, so
    /// a roughly-straight wall drawn a little off it keeps its own straight line
    /// instead of clinging onto the boundary. CURVE facets always use the full
    /// `follow_tol` (so freehand-traced curves stay preserved).
    pub straight_cling_factor: f32,
    /// A "following" graze shorter than `follow_tol * this`, flanked by diverging
    /// runs, is flipped to diverging — keeps a new wall that brushes an interior
    /// edge in one piece instead of splitting it.
    pub graze_smoothing_factor: f32,
    /// New-wall squaring: an edge is "near-axis" when its off-axis delta is below
    /// `(diag * axis_tol_ratio).max(axis_tol_floor)`.
    pub axis_tol_ratio: f32,
    pub axis_tol_floor: f32,
    /// New-wall squaring: snap only when the long side exceeds `aspect ×` the
    /// short side (clearly an axis wall, not a diagonal curve facet).
    pub aspect: f32,
}

impl Default for SpliceTuning {
    fn default() -> Self {
        Self {
            straight_cling_factor: 0.4,
            graze_smoothing_factor: 3.5,
            axis_tol_ratio: 0.06,
            axis_tol_floor: 0.05,
            aspect: 6.0,
        }
    }
}

/// Tuning for sub-area carves ([`FloorPlan::carve_subarea`] — living areas and
/// construction-phase areas): the closed-loop gap and the boundary-follow
/// classifier band. Defaults reproduce the values used before extraction; the
/// app overrides them from `tuning.yaml`.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct SubareaTuning {
    /// A stroke is a CLOSED loop when its first and last points are within this
    /// many METRES (sub-areas are drawn on a dimensioned outline, so absolute).
    pub close_gap_m: f32,
    /// Boundary-follow classifier: a stroke point counts as ON the outline when
    /// within `on_tol` of it, and as having LEFT when past `off_tol` (hysteresis
    /// band; `off_tol > on_tol`). The tolerances are `diag * on_ratio` /
    /// `diag * off_ratio` but CEILINGED at `on_tol_ceil_m` / `off_tol_ceil_m`
    /// metres: without the ceiling a large region inflates them (a 5 m region →
    /// 0.44 m `off_tol`) so an interior wall drawn ~0.4 m off a wall never
    /// "leaves" the boundary and gets swallowed into it. The ceiling keeps the
    /// snap band at hand-precision regardless of region size.
    pub on_ratio: f32,
    pub off_ratio: f32,
    pub on_tol_ceil_m: f32,
    pub off_tol_ceil_m: f32,
    /// RDP / orthogonalization tolerance for the carve, as
    /// `(stroke_diag * rdp_tol_ratio).max(rdp_tol_floor)` (metres). Higher =
    /// the freehand stroke is simplified more coarsely.
    pub rdp_tol_ratio: f32,
    pub rdp_tol_floor: f32,
    /// COARSE "drop hand-wander" simplify tolerance, `simplify_ratio * extent`,
    /// CEILINGED at `simplify_ceil_m` metres (and floored at the fine carve tol).
    /// Used BOTH by the straight cut (extent = the cut's longest span) and by the
    /// carve's interior join (extent = the run's diagonal): a roughly-straight
    /// wall collapses to a clean line instead of staircasing, while a real step ≥
    /// the ceiling always survives. See [`SubareaTuning::coarse_simplify_tol`].
    pub simplify_ratio: f32,
    pub simplify_ceil_m: f32,
    /// A cut endpoint snaps onto an outline VERTEX within `diag * cut_vertex_tol_ratio`,
    /// and the cut is only accepted if both ends REACH the outline within
    /// `diag * cut_reach_tol_ratio`.
    pub cut_vertex_tol_ratio: f32,
    pub cut_reach_tol_ratio: f32,
}

impl Default for SubareaTuning {
    fn default() -> Self {
        Self {
            close_gap_m: 0.7,
            on_ratio: 0.035,
            off_ratio: 0.09,
            on_tol_ceil_m: 0.1,
            off_tol_ceil_m: 0.25,
            rdp_tol_ratio: 0.02,
            rdp_tol_floor: 0.001,
            simplify_ratio: 0.1,
            simplify_ceil_m: 0.7,
            cut_vertex_tol_ratio: 0.03,
            cut_reach_tol_ratio: 0.06,
        }
    }
}

impl SubareaTuning {
    /// The COARSE simplify tolerance for a sub-area stroke of size `extent`
    /// (metres): `(extent * simplify_ratio).max(floor).min(simplify_ceil_m)`.
    /// `floor` is the fine carve/cut RDP tolerance — the coarse value never goes
    /// below it. Single source for both the straight-cut simplify and the carve's
    /// interior-join simplify, so "drop hand-wander but keep real steps" is tuned
    /// in one place.
    pub(crate) fn coarse_simplify_tol(&self, extent: f32, floor: f32) -> f32 {
        (extent * self.simplify_ratio)
            .max(floor)
            .min(self.simplify_ceil_m)
    }
}

/// A processed floor plan: orthogonal polygon as a parametric representation
/// (shared coordinate expressions for collinear walls, one parameter per
/// independent dimension).
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
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
        Self::from_points_oriented(points, tolerance, closed, None)
    }

    /// Like [`from_points_to_orthogonal`](Self::from_points_to_orthogonal) but
    /// orthogonalizes to the WORLD axes (base angle 0) rather than the stroke's
    /// own dominant angle. Use when the stroke is drawn inside an existing world
    /// frame — e.g. a new storey traced over the previous storey and snapped to
    /// its axis-aligned outline. Keeping the dominant-angle default would let a
    /// slightly-rotated freehand trace tilt the whole storey off that frame, so
    /// walls meant to coincide with the storey below wouldn't line up.
    pub fn from_points_to_world_orthogonal(
        points: &[Point2f32],
        tolerance: f32,
        closed: bool,
    ) -> Result<Self, Error> {
        Self::from_points_oriented(points, tolerance, closed, Some(0.0))
    }

    /// Like [`from_points_to_orthogonal`](Self::from_points_to_orthogonal) but
    /// orthogonalizes to a FIXED `base_angle` rather than the stroke's own
    /// dominant angle. `None` = use the dominant angle (the storey-outline
    /// default); `Some(0.0)` = snap to the world axes — used for living areas so
    /// a room drawn partly along a diagonal boundary keeps axis-aligned interior
    /// walls instead of tilting the whole shape to that diagonal's angle.
    fn from_points_oriented(
        points: &[Point2f32],
        tolerance: f32,
        closed: bool,
        base_angle: Option<f32>,
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

        let orthog_points = match base_angle {
            Some(a) => orthogonal::make_orthogonal_at(&simplified, tolerance, a)?,
            None => orthogonal::make_orthogonal(&simplified, tolerance)?,
        };

        // For a closed polygon, finalize the closing structure (see
        // `finalize_closed_ring`) before the parametric layer sees it.
        let final_points = if closed {
            finalize_closed_ring(orthog_points, tolerance)
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
            finalize_closed_ring(orthog_points, tolerance)
        } else {
            orthog_points
        };

        self.parametric.add_ring(&final_points);
        self.parametric.apply_orthogonal_constraints()?;
        Ok(())
    }

    /// Build a plan from an already-clean, closed, axis-aligned ring — no
    /// freehand simplification, closing-edge snap, or corner rotation (those
    /// assume a raw stroke with a trailing drag point, which would drop a real
    /// vertex here). Used when the caller has constructed exact ring points,
    /// e.g. carving a sub-area off an existing outline, or rebuilding a
    /// living area from its resolved (already-clean) polygon on load.
    pub fn from_clean_ring(points: &[Point2f32]) -> Result<Self, Error> {
        let mut parametric = parametric::ParametricPlan::from_points(points);
        parametric.apply_orthogonal_constraints()?;
        Ok(Self { parametric })
    }

    /// Parameters of `area` that are PINNED by this outline — fixed by the
    /// floor plan, so the caller marks them KNOWN and dimensioning only asks
    /// about the walls the user actually drew:
    ///  - the perpendicular coordinate of each axis-aligned `area` wall lying
    ///    on a parallel outline wall (a snapped/shared straight wall), and
    ///  - both coordinates of each `area` vertex coinciding with an outline
    ///    CORNER (a house corner) — which in particular fixes a shared
    ///    DIAGONAL wall, whose endpoints are corners, so its length isn't
    ///    prompted.
    ///
    /// (A coordinate that's a constant needs no entry — it's already fixed.)
    pub fn params_pinned_to_outline(
        &self,
        area: &FloorPlan,
    ) -> std::collections::HashSet<ParameterId> {
        let mut pinned = std::collections::HashSet::new();
        let outline = self.parametric.ring_points(0);
        let no = outline.len();
        let apts = area.get_points();
        let n = apts.len();
        if no < 3 || n < 3 {
            return pinned;
        }
        let eps = 1e-3_f32;
        // Outline walls by axis: vertical (x, ylo, yhi); horizontal (y, xlo, xhi).
        let mut vert: Vec<(f32, f32, f32)> = Vec::new();
        let mut horiz: Vec<(f32, f32, f32)> = Vec::new();
        for i in 0..no {
            let a = outline[i];
            let b = outline[(i + 1) % no];
            if (a.x - b.x).abs() < eps {
                vert.push((a.x, a.y.min(b.y), a.y.max(b.y)));
            } else if (a.y - b.y).abs() < eps {
                horiz.push((a.y, a.x.min(b.x), a.x.max(b.x)));
            }
        }
        let overlaps = |alo: f32, ahi: f32, blo: f32, bhi: f32| ahi.min(bhi) - alo.max(blo) > eps;
        for i in 0..n {
            let a = apts[i];
            let b = apts[(i + 1) % n];
            if (a.x - b.x).abs() < eps {
                let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
                if vert
                    .iter()
                    .any(|(x, ylo, yhi)| (x - a.x).abs() < eps && overlaps(lo, hi, *ylo, *yhi))
                {
                    if let Some(pid) = area.vertex_x_param(0, i) {
                        pinned.insert(pid);
                    }
                }
            } else if (a.y - b.y).abs() < eps {
                let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
                if horiz
                    .iter()
                    .any(|(y, xlo, xhi)| (y - a.y).abs() < eps && overlaps(lo, hi, *xlo, *xhi))
                {
                    if let Some(pid) = area.vertex_y_param(0, i) {
                        pinned.insert(pid);
                    }
                }
            }
        }
        // Vertices coinciding with an outline corner are fully fixed by the
        // storey — pin both coordinates. This fixes a shared DIAGONAL wall
        // (endpoints are corners), so its length isn't prompted; only the
        // genuinely-drawn walls (e.g. a cut) remain to dimension.
        for i in 0..n {
            let v = apts[i];
            if outline
                .iter()
                .any(|o| (o.x - v.x).abs() < eps && (o.y - v.y).abs() < eps)
            {
                if let Some(pid) = area.vertex_x_param(0, i) {
                    pinned.insert(pid);
                }
                if let Some(pid) = area.vertex_y_param(0, i) {
                    pinned.insert(pid);
                }
            }
        }
        // A region edge lying ON an outline DIAGONAL edge (a wall the room
        // shares with a diagonal storey wall, e.g. one it was drawn hugging) is
        // fixed by the house — pin both its endpoints' coordinates so the
        // diagonal's length isn't prompted. Its endpoints may sit mid-diagonal
        // (not corners), which the corner pass above wouldn't catch.
        let on_seg = |v: Point2f32, a: Point2f32, b: Point2f32| {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l2 = dx * dx + dy * dy;
            if l2 < 1e-9 {
                return false;
            }
            let t = ((v.x - a.x) * dx + (v.y - a.y) * dy) / l2;
            if !(-0.02..=1.02).contains(&t) {
                return false;
            }
            let (qx, qy) = (a.x + t * dx, a.y + t * dy);
            (v.x - qx).abs() < eps && (v.y - qy).abs() < eps
        };
        for i in 0..n {
            let (a, b) = (apts[i], apts[(i + 1) % n]);
            if (a.x - b.x).abs() < eps || (a.y - b.y).abs() < eps {
                continue; // axis-aligned region edge — handled above
            }
            let on_diagonal = (0..no).any(|j| {
                let (oa, ob) = (outline[j], outline[(j + 1) % no]);
                (oa.x - ob.x).abs() >= eps
                    && (oa.y - ob.y).abs() >= eps
                    && on_seg(a, oa, ob)
                    && on_seg(b, oa, ob)
            });
            if on_diagonal {
                for idx in [i, (i + 1) % n] {
                    if let Some(pid) = area.vertex_x_param(0, idx) {
                        pinned.insert(pid);
                    }
                    if let Some(pid) = area.vertex_y_param(0, idx) {
                        pinned.insert(pid);
                    }
                }
            }
        }
        // Per-VERTEX curve/diagonal pin: a region vertex sitting ON a
        // non-axis-aligned outline edge (a CURVE facet or a diagonal) is fixed by
        // that boundary, so pin both its coords. The edge-based diagonal pass
        // above needs BOTH endpoints of one region edge on ONE outline edge — a
        // curve defeats that, since consecutive region vertices sit on DIFFERENT
        // facets. A slightly generous tolerance absorbs the minor perturbation
        // the sub-area carve introduces along a faceted wall.
        let facet_tol = (geom::bbox_diagonal(&outline) * 0.01).max(eps);
        let on_seg_tol = |v: Point2f32, a: Point2f32, b: Point2f32| {
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let l2 = dx * dx + dy * dy;
            if l2 < 1e-9 {
                return false;
            }
            let t = ((v.x - a.x) * dx + (v.y - a.y) * dy) / l2;
            if !(-0.02..=1.02).contains(&t) {
                return false;
            }
            let (qx, qy) = (a.x + t * dx, a.y + t * dy);
            (v.x - qx).abs() < facet_tol && (v.y - qy).abs() < facet_tol
        };
        for i in 0..n {
            let v = apts[i];
            let on_facet = (0..no).any(|j| {
                let (oa, ob) = (outline[j], outline[(j + 1) % no]);
                (oa.x - ob.x).abs() >= eps && (oa.y - ob.y).abs() >= eps && on_seg_tol(v, oa, ob)
            });
            if on_facet {
                if let Some(pid) = area.vertex_x_param(0, i) {
                    pinned.insert(pid);
                }
                if let Some(pid) = area.vertex_y_param(0, i) {
                    pinned.insert(pid);
                }
            }
        }
        pinned
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

    /// Whether `(x, y)` lies inside the primary ring — see [`carve::contains_point`].
    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        carve::contains_point(&self.parametric.ring_points(0), x, y)
    }

    /// Clamp points that stray outside the outline back onto it — see
    /// [`carve::clamp_inside`].
    pub fn clamp_inside(&self, points: &[Point2f32]) -> Vec<Point2f32> {
        carve::clamp_inside(&self.parametric.ring_points(0), points)
    }

    /// First-stage clip of a freehand sub-area stroke onto the storey — see
    /// [`carve::clip_stroke_inside`].
    pub fn clip_stroke_inside(&self, points: &[Point2f32], close_gap_m: f32) -> Vec<Point2f32> {
        carve::clip_stroke_inside(&self.parametric.ring_points(0), points, close_gap_m)
    }

    /// This plan MINUS the union of `holes`, as a fresh `FloorPlan` — see
    /// [`carve::subtract`].
    pub fn subtract(&self, holes: &[Vec<Point2f32>]) -> Option<FloorPlan> {
        carve::subtract(&self.parametric.ring_points(0), holes)
    }

    /// Whether any vertex or edge-midpoint of `area` lies clearly OUTSIDE this
    /// plan's outline — farther than `tol` beyond the boundary. Points on a
    /// shared wall (distance ~0) don't count. Used as a dimensioning guardrail:
    /// reject a dimension that would push a living area out of the storey.
    pub fn any_point_outside(&self, area: &FloorPlan, tol: f32) -> bool {
        let ring = self.parametric.ring_points(0);
        let no = ring.len();
        if no < 3 {
            return false;
        }
        let dist_outside = |p: Point2f32| -> f32 {
            if self.contains_point(p.x, p.y) {
                return 0.0;
            }
            let mut best = f32::MAX;
            for i in 0..no {
                let (a, b) = (ring[i], ring[(i + 1) % no]);
                let (_q, _t, d) = geom::project_point_to_segment(p, a, b);
                best = best.min(d);
            }
            best
        };
        let pts = area.get_points();
        let n = pts.len();
        if n < 3 {
            return false;
        }
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            let mid = Point2f32::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            if dist_outside(a) > tol || dist_outside(mid) > tol {
                return true;
            }
        }
        false
    }

    /// Longest run of CONSECUTIVE `points` lying farther than `off_tol` from this
    /// plan's outline ring (cyclic — the trace is a closed loop). Used by the
    /// new-storey flow to tell a trace that FOLLOWS this boundary (only isolated
    /// or short strays — hand slop, e.g. tracing a curve loosely → short run)
    /// from one with a SUSTAINED excursion such as an extension (a long run of
    /// points well off the boundary). A run-of-points test is robust where a
    /// max-distance test isn't: one deep stray near a curve doesn't look like a
    /// new wall, but several consecutive points clearly off the boundary do.
    pub fn longest_offring_run(&self, points: &[Point2f32], off_tol: f32) -> usize {
        let ring = self.parametric.ring_points(0);
        let n = ring.len();
        let m = points.len();
        if n < 2 || m == 0 {
            return 0;
        }
        let off: Vec<bool> = points
            .iter()
            .map(|p| {
                let d = (0..n)
                    .map(|i| geom::project_point_to_segment(*p, ring[i], ring[(i + 1) % n]).2)
                    .fold(f32::MAX, f32::min);
                d > off_tol
            })
            .collect();
        if off.iter().all(|&b| b) {
            return m;
        }
        let (mut best, mut cur) = (0usize, 0usize);
        for k in 0..2 * m {
            if off[k % m] {
                cur += 1;
                best = best.max(cur);
            } else {
                cur = 0;
            }
        }
        best.min(m)
    }

    /// Longest contiguous span (by arc length) of this plan's outline that the
    /// trace `points` did NOT follow — every point of it is farther than
    /// `cover_tol` from the trace. Complements [`longest_offring_run`]: that
    /// catches a trace going OUTSIDE the boundary (an extension); this catches
    /// one that SKIPS part of it (stops early on a side, cuts a bulge off with a
    /// straight wall, overhangs a cut-out). A reproduce must do neither. Cyclic;
    /// `0.0` when fully followed.
    ///
    /// Works by SAMPLING the outline along its length and measuring each sample's
    /// distance to the trace — NOT "is any trace point near this edge", which a
    /// single trace point sitting on the edge's far ENDPOINT would satisfy even
    /// when the trace never follows the edge's body (e.g. a straight wall meeting
    /// a bulge only at the shared corners).
    pub fn uncovered_perimeter_run(&self, points: &[Point2f32], cover_tol: f32) -> f32 {
        let ring = self.parametric.ring_points(0);
        let n = ring.len();
        if n < 3 || points.is_empty() {
            return 0.0;
        }
        let dist = |a: Point2f32, b: Point2f32| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
        // Distance from an outline sample to the nearest trace point.
        let to_trace =
            |q: Point2f32| -> f32 { points.iter().map(|p| dist(*p, q)).fold(f32::MAX, f32::min) };
        // Sample each edge at ~cover_tol spacing; record (covered, arc length).
        let step = cover_tol.max(1e-3);
        let mut cov: Vec<(bool, f32)> = Vec::new();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let len = dist(a, b);
            if len < 1e-6 {
                continue;
            }
            let nsamp = (len / step).ceil().max(1.0) as usize;
            let seg = len / nsamp as f32;
            for s in 0..nsamp {
                let f = (s as f32 + 0.5) / nsamp as f32;
                let q = Point2f32::new(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y));
                cov.push((to_trace(q) <= cover_tol, seg));
            }
        }
        if cov.is_empty() || cov.iter().all(|(c, _)| *c) {
            return 0.0;
        }
        let total: f32 = cov.iter().map(|(_, l)| *l).sum();
        if cov.iter().all(|(c, _)| !*c) {
            return total;
        }
        let k = cov.len();
        let (mut best, mut cur) = (0.0f32, 0.0f32);
        for idx in 0..2 * k {
            let (c, l) = cov[idx % k];
            if c {
                cur = 0.0;
            } else {
                cur += l;
                best = best.max(cur);
            }
        }
        best.min(total)
    }

    /// Build a new-storey outline that FOLLOWS this plan's outline where the
    /// trace hugs it — copying the boundary VERBATIM, curve facets and all —
    /// and replaces ONLY the spans where the trace diverges with orthogonalized
    /// new walls. This is the partial-reproduce case: a storey that's mostly the
    /// one below but differs over a short run (a setback, an overhang). Without
    /// it, any divergence forces the WHOLE trace through orthogonalization, which
    /// squares off curves the assessor carefully traced.
    ///
    /// `trace` is the RAW (un-snapped) freehand outline in this plan's metre
    /// space — raw so a diverging wall keeps the drawn position, not a snapped-
    /// onto-the-boundary one. A point within `follow_tol` of the boundary that
    /// advances smoothly along it is "following"; a point off the boundary, or
    /// one whose projected position jumps (a wall cutting across), is "free".
    /// Following runs emit this plan's own vertices; free runs are orthogonalized
    /// with `ortho_tol`. Returns `None` — caller falls back to a whole-trace
    /// orthogonalize — when the trace doesn't split into a clean follow/diverge
    /// alternation (all-following is a pure reproduce; none-following is a wholly
    /// new shape; neither belongs here).
    pub fn outline_following_trace(
        &self,
        trace: &[Point2f32],
        follow_tol: f32,
        ortho_tol: f32,
        tuning: &SpliceTuning,
    ) -> Option<Vec<Point2f32>> {
        let ring = self.parametric.ring_points(0);
        let n = ring.len();
        if n < 3 || trace.len() < 4 {
            return None;
        }
        let dist2 = |a: Point2f32, b: Point2f32| (a.x - b.x).powi(2) + (a.y - b.y).powi(2);
        let elen: Vec<f32> = (0..n)
            .map(|i| dist2(ring[i], ring[(i + 1) % n]).sqrt())
            .collect();
        let mut cum = vec![0.0f32; n];
        for i in 1..n {
            cum[i] = cum[i - 1] + elen[i - 1];
        }
        let perim = cum[n - 1] + elen[n - 1];
        if perim <= 1e-4 {
            return None;
        }
        // Project a point onto the nearest edge → (perimeter position, distance,
        // nearest edge index).
        let project = |p: Point2f32| -> (f32, f32, usize) {
            let mut best = (0usize, 0.0f32, f32::MAX);
            for i in 0..n {
                let (_q, t, d) = geom::project_point_to_segment(p, ring[i], ring[(i + 1) % n]);
                if d < best.2 {
                    best = (i, t.clamp(0.0, 1.0), d);
                }
            }
            (cum[best.0] + best.1 * elen[best.0], best.2, best.0)
        };
        // How close the trace must be to COUNT as following a given edge. We cling
        // TIGHTLY to a STRAIGHT wall (only when nearly touching) so a wall drawn
        // roughly straight but a little off it keeps its own line rather than
        // being yanked onto the boundary — but stay GENEROUS on a CURVE facet (a
        // non-axis edge), so a freehand-traced curve is preserved.
        let tol_at = |e: usize| -> f32 {
            let (a, b) = (ring[e], ring[(e + 1) % n]);
            let straight = (a.x - b.x).abs() < 1e-3 || (a.y - b.y).abs() < 1e-3;
            if straight {
                follow_tol * tuning.straight_cling_factor
            } else {
                follow_tol
            }
        };
        // (perimeter pos, distance to boundary, raw point, nearest edge index)
        let mut sn: Vec<(f32, f32, Point2f32, usize)> = trace
            .iter()
            .map(|&p| {
                let (pos, d, e) = project(p);
                (pos, d, p, e)
            })
            .collect();
        let m = sn.len();
        // Signed minimal circular delta a→b, in [-perim/2, perim/2].
        let fwd = |a: f32, b: f32| -> f32 {
            let mut d = b - a;
            while d > perim * 0.5 {
                d -= perim;
            }
            while d < -perim * 0.5 {
                d += perim;
            }
            d
        };
        // Orient the trace so it advances FORWARD along the boundary.
        let mut net = 0.0;
        for k in 0..m - 1 {
            if sn[k].1 <= tol_at(sn[k].3) && sn[k + 1].1 <= tol_at(sn[k + 1].3) {
                net += fwd(sn[k].0, sn[k + 1].0);
            }
        }
        if net < 0.0 {
            sn.reverse();
        }
        let _ = &fwd; // (direction already applied above)
                      // Classify per-SEGMENT, not per-point: the segment from point k to k+1
                      // "follows" the boundary when both endpoints AND its midpoint hug it. A
                      // long straight chord sampled sparsely stays on its edge (midpoint on the
                      // edge → following); a chord that cuts ACROSS (an overhang over a notch,
                      // a setback) leaves the boundary at the midpoint → free. This is robust
                      // to sampling density, where a per-point "smooth advance" test is not.
        let seg_on = |k: usize| -> bool {
            let a = sn[k].2;
            let b = sn[(k + 1) % m].2;
            if sn[k].1 > tol_at(sn[k].3) || sn[(k + 1) % m].1 > tol_at(sn[(k + 1) % m].3) {
                return false;
            }
            let mid = Point2f32::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            let (_, md, me) = project(mid);
            md <= tol_at(me)
        };
        let mut on: Vec<bool> = (0..m).map(seg_on).collect();
        // Smooth out brief FOLLOWING grazes: a SHORT run of "following" segments
        // flanked by diverging segments is a new wall momentarily running near
        // the boundary (e.g. a cut wall passing within tolerance of an interior
        // edge), NOT a real shared wall. Left split, it breaks one new wall into
        // two — injecting a spurious vertex and extra dimension prompts. Flip
        // such a run to diverging so the new wall stays one piece. (Rotate a free
        // segment to index 0 first so no following run wraps the seam.)
        if let Some(f0) = (0..m).find(|&k| !on[k]) {
            sn.rotate_left(f0);
            on.rotate_left(f0);
            let seg_len = |k: usize| -> f32 {
                let a = sn[k].2;
                let b = sn[(k + 1) % m].2;
                ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
            };
            // A graze runs roughly `feature + 2·follow_tol` long (the wall reads
            // as "following" for ~follow_tol either side of where it's actually
            // close), so the threshold must scale with follow_tol — a fixed
            // fraction of the diagonal is too small. A real shared wall shorter
            // than this, flanked by two new walls, would also be flipped (then
            // dimensioned) — an acceptable trade vs. a split-in-two wall.
            let graze_max = follow_tol * tuning.graze_smoothing_factor;
            let mut i = 0;
            while i < m {
                if on[i] {
                    let s = i;
                    let mut e = i;
                    while e + 1 < m && on[e + 1] {
                        e += 1;
                    }
                    let len: f32 = (s..=e).map(seg_len).sum();
                    if len < graze_max {
                        for k in s..=e {
                            on[k] = false;
                        }
                    }
                    i = e + 1;
                } else {
                    i += 1;
                }
            }
        }
        let n_follow = on.iter().filter(|&&f| f).count();
        println!(
            "[splice] {m} trace segs: {n_follow} follow the boundary, {} diverge \
             (follow_tol {follow_tol:.3} m)",
            m - n_follow
        );
        // Need BOTH a following and a free segment to splice; else this isn't
        // ours (all-on is a pure reproduce; none-on is a wholly new shape).
        if !on.iter().any(|&f| f) || on.iter().all(|&f| f) {
            println!(
                "[splice] declined: {}",
                if n_follow == 0 {
                    "no segment follows the boundary (wholly new shape)"
                } else {
                    "every segment follows the boundary (a reproduce)"
                }
            );
            return None;
        }
        // Rotate so segment 0 starts a following run (its point preceded by a
        // free segment), so runs alternate starting on and ending free.
        let start = match (0..m).find(|&k| on[k] && !on[(k + m - 1) % m]) {
            Some(s) => s,
            None => {
                println!("[splice] declined: no free→follow boundary found");
                return None;
            }
        };
        sn.rotate_left(start);
        on.rotate_left(start);
        // Maximal alternating SEGMENT runs: (is_following, seg_start, seg_end).
        let mut runs: Vec<(bool, usize, usize)> = Vec::new();
        let mut i = 0;
        while i < m {
            let f = on[i];
            let mut j = i;
            while j + 1 < m && on[j + 1] == f {
                j += 1;
            }
            runs.push((f, i, j));
            i = j + 1;
        }
        // Point on the ring at perimeter position `pos`.
        let r_point_at = |pos: f32| -> Point2f32 {
            let pos = pos.rem_euclid(perim);
            for i in 0..n {
                if pos >= cum[i] - 1e-4 && pos <= cum[i] + elen[i] + 1e-4 {
                    let t = if elen[i] > 1e-6 {
                        ((pos - cum[i]) / elen[i]).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let (a, b) = (ring[i], ring[(i + 1) % n]);
                    return Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
                }
            }
            ring[0]
        };
        // Ring vertices strictly inside the forward arc (p_from, p_to), in order
        // — but ONLY the ones the trace actually VISITED. A vertex with no trace
        // point within `follow_tol` is boundary the run's perimeter span wrapped
        // OVER (a graze near an interior feature can make the projected position
        // jump, so the forward span sweeps past walls the trace never traced).
        // Emitting those would pull unvisited boundary (e.g. the storey-below's
        // far side) into the new storey.
        let ft2 = follow_tol * follow_tol;
        let visited = |q: Point2f32| -> bool {
            sn.iter()
                .any(|s| (s.2.x - q.x).powi(2) + (s.2.y - q.y).powi(2) <= ft2)
        };
        let arc_vertices = |p_from: f32, p_to: f32| -> Vec<Point2f32> {
            let span = (p_to - p_from).rem_euclid(perim);
            let mut v: Vec<(f32, Point2f32)> = Vec::new();
            for j in 0..n {
                let d = (cum[j] - p_from).rem_euclid(perim);
                if d > 1e-4 && d < span - 1e-4 && visited(ring[j]) {
                    v.push((d, ring[j]));
                }
            }
            v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
            v.into_iter().map(|(_, p)| p).collect()
        };
        // Splice: a following segment-run [sa..=sb] spans points sa..(sb+1) — emit
        // the ring's own vertices over that arc (curve facets verbatim). A free
        // run's interior points sa+1..=sb are the new wall — orthogonalize them,
        // anchored to the ring points where the trace left and rejoins.
        // Each vertex is tagged `is_curve` = it's a VERBATIM facet vertex of a
        // followed arc (part of a curve). Anchors and new-wall vertices are not.
        let mut out: Vec<(Point2f32, bool)> = Vec::new();
        for &(f, sa, sb) in &runs {
            if f {
                let (p_from, p_to) = (sn[sa].0, sn[(sb + 1) % m].0);
                out.push((r_point_at(p_from), false));
                out.extend(arc_vertices(p_from, p_to).into_iter().map(|p| (p, true)));
                out.push((r_point_at(p_to), false));
            } else {
                let prev_pt = out.last().map(|(p, _)| *p).unwrap_or(r_point_at(sn[sa].0));
                let next_pt = r_point_at(sn[(sb + 1) % m].0);
                let mut seg = Vec::with_capacity(sb - sa + 2);
                seg.push(prev_pt);
                for k in (sa + 1)..=sb {
                    seg.push(sn[k].2);
                }
                seg.push(next_pt);
                // Orthogonalize this new wall in isolation. If it can't be
                // (too few/degenerate points), connect the anchors straight —
                // ONE bad run must not abort the whole splice.
                let ortho = crate::extend::orthogonalize_open_path(&seg, ortho_tol)
                    .unwrap_or_else(|_| vec![prev_pt, next_pt]);
                // Interior vertices only — the endpoints belong to the arcs.
                let upto = ortho.len().saturating_sub(1);
                for p in ortho.iter().take(upto).skip(1) {
                    out.push((*p, false));
                }
            }
        }
        // Square off the NEW walls. A free run emitted between two boundary
        // anchors that DON'T share a coordinate comes out slightly tilted; left
        // tilted, dimensioning a neighbour later flips its lean ("inverted on
        // axis"). Snap a nearly-axis-aligned edge onto its axis — but NEVER a
        // curve facet (both endpoints tagged `is_curve`), and only when the edge
        // is clearly meant to be axis-aligned (high aspect ratio), so a short
        // diagonal facet at a curve's tangent is left alone. The curve-facet tag
        // (not a length gate) is what protects the curve, so this catches a short
        // new wall on a small storey that a `> 0.2·diag` length gate would miss.
        let (mut lo, mut hi) = (out[0].0, out[0].0);
        for (p, _) in &out {
            lo = Point2f32::new(lo.x.min(p.x), lo.y.min(p.y));
            hi = Point2f32::new(hi.x.max(p.x), hi.y.max(p.y));
        }
        let odiag = ((hi.x - lo.x).powi(2) + (hi.y - lo.y).powi(2)).sqrt();
        let axis_tol = (odiag * tuning.axis_tol_ratio).max(tuning.axis_tol_floor);
        let k = out.len();
        for i in 0..k {
            let j = (i + 1) % k;
            if out[i].1 && out[j].1 {
                continue; // a curve facet — keep it verbatim
            }
            let (a, b) = (out[i].0, out[j].0);
            let (dx, dy) = ((a.x - b.x).abs(), (a.y - b.y).abs());
            if dx < axis_tol && dy > axis_tol && dy > dx * tuning.aspect {
                let mx = (a.x + b.x) * 0.5; // near-vertical → share x
                out[i].0.x = mx;
                out[j].0.x = mx;
            } else if dy < axis_tol && dx > axis_tol && dx > dy * tuning.aspect {
                let my = (a.y + b.y) * 0.5; // near-horizontal → share y
                out[i].0.y = my;
                out[j].0.y = my;
            }
        }
        // Drop consecutive coincident vertices (arc endpoints meeting walls), and
        // any closing duplicate.
        let mut cleaned: Vec<(Point2f32, bool)> = Vec::with_capacity(out.len());
        for v in out {
            if cleaned.last().map_or(true, |(q, _)| dist2(*q, v.0) > 1e-6) {
                cleaned.push(v);
            }
        }
        while cleaned.len() >= 2 && dist2(cleaned[0].0, cleaned.last().unwrap().0) <= 1e-6 {
            cleaned.pop();
        }
        if cleaned.len() < 3 {
            println!(
                "[splice] declined: spliced ring collapsed to {} verts",
                cleaned.len()
            );
            return None;
        }
        // Re-root the ring so vertex 0 — which becomes the parametric model's
        // fixed CONSTANT origin — lands on a vertex SHARED with the previous
        // outline (a genuinely fixed corner), never a NEW-WALL junction. A new
        // wall's junction sits mid-shared-wall; if it were the origin its position
        // would be frozen as a constant and couldn't be dimensioned. (Same intent
        // as `build_subarea_region`'s corner-anchor step.)
        if let Some(rk) = cleaned
            .iter()
            .position(|(p, _)| ring.iter().any(|o| dist2(*o, *p) < 1e-4))
        {
            cleaned.rotate_left(rk);
        }
        let cleaned: Vec<Point2f32> = cleaned.into_iter().map(|(p, _)| p).collect();
        println!(
            "[splice] built {} verts from {} runs ({} following, {} new-wall)",
            cleaned.len(),
            runs.len(),
            runs.iter().filter(|r| r.0).count(),
            runs.iter().filter(|r| !r.0).count(),
        );
        Some(cleaned)
    }
}

/// Signed area of a closed polygon (shoelace). Sign reflects winding;
/// `trim_by_line` only uses the magnitude to pick the larger side.
fn polygon_area(points: &[Point2f32]) -> f32 {
    geom::ring_signed_area2(points) * 0.5
}

/// Finalize a CLOSED, orthogonalized ring before the parametric layer sees it:
///   1. `snap_closing_edge` — axis-align the implicit closing edge,
///   2. `rotate_to_corner_start` — make the first vertex a real corner (handles
///      the "user started mid-wall" case, which would otherwise leave a spurious
///      extra vertex),
///   3. `remove_collinear` — drop the previously-anchored start, now a mid-list
///      collinear vertex,
///   4. `snap_closing_edge` again — the rotation in step 2 changed which vertex
///      is first, so re-snap on the final order to guarantee the closing edge is
///      axis-aligned (otherwise the top wall can read as non-orthogonal).
///
/// We deliberately do NOT auto-merge near-collinear walls here — those are left
/// for the `suggest_collinear` review so the model stays honest about what the
/// user drew.
fn finalize_closed_ring(orthog_points: Vec<Point2f32>, tolerance: f32) -> Vec<Point2f32> {
    let snapped = orthogonal::snap_closing_edge(orthog_points, tolerance);
    let rotated = orthogonal::rotate_to_corner_start(snapped, tolerance);
    let cleaned = orthogonal::remove_collinear(&rotated, tolerance);
    orthogonal::snap_closing_edge(cleaned, tolerance)
}

/// Orthogonalize an OPEN path to the world axes with the first and last points
/// FIXED (locked to a boundary). Interior points snap to horizontal/vertical
/// relative to the running corner; the connection to the fixed last point inserts
/// an orthogonal STEP — continuing the incoming wall's axis, then a short
/// perpendicular leg to `last` — rather than pulling the last interior corner onto
/// `last`'s axis. The two-pass carve uses this to orthogonalize a sub-area's
/// interior between two boundary-pinned points: a pinned end can sit on a DIAGONAL
/// boundary, where pulling a vertex off-axis would tilt the whole interior wall.
fn snap_open_fixed_ends_stepped(pts: &[Point2f32]) -> Vec<Point2f32> {
    let n = pts.len();
    if n < 2 {
        return pts.to_vec();
    }
    let mut out = vec![pts[0]];
    let mut cur = pts[0];
    for nx in &pts[1..n - 1] {
        let (dx, dy) = (nx.x - cur.x, nx.y - cur.y);
        let snapped = if dx.abs() > dy.abs() {
            Point2f32::new(nx.x, cur.y) // horizontal step
        } else {
            Point2f32::new(cur.x, nx.y) // vertical step
        };
        out.push(snapped);
        cur = snapped;
    }
    let last = pts[n - 1];
    let (dx, dy) = ((last.x - cur.x).abs(), (last.y - cur.y).abs());
    if dx > f32::EPSILON && dy > f32::EPSILON {
        let incoming_horizontal = if out.len() >= 2 {
            let prev = out[out.len() - 2];
            (prev.y - cur.y).abs() <= (prev.x - cur.x).abs()
        } else {
            dx >= dy
        };
        out.push(if incoming_horizontal {
            Point2f32::new(last.x, cur.y) // horizontal wall, then vertical step
        } else {
            Point2f32::new(cur.x, last.y) // vertical wall, then horizontal step
        });
    }
    out.push(last);
    out
}
