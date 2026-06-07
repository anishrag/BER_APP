//! Snapping points and strokes ONTO a polygon's boundary.
//!
//! These are the tolerance/projection helpers that put a freehand point or
//! stroke exactly onto an existing ring — its nearest perimeter point, a nearby
//! corner, or a specific edge's line. They build on [`crate::geom`]'s raw
//! coordinate math (`project_point_to_segment`); callers across extend, trim,
//! carve, and the new-storey flow share them.

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

/// Snap a point to a polygon's perimeter: the closest vertex if any is within
/// `vertex_tol`, else the closest point on the closest edge. Returns
/// `(edge_idx, t, distance, snapped_to_vertex)` with `t` CLAMPED to `[0, 1]`
/// (callers rebuild the point from it). When snapping to vertex `i`, returns
/// `(i, 0.0, dist, true)` — treating vertex `i` as the start of edge `i`.
pub(crate) fn snap_to_polygon(
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
        let (_q, t, dist) = geom::project_point_to_segment(pt, a, b);
        if dist < best.2 {
            best = (i, t.clamp(0.0, 1.0), dist);
        }
    }
    (best.0, best.1, best.2, false)
}

/// Snap `p` to the nearest ring VERTEX within `tol`; otherwise leave it.
/// Lets a hug transition that lands near a corner sit exactly on it.
pub(crate) fn snap_to_corner(p: Point2f32, ring: &[Point2f32], tol: f32) -> Point2f32 {
    match ring
        .iter()
        .copied()
        .map(|v| (v, (v.x - p.x).powi(2) + (v.y - p.y).powi(2)))
        .filter(|(_, d2)| *d2 <= tol * tol)
        .min_by(|(_, da), (_, db)| da.total_cmp(db))
    {
        Some((v, _)) => elective_snap(SnapKind::ToCorner, p, v),
        None => p,
    }
}

/// Pin a point exactly onto an edge of `ring` by projecting it onto that edge's
/// LINE — so it sits on the boundary while keeping its position along the edge.
/// For an axis-aligned edge this is exactly "match the edge's perpendicular
/// coordinate" (a horizontal edge fixes `y`, a vertical one `x`); for a DIAGONAL
/// edge (a curve facet) it projects onto the slanted line rather than snapping to
/// an axis, which would drag the point off the boundary. Used to pin attach/cut
/// endpoints onto their edges so they line up with the spliced boundary — extend
/// strokes attach to axis walls, a cut may reach a curve.
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
    // Foot of the perpendicular from `p` to the line through a→b. For an axis
    // edge this reduces to setting just the perpendicular coordinate (the
    // parallel one projects back to itself), so axis behaviour is unchanged.
    let t = ((p.x - a.x) * dx + (p.y - a.y) * dy) / len2;
    p.x = a.x + t * dx;
    p.y = a.y + t * dy;
}

/// Snap each point of `stroke` onto `ring`'s perimeter when within `snap_tol`,
/// leaving points farther away untouched. Used by the multi-storey "draw on
/// top of the previous storey" flow so endpoints land exactly on the previous
/// outline (so the new storey's footprint reuses those coordinates verbatim).
/// [`snap_to_polygon`] already handles closest-vertex vs closest-edge — we just
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
            let snapped = if on_vertex {
                a
            } else {
                let b = ring[(edge + 1) % n];
                Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y))
            };
            elective_snap(SnapKind::ToPreviousOutline, *p, snapped)
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
    fn snap_to_nearest_vertex_when_within_tol() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        let (edge, t, dist, is_vertex) = snap_to_polygon(&ring, p(10.5, 9.6), 2.0);
        assert_eq!(edge, 2);
        assert_eq!(t, 0.0);
        assert!(is_vertex);
        assert!(dist < 2.0);
    }

    #[test]
    fn snap_to_edge_when_far_from_any_vertex() {
        let p = |x, y| Point2f32::new(x, y);
        let ring = unit_square();
        let (edge, t, dist, is_vertex) = snap_to_polygon(&ring, p(5.0, -1.0), 2.0);
        assert_eq!(edge, 0);
        assert!((t - 0.5).abs() < 1e-3);
        assert!(!is_vertex);
        assert!((dist - 1.0).abs() < 1e-3);
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
