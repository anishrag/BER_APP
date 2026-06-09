//! Building an upper storey from a freehand trace over the storey below.
//!
//! When the assessor draws an upper storey on top of the previous one, most of
//! it usually REPRODUCES the storey below, differing only over a short run (a
//! setback, an overhang, an extension). These helpers decide whether a trace is
//! a faithful reproduce and, if it's a partial one, build the new outline that
//! follows the boundary verbatim where the trace hugs it and replaces only the
//! diverging spans with orthogonalized new walls.
//!
//! (Future home for new-storey *proposal* logic too.) Point/stroke snapping onto
//! the previous outline lives in [`crate::snap`]; the ring-splice machinery in
//! [`crate::splice`].

use crate::{snap, SpliceTuning};
use domain::points::Point2f32;

fn dist2(a: Point2f32, b: Point2f32) -> f32 {
    (a.x - b.x).powi(2) + (a.y - b.y).powi(2)
}

/// A projected trace point: `(perimeter position, distance to boundary, raw
/// point, nearest edge index)`.
type TracePt = (f32, f32, Point2f32, usize);

/// Cumulative-length view of a ring, for mapping points to/from PERIMETER
/// positions (arc length from vertex 0). The shared spatial index the trace
/// analysis runs against.
struct Perimeter<'a> {
    ring: &'a [Point2f32],
    cum: Vec<f32>,  // arc length at each vertex
    elen: Vec<f32>, // each edge's length
    total: f32,
}

impl<'a> Perimeter<'a> {
    fn new(ring: &'a [Point2f32]) -> Option<Self> {
        let n = ring.len();
        if n < 3 {
            return None;
        }
        let elen: Vec<f32> = (0..n)
            .map(|i| dist2(ring[i], ring[(i + 1) % n]).sqrt())
            .collect();
        let mut cum = vec![0.0f32; n];
        for i in 1..n {
            cum[i] = cum[i - 1] + elen[i - 1];
        }
        let total = cum[n - 1] + elen[n - 1];
        (total > 1e-4).then_some(Self {
            ring,
            cum,
            elen,
            total,
        })
    }

    /// Project `p` onto the nearest edge → `(perimeter position, distance,
    /// nearest edge index)`. Uses [`snap::find_nearest`] for the snap.
    fn project(&self, p: Point2f32) -> (f32, f32, usize) {
        let (loc, d) = snap::find_nearest(self.ring, p, 0.0);
        let (e, t) = loc.edge_t();
        (self.cum[e] + t * self.elen[e], d, e)
    }

    /// Point on the ring at perimeter position `pos` (wraps).
    fn point_at(&self, pos: f32) -> Point2f32 {
        let n = self.ring.len();
        let pos = pos.rem_euclid(self.total);
        for i in 0..n {
            if pos >= self.cum[i] - 1e-4 && pos <= self.cum[i] + self.elen[i] + 1e-4 {
                let t = if self.elen[i] > 1e-6 {
                    ((pos - self.cum[i]) / self.elen[i]).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let (a, b) = (self.ring[i], self.ring[(i + 1) % n]);
                return Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
            }
        }
        self.ring[0]
    }

    /// Signed minimal circular delta `a → b`, in `[-total/2, total/2]`.
    fn fwd(&self, a: f32, b: f32) -> f32 {
        let mut d = b - a;
        while d > self.total * 0.5 {
            d -= self.total;
        }
        while d < -self.total * 0.5 {
            d += self.total;
        }
        d
    }

    /// Ring vertices strictly inside the forward arc `(p_from, p_to)`, in order,
    /// keeping only those `keep` accepts (a graze can make the arc span sweep
    /// past walls the trace never visited — those must be excluded).
    fn arc_vertices(
        &self,
        p_from: f32,
        p_to: f32,
        keep: impl Fn(Point2f32) -> bool,
    ) -> Vec<Point2f32> {
        let n = self.ring.len();
        let span = (p_to - p_from).rem_euclid(self.total);
        let mut v: Vec<(f32, Point2f32)> = Vec::new();
        for j in 0..n {
            let d = (self.cum[j] - p_from).rem_euclid(self.total);
            if d > 1e-4 && d < span - 1e-4 && keep(self.ring[j]) {
                v.push((d, self.ring[j]));
            }
        }
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        v.into_iter().map(|(_, p)| p).collect()
    }
}

/// Longest run of CONSECUTIVE `points` lying farther than `off_tol` from the
/// outline `ring` (cyclic — the trace is a closed loop). Used to tell a trace
/// that FOLLOWS the boundary (only isolated/short strays — hand slop) from one
/// with a SUSTAINED excursion such as an extension (a long run of points well
/// off the boundary). A run-of-points test is robust where a max-distance test
/// isn't: one deep stray near a curve doesn't look like a new wall, but several
/// consecutive points clearly off the boundary do.
pub(crate) fn longest_offring_run(ring: &[Point2f32], points: &[Point2f32], off_tol: f32) -> usize {
    let n = ring.len();
    let m = points.len();
    if n < 2 || m == 0 {
        return 0;
    }
    let off: Vec<bool> = points
        .iter()
        .map(|p| snap::find_nearest(ring, *p, 0.0).1 > off_tol)
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

/// Longest contiguous span (by arc length) of the outline `ring` that the trace
/// `points` did NOT follow — every point of it is farther than `cover_tol` from
/// the trace. Complements [`longest_offring_run`]: that catches a trace going
/// OUTSIDE the boundary (an extension); this catches one that SKIPS part of it
/// (stops early on a side, cuts a bulge off with a straight wall, overhangs a
/// cut-out). A reproduce must do neither. Cyclic; `0.0` when fully followed.
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
    let dist = |a: Point2f32, b: Point2f32| dist2(a, b).sqrt();
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

/// Classify each trace SEGMENT as following the boundary (`on[k]`) or diverging,
/// orienting the trace forward first and smoothing out brief "following" grazes.
///
/// Per-SEGMENT (not per-point): segment k→k+1 follows when both endpoints AND
/// its midpoint hug the boundary — robust to sampling density, where a per-point
/// "smooth advance" test is not. A STRAIGHT wall is followed only within
/// `follow_tol * straight_cling_factor` (so a roughly-straight wall drawn a
/// little off keeps its own line); a CURVE facet uses the full `follow_tol`.
/// Returns the (possibly reversed + rotated) projected points and the per-segment
/// flags, aligned.
fn classify_following(
    perim: &Perimeter,
    trace: &[Point2f32],
    follow_tol: f32,
    tuning: &SpliceTuning,
) -> (Vec<TracePt>, Vec<bool>) {
    let ring = perim.ring;
    let n = ring.len();
    let tol_at = |e: usize| -> f32 {
        let (a, b) = (ring[e], ring[(e + 1) % n]);
        let straight = (a.x - b.x).abs() < 1e-3 || (a.y - b.y).abs() < 1e-3;
        if straight {
            follow_tol * tuning.straight_cling_factor
        } else {
            follow_tol
        }
    };
    let mut sn: Vec<TracePt> = trace
        .iter()
        .map(|&p| {
            let (pos, d, e) = perim.project(p);
            (pos, d, p, e)
        })
        .collect();
    let m = sn.len();
    // Orient the trace so it advances FORWARD along the boundary.
    let mut net = 0.0;
    for k in 0..m - 1 {
        if sn[k].1 <= tol_at(sn[k].3) && sn[k + 1].1 <= tol_at(sn[k + 1].3) {
            net += perim.fwd(sn[k].0, sn[k + 1].0);
        }
    }
    if net < 0.0 {
        sn.reverse();
    }
    // Per-segment following flags.
    let mut on = vec![false; m];
    for k in 0..m {
        if sn[k].1 > tol_at(sn[k].3) || sn[(k + 1) % m].1 > tol_at(sn[(k + 1) % m].3) {
            continue;
        }
        let (a, b) = (sn[k].2, sn[(k + 1) % m].2);
        let mid = Point2f32::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
        let (_, md, me) = perim.project(mid);
        on[k] = md <= tol_at(me);
    }
    // Smooth out brief FOLLOWING grazes: a SHORT run of "following" segments
    // flanked by diverging segments is a new wall momentarily running near the
    // boundary, NOT a real shared wall. Left split it breaks one new wall into
    // two; flip it to diverging so the wall stays one piece. (Rotate a free
    // segment to index 0 first so no following run wraps the seam.)
    if let Some(f0) = (0..m).find(|&k| !on[k]) {
        sn.rotate_left(f0);
        on.rotate_left(f0);
        let graze_max = follow_tol * tuning.graze_smoothing_factor;
        let mut i = 0;
        while i < m {
            if on[i] {
                let s = i;
                let mut e = i;
                while e + 1 < m && on[e + 1] {
                    e += 1;
                }
                let len: f32 = (s..=e)
                    .map(|k| dist2(sn[k].2, sn[(k + 1) % m].2).sqrt())
                    .sum();
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
    (sn, on)
}

/// Maximal alternating runs of equal `on` flags: `(is_following, start, end)`.
fn alternating_runs(on: &[bool]) -> Vec<(bool, usize, usize)> {
    let m = on.len();
    let mut runs = Vec::new();
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
    runs
}

/// Square off the NEW walls. A free run emitted between two boundary anchors
/// that DON'T share a coordinate comes out slightly tilted; left tilted,
/// dimensioning a neighbour later flips its lean. Snap a nearly-axis-aligned
/// edge onto its axis — but NEVER a curve facet (both endpoints tagged
/// `is_curve`), and only when the edge is clearly axis-aligned (high aspect
/// ratio), so a short diagonal facet at a curve's tangent is left alone.
fn square_off_new_walls(out: &mut [(Point2f32, bool)], tuning: &SpliceTuning) {
    if out.is_empty() {
        return;
    }
    let (mut lo, mut hi) = (out[0].0, out[0].0);
    for (p, _) in out.iter() {
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
        } else if dx < axis_tol && dy < axis_tol {
            // A tiny diagonal JUNCTION connector (both deltas sub-tolerance) where
            // a followed-boundary anchor meets a squared new wall but the two
            // didn't quite line up. It can't read as horizontal OR vertical above,
            // so square it to the nearer axis — otherwise it leaves a small
            // non-orthogonal seam in a footprint that should be all right angles.
            if dx <= dy {
                let mx = (a.x + b.x) * 0.5; // closer to vertical → share x
                out[i].0.x = mx;
                out[j].0.x = mx;
            } else {
                let my = (a.y + b.y) * 0.5; // closer to horizontal → share y
                out[i].0.y = my;
                out[j].0.y = my;
            }
        }
    }
}

/// Build a new-storey outline that FOLLOWS `ring` where the trace hugs it —
/// copying the boundary VERBATIM, curve facets and all — and replaces ONLY the
/// spans where the trace diverges with orthogonalized new walls. The
/// partial-reproduce case: a storey mostly like the one below but differing over
/// a short run (a setback, an overhang). Without it, any divergence forces the
/// WHOLE trace through orthogonalization, squaring off carefully traced curves.
///
/// `trace` is the RAW (un-snapped) freehand outline in `ring`'s metre space — so
/// a diverging wall keeps its drawn position. Returns `None` (caller falls back
/// to a whole-trace orthogonalize) when the trace doesn't split into a clean
/// follow/diverge alternation (all-following is a pure reproduce; none-following
/// is a wholly new shape; neither belongs here).
pub(crate) fn outline_following_trace(
    ring: &[Point2f32],
    trace: &[Point2f32],
    follow_tol: f32,
    ortho_tol: f32,
    tuning: &SpliceTuning,
) -> Option<Vec<Point2f32>> {
    if ring.len() < 3 || trace.len() < 4 {
        return None;
    }
    let perim = Perimeter::new(ring)?;
    let (mut sn, mut on) = classify_following(&perim, trace, follow_tol, tuning);
    let m = sn.len();
    let n_follow = on.iter().filter(|&&f| f).count();
    log::debug!(
        "[new_storey] {m} trace segs: {n_follow} follow the boundary, {} diverge \
             (follow_tol {follow_tol:.3} m)",
        m - n_follow
    );
    // Need BOTH a following and a free segment to splice; else this isn't ours
    // (all-on is a pure reproduce; none-on is a wholly new shape).
    if !on.iter().any(|&f| f) || on.iter().all(|&f| f) {
        log::debug!(
            "[new_storey] declined: {}",
            if n_follow == 0 {
                "no segment follows the boundary (wholly new shape)"
            } else {
                "every segment follows the boundary (a reproduce)"
            }
        );
        return None;
    }
    // Rotate so segment 0 starts a following run (its point preceded by a free
    // segment), so runs alternate starting on and ending free.
    let start = match (0..m).find(|&k| on[k] && !on[(k + m - 1) % m]) {
        Some(s) => s,
        None => {
            log::debug!("[new_storey] declined: no free→follow boundary found");
            return None;
        }
    };
    sn.rotate_left(start);
    on.rotate_left(start);
    let runs = alternating_runs(&on);

    // A following run emits the ring's own vertices over its arc (curve facets
    // verbatim, `is_curve = true`); a free run's interior points are the new
    // wall — orthogonalized, anchored to the ring points where the trace left
    // and rejoins. Only vertices the trace actually VISITED are emitted.
    let ft2 = follow_tol * follow_tol;
    let visited = |q: Point2f32| sn.iter().any(|s| dist2(s.2, q) <= ft2);
    let mut out: Vec<(Point2f32, bool)> = Vec::new();
    for &(f, sa, sb) in &runs {
        if f {
            let (p_from, p_to) = (sn[sa].0, sn[(sb + 1) % m].0);
            out.push((perim.point_at(p_from), false));
            out.extend(
                perim
                    .arc_vertices(p_from, p_to, &visited)
                    .into_iter()
                    .map(|p| (p, true)),
            );
            out.push((perim.point_at(p_to), false));
        } else {
            let prev_pt = out
                .last()
                .map(|(p, _)| *p)
                .unwrap_or(perim.point_at(sn[sa].0));
            let next_pt = perim.point_at(sn[(sb + 1) % m].0);
            let mut seg = Vec::with_capacity(sb - sa + 2);
            seg.push(prev_pt);
            for k in (sa + 1)..=sb {
                seg.push(sn[k].2);
            }
            seg.push(next_pt);
            // Orthogonalize this new wall in isolation. If it can't be (too
            // few/degenerate points), connect the anchors straight — ONE bad
            // run must not abort the whole splice.
            let ortho = crate::orthogonal::orthogonalize_open_path(&seg, ortho_tol)
                .unwrap_or_else(|_| vec![prev_pt, next_pt]);
            // Interior vertices only — the endpoints belong to the arcs.
            let upto = ortho.len().saturating_sub(1);
            for p in ortho.iter().take(upto).skip(1) {
                out.push((*p, false));
            }
        }
    }
    square_off_new_walls(&mut out, tuning);

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
            "[new_storey] declined: spliced ring collapsed to {} verts",
            cleaned.len()
        );
        return None;
    }
    // Re-root the ring so vertex 0 — the parametric model's fixed CONSTANT
    // origin — lands on a vertex SHARED with the previous outline (a genuinely
    // fixed corner), never a NEW-WALL junction (which sits mid-shared-wall and
    // would freeze as an un-dimensionable constant). Same intent as
    // `build_region`'s corner-anchor step.
    if let Some(rk) = cleaned
        .iter()
        .position(|(p, _)| ring.iter().any(|o| dist2(*o, *p) < 1e-4))
    {
        cleaned.rotate_left(rk);
    }
    let cleaned: Vec<Point2f32> = cleaned.into_iter().map(|(p, _)| p).collect();
    // Final safety net: square off any junction tilt that `square_off_new_walls`'
    // single-pass snapping left behind (snapping one seam can un-square a
    // neighbour). This squares only edges that are tilted AND NOT on `ring`, so
    // followed edges and curve facets (which sit verbatim on the boundary) are
    // kept; the new storey over an axis-aligned previous comes out fully
    // orthogonal. Same pass `build_region` uses.
    let cleaned = crate::geom::orthogonalize_off_boundary(&cleaned, ring, 0.003);
    log::debug!(
        "[new_storey] built {} verts from {} runs ({} following, {} new-wall)",
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

    /// Partial reproduce: a 10×10 storey traced again, but with the TOP wall
    /// pulled down to y=8 over the middle (a setback). The result follows the
    /// other three walls and keeps the setback notch.
    #[test]
    fn partial_reproduce_keeps_a_setback() {
        let ring = vec![p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
        let trace = vec![
            p(0., 0.),
            p(5., 0.),
            p(10., 0.),
            p(10., 5.),
            p(10., 10.),
            p(7., 10.),
            p(7., 8.), // setback in
            p(3., 8.), // setback floor
            p(3., 10.),
            p(0., 10.),
            p(0., 5.),
        ];
        let out = outline_following_trace(&ring, &trace, 0.5, 0.3, &SpliceTuning::default())
            .expect("partial reproduce should splice");
        // the three followed walls + the setback notch
        assert_eq!(out.len(), 8, "{out:?}");
        // the setback floor sits at y=8, between two corners
        assert_eq!(
            out.iter().filter(|v| (v.y - 8.0).abs() < 1e-3).count(),
            2,
            "two setback corners at y=8: {out:?}"
        );
        // the bottom-right corner of the storey is reproduced verbatim
        assert!(
            out.iter()
                .any(|v| (v.x - 10.0).abs() < 1e-3 && v.y.abs() < 1e-3),
            "kept corner (10,0): {out:?}"
        );
    }

    /// A trace that hugs the boundary all the way round (a pure reproduce) is
    /// declined — there's nothing to splice.
    #[test]
    fn full_reproduce_declines() {
        let ring = vec![p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)];
        let trace = vec![
            p(0., 0.),
            p(5., 0.05),
            p(10., 0.),
            p(10., 5.),
            p(10., 10.),
            p(5., 9.95),
            p(0., 10.),
            p(0., 5.),
        ];
        assert!(
            outline_following_trace(&ring, &trace, 0.5, 0.3, &SpliceTuning::default()).is_none()
        );
    }
}

#[cfg(test)]
mod square_off_tests {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }
    /// A tiny diagonal junction (both deltas sub-tolerance) where a followed
    /// anchor meets a squared new wall must be squared, not left as a seam.
    #[test]
    fn squares_a_tiny_diagonal_junction() {
        // Quad with a small diagonal on edge 0 (the seam from the field log).
        let mut out = vec![
            (p(0.236, 2.321), false),
            (p(0.259, 2.331), false),
            (p(0.259, 0.401), false),
            (p(0.236, 0.401), false),
        ];
        square_off_new_walls(&mut out, &SpliceTuning::default());
        // Edge 0 is now axis-aligned (shares one coordinate).
        let (a, b) = (out[0].0, out[1].0);
        let axis_aligned = (a.x - b.x).abs() < 1e-4 || (a.y - b.y).abs() < 1e-4;
        assert!(axis_aligned, "tiny diagonal junction squared: {a:?}->{b:?}");
    }
}

#[cfg(test)]
mod new_storey_orthogonal_regression {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }
    /// Field case: tracing an upper storey over a ~2×2 m ground floor produced a
    /// footprint with a NON-ORTHOGONAL seam where a followed-boundary run met a
    /// squared new wall. A new storey over an axis-aligned storey must come out
    /// fully orthogonal. (Reproduces the exact raw trace + previous ring.)
    #[test]
    fn traced_new_storey_is_fully_orthogonal() {
        // Previous storey: a 2×2 m square (world/metre coords from the log).
        let ring = vec![
            p(0.236, 0.321),
            p(2.236, 0.321),
            p(2.236, 2.321),
            p(0.236, 2.321),
        ];
        // The raw freehand trace (137 pts) as captured.
        let trace = vec![
            p(0.261, 0.362),
            p(0.261, 0.362),
            p(0.261, 0.420),
            p(0.261, 0.466),
            p(0.261, 0.521),
            p(0.261, 0.629),
            p(0.261, 0.715),
            p(0.261, 0.802),
            p(0.261, 0.869),
            p(0.261, 0.934),
            p(0.261, 0.992),
            p(0.261, 1.056),
            p(0.261, 1.104),
            p(0.248, 1.177),
            p(0.241, 1.265),
            p(0.233, 1.352),
            p(0.226, 1.431),
            p(0.220, 1.561),
            p(0.220, 1.637),
            p(0.220, 1.713),
            p(0.220, 1.800),
            p(0.234, 1.879),
            p(0.234, 1.955),
            p(0.248, 2.032),
            p(0.248, 2.106),
            p(0.248, 2.173),
            p(0.248, 2.215),
            p(0.248, 2.252),
            p(0.248, 2.292),
            p(0.248, 2.324),
            p(0.259, 2.372),
            p(0.265, 2.427),
            p(0.271, 2.469),
            p(0.282, 2.495),
            p(0.282, 2.500),
            p(0.282, 2.500),
            p(0.282, 2.500),
            p(0.282, 2.496),
            p(0.282, 2.496),
            p(0.287, 2.466),
            p(0.310, 2.426),
            p(0.321, 2.393),
            p(0.331, 2.362),
            p(0.347, 2.346),
            p(0.347, 2.341),
            p(0.347, 2.341),
            p(0.352, 2.341),
            p(0.377, 2.341),
            p(0.448, 2.341),
            p(0.557, 2.364),
            p(0.983, 2.437),
            p(1.350, 2.489),
            p(1.728, 2.521),
            p(2.064, 2.531),
            p(2.305, 2.531),
            p(2.405, 2.531),
            p(2.448, 2.514),
            p(2.464, 2.498),
            p(2.464, 2.487),
            p(2.474, 2.487),
            p(2.485, 2.487),
            p(2.485, 2.487),
            p(2.485, 2.487),
            p(2.485, 2.483),
            p(2.485, 2.483),
            p(2.485, 2.473),
            p(2.485, 2.441),
            p(2.469, 2.415),
            p(2.427, 2.385),
            p(2.376, 2.353),
            p(2.302, 2.333),
            p(2.233, 2.326),
            p(2.185, 2.314),
            p(2.140, 2.314),
            p(2.135, 2.314),
            p(2.135, 2.314),
            p(2.135, 2.314),
            p(2.135, 2.309),
            p(2.135, 2.273),
            p(2.141, 2.209),
            p(2.170, 2.122),
            p(2.219, 2.016),
            p(2.271, 1.877),
            p(2.322, 1.672),
            p(2.363, 1.479),
            p(2.413, 1.178),
            p(2.423, 1.061),
            p(2.423, 1.044),
            p(2.423, 1.028),
            p(2.423, 0.950),
            p(2.423, 0.798),
            p(2.423, 0.670),
            p(2.439, 0.557),
            p(2.439, 0.488),
            p(2.439, 0.438),
            p(2.439, 0.417),
            p(2.439, 0.401),
            p(2.439, 0.401),
            p(2.439, 0.401),
            p(2.429, 0.401),
            p(2.429, 0.401),
            p(2.393, 0.396),
            p(2.361, 0.385),
            p(2.351, 0.364),
            p(2.345, 0.348),
            p(2.335, 0.343),
            p(2.335, 0.338),
            p(2.335, 0.338),
            p(2.335, 0.338),
            p(2.330, 0.338),
            p(2.330, 0.338),
            p(2.316, 0.338),
            p(2.271, 0.338),
            p(2.199, 0.338),
            p(2.101, 0.361),
            p(2.019, 0.375),
            p(1.888, 0.404),
            p(1.765, 0.437),
            p(1.583, 0.465),
            p(1.356, 0.486),
            p(1.093, 0.486),
            p(0.820, 0.475),
            p(0.626, 0.455),
            p(0.535, 0.455),
            p(0.485, 0.449),
            p(0.445, 0.443),
            p(0.406, 0.443),
            p(0.368, 0.443),
            p(0.313, 0.443),
            p(0.281, 0.443),
            p(0.250, 0.443),
            p(0.218, 0.443),
            p(0.187, 0.443),
            p(0.166, 0.443),
            p(0.155, 0.443),
            p(0.155, 0.443),
            p(0.155, 0.443),
        ];
        let out = outline_following_trace(&ring, &trace, 0.141, 0.050, &SpliceTuning::default())
            .expect("the trace decomposes into a partial-reproduce splice");
        let n = out.len();
        let non_ortho: Vec<_> = (0..n)
            .filter(|&i| {
                let (a, b) = (out[i], out[(i + 1) % n]);
                (a.x - b.x).abs() >= 1e-3 && (a.y - b.y).abs() >= 1e-3
            })
            .collect();
        assert!(
            non_ortho.is_empty(),
            "new-storey outline must be fully orthogonal; non-ortho edges at {non_ortho:?} in {out:?}"
        );
    }
}
