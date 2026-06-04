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
    pub fn carve_subarea(&self, path: &[Point2f32], tolerance: f32) -> Result<Self, Error> {
        let ring = self.parametric.ring_points(0);
        let diag = self.bounding_box_diagonal();

        // Did the user draw a CLOSED loop (all the walls) or an OPEN path (just
        // the new interior walls, ends meant to meet the house)? A closed loop
        // has its first and last points nearly coincident relative to its own
        // extent. Carve (snap endpoints + fill the shared outline arc) is only
        // for the OPEN case; a closed loop becomes a full ring whose walls are
        // then snapped onto any house wall they run along.
        let drawn_closed = {
            let dd = geom::bbox_diagonal(path);
            let ends_gap = path
                .first()
                .zip(path.last())
                .map(|(f, l)| ((f.x - l.x).powi(2) + (f.y - l.y).powi(2)).sqrt())
                .unwrap_or(0.0);
            ends_gap < dd * 0.15
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
            // new wall is clean (the usual vertical/horizontal cut).
            // `simplify_stroke` rejects a <3-point result, so a straight cut
            // surfaces as `InsufficientPoints`; treat that as the cut endpoints.
            let simplified = match simplification::simplify_stroke(path, tolerance) {
                Ok(s) => s,
                Err(Error::InsufficientPoints) => vec![path[0], path[path.len() - 1]],
                Err(e) => return Err(e),
            };
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
                // A closed loop hugging a DIAGONAL boundary: keep the diagonal
                // verbatim, orthogonalize only the interior (snap-before-ortho).
                // The diagonal is already placed, so skip the wall-snap pass.
                if let Some(p) = self.boundary_aware_loop_points(path, tolerance) {
                    // Axis-snap the interior walls onto the storey (so e.g. a
                    // far wall lands on the storey edge), but keep the diagonal
                    // snap OFF so the verbatim diagonal isn't disturbed.
                    return self.build_subarea_region(&p, true, false);
                }
                // Free-standing interior room: close the drawn path into its own
                // loop, orthogonalized to the storey's axes (0°) — not the
                // stroke's dominant angle — so interior walls stay axis-aligned.
                Self::from_points_oriented(path, tolerance, true, Some(0.0))?.get_points()
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

    /// If a CLOSED-loop sub-area stroke HUGS a non-axis-aligned outline edge (a
    /// diagonal) for a run of points, build the ring with that diagonal kept
    /// VERBATIM and only the interior orthogonalized — i.e. snap to the boundary
    /// *before* orthogonalizing, and lock it, so the diagonal is followed fully
    /// (orthogonalizing it first would flatten it irreversibly).
    ///
    /// Returns `None` when no such run exists, so the caller orthogonalizes
    /// normally — axis-only outlines and non-hugging strokes are untouched.
    fn boundary_aware_loop_points(
        &self,
        path: &[Point2f32],
        tolerance: f32,
    ) -> Option<Vec<Point2f32>> {
        let ring = self.parametric.ring_points(0);
        let no = ring.len();
        if no < 3 {
            return None;
        }
        let eps = 1e-3_f32;
        // Diagonal (non-axis-aligned) outline edges.
        let diags: Vec<(usize, Point2f32, Point2f32)> = (0..no)
            .map(|i| (i, ring[i], ring[(i + 1) % no]))
            .filter(|(_, a, b)| (a.x - b.x).abs() >= eps && (a.y - b.y).abs() >= eps)
            .collect();
        if diags.is_empty() {
            return None;
        }
        let mut simp = simplification::simplify_stroke(path, tolerance).ok()?;
        if simp.len() > 3 {
            let (f, l) = (simp[0], *simp.last().unwrap());
            if ((f.x - l.x).powi(2) + (f.y - l.y).powi(2)).sqrt() < tolerance {
                simp.pop();
            }
        }
        let m = simp.len();
        if m < 4 {
            return None;
        }
        let btol = (geom::bbox_diagonal(&simp) * 0.1).max(1e-3);

        // For each simplified point, the nearest diagonal it lies alongside.
        let near: Vec<Option<usize>> = simp
            .iter()
            .map(|&p| {
                let mut best: Option<(f32, usize)> = None;
                for (ei, a, b) in &diags {
                    let (_q, t, d) = geom::project_point_to_segment(p, *a, *b);
                    if (-0.05..=1.05).contains(&t)
                        && d <= btol
                        && best.map_or(true, |(bd, _)| d < bd)
                    {
                        best = Some((d, *ei));
                    }
                }
                best.map(|(_, ei)| ei)
            })
            .collect();

        // Longest cyclic run of points on the SAME diagonal edge.
        let (mut best_start, mut best_len, mut best_edge) = (0usize, 0usize, 0usize);
        for start in 0..m {
            let Some(e) = near[start] else { continue };
            let mut len = 0;
            while len < m && near[(start + len) % m] == Some(e) {
                len += 1;
            }
            if len > best_len {
                best_len = len;
                best_start = start;
                best_edge = e;
            }
        }
        if best_len < 2 || best_len >= m - 1 {
            return None; // no diagonal run, or (nearly) the whole loop
        }

        // Rotate so the run starts at index 0; the diagonal endpoints A, B are
        // its first/last points projected onto the diagonal. The rest is interior.
        let rot: Vec<Point2f32> = (0..m).map(|i| simp[(best_start + i) % m]).collect();
        // `best_edge` is an OUTLINE edge index (stored in `near`); read it from
        // the ring directly.
        let (da, db) = (ring[best_edge], ring[(best_edge + 1) % no]);
        // Project the run's first/last points onto the diagonal; if a projection
        // lands near a diagonal ENDPOINT (the corner where the run reached it),
        // snap it exactly there so the follow covers the full diagonal — the
        // interior walls then meet the diagonal at its corner, not mid-way.
        let snap_to_end = |q: Point2f32| -> Point2f32 {
            let dda = ((q.x - da.x).powi(2) + (q.y - da.y).powi(2)).sqrt();
            let ddb = ((q.x - db.x).powi(2) + (q.y - db.y).powi(2)).sqrt();
            if dda <= btol && dda <= ddb {
                da
            } else if ddb <= btol {
                db
            } else {
                q
            }
        };
        let a_proj = snap_to_end(geom::project_point_to_segment(rot[0], da, db).0);
        let b_proj = snap_to_end(geom::project_point_to_segment(rot[best_len - 1], da, db).0);
        if ((a_proj.x - b_proj.x).powi(2) + (a_proj.y - b_proj.y).powi(2)).sqrt() < 1e-3 {
            return None; // run collapsed to a point
        }

        // Interior path B → (drawn interior) → A, orthogonalized with A, B fixed.
        let mut interior_in = vec![b_proj];
        interior_in.extend_from_slice(&rot[best_len..m]);
        interior_in.push(a_proj);
        let ortho = snap_open_fixed_ends(&interior_in, tolerance);
        if ortho.len() < 2 {
            return None;
        }
        // Ring: A → B (the diagonal, verbatim) → orthogonalized interior → A.
        let mut out = vec![a_proj, b_proj];
        out.extend_from_slice(&ortho[1..ortho.len() - 1]);
        if out.len() < 3 {
            return None;
        }
        Some(out)
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
        let cut_tol = (span * 0.1).max(tolerance);
        let simplified = match simplification::simplify_stroke(path, cut_tol) {
            Ok(s) => s,
            Err(Error::InsufficientPoints) => vec![path[0], path[path.len() - 1]],
            Err(e) => return Err(e),
        };
        let mut opath = if simplified.len() >= 3 {
            orthogonal::make_orthogonal_at(&simplified, cut_tol, 0.0)?
        } else {
            let (s, e) = (simplified[0], simplified[simplified.len() - 1]);
            if (s.x - e.x).abs() <= (s.y - e.y).abs() {
                let x = (s.x + e.x) * 0.5; // near-vertical → vertical cut
                vec![Point2f32::new(x, s.y), Point2f32::new(x, e.y)]
            } else {
                let y = (s.y + e.y) * 0.5; // near-horizontal → horizontal cut
                vec![Point2f32::new(s.x, y), Point2f32::new(e.x, y)]
            }
        };
        if opath.len() < 2 {
            return Ok(None);
        }

        // Both endpoints must reach the outline at two DISTINCT points; snap them
        // exactly onto it so the cut divides the boundary cleanly.
        let vertex_tol = (diag * 0.04).max(1e-4);
        let reach_tol = (diag * 0.06).max(1e-3);
        let last = opath.len() - 1;
        let (e1, t1, d1, _) = extend::snap_to_polygon(&ring, opath[0], vertex_tol);
        let (e2, t2, d2, _) = extend::snap_to_polygon(&ring, opath[last], vertex_tol);
        if d1 > reach_tol || d2 > reach_tol || (e1 == e2 && (t1 - t2).abs() < 1e-3) {
            return Ok(None);
        }
        extend::pin_to_edge(&mut opath[0], &ring, e1);
        extend::pin_to_edge(&mut opath[last], &ring, e2);

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
            // No axis wall-snapping: the cut endpoints are pinned to the boundary
            // and the kept arc is the outline verbatim (curves included).
            self.build_subarea_region(&pts, false, false)
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

/// Orthogonalize an OPEN path to the world axes while keeping the first and
/// last points FIXED (they're locked to a boundary). Interior points snap to
/// horizontal/vertical relative to the running corner; a corner is inserted if
/// needed so the final segment reaches the fixed last point axis-aligned. Used
/// to orthogonalize a sub-area's interior between two points pinned on a
/// diagonal boundary, without disturbing that boundary.
fn snap_open_fixed_ends(pts: &[Point2f32], tol: f32) -> Vec<Point2f32> {
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
    // Connect the running corner to the fixed last point. If both deltas are
    // significant, insert a corner; if it's only slightly off one axis, pull
    // the last interior corner exactly onto that axis so the closing wall is
    // truly horizontal/vertical (the last point is fixed on the boundary).
    let (dx, dy) = ((last.x - cur.x).abs(), (last.y - cur.y).abs());
    if dx > tol && dy > tol {
        out.push(Point2f32::new(last.x, cur.y));
    } else if dy <= tol && dy > f32::EPSILON {
        if let Some(c) = out.last_mut() {
            c.y = last.y; // make the closing segment exactly horizontal
        }
    } else if dx <= tol && dx > f32::EPSILON {
        if let Some(c) = out.last_mut() {
            c.x = last.x; // exactly vertical
        }
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
            .split_by_cut(&cut, 0.05)
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
        let room = outline.carve_subarea(&path, 0.5).unwrap();
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
        let room = outline.carve_subarea(&path, 0.3).unwrap();
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
        let room = big_house.carve_subarea(&path, tol).unwrap();
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
        let room = outline.carve_subarea(&path, 0.1).unwrap();
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
        let room = outline.carve_subarea(&stroke, 0.1).unwrap();
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
        let (a, b) = outline.split_by_cut(&stroke, 0.1).unwrap().unwrap();
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
        assert!(outline.split_by_cut(&bent, 0.1).unwrap().is_some());
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
            .split_by_cut(&stroke, 0.1)
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
            .split_by_cut(&stepped, 0.1)
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
        let (ra, rb) = outline.split_by_cut(&stroke, 0.05).unwrap().unwrap();
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
        let res = outline.split_by_cut(&stroke, 0.02).unwrap();
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
        let (ra, rb) = outline.split_by_cut(&stroke, 0.02).unwrap().unwrap();
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
            p(8.6, 4.0),
            p(8.6, 7.1), // top-right corner ~0.2 off the diagonal
            p(4.0, 7.1),
            p(4.1, 4.1),
        ];
        let room = outline.carve_subarea(&stroke, 0.15).unwrap();
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
        let room = outline.carve_subarea(&stroke, 0.02).unwrap();
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
        let room = outline.carve_subarea(&stroke, 0.1).unwrap();
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
        let room = outline.carve_subarea(&stroke, 0.04).unwrap();
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
