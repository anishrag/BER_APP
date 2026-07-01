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
mod debug_stages;
mod error;
mod geom;
mod orthogonal;
mod parametric;
mod path_edits;
mod poly3d;
mod query;
mod region;
mod simplification;
mod snap;
mod splice;
mod surface_ref;

pub use carve::SubareaOutcome;
pub use debug_stages::{take_splice_debug, SplicePhase};
pub use error::Error;
pub use geom::{
    bbox_diagonal as ring_diagonal, find_flipped_edge, fit_axis_line, fit_axis_rect,
    fit_axis_segment, interior_perpendicular_offset, interior_split_edge, intersect_circles,
    lerp_along, nearest_parallel_edge_distance, point_in_polygon, project_distance_along,
    project_to_circle, ring_contains_point, ring_within_ring, segment_len, snap_on_face,
    solve_axis_wall, subdivide_face, FaceSnap,
};
pub use parametric::{Axis, CollinearitySuggestion, ParameterAxis, ParameterId, VertexId};
pub use poly3d::{Polygon3D, SplitLine3D};
pub use region::stroke_is_closed;
pub use simplification::simplify_to_cut;
pub use snap::{
    clear_snap_policy, set_snap_policy, snap_stroke_to_ring, snap_stroke_to_segments,
    take_snap_decisions, SnapCandidate, SnapChoice, SnapDecision, SnapKind,
};
pub use surface_ref::SurfaceRef;

use domain::points::Point2f32;

/// Tuning for sub-area carves ([`FloorPlan::carve_or_cut`] — living areas and
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
    /// A boundary-follow hug run's transition point (where the stroke peels off
    /// the wall) snaps to an outline CORNER within this many METRES — so a carve
    /// that hugs a wall up to a corner reaches the corner exactly instead of
    /// stopping a hand-width short. Absolute (sub-areas are drawn in metres).
    pub corner_snap_m: f32,
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
            corner_snap_m: 0.15,
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

        // A known loop is `Closed`; an otherwise-open shape is `Auto` (snap shut
        // only if the ends nearly meet — the user may have drawn either).
        let closure = if closed {
            orthogonal::Closure::Closed
        } else {
            orthogonal::Closure::Auto
        };
        let orthog_points = match base_angle {
            Some(a) => orthogonal::make_orthogonal_at(&simplified, tolerance, a, closure)?,
            None => orthogonal::make_orthogonal(&simplified, tolerance, closure)?,
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
        let closure = if closed {
            orthogonal::Closure::Closed
        } else {
            orthogonal::Closure::Auto
        };
        let orthog_points = orthogonal::make_orthogonal(&simplified, tolerance, closure)?;
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
        ref_walls: &[[Point2f32; 2]],
    ) -> std::collections::HashSet<ParameterId> {
        let mut pinned = std::collections::HashSet::new();
        self.scan_outline_contacts(area, ref_walls, &mut |idx, axis| {
            let pid = match axis {
                ParameterAxis::X => area.vertex_x_param(0, idx),
                ParameterAxis::Y => area.vertex_y_param(0, idx),
            };
            if let Some(pid) = pid {
                pinned.insert(pid);
            }
        });
        pinned
    }

    /// Axes along which `area`'s absolute position is NOT fixed by this
    /// boundary plan — the whole area could translate along a listed axis
    /// without violating any snapped wall / shared corner / diagonal-facet
    /// contact, so its drawn position on that axis is approximate. Same
    /// contact tests as [`params_pinned_to_outline`](Self::params_pinned_to_outline),
    /// but per AXIS and including Constant coords (a pin on V0 fixes an axis
    /// without any param existing).
    ///
    /// Axis A is fixed iff any `area` wall perpendicular to A overlaps a
    /// parallel boundary wall at the same coordinate, OR any `area` vertex
    /// coincides with a boundary corner / lies on a boundary diagonal edge /
    /// sits on a curve facet (those fix BOTH axes). Returned in stable order
    /// X then Y; degenerate inputs (<3 vertices on either ring — nothing to
    /// scan) report both axes free.
    pub fn free_position_axes(
        &self,
        area: &FloorPlan,
        ref_walls: &[[Point2f32; 2]],
    ) -> Vec<ParameterAxis> {
        let (mut x_fixed, mut y_fixed) = (false, false);
        self.scan_outline_contacts(area, ref_walls, &mut |_idx, axis| match axis {
            ParameterAxis::X => x_fixed = true,
            ParameterAxis::Y => y_fixed = true,
        });
        let mut free = Vec::new();
        if !x_fixed {
            free.push(ParameterAxis::X);
        }
        if !y_fixed {
            free.push(ParameterAxis::Y);
        }
        free
    }

    /// The contact scan shared by
    /// [`params_pinned_to_outline`](Self::params_pinned_to_outline) and
    /// [`free_position_axes`](Self::free_position_axes): walks `area`'s ring-0
    /// geometry against this plan's ring-0 outline and reports every boundary
    /// contact as a `pin(vertex_idx, axis)` event (vertex indices are into
    /// `area`'s ring 0):
    ///  - an axis-aligned `area` wall overlapping a parallel outline wall at
    ///    the same coordinate pins the wall's PERPENDICULAR axis at the wall's
    ///    start vertex;
    ///  - an `area` vertex coinciding with an outline CORNER pins both axes;
    ///  - an `area` edge lying on an outline DIAGONAL edge pins both axes of
    ///    both endpoints;
    ///  - an `area` vertex sitting on a non-axis-aligned outline facet (curve
    ///    facet or diagonal) pins both axes.
    ///
    /// `ref_walls` are extra reference walls (world-metre segments) the area's
    /// walls were also levelled onto besides the outline — the construction-phase
    /// lines a living area snaps to. Treated exactly like axis-aligned outline
    /// walls / corners here, so a wall snapped onto a phase line is pinned known
    /// too (a snapped wall is a known wall — that's the point of snapping).
    fn scan_outline_contacts(
        &self,
        area: &FloorPlan,
        ref_walls: &[[Point2f32; 2]],
        pin: &mut dyn FnMut(usize, ParameterAxis),
    ) {
        let outline = self.parametric.ring_points(0);
        let no = outline.len();
        let apts = area.get_points();
        let n = apts.len();
        if no < 3 || n < 3 {
            return;
        }
        let eps = 1e-3_f32;
        // Outline walls by axis: vertical (x, ylo, yhi); horizontal (y, xlo, xhi).
        let mut vert: Vec<(f32, f32, f32)> = Vec::new();
        let mut horiz: Vec<(f32, f32, f32)> = Vec::new();
        let mut push_axis_aligned = |a: Point2f32, b: Point2f32| {
            if (a.x - b.x).abs() < eps {
                vert.push((a.x, a.y.min(b.y), a.y.max(b.y)));
            } else if (a.y - b.y).abs() < eps {
                horiz.push((a.y, a.x.min(b.x), a.x.max(b.x)));
            }
        };
        for i in 0..no {
            push_axis_aligned(outline[i], outline[(i + 1) % no]);
        }
        // Reference walls (phase lines) the area also levelled onto — pin like
        // outline walls. Their endpoints join the corner set below.
        for seg in ref_walls {
            push_axis_aligned(seg[0], seg[1]);
        }
        // Corner set: outline vertices plus reference-wall endpoints. An area
        // vertex coinciding with one is fully fixed (both axes).
        let corners: Vec<Point2f32> = outline
            .iter()
            .copied()
            .chain(ref_walls.iter().flat_map(|s| s.iter().copied()))
            .collect();
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
                    pin(i, ParameterAxis::X);
                }
            } else if (a.y - b.y).abs() < eps {
                let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
                if horiz
                    .iter()
                    .any(|(y, xlo, xhi)| (y - a.y).abs() < eps && overlaps(lo, hi, *xlo, *xhi))
                {
                    pin(i, ParameterAxis::Y);
                }
            }
        }
        // Vertices coinciding with an outline (or reference-wall) corner are
        // fully fixed by the storey — pin both coordinates. This fixes a shared
        // DIAGONAL wall (endpoints are corners), so its length isn't prompted;
        // only the genuinely-drawn walls (e.g. a cut) remain to dimension.
        for i in 0..n {
            let v = apts[i];
            if corners
                .iter()
                .any(|o| (o.x - v.x).abs() < eps && (o.y - v.y).abs() < eps)
            {
                pin(i, ParameterAxis::X);
                pin(i, ParameterAxis::Y);
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
                    pin(idx, ParameterAxis::X);
                    pin(idx, ParameterAxis::Y);
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
                pin(i, ParameterAxis::X);
                pin(i, ParameterAxis::Y);
            }
        }
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

    // `contains_point` / `clamp_inside` / `clip_stroke_inside` / `subtract` /
    // `any_point_outside` are region operations — see the `region` module.
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

#[cfg(test)]
mod position_axis_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// 10 x 8 storey outline.
    fn rect_outline() -> FloorPlan {
        FloorPlan::from_clean_ring(&[p(0.0, 0.0), p(10.0, 0.0), p(10.0, 8.0), p(0.0, 8.0)]).unwrap()
    }

    /// A fully interior area touches no boundary — both axes are free, in
    /// the documented stable order X then Y.
    #[test]
    fn interior_area_is_free_on_both_axes() {
        let outline = rect_outline();
        let area =
            FloorPlan::from_clean_ring(&[p(2.0, 2.0), p(5.0, 2.0), p(5.0, 4.0), p(2.0, 4.0)])
                .unwrap();
        assert_eq!(
            outline.free_position_axes(&area, &[]),
            vec![ParameterAxis::X, ParameterAxis::Y]
        );
    }

    /// An area hugging ONE outline wall mid-span (no corner reached) is fixed
    /// on the wall's perpendicular axis only — it could still slide along the
    /// wall. Note the shared coordinate sits on the area's CONSTANT origin
    /// vertex, so this fixedness is invisible to `params_pinned_to_outline`
    /// (no param exists to pin) but must still register here.
    #[test]
    fn mid_span_wall_hug_leaves_only_the_along_wall_axis_free() {
        let outline = rect_outline();
        // Hugs the bottom wall (y = 0): Y fixed, X free.
        let bottom =
            FloorPlan::from_clean_ring(&[p(2.0, 0.0), p(5.0, 0.0), p(5.0, 3.0), p(2.0, 3.0)])
                .unwrap();
        assert_eq!(
            outline.free_position_axes(&bottom, &[]),
            vec![ParameterAxis::X]
        );
        // Hugs the left wall (x = 0): X fixed, Y free.
        let left =
            FloorPlan::from_clean_ring(&[p(0.0, 2.0), p(3.0, 2.0), p(3.0, 5.0), p(0.0, 5.0)])
                .unwrap();
        assert_eq!(
            outline.free_position_axes(&left, &[]),
            vec![ParameterAxis::Y]
        );
    }

    /// An area vertex coinciding with an outline corner fixes both axes.
    #[test]
    fn corner_contact_fixes_both_axes() {
        let outline = rect_outline();
        let area =
            FloorPlan::from_clean_ring(&[p(0.0, 0.0), p(3.0, 0.0), p(3.0, 3.0), p(0.0, 3.0)])
                .unwrap();
        assert!(outline.free_position_axes(&area, &[]).is_empty());
    }

    /// An area vertex sitting on a DIAGONAL outline facet fixes both axes,
    /// even with no axis-aligned wall overlap and no shared corner.
    #[test]
    fn vertex_on_diagonal_facet_fixes_both_axes() {
        // 10x10 square with the top-right corner cut along x + y = 16.
        let outline = FloorPlan::from_clean_ring(&[
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 6.0),
            p(6.0, 10.0),
            p(0.0, 10.0),
        ])
        .unwrap();
        // (8, 8) lies on the diagonal; no other contact anywhere.
        let area =
            FloorPlan::from_clean_ring(&[p(4.0, 4.0), p(8.0, 4.0), p(8.0, 8.0), p(4.0, 8.0)])
                .unwrap();
        assert!(outline.free_position_axes(&area, &[]).is_empty());
    }

    /// A CUT region keeps an outline arc including its corners, so both axes
    /// are fixed — even though its origin coords are Constants with no
    /// parameters to pin.
    #[test]
    fn cut_region_fixes_both_axes() {
        let outline = rect_outline();
        // Straight vertical cut at x = 4, reaching both walls.
        let stroke = vec![
            p(4.0, 0.1),
            p(4.0, 2.0),
            p(4.0, 4.0),
            p(4.0, 6.0),
            p(4.0, 7.9),
        ];
        match outline
            .carve_or_cut(&stroke, &SubareaTuning::default(), false)
            .unwrap()
        {
            SubareaOutcome::Cut(a, b) => {
                assert!(
                    outline.free_position_axes(&a, &[]).is_empty(),
                    "cut region a is position-fixed on both axes"
                );
                assert!(
                    outline.free_position_axes(&b, &[]).is_empty(),
                    "cut region b is position-fixed on both axes"
                );
            }
            other => panic!("expected a Cut, got {other:?}"),
        }
    }

    /// Degenerate input (<3 vertices) — nothing to scan, both axes free.
    #[test]
    fn degenerate_area_reports_both_axes_free() {
        let outline = rect_outline();
        assert_eq!(
            outline.free_position_axes(&FloorPlan::default(), &[]),
            vec![ParameterAxis::X, ParameterAxis::Y]
        );
    }

    /// `translate_all` rigid-moves the whole plan: every point shifts by
    /// exactly (dx, dy) and every wall length is unchanged. The L-shape gives
    /// non-V0 constants (coords collapsed onto the origin's lines) to verify
    /// constants translate along with parameters.
    #[test]
    fn translate_all_shifts_points_uniformly_and_preserves_lengths() {
        let mut plan = FloorPlan::from_clean_ring(&[
            p(0.0, 0.0),
            p(6.0, 0.0),
            p(6.0, 3.0),
            p(3.0, 3.0),
            p(3.0, 5.0),
            p(0.0, 5.0),
        ])
        .unwrap();
        let before = plan.get_points();
        let n = before.len();
        let pre_lengths: Vec<f32> = (0..n).map(|i| plan.wall_length(0, i).unwrap()).collect();
        let (dx, dy) = (2.5, -1.75);
        plan.translate_all(dx, dy);
        let after = plan.get_points();
        assert_eq!(before.len(), after.len());
        for (b, a) in before.iter().zip(&after) {
            assert!(
                (a.x - b.x - dx).abs() < 1e-3 && (a.y - b.y - dy).abs() < 1e-3,
                "every vertex shifts by exactly (dx, dy): {b:?} -> {a:?}"
            );
        }
        for (i, pre) in pre_lengths.iter().enumerate() {
            assert!(
                (plan.wall_length(0, i).unwrap() - pre).abs() < 1e-3,
                "wall {i} length invariant under translation"
            );
        }
    }
}
