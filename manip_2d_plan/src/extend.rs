//! Stroke-vs-outline analysis for the Extend / new-storey "draw on top of the
//! previous storey" flows.
//!
//! - [`snap_stroke_to_ring`] pulls a freehand stroke's points onto the previous
//!   outline so a new storey's footprint reuses those coordinates verbatim.
//! - [`longest_offring_run`] / [`uncovered_perimeter_run`] / [`outline_following_trace`]
//!   decide whether (and how) an upper-storey trace reproduces the storey below.
//!
//! The ring-splice machinery these flows ultimately feed lives in
//! [`crate::splice`]; pure point/edge helpers live in [`crate::geom`].

use crate::{geom, SpliceTuning};
use domain::points::Point2f32;

/// Snap each point of `stroke` onto `ring`'s perimeter when within `snap_tol`,
/// leaving points farther away untouched. Used by the multi-storey "draw on
/// top of the previous storey" flow so endpoints land exactly on the previous
/// outline (so the new storey's footprint reuses those coordinates verbatim).
/// `geom::snap_to_polygon` already handles closest-vertex vs closest-edge — we
/// just project to the returned `(edge, t)` when the gap is within tolerance.
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
            let (edge, t, dist, on_vertex) = geom::snap_to_polygon(ring, *p, snap_tol);
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

// ── New-storey reproduce check ──────────────────────────────────────────────
// Does a freehand trace of an upper storey faithfully follow the storey below?
// Pure geometry over the previous outline ring; the new-storey flow uses these
// alongside the splice in `crate::splice`. Exposed via thin FloorPlan facades
// (see lib.rs).
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
    log::debug!(
        "[splice] {m} trace segs: {n_follow} follow the boundary, {} diverge \
             (follow_tol {follow_tol:.3} m)",
        m - n_follow
    );
    // Need BOTH a following and a free segment to splice; else this isn't
    // ours (all-on is a pure reproduce; none-on is a wholly new shape).
    if !on.iter().any(|&f| f) || on.iter().all(|&f| f) {
        log::debug!(
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
            log::debug!("[splice] declined: no free→follow boundary found");
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
            let ortho = crate::orthogonal::orthogonalize_open_path(&seg, ortho_tol)
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
        log::debug!(
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
    // as `build_region`'s corner-anchor step.)
    if let Some(rk) = cleaned
        .iter()
        .position(|(p, _)| ring.iter().any(|o| dist2(*o, *p) < 1e-4))
    {
        cleaned.rotate_left(rk);
    }
    let cleaned: Vec<Point2f32> = cleaned.into_iter().map(|(p, _)| p).collect();
    log::debug!(
        "[splice] built {} verts from {} runs ({} following, {} new-wall)",
        cleaned.len(),
        runs.len(),
        runs.iter().filter(|r| r.0).count(),
        runs.iter().filter(|r| !r.0).count(),
    );
    Some(cleaned)
}
