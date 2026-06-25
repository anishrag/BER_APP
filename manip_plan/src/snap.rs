//! Snapping points and strokes ONTO a polygon's boundary.
//!
//! Two phases, kept separate:
//! - **FIND** — [`find_nearest`] locates the nearest boundary spot as an
//!   [`OnRing`] (a *vertex* or a *point along an edge*) plus its distance. It's
//!   pure — it applies nothing.
//! - **APPLY** — resolve that [`OnRing`] to a coordinate ([`OnRing::point`]) or,
//!   for an optional move, route it through [`elective_snap`] (which records it).
//!
//! `OnRing` carries the STRUCTURAL fact (which vertex / which edge + how far
//! along), so a within-plan consumer can preserve parametric identity (reuse the
//! vertex, splice into the edge), while a cross-ring consumer projects it to a
//! raw coordinate. They build on [`crate::geom`]'s coordinate math; callers
//! across extend, trim, carve, and the new-storey flow share them.

use crate::geom;
use domain::points::Point2f32;

/// What an ELECTIVE snap snapped to. An elective snap is one taken because a
/// nearby feature was a BETTER target, when staying put was also valid — as
/// opposed to a FORCED snap (e.g. clamping a point that fell OUTSIDE the outline
/// back onto it), which has no alternative and is NOT routed through here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapKind {
    /// A point snapped onto a polygon VERTEX instead of staying on the edge.
    ToVertex,
    /// A point attached at the nearest point ALONG an edge — the less-aggressive
    /// alternative to snapping onto the corner.
    ToEdge,
    /// A point snapped onto a nearby CORNER (it could have stayed where drawn).
    ToCorner,
    /// A wall snapped onto a PARALLEL boundary wall it runs alongside.
    ToParallelWall,
    /// A stroke point snapped onto the PREVIOUS storey's outline.
    ToPreviousOutline,
    /// A stroke point snapped onto a REFERENCE segment supplied from outside the
    /// plan (e.g. an interior roof-region division or a floor-above wall base the
    /// ceiling carve should follow), not part of the carve's boundary ring.
    ToReferenceSegment,
}

/// One option an elective snap considered: a `kind`, the point it would move to,
/// and a `score` (lower = closer / less of a reach). Candidates are ranked
/// most-aggressive-first; the chosen one is the most aggressive that passed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SnapCandidate {
    pub kind: SnapKind,
    pub to: Point2f32,
    pub score: f32,
}

/// One recorded elective-snap decision: where the point started, the candidates
/// considered (most-aggressive first), and which one was applied — `Some(i)` for
/// candidate `i`, or `None` for "didn't snap" (reverted to `from`). The shell
/// surfaces these as canvas tags; choosing a different candidate (or `None`)
/// replays the action with that choice forced.
#[derive(Debug, Clone)]
pub struct SnapDecision {
    pub from: Point2f32,
    pub candidates: Vec<SnapCandidate>,
    pub chosen: Option<usize>,
    /// Whether "don't snap" is a valid outcome. `false` for a MANDATORY attach (an
    /// extend/trim endpoint must join the outline — the least it can do is land on
    /// the edge, never float); `true` for an optional snap (a hug-to-corner /
    /// parallel-wall alignment, where staying where drawn is fine).
    pub skippable: bool,
}

/// A forced snap choice for the replay policy: apply candidate `i`, or `Skip`
/// (don't snap, revert to the drawn point). Keyed by the decision's `from` point.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SnapChoice {
    Apply(usize),
    Skip,
}

thread_local! {
    /// Forced choices for the current replay, keyed by the decision's drawn point.
    /// Empty for a normal (non-replay) action. Set by the shell before re-running.
    static SNAP_POLICY: std::cell::RefCell<Vec<(Point2f32, SnapChoice)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Install the forced-choice policy for a replay (cleared with [`clear_snap_policy`]).
pub fn set_snap_policy(policy: Vec<(Point2f32, SnapChoice)>) {
    SNAP_POLICY.with(|p| *p.borrow_mut() = policy);
}

/// Drop the forced-choice policy (back to normal snapping).
pub fn clear_snap_policy() {
    SNAP_POLICY.with(|p| p.borrow_mut().clear());
}

/// The forced choice for a decision at `from`, if the policy has one (matched
/// within a small tolerance — the drawn point is stable across replays).
fn policy_for(from: Point2f32) -> Option<SnapChoice> {
    SNAP_POLICY.with(|p| {
        p.borrow()
            .iter()
            .find(|(f, _)| (f.x - from.x).abs() < 1e-3 && (f.y - from.y).abs() < 1e-3)
            .map(|(_, c)| *c)
    })
}

thread_local! {
    static SNAP_DECISIONS: std::cell::RefCell<Vec<SnapDecision>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain the elective-snap decisions recorded since the last drain. The shell
/// drains BEFORE an action (clear stale) and again AFTER, to tag that action's
/// snaps. Mirrors [`crate::take_splice_debug`].
pub fn take_snap_decisions() -> Vec<SnapDecision> {
    SNAP_DECISIONS.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

/// Record an elective-snap decision and return the chosen point. The single
/// place a snap is both APPLIED and CAPTURED — `candidates[chosen].to` is the
/// result. `candidates` are most-aggressive-first.
/// The policy-resolved choice for a decision of `n` candidates whose default pick
/// is `default`: a forced candidate, a forced skip, or the default. A `Skip` on a
/// NON-skippable decision can't float — it falls to the least-aggressive candidate
/// (the last, since candidates are most-aggressive-first).
fn resolve(from: Point2f32, n: usize, default: usize, skippable: bool) -> Option<usize> {
    match policy_for(from) {
        Some(SnapChoice::Apply(i)) if i < n => Some(i),
        Some(SnapChoice::Skip) if skippable => None,
        Some(SnapChoice::Skip) => Some(n - 1),
        _ => Some(default),
    }
}

/// Log + store one resolved decision (the single place a `SnapDecision` is born).
fn commit(from: Point2f32, candidates: Vec<SnapCandidate>, chosen: Option<usize>, skippable: bool) {
    let to = chosen.map_or(from, |i| candidates[i].to);
    log::info!(
        "[snap] {:?}: ({:.3}, {:.3}) -> ({:.3}, {:.3})  [chosen {:?} of {}]",
        chosen.map(|i| candidates[i].kind),
        from.x,
        from.y,
        to.x,
        to.y,
        chosen,
        candidates.len()
    );
    SNAP_DECISIONS.with(|d| {
        d.borrow_mut().push(SnapDecision {
            from,
            candidates,
            chosen,
            skippable,
        });
    });
}

/// Record a (typically single-candidate) decision and return the applied point —
/// or `from` if the policy skipped a skippable one. The default pick is `default`.
pub(crate) fn record_choice(
    from: Point2f32,
    candidates: Vec<SnapCandidate>,
    default: usize,
    skippable: bool,
) -> Point2f32 {
    let chosen = resolve(from, candidates.len(), default, skippable);
    let to = chosen.map_or(from, |i| candidates[i].to);
    commit(from, candidates, chosen, skippable);
    to
}

/// A located point on a ring's boundary — a vertex, or a position along an edge.
/// The structural fact a snap targets: [`find_nearest`] produces it; a within-plan
/// consumer reads it as IDENTITY (`Vertex(i)` → reuse vertex `i`; `Edge` → splice
/// into that edge), a cross-ring consumer projects it to a coordinate with
/// [`OnRing::point`]. The index is only meaningful for the ring it was found in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum OnRing {
    /// Ring vertex `i`.
    Vertex(usize),
    /// A point `t ∈ [0, 1]` along edge `edge` (from vertex `edge` to `edge + 1`).
    Edge { edge: usize, t: f32 },
}

impl OnRing {
    /// Resolve to the coordinate on `ring` — the lossy projection that DROPS the
    /// structural identity. Use when you want a raw point (e.g. a cross-ring
    /// consumer reusing the previous storey's wall position).
    pub(crate) fn point(self, ring: &[Point2f32]) -> Point2f32 {
        let n = ring.len();
        match self {
            OnRing::Vertex(i) => ring[i % n],
            OnRing::Edge { edge, t } => {
                let a = ring[edge % n];
                let b = ring[(edge + 1) % n];
                Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
            }
        }
    }

    /// As an `(edge, t)` attach point for splicing — a vertex `i` is the START of
    /// edge `i` (`t = 0`), matching the splice machinery's convention.
    pub(crate) fn edge_t(self) -> (usize, f32) {
        match self {
            OnRing::Vertex(i) => (i, 0.0),
            OnRing::Edge { edge, t } => (edge, t),
        }
    }

    /// Did the snap land on an existing CORNER (vs mid-edge)?
    pub(crate) fn is_vertex(self) -> bool {
        matches!(self, OnRing::Vertex(_))
    }
}

/// The choke point a single-option elective snap routes through: move `from` →
/// `to` because of a nearby feature, when staying at `from` was also valid.
/// Records it as a one-candidate [`SnapDecision`] and returns `to`. Sites that
/// can offer alternatives build a candidate list and call [`record_choice`]
/// directly.
pub(crate) fn elective_snap(
    kind: SnapKind,
    from: Point2f32,
    to: Point2f32,
    skippable: bool,
) -> Point2f32 {
    let dx = to.x - from.x;
    let dy = to.y - from.y;
    record_choice(
        from,
        vec![SnapCandidate {
            kind,
            to,
            score: (dx * dx + dy * dy).sqrt(),
        }],
        0,
        skippable,
    )
}

/// How AGGRESSIVE a snap kind is (lower = more aggressive — imposes more on the
/// drawn point). Snapping onto a CORNER is the strongest (it both moves the point
/// AND reuses a vertex's identity); the previous-outline / parallel-wall snaps
/// move it onto a line. Used to rank candidates so the most aggressive that
/// passed its threshold is chosen by default.
fn aggressiveness(kind: SnapKind) -> u8 {
    match kind {
        SnapKind::ToVertex | SnapKind::ToCorner => 0,
        SnapKind::ToEdge => 1,
        SnapKind::ToPreviousOutline | SnapKind::ToReferenceSegment => 2,
        SnapKind::ToParallelWall => 3,
    }
}

/// Choose among several elective candidates: sort most-aggressive-first (ties
/// broken by closeness), apply the forced-choice policy (default = the top),
/// record the decision (alternatives preserved for the UI), and return the CHOSEN
/// candidate — or `None` if the policy skipped the snap. Empty `candidates` is a
/// programming error: a site with nothing to offer must not call this.
pub(crate) fn elective_choice(
    from: Point2f32,
    mut candidates: Vec<SnapCandidate>,
    skippable: bool,
) -> Option<SnapCandidate> {
    candidates.sort_by(|a, b| {
        aggressiveness(a.kind)
            .cmp(&aggressiveness(b.kind))
            .then(a.score.total_cmp(&b.score))
    });
    let chosen = resolve(from, candidates.len(), 0, skippable);
    let result = chosen.map(|i| candidates[i]);
    commit(from, candidates, chosen, skippable);
    result
}

/// Enumerate the ring VERTICES within `tol` of `p` as `ToCorner` candidates,
/// nearest-first — the alternatives when a hug-point could reach more than one
/// corner. Empty when none are in reach.
fn find_corner_candidates(ring: &[Point2f32], p: Point2f32, tol: f32) -> Vec<SnapCandidate> {
    let mut cands: Vec<SnapCandidate> = ring
        .iter()
        .filter_map(|&v| {
            let d = ((v.x - p.x).powi(2) + (v.y - p.y).powi(2)).sqrt();
            (d <= tol).then_some(SnapCandidate {
                kind: SnapKind::ToCorner,
                to: v,
                score: d,
            })
        })
        .collect();
    cands.sort_by(|a, b| a.score.total_cmp(&b.score));
    cands
}

/// FIND the nearest boundary location to `p`: the closest vertex if one is within
/// `vertex_tol` (→ `Vertex`), else the closest point on the closest edge (→
/// `Edge`, `t` clamped to `[0, 1]`). Returns the location + its distance. PURE —
/// no snap is applied; the caller decides, routing elective applies through
/// [`elective_snap`].
pub(crate) fn find_nearest(ring: &[Point2f32], p: Point2f32, vertex_tol: f32) -> (OnRing, f32) {
    let n = ring.len();
    if n < 2 {
        return (OnRing::Vertex(0), f32::INFINITY);
    }
    // Closest vertex.
    let mut closest_v: (usize, f32) = (0, f32::INFINITY);
    for i in 0..n {
        let dx = ring[i].x - p.x;
        let dy = ring[i].y - p.y;
        let d = (dx * dx + dy * dy).sqrt();
        if d < closest_v.1 {
            closest_v = (i, d);
        }
    }
    if closest_v.1 <= vertex_tol {
        return (OnRing::Vertex(closest_v.0), closest_v.1);
    }
    // Otherwise closest point on an edge.
    let mut best: (usize, f32, f32) = (0, 0.0, f32::INFINITY);
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        if (a.x - b.x).powi(2) + (a.y - b.y).powi(2) < 1e-6 {
            continue; // degenerate edge
        }
        let (_q, t, dist) = geom::project_point_to_segment(p, a, b);
        if dist < best.2 {
            best = (i, t.clamp(0.0, 1.0), dist);
        }
    }
    (
        OnRing::Edge {
            edge: best.0,
            t: best.1,
        },
        best.2,
    )
}

/// The closest edge `p` can drop a PERPENDICULAR onto — the foot landing strictly
/// WITHIN the segment (an "existing part" of the wall, never clamped to an end) and
/// within `tol`. Returns `(edge, t, perp_dist)`, or `None` when no wall admits a
/// perpendicular foot in range. Ranks by perpendicular distance, so a point beyond
/// a near wall's end falls through to the next wall it genuinely lines up with.
fn find_edge_snap(ring: &[Point2f32], p: Point2f32, tol: f32) -> Option<(usize, f32, f32)> {
    let n = ring.len();
    let mut best: Option<(usize, f32, f32)> = None;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        if (a.x - b.x).powi(2) + (a.y - b.y).powi(2) < 1e-6 {
            continue; // degenerate edge
        }
        let (_q, t, dist) = geom::project_point_to_segment(p, a, b);
        if !(0.0..=1.0).contains(&t) {
            continue; // perpendicular foot lies OFF the segment — not this wall
        }
        if dist > tol {
            continue; // too far to project onto
        }
        if best.is_none_or(|(_, _, bd)| dist < bd) {
            best = Some((i, t, dist));
        }
    }
    best
}

/// Resolve a splice ATTACH location into an elective choice between snapping to a
/// CORNER and dropping a PERPENDICULAR onto a nearby wall. `found`/`found_dist` is
/// the site's `find_nearest` result (reach already checked). When `found` is a
/// vertex (a corner is in reach), the edge alternative is the closest wall `p` can
/// project perpendicularly onto within `edge_tol` ([`find_edge_snap`]); if one
/// exists, records a `ToVertex`-vs-`ToEdge` decision and returns the CHOSEN
/// `OnRing`. With no feasible wall the corner is the only option, so nothing is
/// recorded (no choice to offer). A non-vertex `found` is already a perpendicular
/// edge attach — returned untouched.
pub(crate) fn choose_attach(
    ring: &[Point2f32],
    p: Point2f32,
    found: OnRing,
    found_dist: f32,
    edge_tol: f32,
) -> OnRing {
    let OnRing::Vertex(vi) = found else {
        return found; // mid-edge attach — already a perpendicular projection
    };
    let Some((e_edge, e_t, e_dist)) = find_edge_snap(ring, p, edge_tol) else {
        return OnRing::Vertex(vi); // no wall to project onto → corner only, no tag
    };
    let edge_loc = OnRing::Edge {
        edge: e_edge,
        t: e_t,
    };
    let chosen = elective_choice(
        p,
        vec![
            SnapCandidate {
                kind: SnapKind::ToVertex,
                to: ring[vi],
                score: found_dist,
            },
            SnapCandidate {
                kind: SnapKind::ToEdge,
                to: edge_loc.point(ring),
                score: e_dist,
            },
        ],
        false, // a splice endpoint MUST attach — never "don't snap"
    );
    match chosen {
        Some(c) if c.kind == SnapKind::ToVertex => OnRing::Vertex(vi),
        _ => edge_loc,
    }
}

/// Snap `p` to the nearest ring VERTEX within `tol`; otherwise leave it. ENUMERATE
/// the corners in reach, then apply the nearest (recording the rest as
/// alternatives) — so a hug-point near more than one corner can later be
/// reassigned. Leaving it untouched (no candidate in reach) records nothing.
pub(crate) fn snap_to_corner(p: Point2f32, ring: &[Point2f32], tol: f32) -> Point2f32 {
    let candidates = find_corner_candidates(ring, p, tol);
    if candidates.is_empty() {
        p
    } else {
        // Optional: a hug-to-corner could just stay where drawn.
        elective_choice(p, candidates, true).map_or(p, |c| c.to)
    }
}

/// Pin a point exactly onto an edge of `ring` by projecting it onto that edge's
/// LINE — so it sits on the boundary while keeping its position along the edge.
/// For an axis-aligned edge this is exactly "match the edge's perpendicular
/// coordinate" (a horizontal edge fixes `y`, a vertical one `x`); for a DIAGONAL
/// edge (a curve facet) it projects onto the slanted line rather than snapping to
/// an axis, which would drag the point off the boundary. An APPLY onto a KNOWN
/// edge — note the projection is UNCLAMPED (foot of perpendicular, may land past
/// an endpoint), unlike [`find_nearest`]'s clamped edge result. Used to pin
/// attach/cut endpoints onto their edges so they line up with the spliced
/// boundary.
pub(crate) fn pin_to_edge(p: &mut Point2f32, ring: &[Point2f32], edge: usize) {
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
    // Foot of the perpendicular from `p` to the line through a→b, CLAMPED to the
    // segment so the pinned point stays ON the edge — an extend/trim must join the
    // outline within the wall, never past its ends (and this matches the clamped
    // `t` the splice topology uses, so position and topology agree).
    let t = (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0);
    *p = OnRing::Edge { edge, t }.point(ring);
}

/// Snap each point of `stroke` onto `ring`'s perimeter when within `snap_tol`,
/// leaving points farther away untouched. Used by the multi-storey "draw on
/// top of the previous storey" flow so endpoints land exactly on the previous
/// outline (so the new storey's footprint reuses those coordinates verbatim).
/// A cross-ring consumer: it FINDs on the previous ring, then projects the
/// result to a COORDINATE ([`OnRing::point`]) — the new storey mints its own
/// vertices, so the previous ring's vertex identity is correctly discarded.
pub fn snap_stroke_to_ring(
    stroke: &[Point2f32],
    ring: &[Point2f32],
    snap_tol: f32,
) -> Vec<Point2f32> {
    if ring.len() < 2 {
        return stroke.to_vec();
    }
    stroke
        .iter()
        .map(|p| {
            let (loc, dist) = find_nearest(ring, *p, snap_tol);
            if dist > snap_tol {
                return *p;
            }
            elective_snap(SnapKind::ToPreviousOutline, *p, loc.point(ring), true)
        })
        .collect()
}

/// Snap each point of `stroke` onto the nearest of a set of REFERENCE
/// `segments` when within `tol`, leaving points farther away untouched.
///
/// Unlike [`snap_stroke_to_ring`], the references are an arbitrary, unconnected
/// COLLECTION of segments (not one closed ring) — supplied from outside the
/// plan so a carve can follow features that aren't on its clipping boundary
/// (the "end of the roof" interior division, the floor-above wall base). For
/// each stroke vertex it finds the nearest point on any segment; if within
/// `tol` it moves the vertex there, PREFERRING a segment ENDPOINT when the
/// vertex is also within `tol` of one (so corners catch and shared corners of
/// two segments stay coincident), else the nearest point along the segment.
/// A point with no segment in reach is left exactly as drawn. Each applied move
/// is routed through [`elective_snap`] (recorded as a `ToReferenceSegment`
/// decision) so the controller's snap-tag replay machinery sees it. Pure aside
/// from that recording; degenerate (zero-length) reference segments are
/// skipped, and an empty `segments` leaves the whole stroke untouched.
pub fn snap_stroke_to_segments(
    stroke: &[Point2f32],
    segments: &[[Point2f32; 2]],
    tol: f32,
) -> Vec<Point2f32> {
    if segments.is_empty() {
        return stroke.to_vec();
    }
    stroke
        .iter()
        .map(|&p| {
            // Track the nearest ENDPOINT and the nearest mid-segment FOOT
            // independently: an endpoint in reach is PREFERRED (corners catch)
            // even when a foot is marginally closer, so a vertex near a corner
            // lands ON the corner rather than just beside it on the edge.
            let mut best_end: Option<(f32, Point2f32)> = None;
            let mut best_foot: Option<(f32, Point2f32)> = None;
            let consider = |slot: &mut Option<(f32, Point2f32)>, dist: f32, target| {
                if dist <= tol && slot.is_none_or(|(bd, _)| dist < bd) {
                    *slot = Some((dist, target));
                }
            };
            for seg in segments {
                let (a, b) = (seg[0], seg[1]);
                if geom::segment_len(a, b) < 1e-6 {
                    continue; // degenerate reference segment
                }
                consider(&mut best_end, geom::segment_len(p, a), a);
                consider(&mut best_end, geom::segment_len(p, b), b);
                let (q, _t, dist) = geom::project_point_to_segment(p, a, b);
                consider(&mut best_foot, dist, q);
            }
            match best_end.or(best_foot) {
                Some((_, target)) => elective_snap(SnapKind::ToReferenceSegment, p, target, true),
                None => p,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // 10x10 square: 0:(0,0) 1:(10,0) 2:(10,10) 3:(0,10)
    fn unit_square() -> Vec<Point2f32> {
        let p = |x, y| Point2f32::new(x, y);
        vec![p(0.0, 0.0), p(10.0, 0.0), p(10.0, 10.0), p(0.0, 10.0)]
    }

    #[test]
    fn find_nearest_vertex_when_within_tol() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        let (loc, dist) = find_nearest(&ring, p(10.5, 9.6), 2.0);
        assert_eq!(loc, OnRing::Vertex(2));
        assert!(dist < 2.0);
    }

    #[test]
    fn find_nearest_edge_when_far_from_any_vertex() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        let (loc, dist) = find_nearest(&ring, p(5.0, -1.0), 2.0);
        match loc {
            OnRing::Edge { edge, t } => {
                assert_eq!(edge, 0);
                assert!((t - 0.5).abs() < 1e-3);
            }
            OnRing::Vertex(_) => panic!("expected an edge"),
        }
        assert!((dist - 1.0).abs() < 1e-3);
    }

    #[test]
    fn on_ring_point_resolves_vertex_and_edge() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        assert_eq!(OnRing::Vertex(2).point(&ring), p(10.0, 10.0));
        assert_eq!(OnRing::Edge { edge: 0, t: 0.5 }.point(&ring), p(5.0, 0.0));
    }

    #[test]
    fn elective_snap_is_recorded_as_a_decision() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        let _ = take_snap_decisions(); // clear any stray
                                       // A corner snap is elective → recorded.
        snap_to_corner(p(9.95, 10.05), &ring, 0.2);
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1, "one elective decision recorded");
        let d = &decisions[0];
        assert_eq!(d.candidates.len(), 1, "single candidate for now");
        assert_eq!(d.chosen, Some(0));
        assert_eq!(d.candidates[0].kind, SnapKind::ToCorner);
        assert_eq!(d.candidates[0].to, p(10.0, 10.0));
        // Draining again yields nothing.
        assert!(take_snap_decisions().is_empty());
    }

    #[test]
    fn no_snap_records_nothing() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        let _ = take_snap_decisions();
        snap_to_corner(p(5.0, 0.05), &ring, 0.2); // mid-edge, no snap
        assert!(
            take_snap_decisions().is_empty(),
            "no decision when nothing snapped"
        );
    }

    #[test]
    fn snap_to_corner_records_all_nearby_corners_as_alternatives() {
        let p = |x, y| Point2f32::new(x, y);
        // Two corners 0.1 apart; the rest far away.
        let ring = vec![p(0.0, 0.0), p(0.1, 0.0), p(5.0, 5.0), p(0.0, 5.0)];
        let _ = take_snap_decisions();
        // Drawn point closer to corner (0,0) than to (0.1,0), both within tol.
        let result = snap_to_corner(p(0.04, -0.05), &ring, 0.2);
        assert_eq!(result, p(0.0, 0.0), "applies the NEAREST corner");
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1);
        let d = &decisions[0];
        assert_eq!(
            d.candidates.len(),
            2,
            "both nearby corners are alternatives"
        );
        assert_eq!(d.chosen, Some(0));
        assert_eq!(d.candidates[0].to, p(0.0, 0.0)); // chosen (nearest)
        assert_eq!(d.candidates[1].to, p(0.1, 0.0)); // alternative
        assert!(d.candidates.iter().all(|c| c.kind == SnapKind::ToCorner));
    }

    #[test]
    fn choose_attach_records_corner_vs_edge_and_keeps_corner() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square(); // corner (10,10) is vertex 2
        let _ = take_snap_decisions();
        let (found, d) = find_nearest(&ring, p(9.9, 10.1), 0.5);
        assert!(found.is_vertex(), "found the corner");
        // p(9.9, 10.1) drops a perpendicular onto the top wall (y=10) within
        // tol, so the edge alternative is offered.
        let chosen = choose_attach(&ring, p(9.9, 10.1), found, d, 0.5);
        assert_eq!(chosen, OnRing::Vertex(2), "default keeps the corner");
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1);
        let kinds: Vec<_> = decisions[0].candidates.iter().map(|c| c.kind).collect();
        assert!(
            kinds.contains(&SnapKind::ToVertex) && kinds.contains(&SnapKind::ToEdge),
            "records corner AND edge alternatives: {kinds:?}"
        );
    }

    #[test]
    fn choose_attach_offers_no_edge_when_perpendicular_foot_is_off_every_segment() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square(); // corner (10,10) is vertex 2
        let _ = take_snap_decisions();
        // Diagonally PAST the corner: the foot on both incident walls falls beyond
        // the segment ends, so no wall can be projected onto — corner only.
        let probe = p(10.1, 10.1);
        let (found, d) = find_nearest(&ring, probe, 0.5);
        assert!(found.is_vertex());
        let chosen = choose_attach(&ring, probe, found, d, 0.5);
        assert_eq!(chosen, OnRing::Vertex(2));
        assert!(
            take_snap_decisions().is_empty(),
            "no edge alternative → no elective decision recorded"
        );
    }

    #[test]
    fn policy_can_force_no_snap() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        let from = p(9.95, 10.05);
        let _ = take_snap_decisions();
        // Force "don't snap" at this drawn point.
        set_snap_policy(vec![(from, SnapChoice::Skip)]);
        let result = snap_to_corner(from, &ring, 0.2);
        clear_snap_policy();
        assert_eq!(result, from, "policy reverted to the drawn point");
        let d = take_snap_decisions();
        assert_eq!(d.len(), 1, "the decision is still recorded");
        assert_eq!(d[0].chosen, None, "recorded as no-snap");
    }

    #[test]
    fn snap_to_corner_takes_nearby_vertex_only() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        // near corner (10,10) → snaps to it
        assert_eq!(snap_to_corner(p(9.95, 10.05), &ring, 0.2), p(10.0, 10.0));
        // mid-edge, far from any corner → unchanged
        assert_eq!(snap_to_corner(p(5.0, 0.05), &ring, 0.2), p(5.0, 0.05));
    }

    #[test]
    fn snap_stroke_to_segments_moves_near_vertex_onto_segment() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A vertical division line at x = 5, running y ∈ [0, 10].
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        let stroke = vec![p(5.08, 4.0), p(8.0, 4.0)]; // first near the line, second far
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15);
        // Within tol → snapped onto the line (x becomes 5); mid-segment foot.
        assert!(
            (out[0].x - 5.0).abs() < 1e-4,
            "snapped onto x=5: {:?}",
            out[0]
        );
        assert!((out[0].y - 4.0).abs() < 1e-4, "y unchanged along the line");
        // Outside tol → untouched.
        assert_eq!(out[1], p(8.0, 4.0));
    }

    #[test]
    fn snap_stroke_to_segments_prefers_endpoint_for_corner() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        // Near the BOTTOM endpoint (5,0): both endpoint and edge-foot are in
        // reach, but the corner should win so the vertex lands on (5,0).
        let stroke = vec![p(5.05, 0.05)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15);
        assert_eq!(out[0], p(5.0, 0.0), "snapped onto the endpoint corner");
    }

    #[test]
    fn snap_stroke_to_segments_leaves_far_points_and_records_only_moves() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        let stroke = vec![p(0.0, 0.0), p(5.05, 6.0), p(9.0, 9.0)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15);
        assert_eq!(out[0], p(0.0, 0.0), "far point untouched");
        assert!((out[1].x - 5.0).abs() < 1e-4, "near point snapped");
        assert_eq!(out[2], p(9.0, 9.0), "far point untouched");
        // Only the one moved vertex records an elective decision.
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions[0].candidates[0].kind,
            SnapKind::ToReferenceSegment
        );
    }

    #[test]
    fn snap_stroke_to_segments_empty_segments_is_identity() {
        let p = |x, y| Point2f32::new(x, y);
        let stroke = vec![p(1.0, 2.0), p(3.0, 4.0)];
        assert_eq!(snap_stroke_to_segments(&stroke, &[], 0.15), stroke);
    }

    #[test]
    fn snap_stroke_to_segments_skips_degenerate_segments() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A zero-length "segment" is no snap target.
        let segs = [[p(5.0, 5.0), p(5.0, 5.0)]];
        let stroke = vec![p(5.02, 5.02)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15);
        assert_eq!(out[0], p(5.02, 5.02), "degenerate segment ignored");
        assert!(take_snap_decisions().is_empty());
    }
}
