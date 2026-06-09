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
pub(crate) enum SnapKind {
    /// A point snapped onto a polygon VERTEX instead of staying on the edge.
    ToVertex,
    /// A point snapped onto a nearby CORNER (it could have stayed where drawn).
    ToCorner,
    /// A wall snapped onto a PARALLEL boundary wall it runs alongside.
    ToParallelWall,
    /// A stroke point snapped onto the PREVIOUS storey's outline.
    ToPreviousOutline,
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

/// The single choke point every ELECTIVE snap routes through: move `from` → `to`
/// because of a nearby feature, when staying at `from` was also valid. Returns
/// `to` (the snap is applied). Centralising it means snaps can later be surfaced
/// on the UI and undone individually (reverting to `from`). For now it just logs.
pub(crate) fn elective_snap(kind: SnapKind, from: Point2f32, to: Point2f32) -> Point2f32 {
    log::info!(
        "[snap] {kind:?}: ({:.3}, {:.3}) -> ({:.3}, {:.3})",
        from.x,
        from.y,
        to.x,
        to.y
    );
    to
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

/// Snap `p` to the nearest ring VERTEX within `tol`; otherwise leave it. A FIND
/// (vertex only) + an elective APPLY. Lets a hug transition that lands near a
/// corner sit exactly on it.
pub(crate) fn snap_to_corner(p: Point2f32, ring: &[Point2f32], tol: f32) -> Point2f32 {
    match find_nearest(ring, p, tol).0 {
        OnRing::Vertex(i) => elective_snap(SnapKind::ToCorner, p, ring[i]),
        OnRing::Edge { .. } => p,
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
    // Foot of the perpendicular from `p` to the line through a→b (UNCLAMPED), then
    // resolve through the shared `OnRing::point` apply.
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2;
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
            elective_snap(SnapKind::ToPreviousOutline, *p, loc.point(ring))
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
    fn snap_to_corner_takes_nearby_vertex_only() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        // near corner (10,10) → snaps to it
        assert_eq!(snap_to_corner(p(9.95, 10.05), &ring, 0.2), p(10.0, 10.0));
        // mid-edge, far from any corner → unchanged
        assert_eq!(snap_to_corner(p(5.0, 0.05), &ring, 0.2), p(5.0, 0.05));
    }
}
