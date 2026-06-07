//! Carving a freehand stroke into a sub-area region against an outline ring.
//!
//! Pure geometry, `(outline ring, stroke) → FloorPlan`: classification lives in
//! `boundary_follow`; this module owns the orchestration (`sub_area`,
//! `split_cut`) and the finalize (`build_region`, `snap_walls`), all over
//! `&[Point2f32]`. `FloorPlan::carve_or_cut` is the single public entry — a thin
//! facade that clips the stroke, then routes to `split_cut` or `sub_area`.
use crate::boundary_follow::{self, Run};
use crate::{geom, orthogonal, region, simplification, snap, splice};
use crate::{Error, FloorPlan, SubareaTuning};
use domain::points::Point2f32;
// Shared crate-root test helper, used by the carve test modules via `super::*`.

/// Project `p` onto outline `edge` (clamped to the segment).
fn project_onto_edge(outline: &[Point2f32], p: Point2f32, edge: usize) -> Point2f32 {
    let n = outline.len();
    let a = outline[edge];
    let b = outline[(edge + 1) % n];
    geom::project_point_to_segment(p, a, b).0
}

/// The real outline polyline from `entry` (on the run's first edge) to `exit`
/// (on its last edge), inserting the shared corner vertex between each pair of
/// consecutive edges the run hugged — so a run that rounds a corner keeps the
/// corner. Non-adjacent edge jumps are just connected directly.
fn outline_subpath(
    outline: &[Point2f32],
    edges: &[usize],
    entry: Point2f32,
    exit: Point2f32,
) -> Vec<Point2f32> {
    let n = outline.len();
    let mut out = vec![entry];
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b == (a + 1) % n {
            out.push(outline[b]); // forward corner
        } else if a == (b + 1) % n {
            out.push(outline[a]); // backward corner
        }
    }
    out.push(exit);
    out
}

/// The nearest on-boundary run to run `i`, searching cyclically in `dir`
/// (+1 forward, -1 back). Returns its `(entry, exit, edges)`.
fn neighbor_boundary(
    bverts: &[Option<(Point2f32, Point2f32, Vec<usize>)>],
    i: usize,
    dir: isize,
) -> Option<(Point2f32, Point2f32)> {
    let m = bverts.len() as isize;
    for step in 1..=m {
        let j = (i as isize + dir * step).rem_euclid(m) as usize;
        if let Some((entry, exit, _)) = &bverts[j] {
            return Some((*entry, *exit));
        }
    }
    None
}

/// Append `pts` to `ring`, skipping a point equal (within eps) to the current
/// last — so shared transition vertices aren't duplicated.
fn push_ring(ring: &mut Vec<Point2f32>, pts: &[Point2f32]) {
    for &p in pts {
        if ring.last().map_or(true, |l| {
            (l.x - p.x).abs() > 1e-3 || (l.y - p.y).abs() > 1e-3
        }) {
            ring.push(p);
        }
    }
}

/// PASS 2: build the sub-area ring from a closed loop's runs. On-boundary runs
/// become the real outline sub-path (corners preserved); interior runs are
/// orthogonalized between the bounding transition vertices (which sit on the
/// boundary). The result follows the storey edge exactly where you drew along
/// it, so it needs no clip afterward. (Open paths + the all-interior free-room
/// case are handled when this is wired into `carve_or_cut`.)
pub(crate) fn build_region_ring(
    outline: &[Point2f32],
    stroke: &[Point2f32],
    runs: &[Run],
    tol: f32,
    tuning: &crate::SubareaTuning,
) -> Vec<Point2f32> {
    // Entry/exit (projected onto the boundary) for each on-boundary run.
    let bverts: Vec<Option<(Point2f32, Point2f32, Vec<usize>)>> = runs
        .iter()
        .map(|r| match r {
            Run::OnBoundary { start, end, edges } => {
                // Snap each transition to a nearby outline CORNER, so a hug that
                // runs up to a corner reaches it exactly (not a hand-width short).
                let entry = snap::snap_to_corner(
                    project_onto_edge(outline, stroke[*start], edges[0]),
                    outline,
                    tuning.corner_snap_m,
                );
                let exit = snap::snap_to_corner(
                    project_onto_edge(outline, stroke[*end], *edges.last().unwrap()),
                    outline,
                    tuning.corner_snap_m,
                );
                Some((entry, exit, edges.clone()))
            }
            Run::Interior { .. } => None,
        })
        .collect();

    let mut ring: Vec<Point2f32> = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        match r {
            Run::OnBoundary { .. } => {
                let (entry, exit, edges) = bverts[i].as_ref().unwrap();
                let sub = outline_subpath(outline, edges, *entry, *exit);
                push_ring(&mut ring, &sub);
            }
            Run::Interior { start, end } => {
                // Pin the interior walls to the adjacent boundary runs' vertices
                // (cyclic); fall back to the raw stroke ends if there are none.
                let pin_start = neighbor_boundary(&bverts, i, -1)
                    .map(|(_, exit)| exit)
                    .unwrap_or(stroke[*start]);
                let pin_end = neighbor_boundary(&bverts, i, 1)
                    .map(|(entry, _)| entry)
                    .unwrap_or(stroke[*end]);
                // Coarse open-polyline RDP (allow a 2-point collapse) so a roughly
                // straight wall is a clean line, not a staircase; then orthogonalize
                // with the pinned ends.
                let raw = &stroke[*start..=*end];
                let coarse = tuning.coarse_simplify_tol(geom::bbox_diagonal(raw), tol);
                let simplified = crate::simplification::simplify_open(raw, coarse);
                let mut input = vec![pin_start];
                input.extend_from_slice(&simplified);
                input.push(pin_end);
                let ortho = crate::snap_open_fixed_ends_stepped(&input);
                push_ring(&mut ring, &ortho);
            }
        }
    }
    // Drop a closing duplicate.
    if ring.len() > 1 {
        let (f, l) = (ring[0], *ring.last().unwrap());
        if (f.x - l.x).abs() < 1e-3 && (f.y - l.y).abs() < 1e-3 {
            ring.pop();
        }
    }
    ring
}

#[cfg(test)]
use crate::polygon_area;

/// `FloorPlan`'s public carving API. The carve is geometry over an outline RING,
/// so the real work (`carve_or_cut` / `sub_area` / `split_cut`) takes a
/// `&[Point2f32]`; this hands it `self`'s outline. `carve_or_cut` is the single
/// entry point — it clips, then decides cut vs carve.
impl FloorPlan {
    /// Resolve a freehand sub-area stroke against this outline in one step —
    /// clip, then cut or carve. See [`carve_or_cut`].
    pub fn carve_or_cut(
        &self,
        stroke: &[Point2f32],
        tuning: &SubareaTuning,
    ) -> Result<SubareaOutcome, Error> {
        carve_or_cut(&self.parametric.ring_points(0), stroke, tuning)
    }
}

/// What a freehand sub-area stroke resolved to.
#[derive(Debug)]
pub enum SubareaOutcome {
    /// A straight cut split the outline into two regions — the caller lets the
    /// user pick which becomes the sub-area.
    Cut(FloorPlan, FloorPlan),
    /// A single carved sub-area.
    Area(FloorPlan),
}

/// Resolve a freehand sub-area stroke against `outline`: clip it onto the storey
/// ([`region::clip_stroke_inside`]), then either split it (a straight cut → two regions,
/// [`split_cut`]) or carve it (anything else → one area, [`sub_area`]). The carve
/// tolerance is derived from the clipped stroke + `tuning`, so the caller passes
/// neither a pre-clipped stroke nor a tolerance — the whole clip→tol→cut/carve
/// pipeline lives here, not in the caller.
fn carve_or_cut(
    outline: &[Point2f32],
    stroke: &[Point2f32],
    tuning: &SubareaTuning,
) -> Result<SubareaOutcome, Error> {
    let pts = region::clip_stroke_inside(outline, stroke, tuning.close_gap_m);
    let tol = (geom::bbox_diagonal(&pts) * tuning.rdp_tol_ratio).max(tuning.rdp_tol_floor);
    match split_cut(outline, &pts, tol, tuning)? {
        Some((a, b)) => Ok(SubareaOutcome::Cut(a, b)),
        None => Ok(SubareaOutcome::Area(sub_area(outline, &pts, tol, tuning)?)),
    }
}

/// Carve a sub-area (a living space) out of an OUTLINE RING from an OPEN drawn
/// path. The path supplies the sub-area's new interior walls; its two endpoints
/// snap onto the outline and the area is closed along the SHORTER outline arc
/// between them — the walls it SHARES with the storey. Returns an INDEPENDENT
/// orthogonal `FloorPlan` (placement isn't preserved). If the endpoints don't
/// both reach the outline (a free-standing interior room), the path is closed
/// into its own loop instead.
///
/// `pub(crate)` (not private) only so the carve-path / split-path unit tests in
/// `trim.rs` can drive it directly — they need an explicit tolerance and to
/// exercise each branch in isolation, which `carve_or_cut` doesn't expose. NOT
/// public API; the sole public entry is `FloorPlan::carve_or_cut`. (Same for
/// `split_cut`.)
pub(crate) fn sub_area(
    outline: &[Point2f32],
    path: &[Point2f32],
    tolerance: f32,
    tuning: &SubareaTuning,
) -> Result<FloorPlan, Error> {
    let diag = geom::bbox_diagonal(outline);

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

    // An OPEN path that reaches the outline at both ends is a CUT where we keep
    // ONE side automatically: the SHORTER outline arc (the corner/alcove the
    // path cuts off). Identical machinery to the user-pickable `split_cut` -- we
    // just pick the side here instead of prompting. (`carve_or_cut` only lands
    // here when the stroke was too loop-like for `split_cut` to offer as a cut.)
    if !drawn_closed && outline.len() >= 3 && path.len() >= 2 {
        if let Some(plan) = cut_outline(outline, path, tolerance, tuning)? {
            let replace_longer = longer_arc_is_1_to_2(outline, &plan);
            return cut_region(outline, &plan, replace_longer);
        }
    }

    // CLOSED loop, or an open path that doesn't reach the outline: two-pass
    // boundary-aware carve. Classify the loop into runs that HUG the outline and
    // runs that CUT through the interior, then rebuild so the hugging runs follow
    // the REAL wall (corners, diagonals and curve facets kept, walked in order)
    // and only the interior is orthogonalized -- so it follows the boundary
    // instead of free-floating into a box, and handles slanted/curved walls.
    let on_tol = (diag * tuning.on_ratio).max(1e-3).min(tuning.on_tol_ceil_m);
    let off_tol = (diag * tuning.off_ratio)
        .max(1e-3)
        .min(tuning.off_tol_ceil_m);
    // Densify first: the classifier samples POINTS, so a sparse stroke could leap
    // across the interior between two boundary edges undetected. The extra points
    // are dropped again by the interior RDP simplify.
    let dpath = boundary_follow::densify(path, on_tol);
    let path = &dpath[..];
    // Rotate the loop to start at a boundary<->interior transition so a run that
    // wraps past the stroke's start/end stays one run.
    let classes0 = boundary_follow::classify_points(outline, path, on_tol, off_tol);
    let rot = boundary_follow::rotate_to_transition(path, &classes0);
    let classes = boundary_follow::classify_points(outline, &rot, on_tol, off_tol);
    let bruns = boundary_follow::runs(&classes);
    let hugs_wall = bruns
        .iter()
        .any(|r| matches!(r, boundary_follow::Run::OnBoundary { .. }));
    let ortho = if hugs_wall {
        // Follows the outline wherever it was hugged (incl. the whole-boundary
        // case, which reproduces the full outline, notches/diagonals included).
        build_region_ring(outline, &rot, &bruns, tolerance, tuning)
    } else {
        // Never touches a wall -> a free-standing interior room: close the drawn
        // loop into its own orthogonal polygon.
        FloorPlan::from_points_oriented(path, tolerance, true, Some(0.0))?.get_points()
    };
    // `build_region_ring` already reproduced any diagonal/curve verbatim, so this
    // must NOT re-snap to diagonals.
    build_region(outline, &ortho)
}

/// The geometry CLEANUP shared by sub-area construction (cut, ortho-trim): snap
/// drawn walls onto parallel outline walls, strip coincident/collinear remnants,
/// un-intersect a folded ring, and square off spurious diagonals. Returns the
/// cleaned ring (NOT yet corner-anchored or parametrized).
///
/// Strips the coincident / collinear remnants left when the orthogonalizer
/// collapses a sub-threshold feature (e.g. a too-shallow nook) onto a wall:
/// those zero-length walls make `validate_simple` reject the shape and block
/// dimensioning. The clean tolerance is keyed off the SUB-AREA's own size — NOT
/// the outline's — so a small room inside a big house is cleaned at its own
/// scale. Kept tiny (0.5% of the area's diagonal) so it only removes genuine
/// collapse remnants and never a real (≥~10%) nook.
pub(crate) fn clean_region_ring(outline: &[Point2f32], pts: &[Point2f32]) -> Vec<Point2f32> {
    let area_diag = geom::bbox_diagonal(pts);

    // Correct any drawn wall that runs close to (and overlaps) a parallel house
    // wall onto it — so a sub-area drawn as a FULL loop, with some of its walls
    // meant to sit on the storey, lands exactly on those walls. Only AXIS-aligned
    // walls snap: a diagonal is placed verbatim by the boundary-aware loop, and a
    // cut's ends are already exact boundary crossings.
    let snap_tol = (area_diag * 0.06).max(1e-3);
    let snapped = snap_walls(outline, pts, snap_tol);

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
    let squared = geom::orthogonalize_off_boundary(&cleaned, outline, 0.003);
    let cleaned = orthogonal::remove_collinear(&geom::untangle_ring(&squared), collinear_tol);
    crate::trim::record_phase("cleaned ring", &cleaned, true);
    cleaned
}

/// Finalize a cleaned candidate ring against `outline` into an INDEPENDENT
/// `FloorPlan` (fresh params): anchor at a shared corner, parametrize. The
/// cut/carve region finalize. (Ortho-trim finalizes differently — in place.)
fn build_region(outline: &[Point2f32], pts: &[Point2f32]) -> Result<FloorPlan, Error> {
    let area_diag = geom::bbox_diagonal(pts);
    let cleaned = clean_region_ring(outline, pts);
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
    FloorPlan::from_clean_ring(&cleaned)
}

/// Split the outline with a CUT stroke into its TWO regions, returned as
/// independent `FloorPlan`s (so the caller can let the user pick which side
/// becomes the living area). The cut may be a straight line OR a STEPPED
/// (orthogonal, multi-leg) path — it's orthogonalized to the storey axes and
/// followed verbatim, so an L-shaped cut keeps its bend instead of being
/// flattened to a single axis. Returns `None` when the stroke is a loop, or
/// its two ends don't both reach the outline at distinct points (the caller
/// then falls back to [`sub_area`].
pub(crate) fn split_cut(
    outline: &[Point2f32],
    path: &[Point2f32],
    tolerance: f32,
    tuning: &SubareaTuning,
) -> Result<Option<(FloorPlan, FloorPlan)>, Error> {
    if outline.len() < 3 || path.len() < 2 {
        return Ok(None);
    }
    // A cut is an OPEN stroke (ends far apart) that divides the storey edge to
    // edge. A CLOSED loop (ends nearly meeting) is a drawn room -> carve. Whether
    // the open stroke reaches two DISTINCT outline edges is decided inside
    // `cut_outline`'s snap/reach check.
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
        return Ok(None); // a loop / 2D blob -> carve, not cut
    }
    // Shared machinery (orthogonalize + snap + square the ends onto the outline).
    // Here we hand the caller BOTH regions to pick from; the carve auto-keeps one.
    let Some(plan) = cut_outline(outline, path, tolerance, tuning)? else {
        return Ok(None);
    };
    Ok(Some((
        cut_region(outline, &plan, true)?,
        cut_region(outline, &plan, false)?,
    )))
}

/// A cut path orthogonalized and snapped onto the outline `ring`, ready to
/// splice: the squared poly-line plus the two boundary crossings it lands on.
/// `pub(crate)` so ortho-trim can reuse the cut's attach machinery.
pub(crate) struct CutPlan {
    pub(crate) opath: Vec<Point2f32>,
    pub(crate) e1: usize,
    pub(crate) t1: f32,
    pub(crate) e2: usize,
    pub(crate) t2: f32,
}

/// Resolve an OPEN cut `path` against `ring`. A STRAIGHT cut (step 2) slides both
/// ends along the edges they land on to a shared row/column; a STEPPED cut
/// (step 3) orthogonalizes the interior and slides each terminal end to its edge.
/// `None` when the cut doesn't reach the outline (→ carve); `Err` when it reaches
/// but can't be made orthogonal without changing its shape.
pub(crate) fn cut_outline(
    ring: &[Point2f32],
    path: &[Point2f32],
    tolerance: f32,
    tuning: &SubareaTuning,
) -> Result<Option<CutPlan>, Error> {
    crate::trim::record_phase("raw stroke", path, false);
    let diag = geom::bbox_diagonal(ring);
    let Some(((bnx, bny), (bxx, bxy))) = geom::bbox(path) else {
        return Ok(None);
    };
    let span = (bxx - bnx).max(bxy - bny);
    if span < 1e-6 {
        return Ok(None);
    }
    let cut_tol = tuning.coarse_simplify_tol(span, tolerance);
    let simplified = simplification::simplify_open(path, cut_tol);
    crate::trim::record_phase("simplified (RDP)", &simplified, false);
    let vertex_tol = (diag * tuning.cut_vertex_tol_ratio).max(1e-4);
    let reach_tol = (diag * tuning.cut_reach_tol_ratio).max(1e-3);

    // STEP 3: a cut with corners orthogonalizes to a stepped polyline. A
    // near-straight one collapses and falls through to the straight-cut solve.
    let stepped = if simplified.len() >= 3 {
        match orthogonal::make_orthogonal_at(&simplified, cut_tol, 0.0, orthogonal::Closure::Open) {
            Ok(o) if o.len() >= 3 => Some(o),
            _ => None,
        }
    } else {
        None
    };

    let Some(mut opath) = stepped else {
        // STEP 2: a single straight cut.
        return solve_straight_cut(
            ring,
            simplified[0],
            simplified[simplified.len() - 1],
            vertex_tol,
            reach_tol,
        );
    };

    // Place each terminal END onto the outline along its own leg's rail.
    let last = opath.len() - 1;
    let n = ring.len();
    let place = |opath: &mut Vec<Point2f32>, e_idx: usize, n_idx: usize| -> Option<(usize, f32)> {
        let end = opath[e_idx];
        let neighbour = opath[n_idx];
        let (edge, t_snap, d, is_vertex) = snap::snap_to_polygon(ring, end, vertex_tol);
        if d > reach_tol {
            return None;
        }
        let leg_horizontal = (end.y - neighbour.y).abs() <= (end.x - neighbour.x).abs();
        if is_vertex {
            let v = ring[edge];
            // Elective: the end snapped onto a CORNER rather than its edge.
            opath[e_idx] = snap::elective_snap(snap::SnapKind::ToVertex, end, v);
            if leg_horizontal {
                opath[n_idx].y = v.y;
            } else {
                opath[n_idx].x = v.x;
            }
            return Some((edge, 0.0));
        }
        let (a, b) = (ring[edge], ring[(edge + 1) % n]);
        let crossing = if leg_horizontal {
            let c = neighbour.y;
            ((b.y - a.y).abs() > 1e-6)
                .then(|| (c - a.y) / (b.y - a.y))
                .filter(|t| (0.0f32..=1.0).contains(t))
                .map(|t| (Point2f32::new(a.x + t * (b.x - a.x), c), t))
        } else {
            let c = neighbour.x;
            ((b.x - a.x).abs() > 1e-6)
                .then(|| (c - a.x) / (b.x - a.x))
                .filter(|t| (0.0f32..=1.0).contains(t))
                .map(|t| (Point2f32::new(c, a.y + t * (b.y - a.y)), t))
        };
        match crossing {
            Some((pt, t)) => {
                opath[e_idx] = pt;
                Some((edge, t))
            }
            None => {
                snap::pin_to_edge(&mut opath[e_idx], ring, edge);
                Some((edge, t_snap))
            }
        }
    };

    let drawn_reaches = |p: Point2f32| snap::snap_to_polygon(ring, p, vertex_tol).2 <= reach_tol;
    let drawn_reached_both = drawn_reaches(path[0]) && drawn_reaches(path[path.len() - 1]);
    let p1 = place(&mut opath, 0, 1usize.min(last));
    let p2 = place(&mut opath, last, last.saturating_sub(1));
    let (Some((e1, t1)), Some((e2, t2))) = (p1, p2) else {
        return if drawn_reached_both {
            Err(Error::CutWouldChangeShape)
        } else {
            Ok(None)
        };
    };
    if e1 == e2 && (t1 - t2).abs() < 1e-3 {
        return Ok(None);
    }
    crate::trim::record_phase("attached to outline", &opath, false);
    Ok(Some(CutPlan {
        opath,
        e1,
        t1,
        e2,
        t2,
    }))
}

/// `[lo, hi]` overlap of two 1-D ranges (each given by its two endpoints), or
/// `None` if they don't overlap.
fn range_overlap(a0: f32, a1: f32, b0: f32, b1: f32) -> Option<(f32, f32)> {
    let lo = a0.min(a1).max(b0.min(b1));
    let hi = a0.max(a1).min(b0.max(b1));
    (lo <= hi + 1e-4).then_some((lo, hi))
}

/// Place a point on edge `(a, b)` at a shared coordinate, sliding along the edge.
/// `horizontal_cut` → the shared coordinate `coord` is a Y (the cut is horizontal);
/// else it's an X. `toward` breaks the tie when the edge is parallel to the cut.
/// `None` if `coord` is off the edge's span. Returns the point and its `t`.
fn slide_on_edge(
    a: Point2f32,
    b: Point2f32,
    horizontal_cut: bool,
    coord: f32,
    toward: Point2f32,
) -> Option<(Point2f32, f32)> {
    let eps = 1e-4;
    if horizontal_cut {
        let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
        if coord < lo - eps || coord > hi + eps {
            return None;
        }
        let t = if (b.y - a.y).abs() < eps {
            // Horizontal edge at y≈coord: x free → slide toward `toward.x`.
            if (b.x - a.x).abs() < eps {
                0.0
            } else {
                ((toward.x - a.x) / (b.x - a.x)).clamp(0.0, 1.0)
            }
        } else {
            ((coord - a.y) / (b.y - a.y)).clamp(0.0, 1.0)
        };
        Some((Point2f32::new(a.x + t * (b.x - a.x), coord), t))
    } else {
        let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
        if coord < lo - eps || coord > hi + eps {
            return None;
        }
        let t = if (b.x - a.x).abs() < eps {
            if (b.y - a.y).abs() < eps {
                0.0
            } else {
                ((toward.y - a.y) / (b.y - a.y)).clamp(0.0, 1.0)
            }
        } else {
            ((coord - a.x) / (b.x - a.x)).clamp(0.0, 1.0)
        };
        Some((Point2f32::new(coord, a.y + t * (b.y - a.y)), t))
    }
}

/// STEP 2: a STRAIGHT cut. Snap each drawn end to the edge it sits on, then slide
/// both ends along their edges to a shared coordinate so the cut is one HORIZONTAL
/// or VERTICAL line (picking the orientation closest to the drawn cut). `Err`
/// (`CutWouldChangeShape`) when neither is possible (the two edges share no row or
/// column); `None` (→ carve) when the drawn cut doesn't reach the outline.
fn solve_straight_cut(
    ring: &[Point2f32],
    d0: Point2f32,
    d1: Point2f32,
    vertex_tol: f32,
    reach_tol: f32,
) -> Result<Option<CutPlan>, Error> {
    let n = ring.len();
    let eps = 1e-3_f32;
    let (e1, _, dd1, v1) = snap::snap_to_polygon(ring, d0, vertex_tol);
    let (e2, _, dd2, v2) = snap::snap_to_polygon(ring, d1, vertex_tol);
    if dd1 > reach_tol || dd2 > reach_tol {
        return Ok(None); // doesn't reach → let the caller carve
    }
    if e1 == e2 {
        return Ok(None); // both ends on one edge → not a dividing cut
    }
    let (a1, b1) = (ring[e1], ring[(e1 + 1) % n]);
    let (a2, b2) = (ring[e2], ring[(e2 + 1) % n]);
    let yov = range_overlap(a1.y, b1.y, a2.y, b2.y);
    let xov = range_overlap(a1.x, b1.x, a2.x, b2.x);

    // Resolve one cut end onto the shared row/column `c` (`horizontal` = shared
    // y). A VERTEX end pins to its CORNER — so a cut that extends an existing
    // wall terminates exactly at the wall's end (collinear), instead of sliding
    // onto and overlapping it, or leaving a sliver beside it. A mid-edge end
    // slides along its edge to `c`. `None` if a corner doesn't lie on `c`.
    let endpoint = |e: usize,
                    a: Point2f32,
                    b: Point2f32,
                    is_vertex: bool,
                    horizontal: bool,
                    c: f32,
                    toward: Point2f32|
     -> Option<(Point2f32, f32)> {
        if is_vertex {
            let v = ring[e];
            let on = if horizontal {
                (v.y - c).abs() <= eps
            } else {
                (v.x - c).abs() <= eps
            };
            on.then(|| {
                // Elective: terminate at the CORNER rather than sliding along the
                // edge to `c` (the fallback the user reverts to on undo).
                let from = slide_on_edge(a, b, horizontal, c, toward).map_or(toward, |(p, _)| p);
                (snap::elective_snap(snap::SnapKind::ToVertex, from, v), 0.0)
            })
        } else {
            slide_on_edge(a, b, horizontal, c, toward)
        }
    };

    let horizontal = || -> Option<CutPlan> {
        let (lo, hi) = yov?;
        // A vertex end fixes the row; else the overlap midpoint.
        let y0 = match (v1, v2) {
            (true, _) => ring[e1].y,
            (_, true) => ring[e2].y,
            _ => ((d0.y + d1.y) * 0.5).clamp(lo, hi),
        };
        if y0 < lo - eps || y0 > hi + eps {
            return None;
        }
        let (p1, t1) = endpoint(e1, a1, b1, v1, true, y0, d0)?;
        let (p2, t2) = endpoint(e2, a2, b2, v2, true, y0, d1)?;
        Some(CutPlan {
            opath: vec![p1, p2],
            e1,
            t1,
            e2,
            t2,
        })
    };
    let vertical = || -> Option<CutPlan> {
        let (lo, hi) = xov?;
        let x0 = match (v1, v2) {
            (true, _) => ring[e1].x,
            (_, true) => ring[e2].x,
            _ => ((d0.x + d1.x) * 0.5).clamp(lo, hi),
        };
        if x0 < lo - eps || x0 > hi + eps {
            return None;
        }
        let (p1, t1) = endpoint(e1, a1, b1, v1, false, x0, d0)?;
        let (p2, t2) = endpoint(e2, a2, b2, v2, false, x0, d1)?;
        Some(CutPlan {
            opath: vec![p1, p2],
            e1,
            t1,
            e2,
            t2,
        })
    };

    // Prefer the orientation matching the drawn cut's dominant axis.
    let h = horizontal();
    let v = vertical();
    let plan = if (d1.x - d0.x).abs() >= (d1.y - d0.y).abs() {
        h.or(v)
    } else {
        v.or(h)
    };
    match plan {
        Some(p) => Ok(Some(p)),
        None => Err(Error::CutWouldChangeShape),
    }
}

/// Splice `plan`'s cut into `outline` -- replacing the arc from crossing 1->2
/// when `replace_arc_1_to_2`, else the other arc -- and finalize into a
/// `FloorPlan`.
fn cut_region(
    outline: &[Point2f32],
    plan: &CutPlan,
    replace_arc_1_to_2: bool,
) -> Result<FloorPlan, Error> {
    let seq = splice::splice_sequence(
        outline,
        plan.e1,
        plan.t1,
        plan.e2,
        plan.t2,
        &plan.opath,
        replace_arc_1_to_2,
    );
    let pts: Vec<Point2f32> = seq
        .iter()
        .map(|el| match el {
            splice::SpliceElem::Keep(i) => outline[*i],
            splice::SpliceElem::New(p) => *p,
        })
        .collect();
    if pts.len() < 3 {
        return Err(Error::ExtendError("cut region collapsed".into()));
    }
    crate::trim::record_phase("spliced ring", &pts, true);
    build_region(outline, &pts)
}

/// Whether the outline arc from crossing 1->2 is the LONGER of the two -- the arc
/// to REPLACE with the cut when keeping the SHORTER side (the corner/alcove a
/// carve cuts off).
fn longer_arc_is_1_to_2(ring: &[Point2f32], plan: &CutPlan) -> bool {
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
    let (pos1, pos2) = (pos_of(plan.e1, plan.t1), pos_of(plan.e2, plan.t2));
    let arc_1_to_2 = if pos2 >= pos1 {
        pos2 - pos1
    } else {
        pos2 - pos1 + total
    };
    arc_1_to_2 > total - arc_1_to_2
}

/// Correct each axis-aligned edge of `pts` (a candidate sub-area ring) that
/// runs close to — and overlaps — a PARALLEL wall of this plan's outline,
/// snapping that edge's perpendicular coordinate exactly onto the outline
/// wall. Lets the user draw a living area as a full closed loop and have the
/// walls they meant to share with the storey land on it. Coordinate snaps
/// are applied to every vertex sharing the coord, so the ring stays a clean
/// orthogonal polygon. `tol` is the max perpendicular gap that still snaps.
fn snap_walls(outline: &[Point2f32], pts: &[Point2f32], tol: f32) -> Vec<Point2f32> {
    let no = outline.len();
    if no < 3 || pts.len() < 3 {
        return pts.to_vec();
    }
    let eps = 1e-3_f32;
    // Outline walls split by axis: vertical = (x, ylo, yhi); horizontal =
    // (y, xlo, xhi).
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
                .filter(|(ox, ylo, yhi)| (ox - a.x).abs() <= tol && overlaps(lo, hi, *ylo, *yhi))
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
                .filter(|(oy, xlo, xhi)| (oy - a.y).abs() <= tol && overlaps(lo, hi, *xlo, *xhi))
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
        let drawn = *p;
        if let Some((_, to)) = remap_x.iter().find(|(from, _)| (p.x - from).abs() < eps) {
            p.x = *to;
        }
        if let Some((_, to)) = remap_y.iter().find(|(from, _)| (p.y - from).abs() < eps) {
            p.y = *to;
        }
        // Elective: this vertex moved onto a parallel boundary wall.
        if *p != drawn {
            *p = snap::elective_snap(snap::SnapKind::ToParallelWall, drawn, *p);
        }
    }

    out
}

#[cfg(test)]
mod cut_line_tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A horizontal cut meeting a DIAGONAL house wall slides its end ALONG the
    /// cut to the clean axis-crossing (~(8,8) on x+y=16) — a single straight cut
    /// wall, no jog — because the orthogonality-aware snap moves the end along its
    /// own rail to where that rail crosses the diagonal.
    #[test]
    fn cut_to_a_diagonal_wall_reaches_it() {
        // Square with the top-right corner cut by a diagonal (10,6)->(6,10).
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(10., 0.),
            p(10., 6.),
            p(6., 10.),
            p(0., 10.),
        ])
        .unwrap();
        assert!(
            !outline.non_orthogonal_edges().is_empty(),
            "outline has a diagonal"
        );
        // Horizontal cut from the left wall across to the diagonal (meets at (8,8)).
        let cut = vec![p(0.1, 8.0), p(7.0, 8.0)];
        let split = split_cut(&outline.get_points(), &cut, 0.1, &SubareaTuning::default()).unwrap();
        let (a, b) = split.expect("cut reaches the left wall and the diagonal → two regions");
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();
        let on_diag = |fp: &FloorPlan| {
            fp.get_points()
                .iter()
                .any(|q| (q.x + q.y - 16.0).abs() < 0.2 && (q.y - 8.0).abs() < 0.4)
        };
        assert!(
            on_diag(&a) || on_diag(&b),
            "the cut's far end reached the diagonal near (8,8): a={:?} b={:?}",
            a.get_points(),
            b.get_points()
        );
    }

    /// A "vertical" cut across an all-DIAGONAL storey (a diamond), drawn from the
    /// upper-left edge to the lower-right edge. Those edges share a common x only
    /// at the vertices, so the only orthogonal cut slides both ends to the top and
    /// bottom vertices — a clean vertical diagonal (5,10)-(5,0). The ends leave the
    /// edges they were drawn on because that's the sole feasible orthogonal cut.
    #[test]
    fn diamond_vertical_cut_slides_to_the_vertices() {
        // CCW diamond: bottom (5,0), right (10,5), top (5,10), left (0,5).
        let outline =
            FloorPlan::from_clean_ring(&[p(5., 0.), p(10., 5.), p(5., 10.), p(0., 5.)]).unwrap();
        let cut = vec![p(2., 7.), p(6., 1.)]; // upper-left edge → lower-right edge
        let (a, b) = split_cut(&outline.get_points(), &cut, 0.1, &SubareaTuning::default())
            .unwrap()
            .expect("slides to the vertical diagonal between the top and bottom vertices");
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();
        let has = |fp: &FloorPlan, x: f32, y: f32| {
            fp.get_points()
                .iter()
                .any(|q| (q.x - x).abs() < 0.1 && (q.y - y).abs() < 0.1)
        };
        // The cut runs (5,10)-(5,0); both regions carry those endpoints.
        assert!(
            has(&a, 5., 10.) && has(&a, 5., 0.),
            "region a on the cut: {:?}",
            a.get_points()
        );
        assert!(
            has(&b, 5., 10.) && has(&b, 5., 0.),
            "region b on the cut: {:?}",
            b.get_points()
        );
    }

    /// A cut between two edges that share NO row or column — the top-left and
    /// bottom-right chamfers of an octagon — can't be made horizontal OR vertical
    /// by sliding, so it errors clearly rather than distorting the cut's shape.
    #[test]
    fn cut_between_opposite_chamfers_errors() {
        // Chamfered square: TL chamfer (2,10)-(0,8) [x∈0..2,y∈8..10]; BR chamfer
        // (8,0)-(10,2) [x∈8..10,y∈0..2] — disjoint in both x and y.
        let outline = FloorPlan::from_clean_ring(&[
            p(2., 0.),
            p(8., 0.),
            p(10., 2.),
            p(10., 8.),
            p(8., 10.),
            p(2., 10.),
            p(0., 8.),
            p(0., 2.),
        ])
        .unwrap();
        let cut = vec![p(1., 9.), p(9., 1.)]; // TL chamfer → BR chamfer
        let err = carve_or_cut(&outline.get_points(), &cut, &SubareaTuning::default())
            .expect_err("opposite chamfers share no row or column");
        assert!(matches!(err, Error::CutWouldChangeShape), "got {err:?}");
    }

    /// PARKED: a terminal leg drawn PARALLEL to a wall, ending in the MIDDLE of it
    /// (far from any corner, so it doesn't snap to a vertex), can't slide along
    /// itself to reach that wall. Today `place` falls back to the perpendicular
    /// foot, which TILTS the leg. The desired behavior is to NOT emit a
    /// non-orthogonal cut — either decline it or add a perpendicular jog. Ignored
    /// until we decide reject-vs-jog for the parallel-wall fallback.
    #[ignore = "parallel-wall fallback can tilt the leg; reject-vs-jog not decided"]
    #[test]
    fn cut_leg_along_a_parallel_wall_stays_orthogonal_or_declines() {
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap();
        // L-cut: up at x=3, then a HORIZONTAL leg ending mid-way under the top
        // wall at (6, 9.7) — parallel to it, far from any corner.
        let cut = vec![p(3., 0.), p(3., 9.7), p(6., 9.7)];
        match split_cut(&outline.get_points(), &cut, 0.1, &SubareaTuning::default()) {
            Ok(None) | Err(_) => {} // declined or errored — acceptable
            Ok(Some((a, b))) => assert!(
                a.non_orthogonal_edges().is_empty() && b.non_orthogonal_edges().is_empty(),
                "the parallel-wall leg must not tilt: a={:?} b={:?}",
                a.non_orthogonal_edges(),
                b.non_orthogonal_edges()
            ),
        }
    }

    /// Documents why the interior run still can't use cut's slide, EVEN with the
    /// corner-snap: when a hug→interior transition lands MID-WALL (far from any
    /// corner), the boundary follower projects it PERPENDICULAR onto the wall,
    /// while `cut_outline` slides its leg to the RAIL-crossing — two different
    /// points on a diagonal, with no corner to snap either to. So routing the
    /// interior run through cut would still detach the ring (Problem B). Asserts
    /// the two endpoints match, so it FAILS under `--ignored`, showing the gap.
    #[ignore = "cut endpoint vs boundary-follower endpoint diverge mid-wall; corner-snap can't reconcile them"]
    #[test]
    fn cut_and_boundary_follower_endpoints_diverge_mid_wall() {
        // Right triangle: the hypotenuse (10,0)->(0,10) is a long DIAGONAL wall.
        let outline = FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(0., 10.)]).unwrap();
        let ring = outline.get_points();
        // A vertical cut from the bottom wall up to MID-hypotenuse (drawn end (4,5)).
        let cut = vec![p(4., 0.2), p(4., 5.)];
        let plan = cut_outline(&ring, &cut, 0.05, &SubareaTuning::default())
            .unwrap()
            .expect("the cut reaches both walls");
        // cut's hypotenuse endpoint: it slid the vertical leg to the rail crossing.
        let cut_end = plan.opath[1];
        // boundary follower's endpoint: project the drawn end PERPENDICULAR onto
        // the hypotenuse (edge 1), with corner-snap applied (a no-op here — the
        // nearest corner is ~6 m away).
        let bf_end = snap::snap_to_corner(
            project_onto_edge(&ring, p(4., 5.), 1),
            &ring,
            SubareaTuning::default().corner_snap_m,
        );
        assert!(
            (cut_end - bf_end).norm() < 0.02,
            "cut endpoint {cut_end:?} != boundary-follower endpoint {bf_end:?} \
             (mid-wall diagonal, no corner to reconcile)"
        );
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.1,
            &SubareaTuning::default(),
        )
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

    /// `split_cut` returns BOTH regions of a cut so the caller can let the
    /// user pick. The two areas are complementary (together ~= the whole storey)
    /// and both orthogonal here.
    #[test]
    fn split_cut_returns_both_regions() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 6.), p(0., 6.), p(0.1, 0.1)],
            0.2,
            true,
        )
        .unwrap();
        let stroke = vec![p(3.0, 0.05), p(3.0, 5.95)];
        let (a, b) = split_cut(
            &outline.get_points(),
            &stroke,
            0.1,
            &SubareaTuning::default(),
        )
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
        assert!(
            split_cut(&outline.get_points(), &bent, 0.1, &SubareaTuning::default())
                .unwrap()
                .is_some()
        );
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
        let (a, b) = split_cut(
            &outline.get_points(),
            &stroke,
            0.1,
            &SubareaTuning::default(),
        )
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
        let (a, b) = split_cut(
            &outline.get_points(),
            &stepped,
            0.1,
            &SubareaTuning::default(),
        )
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
    /// terminal leg must stay axis-aligned. The bottom end sits just inside the
    /// (10,0) corner, so it snaps to that corner's vertical wall.
    #[test]
    fn cut_snapping_to_a_corner_stays_orthogonal() {
        let outline = FloorPlan::from_points_to_orthogonal(
            &[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.), p(0.1, 0.1)],
            0.2,
            true,
        )
        .unwrap();
        let stepped = vec![p(9.7, 0.15), p(9.7, 5.0), p(5.0, 5.0), p(5.0, 9.85)];
        let (a, b) = split_cut(
            &outline.get_points(),
            &stepped,
            0.1,
            &SubareaTuning::default(),
        )
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
        let (a, b) = split_cut(&region.get_points(), &cut, 0.05, &SubareaTuning::default())
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

    /// A STRAIGHT cut whose end snaps to a VERTEX must stay axis-aligned: the leg
    /// lands exactly on the corner and its inner neighbour is aligned onto the
    /// corner's rail, so the cut is vertical THROUGH the vertex — not tilted, and
    /// with no stray free parameter.
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
        let (a, b) = split_cut(
            &outline.get_points(),
            &stroke,
            0.1,
            &SubareaTuning::default(),
        )
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
        let res = split_cut(
            &outline.get_points(),
            &stroke,
            0.1,
            &SubareaTuning::default(),
        );
        assert!(res.is_ok(), "split must not error: {:?}", res);
        let (a, b) = res.unwrap().expect("still splits into two regions");
        a.validate_simple().unwrap();
        b.validate_simple().unwrap();
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
        let (a, b) = split_cut(
            &outline.get_points(),
            &stepped,
            0.1,
            &SubareaTuning::default(),
        )
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
        let (ra, rb) = split_cut(
            &outline.get_points(),
            &stroke,
            0.05,
            &SubareaTuning::default(),
        )
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
        let res = split_cut(
            &outline.get_points(),
            &stroke,
            0.02,
            &SubareaTuning::default(),
        )
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
        let (ra, rb) = split_cut(
            &outline.get_points(),
            &stroke,
            0.02,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.15,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.02,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.1,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.04,
            &SubareaTuning::default(),
        )
        .unwrap();
        let poly = room.polygons()[0].clone();
        // EXACTLY the drawn shape: 4 walls — the shared diagonal + three drawn
        // walls — with NO spurious floor stub at the bottom corner.
        assert_eq!(poly.len(), 4, "exactly 4 walls (no floor stub): {poly:?}");
        // exactly one non-orthogonal edge (the shared diagonal)
        assert_eq!(
            room.non_orthogonal_edges().len(),
            1,
            "diagonal only: {poly:?}"
        );
        // the diagonal follows ALL THE WAY to the storey corner v0 = (2.677, 1.772)
        // — the corner-snap makes the hug reach it instead of stopping short.
        assert!(
            poly.iter()
                .any(|v| (v.x - 2.677).abs() < 0.02 && (v.y - 1.772).abs() < 0.01),
            "diagonal reaches the corner v0 (2.677, 1.772): {poly:?}"
        );
        // the far wall snapped down onto the storey top (4.772), not left at 4.80
        let max_y = poly.iter().map(|v| v.y).fold(f32::MIN, f32::max);
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.1,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.023,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.058,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.049,
            &SubareaTuning::default(),
        )
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
        let room = sub_area(
            &outline.get_points(),
            &stroke,
            0.066,
            &SubareaTuning::default(),
        )
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

#[cfg(test)]
mod boundary_carve_tests {
    use super::*;
    use crate::boundary_follow::{classify_points, runs, PointClass};

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    fn ring_area(ring: &[Point2f32]) -> f32 {
        crate::geom::ring_signed_area2(ring).abs() / 2.0
    }

    fn has_vertex(ring: &[Point2f32], x: f32, y: f32) -> bool {
        ring.iter()
            .any(|q| (q.x - x).abs() < 1e-2 && (q.y - y).abs() < 1e-2)
    }

    // CCW square. Edge 0 = bottom (y=0), 1 = right (x=10), 2 = top, 3 = left.
    fn square() -> Vec<Point2f32> {
        vec![p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]
    }

    /// PASS 2: the bottom-strip loop builds a region whose bottom edge lies on
    /// the real wall (y=0) — not a free-floating box.
    #[test]
    fn bottom_strip_builds_region_on_the_wall() {
        let outline = square();
        let stroke = vec![
            p(2., 0.),
            p(4., 0.),
            p(6., 0.),
            p(8., 0.),
            p(8., 2.),
            p(8., 3.),
            p(6., 3.),
            p(4., 3.),
            p(2., 3.),
            p(2., 2.),
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 18.0).abs() < 0.5,
            "strip [2,8]x[0,3] ≈ 18 m², got {} (ring {:?})",
            ring_area(&ring),
            ring
        );
        // The two bottom corners sit exactly on the wall y=0.
        assert!(has_vertex(&ring, 2.0, 0.0), "left end on the wall");
        assert!(has_vertex(&ring, 8.0, 0.0), "right end on the wall");
        assert!(
            ring.iter().filter(|q| q.y.abs() < 1e-2).count() >= 2,
            "≥2 vertices on the bottom wall"
        );
    }

    /// PASS 2: the corner loop keeps the REAL outline corner (10,0) — the run
    /// rounded it, so the region's boundary follows the house through the corner.
    #[test]
    fn corner_loop_keeps_the_real_corner() {
        let outline = square();
        let stroke = vec![
            p(4., 0.),
            p(6., 0.),
            p(8., 0.),
            p(10., 0.),
            p(10., 2.),
            p(10., 4.),
            p(8., 4.),
            p(6., 4.),
            p(4., 4.),
            p(4., 2.),
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 24.0).abs() < 0.5,
            "corner rect [4,10]x[0,4] ≈ 24 m², got {} (ring {:?})",
            ring_area(&ring),
            ring
        );
        assert!(
            has_vertex(&ring, 10.0, 0.0),
            "real outline corner (10,0) preserved in the region: {:?}",
            ring
        );
    }

    /// REGRESSION: a DENSE freehand interior (hundreds of points, as a real
    /// stroke) must collapse to a clean few-segment cut — not one stair-step per
    /// point. (`snap_open_fixed_ends` steps per point, so the interior must be
    /// RDP-simplified first.)
    #[test]
    fn dense_interior_does_not_staircase() {
        let outline = square();
        let mut stroke = vec![p(2., 0.), p(4., 0.), p(6., 0.), p(8., 0.)]; // bottom, on wall
                                                                           // dense, slightly wobbly right wall going up
        for k in 1..=20 {
            let wob = if k % 2 == 0 { 0.05 } else { -0.05 };
            stroke.push(p(8.0 + wob, 0.2 + 2.6 * k as f32 / 20.0));
        }
        // dense top
        for k in 1..=10 {
            stroke.push(p(8.0 - 6.0 * k as f32 / 10.0, 3.0));
        }
        // dense left wall going down
        for k in 1..=20 {
            stroke.push(p(2.0, 3.0 - 2.6 * k as f32 / 20.0));
        }
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            ring.len() <= 8,
            "dense interior collapses to a clean cut, got {} vertices: {:?}",
            ring.len(),
            ring
        );
        assert!(
            (ring_area(&ring) - 18.0).abs() < 1.5,
            "strip ≈ 18 m², got {}",
            ring_area(&ring)
        );
    }

    /// REGRESSION (the notch bug): drawing a loop AROUND the whole outline — all
    /// boundary, no interior cut — must reproduce the full outline including a
    /// notch, NOT collapse to a box.
    #[test]
    fn draw_around_whole_outline_keeps_the_notch() {
        // 10x10 square with a [7,10]x[3,6] notch cut out of the right side.
        let outline = vec![
            p(0., 0.),
            p(10., 0.),
            p(10., 3.),
            p(7., 3.),
            p(7., 6.),
            p(10., 6.),
            p(10., 10.),
            p(0., 10.),
        ];
        let stroke = vec![
            p(0.5, 0.),
            p(5., 0.),
            p(9., 0.), // bottom
            p(10., 1.),
            p(10., 2.5), // right-lower
            p(9., 3.),
            p(7.5, 3.), // notch bottom
            p(7., 4.),
            p(7., 5.5), // notch back
            p(8., 6.),
            p(9.5, 6.), // notch top
            p(10., 7.),
            p(10., 9.5), // right-upper
            p(9., 10.),
            p(5., 10.),
            p(1., 10.), // top
            p(0., 9.),
            p(0., 5.),
            p(0., 1.), // left
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        assert!(
            classes
                .iter()
                .all(|c| matches!(c, PointClass::OnBoundary { .. })),
            "the whole trace hugs the boundary: {classes:?}"
        );
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 91.0).abs() < 3.0,
            "L-shape ≈ 91 m² (notch kept), got {} — a box would be 100 (ring {:?})",
            ring_area(&ring),
            ring
        );
        assert!(
            has_vertex(&ring, 7.0, 3.0) && has_vertex(&ring, 7.0, 6.0),
            "notch corners preserved: {:?}",
            ring
        );
    }

    /// `build_region_ring` reproduces a DIAGONAL edge when the whole loop hugs
    /// the boundary (it walks the real outline vertices). NB: the live carve
    /// still routes diagonal *rooms* through `boundary_aware_loop_points`, which
    /// is more robust on small slanted-wall rooms — this just documents that the
    /// general builder handles a diagonal in the simple draw-around case.
    #[test]
    fn draw_around_diagonal_outline_keeps_the_diagonal() {
        // Square with the top-right corner cut by a diagonal (10,7)->(7,10).
        let outline = vec![p(0., 0.), p(10., 0.), p(10., 7.), p(7., 10.), p(0., 10.)];
        let stroke = vec![
            p(1., 0.),
            p(5., 0.),
            p(9., 0.), // bottom
            p(10., 2.),
            p(10., 5.), // right
            p(9.25, 7.75),
            p(8.5, 8.5),
            p(7.75, 9.25), // along the diagonal
            p(5., 10.),
            p(2., 10.), // top
            p(0., 7.),
            p(0., 3.),
            p(0., 0.5), // left
        ];
        let classes = classify_points(&outline, &stroke, 0.6, 1.6);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 95.5).abs() < 3.0,
            "pentagon ≈ 95.5 m² (diagonal kept), got {} (ring {:?})",
            ring_area(&ring),
            ring
        );
        assert!(
            has_vertex(&ring, 10.0, 7.0) && has_vertex(&ring, 7.0, 10.0),
            "diagonal endpoints preserved (not flattened): {:?}",
            ring
        );
    }
}

#[cfg(test)]
mod checkpoint_cut {
    use super::*;
    use crate::SubareaTuning;
    fn pt(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }
    fn outline() -> Vec<Point2f32> {
        vec![
            pt(2.95, 0.571),
            pt(2.45, 0.571),
            pt(2.45, 1.571),
            pt(1.55, 1.571),
            pt(1.55, 2.571),
            pt(1.552, 2.653),
            pt(1.562, 2.735),
            pt(1.578, 2.815),
            pt(1.601, 2.894),
            pt(1.63, 2.971),
            pt(1.665, 3.045),
            pt(1.706, 3.116),
            pt(1.753, 3.183),
            pt(1.805, 3.246),
            pt(1.863, 3.305),
            pt(1.925, 3.359),
            pt(1.991, 3.407),
            pt(2.061, 3.45),
            pt(2.134, 3.487),
            pt(2.21, 3.518),
            pt(2.289, 3.542),
            pt(2.369, 3.56),
            pt(2.45, 3.571),
            pt(3.45, 3.571),
            pt(3.45, 3.071),
            pt(4.15, 3.071),
            pt(4.15, 3.571),
            pt(5.45, 3.571),
            pt(5.45, 2.871),
            pt(4.85, 2.571),
            pt(3.45, 2.571),
            pt(3.45, 1.271),
            pt(3.455, 1.219),
            pt(3.456, 1.166),
            pt(3.453, 1.113),
            pt(3.445, 1.061),
            pt(3.433, 1.01),
            pt(3.416, 0.959),
            pt(3.395, 0.911),
            pt(3.37, 0.864),
            pt(3.341, 0.82),
            pt(3.309, 0.778),
            pt(3.273, 0.74),
            pt(3.234, 0.704),
            pt(3.192, 0.672),
            pt(3.147, 0.644),
            pt(3.1, 0.619),
            pt(3.052, 0.599),
            pt(3.001, 0.583),
        ]
    }
    #[test]
    fn short_cut_in_line_with_a_wall_still_divides() {
        let o = outline();
        let stroke = vec![
            pt(3.460, 2.561),
            pt(3.460, 2.65),
            pt(3.460, 2.72),
            pt(3.454, 2.84),
            pt(3.454, 2.90),
            pt(3.454, 2.95),
            pt(3.457, 2.996),
            pt(3.460, 3.04),
            pt(3.460, 3.082),
        ];
        let t = SubareaTuning::default();
        let res = FloorPlan::from_clean_ring(&o)
            .unwrap()
            .carve_or_cut(&stroke, &t);
        // A short near-vertical cut (ends within `close_gap_m`, so it must NOT be
        // mis-clipped as a closed loop) whose ends land on CORNERS in line with an
        // existing wall must still be a dividing CUT: the cut extends the wall
        // through the gap (terminating at the corners), splitting off the room.
        match res {
            Ok(SubareaOutcome::Cut(a, b)) => {
                a.validate_simple().expect("region a valid");
                b.validate_simple().expect("region b valid");
            }
            other => panic!("expected a Cut, got {other:?}"),
        }
    }
}
