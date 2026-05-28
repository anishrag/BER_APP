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

use crate::Error;
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
        let abx = b.x - a.x;
        let aby = b.y - a.y;
        let len_sq = abx * abx + aby * aby;
        if len_sq < 1e-6 {
            continue;
        }
        let apx = pt.x - a.x;
        let apy = pt.y - a.y;
        let t = ((apx * abx + apy * aby) / len_sq).clamp(0.0, 1.0);
        let proj_x = a.x + t * abx;
        let proj_y = a.y + t * aby;
        let dist = ((pt.x - proj_x).powi(2) + (pt.y - proj_y).powi(2)).sqrt();
        if dist < best.2 {
            best = (i, t, dist);
        }
    }
    (best.0, best.1, best.2, false)
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

/// Force a point's perpendicular coordinate onto an axis-aligned edge of
/// `ring`, so a near-edge point sits exactly on the edge's line. For a
/// horizontal edge that means matching the edge's `y`; for a vertical edge,
/// its `x`. Used to pin an extend stroke's attach points onto their edges so
/// they pick up the edge's perpendicular coordinate expression in the splice.
pub fn pin_to_edge(p: &mut Point2f32, ring: &[Point2f32], edge: usize) {
    let n = ring.len();
    if n < 2 {
        return;
    }
    let a = ring[edge % n];
    let b = ring[(edge + 1) % n];
    if (a.y - b.y).abs() <= (a.x - b.x).abs() {
        p.y = a.y; // horizontal edge — perpendicular coord is y
    } else {
        p.x = a.x; // vertical edge — perpendicular coord is x
    }
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
