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

mod boundary_follow;
mod error;
mod extend;
mod geom;
mod orthogonal;
mod parametric;
mod simplification;

pub use error::Error;
pub use extend::snap_stroke_to_ring;
pub use geom::{
    bbox_diagonal as ring_diagonal, find_flipped_edge, intersect_circles, project_to_circle,
    ring_contains_point, solve_axis_wall,
};
pub use parametric::{Axis, CollinearitySuggestion, ParameterAxis, ParameterId, VertexId};

use domain::points::Point2f32;
use std::collections::HashSet;

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

    /// Carve a sub-area (a living space) out of THIS plan's outline from an
    /// OPEN drawn path. The path supplies the sub-area's new interior walls;
    /// its two endpoints snap onto the outline and the area is closed along the
    /// SHORTER outline arc between them — the walls it SHARES with the storey,
    /// so the user needn't redraw them. The result is an INDEPENDENT orthogonal
    /// `FloorPlan` (placement isn't preserved; every wall, shared or drawn,
    /// dimensions like any other).
    ///
    /// If the endpoints don't both reach the outline (a free-standing interior
    /// room), the path is simply closed into its own loop instead.
    pub fn carve_subarea(
        &self,
        path: &[Point2f32],
        tolerance: f32,
        tuning: &SubareaTuning,
    ) -> Result<Self, Error> {
        let ring = self.parametric.ring_points(0);
        let diag = self.bounding_box_diagonal();

        // Did the user draw a CLOSED loop (all the walls) or an OPEN path (just
        // the new interior walls, ends meant to meet the house)? A closed loop
        // has its first and last points within `CLOSE_GAP_M` of each other. Carve
        // (snap endpoints + fill the shared outline arc) is only for the OPEN
        // case; a closed loop becomes a full ring whose walls are then snapped
        // onto any house wall they run along. Sub-areas are always drawn on a
        // dimensioned (metre) outline, so the gap is an absolute distance.
        let drawn_closed = {
            let ends_gap = path
                .first()
                .zip(path.last())
                .map(|(f, l)| ((f.x - l.x).powi(2) + (f.y - l.y).powi(2)).sqrt())
                .unwrap_or(0.0);
            ends_gap < tuning.close_gap_m
        };

        // Try to carve against the outline (shared walls). Returns the closed
        // ring points, or `None` to fall back to a free-standing room (a closed
        // loop, nothing to share with, or the endpoints don't both reach the
        // outline).
        let carve_points = || -> Result<Option<Vec<Point2f32>>, Error> {
            if drawn_closed || ring.len() < 3 || path.len() < 2 {
                return Ok(None);
            }
            // Simplify, then orthogonalize. A path with interior corners (≥3
            // points) becomes a stepped run of walls; a path that collapses to a
            // single segment is a straight CUT line splitting the outline into
            // two regions — keep it, snapping the cut to its nearest axis so the
            // new wall is clean (the usual vertical/horizontal cut). Open-polyline
            // RDP, so a straight cut collapses to its two endpoints.
            let simplified = simplification::simplify_open(path, tolerance);
            let mut opath = if simplified.len() >= 3 {
                // Orthogonalize to the storey's axes (0°), not the stroke's
                // dominant angle — keeps interior walls axis-aligned even when
                // the path runs along a diagonal boundary.
                orthogonal::make_orthogonal_at(&simplified, tolerance, 0.0)?
            } else if simplified.len() == 2 {
                let (a, b) = (simplified[0], simplified[1]);
                if (a.x - b.x).abs() <= (a.y - b.y).abs() {
                    let x = (a.x + b.x) * 0.5; // near-vertical → vertical cut
                    vec![Point2f32::new(x, a.y), Point2f32::new(x, b.y)]
                } else {
                    let y = (a.y + b.y) * 0.5; // near-horizontal → horizontal cut
                    vec![Point2f32::new(a.x, y), Point2f32::new(b.x, y)]
                }
            } else {
                return Ok(None);
            };
            if opath.len() < 2 {
                return Ok(None);
            }
            let vertex_tol = (diag * 0.04).max(1e-4);
            // "Reaches the outline" threshold: an endpoint within this of a wall
            // shares it; otherwise the area free-floats.
            let reach_tol = (diag * 0.06).max(1e-3);
            let start = opath[0];
            let end = opath[opath.len() - 1];
            let (e1, t1, d1, _) = extend::snap_to_polygon(&ring, start, vertex_tol);
            let (e2, t2, d2, _) = extend::snap_to_polygon(&ring, end, vertex_tol);
            let same_point = e1 == e2 && (t1 - t2).abs() < 1e-3;
            if d1 > reach_tol || d2 > reach_tol || same_point {
                return Ok(None);
            }
            let last = opath.len() - 1;
            extend::pin_to_edge(&mut opath[0], &ring, e1);
            extend::pin_to_edge(&mut opath[last], &ring, e2);

            // Keep the SHORTER outline arc (the corner/alcove the path cuts
            // off): replace the LONGER arc with the path.
            let n = ring.len();
            let edge_lens: Vec<f32> = (0..n)
                .map(|i| {
                    let a = ring[i];
                    let b = ring[(i + 1) % n];
                    ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
                })
                .collect();
            let total: f32 = edge_lens.iter().sum();
            let pos_of = |e: usize, t: f32| edge_lens[..e].iter().sum::<f32>() + t * edge_lens[e];
            let (pos1, pos2) = (pos_of(e1, t1), pos_of(e2, t2));
            let arc_1_to_2 = if pos2 >= pos1 {
                pos2 - pos1
            } else {
                pos2 - pos1 + total
            };
            let replace_arc_1_to_2 = arc_1_to_2 > total - arc_1_to_2;

            let seq = extend::splice_sequence(&ring, e1, t1, e2, t2, &opath, replace_arc_1_to_2);
            let carved: Vec<Point2f32> = seq
                .iter()
                .map(|el| match el {
                    extend::SpliceElem::Keep(i) => ring[*i],
                    extend::SpliceElem::New(p) => *p,
                })
                .collect();
            if carved.len() < 3 {
                return Ok(None);
            }
            Ok(Some(carved))
        };

        let pts = match carve_points()? {
            Some(c) => c,
            None => {
                // CLOSED loop: two-pass boundary-aware carve. Classify the loop
                // into runs that HUG the outline and runs that CUT through the
                // interior, then rebuild so the hugging runs follow the REAL wall
                // — corners, DIAGONALS and curve facets kept, walked in order —
                // and only the interior is orthogonalized (cut-style steps). This
                // follows the boundary instead of free-floating into a box, and
                // handles slanted/curved walls, so there's no separate diagonal
                // path.
                let on_tol = (diag * tuning.on_ratio).max(1e-3).min(tuning.on_tol_ceil_m);
                let off_tol = (diag * tuning.off_ratio)
                    .max(1e-3)
                    .min(tuning.off_tol_ceil_m);
                // Densify first: the classifier samples POINTS, so a sparse
                // clipped/simplified stroke could leap across the interior between
                // two boundary edges undetected (hugging non-adjacent edges, which
                // outline_subpath then bridges with a spurious diagonal). Sample at
                // `on_tol` so the interior stretch is always seen; the extra points
                // are dropped again by the interior RDP simplify.
                let dpath = boundary_follow::densify(path, on_tol);
                let path = &dpath[..];
                // Rotate the loop to start at a boundary↔interior transition, so a
                // run that wraps past the stroke's start/end stays one run.
                let classes0 = boundary_follow::classify_points(&ring, path, on_tol, off_tol);
                let rot = boundary_follow::rotate_to_transition(path, &classes0);
                let classes = boundary_follow::classify_points(&ring, &rot, on_tol, off_tol);
                let bruns = boundary_follow::runs(&classes);
                let hugs_wall = bruns
                    .iter()
                    .any(|r| matches!(r, boundary_follow::Run::OnBoundary { .. }));
                let ortho = if hugs_wall {
                    // Follows the outline wherever it was hugged — including the
                    // whole-boundary case (draw around the whole storey), which
                    // reproduces the full outline (notches/diagonals included).
                    boundary_follow::build_region_ring(&ring, &rot, &bruns, tolerance, tuning)
                } else {
                    // Never touches a wall → genuine free-standing interior room:
                    // close the drawn loop into its own orthogonal polygon.
                    Self::from_points_oriented(path, tolerance, true, Some(0.0))?.get_points()
                };
                // `build_region_ring` already reproduced any diagonal/curve
                // verbatim (it walks the real outline vertices), so this must NOT
                // re-snap to diagonals — that would drag interior walls onto them.
                return self.build_subarea_region(&ortho, true, false);
            }
        };
        self.build_subarea_region(&pts, true, true)
    }

    /// Finalize a candidate sub-area ring: optionally snap walls onto the
    /// outline, strip collapse remnants, anchor at a shared corner, and
    /// parametrize. Shared tail of carve / cut region construction.
    ///
    /// `snap_to_outline` pulls drawn walls onto nearby outline walls — wanted
    /// for a freehand room, but NOT for a cut, whose endpoints are already the
    /// exact line/boundary crossings (snapping would drag them to a nearby
    /// corner/diagonal).
    ///
    /// Strips the coincident / collinear remnants left when the orthogonalizer
    /// collapses a sub-threshold feature (e.g. a too-shallow nook) onto a wall:
    /// those zero-length walls make `validate_simple` reject the shape and block
    /// dimensioning. The tolerance is keyed off the SUB-AREA's own size — NOT
    /// the outline's — so a small room inside a big house is cleaned at its own
    /// scale, matching how the storey outline is cleaned at the storey's scale.
    /// Kept tiny (0.5% of the area's diagonal) so it only removes genuine
    /// collapse remnants and never a real (≥~10%) nook.
    fn build_subarea_region(
        &self,
        pts: &[Point2f32],
        snap_axis: bool,
        snap_diagonal: bool,
    ) -> Result<Self, Error> {
        let area_diag = geom::bbox_diagonal(pts);

        // Correct any drawn wall that runs close to (and overlaps) a parallel
        // house wall onto it — so a sub-area drawn as a FULL loop, with some of
        // its walls meant to sit on the storey, lands exactly on those walls.
        // `snap_diagonal` projects vertices onto diagonal house walls too; it's
        // off when a diagonal is already placed verbatim (boundary-aware loop /
        // cut) so the axis snap can still pull other walls onto the storey
        // without disturbing that diagonal.
        let snapped = if snap_axis {
            let snap_tol = (area_diag * 0.06).max(1e-3);
            self.snap_walls_to_outline(pts, snap_tol, snap_diagonal)
        } else {
            pts.to_vec()
        };

        let clean_tol = (area_diag * 0.005).max(1e-4);
        // De-dup uses the loose tolerance (merge coincident jitter), but collinear
        // removal uses `EXACT_COLLINEAR_TOL` because this ring contains VERBATIM
        // outline/curve geometry — see that constant's docs for why a loose
        // tolerance would drop a curve-start corner.
        let collinear_tol = orthogonal::EXACT_COLLINEAR_TOL;
        let deduped = orthogonal::remove_duplicates(&snapped, clean_tol);
        let cleaned = orthogonal::remove_collinear(&deduped, collinear_tol);
        // Un-intersect: if the assembled ring folds over itself (e.g. the
        // boundary-aware loop placed a diagonal endpoint past an adjacent wall),
        // split it at the crossing and keep the larger loop, rather than
        // emitting a self-crossing shape. Re-clean to drop the inserted
        // crossing vertex if it lands collinear.
        let untangled = geom::untangle_ring(&cleaned);
        let cleaned = orthogonal::remove_collinear(
            &orthogonal::remove_duplicates(&untangled, clean_tol),
            collinear_tol,
        );
        // Square off any spurious diagonal — a tilted edge NOT on a verbatim house
        // wall (a boundary hop, or an interior wall meeting the boundary
        // off-level). Runs AFTER the dedup so the inserted corner survives; a
        // final collinear pass (exact tol, no dedup) drops only truly-redundant
        // vertices, and untangle repairs any crossing the corner introduces.
        let squared =
            geom::orthogonalize_off_boundary(&cleaned, &self.parametric.ring_points(0), 0.003);
        let cleaned = orthogonal::remove_collinear(&geom::untangle_ring(&squared), collinear_tol);
        if cleaned.len() < 3 {
            return Err(Error::ExtendError("living area collapsed to a line".into()));
        }

        // Anchor the ring at a vertex coinciding with an outline CORNER (a
        // house corner shared with the storey), so the parametric origin
        // constant lands there — making the two shared walls meeting at it
        // fixed/const and leaving the DRAWN walls as free parameters the user
        // can dimension. Without this the origin can land on a drawn wall (e.g.
        // a cut), baking its position as a constant the user can't set. A cut
        // endpoint sits mid-wall (not a corner), so it's NOT chosen. A
        // free-standing room (no shared corner) keeps its order.
        let outline = self.parametric.ring_points(0);
        let eps = (area_diag * 0.002).max(1e-4);
        let cleaned = match cleaned.iter().position(|&p| {
            outline
                .iter()
                .any(|o| (o.x - p.x).abs() < eps && (o.y - p.y).abs() < eps)
        }) {
            Some(k) if k > 0 => {
                let mut r = cleaned[k..].to_vec();
                r.extend_from_slice(&cleaned[..k]);
                r
            }
            _ => cleaned,
        };
        Self::from_clean_ring(&cleaned)
    }

    /// Split the outline with a CUT stroke into its TWO regions, returned as
    /// independent `FloorPlan`s (so the caller can let the user pick which side
    /// becomes the living area). The cut may be a straight line OR a STEPPED
    /// (orthogonal, multi-leg) path — it's orthogonalized to the storey axes and
    /// followed verbatim, so an L-shaped cut keeps its bend instead of being
    /// flattened to a single axis. Returns `None` when the stroke is a loop, or
    /// its two ends don't both reach the outline at distinct points (the caller
    /// then falls back to [`carve_subarea`](Self::carve_subarea)).
    pub fn split_by_cut(
        &self,
        path: &[Point2f32],
        tolerance: f32,
        tuning: &SubareaTuning,
    ) -> Result<Option<(Self, Self)>, Error> {
        let ring = self.parametric.ring_points(0);
        if ring.len() < 3 || path.len() < 2 {
            return Ok(None);
        }
        let diag = self.bounding_box_diagonal();
        // A cut is an OPEN stroke (ends far apart) that divides the storey edge to
        // edge — straight, stepped, or a balanced L (any number of orthogonal
        // legs). A CLOSED loop (ends nearly meeting) is a drawn room → carve.
        // Whether the open stroke actually reaches two DISTINCT outline edges is
        // decided by the snap/reach check below; a free-standing interior path
        // that doesn't reach the outline returns `None` and the caller carves.
        let Some(((bnx, bny), (bxx, bxy))) = geom::bbox(path) else {
            return Ok(None);
        };
        let span = (bxx - bnx).max(bxy - bny);
        if span < 1e-6 {
            return Ok(None);
        }
        let (a, b) = (path[0], path[path.len() - 1]);
        let ends_gap = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
        if ends_gap <= span * 0.5 {
            return Ok(None); // a loop / 2D blob → carve, not cut
        }

        // Orthogonalize the cut to the storey axes. A path with a corner (≥3
        // points) becomes a clean stepped polyline (the bend is KEPT); a straight
        // cut collapses to two points snapped to the nearer axis. Simplify with a
        // COARSE tolerance (relative to the cut's span) so hand wobble is dropped
        // as noise — a real step is large relative to the span and survives,
        // whereas a fixed fine tolerance would orthogonalize wobble into a
        // spurious staircase.
        // Coarse (drops wobble) BUT ceilinged, so a corner ≥ the ceiling is always
        // kept even on a long cut (where the ratio alone would inflate the
        // tolerance past a real corner). Shared with the carve interior join.
        let cut_tol = tuning.coarse_simplify_tol(span, tolerance);
        // Open-polyline RDP (allows a 2-point collapse to the endpoints) — same
        // simplify the carve interior uses. A straight cut collapses to its two
        // ends; a stepped cut keeps the step.
        let simplified = simplification::simplify_open(path, cut_tol);
        // A straight cut between two points, snapped to the nearer axis.
        let straight = |s: Point2f32, e: Point2f32| {
            if (s.x - e.x).abs() <= (s.y - e.y).abs() {
                let x = (s.x + e.x) * 0.5; // near-vertical → vertical cut
                vec![Point2f32::new(x, s.y), Point2f32::new(x, e.y)]
            } else {
                let y = (s.y + e.y) * 0.5; // near-horizontal → horizontal cut
                vec![Point2f32::new(s.x, y), Point2f32::new(e.x, y)]
            }
        };
        let (s0, e0) = (simplified[0], simplified[simplified.len() - 1]);
        let mut opath = if simplified.len() >= 3 {
            // Orthogonalization can COLLAPSE a shallow/drifting step: RDP may keep
            // only one corner of the jog, leaving a near-straight "<" that squares
            // to a single line (<3 points). Fall back to a straight cut rather than
            // failing the whole split (which would dump the user into the carve).
            match orthogonal::make_orthogonal_at(&simplified, cut_tol, 0.0) {
                Ok(o) if o.len() >= 2 => o,
                _ => straight(s0, e0),
            }
        } else {
            straight(s0, e0)
        };
        if opath.len() < 2 {
            return Ok(None);
        }

        // Both endpoints must reach the outline at two DISTINCT points; snap them
        // exactly onto it so the cut divides the boundary cleanly.
        let vertex_tol = (diag * tuning.cut_vertex_tol_ratio).max(1e-4);
        let reach_tol = (diag * tuning.cut_reach_tol_ratio).max(1e-3);
        let last = opath.len() - 1;
        let (e1, t1, d1, v1) = extend::snap_to_polygon(&ring, opath[0], vertex_tol);
        let (e2, t2, d2, v2) = extend::snap_to_polygon(&ring, opath[last], vertex_tol);
        if d1 > reach_tol || d2 > reach_tol || (e1 == e2 && (t1 - t2).abs() < 1e-3) {
            return Ok(None);
        }
        let (pre0, pre_last) = (opath[0], opath[last]);
        // A vertex snap (t=0 at corner `e`) must land EXACTLY on the corner —
        // projecting onto the adjacent edge instead leaves a tiny offset segment
        // that collapses into a tilted boundary edge.
        if v1 {
            opath[0] = ring[e1];
        } else {
            extend::pin_to_edge(&mut opath[0], &ring, e1);
        }
        if v2 {
            opath[last] = ring[e2];
        } else {
            extend::pin_to_edge(&mut opath[last], &ring, e2);
        }

        // Keep the cut ORTHOGONAL after the snap, in two passes over its two ends.
        // A snap can tilt a terminal leg only when the endpoint moved OFF the leg's
        // rail (its shared axis coordinate); the fix depends on WHY it moved:
        //
        //  1. ALIGN — the end landed on an INTERIOR-FACING vertex (no outline edge
        //     parallel to the leg there). Shift the inward neighbour onto that
        //     vertex's rail, so the leg is axis-aligned THROUGH the vertex. The
        //     cut's position is then fixed by the vertex (both ends share one
        //     parameter), leaving nothing to dimension on that wall.
        //  2. STEP — the end landed on an edge/corner whose wall runs PARALLEL to
        //     the leg, where aligning would lay the cut on top of that wall. Keep
        //     the leg's rail and add a short perpendicular jog out to the endpoint.
        let eps = 1e-3;
        let nverts = ring.len();
        let tilted =
            |a: Point2f32, b: Point2f32| (a.x - b.x).abs() > eps && (a.y - b.y).abs() > eps;
        // A leg is vertical when its PRE-snap endpoint shared the neighbour's x.
        let leg_vertical = |pre: Point2f32, c: Point2f32| (pre.x - c.x).abs() < (pre.y - c.y).abs();
        let faces_interior = |vi: usize, vertical: bool| {
            let (v, p, q) = (
                ring[vi],
                ring[(vi + nverts - 1) % nverts],
                ring[(vi + 1) % nverts],
            );
            if vertical {
                (p.x - v.x).abs() > eps && (q.x - v.x).abs() > eps
            } else {
                (p.y - v.y).abs() > eps && (q.y - v.y).abs() > eps
            }
        };
        // PASS 1: align onto interior-facing vertices (no insertions → stable
        // indices). For a straight cut whose other end is the anchored vertex, this
        // shifts THIS end onto it; for a multi-leg cut it shifts the free corner.
        for (e_idx, c_idx, is_v, vi, pre) in [
            (0usize, 1usize.min(last), v1, e1, pre0),
            (last, last.saturating_sub(1), v2, e2, pre_last),
        ] {
            if is_v && tilted(opath[e_idx], opath[c_idx]) {
                let vertical = leg_vertical(pre, opath[c_idx]);
                if faces_interior(vi, vertical) {
                    if vertical {
                        opath[c_idx].x = opath[e_idx].x;
                    } else {
                        opath[c_idx].y = opath[e_idx].y;
                    }
                }
            }
        }
        // PASS 2: step whatever is still tilted. END first so START indices hold.
        let step = |opath: &mut Vec<Point2f32>, e_idx: usize, c_idx: usize, pre: Point2f32| {
            if tilted(opath[e_idx], opath[c_idx]) {
                let (e, c) = (opath[e_idx], opath[c_idx]);
                let corner = if leg_vertical(pre, c) {
                    Point2f32::new(c.x, e.y) // vertical leg toward c, jog in x to e
                } else {
                    Point2f32::new(e.x, c.y)
                };
                let ins = if e_idx > c_idx { e_idx } else { c_idx };
                opath.insert(ins, corner);
            }
        };
        let m = opath.len();
        step(&mut opath, m - 1, m - 2, pre_last);
        step(&mut opath, 0, 1, pre0);

        // The two regions = the cut polyline + each of the outline's two arcs.
        let region = |replace_arc_1_to_2: bool| -> Result<Self, Error> {
            let seq = extend::splice_sequence(&ring, e1, t1, e2, t2, &opath, replace_arc_1_to_2);
            let pts: Vec<Point2f32> = seq
                .iter()
                .map(|el| match el {
                    extend::SpliceElem::Keep(i) => ring[*i],
                    extend::SpliceElem::New(p) => *p,
                })
                .collect();
            if pts.len() < 3 {
                return Err(Error::ExtendError("cut region collapsed".into()));
            }
            // Axis wall-snap (the SAME `snap_walls_to_outline` the carve uses): a
            // cut leg drawn near a PARALLEL boundary edge — the outline OR a
            // neighbour phase's shared edge in the available region — lands exactly
            // on it, so cut-based phases tile like carved ones. The kept arc is
            // already on the outline so its snap is a no-op, and `snap_diagonal =
            // false` leaves verbatim curves/diagonals untouched.
            self.build_subarea_region(&pts, true, false)
        };
        Ok(Some((region(true)?, region(false)?)))
    }

    /// Correct each axis-aligned edge of `pts` (a candidate sub-area ring) that
    /// runs close to — and overlaps — a PARALLEL wall of this plan's outline,
    /// snapping that edge's perpendicular coordinate exactly onto the outline
    /// wall. Lets the user draw a living area as a full closed loop and have the
    /// walls they meant to share with the storey land on it. Coordinate snaps
    /// are applied to every vertex sharing the coord, so the ring stays a clean
    /// orthogonal polygon. `tol` is the max perpendicular gap that still snaps.
    ///
    /// A second pass handles NON-axis-aligned (diagonal) outline walls — e.g. a
    /// trim cut: any sub-area vertex lying within `tol` of a diagonal outline
    /// edge is projected onto that edge's line, so a wall drawn near the
    /// diagonal follows it (relaxing orthogonality to match the storey). The
    /// pass is a no-op when the outline is fully rectilinear.
    fn snap_walls_to_outline(
        &self,
        pts: &[Point2f32],
        tol: f32,
        do_diagonal: bool,
    ) -> Vec<Point2f32> {
        let ring = self.parametric.ring_points(0);
        let no = ring.len();
        if no < 3 || pts.len() < 3 {
            return pts.to_vec();
        }
        let eps = 1e-3_f32;
        // Outline walls split by axis: vertical = (x, ylo, yhi); horizontal =
        // (y, xlo, xhi).
        let mut vert: Vec<(f32, f32, f32)> = Vec::new();
        let mut horiz: Vec<(f32, f32, f32)> = Vec::new();
        for i in 0..no {
            let a = ring[i];
            let b = ring[(i + 1) % no];
            if (a.x - b.x).abs() < eps {
                vert.push((a.x, a.y.min(b.y), a.y.max(b.y)));
            } else if (a.y - b.y).abs() < eps {
                horiz.push((a.y, a.x.min(b.x), a.x.max(b.x)));
            }
        }
        // For each sub-area edge, find the nearest parallel, overlapping outline
        // wall within `tol`; record a coordinate remap.
        let n = pts.len();
        let mut remap_x: Vec<(f32, f32)> = Vec::new();
        let mut remap_y: Vec<(f32, f32)> = Vec::new();
        let overlaps = |alo: f32, ahi: f32, blo: f32, bhi: f32| ahi.min(bhi) - alo.max(blo) > eps;
        for i in 0..n {
            let a = pts[i];
            let b = pts[(i + 1) % n];
            if (a.x - b.x).abs() < eps {
                let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
                if let Some((ox, _, _)) = vert
                    .iter()
                    .filter(|(ox, ylo, yhi)| {
                        (ox - a.x).abs() <= tol && overlaps(lo, hi, *ylo, *yhi)
                    })
                    .min_by(|u, v| (u.0 - a.x).abs().total_cmp(&(v.0 - a.x).abs()))
                {
                    if (ox - a.x).abs() > eps {
                        remap_x.push((a.x, *ox));
                    }
                }
            } else if (a.y - b.y).abs() < eps {
                let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
                if let Some((oy, _, _)) = horiz
                    .iter()
                    .filter(|(oy, xlo, xhi)| {
                        (oy - a.y).abs() <= tol && overlaps(lo, hi, *xlo, *xhi)
                    })
                    .min_by(|u, v| (u.0 - a.y).abs().total_cmp(&(v.0 - a.y).abs()))
                {
                    if (oy - a.y).abs() > eps {
                        remap_y.push((a.y, *oy));
                    }
                }
            }
        }
        let mut out = pts.to_vec();
        for p in &mut out {
            if let Some((_, to)) = remap_x.iter().find(|(from, _)| (p.x - from).abs() < eps) {
                p.x = *to;
            }
            if let Some((_, to)) = remap_y.iter().find(|(from, _)| (p.y - from).abs() < eps) {
                p.y = *to;
            }
        }

        // Diagonal pass: project sub-area vertices lying close to a NON-axis-
        // aligned outline edge onto that edge's line, so a wall drawn near a
        // diagonal house wall (e.g. a trim cut) follows it. Forcing the interior
        // to the storey axes turns a wall drawn ALONG the diagonal into a
        // stair-step whose corners sit a little off the line, so this pass uses
        // a more generous reach than the axis snap to pull them on.
        let diag_tol = tol * 2.5;
        let diagonals: Vec<(Point2f32, Point2f32)> = if do_diagonal {
            (0..no)
                .map(|i| (ring[i], ring[(i + 1) % no]))
                .filter(|(a, b)| (a.x - b.x).abs() >= eps && (a.y - b.y).abs() >= eps)
                .collect()
        } else {
            Vec::new()
        };
        if !diagonals.is_empty() {
            for p in &mut out {
                let mut best: Option<(f32, Point2f32)> = None;
                for (a, b) in &diagonals {
                    let (proj, t, d) = geom::project_point_to_segment(*p, *a, *b);
                    if !(-0.1..=1.1).contains(&t) {
                        continue; // not alongside this segment
                    }
                    if d <= diag_tol && best.map_or(true, |(bd, _)| d < bd) {
                        best = Some((d, proj));
                    }
                }
                if let Some((_, proj)) = best {
                    *p = proj;
                }
            }
        }
        out
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

    /// Extend the primary ring by splicing a drawn path onto a wall, *adding*
    /// the enclosed area (the SHORTER arc between the attach points is replaced
    /// by the path). `extend_ortho` orthogonalizes the path (a stepped porch);
    /// see [`extend_curve`](Self::extend_curve) for a faceted arc. Untouched
    /// vertices keep their identity/parameters; returns the newly-minted
    /// parameter IDs. See [`splice_path`](Self::splice_path) for the mechanics.
    pub fn extend_ortho(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        self.splice_path(stroke, true, false)
    }

    /// Extend a wall with a faceted freeform CURVE (adds area): the drawn arc is
    /// RDP-simplified into a short polyline and spliced onto the shorter arc.
    /// The facet walls aren't individually dimensioned and scale with the plan.
    pub fn extend_curve(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        self.splice_path(stroke, false, false)
    }

    /// Trim the primary ring with a freehand cut `stroke`: the stroke is fitted
    /// to a straight line, the smaller-area side is discarded, and the two
    /// boundary crossings are joined by a single straight (generally diagonal,
    /// i.e. NON-orthogonal) cut edge. This is how angled walls are introduced
    /// after the orthogonal base is drawn.
    ///
    /// Robustness: the cut line is the total-least-squares fit of the whole
    /// stroke (so a wavy hand-drawn line still gives a clean straight cut), and
    /// when the fitted line crosses a concave outline more than twice we keep
    /// the two crossings nearest where the stroke was actually drawn. A
    /// crossing that lands within a small tolerance of a corner collapses that
    /// would-be zero-length remnant, so a cut "through" a corner is accepted.
    ///
    /// Kept vertices keep their identity and parameters (so dimensions on the
    /// surviving walls are preserved). Each crossing vertex sits on its host
    /// edge — sharing that edge's perpendicular coordinate — but is free along
    /// it; the cut edge shares neither coordinate, hence diagonal. The result
    /// is a `ParametricPlan` that serializes like any other, so a trimmed
    /// storey saves and reloads through the normal snapshot path. Returns the
    /// newly-minted parameter IDs; errors (leaving the plan unchanged) if the
    /// line doesn't cross the outline or the result isn't a simple polygon.
    pub fn trim_by_stroke(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        use crate::extend::SpliceElem;
        if stroke.len() < 2 {
            return Err(Error::TrimError(
                "trim stroke needs at least 2 points".into(),
            ));
        }
        let ring = self.parametric.ring_points(0);
        let n = ring.len();
        if n < 3 {
            return Err(Error::TrimError("no polygon to trim".into()));
        }

        // Fit a straight line to the (possibly wavy) stroke.
        let (c, dir) = geom::fit_line(stroke);
        if dir.0.abs() < 1e-9 && dir.1.abs() < 1e-9 {
            return Err(Error::TrimError("trim stroke is degenerate".into()));
        }
        let side = |p: Point2f32| -> f32 { dir.0 * (p.y - c.y) - dir.1 * (p.x - c.x) };
        let along = |p: Point2f32| -> f32 { (p.x - c.x) * dir.0 + (p.y - c.y) * dir.1 };

        let diag = self.bounding_box_diagonal();
        // Within this of a corner, the cut is treated as passing through it
        // (the would-be zero-length remnant wall collapses).
        let corner_tol = (diag * 0.02).max(1e-4);

        // All crossings of the infinite fitted line with the outline.
        let eps = 1e-6_f32;
        let mut crossings: Vec<(usize, Point2f32, f32)> = Vec::new(); // (edge, pt, along)
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            let sa = side(a);
            let sb = side(b);
            if (sa > eps && sb < -eps) || (sa < -eps && sb > eps) {
                let t = sa / (sa - sb);
                let pt = Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
                crossings.push((i, pt, along(pt)));
            }
        }
        // Crossings that pass exactly through a vertex (the boundary changes
        // side there — a transversal, not a tangent touch). The interior-edge
        // loop above skips these (it needs strictly-opposite signs). Recording
        // on the edge ENDING at the vertex lets the at-corner omission below
        // reuse the existing corner, so a cut "through" a corner is accepted
        // and leaves no zero-length remnant.
        for v in 0..n {
            if side(ring[v]).abs() < eps {
                let sp = side(ring[(v + n - 1) % n]);
                let sn = side(ring[(v + 1) % n]);
                if (sp > eps && sn < -eps) || (sp < -eps && sn > eps) {
                    crossings.push(((v + n - 1) % n, ring[v], along(ring[v])));
                }
            }
        }
        if crossings.len() < 2 {
            return Err(Error::TrimError(format!(
                "cut line must cross the outline (crossed {})",
                crossings.len()
            )));
        }

        // Keep the two crossings matching the stroke's drawn span — nearest the
        // start projection and nearest the end projection. This ignores the
        // extra crossings a concave outline produces far from the actual cut.
        let a_start = along(*stroke.first().unwrap());
        let a_end = along(*stroke.last().unwrap());
        let nearest = |target: f32, skip: Option<usize>| -> usize {
            let mut best = usize::MAX;
            let mut best_d = f32::INFINITY;
            for (i, cr) in crossings.iter().enumerate() {
                if Some(i) == skip {
                    continue;
                }
                let d = (cr.2 - target).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            best
        };
        let i0 = nearest(a_start, None);
        let i1 = nearest(a_end, Some(i0));
        let (e1, pa, _) = crossings[i0];
        let (e2, pb, _) = crossings[i1];
        if e1 == e2 {
            return Err(Error::TrimError(
                "cut enters and leaves through the same wall".into(),
            ));
        }

        // Two candidate kept polygons (one per arc), closed by the straight cut
        // edge; keep the larger. A boundary within `corner_tol` of its first
        // kept vertex is omitted so an at-corner cut collapses cleanly.
        let arc = |from_edge: usize, to_edge: usize, first: Point2f32, last: Point2f32| {
            let mut seq: Vec<SpliceElem> = Vec::new();
            let fk = ring[(from_edge + 1) % n];
            if ((first.x - fk.x).powi(2) + (first.y - fk.y).powi(2)).sqrt() > corner_tol {
                seq.push(SpliceElem::New(first));
            }
            let mut i = (from_edge + 1) % n;
            loop {
                seq.push(SpliceElem::Keep(i));
                if i == to_edge {
                    break;
                }
                i = (i + 1) % n;
            }
            let lk = ring[to_edge];
            if ((last.x - lk.x).powi(2) + (last.y - lk.y).powi(2)).sqrt() > corner_tol {
                seq.push(SpliceElem::New(last));
            }
            seq
        };
        let seq_a = arc(e1, e2, pa, pb);
        let seq_b = arc(e2, e1, pb, pa);

        let area_of = |seq: &[SpliceElem]| -> f32 {
            let pts: Vec<Point2f32> = seq
                .iter()
                .map(|e| match e {
                    SpliceElem::Keep(idx) => ring[*idx],
                    SpliceElem::New(p) => *p,
                })
                .collect();
            polygon_area(&pts).abs()
        };
        let chosen = if area_of(&seq_a) >= area_of(&seq_b) {
            seq_a
        } else {
            seq_b
        };
        self.parametric.splice_into_edge(0, &chosen)
    }

    /// Convenience: trim along the straight line through two explicit points.
    pub fn trim_by_line(
        &mut self,
        p0: Point2f32,
        p1: Point2f32,
    ) -> Result<Vec<ParameterId>, Error> {
        self.trim_by_stroke(&[p0, p1])
    }

    /// Trim with an orthogonal cut PATH (removing area): the stroke is
    /// orthogonalized, its endpoints attach to the outline, and the
    /// smaller-area side is replaced by the stepped cut — an orthogonal
    /// notch/chamfer (vs the straight diagonal [`trim_by_stroke`](Self::trim_by_stroke)
    /// cuts). See [`splice_path`](Self::splice_path).
    pub fn trim_ortho(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        self.splice_path(stroke, true, true)
    }

    /// Trim a wall with a faceted freeform CURVE (removing area): the drawn arc
    /// is RDP-simplified into a short polyline and the smaller-area side is
    /// replaced by it — e.g. a wall recessed around a tower. Facet walls aren't
    /// individually dimensioned and scale with the plan.
    pub fn trim_curve(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        self.splice_path(stroke, false, true)
    }

    /// Shared mechanics for the four path-splice operations
    /// ([`extend_ortho`](Self::extend_ortho), [`extend_curve`](Self::extend_curve),
    /// [`trim_ortho`](Self::trim_ortho), [`trim_curve`](Self::trim_curve)). Snaps
    /// the stroke's endpoints to the outline, cleans the path (`orthogonalize` →
    /// stepped H/V; else RDP-faceted curve), pins the endpoints onto their
    /// edges, and splices the path in — replacing the SHORTER arc when extending
    /// (`!keep_larger`, adds area) or the smaller-area side when trimming
    /// (`keep_larger`, removes area). Untouched vertices keep their identity and
    /// parameters; returns the newly-minted parameter IDs. The plan is left
    /// unchanged on error. Tolerances scale with the bounding-box diagonal, so
    /// it works in pixels (pre-dimensioning) or metres (post-dimensioning).
    fn splice_path(
        &mut self,
        stroke: &[Point2f32],
        orthogonalize: bool,
        keep_larger: bool,
    ) -> Result<Vec<ParameterId>, Error> {
        let err = |m: String| {
            if keep_larger {
                Error::TrimError(m)
            } else {
                Error::ExtendError(m)
            }
        };
        let diag = self.bounding_box_diagonal();
        let merge_tol = if diag > 0.0 {
            (diag * 0.01).max(1e-4)
        } else {
            10.0
        };
        // Orthogonalize (Ortho tools) snaps to H/V; simplify (Curve tools) just
        // RDP-facets the hand-drawn arc, keeping its curvature.
        let path = if orthogonalize {
            extend::orthogonalize_open_path(stroke, merge_tol)?
        } else {
            simplification::simplify_stroke(stroke, merge_tol)?
        };
        if path.len() < 2 {
            return Err(err("path collapsed".into()));
        }
        self.splice_cleaned(&path, keep_larger)
    }

    /// Splice an already-clean `path` (no further simplification) onto the
    /// primary ring: snap its endpoints to the outline, pin them onto their
    /// attach edges, and replace the SHORTER arc when extending
    /// (`!keep_larger`) or the smaller-area side when trimming (`keep_larger`).
    /// The shared tail of every path-splice — `splice_path` cleans first, the
    /// arc flow ([`begin_arc`](Self::begin_arc)) feeds it a ready 2-point chord.
    fn splice_cleaned(
        &mut self,
        path: &[Point2f32],
        keep_larger: bool,
    ) -> Result<Vec<ParameterId>, Error> {
        let err = |m: String| {
            if keep_larger {
                Error::TrimError(m)
            } else {
                Error::ExtendError(m)
            }
        };
        let ring = self.parametric.ring_points(0);
        if ring.len() < 3 {
            return Err(err("no existing polygon to modify".into()));
        }
        if path.len() < 2 {
            return Err(err("path collapsed".into()));
        }
        let diag = self.bounding_box_diagonal();
        let vertex_tol = (diag * 0.04).max(1e-4);
        let max_tol = (diag * 0.25).max(1e-3);

        // Snap the path's endpoints to find the attach edges, then pin them onto
        // those edges so they share the edges' perpendicular coordinate exprs.
        // `snapped*` is true when the endpoint landed on an existing CORNER.
        let start = path[0];
        let end = path[path.len() - 1];
        let (e1, t1, s_dist, snapped1) = extend::snap_to_polygon(&ring, start, vertex_tol);
        let (e2, t2, e_dist, snapped2) = extend::snap_to_polygon(&ring, end, vertex_tol);
        if s_dist > max_tol || e_dist > max_tol {
            return Err(err(format!(
                "endpoints too far from the outline (start={:.3}, end={:.3}, max={:.3})",
                s_dist, e_dist, max_tol
            )));
        }
        if e1 == e2 && (t1 - t2).abs() < 1e-3 {
            return Err(err("stroke starts and ends at the same point".into()));
        }

        let mut path = path.to_vec();
        let last = path.len() - 1;
        extend::pin_to_edge(&mut path[0], &ring, e1);
        extend::pin_to_edge(&mut path[last], &ring, e2);

        let mut chosen = if keep_larger {
            // Trim: build both arc-replacements; keep whichever leaves more area.
            let area_of = |seq: &[crate::extend::SpliceElem]| -> f32 {
                let pts: Vec<Point2f32> = seq
                    .iter()
                    .map(|e| match e {
                        crate::extend::SpliceElem::Keep(idx) => ring[*idx],
                        crate::extend::SpliceElem::New(pp) => *pp,
                    })
                    .collect();
                polygon_area(&pts).abs()
            };
            let seq_a = extend::splice_sequence(&ring, e1, t1, e2, t2, &path, true);
            let seq_b = extend::splice_sequence(&ring, e1, t1, e2, t2, &path, false);
            if area_of(&seq_a) >= area_of(&seq_b) {
                seq_a
            } else {
                seq_b
            }
        } else {
            // Extend: replace the SHORTER arc with the path (adds the feature).
            extend::plan_splice_sequence(&ring, e1, t1, e2, t2, &path)
        };

        // Corner reuse. When an attach endpoint snaps onto an EXISTING corner
        // vertex, reuse that vertex (Keep) instead of splicing a new point.
        // Otherwise the new point — pinned to one wall's perpendicular while its
        // surviving neighbour sits on the PERPENDICULAR wall — becomes a
        // free-floating vertex (both coords independent) that the dimensioning
        // solver can never isolate (it only prompts walls with exactly one
        // unknown). That strands a curve drawn across a corner, and makes an
        // orthogonal porch attached AT a corner over-parametrise and bend the
        // (already-fixed) wall it joins. Reusing the corner attaches at it, with
        // the corner's own already-known params and no extra param. Draw the
        // endpoint clearly off a corner to attach elsewhere. The guard avoids a
        // duplicate Keep when the corner happens to survive in the splice walk.
        use crate::extend::SpliceElem;
        let kept: std::collections::HashSet<usize> = chosen
            .iter()
            .filter_map(|el| match el {
                SpliceElem::Keep(i) => Some(*i),
                SpliceElem::New(_) => None,
            })
            .collect();
        let a1 = path[0];
        let a2 = path[last];
        let m = chosen.len();
        // Plan the reuses (and the realignment each implies) first, then apply —
        // so we don't borrow `chosen` mutably while reading neighbours.
        let mut ops: Vec<(usize, SpliceElem)> = Vec::new();
        for j in 0..m {
            let SpliceElem::New(pt) = chosen[j] else {
                continue;
            };
            let corner = if snapped1 && pt == a1 && !kept.contains(&e1) {
                Some(e1)
            } else if snapped2 && pt == a2 && !kept.contains(&e2) {
                Some(e2)
            } else {
                None
            };
            let Some(e) = corner else { continue };
            let c = ring[e];
            ops.push((j, SpliceElem::Keep(e)));
            // The path was orthogonalized to the DRAWN endpoint; snapping that
            // endpoint to the corner can leave the adjacent porch-wall point off
            // the corner's line, tilting the wall. Pull that neighbour onto the
            // corner's shared coordinate so the wall stays axis-aligned. Skip the
            // OTHER attach endpoint (a curve's chord, deliberately diagonal) and
            // any genuinely-diagonal neighbour.
            for nb in [(j + m - 1) % m, (j + 1) % m] {
                let SpliceElem::New(npt) = chosen[nb] else {
                    continue;
                };
                if npt == a1 || npt == a2 {
                    continue;
                }
                let mut np = npt;
                if (pt.x - npt.x).abs() < 1e-3 {
                    np.x = c.x; // vertical porch wall → share the corner's x
                    ops.push((nb, SpliceElem::New(np)));
                } else if (pt.y - npt.y).abs() < 1e-3 {
                    np.y = c.y; // horizontal porch wall → share the corner's y
                    ops.push((nb, SpliceElem::New(np)));
                }
            }
        }
        for (idx, val) in ops {
            chosen[idx] = val;
        }
        self.parametric.splice_into_edge(0, &chosen)
    }

    /// Begin a PROVISIONAL curved wall. Fits the freehand `stroke` to a circular
    /// arc (chord + signed bulge), then splices ONLY the two endpoints onto the
    /// outline as pinned vertices — a straight placeholder edge between them,
    /// leaving the shape otherwise UNCHANGED. Returns `(start_vertex,
    /// end_vertex, bulge_ratio)` where `bulge_ratio` is the signed sagitta as a
    /// fraction of the chord (left of start→end), so it survives both the
    /// pixel→metre rescale and any move of the endpoints (the bulge scales with
    /// the chord). The caller dimensions the two attach stubs through the normal
    /// flow, then calls [`finish_arc`](Self::finish_arc) to bake the arc in —
    /// nothing about the geometry changes until then.
    pub fn begin_arc(&mut self, stroke: &[Point2f32]) -> Result<(VertexId, VertexId, f32), Error> {
        let p1 = *stroke.first().ok_or(Error::InsufficientPoints)?;
        let p2 = *stroke.last().ok_or(Error::InsufficientPoints)?;
        let dx = p2.x - p1.x;
        let dy = p2.y - p1.y;
        let chord = (dx * dx + dy * dy).sqrt();
        if chord < 1e-4 {
            return Err(Error::ExtendError("curve endpoints coincide".into()));
        }
        let bulge_ratio = geom::stroke_sagitta(stroke) / chord;
        // Splice the straight chord (no bulge): inserts the two endpoints as
        // pinned vertices. An endpoint landing on an existing corner reuses that
        // corner vertex (see `splice_cleaned`) rather than floating a new point.
        self.splice_cleaned(&[p1, p2], false)?;
        let v1 = self
            .vertex_near(p1)
            .ok_or_else(|| Error::ExtendError("lost arc start vertex".into()))?;
        let v2 = self
            .vertex_near(p2)
            .ok_or_else(|| Error::ExtendError("lost arc end vertex".into()))?;
        Ok((v1, v2, bulge_ratio))
    }

    /// Bake a provisional arc (from [`begin_arc`](Self::begin_arc)) into the
    /// model now that its endpoints are positioned: facet the circular arc
    /// between `v1` and `v2` (bulge = `bulge_ratio` × the current chord) and
    /// replace the straight placeholder edge with the facet vertices. The two
    /// endpoints (and their dimensioned attach stubs) are kept untouched; only
    /// the placeholder edge is subdivided. A near-straight arc is left flat.
    ///
    /// Returns the freshly-minted parameter IDs of the facet vertices. The
    /// caller should treat these as KNOWN (locked): the curve's shape is fully
    /// determined once its attach lines are set, so the dimensioning solver must
    /// not later redistribute the facet vertices (`repair`/`aesthetic_rescale`
    /// only move un-set params) — doing so would flatten the arc back to a line.
    pub fn finish_arc(
        &mut self,
        v1: VertexId,
        v2: VertexId,
        bulge_ratio: f32,
    ) -> Result<Vec<ParameterId>, Error> {
        use crate::extend::SpliceElem;
        let (r1, i1) = self
            .find_vertex(v1)
            .ok_or_else(|| Error::ExtendError("arc start vertex gone".into()))?;
        let (r2, i2) = self
            .find_vertex(v2)
            .ok_or_else(|| Error::ExtendError("arc end vertex gone".into()))?;
        if r1 != r2 {
            return Err(Error::ExtendError(
                "arc endpoints on different rings".into(),
            ));
        }
        let ring = self.parametric.ring_points(r1);
        let n = ring.len();
        let p1 = ring[i1];
        let p2 = ring[i2];
        let chord = ((p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2)).sqrt();
        let facets = geom::circular_arc_points(p1, p2, bulge_ratio * chord, 5.0);
        if facets.len() <= 2 {
            return Ok(Vec::new()); // ~straight: leave the flat placeholder edge.
        }
        let interior = &facets[1..facets.len() - 1];

        // The placeholder is the edge joining v1 and v2 directly; find its ring
        // direction so the facets are inserted in between in the right order.
        let (start_idx, forward) = if (i1 + 1) % n == i2 {
            (i1, true) // ring goes v1 → v2
        } else if (i2 + 1) % n == i1 {
            (i2, false) // ring goes v2 → v1
        } else {
            return Err(Error::ExtendError(
                "arc endpoints are no longer adjacent".into(),
            ));
        };
        let mut seq: Vec<SpliceElem> = Vec::with_capacity(n + interior.len());
        for idx in 0..n {
            seq.push(SpliceElem::Keep(idx));
            if idx == start_idx {
                if forward {
                    seq.extend(interior.iter().map(|f| SpliceElem::New(*f)));
                } else {
                    seq.extend(interior.iter().rev().map(|f| SpliceElem::New(*f)));
                }
            }
        }
        self.parametric.splice_into_edge(r1, &seq)
    }

    /// World-space facet points of a provisional arc (between two pinned
    /// endpoints), for drawing the preview before it's committed. Uses the same
    /// sampling [`finish_arc`](Self::finish_arc) will. `None` if a vertex is gone.
    pub fn arc_preview(
        &self,
        v1: VertexId,
        v2: VertexId,
        bulge_ratio: f32,
    ) -> Option<Vec<Point2f32>> {
        let (_, i1) = self.find_vertex(v1)?;
        let (_, i2) = self.find_vertex(v2)?;
        let ring = self.parametric.ring_points(0);
        let p1 = *ring.get(i1)?;
        let p2 = *ring.get(i2)?;
        let chord = ((p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2)).sqrt();
        Some(geom::circular_arc_points(p1, p2, bulge_ratio * chord, 5.0))
    }

    /// `VertexId` of the primary ring's vertex closest to `pt` (used to recover
    /// the endpoints `begin_arc` just spliced in by their pinned position).
    fn vertex_near(&self, pt: Point2f32) -> Option<VertexId> {
        let ring = self.parametric.ring_points(0);
        let mut best: Option<(f32, usize)> = None;
        for (i, v) in ring.iter().enumerate() {
            let d = (v.x - pt.x).powi(2) + (v.y - pt.y).powi(2);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
        best.and_then(|(_, i)| self.vertex_id_at(0, i))
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

    /// Whether `(x, y)` lies inside the primary ring (ray-casting). Used to
    /// hit-test which candidate region a tap landed in.
    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        let ring = self.parametric.ring_points(0);
        let n = ring.len();
        if n < 3 {
            return false;
        }
        let mut inside = false;
        let mut j = n - 1;
        for i in 0..n {
            let (a, b) = (ring[i], ring[j]);
            if (a.y > y) != (b.y > y) {
                let xc = a.x + (y - a.y) / (b.y - a.y) * (b.x - a.x);
                if x < xc {
                    inside = !inside;
                }
            }
            j = i;
        }
        inside
    }

    /// Clamp each point that lies OUTSIDE this plan's outline back onto its
    /// boundary (nearest point on any edge); points already inside are left as
    /// drawn. A containment guardrail: snap a drawn sub-area back inside the
    /// storey before anything else processes it, so a stroke that strays out is
    /// brought to the boundary rather than producing geometry outside the house.
    pub fn clamp_inside(&self, points: &[Point2f32]) -> Vec<Point2f32> {
        let ring = self.parametric.ring_points(0);
        let n = ring.len();
        if n < 3 {
            return points.to_vec();
        }
        points
            .iter()
            .map(|&p| {
                if self.contains_point(p.x, p.y) {
                    return p;
                }
                let mut best = p;
                let mut best_d = f32::MAX;
                for i in 0..n {
                    let (a, b) = (ring[i], ring[(i + 1) % n]);
                    let (q, _t, d) = geom::project_point_to_segment(p, a, b);
                    if d < best_d {
                        best_d = d;
                        best = q;
                    }
                }
                best
            })
            .collect()
    }

    /// First-stage clip of a freehand sub-area stroke ONTO the storey, run before
    /// any simplification. A CLOSED loop is intersected with the outline polygon,
    /// so the loop is routed AROUND concavities (a trimmed-out notch is removed
    /// from the area, not bridged) — unlike the per-point [`clamp_inside`], which
    /// collapses a stroke drawn across a notch onto its corners. An OPEN path (or
    /// a failed/degenerate clip) falls back to `clamp_inside`. `close_gap_m` is
    /// the closed-loop threshold (see [`SubareaTuning::close_gap_m`]).
    pub fn clip_stroke_inside(&self, points: &[Point2f32], close_gap_m: f32) -> Vec<Point2f32> {
        let closed = points.len() >= 3
            && points
                .first()
                .zip(points.last())
                .map(|(a, b)| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt() < close_gap_m)
                .unwrap_or(false);
        if closed {
            if let Some(clipped) = self.clip_loop_to_outline(points) {
                return clipped;
            }
        }
        self.clamp_inside(points)
    }

    /// Intersect a closed freehand loop with the outline polygon and return the
    /// largest resulting piece's boundary (the loop clipped to the storey, voids
    /// removed). `None` if the intersection is empty or degenerate.
    fn clip_loop_to_outline(&self, loop_pts: &[Point2f32]) -> Option<Vec<Point2f32>> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let to_poly = |pts: &[Point2f32]| -> geo::Polygon<f64> {
            geo::Polygon::new(
                geo::LineString::from(
                    pts.iter()
                        .map(|p| (p.x as f64, p.y as f64))
                        .collect::<Vec<_>>(),
                ),
                vec![],
            )
        };
        let outline = to_poly(&self.parametric.ring_points(0));
        let stroke = to_poly(loop_pts);
        let clipped = stroke.intersection(&outline);
        let best = clipped.0.into_iter().max_by(|a, b| {
            a.unsigned_area()
                .partial_cmp(&b.unsigned_area())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let ring: Vec<Point2f32> = best
            .exterior()
            .points()
            .map(|c| Point2f32::new(c.x() as f32, c.y() as f32))
            .collect();
        (ring.len() >= 4).then_some(ring)
    }

    /// This plan MINUS the union of `holes`, as the largest remaining piece's
    /// boundary (a fresh `FloorPlan`). Carving a sub-area against this — instead
    /// of the raw outline — makes it snap to the holes' shared edges AND keeps it
    /// from overlapping them (the region excludes them), so e.g. a new
    /// construction phase tiles cleanly against the phases already drawn. `None`
    /// when nothing meaningful remains. NB: a subtraction that punches an interior
    /// HOLE keeps only the exterior boundary (phase areas tile from the edges in
    /// practice; an enclosed leftover is rare and deferred).
    pub fn subtract(&self, holes: &[Vec<Point2f32>]) -> Option<FloorPlan> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let to_poly = |pts: &[Point2f32]| -> geo::Polygon<f64> {
            geo::Polygon::new(
                geo::LineString::from(
                    pts.iter()
                        .map(|p| (p.x as f64, p.y as f64))
                        .collect::<Vec<_>>(),
                ),
                vec![],
            )
        };
        let mut region: geo::MultiPolygon<f64> =
            geo::MultiPolygon::new(vec![to_poly(&self.parametric.ring_points(0))]);
        for h in holes {
            if h.len() >= 3 {
                region = region.difference(&geo::MultiPolygon::new(vec![to_poly(h)]));
            }
        }
        let best = region.0.into_iter().max_by(|a, b| {
            a.unsigned_area()
                .partial_cmp(&b.unsigned_area())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        let mut ring: Vec<Point2f32> = best
            .exterior()
            .points()
            .map(|c| Point2f32::new(c.x() as f32, c.y() as f32))
            .collect();
        if ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        (ring.len() >= 3)
            .then(|| FloorPlan::from_clean_ring(&ring).ok())
            .flatten()
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

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A baked (faceted) curve is just extra straight-edged vertices + params in
    /// the parametric model, so saving and reopening a BER must reproduce the
    /// curve — facets, geometry, and parameter count — not a straight line.
    #[test]
    fn baked_curve_survives_json_roundtrip() {
        let square = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        let arc = vec![
            p(3.0, 10.0),
            p(4.0, 11.0),
            p(5.0, 11.5),
            p(6.0, 11.0),
            p(7.0, 10.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        plan.finish_arc(v1, v2, bulge).unwrap();

        let diag = |pl: &FloorPlan| {
            let r = &pl.polygons()[0];
            let n = r.len();
            (0..n)
                .filter(|&i| {
                    let (a, b) = (r[i], r[(i + 1) % n]);
                    (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3
                })
                .count()
        };
        let verts_before = plan.polygons()[0].len();
        let params_before = plan.active_parameter_count();
        let diag_before = diag(&plan);
        assert!(
            diag_before >= 2,
            "precondition: the curve baked into facets"
        );

        let json = serde_json::to_string(&plan).expect("serialize");
        let back: FloorPlan = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            back.polygons()[0].len(),
            verts_before,
            "facet vertices kept"
        );
        assert_eq!(back.active_parameter_count(), params_before);
        assert_eq!(diag(&back), diag_before, "the curve's facets must survive");
        for (a, b) in back
            .polygons()
            .iter()
            .flatten()
            .zip(plan.polygons().iter().flatten())
        {
            assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
        }
    }

    /// The persisted snapshot must preserve the parametric structure, not just
    /// the points: collinearity equivalences are shared parameters, so the
    /// active-parameter count (which drops when walls share a coord) is the
    /// signal. A square has 2 params; after a porch extend it gains a few. The
    /// round-trip must reproduce that exactly — otherwise reopening a saved BER
    /// would lose the equivalences.
    #[test]
    fn floor_plan_json_roundtrip_preserves_parametric_structure() {
        // Last point mimics the user's "lift near the start" drag point that
        // `from_points_to_orthogonal` drops for an implicitly-closed stroke.
        let square = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        // Porch out of the top edge — creates new shared-param equivalences.
        let porch = vec![p(3.0, 0.0), p(3.0, -4.0), p(7.0, -4.0), p(7.0, 0.0)];
        plan.extend_ortho(&porch).unwrap();

        let params_before = plan.active_parameter_count();
        let rings_before: Vec<usize> = plan.polygons().iter().map(|r| r.len()).collect();

        let json = serde_json::to_string(&plan).expect("serialize");
        let back: FloorPlan = serde_json::from_str(&json).expect("deserialize");

        // The private id counters MUST persist: if they reset, a post-reload
        // edit would mint param/vertex ids that collide with existing ones and
        // silently corrupt `known_params` / vertex identity.
        assert!(
            json.contains("next_param_id") && json.contains("next_vertex_id"),
            "id counters must be serialized to prevent post-reload id collisions"
        );

        assert_eq!(
            back.active_parameter_count(),
            params_before,
            "shared-parameter equivalences must survive the round-trip"
        );
        let rings_after: Vec<usize> = back.polygons().iter().map(|r| r.len()).collect();
        assert_eq!(rings_after, rings_before);
        // Geometry matches too (within f32 round-trip tolerance).
        for (a, b) in back
            .polygons()
            .iter()
            .flatten()
            .zip(plan.polygons().iter().flatten())
        {
            assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
        }
    }
}

#[cfg(test)]
mod trim_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// `outline_following_trace` keeps a CURVE the trace followed (the boundary
    /// is copied verbatim there) while squaring off only a wall the trace added —
    /// the partial-reproduce a whole-trace orthogonalize would have flattened.
    #[test]
    fn outline_following_trace_keeps_curve_and_squares_new_wall() {
        // Previous storey: a box whose RIGHT side bulges out via diagonal facets
        // (a persisted "curve"); everything else axis-aligned.
        let prev_ring = vec![
            p(0., 0.),
            p(4., 0.),
            p(4.3, 1.),
            p(4.5, 2.),
            p(4.3, 3.),
            p(4., 4.),
            p(0., 4.),
        ];
        let prev = FloorPlan::from_clean_ring(&prev_ring).unwrap();

        // Trace: hug the whole outline, but on the BOTTOM edge dip into a 1 m-deep
        // porch bump (a genuinely-new wall well off the boundary).
        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            let steps = 24;
            for s in 0..steps {
                let f = s as f32 / steps as f32;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        let mut trace = Vec::new();
        // bottom with a downward bump between x=1.5 and x=2.5
        sample(p(0., 0.), p(1.5, 0.), &mut trace);
        sample(p(1.5, 0.), p(1.5, -1.0), &mut trace);
        sample(p(1.5, -1.0), p(2.5, -1.0), &mut trace);
        sample(p(2.5, -1.0), p(2.5, 0.), &mut trace);
        sample(p(2.5, 0.), p(4., 0.), &mut trace);
        // up the curved right side, across the top, down the left — verbatim path
        sample(p(4., 0.), p(4.3, 1.), &mut trace);
        sample(p(4.3, 1.), p(4.5, 2.), &mut trace);
        sample(p(4.5, 2.), p(4.3, 3.), &mut trace);
        sample(p(4.3, 3.), p(4., 4.), &mut trace);
        sample(p(4., 4.), p(0., 4.), &mut trace);
        sample(p(0., 4.), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a follow/diverge trace should splice");
        let spliced = FloorPlan::from_clean_ring(&out).unwrap();

        // The curve survived: a diagonal (non-axis-aligned) edge remains.
        assert!(
            !spliced.non_orthogonal_edges().is_empty(),
            "the followed curve facets must be preserved verbatim, not squared off"
        );
        // The bulge vertices are carried up verbatim.
        let pts = spliced.get_points();
        assert!(
            pts.iter()
                .any(|q| (q.x - 4.5).abs() < 1e-3 && (q.y - 2.0).abs() < 1e-3),
            "the curve apex (4.5, 2.0) is copied from the storey below"
        );
        // The new porch bump is present (a wall well below the boundary).
        assert!(
            pts.iter().any(|q| q.y < -0.3),
            "the diverging porch wall is kept (squared off, but present)"
        );

        // Contrast: a pure full-follow (no bump) is NOT ours — it's a reproduce.
        let mut full = Vec::new();
        sample(p(0., 0.), p(4., 0.), &mut full);
        sample(p(4., 0.), p(4.3, 1.), &mut full);
        sample(p(4.3, 1.), p(4.5, 2.), &mut full);
        sample(p(4.5, 2.), p(4.3, 3.), &mut full);
        sample(p(4.3, 3.), p(4., 4.), &mut full);
        sample(p(4., 4.), p(0., 4.), &mut full);
        sample(p(0., 4.), p(0., 0.), &mut full);
        assert!(
            prev.outline_following_trace(&full, 0.3, 0.05, &SpliceTuning::default())
                .is_none(),
            "an all-following trace is a pure reproduce, not a splice"
        );
    }

    /// A single cut wall that GRAZES an interior step of the storey below (runs
    /// close and parallel to it for a stretch) must stay ONE wall — not split
    /// into two by the brief "following" graze, which would inject a spurious
    /// vertex and turn one dimension into three.
    #[test]
    fn outline_following_trace_keeps_a_grazing_cut_wall_whole() {
        // Storey below: a box with the LEFT side curved, and a SMALL step on the
        // right (an interior vertical edge at x=2.7, y 1.4→1.7).
        let prev = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(3., 0.),
            p(3., 1.4),
            p(2.7, 1.4),
            p(2.7, 1.7),
            p(3., 1.7),
            p(3., 3.),
            p(0., 3.),
            p(-0.25, 2.2),
            p(-0.35, 1.5),
            p(-0.25, 0.8),
        ])
        .unwrap();

        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            for s in 0..40 {
                let f = s as f32 / 40.0;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        // Cut at x=2.6 — 0.1 m left of the step, so the wall grazes it for
        // y 1.4→1.7 (within the follow tolerance).
        let mut trace = Vec::new();
        sample(p(0., 0.), p(2.6, 0.), &mut trace);
        sample(p(2.6, 0.), p(2.6, 3.), &mut trace);
        sample(p(2.6, 3.), p(0., 3.), &mut trace);
        sample(p(0., 3.), p(-0.25, 2.2), &mut trace);
        sample(p(-0.25, 2.2), p(-0.35, 1.5), &mut trace);
        sample(p(-0.35, 1.5), p(-0.25, 0.8), &mut trace);
        sample(p(-0.25, 0.8), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a cut past a step still splices");
        let n = out.len();
        // Exactly ONE long vertical wall (the cut) — not two split by the graze.
        let cut_walls = (0..n)
            .filter(|&i| {
                let (a, b) = (out[i], out[(i + 1) % n]);
                (a.x - b.x).abs() < 0.05 && (a.y - b.y).abs() > 1.5
            })
            .count();
        assert_eq!(cut_walls, 1, "the grazing cut wall stays one piece");
    }

    /// A new wall that CROSSES interior edges of the storey below (e.g. a cut
    /// that passes over a notch) must stay one clean wall — no spurious vertex
    /// injected at each crossing, and the notch's interior vertices (which the
    /// trace never visited) must not be pulled in.
    #[test]
    fn outline_following_trace_no_vertex_where_new_wall_crosses_below_storey() {
        // Storey below: a box with a NOTCH on the right (interior horizontal
        // edges at y=1 and y=2, x 2→4) and a curved LEFT side.
        let prev = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(4., 0.),
            p(4., 1.),
            p(2., 1.),
            p(2., 2.),
            p(4., 2.),
            p(4., 3.),
            p(0., 3.),
            p(-0.25, 2.2),
            p(-0.35, 1.5),
            p(-0.25, 0.8),
        ])
        .unwrap();

        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            for s in 0..40 {
                let f = s as f32 / 40.0;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        // Cut straight up at x=3 — crosses the notch edges at (3,1) and (3,2).
        let mut trace = Vec::new();
        sample(p(0., 0.), p(3., 0.), &mut trace);
        sample(p(3., 0.), p(3., 3.), &mut trace);
        sample(p(3., 3.), p(0., 3.), &mut trace);
        sample(p(0., 3.), p(-0.25, 2.2), &mut trace);
        sample(p(-0.25, 2.2), p(-0.35, 1.5), &mut trace);
        sample(p(-0.35, 1.5), p(-0.25, 0.8), &mut trace);
        sample(p(-0.25, 0.8), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a cut over a notch still splices");
        let n = out.len();
        // No vertex at the crossings (3,1) / (3,2) — the cut wall isn't split.
        for cross in [(3.0_f32, 1.0_f32), (3.0, 2.0)] {
            assert!(
                !out.iter()
                    .any(|q| (q.x - cross.0).abs() < 0.1 && (q.y - cross.1).abs() < 0.1),
                "no spurious vertex at the crossing {cross:?}"
            );
        }
        // The notch's interior corners (x=2) were never traced → not pulled in.
        assert!(
            !out.iter()
                .any(|q| (q.x - 2.0).abs() < 0.1 && (q.y - 1.0).abs() < 0.2),
            "the unvisited notch interior is not included"
        );
        // The cut is still exactly ONE long vertical wall.
        let cut_walls = (0..n)
            .filter(|&i| {
                let (a, b) = (out[i], out[(i + 1) % n]);
                (a.x - b.x).abs() < 0.05 && (a.y - b.y).abs() > 1.5
            })
            .count();
        assert_eq!(cut_walls, 1, "the cut crossing the notch stays one wall");
    }

    /// A new wall drawn TILTED (its two ends, where the trace left and rejoined
    /// the boundary, don't share a coordinate) is squared onto its axis — so
    /// dimensioning an adjacent wall later doesn't flip its lean and get rejected.
    #[test]
    fn outline_following_trace_squares_a_tilted_new_wall() {
        // 3x3 box with a curved LEFT side (diagonal facets to preserve).
        let prev = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(3., 0.),
            p(3., 3.),
            p(0., 3.),
            p(-0.25, 2.2),
            p(-0.35, 1.5),
            p(-0.25, 0.8),
        ])
        .unwrap();

        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            for s in 0..24 {
                let f = s as f32 / 24.0;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        // Follow the bottom, then a clearly-set-back right wall drawn with a
        // TILT (x=2.5 at the bottom, x=2.6 at the top), then the top and the
        // curved left side.
        let mut trace = Vec::new();
        sample(p(0., 0.), p(2.5, 0.), &mut trace);
        sample(p(2.5, 0.), p(2.6, 3.), &mut trace);
        sample(p(2.6, 3.), p(0., 3.), &mut trace);
        sample(p(0., 3.), p(-0.25, 2.2), &mut trace);
        sample(p(-0.25, 2.2), p(-0.35, 1.5), &mut trace);
        sample(p(-0.35, 1.5), p(-0.25, 0.8), &mut trace);
        sample(p(-0.25, 0.8), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a set-back right wall splices");
        let n = out.len();
        // The long near-vertical new wall is now EXACTLY vertical (dx≈0).
        let tilted = (0..n).any(|i| {
            let (a, b) = (out[i], out[(i + 1) % n]);
            let (dx, dy) = ((a.x - b.x).abs(), (a.y - b.y).abs());
            dy > 2.0 && dx > 1e-3 // a long vertical-ish wall still off-axis
        });
        assert!(!tilted, "the new wall was squared onto its axis");
        // The left curve is still there.
        assert!(
            !FloorPlan::from_clean_ring(&out)
                .unwrap()
                .non_orthogonal_edges()
                .is_empty(),
            "the followed curve is preserved"
        );
    }

    /// `uncovered_perimeter_run` flags a side of the outline the trace never
    /// visited — what tells a real reproduce from a smaller storey that stops
    /// early on a side (even when its new walls snap onto the outline elsewhere).
    #[test]
    fn uncovered_perimeter_run_detects_a_skipped_side() {
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap();
        // Cover bottom, right, top — but NOT the left wall (x=0). Stay off the
        // corners so the left edge truly has nothing within tolerance.
        let mut pts = Vec::new();
        for k in 1..20 {
            let f = k as f32 * 0.5;
            pts.push(p(f, 0.0)); // bottom
            pts.push(p(10.0, f)); // right
            pts.push(p(f, 10.0)); // top
        }
        let un = outline.uncovered_perimeter_run(&pts, 0.3);
        assert!(
            (un - 10.0).abs() < 1.0,
            "the unvisited left wall (~10 m) is the gap, got {un}"
        );
        // Now trace the left wall too → fully covered.
        for k in 1..20 {
            pts.push(p(0.0, k as f32 * 0.5));
        }
        assert!(
            outline.uncovered_perimeter_run(&pts, 0.3) < 1.0,
            "fully covered now"
        );
    }

    /// A living area cut from a storey with a CURVED wall must follow the whole
    /// curve — including the curve-START corner where the straight wall meets the
    /// first (gentle, near-tangent) facet. That facet's small off-axis delta was
    /// previously mistaken for "collinear" with the straight wall and the corner
    /// dropped, so the region skipped a vertex and stopped hugging the boundary.
    #[test]
    fn cut_region_follows_curve_including_the_start_vertex() {
        // Box whose top-left is a curve down to (0, 2.5); the LAST facet
        // (0.015,2.6)->(0,2.5) is nearly tangent to the straight left wall below.
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(4., 0.),
            p(4., 4.),
            p(1., 4.),
            p(0.5, 3.6),
            p(0.2, 3.0),
            p(0.015, 2.6),
            p(0., 2.5),
        ])
        .unwrap();
        // Sanity: the outline itself keeps the curve-start corner.
        let has = |r: &[Point2f32], x: f32, y: f32| {
            r.iter()
                .any(|q| (q.x - x).abs() < 2e-2 && (q.y - y).abs() < 2e-2)
        };
        assert!(
            has(&outline.get_points(), 0.0, 2.5),
            "outline keeps the curve-start corner"
        );
        // Vertical cut at x=2 → left (curved) region + right region.
        let cut = vec![p(2., 0.2), p(2., 3.8)];
        let (ra, rb) = outline
            .split_by_cut(&cut, 0.05, &SubareaTuning::default())
            .unwrap()
            .expect("vertical cut splits the storey");
        // The region carrying the curve must still include the curve-start corner.
        let left = if has(&ra.get_points(), 1.0, 4.0) {
            ra.get_points()
        } else {
            rb.get_points()
        };
        assert!(has(&left, 1.0, 4.0), "curved region has a curve point");
        assert!(
            has(&left, 0.0, 2.5),
            "curved region includes the curve-start corner (not skipped): {left:?}"
        );
    }

    /// A living-area vertex that sits MID-facet on the storey's curved/slanted
    /// boundary is pinned (so the curve-following wall isn't prompted) even
    /// though it's neither an outline corner nor part of a region edge lying on a
    /// single outline diagonal — the cases the older passes handle.
    #[test]
    fn params_pinned_includes_a_vertex_on_a_curve_facet() {
        // Outline: box with a slanted (faceted) top-right edge (4,3)->(2,4).
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(4., 0.), p(4., 3.), p(2., 4.), p(0., 4.)])
                .unwrap();
        // Area whose top-right corner sits MID-facet at (3, 3.5); its other
        // corners are free interior points and its edges there are axis-aligned,
        // so only the per-vertex facet pass can pin it.
        let area =
            FloorPlan::from_clean_ring(&[p(1., 2.), p(3., 2.), p(3., 3.5), p(1., 3.5)]).unwrap();
        let pts = area.get_points();
        let idx = pts
            .iter()
            .position(|q| (q.x - 3.0).abs() < 1e-3 && (q.y - 3.5).abs() < 1e-3)
            .expect("area has the mid-facet vertex");
        let pinned = outline.params_pinned_to_outline(&area);
        let xp = area.vertex_x_param(0, idx);
        let yp = area.vertex_y_param(0, idx);
        assert!(
            xp.is_some_and(|p| pinned.contains(&p)),
            "mid-facet vertex's x param is pinned"
        );
        assert!(
            yp.is_some_and(|p| pinned.contains(&p)),
            "mid-facet vertex's y param is pinned"
        );
    }

    /// `from_points_to_world_orthogonal` aligns a rotated trace to the world
    /// axes, so every edge ends up horizontal or vertical — unlike the
    /// dominant-angle default, which (beyond its ~15° axis-snap window) keeps
    /// the whole shape tilted. This is what guarantees a new storey stays
    /// aligned with the (axis-aligned) storey below regardless of trace angle.
    #[test]
    fn world_orthogonal_removes_a_tilt() {
        // A ~10x10 square rotated ~20° about the origin — beyond the dominant-
        // angle path's 15° snap-to-axis window, so the two paths diverge.
        let theta = 0.35_f32; // ~20°
        let (c, s) = (theta.cos(), theta.sin());
        let rot = |x: f32, y: f32| p(x * c - y * s, x * s + y * c);
        let stroke = vec![
            rot(0.0, 0.0),
            rot(10.0, 0.0),
            rot(10.0, 10.0),
            rot(0.0, 10.0),
            rot(0.1, 0.1),
        ];

        // Dominant-angle orthogonalization keeps the tilt → edges aren't on the
        // world axes.
        let tilted = FloorPlan::from_points_to_orthogonal(&stroke, 0.1, true).unwrap();
        assert!(
            !tilted.non_orthogonal_edges().is_empty(),
            "dominant-angle orthogonalization preserves the tilt"
        );

        // World orthogonalization flattens it onto the world axes.
        let flat = FloorPlan::from_points_to_world_orthogonal(&stroke, 0.1, true).unwrap();
        assert!(
            flat.non_orthogonal_edges().is_empty(),
            "world orthogonalization aligns every edge to the world axes: {:?}",
            flat.non_orthogonal_edges()
        );
    }

    fn unit_square_plan() -> FloorPlan {
        // 5th point is the "lift near start" drag point dropped on close.
        let stroke = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        FloorPlan::from_points_to_orthogonal(&stroke, 0.5, true).unwrap()
    }

    /// A living area in a corner of the storey: the user draws only the two
    /// NEW interior walls (an open L from the left wall across to the bottom
    /// wall); the two shared house walls are filled in along the outline, so
    /// the result is a self-contained quad hugging the corner.
    #[test]
    fn carve_subarea_shares_corner_walls_with_the_outline() {
        let outline = unit_square_plan();
        let path = vec![p(0.0, 4.0), p(4.0, 4.0), p(4.0, 0.0)];
        let room = outline
            .carve_subarea(&path, 0.5, &SubareaTuning::default())
            .unwrap();
        assert_eq!(room.ring_count(), 1);
        let poly = room.polygons()[0].clone();
        assert_eq!(
            poly.len(),
            4,
            "corner room is a quad: 2 drawn + 2 shared walls"
        );
        let area = polygon_area(&poly).abs();
        assert!(
            (area - 16.0).abs() < 0.5,
            "4x4 corner room (~16), got {area}"
        );
        assert!(
            poly.iter().any(|v| v.x.abs() < 0.05 && v.y.abs() < 0.05),
            "it reuses the storey corner (0,0): {poly:?}"
        );
        // Two walls lie on the outline (x==0 and y==0).
        assert!(poly.iter().filter(|v| v.x.abs() < 0.05).count() >= 2);
        assert!(poly.iter().filter(|v| v.y.abs() < 0.05).count() >= 2);
        room.validate_simple().unwrap();
    }

    /// A living area drawn away from every wall doesn't share anything — it
    /// closes into a free-standing interior room.
    #[test]
    fn carve_subarea_interior_path_becomes_a_freestanding_room() {
        let outline = unit_square_plan();
        let path = vec![
            p(3.0, 3.0),
            p(6.0, 3.0),
            p(6.0, 6.0),
            p(3.0, 6.0),
            p(3.1, 3.1),
        ];
        let room = outline
            .carve_subarea(&path, 0.3, &SubareaTuning::default())
            .unwrap();
        assert_eq!(room.ring_count(), 1);
        assert!(room.polygons()[0].len() >= 4);
        let area = polygon_area(&room.polygons()[0]).abs();
        assert!(
            (area - 9.0).abs() < 1.0,
            "3x3 interior room (~9), got {area}"
        );
        room.validate_simple().unwrap();
    }

    /// Confirms the reported cause: if the top (closing) wall's perpendicular
    /// coordinate ends up off the anchor, the parametric build leaves it as a
    /// FREE parameter rather than tying it to the anchor — so the shape looks
    /// ~orthogonal but that free param can DRIFT when dimensioning nudges un-set
    /// params ("orthogonal until dimensioned, then less orthogonal"). The full
    /// `from_points_to_orthogonal` pipeline snaps the closing edge so the coord
    /// is tied (axis-aligned, no orphan free parameter).
    #[test]
    fn closing_edge_coord_is_tied_not_left_as_a_free_parameter() {
        // Staircase whose top (closing) wall is ~7 units off horizontal
        // (v7.y=284 vs v0.y=291).
        let stair8 = vec![
            p(73.0, 291.0),
            p(73.0, 619.0),
            p(202.0, 619.0),
            p(202.0, 711.0),
            p(397.0, 711.0),
            p(397.0, 615.0),
            p(621.0, 615.0),
            p(621.0, 284.0),
        ];

        // No-snap parametrize (what the orthogonalizer effectively produced for
        // the noisy stroke): the closing edge stays diagonal — its perpendicular
        // coord is a free parameter 7 units off the anchor.
        let untied = FloorPlan::from_clean_ring(&stair8).unwrap();
        let up = untied.polygons()[0].clone();
        let un = up.len();
        let untied_off = (up[un - 1].y - up[0].y)
            .abs()
            .min((up[un - 1].x - up[0].x).abs());
        assert!(
            untied_off > 1.0,
            "without the closing-edge snap the top wall is left diagonal \
             (free param, not tied): off={untied_off}"
        );

        // Full pipeline (with the closing-edge guarantee): the top wall is
        // snapped axis-aligned, so its coord ties to the anchor — no orphan
        // free param to drift.
        let mut stroke = stair8.clone();
        stroke.push(p(74.0, 290.0)); // trailing "lift near start" drag point
        let fixed = FloorPlan::from_points_to_orthogonal(&stroke, 8.0, true).unwrap();
        let fp = fixed.polygons()[0].clone();
        let m = fp.len();
        let fixed_off = (fp[m - 1].y - fp[0].y)
            .abs()
            .min((fp[m - 1].x - fp[0].x).abs());
        assert!(
            fixed_off < 1e-3,
            "the closing edge is snapped axis-aligned: off={fixed_off}"
        );
        fixed.validate_simple().unwrap();
    }

    /// Regression: a small room with a clear nook drawn inside a BIG house must
    /// keep the nook. The cleanup that strips collapse-remnants is scaled to the
    /// SUB-AREA's own size, not the house's — so the nook isn't wrongly erased
    /// just because the storey is large. (With the house-scaled tolerance this
    /// nook was flattened to a plain quad.)
    #[test]
    fn carve_subarea_keeps_a_real_nook_in_a_big_house() {
        let big_house = FloorPlan::from_points_to_orthogonal(
            &[
                p(0.0, 0.0),
                p(60.0, 0.0),
                p(60.0, 60.0),
                p(0.0, 60.0),
                p(0.5, 0.5),
            ],
            1.0,
            true,
        )
        .unwrap();
        // ~2m square room with a 0.3m-deep nook (15% of the side), free-standing.
        let path = vec![
            p(10.0, 10.0),
            p(12.0, 10.0),
            p(12.0, 10.7),
            p(12.3, 10.7),
            p(12.3, 11.3),
            p(12.0, 11.3),
            p(12.0, 12.0),
            p(10.0, 12.0),
            p(10.1, 10.1),
        ];
        let (mut mnx, mut mny, mut mxx, mut mxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for q in &path {
            mnx = mnx.min(q.x);
            mny = mny.min(q.y);
            mxx = mxx.max(q.x);
            mxy = mxy.max(q.y);
        }
        let tol = ((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt() * 0.02;
        let room = big_house
            .carve_subarea(&path, tol, &SubareaTuning::default())
            .unwrap();
        assert!(
            room.polygons()[0].len() > 4,
            "the nook survives even though the house is large: {:?}",
            room.polygons()[0]
        );
        room.validate_simple().unwrap();
    }

    /// Drawing a living area as a FULL closed loop: walls drawn close to a
    /// parallel house wall get corrected onto it, while interior walls are left
    /// alone. Here the left + bottom walls are drawn ~0.15 off the storey's
    /// left/bottom walls and must snap to x=0 / y=0.
    #[test]
    fn full_loop_living_area_walls_snap_onto_nearby_house_walls() {
        let outline = unit_square_plan(); // 10x10, corner at (0,0)
        let path = vec![
            p(0.15, 0.12), // near corner (0,0)
            p(4.0, 0.12),  // bottom wall, ~0.12 above the storey's y=0 wall
            p(4.0, 4.0),   // right wall — interior, far from any house wall
            p(0.15, 4.0),  // left wall, ~0.15 right of the storey's x=0 wall
            p(0.16, 0.13), // close
        ];
        let room = outline
            .carve_subarea(&path, 0.1, &SubareaTuning::default())
            .unwrap();
        let poly = room.polygons()[0].clone();
        assert!(
            poly.iter().any(|v| v.x.abs() < 0.01),
            "left wall snapped onto the storey wall x=0: {poly:?}"
        );
        assert!(
            poly.iter().any(|v| v.y.abs() < 0.01),
            "bottom wall snapped onto the storey wall y=0: {poly:?}"
        );
        // Interior walls untouched (still near x=4 / y=4, not pulled to x=10/y=10).
        assert!(
            poly.iter().any(|v| (v.x - 4.0).abs() < 0.2),
            "interior right wall left alone: {poly:?}"
        );
        assert!(room.non_orthogonal_edges().is_empty(), "stays orthogonal");
        room.validate_simple().unwrap();
    }

    /// The reported use case: an L (3/4 of a square, reflex corner) with a
    /// curve rounding the bottom-right notch — drawn from near the bottom corner
    /// (v5) to near the right corner (v3). Each endpoint lands on an existing
    /// corner, so it REUSES that corner vertex (its already-known params) rather
    /// than splicing a new point. Without that, an endpoint at a corner whose
    /// wall the curve deletes would float free and the dimensioning would stall.
    ///
    /// The L is sized so the deleted notch-top wall (length 5) is longer than
    /// the surviving right wall (length 1): an endpoint near v3 sits close
    /// enough to drop v3 yet far enough off the right wall to float — exactly
    /// the case corner-reuse turns into a clean attach.
    #[test]
    fn corner_curve_reuses_corner_vertices_and_bakes() {
        use std::collections::HashSet;
        let l = vec![
            p(0.0, 0.0),   // v0
            p(0.0, 5.0),   // v1
            p(6.0, 5.0),   // v2
            p(6.0, 4.0),   // v3  (right wall v2→v3 is SHORT, length 1)
            p(1.0, 4.0),   // v4  (notch-top v3→v4 is WIDE, length 5)
            p(1.0, 0.0),   // v5
            p(0.02, 0.02), // lift-near-start drag point, dropped on close
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&l, 0.05, true).unwrap();
        let old_vids: HashSet<VertexId> = (0..plan.polygons()[0].len())
            .filter_map(|i| plan.vertex_id_at(0, i))
            .collect();
        let known: HashSet<ParameterId> = plan.all_active_params();

        // Arc rounding the notch: start near corner v5 (bottom), end ~3mm from
        // corner v3 (right). Both endpoints land on existing corners.
        let arc = vec![
            p(0.95, 0.0),
            p(2.5, 1.2),
            p(4.2, 2.4),
            p(5.5, 3.4),
            p(5.997, 4.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();

        // Option #2: each endpoint REUSES the existing corner vertex — no new
        // floating param, the curve attaches AT the corner.
        assert!(
            old_vids.contains(&v1),
            "start endpoint should reuse an existing corner vertex, got {v1}"
        );
        assert!(
            old_vids.contains(&v2),
            "end endpoint should reuse an existing corner vertex, got {v2}"
        );

        // Both attach points are existing corners (known params), so the curve
        // is fully constrained immediately — nothing to dimension.
        let (_, i1) = plan.find_vertex(v1).unwrap();
        let (_, i2) = plan.find_vertex(v2).unwrap();
        let n = plan.polygons()[0].len();
        let edge_start = if (i1 + 1) % n == i2 { i1 } else { i2 };
        let deps = plan.wall_param_deps(0, edge_start);
        assert!(
            deps.iter().all(|q| known.contains(q)),
            "a corner-to-corner placeholder is already known; deps={deps:?} known={known:?}"
        );

        // It bakes into a real faceted curve, leaving a valid polygon.
        plan.finish_arc(v1, v2, bulge).unwrap();
        assert!(plan.validate_simple().is_ok());
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "the rounded corner should bake into facets, got {}",
            diagonal_edge_count(&plan)
        );
    }

    fn diagonal_edge_count(plan: &FloorPlan) -> usize {
        let ring = &plan.polygons()[0];
        let n = ring.len();
        let eps = 1e-3_f32;
        (0..n)
            .filter(|&i| {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                (a.x - b.x).abs() > eps && (a.y - b.y).abs() > eps
            })
            .count()
    }

    #[test]
    fn ortho_porch_at_a_corner_reuses_it_and_stays_orthogonal() {
        // A porch (orthogonal bump) added with ONE endpoint on an existing
        // square corner must reuse that corner — otherwise the corner endpoint
        // floats free, over-parametrises, and dimensioning bends the (fixed)
        // wall it joins. The porch is drawn FREEHAND-WONKY (as a stylus would):
        // orthogonalize cleans it to the drawn endpoint, then reusing the corner
        // must also pull the adjacent porch-wall point onto the corner so that
        // wall stays vertical. With both, the porch needs only 2 new params (its
        // other attach position + its depth) and everything stays orthogonal.
        let mut plan = unit_square_plan(); // corners incl. (10,10)
        let before = polygon_area(&plan.polygons()[0]).abs();
        // One end NEAR corner (10,10) but drawn a touch off; the rest wobbly.
        let porch = vec![p(10.3, 10.2), p(10.1, 14.1), p(5.8, 13.9), p(6.1, 10.3)];
        let minted = plan.extend_ortho(&porch).unwrap();

        assert_eq!(
            minted.len(),
            2,
            "a corner-anchored ortho porch needs exactly 2 params (depth + far attach), got {}",
            minted.len()
        );
        assert!(plan.validate_simple().is_ok());
        assert_eq!(
            diagonal_edge_count(&plan),
            0,
            "the porch and square must stay orthogonal (no tilted wall at the reused corner)"
        );
        assert!(
            polygon_area(&plan.polygons()[0]).abs() > before,
            "the porch adds area"
        );
        // The square corner (10,10) is preserved (reused), not replaced.
        assert!(plan.polygons()[0]
            .iter()
            .any(|q| (q.x - 10.0).abs() < 1e-3 && (q.y - 10.0).abs() < 1e-3));
    }

    #[test]
    fn remove_collinear_drops_flush_corner_but_keeps_preserved() {
        use std::collections::HashSet;
        // Freehand porch flush at corner (10,10): leaves that corner collinear
        // on the right wall (10,0)-(10,~14). It's spurious for the envelope.
        let mut plan = unit_square_plan();
        let porch = vec![p(10.3, 10.2), p(10.1, 14.1), p(5.8, 13.9), p(6.1, 10.3)];
        plan.extend_ortho(&porch).unwrap();
        let n_before = plan.polygons()[0].len();
        let area_before = polygon_area(&plan.polygons()[0]).abs();

        // Find the collinear corner's id.
        let corner_vid = {
            let pts = plan.polygons()[0].clone();
            let i = pts
                .iter()
                .position(|q| (q.x - 10.0).abs() < 1e-3 && (q.y - 10.0).abs() < 1e-3)
                .expect("reused corner present");
            plan.vertex_id_at(0, i).unwrap()
        };

        // Preserved → kept even though collinear (a future flooring boundary).
        let mut preserve = HashSet::new();
        preserve.insert(corner_vid);
        assert!(
            plan.remove_collinear_vertices(&preserve).is_empty(),
            "a preserved collinear vertex must be kept"
        );
        assert_eq!(plan.polygons()[0].len(), n_before);

        // Not preserved → the flush corner is dropped, envelope unchanged.
        let removed = plan.remove_collinear_vertices(&HashSet::new());
        assert_eq!(
            removed,
            vec![corner_vid],
            "exactly the flush corner is removed"
        );
        assert_eq!(plan.polygons()[0].len(), n_before - 1);
        assert!(plan.validate_simple().is_ok());
        assert_eq!(diagonal_edge_count(&plan), 0, "stays orthogonal");
        assert!(
            (polygon_area(&plan.polygons()[0]).abs() - area_before).abs() < 1e-2,
            "removing a collinear vertex must not change the shape"
        );
        // Idempotent — nothing collinear remains.
        assert!(plan.remove_collinear_vertices(&HashSet::new()).is_empty());
    }

    #[test]
    fn skipping_a_wall_keeps_prompting_others_and_the_same_param_elsewhere() {
        use std::collections::HashSet;
        // SKIP must be per-WALL, not per-parameter. Skipping a wall must not halt
        // the prompt loop, and a parameter the skipped wall would have set is
        // still offered via another wall that also controls it (e.g. the
        // opposite side of a rectangle). The user hit this twice: "I skip the
        // second of three and it won't ask me for the third / the derivable one."
        let plan = unit_square_plan(); // 2 params, each shared by an opposite pair
        let none: HashSet<VertexId> = HashSet::new();
        let nothing: HashSet<ParameterId> = HashSet::new();

        let (first_vid, first_param) = plan
            .next_promptable_wall(&nothing, &none)
            .expect("a wall should be promptable initially");

        // Skipping that wall does NOT stop the loop — another wall is offered.
        let mut skipped = HashSet::new();
        skipped.insert(first_vid);
        let (second_vid, _) = plan
            .next_promptable_wall(&nothing, &skipped)
            .expect("skipping one wall must not halt prompting");
        assert_ne!(second_vid, first_vid);

        // The skipped wall's parameter is still reachable through another wall
        // that controls it: with every OTHER parameter known, the opposite wall
        // is offered to set it (so a skipped value the user can measure
        // elsewhere is still asked for).
        let mut known: HashSet<ParameterId> = plan.all_active_params();
        known.remove(&first_param);
        let (vid, param) = plan
            .next_promptable_wall(&known, &skipped)
            .expect("the opposite wall should still set the skipped param");
        assert_eq!(
            param, first_param,
            "same param, reached via a different wall"
        );
        assert_ne!(vid, first_vid);
    }

    #[test]
    fn trim_cuts_corner_into_a_diagonal_wall() {
        let mut plan = unit_square_plan();
        assert_eq!(diagonal_edge_count(&plan), 0, "square starts orthogonal");
        // Cut across the top-right corner: right edge at y=4 to top edge at x=6.
        plan.trim_by_line(p(10.0, 4.0), p(6.0, 0.0)).unwrap();

        assert_eq!(
            diagonal_edge_count(&plan),
            1,
            "the cut edge should be the one non-orthogonal wall"
        );
        assert_eq!(plan.polygons()[0].len(), 5, "4 corners − 1 + 2 crossings");
        // Larger side kept: ~100 minus the small corner triangle (0.5*4*4=8).
        assert!(plan.validate_simple().is_ok());
    }

    #[test]
    fn trim_line_must_cross_the_outline() {
        let mut plan = unit_square_plan();
        // A line entirely outside the polygon never crosses.
        assert!(plan.trim_by_line(p(20.0, 0.0), p(20.0, 10.0)).is_err());
    }

    fn u_shape_plan() -> FloorPlan {
        // A U: a horizontal line at y=4 crosses its boundary FOUR times
        // (x = 0, 2, 4, 6) — the concave case that broke the old endpoint line.
        let stroke = vec![
            p(0.0, 0.0),
            p(6.0, 0.0),
            p(6.0, 6.0),
            p(4.0, 6.0),
            p(4.0, 2.0),
            p(2.0, 2.0),
            p(2.0, 6.0),
            p(0.0, 6.0),
            p(0.1, 0.1),
        ];
        FloorPlan::from_points_to_orthogonal(&stroke, 0.3, true).unwrap()
    }

    #[test]
    fn trim_picks_intended_two_of_four_crossings() {
        let mut plan = u_shape_plan();
        // Cut across the LEFT prong only (stroke spans x≈-1..3 at y≈4). The
        // fitted line crosses 4 edges, but we keep the two nearest the drawn
        // span (x=0 and x=2), not the far ones at x=4,6.
        let res = plan.trim_by_stroke(&[p(-1.0, 4.0), p(3.0, 4.0)]);
        assert!(
            res.is_ok(),
            "expected the left-prong cut to succeed: {:?}",
            res
        );
        assert!(plan.validate_simple().is_ok());
    }

    #[test]
    fn wavy_stroke_fits_to_the_same_cut_as_a_straight_one() {
        let mut wavy = unit_square_plan();
        let mut straight = unit_square_plan();
        // Wavy stroke wobbling around the straight line (10,4)→(6,0).
        let wavy_stroke = vec![
            p(10.0, 4.0),
            p(9.3, 2.8),
            p(8.2, 2.4),
            p(7.1, 1.2),
            p(6.0, 0.0),
        ];
        wavy.trim_by_stroke(&wavy_stroke).unwrap();
        straight.trim_by_line(p(10.0, 4.0), p(6.0, 0.0)).unwrap();
        let wp = &wavy.polygons()[0];
        let sp = &straight.polygons()[0];
        assert_eq!(wp.len(), sp.len());
        for (a, b) in wp.iter().zip(sp.iter()) {
            assert!((a.x - b.x).abs() < 0.3 && (a.y - b.y).abs() < 0.3);
        }
    }

    #[test]
    fn remove_vertex_collapses_to_a_valid_polygon() {
        // Collapsing a wall to zero removes a vertex; the result must stay a
        // valid (here non-orthogonal) simple polygon and keep ≥3 vertices.
        let mut plan = unit_square_plan();
        assert!(plan.remove_vertex(0, 1));
        assert_eq!(plan.polygons()[0].len(), 3);
        assert!(plan.validate_simple().is_ok());
        // Can't drop below a triangle.
        assert!(!plan.remove_vertex(0, 0));
    }

    #[test]
    fn trim_through_a_corner_is_accepted() {
        // Cut from corner (0,0) to the right wall: the would-be remnant wall at
        // the corner has length 0, which must be accepted (collapsed), not
        // rejected. Reuses the corner vertex rather than a zero-length edge.
        let mut plan = unit_square_plan();
        let res = plan.trim_by_stroke(&[p(0.0, 0.0), p(10.0, 4.0)]);
        assert!(res.is_ok(), "corner cut should be accepted: {:?}", res);
        assert!(plan.validate_simple().is_ok());
        assert_eq!(
            diagonal_edge_count(&plan),
            1,
            "the cut is the diagonal wall"
        );
    }

    #[test]
    fn trim_by_polygon_carves_an_orthogonal_notch() {
        let mut plan = unit_square_plan(); // 10x10, area 100
        let before = polygon_area(&plan.polygons()[0]).abs();
        assert!((before - 100.0).abs() < 1.0);

        // Rectangular notch into the top edge (y=10), down to y=7, x in [3,7].
        let notch = vec![p(3.0, 10.0), p(3.0, 7.0), p(7.0, 7.0), p(7.0, 10.0)];
        plan.trim_ortho(&notch).unwrap();

        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after < before,
            "notch should remove area ({after} vs {before})"
        );
        assert!(
            (before - after - 12.0).abs() < 1.0,
            "should remove ~12 (4x3); removed {}",
            before - after
        );
        assert!(plan.validate_simple().is_ok());
        // The notch's inner corners are present, and it stays orthogonal.
        let ring = &plan.polygons()[0];
        assert!(ring
            .iter()
            .any(|q| (q.x - 3.0).abs() < 1e-2 && (q.y - 7.0).abs() < 1e-2));
        assert!(ring
            .iter()
            .any(|q| (q.x - 7.0).abs() < 1e-2 && (q.y - 7.0).abs() < 1e-2));
        assert_eq!(
            diagonal_edge_count(&plan),
            0,
            "an ortho notch has no diagonal"
        );
    }

    #[test]
    fn curve_bulging_out_adds_a_faceted_arc() {
        let mut plan = unit_square_plan(); // 10x10, area 100
        let before = polygon_area(&plan.polygons()[0]).abs();
        // An arc bulging OUT of the top edge (y=10), apex ~(5,12).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 11.0),
            p(4.3, 11.7),
            p(5.0, 12.0),
            p(5.7, 11.7),
            p(6.4, 11.0),
            p(7.0, 10.0),
        ];
        plan.extend_curve(&arc).unwrap();

        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after > before,
            "outward curve adds area ({after} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        // Captured as a run of facets (>=2 non-axis-aligned edges).
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "curve should be multiple facets, got {}",
            diagonal_edge_count(&plan)
        );
    }

    #[test]
    fn provisional_arc_is_inert_until_finished() {
        let mut plan = unit_square_plan(); // 10x10, area 100
        let before = polygon_area(&plan.polygons()[0]).abs();
        let n_before = plan.polygons()[0].len();
        // Outward arc on the top edge, apex ~(5,12).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 11.0),
            p(5.0, 12.0),
            p(6.4, 11.0),
            p(7.0, 10.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        // begin_arc only splits the top edge — shape (area) is unchanged, but
        // the two endpoints now exist as vertices (2 more than before).
        let after_begin = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            (after_begin - before).abs() < 1e-2,
            "begin_arc must not change the area ({after_begin} vs {before})"
        );
        assert_eq!(plan.polygons()[0].len(), n_before + 2);
        assert_eq!(
            diagonal_edge_count(&plan),
            0,
            "still orthogonal while provisional"
        );
        assert!(bulge > 0.0, "outward bulge is positive (left of start→end)");
        // The preview is a multi-point arc bulging out past the top edge.
        let preview = plan.arc_preview(v1, v2, bulge).unwrap();
        assert!(preview.len() > 2 && preview.iter().any(|q| q.y > 10.5));

        // Finishing bakes the arc in: area grows and facets appear.
        plan.finish_arc(v1, v2, bulge).unwrap();
        let after_finish = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after_finish > before + 1.0,
            "finish_arc adds the bulge area ({after_finish} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "baked arc is a run of facets, got {}",
            diagonal_edge_count(&plan)
        );
    }

    #[test]
    fn small_scale_arc_facets_arent_rejected_as_collapsed() {
        // Regression for the reported extend-curve bug: on a small (metre-scale)
        // plan the faceted arc's edges are only a couple of cm — below the OLD
        // 3cm "collapsed edge" floor, which wrongly rejected the splice and left
        // the curve as a straight placeholder line.
        let mut plan = unit_square_plan(); // 10x10
        plan.scale_all(0.1); // 1m x 1m — walls ~1m, like the user's plan
        let before = polygon_area(&plan.polygons()[0]).abs();
        // Shallow outward bump on the top edge (y≈1), apex ~(0.5, 1.06).
        let arc = vec![
            p(0.3, 1.0),
            p(0.4, 1.04),
            p(0.5, 1.06),
            p(0.6, 1.04),
            p(0.7, 1.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        let baked = plan.finish_arc(v1, v2, bulge);
        assert!(
            baked.is_ok(),
            "a small-scale arc's cm facets must splice, not be rejected: {:?}",
            baked
        );
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "curve should bake into facets, got {}",
            diagonal_edge_count(&plan)
        );
        assert!(
            polygon_area(&plan.polygons()[0]).abs() > before,
            "outward arc adds area"
        );
        assert!(plan.validate_simple().is_ok());
    }

    #[test]
    fn provisional_arc_inward_carves_a_recess_on_finish() {
        // TrimCurve uses the same begin/finish path; an inward (negative) bulge
        // carves area rather than adding it — no separate keep-larger step.
        let mut plan = unit_square_plan();
        let before = polygon_area(&plan.polygons()[0]).abs();
        // Arc dipping INTO the square from the top edge, apex ~(5,8).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 9.0),
            p(5.0, 8.0),
            p(6.4, 9.0),
            p(7.0, 10.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        assert!(bulge < 0.0, "inward bulge is negative (right of start→end)");
        assert!((polygon_area(&plan.polygons()[0]).abs() - before).abs() < 1e-2);

        plan.finish_arc(v1, v2, bulge).unwrap();
        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after < before - 1.0,
            "inward arc removes area ({after} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        assert!(diagonal_edge_count(&plan) >= 2);
    }

    #[test]
    fn curve_bulging_in_carves_a_faceted_recess() {
        let mut plan = unit_square_plan();
        let before = polygon_area(&plan.polygons()[0]).abs();
        // An arc dipping INTO the square from the top edge, apex ~(5,8).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 9.0),
            p(4.3, 8.3),
            p(5.0, 8.0),
            p(5.7, 8.3),
            p(6.4, 9.0),
            p(7.0, 10.0),
        ];
        plan.trim_curve(&arc).unwrap();

        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after < before,
            "inward curve removes area ({after} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        assert!(diagonal_edge_count(&plan) >= 2);
    }
}

#[cfg(all(test, feature = "serde"))]
mod trim_serde_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    #[test]
    fn trimmed_storey_survives_json_roundtrip() {
        let stroke = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&stroke, 0.5, true).unwrap();
        plan.trim_by_line(p(10.0, 4.0), p(6.0, 0.0)).unwrap();

        let rings_before = plan.polygons();
        let params_before = plan.active_parameter_count();

        let json = serde_json::to_string(&plan).expect("serialize");
        let back: FloorPlan = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.active_parameter_count(), params_before);
        let rings_after = back.polygons();
        assert_eq!(rings_after.len(), rings_before.len());
        assert_eq!(rings_after[0].len(), rings_before[0].len());
        // The diagonal cut edge comes back intact.
        for (a, b) in rings_after[0].iter().zip(rings_before[0].iter()) {
            assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
        }
    }
}

#[cfg(test)]
mod cut_line_tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// Drawing a single straight CUT line across the storey (endpoints on two
    /// walls) splits it and takes one side as the living area — rather than
    /// erroring as "too few points". Picks the smaller region (shorter arc).
    #[test]
    fn cut_line_splits_off_a_region() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 6.), p(0., 6.), p(0.1, 0.1)],
            0.2,
            true,
        )
        .unwrap();
        // a near-vertical cut from the bottom wall to the top wall at x~3,
        // delivered as a noisy multi-point stroke (collapses to a straight line)
        let stroke = vec![p(3.0, 0.05), p(3.02, 2.0), p(2.98, 4.0), p(3.0, 5.95)];
        let room = outline
            .carve_subarea(&stroke, 0.1, &SubareaTuning::default())
            .unwrap();
        let poly = room.polygons()[0].clone();
        assert_eq!(poly.len(), 4, "cut yields a quad region: {poly:?}");
        assert!(
            poly.iter().any(|v| v.x.abs() < 0.05),
            "includes storey wall x=0"
        );
        assert!(
            poly.iter().any(|v| (v.x - 3.0).abs() < 0.2),
            "bounded by the cut at x~3"
        );
        let area = polygon_area(&poly).abs();
        assert!(
            (area - 18.0).abs() < 1.0,
            "smaller (left) region ~3x6=18, got {area}"
        );
        assert!(room.non_orthogonal_edges().is_empty());
        room.validate_simple().unwrap();
    }

    /// `split_by_cut` returns BOTH regions of a cut so the caller can let the
    /// user pick. The two areas are complementary (together ~= the whole storey)
    /// and both orthogonal here.
    #[test]
    fn split_by_cut_returns_both_regions() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 6.), p(0., 6.), p(0.1, 0.1)],
            0.2,
            true,
        )
        .unwrap();
        let stroke = vec![p(3.0, 0.05), p(3.0, 5.95)];
        let (a, b) = outline
            .split_by_cut(&stroke, 0.1, &SubareaTuning::default())
            .unwrap()
            .unwrap();
        let area_a = polygon_area(&a.polygons()[0]).abs();
        let area_b = polygon_area(&b.polygons()[0]).abs();
        // 3x6 + 7x6 == 60 (the whole 10x6 storey).
        assert!((area_a + area_b - 60.0).abs() < 1.0, "{area_a} + {area_b}");
        assert!(
            ((area_a - 18.0).abs() < 1.0 && (area_b - 42.0).abs() < 1.0)
                || ((area_a - 42.0).abs() < 1.0 && (area_b - 18.0).abs() < 1.0),
            "regions are the 18 and 42 halves: {area_a}, {area_b}"
        );
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();

        // A balanced L stroke is also a cut (offers both regions to pick),
        // following its bend rather than flattening — see `stepped_cut_follows_its_jog`.
        let bent = vec![p(3.0, 0.05), p(3.0, 3.0), p(6.0, 3.0), p(6.0, 5.95)];
        assert!(outline
            .split_by_cut(&bent, 0.1, &SubareaTuning::default())
            .unwrap()
            .is_some());
    }

    /// A cut whose endpoint lands on a DIAGONAL outline wall (a curve facet)
    /// must meet that wall ON it — not be dragged onto an axis. Reproduces the
    /// report that one of the two offered regions "didn't follow the boundary":
    /// `pin_to_edge` axis-snapped a cut endpoint sitting on a diagonal facet,
    /// pulling it off the boundary for the region that keeps that arc.
    #[test]
    fn cut_endpoint_on_a_diagonal_wall_stays_on_it() {
        // House with a slanted (diagonal) roof wall from (6,4) to (3,7).
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(6., 0.), p(6., 4.), p(3., 7.), p(0., 4.)])
                .unwrap();
        // Vertical cut from the bottom up to the diagonal: at x≈4 the diagonal
        // sits at y=6, so the top endpoint must land near (4, 6) — NOT (4, 4),
        // where an axis-snap of the (more-horizontal-than-vertical) diagonal
        // would drop it.
        let stroke = vec![p(4.0, 0.05), p(4.0, 5.9)];
        let (a, b) = outline
            .split_by_cut(&stroke, 0.1, &SubareaTuning::default())
            .unwrap()
            .expect("a vertical cut reaching the diagonal is a cut");
        let on_diag = |r: &FloorPlan| {
            r.polygons()[0]
                .iter()
                .any(|v| (v.x - 4.0).abs() < 0.2 && (v.y - 6.0).abs() < 0.25)
        };
        assert!(
            on_diag(&a) && on_diag(&b),
            "both regions meet the cut ON the diagonal (~4,6), not axis-snapped to (4,4)"
        );
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();
    }

    /// A THIN cut with a real orthogonal STEP (one dominant axis plus a short
    /// jog) is followed verbatim — the bend is kept, not flattened to a single
    /// axis line. Reproduces the report where an ortho cut "ignored the
    /// horizontal leg and split vertically".
    #[test]
    fn stepped_cut_follows_its_jog() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 6.), p(0., 6.), p(0.1, 0.1)],
            0.2,
            true,
        )
        .unwrap();
        // Up the bottom at x=3 to y=4, jog right to x=4, up to the top — thin
        // overall (1 m jog on a 6 m cut) but the jog is real.
        let stepped = vec![p(3.0, 0.05), p(3.0, 4.0), p(4.0, 4.0), p(4.0, 5.95)];
        let (a, b) = outline
            .split_by_cut(&stepped, 0.1, &SubareaTuning::default())
            .unwrap()
            .expect("a thin stepped stroke is still a cut");
        let has = |r: &FloorPlan, x: f32, y: f32| {
            r.polygons()[0]
                .iter()
                .any(|v| (v.x - x).abs() < 0.15 && (v.y - y).abs() < 0.15)
        };
        // One region carries the cut polyline → it must contain BOTH step corners
        // (3,4) and (4,4); a flattened single-axis cut would have neither.
        let region = if has(&a, 3.0, 4.0) { &a } else { &b };
        assert!(
            has(region, 3.0, 4.0) && has(region, 4.0, 4.0),
            "the cut follows its jog (keeps both step corners)"
        );
        // Regions still tile the whole storey.
        let total = polygon_area(&a.polygons()[0]).abs() + polygon_area(&b.polygons()[0]).abs();
        assert!(
            (total - 60.0).abs() < 1.0,
            "regions tile the 10x6 storey: {total}"
        );
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();
    }

    /// A cut endpoint that snaps to a CORNER whose wall runs parallel to the
    /// terminal leg must not tilt the leg — an orthogonal STEP is inserted so the
    /// cut stays axis-aligned. The bottom end sits just inside the (10,0) corner,
    /// so it snaps to that corner's vertical wall.
    #[test]
    fn cut_snapping_to_a_corner_stays_orthogonal() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0.1, 0.1)],
            0.2,
            true,
        )
        .unwrap();
        let stepped = vec![p(9.7, 0.15), p(9.7, 5.0), p(5.0, 5.0), p(5.0, 9.85)];
        let (a, b) = outline
            .split_by_cut(&stepped, 0.1, &SubareaTuning::default())
            .unwrap()
            .expect("a stepped cut near a corner is still a cut");
        assert!(
            a.non_orthogonal_edges().is_empty(),
            "region A stays orthogonal: {:?}",
            a.non_orthogonal_edges()
        );
        assert!(
            b.non_orthogonal_edges().is_empty(),
            "region B stays orthogonal: {:?}",
            b.non_orthogonal_edges()
        );
    }

    /// A cut leg drawn just off a PARALLEL boundary edge (e.g. a neighbour phase's
    /// shared edge in the available region) snaps ONTO it — the same
    /// `snap_walls_to_outline` the carve uses — so cut-based phases tile with no
    /// sliver. The drawn leg at x=2.66 must NOT survive; it lands on x=2.65.
    #[test]
    fn cut_leg_snaps_onto_a_parallel_boundary_edge() {
        let region =
            FloorPlan::from_clean_ring(&[p(2.65, 0.5), p(5.45, 0.5), p(5.45, 3.57), p(2.65, 3.57)])
                .unwrap();
        // L-cut: down a leg ~1 cm right of the left edge, then right to the far edge.
        let cut = vec![p(2.66, 3.57), p(2.66, 2.0), p(5.45, 2.0)];
        let (a, b) = region
            .split_by_cut(&cut, 0.05, &SubareaTuning::default())
            .unwrap()
            .expect("the L still splits the region");
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();
        let at_drawn = |r: &FloorPlan| r.polygons()[0].iter().any(|v| (v.x - 2.66).abs() < 4e-3);
        assert!(
            !at_drawn(&a) && !at_drawn(&b),
            "leg snapped to x=2.65, not left at the drawn x=2.66: {:?} {:?}",
            a.polygons()[0],
            b.polygons()[0]
        );
    }

    /// A STRAIGHT cut whose end snaps to a VERTEX must stay axis-aligned: the
    /// OTHER end is re-aligned onto the vertex (it slides along its perpendicular
    /// edge), so the cut is vertical THROUGH the vertex — not tilted, and with no
    /// stray free parameter. (Regression: a near-vertical cut whose bottom snapped
    /// to a vertex came out tilted because the step-fix only ran for ≥3-leg cuts.)
    #[test]
    fn straight_cut_snapping_to_a_vertex_stays_axis_aligned() {
        // (6,0) is a real corner (flat bottom meets a diagonal), so a vertical cut
        // up from it is interior; the left half is a clean rectangle.
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(6., 0.),
            p(8., 1.),
            p(10., 1.),
            p(10., 10.),
            p(0., 10.),
        ])
        .unwrap();
        // Near-vertical, bottom just inside the (6,0) vertex (so it snaps there).
        let stroke = vec![p(6.2, 0.2), p(6.0, 5.0), p(5.9, 9.8)];
        let (a, b) = outline
            .split_by_cut(&stroke, 0.1, &SubareaTuning::default())
            .unwrap()
            .expect("a near-vertical stroke splits the storey");
        // The left region (the rectangle with (0,0)) is fully orthogonal ONLY if
        // the cut stayed vertical through (6,0); a tilted cut would taint it.
        let left = if a.polygons()[0].iter().any(|v| v.x < 0.1 && v.y < 0.1) {
            &a
        } else {
            &b
        };
        assert!(
            left.non_orthogonal_edges().is_empty(),
            "cut stayed vertical (left region orthogonal): {:?}",
            left.polygons()[0]
        );
        let has = |r: &FloorPlan, x: f32, y: f32| {
            r.polygons()[0]
                .iter()
                .any(|v| (v.x - x).abs() < 0.2 && (v.y - y).abs() < 0.2)
        };
        assert!(
            has(left, 6., 0.) && has(left, 6., 10.),
            "cut spans the vertex at x=6: {:?}",
            left.polygons()[0]
        );
    }

    /// A cut stroke that ORTHOGONALIZES to a single line (a shallow "<" whose jog
    /// RDP half-drops) must degrade to a straight cut, NOT error out of the split
    /// — erroring would dump a stepped-cut attempt into the carve path.
    #[test]
    fn cut_that_squares_to_a_line_falls_back_to_a_straight_cut() {
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap();
        // Down-left then up-right: snap_to_orthogonal makes both legs vertical at
        // the same x, remove_collinear drops the middle → <3 points.
        let stroke = vec![p(5., 0.2), p(4., 5.), p(5.5, 9.8)];
        let res = outline.split_by_cut(&stroke, 0.1, &SubareaTuning::default());
        assert!(res.is_ok(), "split must not error: {:?}", res);
        let (a, b) = res.unwrap().expect("still splits into two regions");
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();
    }

    /// `subtract` (the "available region" for a new phase): the outline minus an
    /// existing phase leaves the remaining space, whose boundary is the shared
    /// edge — so a new phase carved against it snaps to that edge and can't
    /// overlap the old phase.
    #[test]
    fn subtract_leaves_the_available_region_with_the_shared_edge() {
        // 10×10 storey minus a 10×4 strip along the bottom → a 10×6 top region.
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap();
        let phase = vec![p(0., 0.), p(10., 0.), p(10., 4.), p(0., 4.)];
        let avail = outline.subtract(&[phase]).expect("a region remains");
        let pts = avail.get_points();
        assert!(
            geom::ring_contains_point(&pts, 5.0, 7.0),
            "top strip kept: {:?}",
            pts
        );
        assert!(
            !geom::ring_contains_point(&pts, 5.0, 2.0),
            "the existing phase (bottom strip) is excluded: {:?}",
            pts
        );
        // The shared edge y=4 is now part of the available region's boundary.
        assert!(
            pts.iter().any(|p| (p.y - 4.0).abs() < 1e-3),
            "shared edge y=4 present: {:?}",
            pts
        );
    }

    /// First-stage clip: a closed loop drawn STRAIGHT across a trimmed-out notch
    /// must be routed AROUND the void (the notch stays excluded), not bridged.
    #[test]
    fn clip_stroke_inside_routes_a_loop_around_a_notch() {
        // 6×4 storey with a notch bitten out of the top middle: x[2,4] y[2,4] is
        // OUTSIDE the storey.
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(6., 0.),
            p(6., 4.),
            p(4., 4.),
            p(4., 2.),
            p(2., 2.),
            p(2., 4.),
            p(0., 4.),
        ])
        .unwrap();
        // A loop drawn straight across the top, over the notch opening.
        let loop_pts = vec![p(1., 1.), p(5., 1.), p(5., 3.9), p(1., 3.9), p(1., 1.)];
        // The raw loop wrongly encloses the notch interior...
        assert!(geom::ring_contains_point(&loop_pts, 3.0, 3.0));
        // ...but clipped to the storey it does NOT, while keeping the real interior.
        let clipped = outline.clip_stroke_inside(&loop_pts, 0.7);
        assert!(
            !geom::ring_contains_point(&clipped, 3.0, 3.0),
            "notch stays excluded: {:?}",
            clipped
        );
        assert!(
            geom::ring_contains_point(&clipped, 1.5, 1.5),
            "storey interior kept: {:?}",
            clipped
        );
    }

    /// The `cut_simplify_ceil_m` ceiling keeps a real corner on a LONG cut, where
    /// the span-ratio alone would inflate the tolerance past the corner. A 2 m
    /// jog on a ~20 m cut deviates ~1 m from the chord: the raw ratio
    /// (0.1 × 20 = 2 m) would flatten it, but the 0.7 m ceiling keeps it.
    #[test]
    fn long_cut_keeps_a_big_step_via_the_ceiling() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(20., 0.), p(20., 20.), p(0., 20.), p(0.1, 0.1)],
            0.3,
            true,
        )
        .unwrap();
        // x=10 up to y=10, jog 2 m right to x=12, up to the top.
        let stepped = vec![p(10.0, 0.1), p(10.0, 10.0), p(12.0, 10.0), p(12.0, 19.9)];
        let (a, b) = outline
            .split_by_cut(&stepped, 0.1, &SubareaTuning::default())
            .unwrap()
            .expect("a stepped cut on a long storey is still a cut");
        let has = |r: &FloorPlan, x: f32, y: f32| {
            r.polygons()[0]
                .iter()
                .any(|v| (v.x - x).abs() < 0.3 && (v.y - y).abs() < 0.3)
        };
        let region = if has(&a, 10.0, 10.0) { &a } else { &b };
        assert!(
            has(region, 10.0, 10.0) && has(region, 12.0, 10.0),
            "the 2 m jog survives on a 20 m cut (ceiling keeps it): {:?}",
            region.polygons()[0]
        );
    }

    /// Reproduces the reported bug: a storey with diagonal trim-cut corners. A
    /// vertical cut near a corner must land on the HORIZONTAL top/bottom
    /// segments it crosses — not clamp to the adjacent diagonal — and stay
    /// vertical.
    #[test]
    fn cut_lands_on_crossed_segments_not_a_nearby_diagonal() {
        // Hexagon: 2x2-ish with both bottom corners cut diagonally.
        let outline = FloorPlan::from_clean_ring(&[
            p(1.29, 0.97),
            p(0.99, 0.97),
            p(0.39, 1.67),
            p(0.39, 2.97),
            p(2.39, 2.97),
            p(2.39, 1.57),
        ])
        .unwrap();
        assert_eq!(
            outline.non_orthogonal_edges().len(),
            2,
            "two diagonal corners"
        );
        // Vertical cut at x~1.1 — crosses the bottom segment (y=0.97) and the
        // top (y=2.97); the diagonal next to the corner is NOT crossed.
        let stroke = vec![p(1.1, 1.0), p(1.1, 2.0), p(1.1, 2.9)];
        let (ra, rb) = outline
            .split_by_cut(&stroke, 0.05, &SubareaTuning::default())
            .unwrap()
            .unwrap();
        let has = |r: &FloorPlan, w: (f32, f32)| {
            r.polygons()[0]
                .iter()
                .any(|v| (v.x - w.0).abs() < 0.06 && (v.y - w.1).abs() < 0.06)
        };
        assert!(
            has(&ra, (1.1, 0.97)) || has(&rb, (1.1, 0.97)),
            "cut hits the bottom SEGMENT at (1.1, 0.97), not the diagonal: {:?} {:?}",
            ra.polygons()[0],
            rb.polygons()[0]
        );
        assert!(
            has(&ra, (1.1, 2.97)) || has(&rb, (1.1, 2.97)),
            "cut hits the top segment at (1.1, 2.97)"
        );
        // The cut endpoint did NOT get dragged onto the corner (0.99, 0.97).
        assert!(
            !has(&ra, (0.99, 0.97)) || !has(&rb, (0.99, 0.97)),
            "the cut endpoint isn't clamped to the diagonal corner"
        );
    }

    /// Reproduces the reported failure: a hand-drawn near-vertical cut with
    /// wobble (so RDP leaves >2 points). It must still be recognized as a cut
    /// (thin + open), not rejected and sent to carve where it errors.
    #[test]
    fn wobbly_near_vertical_cut_is_still_a_cut() {
        let outline = FloorPlan::from_clean_ring(&[
            p(0.74, 0.814),
            p(0.44, 0.814),
            p(0.24, 1.314),
            p(0.24, 1.814),
            p(1.24, 1.814),
            p(1.24, 1.314),
        ])
        .unwrap();
        // y sweeps top→bottom; x wobbles around 0.53 (won't simplify to 2 pts).
        let mut stroke = Vec::new();
        let mut y = 0.82;
        let mut i = 0;
        while y < 1.81 {
            let wob = if i % 2 == 0 { 0.05 } else { -0.04 };
            stroke.push(p(0.53 + wob, y));
            y += 0.05;
            i += 1;
        }
        let res = outline
            .split_by_cut(&stroke, 0.02, &SubareaTuning::default())
            .unwrap();
        assert!(res.is_some(), "wobbly vertical line recognized as a cut");
        let (ra, rb) = res.unwrap();
        let has = |r: &FloorPlan, w: (f32, f32)| {
            r.polygons()[0]
                .iter()
                .any(|v| (v.x - w.0).abs() < 0.08 && (v.y - w.1).abs() < 0.08)
        };
        assert!(
            has(&ra, (0.53, 0.814)) || has(&rb, (0.53, 0.814)),
            "cut hits the bottom segment"
        );
        assert!(
            has(&ra, (0.53, 1.814)) || has(&rb, (0.53, 1.814)),
            "cut hits the top segment"
        );
    }

    /// When a cut region inherits a shared DIAGONAL wall from the storey, that
    /// diagonal is PINNED (fixed by the house) — so only the cut wall remains
    /// to dimension, not the diagonal.
    #[test]
    fn shared_diagonal_in_a_cut_region_is_pinned() {
        // Square with the bottom-right corner cut diagonally: diagonal (1,0)→(2,1).
        let outline = FloorPlan::from_clean_ring(&[
            p(0.0, 0.0),
            p(1.0, 0.0),
            p(2.0, 1.0),
            p(2.0, 2.0),
            p(0.0, 2.0),
        ])
        .unwrap();
        assert_eq!(outline.non_orthogonal_edges().len(), 1, "one diagonal");
        // Vertical cut at x~0.5: the RIGHT region keeps the diagonal.
        let stroke = vec![p(0.5, 0.1), p(0.5, 1.0), p(0.5, 1.9)];
        let (ra, rb) = outline
            .split_by_cut(&stroke, 0.02, &SubareaTuning::default())
            .unwrap()
            .unwrap();
        let diag_region = if !ra.non_orthogonal_edges().is_empty() {
            &ra
        } else {
            &rb
        };
        assert!(
            !diag_region.non_orthogonal_edges().is_empty(),
            "the right region kept the diagonal"
        );
        let total = diag_region.all_active_params().len();
        let pinned = outline.params_pinned_to_outline(diag_region).len();
        assert_eq!(
            total - pinned,
            1,
            "only the cut wall is free; the shared diagonal is pinned \
             (total={total}, pinned={pinned})"
        );
    }
}

#[cfg(test)]
mod diagonal_snap_tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// Outline with a DIAGONAL wall: a 10x10 square with the top-right corner
    /// cut off along the line x+y=16, i.e. edge (10,6)->(6,10).
    fn diag_outline() -> FloorPlan {
        let fp = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(10., 0.),
            p(10., 6.),
            p(6., 10.),
            p(0., 10.),
        ])
        .unwrap();
        assert!(
            !fp.non_orthogonal_edges().is_empty(),
            "the outline keeps its diagonal edge"
        );
        fp
    }

    /// Two living-area walls drawn just inside a diagonal house wall are pulled
    /// ONTO it, so the wall between them follows the diagonal (orthogonality
    /// relaxed to match the storey).
    #[test]
    fn walls_near_a_diagonal_outline_wall_snap_onto_it() {
        let outline = diag_outline();
        // Ring with two vertices ~0.35 off the diagonal line x+y=16.
        let pts = vec![p(4.0, 4.0), p(8.5, 7.0), p(7.0, 8.5), p(4.0, 8.0)];
        let snapped = outline.snap_walls_to_outline(&pts, 0.8, true);
        // The two near-diagonal vertices now lie on x+y=16.
        assert!(
            (snapped[1].x + snapped[1].y - 16.0).abs() < 0.05,
            "v1 on the diagonal: {:?}",
            snapped[1]
        );
        assert!(
            (snapped[2].x + snapped[2].y - 16.0).abs() < 0.05,
            "v2 on the diagonal: {:?}",
            snapped[2]
        );
        // Vertices far from the diagonal are untouched.
        assert_eq!(snapped[0], p(4.0, 4.0));
        assert_eq!(snapped[3], p(4.0, 8.0));
    }

    /// The diagonal snap reaches FURTHER than the axis snap, because forcing
    /// the interior to the storey axes leaves a wall drawn along the diagonal a
    /// little off the line. A vertex past the axis tolerance is still pulled on.
    #[test]
    fn diagonal_snap_reaches_past_the_axis_tolerance() {
        let outline = diag_outline(); // diagonal x+y=16
                                      // Two vertices ~0.35 off the diagonal — beyond the 0.2 axis tol but
                                      // within the diagonal reach (2.5x = 0.5).
        let pts = vec![p(4.0, 4.0), p(8.0, 7.5), p(7.5, 8.0), p(4.0, 8.0)];
        let snapped = outline.snap_walls_to_outline(&pts, 0.2, true);
        assert!(
            (snapped[1].x + snapped[1].y - 16.0).abs() < 0.05,
            "pulled onto the diagonal despite being past the axis tol: {:?}",
            snapped[1]
        );
        assert!(
            (snapped[2].x + snapped[2].y - 16.0).abs() < 0.05,
            "ditto: {:?}",
            snapped[2]
        );
        // The far interior vertices are untouched.
        assert_eq!(snapped[0], p(4.0, 4.0));
        assert_eq!(snapped[3], p(4.0, 8.0));
    }

    /// End to end: a closed-loop living area whose corner is drawn near the
    /// diagonal house wall snaps onto it and stays a valid shape.
    #[test]
    fn closed_loop_living_area_corner_follows_the_diagonal() {
        let outline = diag_outline();
        let stroke = vec![
            p(4.0, 4.0),
            p(8.7, 4.0),
            p(8.7, 7.2), // top-right corner ~0.07 m off the diagonal (within the
            p(4.0, 7.2), // capped on_tol — a realistic hug, not 0.2 m of slop)
            p(4.1, 4.1),
        ];
        let room = outline
            .carve_subarea(&stroke, 0.15, &SubareaTuning::default())
            .unwrap();
        let poly = room.polygons()[0].clone();
        assert!(
            poly.iter().any(|v| (v.x + v.y - 16.0).abs() < 0.1),
            "the near-diagonal corner snapped onto x+y=16: {poly:?}"
        );
        room.validate_simple().unwrap();
    }
}

#[cfg(test)]
mod boundary_loop_tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A closed-loop room drawn hugging a diagonal boundary follows the diagonal
    /// the FULL way (kept verbatim), with axis-aligned interior walls — the only
    /// non-orthogonal edge is the diagonal itself.
    #[test]
    fn closed_loop_follows_a_diagonal_boundary_fully() {
        let outline = FloorPlan::from_clean_ring(&[
            p(0.903, 0.554),
            p(0.403, 0.554),
            p(0.203, 0.954),
            p(0.203, 1.554),
            p(1.203, 1.554),
            p(1.203, 1.054),
        ])
        .unwrap();
        // Hug the v5->v0 diagonal (slightly off), then interior axis walls back.
        let stroke = vec![
            p(0.91, 0.56),
            p(1.0, 0.71),
            p(1.1, 0.88),
            p(1.19, 1.04),
            p(1.19, 1.3),
            p(0.7, 1.3),
            p(0.7, 0.56),
            p(0.91, 0.57),
        ];
        let room = outline
            .carve_subarea(&stroke, 0.02, &SubareaTuning::default())
            .unwrap();
        // Exactly one non-orthogonal edge — the diagonal — and it spans the whole
        // diagonal (from ~v0 (0.903,0.554) to ~v5 (1.203,1.054)).
        let bad = room.non_orthogonal_edges();
        assert_eq!(
            bad.len(),
            1,
            "only the diagonal is non-orthogonal: {:?}",
            room.polygons()[0]
        );
        let (_, _, a, b) = bad[0];
        let on_diag = |v: Point2f32| (v.y - 0.554 - 1.6667 * (v.x - 0.903)).abs() < 0.02;
        assert!(
            on_diag(a) && on_diag(b),
            "the wall lies on the diagonal line"
        );
        let span = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
        assert!(
            span > 0.5,
            "it follows the diagonal the full way (~0.58), got {span}"
        );
        // The follow reaches the diagonal's actual corners v0 (0.903,0.554) and
        // v5 (1.203,1.054) — not stopping short mid-diagonal.
        let at = |v: Point2f32, w: (f32, f32)| (v.x - w.0).abs() < 0.02 && (v.y - w.1).abs() < 0.02;
        assert!(
            (at(a, (0.903, 0.554)) && at(b, (1.203, 1.054)))
                || (at(a, (1.203, 1.054)) && at(b, (0.903, 0.554))),
            "diagonal runs corner-to-corner: {a:?}..{b:?}"
        );
        room.validate_simple().unwrap();

        // The shared diagonal wall is PINNED (fixed by the house), so it won't
        // be prompted for dimensioning — only the interior walls are free.
        let pinned = outline.params_pinned_to_outline(&room);
        let (_, e, _, _) = bad[0];
        let nn = room.polygons()[0].len();
        for idx in [e, (e + 1) % nn] {
            if let Some(pp) = room.vertex_x_param(0, idx) {
                assert!(pinned.contains(&pp), "diagonal endpoint x param is pinned");
            }
            if let Some(pp) = room.vertex_y_param(0, idx) {
                assert!(pinned.contains(&pp), "diagonal endpoint y param is pinned");
            }
        }
    }

    /// Guard: an axis-only storey (no diagonal) never takes the boundary-aware
    /// path — a closed-loop room stays a plain axis-aligned shape.
    #[test]
    fn axis_only_storey_is_unaffected_by_boundary_path() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0.2, 0.2)],
            0.5,
            true,
        )
        .unwrap();
        let stroke = vec![p(2., 2.), p(6., 2.), p(6., 6.), p(2., 6.), p(2.1, 2.1)];
        let room = outline
            .carve_subarea(&stroke, 0.1, &SubareaTuning::default())
            .unwrap();
        assert!(room.non_orthogonal_edges().is_empty(), "stays orthogonal");
    }
}

#[cfg(test)]
mod farwall_tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A boundary-aware room (hugging a diagonal) still has its OTHER walls
    /// snapped onto the storey: a far wall drawn just past the storey edge is
    /// pulled onto it, while the verbatim diagonal is left untouched.
    #[test]
    fn boundary_aware_room_still_snaps_its_axis_walls() {
        let outline = FloorPlan::from_clean_ring(&[
            p(2.677, 1.772),
            p(2.377, 1.772),
            p(0.377, 3.772),
            p(0.377, 4.772),
            p(3.377, 4.772),
            p(3.377, 2.772),
        ])
        .unwrap();
        // hug the v5->v0 diagonal, then a far wall ~28mm PAST the storey top (4.772).
        let stroke = vec![
            p(2.69, 1.79),
            p(2.9, 2.08),
            p(3.1, 2.37),
            p(3.37, 2.76),
            p(3.40, 3.5),
            p(3.40, 4.80),
            p(2.68, 4.80),
            p(2.68, 2.5),
            p(2.70, 1.80),
        ];
        let room = outline
            .carve_subarea(&stroke, 0.04, &SubareaTuning::default())
            .unwrap();
        // the diagonal follows fully (single non-orthogonal edge)
        assert_eq!(
            room.non_orthogonal_edges().len(),
            1,
            "diagonal only: {:?}",
            room.polygons()[0]
        );
        // the far wall snapped down onto the storey top (4.772), not left at 4.80
        let max_y = room.polygons()[0]
            .iter()
            .map(|v| v.y)
            .fold(f32::MIN, f32::max);
        assert!(
            (max_y - 4.772).abs() < 0.01,
            "far wall snapped to the storey edge, got {max_y}"
        );
        room.validate_simple().unwrap();
    }

    /// A living area hugging a FACETED CURVE keeps every facet (each a separate
    /// non-orthogonal edge, walked in order) and squares its interior cut. This
    /// is the case the retired `boundary_aware_loop_points` could not do (it only
    /// followed ONE diagonal edge) — the two-pass must.
    #[test]
    fn living_area_hugging_a_curve_keeps_every_facet() {
        // 10×10-ish storey whose top-right is a 3-facet curve.
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(10., 0.),
            p(10., 6.),
            p(8.5, 8.),
            p(6., 9.5),
            p(3., 10.),
            p(0., 10.),
        ])
        .unwrap();
        // Hug bottom → right wall → the 3 curve facets, then an interior wall
        // straight down at x=3 (well clear of every outline edge).
        let stroke = vec![
            p(3., 0.2),
            p(5., 0.),
            p(8., 0.),
            p(10., 0.), // bottom
            p(10., 3.),
            p(10., 6.), // right wall
            p(8.5, 8.),
            p(6., 9.5),
            p(3.2, 9.9), // along the curve facets
            p(3., 8.),
            p(3., 4.),
            p(3., 0.5), // interior wall at x=3
        ];
        let room = outline
            .carve_subarea(&stroke, 0.1, &SubareaTuning::default())
            .unwrap();
        room.validate_simple().unwrap();
        // The 3 curve facets survive as 3 distinct non-orthogonal edges (a single
        // diagonal would be 1; a flattened curve, 0).
        assert!(
            room.non_orthogonal_edges().len() >= 3,
            "every curve facet kept: {:?}",
            room.non_orthogonal_edges()
        );
        let has = |x: f32, y: f32| {
            room.polygons()[0]
                .iter()
                .any(|v| (v.x - x).abs() < 0.3 && (v.y - y).abs() < 0.3)
        };
        assert!(
            has(8.5, 8.) && has(6., 9.5),
            "interior facet corners preserved: {:?}",
            room.polygons()[0]
        );
    }

    /// Real captured closed-loop phase-area stroke (rectangle storey, a roughly
    /// vertical right wall with hand-wander) that STILL staircased after the
    /// coarse-simplify fix — used to trace why and lock the fix.
    #[test]
    fn captured_closed_loop_right_wall_does_not_staircase() {
        // Available region = the storey rectangle.
        let outline = FloorPlan::from_clean_ring(&[
            p(0.20660417, 0.3034812),
            p(0.20660417, 1.3034812),
            p(1.2066041, 1.3034812),
            p(1.2066041, 0.3034812),
        ])
        .unwrap();
        let stroke = vec![
            p(0.20660417, 0.6848957),
            p(0.20660417, 0.49920377),
            p(0.20739083, 0.49625444),
            p(0.21089205, 0.48136163),
            p(0.2126446, 0.46646878),
            p(0.21439716, 0.44106454),
            p(0.21527344, 0.427048),
            p(0.21614972, 0.41653267),
            p(0.217026, 0.40251613),
            p(0.21877855, 0.39288098),
            p(0.2205311, 0.3832458),
            p(0.22140738, 0.37360674),
            p(0.22228366, 0.36835298),
            p(0.22403622, 0.35783765),
            p(0.22490859, 0.35083133),
            p(0.22490859, 0.3315649),
            p(0.22403622, 0.32893607),
            p(0.22403622, 0.3271835),
            p(0.22315994, 0.32631114),
            p(0.2223148, 0.325466),
            p(0.2223148, 0.3246871),
            p(0.22146969, 0.3238459),
            p(0.22146969, 0.32299298),
            p(0.22059731, 0.3212443),
            p(0.21796848, 0.31598663),
            p(0.21621592, 0.31423408),
            p(0.21533965, 0.31248152),
            p(0.21358709, 0.3098566),
            p(0.21095826, 0.30635148),
            p(0.2092057, 0.30459893),
            p(0.20836864, 0.3034812),
            p(0.64503497, 0.3034812),
            p(0.6417674, 0.32716405),
            p(0.6391386, 0.34030432),
            p(0.6391386, 0.37008995),
            p(0.64089113, 0.38060528),
            p(0.6426437, 0.39111674),
            p(0.64965, 0.41126722),
            p(0.6540314, 0.4235312),
            p(0.6566602, 0.43404263),
            p(0.6575365, 0.4428015),
            p(0.6575365, 0.45594567),
            p(0.65841275, 0.46558082),
            p(0.65928906, 0.47609228),
            p(0.6601653, 0.48134995),
            p(0.661914, 0.4874839),
            p(0.66366655, 0.49799532),
            p(0.6654191, 0.50412536),
            p(0.66717166, 0.51025933),
            p(0.6689242, 0.51639324),
            p(0.6724293, 0.52515215),
            p(0.6750582, 0.53216237),
            p(0.6803119, 0.5444263),
            p(0.683817, 0.5531852),
            p(0.6864459, 0.561948),
            p(0.6890747, 0.5689543),
            p(0.689951, 0.57158315),
            p(0.689951, 0.5733357),
            p(0.69082725, 0.57508826),
            p(0.69082725, 0.5785934),
            p(0.69170356, 0.5812183),
            p(0.69170356, 0.5847234),
            p(0.69082725, 0.59173363),
            p(0.689951, 0.59874),
            p(0.6890747, 0.61450905),
            p(0.68819845, 0.6337833),
            p(0.68732214, 0.6477998),
            p(0.68732214, 0.7038699),
            p(0.68819845, 0.7117525),
            p(0.6890747, 0.71701014),
            p(0.6890747, 0.7538021),
            p(0.689951, 0.7590598),
            p(0.69082725, 0.76431745),
            p(0.69082725, 0.7713238),
            p(0.69257593, 0.7774577),
            p(0.6943285, 0.78358775),
            p(0.69608104, 0.7897217),
            p(0.6969573, 0.79585564),
            p(0.6969573, 0.802862),
            p(0.6978336, 0.80811965),
            p(0.69958615, 0.81512594),
            p(0.69958615, 0.81950736),
            p(0.7004624, 0.8238887),
            p(0.70221496, 0.8317713),
            p(0.70309126, 0.8379053),
            p(0.7039675, 0.843159),
            p(0.7039675, 0.8563032),
            p(0.70572007, 0.86243325),
            p(0.708345, 0.87207234),
            p(0.7118501, 0.8878414),
            p(0.71360266, 0.895724),
            p(0.7153552, 0.90098166),
            p(0.7162315, 0.9071117),
            p(0.7171078, 0.9106168),
            p(0.7171078, 0.9211283),
            p(0.7188603, 0.92989105),
            p(0.7206129, 0.94741267),
            p(0.7214892, 0.9596767),
            p(0.72236544, 0.9719407),
            p(0.7241141, 0.9920912),
            p(0.72499037, 1.0026026),
            p(0.7258667, 1.013114),
            p(0.7258667, 1.1077286),
            p(0.7267429, 1.1173676),
            p(0.72761923, 1.1261265),
            p(0.7284955, 1.1348894),
            p(0.7284955, 1.1506585),
            p(0.7293718, 1.155036),
            p(0.7293718, 1.1602936),
            p(0.73024803, 1.1681762),
            p(0.73112434, 1.1751864),
            p(0.73112434, 1.1892029),
            p(0.7320006, 1.1953369),
            p(0.7320006, 1.2303764),
            p(0.73112434, 1.2382628),
            p(0.73024803, 1.2435205),
            p(0.7293718, 1.247898),
            p(0.72761923, 1.2531556),
            p(0.7258667, 1.2645433),
            p(0.7241141, 1.2715535),
            p(0.72236544, 1.2803124),
            p(0.7214892, 1.2838175),
            p(0.7214892, 1.2855701),
            p(0.7206129, 1.288199),
            p(0.7206129, 1.3034812),
            p(0.20660417, 1.3034812),
            p(0.20660417, 1.286601),
            p(0.2075544, 1.2818508),
            p(0.21018323, 1.2617003),
            p(0.2119319, 1.2503126),
            p(0.21368445, 1.236296),
            p(0.21543701, 1.2170218),
            p(0.21631329, 1.2065104),
            p(0.21631329, 1.1942425),
            p(0.21718957, 1.180226),
            p(0.21718957, 1.1627043),
            p(0.21806584, 1.1548218),
            p(0.21806584, 1.1241598),
            p(0.21718957, 1.1197784),
            p(0.21631329, 1.1127721),
            p(0.21456073, 1.1005042),
            p(0.21368445, 1.0926216),
            p(0.21280818, 1.087364),
            p(0.2119319, 1.0821102),
            p(0.21105562, 1.0794814),
            p(0.21018323, 1.0751),
            p(0.20930696, 1.0645885),
            p(0.20930696, 1.0505719),
            p(0.20843068, 1.0426855),
            p(0.2075544, 1.0356791),
            p(0.20667812, 1.0269164),
            p(0.20660417, 1.02614),
            p(0.20660417, 0.9554978),
            p(0.2109271, 0.9457692),
            p(0.21355593, 0.9291238),
            p(0.2144361, 0.90980285),
            p(0.2144361, 0.88610834),
            p(0.21531238, 0.8685906),
            p(0.21619256, 0.8509248),
            p(0.21794511, 0.8272692),
            p(0.21794511, 0.7878484),
            p(0.21706884, 0.7764607),
            p(0.21532017, 0.7659454),
            p(0.21444389, 0.75455767),
            p(0.21356761, 0.7449225),
            p(0.21181506, 0.73178226),
            p(0.2100625, 0.7151369),
            p(0.20918623, 0.70812666),
            p(0.20830995, 0.70199656),
            p(0.20743367, 0.69235754),
            p(0.20660417, 0.6848957),
        ];
        let room = outline
            .carve_subarea(&stroke, 0.023, &SubareaTuning::default())
            .unwrap();
        let n = room.polygons()[0].len();
        // A clean shape (left wall + bottom + one right wall + top) is a handful of
        // vertices; the staircase is ~24.
        assert!(
            n <= 8,
            "right wall should be ONE wall, not a staircase — got {n} verts: {:?}",
            room.polygons()[0]
        );
    }

    /// Captured stroke whose interior wall is SPARSE in the clipped stroke (it
    /// leaps x=2.79→3.24 with no point between). Without densifying, the
    /// classifier never sees the interior midpoint, keeps the run on-boundary
    /// across NON-ADJACENT edges (0→19), and `outline_subpath` bridges them with a
    /// spurious diagonal — the result was a degenerate hexagon. With densify the
    /// run splits (wall → interior) and the carve follows the left edge DOWN to
    /// the wall and across it.
    #[test]
    fn sparse_interior_wall_is_not_misread_as_a_boundary_diagonal() {
        let outline = FloorPlan::from_clean_ring(&[
            p(2.7498643, 3.0717998),
            p(2.7498643, 0.57119596),
            p(2.9498644, 0.57119596),
            p(3.001358, 0.5829727),
            p(3.0516596, 0.5990976),
            p(3.100403, 0.6194531),
            p(3.147233, 0.643891),
            p(3.1918087, 0.67223334),
            p(3.2338052, 0.7042736),
            p(3.2729163, 0.7397783),
            p(3.3088572, 0.7784889),
            p(3.3413663, 0.8201233),
            p(3.3702068, 0.86437833),
            p(3.3951683, 0.9109312),
            p(3.4160693, 0.95944345),
            p(3.432757, 1.0095611),
            p(3.4451103, 1.0609193),
            p(3.455425, 1.1131439),
            p(3.455425, 1.2186667),
            p(3.4498644, 1.2711961),
            p(3.4498644, 2.571196),
            p(4.8498645, 2.571196),
            p(5.4498644, 2.871196),
            p(5.4498644, 3.571196),
            p(4.149864, 3.571196),
            p(4.149864, 3.0717998),
            p(3.4498644, 3.0717998),
        ])
        .unwrap();
        let stroke = vec![
            p(2.7498643, 3.0717998),
            p(2.7498643, 2.5600302),
            p(2.7844014, 2.5600302),
            p(2.7870536, 2.5626826),
            p(2.7897058, 2.565323),
            p(3.2355206, 2.565323),
            p(3.250476, 2.559341),
            p(3.3299718, 2.559341),
            p(3.338945, 2.55635),
            p(3.3449268, 2.553359),
            p(3.380819, 2.553359),
            p(3.3927834, 2.5473769),
            p(3.4431868, 2.5473769),
            p(3.445839, 2.5447247),
            p(3.448386, 2.5421894),
            p(3.4498644, 2.5421894),
            p(3.4498644, 2.571196),
            p(4.8498645, 2.571196),
            p(5.4498644, 2.871196),
            p(5.4498644, 3.571196),
            p(4.149864, 3.571196),
            p(4.149864, 3.0717998),
            p(2.7498643, 3.0717998),
        ];
        let room = outline
            .carve_subarea(&stroke, 0.058, &SubareaTuning::default())
            .unwrap();
        room.validate_simple().unwrap();
        let poly = room.polygons()[0].clone();
        // The carve follows the left wall DOWN to the interior wall (≈2.75, 2.565)
        // and across it — NOT a corner-to-corner diagonal from (2.75, 3.07).
        assert!(
            poly.iter()
                .any(|v| (v.x - 2.75).abs() < 0.05 && (v.y - 2.565).abs() < 0.05),
            "left edge followed down to the interior wall: {poly:?}"
        );
        // NOTHING is non-orthogonal except where it follows a verbatim house wall
        // (here, the storey's own diagonal corner near x∈[4.85, 5.45]).
        let spurious = room
            .non_orthogonal_edges()
            .into_iter()
            .find(|(_, _, a, b)| {
                let on_storey_diag = |p: &Point2f32| p.x > 4.8 && p.x < 5.5;
                !(on_storey_diag(a) || on_storey_diag(b))
            });
        assert!(
            spurious.is_none(),
            "every non-axis edge is on the storey diagonal: {spurious:?}"
        );
    }

    /// Captured LAST-phase stroke whose interior wall runs between two CLOSE
    /// boundary edges (gap < off_tol), so densify can't split it — the run hops
    /// non-adjacent edges and produced a `(3.25,2.671)→(3.45,2.571)` diagonal. The
    /// orthogonalize-off-boundary pass squares it into "down, then right along the
    /// boundary". Everything must be orthogonal except the storey's own diagonal.
    #[test]
    fn last_phase_close_walls_squares_the_diagonal() {
        let outline = FloorPlan::from_clean_ring(&[
            p(2.7498643, 2.671196),
            p(2.7498643, 0.57119596),
            p(2.9498644, 0.57119596),
            p(3.001358, 0.5829727),
            p(3.0516596, 0.5990976),
            p(3.100403, 0.6194531),
            p(3.147233, 0.643891),
            p(3.1918087, 0.67223334),
            p(3.2338052, 0.7042736),
            p(3.2729163, 0.7397783),
            p(3.3088572, 0.7784889),
            p(3.3413663, 0.8201233),
            p(3.3702068, 0.86437833),
            p(3.3951683, 0.9109312),
            p(3.4160693, 0.95944345),
            p(3.432757, 1.0095611),
            p(3.4451103, 1.0609193),
            p(3.455425, 1.1131439),
            p(3.455425, 1.2186667),
            p(3.4498644, 1.2711961),
            p(3.4498644, 2.571196),
            p(4.8498645, 2.571196),
            p(5.4498644, 2.871196),
            p(5.4498644, 3.571196),
            p(4.149864, 3.571196),
            p(4.149864, 3.071196),
            p(3.4498644, 3.071196),
            p(3.4498644, 3.571196),
            p(3.2498643, 3.571196),
            p(3.2498643, 2.671196),
        ])
        .unwrap();
        let stroke = vec![
            p(3.2101789, 2.671196),
            p(3.2101789, 2.662321),
            p(3.2161608, 2.653348),
            p(3.2161608, 2.6355538),
            p(3.230263, 2.6355538),
            p(3.239236, 2.6415358),
            p(3.242227, 2.6505089),
            p(3.2452183, 2.6534998),
            p(3.2478938, 2.6534998),
            p(3.2624984, 2.659482),
            p(3.6176946, 2.659482),
            p(3.655281, 2.6669946),
            p(3.6791158, 2.6737478),
            p(3.7007773, 2.6799169),
            p(3.7368448, 2.6870556),
            p(3.8286088, 2.6972437),
            p(3.9333413, 2.7082615),
            p(4.0183287, 2.713227),
            p(4.127583, 2.7349236),
            p(4.258826, 2.7587817),
            p(4.3691435, 2.7703369),
            p(4.452191, 2.785035),
            p(4.4612694, 2.7910755),
            p(4.5935173, 2.7910755),
            p(4.6897907, 2.7817168),
            p(4.766272, 2.7817168),
            p(4.768971, 2.779018),
            p(4.771682, 2.7763073),
            p(4.808193, 2.7700565),
            p(5.2475853, 2.7700565),
            p(5.4498644, 2.871196),
            p(5.4498644, 3.571196),
            p(4.149864, 3.571196),
            p(4.149864, 3.071196),
            p(3.4498644, 3.071196),
            p(3.4498644, 3.571196),
            p(3.257895, 3.571196),
            p(3.257895, 3.5598278),
            p(3.2519128, 3.5598278),
            p(3.2498643, 3.5577793),
            p(3.2498643, 2.671196),
            p(3.2101789, 2.671196),
        ];
        let room = outline
            .carve_subarea(&stroke, 0.049, &SubareaTuning::default())
            .unwrap();
        room.validate_simple().unwrap();
        let spurious = room
            .non_orthogonal_edges()
            .into_iter()
            .find(|(_, _, a, b)| {
                let on_storey_diag = |p: &Point2f32| p.x > 4.8 && p.x < 5.5;
                !(on_storey_diag(a) || on_storey_diag(b))
            });
        assert!(
            spurious.is_none(),
            "no spurious diagonal — squared to an orthogonal L: {spurious:?}"
        );
    }

    /// Real captured stroke on a LARGE notched region: the trace turns down the
    /// `x=2.45` boundary to the bottom (`y≈0.57`) and back up an interior wall
    /// ~0.42 m off that boundary. With an uncapped `off_tol` (≈0.44 m on this 5 m
    /// region) the interior wall was misclassified as still-hugging, so the whole
    /// bottom collapsed into one boundary run and got dropped. The `off_tol`
    /// ceiling fixes it — the carve must follow down to the bottom.
    #[test]
    fn captured_large_region_follows_boundary_down_not_dropping_the_bottom() {
        let outline = FloorPlan::from_clean_ring(&[
            p(2.9498644, 0.57119596),
            p(2.4498644, 0.57119596),
            p(2.4498644, 1.571196),
            p(1.5498644, 1.571196),
            p(1.5498644, 2.571196),
            p(1.5524862, 2.653201),
            p(1.5618532, 2.7347114),
            p(1.5779015, 2.8151736),
            p(1.6005222, 2.8940406),
            p(1.6295614, 2.9707766),
            p(1.6648219, 3.04486),
            p(1.7060641, 3.1157875),
            p(1.753008, 3.1830778),
            p(1.8053343, 3.246273),
            p(1.8626876, 3.304944),
            p(1.9246782, 3.3586922),
            p(1.9908849, 3.4071522),
            p(2.0608578, 3.4499946),
            p(2.1341214, 3.4869287),
            p(2.210178, 3.5177028),
            p(2.288511, 3.542109),
            p(2.3685877, 3.5599804),
            p(2.4498644, 3.571196),
            p(3.4498644, 3.571196),
            p(3.4498644, 3.071196),
            p(4.149864, 3.071196),
            p(4.149864, 3.571196),
            p(5.4498644, 3.571196),
            p(5.4498644, 2.871196),
            p(4.8498645, 2.571196),
            p(3.4498644, 2.571196),
            p(3.4498644, 1.2711961),
            p(3.455425, 1.2186667),
            p(3.455425, 1.1658543),
            p(3.455425, 1.1131439),
            p(3.4451103, 1.0609193),
            p(3.432757, 1.0095611),
            p(3.4160693, 0.95944345),
            p(3.3951683, 0.9109312),
            p(3.3702068, 0.86437833),
            p(3.3413663, 0.8201233),
            p(3.3088572, 0.7784889),
            p(3.2729163, 0.7397783),
            p(3.2338052, 0.7042736),
            p(3.1918087, 0.67223334),
            p(3.147233, 0.643891),
            p(3.100403, 0.6194531),
            p(3.0516596, 0.5990976),
            p(3.001358, 0.5829727),
        ])
        .unwrap();
        let stroke = vec![
            p(1.5498644, 1.7808472),
            p(1.5498644, 1.633819),
            p(1.550369, 1.6333144),
            p(1.550369, 1.6043855),
            p(1.5563511, 1.5924214),
            p(1.5563511, 1.5780505),
            p(1.5623331, 1.5720683),
            p(1.5627693, 1.571196),
            p(1.6597829, 1.571196),
            p(1.6670659, 1.5726526),
            p(1.685421, 1.5756435),
            p(1.7256362, 1.5823617),
            p(2.1899698, 1.5823617),
            p(2.2134774, 1.5722203),
            p(2.2180953, 1.571196),
            p(2.4498644, 1.571196),
            p(2.4498644, 0.87536216),
            p(2.450376, 0.87382716),
            p(2.450376, 0.7153027),
            p(2.4498644, 0.71453524),
            p(2.4498644, 0.57119596),
            p(2.8067522, 0.57119596),
            p(2.8067522, 0.57860345),
            p(2.8127344, 0.57860345),
            p(2.8127344, 0.58372086),
            p(2.8157253, 0.58372086),
            p(2.8187165, 0.58970296),
            p(2.8187165, 0.6443593),
            p(2.8246984, 0.6563234),
            p(2.8246984, 0.67127854),
            p(2.829886, 0.67127854),
            p(2.829886, 0.69221574),
            p(2.8332393, 0.7151041),
            p(2.8405883, 0.7482624),
            p(2.84414, 0.792567),
            p(2.84414, 1.5452543),
            p(2.8501222, 1.5572184),
            p(2.8501222, 1.5721736),
            p(2.8561041, 1.5841377),
            p(2.8561041, 1.6220746),
            p(2.8622382, 1.6404881),
            p(2.8654277, 1.6666245),
            p(2.868419, 1.6845707),
            p(2.868419, 1.7682494),
            p(2.87141, 1.7802135),
            p(2.8744009, 1.7861955),
            p(2.8744009, 2.0234797),
            p(2.867484, 2.0511117),
            p(2.867484, 2.0992837),
            p(2.8734663, 2.1112478),
            p(2.8734663, 2.1561131),
            p(2.867484, 2.1620953),
            p(2.867484, 2.2817364),
            p(2.8615022, 2.2966914),
            p(2.8615022, 2.433706),
            p(2.867484, 2.4426792),
            p(2.867484, 2.4614198),
            p(2.8734663, 2.476375),
            p(2.8734663, 2.5122674),
            p(2.8794482, 2.5242314),
            p(2.8854303, 2.5332046),
            p(2.8854303, 2.760137),
            p(2.8824394, 2.7691102),
            p(2.8794482, 2.7750921),
            p(2.8794482, 2.8470404),
            p(2.8725314, 2.8747191),
            p(2.8693535, 2.9051435),
            p(2.8661873, 2.9271789),
            p(2.8661873, 2.951107),
            p(2.8602052, 2.9600801),
            p(2.8602052, 3.1311762),
            p(2.8572142, 3.161624),
            p(2.8542233, 3.173588),
            p(2.8542233, 3.1885433),
            p(2.851232, 3.1975164),
            p(2.848241, 3.2034984),
            p(2.848241, 3.3530498),
            p(2.8422592, 3.370996),
            p(2.8422592, 3.4547446),
            p(2.848241, 3.4667087),
            p(2.848241, 3.571196),
            p(2.4498644, 3.571196),
            p(2.3685877, 3.5599804),
            p(2.288511, 3.542109),
            p(2.210178, 3.5177028),
            p(2.1341214, 3.4869287),
            p(2.0608578, 3.4499946),
            p(1.9908849, 3.4071522),
            p(1.9246782, 3.3586922),
            p(1.8626876, 3.304944),
            p(1.8053343, 3.246273),
            p(1.753008, 3.1830778),
            p(1.7060641, 3.1157875),
            p(1.6648219, 3.04486),
            p(1.6295614, 2.9707766),
            p(1.6005222, 2.8940406),
            p(1.5779015, 2.8151736),
            p(1.5618532, 2.7347114),
            p(1.5524862, 2.653201),
            p(1.5498644, 2.571196),
            p(1.5498644, 1.9148959),
            p(1.550369, 1.9138867),
            p(1.550369, 1.8989315),
            p(1.5563511, 1.8869674),
            p(1.5563511, 1.8067939),
            p(1.550369, 1.7948298),
            p(1.550369, 1.7828656),
            p(1.5498644, 1.7808472),
        ];
        let room = outline
            .carve_subarea(&stroke, 0.066, &SubareaTuning::default())
            .unwrap();
        room.validate_simple().unwrap();
        // The phase follows x=2.45 down to the bottom (~y=0.57); the bug stopped at
        // y=1.571 (top strip only).
        assert!(
            room.polygons()[0].iter().any(|v| v.y < 0.7),
            "must follow the boundary down to the bottom, not drop it: {:?}",
            room.polygons()[0]
        );
    }
}

#[cfg(test)]
mod guardrail_tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    #[test]
    fn clamp_inside_pulls_outside_points_to_the_boundary() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0.2, 0.2)],
            0.5,
            true,
        )
        .unwrap();
        let clamped = outline.clamp_inside(&[p(5., 5.), p(15., 5.), p(5., -3.)]);
        assert_eq!(clamped[0], p(5., 5.), "inside point unchanged");
        assert!(
            (clamped[1].x - 10.0).abs() < 0.05 && (clamped[1].y - 5.0).abs() < 0.05,
            "right overshoot snaps to (10,5): {:?}",
            clamped[1]
        );
        assert!(
            clamped[2].y.abs() < 0.05 && (clamped[2].x - 5.0).abs() < 0.05,
            "bottom overshoot snaps to (5,0): {:?}",
            clamped[2]
        );
    }

    #[test]
    fn any_point_outside_flags_a_room_poking_out() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0.2, 0.2)],
            0.5,
            true,
        )
        .unwrap();
        let inside =
            FloorPlan::from_clean_ring(&[p(2., 2.), p(6., 2.), p(6., 6.), p(2., 6.)]).unwrap();
        assert!(
            !outline.any_point_outside(&inside, 0.05),
            "fully-inside room is fine"
        );
        let poking =
            FloorPlan::from_clean_ring(&[p(8., 2.), p(13., 2.), p(13., 6.), p(8., 6.)]).unwrap();
        assert!(
            outline.any_point_outside(&poking, 0.05),
            "room past x=10 is flagged"
        );
    }

    /// A self-crossing sub-area ring (the reported fold: a boundary diagonal
    /// whose corner falls left of the room's own left wall) is un-intersected
    /// into a simple plan rather than emitted broken.
    #[test]
    fn self_crossing_subarea_ring_is_untangled_into_a_simple_plan() {
        let crossed = [
            p(2.750228, 1.0471573),
            p(3.650228, 2.0471573),
            p(3.650228, 3.0471573),
            p(2.7811332, 3.0471573),
            p(2.7811332, 1.0471573),
        ];
        // As drawn, the ring self-intersects.
        assert!(
            FloorPlan::from_clean_ring(&crossed)
                .unwrap()
                .validate_simple()
                .is_err(),
            "the raw ring crosses itself"
        );
        // After un-intersecting, the real simplicity checker is satisfied.
        let fixed = geom::untangle_ring(&crossed);
        let plan = FloorPlan::from_clean_ring(&fixed).unwrap();
        assert!(
            plan.validate_simple().is_ok(),
            "untangled ring is simple: {:?}",
            plan.validate_simple()
        );
    }
}
