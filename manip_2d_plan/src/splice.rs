//! Ring-splice machinery shared by the Extend, Trim, and cut/carve operations.
//!
//! Given two attach points on a polygon's perimeter (each an `(edge, t)`) and an
//! orthogonalized open path between them, [`splice_sequence`] rebuilds the ring,
//! replacing one of the two perimeter arcs with the path. It emits the new ring
//! as [`SpliceElem`]s — `Keep(idx)` for a preserved existing vertex, `New(point)`
//! for an attach point or path-interior corner — which `ParametricPlan::splice_into_edge`
//! consumes, keeping every kept vertex's identity and parameter IDs intact.
//!
//! - **Extend** replaces the SHORTER arc with a porch ([`plan_splice_sequence`]).
//! - **Trim** builds both arcs and keeps whichever leaves the larger polygon.
//! - **Cut/carve** replaces a chosen arc with the cut path (either side).

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
/// Extend always replaces the SHORTER arc (via [`plan_splice_sequence`]); Trim
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
}
