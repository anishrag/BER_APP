//! Splicing an "extend" stroke into an existing orthogonal polygon while
//! preserving the parametric model.
//!
//! The Extend tool lets the assessor draw a new orthogonal path whose two
//! endpoints land on (or near) the existing outline. The pipeline:
//!
//!   1. [`snap_to_polygon`] anchors each stroke endpoint to the perimeter,
//!      yielding `(edge, t)` attach points.
//!   2. [`orthogonalize_open_path`] cleans the freehand porch into an
//!      axis-aligned open polyline.
//!   3. [`plan_splice_sequence`] decides which existing vertices survive and
//!      in what order the porch splices in — replacing the *shorter* perimeter
//!      arc between the two attach points. It tags each output slot as either
//!      [`SpliceElem::Keep`] (an existing vertex, preserved verbatim) or
//!      [`SpliceElem::New`] (an attach/porch point needing fresh exprs).
//!   4. `ParametricPlan::splice_into_edge` consumes that sequence, keeping
//!      every kept vertex's identity and parameter IDs intact — so previously
//!      dimensioned walls stay dimensioned.

use crate::{geom, Error, SpliceTuning};
use domain::points::Point2f32;

/// One slot in a planned spliced ring.
#[derive(Debug, Clone, PartialEq)]
pub enum SpliceElem {
    /// Preserve the existing ring vertex at this index — identity, coordinate
    /// expressions, and parameter IDs all carry through unchanged.
    Keep(usize),
    /// A brand-new vertex (a stroke attach point or porch-interior corner).
    /// `splice_into_edge` assigns its coordinate expressions, reusing existing
    /// exprs where coords match and minting fresh parameters otherwise.
    New(Point2f32),
}

/// Snap a point to a polygon's perimeter: vertex if any is within
/// `vertex_tol`, else the closest point on the closest edge. Returns
/// `(edge_idx, t, distance, snapped_to_vertex)`. When snapping to a
/// vertex i, returns `(i, 0.0, dist, true)` — treating vertex i as the
/// start of edge i.
pub fn snap_to_polygon(
    ring: &[Point2f32],
    pt: Point2f32,
    vertex_tol: f32,
) -> (usize, f32, f32, bool) {
    let n = ring.len();
    if n < 2 {
        return (0, 0.0, f32::INFINITY, false);
    }
    // Closest vertex.
    let mut closest_v: (usize, f32) = (0, f32::INFINITY);
    for i in 0..n {
        let dx = ring[i].x - pt.x;
        let dy = ring[i].y - pt.y;
        let d = (dx * dx + dy * dy).sqrt();
        if d < closest_v.1 {
            closest_v = (i, d);
        }
    }
    if closest_v.1 <= vertex_tol {
        return (closest_v.0, 0.0, closest_v.1, true);
    }
    // Otherwise closest point on an edge.
    let mut best: (usize, f32, f32) = (0, 0.0, f32::INFINITY);
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        if (a.x - b.x).powi(2) + (a.y - b.y).powi(2) < 1e-6 {
            continue; // degenerate edge
        }
        // `snap_to_polygon` returns a CLAMPED edge parameter (callers rebuild the
        // point from it), so clamp the shared helper's unclamped `t`.
        let (_q, t, dist) = crate::geom::project_point_to_segment(pt, a, b);
        if dist < best.2 {
            best = (i, t.clamp(0.0, 1.0), dist);
        }
    }
    (best.0, best.1, best.2, false)
}

/// Snap each point of `stroke` onto `ring`'s perimeter when within `snap_tol`,
/// leaving points farther away untouched. Used by the multi-storey "draw on
/// top of the previous storey" flow so endpoints land exactly on the previous
/// outline (so the new storey's footprint reuses those coordinates verbatim).
/// `snap_to_polygon` already handles closest-vertex vs closest-edge — we just
/// project to the returned `(edge, t)` when the gap is within tolerance.
pub fn snap_stroke_to_ring(
    stroke: &[Point2f32],
    ring: &[Point2f32],
    snap_tol: f32,
) -> Vec<Point2f32> {
    if ring.len() < 2 {
        return stroke.to_vec();
    }
    let n = ring.len();
    stroke
        .iter()
        .map(|p| {
            let (edge, t, dist, on_vertex) = snap_to_polygon(ring, *p, snap_tol);
            if dist > snap_tol {
                return *p;
            }
            let a = ring[edge % n];
            if on_vertex {
                return a;
            }
            let b = ring[(edge + 1) % n];
            Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
        })
        .collect()
}

/// Orthogonalize an *open* porch stroke into a clean axis-aligned polyline.
/// Unlike `FloorPlan::from_points_to_orthogonal` this does no closing-edge
/// snap or corner rotation — the path stays open, so its first and last
/// points remain the two attach points.
pub fn orthogonalize_open_path(
    points: &[Point2f32],
    tolerance: f32,
) -> Result<Vec<Point2f32>, Error> {
    let simplified = crate::simplification::simplify_stroke(points, tolerance)?;
    crate::orthogonal::make_orthogonal(&simplified, tolerance)
}

/// Pin a near-edge point exactly onto an edge of `ring` by projecting it onto
/// that edge's LINE — so it sits on the boundary while keeping its position
/// along the edge. For an axis-aligned edge this is exactly "match the edge's
/// perpendicular coordinate" (a horizontal edge fixes `y`, a vertical one `x`);
/// for a DIAGONAL edge (a curve facet) it projects onto the slanted line rather
/// than snapping to an axis, which would drag the point off the boundary. Used
/// to pin attach/cut endpoints onto their edges so they line up with the spliced
/// boundary — extend strokes attach to axis walls, a cut may reach a curve.
pub fn pin_to_edge(p: &mut Point2f32, ring: &[Point2f32], edge: usize) {
    let n = ring.len();
    if n < 2 {
        return;
    }
    let a = ring[edge % n];
    let b = ring[(edge + 1) % n];
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    if len2 < 1e-12 {
        return; // degenerate edge — nothing to pin onto
    }
    // Foot of the perpendicular from `p` to the line through a→b. For an axis
    // edge this reduces to setting just the perpendicular coordinate (the
    // parallel one projects back to itself), so axis behaviour is unchanged.
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2;
    p.x = a.x + t * dx;
    p.y = a.y + t * dy;
}

/// Plan how an extend stroke splices into `ring`, replacing the SHORTER of the
/// two perimeter arcs between the attach points with the stroke. Returns the
/// new ring as an ordered list of [`SpliceElem`]s: preserved vertices are
/// `Keep(idx)`; the two attach points (`ortho_path`'s endpoints) and the porch
/// interior are `New`. Same-edge bumps split the edge; cross-edge replacements
/// drop the spanned corner(s). The stroke orients automatically.
pub fn plan_splice_sequence(
    ring: &[Point2f32],
    e1: usize,
    t1: f32,
    e2: usize,
    t2: f32,
    ortho_path: &[Point2f32],
) -> Vec<SpliceElem> {
    let n = ring.len();
    let edge_lens: Vec<f32> = (0..n)
        .map(|i| {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
        })
        .collect();
    let total: f32 = edge_lens.iter().sum();
    let pos_of =
        |e: usize, t: f32| -> f32 { edge_lens[..e].iter().sum::<f32>() + t * edge_lens[e] };
    let arc_1_to_2 = {
        let (pos1, pos2) = (pos_of(e1, t1), pos_of(e2, t2));
        if pos2 >= pos1 {
            pos2 - pos1
        } else {
            pos2 - pos1 + total
        }
    };
    // Extend replaces the shorter arc with the porch.
    let replace_arc_1_to_2 = arc_1_to_2 <= total - arc_1_to_2;
    splice_sequence(ring, e1, t1, e2, t2, ortho_path, replace_arc_1_to_2)
}

/// Build the spliced ring, replacing one specified arc between the attach
/// points with `ortho_path`. `replace_arc_1_to_2 = true` replaces the arc that
/// runs forward from attach 1 to attach 2 (preserving the complementary arc);
/// `false` replaces the other. Preserved vertices are `Keep(idx)`; the two
/// attach points and the path interior are `New`.
///
/// Extend always replaces the SHORTER arc (via `plan_splice_sequence`); Trim
/// builds both and keeps whichever leaves the larger-area polygon.
pub fn splice_sequence(
    ring: &[Point2f32],
    e1: usize,
    t1: f32,
    e2: usize,
    t2: f32,
    ortho_path: &[Point2f32],
    replace_arc_1_to_2: bool,
) -> Vec<SpliceElem> {
    let n = ring.len();
    // Attach points are the (pinned-to-edge) endpoints of the orthogonalized
    // path; everything between is path interior.
    let attach1 = *ortho_path.first().expect("ortho_path non-empty");
    let attach2 = *ortho_path.last().expect("ortho_path non-empty");
    let interior = if ortho_path.len() > 2 {
        &ortho_path[1..ortho_path.len() - 1]
    } else {
        &[][..]
    };

    // Walk forward from edge `se` (just past it) to edge `ee`, collecting the
    // indices of the existing ring vertices on the PRESERVED arc. `et > 0`
    // means the end attach point lies past `ring[ee]`, so `ring[ee]` is on the
    // preserved side and is included.
    let walk_forward = |se: usize, st: f32, ee: usize, et: f32| -> Vec<usize> {
        let mut out = Vec::new();
        let eps = 1e-3_f32;
        if se == ee && et >= st - eps {
            return out;
        }
        let mut i = (se + 1) % n;
        let safety = n + 4;
        for _ in 0..safety {
            if i == ee {
                if et > eps {
                    out.push(i);
                }
                break;
            }
            out.push(i);
            i = (i + 1) % n;
        }
        out
    };

    let mut seq: Vec<SpliceElem> = Vec::with_capacity(n + ortho_path.len());
    if replace_arc_1_to_2 {
        // Replace forward 1→2 arc; preserved arc walked forward 2→1.
        seq.push(SpliceElem::New(attach2));
        for i in walk_forward(e2, t2, e1, t1) {
            seq.push(SpliceElem::Keep(i));
        }
        seq.push(SpliceElem::New(attach1));
        for p in interior {
            seq.push(SpliceElem::New(*p));
        }
    } else {
        // Replace backward (wrap) arc; preserved arc walked forward 1→2,
        // path reversed (attach2 → attach1).
        seq.push(SpliceElem::New(attach1));
        for i in walk_forward(e1, t1, e2, t2) {
            seq.push(SpliceElem::Keep(i));
        }
        seq.push(SpliceElem::New(attach2));
        for p in interior.iter().rev() {
            seq.push(SpliceElem::New(*p));
        }
    }
    seq
}

// ── New-storey reproduce check ──────────────────────────────────────────────
// Does a freehand trace of an upper storey faithfully follow the storey below?
// Pure geometry over the previous outline ring; the new-storey flow uses these
// alongside the splice above. Exposed via thin FloorPlan facades (see lib.rs).
/// Longest run of CONSECUTIVE `points` lying farther than `off_tol` from this
/// plan's outline ring (cyclic — the trace is a closed loop). Used by the
/// new-storey flow to tell a trace that FOLLOWS this boundary (only isolated
/// or short strays — hand slop, e.g. tracing a curve loosely → short run)
/// from one with a SUSTAINED excursion such as an extension (a long run of
/// points well off the boundary). A run-of-points test is robust where a
/// max-distance test isn't: one deep stray near a curve doesn't look like a
/// new wall, but several consecutive points clearly off the boundary do.
pub(crate) fn longest_offring_run(ring: &[Point2f32], points: &[Point2f32], off_tol: f32) -> usize {
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
pub(crate) fn uncovered_perimeter_run(
    ring: &[Point2f32],
    points: &[Point2f32],
    cover_tol: f32,
) -> f32 {
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
pub(crate) fn outline_following_trace(
    ring: &[Point2f32],
    trace: &[Point2f32],
    follow_tol: f32,
    ortho_tol: f32,
    tuning: &SpliceTuning,
) -> Option<Vec<Point2f32>> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    // 10x10 square: 0:(0,0) 1:(10,0) 2:(10,10) 3:(0,10)
    fn unit_square() -> Vec<Point2f32> {
        vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)]
    }

    fn keeps(seq: &[SpliceElem]) -> Vec<usize> {
        seq.iter()
            .filter_map(|e| match e {
                SpliceElem::Keep(i) => Some(*i),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn snap_to_nearest_vertex_when_within_tol() {
        let ring = unit_square();
        let (edge, t, dist, is_vertex) = snap_to_polygon(&ring, p(10.5, 9.6), 2.0);
        assert_eq!(edge, 2);
        assert_eq!(t, 0.0);
        assert!(is_vertex);
        assert!(dist < 2.0);
    }

    #[test]
    fn snap_to_edge_when_far_from_any_vertex() {
        let ring = unit_square();
        let (edge, t, dist, is_vertex) = snap_to_polygon(&ring, p(5.0, -1.0), 2.0);
        assert_eq!(edge, 0);
        assert!((t - 0.5).abs() < 1e-3);
        assert!(!is_vertex);
        assert!((dist - 1.0).abs() < 1e-3);
    }

    #[test]
    fn same_edge_bump_keeps_all_original_vertices() {
        // Porch out of the top edge (edge 0). Attach points on edge 0; the
        // short arc between them has no interior vertices, so every original
        // vertex survives (the edge is just split).
        let ring = unit_square();
        let ortho = vec![p(3.0, 0.0), p(3.0, -4.0), p(7.0, -4.0), p(7.0, 0.0)];
        let seq = plan_splice_sequence(&ring, 0, 0.3, 0, 0.7, &ortho);
        let kept = keeps(&seq);
        assert_eq!(kept.len(), 4, "all four square corners should be preserved");
        for i in 0..4 {
            assert!(kept.contains(&i), "missing vertex {}", i);
        }
        // Two attach points + two porch-interior corners = 4 New slots.
        let new_count = seq
            .iter()
            .filter(|e| matches!(e, SpliceElem::New(_)))
            .count();
        assert_eq!(new_count, 4);
    }

    #[test]
    fn cross_edge_drops_spanned_corner() {
        // Chamfer the top-right corner: attach on edge 0 and edge 1. The short
        // arc contains vertex 1 (the corner at (10,0)), which must be dropped.
        let ring = unit_square();
        let ortho = vec![p(6.0, 0.0), p(6.0, 4.0), p(10.0, 4.0)];
        let seq = plan_splice_sequence(&ring, 0, 0.6, 1, 0.4, &ortho);
        let kept = keeps(&seq);
        assert!(!kept.contains(&1), "corner vertex 1 should be dropped");
        assert!(kept.contains(&2) && kept.contains(&3) && kept.contains(&0));
        assert_eq!(kept.len(), 3);
    }

    #[test]
    fn orthogonalize_open_path_keeps_endpoints() {
        // A roughly-orthogonal porch with slight wobble: out and back.
        let stroke = vec![
            p(3.0, 0.1),
            p(3.1, -2.0),
            p(2.9, -4.0),
            p(7.0, -4.1),
            p(7.1, -2.0),
            p(6.9, 0.0),
        ];
        let ortho = orthogonalize_open_path(&stroke, 0.5).expect("orthogonalize");
        assert!(ortho.len() >= 4, "expected a clean porch outline");
        // First and last stay near the stroke's endpoints (open path: no
        // closing-edge snap collapsing them).
        assert!((ortho.first().unwrap().x - 3.0).abs() < 1.5);
        assert!((ortho.last().unwrap().x - 7.0).abs() < 1.5);
    }
}
