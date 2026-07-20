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
    /// A stroke point snapped onto a CONSTRUCTION-PHASE zone boundary of the
    /// storey below — the interior "year" division a new upper storey's outline
    /// follows so the inherited phase seed splits it along the real line. Same
    /// mechanics as [`SnapKind::ToReferenceSegment`]; the distinct kind keeps
    /// the snap tag's label honest about WHAT was snapped to.
    ToPhaseBoundary,
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
///
/// For a GROUPED stroke decision (one per FEATURE the stroke snapped to,
/// covering every stroke point that snapped to it — see
/// [`snap_stroke_to_ring`] / [`snap_stroke_to_segments`]), `from` and the
/// candidate's `to` are the group's REPRESENTATIVE (middle) member — where the
/// tag renders — while applying or skipping the decision covers the WHOLE
/// group.
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
///
/// EXACT duplicates within one action are dropped: a pipeline that runs twice
/// per action (the splice pipeline on a two-piece carve) re-records the same
/// decisions on its second pass; pushing them again would stack identical tags
/// and double the log lines. A duplicate has the same `from` (within 1e-3) and
/// the same candidate kinds+targets. The policy lookup keyed by `from` is
/// unaffected — two such decisions were indistinguishable to it anyway.
fn commit(from: Point2f32, candidates: Vec<SnapCandidate>, chosen: Option<usize>, skippable: bool) {
    let duplicate = SNAP_DECISIONS.with(|d| {
        d.borrow().iter().any(|prev| {
            (prev.from.x - from.x).abs() < 1e-3
                && (prev.from.y - from.y).abs() < 1e-3
                && prev.candidates.len() == candidates.len()
                && prev.candidates.iter().zip(&candidates).all(|(a, b)| {
                    a.kind == b.kind
                        && (a.to.x - b.to.x).abs() < 1e-3
                        && (a.to.y - b.to.y).abs() < 1e-3
                })
        })
    });
    if duplicate {
        return;
    }
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

/// Below this move distance (metres, ~1 cm on the plan) a SINGLE-candidate
/// elective snap is INVISIBLE: "snap or don't" moves nothing anyone can see, so
/// it's applied silently and NO decision is recorded (no tag, no question).
/// Multi-candidate decisions stay recorded even when the top candidate sits at
/// `from` — the alternative differs. A perceptual constant (tag legibility),
/// not a geometric tolerance, so it doesn't scale with `ring_diagonal`.
pub(crate) const NOOP_SNAP_EPS_M: f32 = 0.01;

/// Is this candidate set an invisible single-candidate elective (see
/// [`NOOP_SNAP_EPS_M`])? Only SKIPPABLE decisions qualify — a mandatory attach
/// is recorded regardless (its tag documents the forced join).
fn is_invisible_single(from: Point2f32, candidates: &[SnapCandidate], skippable: bool) -> bool {
    skippable
        && candidates.len() == 1
        && geom::segment_len(from, candidates[0].to) < NOOP_SNAP_EPS_M
}

/// Record a (typically single-candidate) decision and return the applied point —
/// or `from` if the policy skipped a skippable one. The default pick is `default`.
/// An invisible single-candidate elective ([`is_invisible_single`]) applies
/// silently without recording.
pub(crate) fn record_choice(
    from: Point2f32,
    candidates: Vec<SnapCandidate>,
    default: usize,
    skippable: bool,
) -> Point2f32 {
    if is_invisible_single(from, &candidates, skippable) {
        return candidates[0].to;
    }
    let chosen = resolve(from, candidates.len(), default, skippable);
    let to = chosen.map_or(from, |i| candidates[i].to);
    commit(from, candidates, chosen, skippable);
    to
}

/// Record ONE skippable single-candidate decision covering a GROUP of moves (a
/// stroke's points against one feature, a wall edge's two endpoints leveling
/// onto one parallel line) and return whether it was APPLIED — the caller moves
/// or reverts EVERY grouped member together. `from`/`to`/`score` are the
/// group's REPRESENTATIVE (where the tag renders); the policy is resolved once,
/// keyed on that `from`.
pub(crate) fn record_grouped_choice(
    kind: SnapKind,
    from: Point2f32,
    to: Point2f32,
    score: f32,
) -> bool {
    let chosen = resolve(from, 1, 0, true);
    commit(from, vec![SnapCandidate { kind, to, score }], chosen, true);
    chosen.is_some()
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
        SnapKind::ToPreviousOutline | SnapKind::ToReferenceSegment | SnapKind::ToPhaseBoundary => 2,
        SnapKind::ToParallelWall => 3,
    }
}

/// Choose among several elective candidates: sort most-aggressive-first (ties
/// broken by closeness), apply the forced-choice policy (default = the top),
/// record the decision (alternatives preserved for the UI), and return the CHOSEN
/// candidate — or `None` if the policy skipped the snap. Empty `candidates` is a
/// programming error: a site with nothing to offer must not call this. An
/// invisible single-candidate elective ([`is_invisible_single`]) applies
/// silently without recording — a `ToCorner (x,y) -> (x,y)` no-op is not a
/// question.
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
    if is_invisible_single(from, &candidates, skippable) {
        return Some(candidates[0]);
    }
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

/// The distance from `p` to the nearest point on `ring`'s perimeter (vertices
/// included — every vertex lies on an edge). `INFINITY` for a degenerate ring.
/// The pure FIND half of [`find_nearest`], exposed for callers that only need
/// the distance — e.g. filtering out reference segments that already lie ON an
/// outline a carve snaps to by itself.
pub fn dist_to_ring(ring: &[Point2f32], p: Point2f32) -> f32 {
    find_nearest(ring, p, 0.0).1
}

/// Snap a tapped `p` onto the nearest boundary FEATURE across every ring in
/// `rings` — a ring VERTEX within `vertex_tol`, or (failing any vertex in
/// reach) the nearest PERPENDICULAR edge foot within `edge_tol` (the foot
/// landing strictly WITHIN a wall, [`find_edge_snap`]). Returns the snapped
/// coordinate, or `p` unchanged when nothing is in reach.
///
/// A vertex always beats an edge (a corner is the stronger target the assessor
/// usually aims for); across rings the nearest wins. Unlike [`snap_to_corner`]
/// this spans SEVERAL rings and records NO elective-snap decision — a two-tap
/// centre-between pick is a one-shot coordinate query, not part of the
/// snap-tag replay machinery. `rings` with fewer than two points are skipped.
pub fn snap_point_across_rings(
    rings: &[&[Point2f32]],
    p: Point2f32,
    vertex_tol: f32,
    edge_tol: f32,
) -> Point2f32 {
    // Prefer the nearest VERTEX in reach across all rings.
    let mut best_vertex: Option<(f32, Point2f32)> = None;
    for ring in rings {
        for &v in *ring {
            let d = geom::segment_len(p, v);
            if d <= vertex_tol && best_vertex.is_none_or(|(bd, _)| d < bd) {
                best_vertex = Some((d, v));
            }
        }
    }
    if let Some((_, v)) = best_vertex {
        return v;
    }
    // Else the nearest PERPENDICULAR edge foot in reach.
    let mut best_edge: Option<(f32, Point2f32)> = None;
    for ring in rings {
        if ring.len() < 2 {
            continue;
        }
        if let Some((edge, t, dist)) = find_edge_snap(ring, p, edge_tol) {
            if best_edge.is_none_or(|(bd, _)| dist < bd) {
                best_edge = Some((dist, OnRing::Edge { edge, t }.point(ring)));
            }
        }
    }
    best_edge.map_or(p, |(_, q)| q)
}

/// Apply a stroke's pre-computed per-point snap targets, recording ONE elective
/// decision per FEATURE: ALL points whose group key `K` compares equal share
/// one decision, regardless of adjacency — a wobbling raw stroke drifting in
/// and out of tolerance against the same wall is still ONE question, one per
/// drawn-wall → target-feature relationship. `targets[i]` is stroke point
/// `i`'s snap — `(target, dist, key)` — or `None` where nothing was in reach.
///
/// The applied GEOMETRY is identical to per-point snapping: under Apply every
/// member still moves to its OWN target; a `Skip` reverts the WHOLE group to
/// the drawn points (un-snapping one raw ink point out of a wall run is never
/// wanted). A group whose members ALL move less than [`NOOP_SNAP_EPS_M`] is
/// invisible — applied silently, no decision recorded.
///
/// The recorded `from`/`to` are the group's REPRESENTATIVE — the MIDDLE member
/// (by stroke order): drawn point and its target (where the tag renders).
/// Replay stability of `from`: computing `targets` never consults the
/// forced-choice policy, so replaying the same input stroke yields the same
/// groups and hence the same representative `from` points — which the policy
/// lookup keyed by `from` relies on.
fn apply_grouped_stroke_snaps<K: PartialEq>(
    stroke: &[Point2f32],
    targets: &[Option<(Point2f32, f32, K)>],
    kind: SnapKind,
) -> Vec<Point2f32> {
    // Gather the feature groups in first-appearance order.
    let mut groups: Vec<(&K, Vec<usize>)> = Vec::new();
    for (i, t) in targets.iter().enumerate() {
        if let Some((_, _, key)) = t {
            match groups.iter_mut().find(|(k, _)| *k == key) {
                Some((_, members)) => members.push(i),
                None => groups.push((key, vec![i])),
            }
        }
    }
    let mut out = stroke.to_vec();
    for (_, members) in groups {
        let mid = members[members.len() / 2];
        let (mid_to, mid_dist, _) = targets[mid].as_ref().unwrap();
        let max_move = members
            .iter()
            .map(|&i| targets[i].as_ref().unwrap().1)
            .fold(0.0_f32, f32::max);
        // Zero-effect suppression: EVERY member's move is invisible → apply
        // silently, ask nothing. (One visible member keeps the group asked.)
        let apply = max_move < NOOP_SNAP_EPS_M
            || record_grouped_choice(kind, stroke[mid], *mid_to, *mid_dist);
        if apply {
            for &i in &members {
                out[i] = targets[i].as_ref().unwrap().0;
            }
        }
    }
    out
}

/// Group key for [`snap_stroke_to_ring`]: the [`OnRing`] SHAPE of a point's
/// target — the vertex index, or the edge index (ignoring `t`, which varies
/// along a run following the edge).
#[derive(Debug, Clone, Copy, PartialEq)]
enum RingSnapKey {
    Vertex(usize),
    Edge(usize),
}

/// Snap each point of `stroke` onto `ring`'s perimeter when within `snap_tol`,
/// leaving points farther away untouched. Used by the multi-storey "draw on
/// top of the previous storey" flow so endpoints land exactly on the previous
/// outline (so the new storey's footprint reuses those coordinates verbatim).
/// A cross-ring consumer: it FINDs on the previous ring, then projects the
/// result to a COORDINATE ([`OnRing::point`]) — the new storey mints its own
/// vertices, so the previous ring's vertex identity is correctly discarded.
///
/// Recording is GROUPED: all points snapping to the same feature (same vertex,
/// or same edge) share ONE `ToPreviousOutline` decision via
/// [`apply_grouped_stroke_snaps`], while each point still moves to its own
/// nearest spot exactly as before.
pub fn snap_stroke_to_ring(
    stroke: &[Point2f32],
    ring: &[Point2f32],
    snap_tol: f32,
) -> Vec<Point2f32> {
    if ring.len() < 2 {
        return stroke.to_vec();
    }
    // Pass 1 — FIND each point's target (policy-free; see the helper's doc).
    let targets: Vec<Option<(Point2f32, f32, RingSnapKey)>> = stroke
        .iter()
        .map(|&p| {
            let (loc, dist) = find_nearest(ring, p, snap_tol);
            if dist > snap_tol {
                return None;
            }
            let key = match loc {
                OnRing::Vertex(i) => RingSnapKey::Vertex(i),
                OnRing::Edge { edge, .. } => RingSnapKey::Edge(edge),
            };
            Some((loc.point(ring), dist, key))
        })
        .collect();
    // Pass 2 — group into runs, record one decision per run, apply.
    apply_grouped_stroke_snaps(stroke, &targets, SnapKind::ToPreviousOutline)
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
/// A point with no segment in reach is left exactly as drawn; degenerate
/// (zero-length) reference segments are skipped, and an empty `segments`
/// leaves the whole stroke untouched.
///
/// Recording is GROUPED: all points snapping to the same feature — the same
/// segment ENDPOINT (keyed by its quantized coordinate, so the shared corner
/// of two segments is one feature) or a foot on the same infinite LINE (keyed
/// canonically, so COLLINEAR segments — a boundary split into pieces — are one
/// feature) — share ONE decision of the supplied `kind` (`ToReferenceSegment`
/// for a roof/wall-base reference, `ToPhaseBoundary` for a storey-below phase
/// division) via [`apply_grouped_stroke_snaps`], so the controller's snap-tag
/// replay machinery sees one tag per drawn-wall → feature relationship, not
/// one per raw ink point. Endpoint catches at the ends of a line the stroke
/// also snapped FEET onto MERGE into that line's decision (see the inline
/// merge rule); an isolated corner catch keeps its own. Each point still moves
/// to its own per-point target exactly as before.
pub fn snap_stroke_to_segments(
    stroke: &[Point2f32],
    segments: &[[Point2f32; 2]],
    tol: f32,
    kind: SnapKind,
) -> Vec<Point2f32> {
    if segments.is_empty() {
        return stroke.to_vec();
    }
    /// Group key: an endpoint target is the endpoint COORDINATE quantized to
    /// ~1e-4 m (stable comparison; shared corners compare equal); a mid-segment
    /// foot is the segment's canonical infinite LINE ([`line_key`]).
    #[derive(Debug, Clone, Copy, PartialEq)]
    enum SegmentSnapKey {
        Endpoint(i64, i64),
        Line(i64, i64, i64),
    }
    fn quantize(v: f32) -> i64 {
        (f64::from(v) / 1e-4).round() as i64
    }
    /// Canonical key of the infinite LINE through `a`→`b`: the sign-normalized
    /// unit normal plus the signed offset from the origin, quantized to ~1e-3 —
    /// so collinear segments (phase-zone edges, outline continuations split
    /// into pieces) share one key, and `a`→`b` equals `b`→`a`.
    fn line_key(a: Point2f32, b: Point2f32) -> SegmentSnapKey {
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = (dx * dx + dy * dy).sqrt();
        let (mut nx, mut ny) = (-dy / len, dx / len);
        let mut off = nx * a.x + ny * a.y;
        if nx < -1e-6 || (nx.abs() <= 1e-6 && ny < 0.0) {
            (nx, ny, off) = (-nx, -ny, -off);
        }
        let q = |v: f32| (f64::from(v) / 1e-3).round() as i64;
        SegmentSnapKey::Line(q(nx), q(ny), q(off))
    }
    // Pass 1 — FIND each point's target (policy-free; see the helper's doc).
    let mut targets: Vec<Option<(Point2f32, f32, SegmentSnapKey)>> = stroke
        .iter()
        .map(|&p| {
            // Track the nearest ENDPOINT and the nearest mid-segment FOOT
            // independently: an endpoint in reach is PREFERRED (corners catch)
            // even when a foot is marginally closer, so a vertex near a corner
            // lands ON the corner rather than just beside it on the edge.
            let mut best_end: Option<(f32, Point2f32)> = None;
            let mut best_foot: Option<(f32, Point2f32, usize)> = None;
            for (si, seg) in segments.iter().enumerate() {
                let (a, b) = (seg[0], seg[1]);
                if geom::segment_len(a, b) < 1e-6 {
                    continue; // degenerate reference segment
                }
                for end in [a, b] {
                    let dist = geom::segment_len(p, end);
                    if dist <= tol && best_end.is_none_or(|(bd, _)| dist < bd) {
                        best_end = Some((dist, end));
                    }
                }
                let (q, _t, dist) = geom::project_point_to_segment(p, a, b);
                if dist <= tol && best_foot.is_none_or(|(bd, _, _)| dist < bd) {
                    best_foot = Some((dist, q, si));
                }
            }
            match best_end {
                Some((dist, end)) => Some((
                    end,
                    dist,
                    SegmentSnapKey::Endpoint(quantize(end.x), quantize(end.y)),
                )),
                None => best_foot
                    .map(|(dist, q, si)| (q, dist, line_key(segments[si][0], segments[si][1]))),
            }
        })
        .collect();
    // MERGE endpoint catches into the line they terminate. A stroke running
    // along one reference line catches the line's corners at both ends — foot
    // group + two endpoint groups — but to the user that's ONE relationship
    // ("snap to the phase line"), not three questions. An endpoint always lies
    // on the line(s) of the segment(s) it terminates, so: if any of those
    // lines has FOOT members from this stroke, re-key the endpoint members to
    // that line's key — Skip then reverts corners and feet together, while
    // Apply still lands every member on its own pass-1 target (corner members
    // still land ON the corner). Ambiguity (an endpoint shared by two grouped
    // lines — a drawn corner between two snapped walls): merge into the line
    // with the MOST foot members; tie → the line whose foot group appears
    // first in stroke order. Deterministic over the same input, so the merged
    // decision's representative `from` is replay-stable like any other. An
    // isolated corner catch (no grouped line through it) keeps its own
    // endpoint-keyed decision.
    let mut line_groups: Vec<(SegmentSnapKey, usize)> = Vec::new(); // stroke order
    for t in &targets {
        if let Some((_, _, key @ SegmentSnapKey::Line(..))) = t {
            match line_groups.iter_mut().find(|(k, _)| k == key) {
                Some((_, count)) => *count += 1,
                None => line_groups.push((*key, 1)),
            }
        }
    }
    if !line_groups.is_empty() {
        for t in &mut targets {
            let Some((_, _, key)) = t else { continue };
            let SegmentSnapKey::Endpoint(qx, qy) = *key else {
                continue;
            };
            let merged = segments
                .iter()
                .filter(|seg| {
                    geom::segment_len(seg[0], seg[1]) >= 1e-6
                        && seg
                            .iter()
                            .any(|e| quantize(e.x) == qx && quantize(e.y) == qy)
                })
                .filter_map(|seg| {
                    let lk = line_key(seg[0], seg[1]);
                    line_groups
                        .iter()
                        .position(|(k, _)| *k == lk)
                        .map(|pos| (line_groups[pos].1, pos, lk))
                })
                // Most foot members wins; tie → earliest group in stroke order.
                .max_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
            if let Some((_, _, lk)) = merged {
                *key = lk;
            }
        }
    }
    // Pass 2 — group by feature, record one decision per group, apply.
    apply_grouped_stroke_snaps(stroke, &targets, kind)
}

/// EDGE-snap the four SIDES of the axis-aligned rectangle `[lo, hi]` onto nearby
/// PARALLEL reference segments — the whole-edge analogue of a point snap, and the
/// robust behaviour a dormer footprint needs (a cheek running alongside a shorter
/// reference snaps its WHOLE side flush, however short the reference). For each
/// side — left (x = lo.x), right (x = hi.x), bottom (y = lo.y), top (y = hi.y):
///
/// - An x-side considers only VERTICAL references (`|dx| ≈ 0`); a y-side only
///   HORIZONTAL ones. A DIAGONAL reference (a roof hip) never snaps an axis side.
/// - A reference is a candidate when its perpendicular offset from the side is
///   ≤ `tol` AND its extent OVERLAPS the side's extent along the shared axis by
///   any POSITIVE amount (a SHORT reference that overlaps only part of the side
///   still counts). The side snaps its whole coordinate onto the NEAREST such
///   reference.
/// - INVERSION GUARD: if snapping a pair of opposing sides would collapse or
///   invert the rectangle on that axis, BOTH snaps are dropped (that axis stays
///   put and unpinned).
///
/// Each snapped side is recorded as ONE elective [`SnapKind::ToParallelWall`]
/// decision (keyed at the side MIDPOINT, replay-stable) and applied only if the
/// forced-choice policy didn't skip it — mirroring `carve::snap_walls`, so the
/// controller's snap-tag / tap-to-unsnap machinery drives it unchanged. A
/// sub-[`NOOP_SNAP_EPS_M`] move is invisible: applied silently, no tag. A
/// side whose decision the policy skipped is left un-snapped AND un-pinned.
///
/// Returns `((lo.x, lo.y), (len.x, len.y), [x_lo, x_hi, y_lo, y_hi])` — the
/// snapped LO corner, the side lengths (`hi - lo`, matching how the rect FSM
/// stores geometry), and which sides pinned.
pub fn snap_axis_rect_to_refs(
    lo: Point2f32,
    hi: Point2f32,
    segments: &[[Point2f32; 2]],
    tol: f32,
) -> ((f32, f32), (f32, f32), [bool; 4]) {
    let eps = 1e-3_f32; // axis-aligned classifier + minimum positive overlap
    let min_extent = 1e-3_f32; // never let a snap collapse/invert the rect

    // Nearest reference coordinate a side can level onto: parallel to the side,
    // overlapping the side's extent by any positive amount, within `tol`
    // perpendicular. `want_vertical` selects vertical refs (for an x-side) or
    // horizontal ones (for a y-side); diagonal refs match neither.
    let nearest = |side_coord: f32, span: (f32, f32), want_vertical: bool| -> Option<f32> {
        let (s_lo, s_hi) = (span.0.min(span.1), span.0.max(span.1));
        let mut best: Option<(f32, f32)> = None; // (coord, |offset|)
        for seg in segments {
            let (a, b) = (seg[0], seg[1]);
            let (dx, dy) = (b.x - a.x, b.y - a.y);
            let (coord, r_lo, r_hi) = if want_vertical {
                if dx.abs() > eps {
                    continue; // not a vertical reference
                }
                (a.x, a.y.min(b.y), a.y.max(b.y))
            } else {
                if dy.abs() > eps {
                    continue; // not a horizontal reference
                }
                (a.y, a.x.min(b.x), a.x.max(b.x))
            };
            let off = (coord - side_coord).abs();
            if off > tol {
                continue;
            }
            if r_hi.min(s_hi) - r_lo.max(s_lo) <= eps {
                continue; // no positive overlap along the shared axis
            }
            if best.is_none_or(|(_, bo)| off < bo) {
                best = Some((coord, off));
            }
        }
        best.map(|(c, _)| c)
    };

    // Candidates from the ORIGINAL rect (replay-stable — never re-derived from a
    // partly snapped rect).
    let mut c_xlo = nearest(lo.x, (lo.y, hi.y), true);
    let mut c_xhi = nearest(hi.x, (lo.y, hi.y), true);
    let mut c_ylo = nearest(lo.y, (lo.x, hi.x), false);
    let mut c_yhi = nearest(hi.y, (lo.x, hi.x), false);

    // Inversion guard, per axis: prospective snapped sides that would collapse or
    // cross the rect drop BOTH snaps on that axis.
    if c_xhi.unwrap_or(hi.x) - c_xlo.unwrap_or(lo.x) <= min_extent {
        c_xlo = None;
        c_xhi = None;
    }
    if c_yhi.unwrap_or(hi.y) - c_ylo.unwrap_or(lo.y) <= min_extent {
        c_ylo = None;
        c_yhi = None;
    }

    // Record + apply one elective decision per snapped side. Returns whether the
    // side was applied (policy could skip it).
    let apply_snap = |from: Point2f32, to: Point2f32, dist: f32| -> bool {
        dist < NOOP_SNAP_EPS_M || record_grouped_choice(SnapKind::ToParallelWall, from, to, dist)
    };

    let (mut lo_x, mut lo_y) = (lo.x, lo.y);
    let (mut hi_x, mut hi_y) = (hi.x, hi.y);
    let mut pins = [false; 4];

    if let Some(tx) = c_xlo {
        let from = Point2f32::new(lo.x, (lo.y + hi.y) * 0.5);
        if apply_snap(from, Point2f32::new(tx, from.y), (tx - lo.x).abs()) {
            lo_x = tx;
            pins[0] = true;
        }
    }
    if let Some(tx) = c_xhi {
        let from = Point2f32::new(hi.x, (lo.y + hi.y) * 0.5);
        if apply_snap(from, Point2f32::new(tx, from.y), (tx - hi.x).abs()) {
            hi_x = tx;
            pins[1] = true;
        }
    }
    if let Some(ty) = c_ylo {
        let from = Point2f32::new((lo.x + hi.x) * 0.5, lo.y);
        if apply_snap(from, Point2f32::new(from.x, ty), (ty - lo.y).abs()) {
            lo_y = ty;
            pins[2] = true;
        }
    }
    if let Some(ty) = c_yhi {
        let from = Point2f32::new((lo.x + hi.x) * 0.5, hi.y);
        if apply_snap(from, Point2f32::new(from.x, ty), (ty - hi.y).abs()) {
            hi_y = ty;
            pins[3] = true;
        }
    }

    ((lo_x, lo_y), (hi_x - lo_x, hi_y - lo_y), pins)
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
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
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
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert_eq!(out[0], p(5.0, 0.0), "snapped onto the endpoint corner");
    }

    #[test]
    fn snap_stroke_to_segments_leaves_far_points_and_records_only_moves() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        let stroke = vec![p(0.0, 0.0), p(5.05, 6.0), p(9.0, 9.0)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
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
        assert_eq!(
            snap_stroke_to_segments(&stroke, &[], 0.15, SnapKind::ToReferenceSegment),
            stroke
        );
    }

    #[test]
    fn snap_stroke_to_segments_skips_degenerate_segments() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A zero-length "segment" is no snap target.
        let segs = [[p(5.0, 5.0), p(5.0, 5.0)]];
        let stroke = vec![p(5.02, 5.02)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert_eq!(out[0], p(5.02, 5.02), "degenerate segment ignored");
        assert!(take_snap_decisions().is_empty());
    }

    #[test]
    fn snap_stroke_to_segments_records_the_supplied_kind() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A phase division at x = 5 — the new-storey outline pre-snap's kind.
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        let stroke = vec![p(5.08, 4.0)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToPhaseBoundary);
        assert!((out[0].x - 5.0).abs() < 1e-4, "snapped onto the division");
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1);
        assert_eq!(
            decisions[0].candidates[0].kind,
            SnapKind::ToPhaseBoundary,
            "the decision carries the caller's kind"
        );
    }

    #[test]
    fn stroke_run_on_one_segment_records_one_decision_and_skip_reverts_the_run() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // Horizontal reference y=5, x ∈ [0, 10]; ten raw ink points wobbling
        // along its middle (all far from the endpoints, so one foot run).
        let segs = [[p(0.0, 5.0), p(10.0, 5.0)]];
        let stroke: Vec<Point2f32> = (0..10)
            .map(|i| p(1.0 + 0.8 * i as f32, if i % 2 == 0 { 5.05 } else { 4.95 }))
            .collect();
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        // Every point moved to its OWN perpendicular foot (per-point geometry
        // identical to the old behaviour).
        for (i, q) in out.iter().enumerate() {
            assert!(
                (q.y - 5.0).abs() < 1e-4,
                "point {i} snapped onto y=5: {q:?}"
            );
            assert!((q.x - stroke[i].x).abs() < 1e-4, "x kept along the run");
        }
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1, "ONE decision for the whole run");
        let d = &decisions[0];
        assert_eq!(d.from, stroke[5], "from = the run's MIDDLE drawn point");
        assert!(d.skippable);
        assert_eq!(d.candidates.len(), 1);
        assert_eq!(d.candidates[0].kind, SnapKind::ToReferenceSegment);
        // Forcing Skip at the recorded `from` reverts ALL ten points to drawn.
        set_snap_policy(vec![(d.from, SnapChoice::Skip)]);
        let out2 = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        clear_snap_policy();
        assert_eq!(out2, stroke, "Skip reverted the WHOLE run");
        let d2 = take_snap_decisions();
        assert_eq!(d2.len(), 1, "the skipped decision is still recorded");
        assert_eq!(d2[0].chosen, None);
        assert_eq!(d2[0].from, stroke[5], "same representative from on replay");
    }

    #[test]
    fn two_runs_on_two_segments_record_two_independently_skippable_decisions() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // Two vertical references at x=0 and x=20.
        let segs = [[p(0.0, 0.0), p(0.0, 10.0)], [p(20.0, 0.0), p(20.0, 10.0)]];
        let stroke = vec![
            p(0.05, 2.0),
            p(-0.05, 3.0),
            p(0.04, 4.0), // run A on segment 0
            p(10.0, 5.0), // far — breaks the run
            p(20.05, 2.0),
            p(19.95, 3.0), // run B on segment 1
        ];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert!(out[0..3].iter().all(|q| q.x.abs() < 1e-4), "run A snapped");
        assert_eq!(out[3], stroke[3], "the far point untouched");
        assert!(
            out[4..6].iter().all(|q| (q.x - 20.0).abs() < 1e-4),
            "run B snapped"
        );
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 2, "one decision per run");
        // Skip run A only: its points revert, run B stays snapped.
        set_snap_policy(vec![(decisions[0].from, SnapChoice::Skip)]);
        let out2 = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        clear_snap_policy();
        let _ = take_snap_decisions();
        assert_eq!(&out2[0..3], &stroke[0..3], "run A reverted independently");
        assert!(
            out2[4..6].iter().all(|q| (q.x - 20.0).abs() < 1e-4),
            "run B still snapped"
        );
    }

    #[test]
    fn line_run_with_corner_catches_at_both_ends_is_one_decision() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // The user's shape: a stroke running along one vertical reference line,
        // catching the line's corners at BOTH ends. One relationship — "snap to
        // the phase line" — must be ONE question, not foot + two corner groups.
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        let stroke = vec![
            p(5.05, 0.03), // catches the bottom endpoint (5,0)
            p(4.95, 2.0),
            p(5.05, 4.0),
            p(4.95, 6.0),
            p(5.05, 8.0),  // perpendicular FEET along the line
            p(5.03, 9.95), // catches the top endpoint (5,10)
        ];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        // Every member keeps its OWN pass-1 target under Apply: corners on the
        // corners, feet at their own heights on the line.
        assert_eq!(
            out[0],
            p(5.0, 0.0),
            "corner member still lands ON the corner"
        );
        assert_eq!(out[5], p(5.0, 10.0));
        for i in 1..5 {
            assert!(
                (out[i].x - 5.0).abs() < 1e-4 && (out[i].y - stroke[i].y).abs() < 1e-4,
                "foot member {i} on the line at its own height: {:?}",
                out[i]
            );
        }
        let decisions = take_snap_decisions();
        assert_eq!(
            decisions.len(),
            1,
            "corner catches merged into the line's decision: {decisions:?}"
        );
        // Skip reverts feet AND corner catches together.
        set_snap_policy(vec![(decisions[0].from, SnapChoice::Skip)]);
        let out2 = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        clear_snap_policy();
        let _ = take_snap_decisions();
        assert_eq!(out2, stroke, "Skip reverted the whole merged group");
    }

    #[test]
    fn isolated_corner_catch_keeps_its_own_decision() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // Two reference lines: the stroke catches line A's corner WITHOUT
        // snapping any feet onto A, and runs feet along line B. The corner is
        // NOT on B's line, so it must not merge into B — it keeps its own
        // endpoint-keyed decision.
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)], [p(20.0, 0.0), p(20.0, 10.0)]];
        let stroke = vec![
            p(5.05, 0.03), // isolated catch on A's bottom corner
            p(20.05, 3.0),
            p(19.95, 5.0), // feet on B
        ];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert_eq!(out[0], p(5.0, 0.0), "corner point landed ON the endpoint");
        assert!((out[1].x - 20.0).abs() < 1e-4);
        let decisions = take_snap_decisions();
        assert_eq!(
            decisions.len(),
            2,
            "an isolated corner catch stays its own decision: {decisions:?}"
        );
    }

    #[test]
    fn ambiguous_corner_catch_merges_into_the_line_with_most_members() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A drawn L against two reference walls sharing corner (0,0): three
        // feet on the vertical line, the corner catch, two feet on the
        // horizontal. The endpoint lies on BOTH lines; both have foot groups —
        // it merges into the one with the MOST members (the vertical).
        let segs = [
            [p(0.0, 0.0), p(10.0, 0.0)], // horizontal y=0
            [p(0.0, 0.0), p(0.0, 10.0)], // vertical x=0 (shares corner (0,0))
        ];
        let stroke = vec![
            p(0.05, 6.0),
            p(-0.04, 4.0),
            p(0.03, 2.0),  // three feet on the vertical
            p(0.06, 0.04), // the ambiguous corner catch at (0,0)
            p(2.0, 0.05),
            p(4.0, -0.03), // two feet on the horizontal
        ];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert_eq!(out[3], p(0.0, 0.0), "corner member landed on the corner");
        let decisions = take_snap_decisions();
        assert_eq!(
            decisions.len(),
            2,
            "one decision per LINE — the corner merged, not a third question"
        );
        // Skipping the VERTICAL line's decision reverts its feet AND the corner
        // (proving the corner merged into the most-members group), while the
        // horizontal feet stay snapped.
        let vertical = decisions
            .iter()
            .find(|d| (d.from.x - stroke[2].x).abs() < 1e-6)
            .expect("vertical group's decision at its middle member");
        set_snap_policy(vec![(vertical.from, SnapChoice::Skip)]);
        let out2 = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        clear_snap_policy();
        let _ = take_snap_decisions();
        assert_eq!(
            &out2[0..4],
            &stroke[0..4],
            "vertical feet + corner reverted"
        );
        assert!(
            (out2[4].y).abs() < 1e-4 && (out2[5].y).abs() < 1e-4,
            "horizontal feet still snapped"
        );
    }

    #[test]
    fn ring_stroke_groups_an_edge_run_and_a_vertex_run_separately() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let ring = unit_square();
        let stroke = vec![
            p(2.0, 0.05),
            p(3.0, -0.05),
            p(4.0, 0.04), // run along edge 0 (y=0)
            p(9.9, 0.05),
            p(10.05, -0.03), // run onto vertex 1 (10,0)
        ];
        let out = snap_stroke_to_ring(&stroke, &ring, 0.2);
        // Edge run: each point dropped onto y=0 at its own x.
        for i in 0..3 {
            assert!(out[i].y.abs() < 1e-4, "edge point {i} on y=0: {:?}", out[i]);
            assert!((out[i].x - stroke[i].x).abs() < 1e-4);
        }
        // Vertex run: both points landed on the corner.
        assert_eq!(out[3], p(10.0, 0.0));
        assert_eq!(out[4], p(10.0, 0.0));
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 2, "edge run + vertex run = two decisions");
        assert!(decisions
            .iter()
            .all(|d| d.candidates[0].kind == SnapKind::ToPreviousOutline));
        // The vertex run is independently skippable at its representative from.
        set_snap_policy(vec![(decisions[1].from, SnapChoice::Skip)]);
        let out2 = snap_stroke_to_ring(&stroke, &ring, 0.2);
        clear_snap_policy();
        let _ = take_snap_decisions();
        assert!(out2[0].y.abs() < 1e-4, "edge run still snapped");
        assert_eq!(&out2[3..5], &stroke[3..5], "vertex run reverted");
    }

    #[test]
    fn wobble_in_and_out_of_tol_against_one_line_is_one_decision() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let segs = [[p(0.0, 5.0), p(10.0, 5.0)]];
        // A raw stroke drifting IN and OUT of tolerance against the same line:
        // grouping is by FEATURE, not adjacency, so this is still one question.
        let stroke = vec![
            p(2.0, 5.05),
            p(3.0, 5.5), // out of tol — no longer breaks the group
            p(4.0, 4.95),
            p(5.0, 5.5), // out of tol
            p(6.0, 5.05),
        ];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        for i in [0, 2, 4] {
            assert!((out[i].y - 5.0).abs() < 1e-4, "member {i} snapped");
        }
        assert_eq!(out[1], stroke[1], "out-of-tol point untouched");
        assert_eq!(out[3], stroke[3]);
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1, "ONE decision despite the tol gaps");
        assert_eq!(
            decisions[0].from, stroke[2],
            "from = the middle MEMBER (by stroke order)"
        );
        // Skip reverts every member of the group.
        set_snap_policy(vec![(decisions[0].from, SnapChoice::Skip)]);
        let out2 = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        clear_snap_policy();
        let _ = take_snap_decisions();
        assert_eq!(out2, stroke, "Skip reverted the whole feature group");
    }

    #[test]
    fn collinear_segments_share_one_feature_decision() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let segs = [
            [p(0.0, 5.0), p(4.0, 5.0)],
            [p(6.0, 5.0), p(10.0, 5.0)], // collinear continuation of the first
            [p(20.0, 0.0), p(20.0, 10.0)], // a perpendicular line — separate
        ];
        let stroke = vec![p(2.0, 5.05), p(8.0, 4.95), p(20.05, 3.0)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert!((out[0].y - 5.0).abs() < 1e-4, "onto the first piece");
        assert!((out[1].y - 5.0).abs() < 1e-4, "onto the collinear piece");
        assert!((out[2].x - 20.0).abs() < 1e-4, "onto the perpendicular");
        let decisions = take_snap_decisions();
        assert_eq!(
            decisions.len(),
            2,
            "collinear pieces are ONE feature; the perpendicular is another"
        );
    }

    #[test]
    fn sub_centimetre_single_candidate_snaps_apply_silently() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A stroke group whose every member moves < 1 cm: applied, unrecorded.
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        let stroke = vec![p(5.005, 3.0), p(4.996, 4.0)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert!(
            (out[0].x - 5.0).abs() < 1e-6 && (out[1].x - 5.0).abs() < 1e-6,
            "the invisible snap is still APPLIED"
        );
        assert!(
            take_snap_decisions().is_empty(),
            "an invisible snap asks nothing"
        );
        // A single-candidate corner hug already (almost) AT the corner — the
        // `ToCorner (x,y) -> (x,y)` no-op tag — is applied without a decision.
        let ring = unit_square();
        let out = snap_to_corner(p(10.004, 10.003), &ring, 0.2);
        assert_eq!(out, p(10.0, 10.0));
        assert!(take_snap_decisions().is_empty());
        // One VISIBLE member keeps the group's question alive.
        let stroke = vec![p(5.005, 3.0), p(5.08, 4.0)];
        let out = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert!((out[0].x - 5.0).abs() < 1e-6 && (out[1].x - 5.0).abs() < 1e-6);
        assert_eq!(take_snap_decisions().len(), 1);
    }

    #[test]
    fn commit_dedupes_identical_decisions_within_one_action() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let segs = [[p(5.0, 0.0), p(5.0, 10.0)]];
        let stroke = vec![p(5.05, 4.0)];
        // The splice pipeline runs twice per two-piece carve — the same snap
        // fires twice within one action. It must record ONCE.
        let a = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        let b = snap_stroke_to_segments(&stroke, &segs, 0.15, SnapKind::ToReferenceSegment);
        assert_eq!(a, b, "the applied geometry is unaffected by the dedup");
        assert!((a[0].x - 5.0).abs() < 1e-4, "still snapped both times");
        let decisions = take_snap_decisions();
        assert_eq!(decisions.len(), 1, "the duplicate was not re-recorded");
    }

    #[test]
    fn dist_to_ring_measures_perimeter_distance() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        assert!((dist_to_ring(&ring, p(5.0, -1.0)) - 1.0).abs() < 1e-4);
        assert!(dist_to_ring(&ring, p(10.0, 4.0)) < 1e-4, "on the ring");
        // Interior point: nearest wall, not a vertex.
        assert!((dist_to_ring(&ring, p(5.0, 2.0)) - 2.0).abs() < 1e-4);
        assert_eq!(dist_to_ring(&[p(0.0, 0.0)], p(1.0, 1.0)), f32::INFINITY);
    }

    #[test]
    fn snap_point_across_rings_prefers_vertex_then_edge_then_leaves() {
        let p = |x, y| Point2f32::new(x, y);
        let a = unit_square(); // corners at (0,0),(10,0),(10,10),(0,10)
        let b = vec![p(20.0, 0.0), p(30.0, 0.0), p(30.0, 10.0), p(20.0, 10.0)];
        let rings = [a.as_slice(), b.as_slice()];
        let _ = take_snap_decisions();
        // Near a corner of ring b → snaps to that vertex (vertex beats edge).
        assert_eq!(
            snap_point_across_rings(&rings, p(20.05, 0.04), 0.2, 0.2),
            p(20.0, 0.0)
        );
        // Off a wall of ring a, far from any corner → perpendicular edge foot.
        let q = snap_point_across_rings(&rings, p(5.0, 0.06), 0.2, 0.2);
        assert!(
            (q.x - 5.0).abs() < 1e-4 && q.y.abs() < 1e-4,
            "foot on y=0: {q:?}"
        );
        // Nothing in reach → unchanged.
        assert_eq!(
            snap_point_across_rings(&rings, p(15.0, 5.0), 0.2, 0.2),
            p(15.0, 5.0)
        );
        // Never records an elective decision.
        assert!(take_snap_decisions().is_empty());
    }

    #[test]
    fn snap_axis_rect_partial_overlap_ref_snaps_whole_side() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A rect x∈[0,4], y∈[0,5]. A SHORT vertical reference at x=0.05 spanning
        // only y∈[1,2] — it overlaps the LEFT side (which spans the full y∈[0,5])
        // by only part of its length. The whole left side must still snap onto it.
        let (lo, hi) = (p(0.05, 0.0), p(4.0, 5.0));
        let refs = [[p(0.0, 1.0), p(0.0, 2.0)]];
        let (slo, slen, pins) = snap_axis_rect_to_refs(lo, hi, &refs, 0.15);
        assert!((slo.0 - 0.0).abs() < 1e-4, "left side snapped onto x=0");
        assert!((slo.1 - 0.0).abs() < 1e-4, "bottom unmoved");
        assert!(
            (slen.0 - 4.0).abs() < 1e-4,
            "right side unmoved (len grows)"
        );
        assert_eq!(pins, [true, false, false, false], "only x_lo pinned");
        let _ = take_snap_decisions();
    }

    #[test]
    fn snap_axis_rect_no_parallel_ref_leaves_side_unpinned() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let (lo, hi) = (p(0.5, 0.5), p(4.0, 5.0));
        // A vertical ref far away in x, and a horizontal ref far away in y.
        let refs = [[p(9.0, 0.0), p(9.0, 5.0)], [p(0.0, 9.0), p(4.0, 9.0)]];
        let (slo, slen, pins) = snap_axis_rect_to_refs(lo, hi, &refs, 0.15);
        assert_eq!((slo, slen), ((0.5, 0.5), (3.5, 4.5)), "unchanged");
        assert_eq!(pins, [false; 4], "nothing in range → nothing pinned");
        assert!(take_snap_decisions().is_empty());
    }

    #[test]
    fn snap_axis_rect_diagonal_ref_never_snaps_an_axis_side() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        let (lo, hi) = (p(0.05, 0.05), p(4.0, 5.0));
        // A DIAGONAL reference passing right by the LO corner — must not snap
        // either the left (vertical) or bottom (horizontal) side.
        let refs = [[p(0.0, 0.0), p(1.0, 1.0)]];
        let (slo, slen, pins) = snap_axis_rect_to_refs(lo, hi, &refs, 0.5);
        assert_eq!((slo, slen), ((0.05, 0.05), (3.95, 4.95)), "unchanged");
        assert_eq!(pins, [false; 4], "diagonal ref snaps no axis side");
        assert!(take_snap_decisions().is_empty());
    }

    #[test]
    fn snap_axis_rect_inversion_guard_drops_the_axis() {
        let p = |x, y| Point2f32::new(x, y);
        let _ = take_snap_decisions();
        // A THIN rect x∈[0.05, 0.12] (0.07 wide) with a SINGLE vertical ref at
        // x=0.10 in reach of BOTH x-sides. Snapping each side onto it would
        // collapse the rect to zero width; the guard drops both snaps, leaving
        // the x axis exactly as drawn and unpinned.
        let (lo, hi) = (p(0.05, 0.0), p(0.12, 5.0));
        let refs = [[p(0.10, 0.0), p(0.10, 5.0)]];
        let (slo, slen, pins) = snap_axis_rect_to_refs(lo, hi, &refs, 0.15);
        assert!(
            (slo.0 - 0.05).abs() < 1e-4 && (slen.0 - 0.07).abs() < 1e-4,
            "x axis left as drawn (guard): {slo:?} {slen:?}"
        );
        assert_eq!(pins, [false; 4], "inversion guard leaves the axis unpinned");
        let _ = take_snap_decisions();
    }
}
