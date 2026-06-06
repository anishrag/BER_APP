//! Boundary-aware stroke segmentation — PASS 1 of the closed-loop carve.
//!
//! A freehand sub-area stroke is part *along the storey outline* (walls the area
//! shares with the house) and part *cutting through the interior* (the new
//! dividing walls). This module classifies each stroke point as ON the boundary
//! (hugging an outline edge, with hysteresis so a wobbly hand doesn't shatter the
//! run) or INTERIOR, and groups the points into runs.
//!
//! PASS 2 — assembling the carved ring from those runs (on-boundary runs follow
//! the real outline verbatim, corners/diagonals/curve facets kept; interior runs
//! orthogonalized) — lives in `carve` (`build_region_ring`).

use crate::geom;
use domain::points::Point2f32;

/// How a single stroke point relates to the outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PointClass {
    /// Hugging outline edge `edge` (index into the outline ring).
    OnBoundary { edge: usize },
    /// Off the boundary — an interior point.
    Interior,
}

/// A maximal run of consecutive same-kind points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Run {
    /// `points[start..=end]` hug the outline, covering `edges` in order
    /// (consecutive duplicates collapsed; >1 edge means it rounded a corner).
    OnBoundary {
        start: usize,
        end: usize,
        edges: Vec<usize>,
    },
    /// `points[start..=end]` are interior (the drawn dividing walls).
    Interior { start: usize, end: usize },
}

/// The nearest outline edge to `p` and the (clamped) distance to it.
fn nearest_edge(outline: &[Point2f32], p: Point2f32) -> (usize, f32) {
    let n = outline.len();
    let mut best = (0usize, f32::MAX);
    for i in 0..n {
        let a = outline[i];
        let b = outline[(i + 1) % n];
        let (_q, _t, d) = geom::project_point_to_segment(p, a, b);
        if d < best.1 {
            best = (i, d);
        }
    }
    best
}

/// Insert points along any segment longer than `max_spacing` so the stroke is
/// sampled at least that finely. The classifier looks at POINTS, so a sparse
/// stroke (e.g. a clipped/simplified interior wall represented by just its two
/// ends) can leap across the interior between two boundary edges without any
/// sample landing in the middle — making the run hug NON-ADJACENT edges, which
/// `outline_subpath` then bridges with a spurious diagonal. Densifying first
/// guarantees the interior stretch is seen and the run splits correctly. The
/// extra points are dropped again by the interior RDP simplify.
pub(crate) fn densify(path: &[Point2f32], max_spacing: f32) -> Vec<Point2f32> {
    if path.len() < 2 || !(max_spacing > 0.0) {
        return path.to_vec();
    }
    let mut out = Vec::with_capacity(path.len());
    for w in path.windows(2) {
        let (a, b) = (w[0], w[1]);
        out.push(a);
        let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
        let n = (len / max_spacing).floor() as usize;
        for i in 1..n {
            let t = i as f32 / n as f32;
            out.push(Point2f32::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t));
        }
    }
    out.push(path[path.len() - 1]);
    out
}

/// Unit direction of `stroke` at index `i`, centred over a `±w` window so hand
/// wobble doesn't flip it. `None` if the window is degenerate (coincident points).
fn stroke_dir(stroke: &[Point2f32], i: usize, w: usize) -> Option<(f32, f32)> {
    let n = stroke.len();
    if n < 2 {
        return None;
    }
    let a = i.saturating_sub(w);
    let b = (i + w).min(n - 1);
    let (dx, dy) = (stroke[b].x - stroke[a].x, stroke[b].y - stroke[a].y);
    let len = (dx * dx + dy * dy).sqrt();
    (len > 1e-6).then(|| (dx / len, dy / len))
}

/// Unit direction of outline `edge`.
fn edge_dir(outline: &[Point2f32], edge: usize) -> (f32, f32) {
    let n = outline.len();
    let (a, b) = (outline[edge], outline[(edge + 1) % n]);
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt().max(1e-6);
    (dx / len, dy / len)
}

/// Whether the stroke at `i` is HUGGING the nearest edge rather than CROSSING it.
/// Distance alone can't tell a wall you trace ALONG from an interior wall you draw
/// BETWEEN two close house walls — both sit "near a wall". The discriminator is
/// direction: hugging runs PARALLEL to the edge; a crossing runs across it. Near a
/// CORNER (within `on_tol` of two+ edges) the stroke is genuinely on the boundary
/// whatever way it points, so we keep it regardless. No direction info (coincident
/// window) falls back to "hug" so a static point doesn't spuriously split a run.
fn hugging(
    outline: &[Point2f32],
    stroke: &[Point2f32],
    i: usize,
    edge: usize,
    on_tol: f32,
) -> bool {
    let p = stroke[i];
    let m = outline.len();
    let near = (0..m)
        .filter(|&e| {
            geom::project_point_to_segment(p, outline[e], outline[(e + 1) % m]).2 <= on_tol
        })
        .count();
    if near >= 2 {
        return true; // at a corner — on the boundary regardless of heading
    }
    match stroke_dir(stroke, i, 2) {
        Some(sd) => {
            let ed = edge_dir(outline, edge);
            // Within ~60° of parallel still counts as a hug: a real trace rounding
            // a corner has its window straddle the turn (≈45°), while a crossing
            // wall is near-perpendicular (≈0). Lenient enough for corners, strict
            // enough to reject an axis wall drawn across an axis boundary.
            (sd.0 * ed.0 + sd.1 * ed.1).abs() >= 0.50
        }
        None => true,
    }
}

/// Classify each stroke point as on-boundary / interior. A point is on-boundary
/// when it is within the hysteresis band `[on_tol, off_tol]` (`off_tol > on_tol`)
/// of an outline edge AND the stroke there HUGS that edge (runs along it) rather
/// than CROSSES it — see [`hugging`]. The hysteresis keeps a noisy near-wall hug
/// as one run; the hug test stops an interior wall drawn close to (but across) a
/// house wall from being mistaken for boundary.
pub(crate) fn classify_points(
    outline: &[Point2f32],
    stroke: &[Point2f32],
    on_tol: f32,
    off_tol: f32,
) -> Vec<PointClass> {
    let mut out = Vec::with_capacity(stroke.len());
    let mut on = false;
    let mut started = false;
    for i in 0..stroke.len() {
        let (edge, d) = nearest_edge(outline, stroke[i]);
        let along = hugging(outline, stroke, i, edge, on_tol);
        if !started {
            on = d <= on_tol && along;
            started = true;
        } else if on {
            if d > off_tol || !along {
                on = false;
            }
        } else if d <= on_tol && along {
            on = true;
        }
        out.push(if on {
            PointClass::OnBoundary { edge }
        } else {
            PointClass::Interior
        });
    }
    out
}

/// Group a per-point classification into maximal runs.
pub(crate) fn runs(classes: &[PointClass]) -> Vec<Run> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < classes.len() {
        match classes[i] {
            PointClass::OnBoundary { .. } => {
                let start = i;
                let mut edges: Vec<usize> = Vec::new();
                while i < classes.len() {
                    let PointClass::OnBoundary { edge } = classes[i] else {
                        break;
                    };
                    if edges.last() != Some(&edge) {
                        edges.push(edge);
                    }
                    i += 1;
                }
                out.push(Run::OnBoundary {
                    start,
                    end: i - 1,
                    edges,
                });
            }
            PointClass::Interior => {
                let start = i;
                while i < classes.len() && matches!(classes[i], PointClass::Interior) {
                    i += 1;
                }
                out.push(Run::Interior { start, end: i - 1 });
            }
        }
    }
    out
}

/// Rotate a CLOSED-loop stroke so it starts at a boundary↔interior transition.
/// A loop can start mid-run, which would split one continuous on-boundary run
/// into a head + a tail (the two never get stitched, breaking the geometry).
/// Starting at a transition keeps every run whole. Returns the stroke unchanged
/// when there's no transition (all on-boundary — "draw around the whole thing" —
/// or all interior).
pub(crate) fn rotate_to_transition(stroke: &[Point2f32], classes: &[PointClass]) -> Vec<Point2f32> {
    let n = stroke.len();
    if n < 2 || classes.len() != n {
        return stroke.to_vec();
    }
    let is_on = |c: PointClass| matches!(c, PointClass::OnBoundary { .. });
    let start = (0..n).find(|&i| is_on(classes[i]) != is_on(classes[(i + n - 1) % n]));
    match start {
        Some(s) if s > 0 => {
            let mut out = stroke[s..].to_vec();
            out.extend_from_slice(&stroke[..s]);
            out
        }
        _ => stroke.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    // CCW square. Edge 0 = bottom (y=0), 1 = right (x=10), 2 = top, 3 = left.
    fn square() -> Vec<Point2f32> {
        vec![p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]
    }

    /// A loop carving a strip along the BOTTOM wall: the bottom run hugs edge 0,
    /// the U back through the interior is off-boundary. (The "porch" analog — the
    /// case that currently free-floats into a square.)
    #[test]
    fn bottom_strip_loop_hugs_the_bottom_edge() {
        let outline = square();
        let stroke = vec![
            p(2., 0.),
            p(4., 0.),
            p(6., 0.),
            p(8., 0.), // along the bottom wall
            p(8., 2.),
            p(8., 3.),
            p(6., 3.),
            p(4., 3.),
            p(2., 3.),
            p(2., 2.), // interior U
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        for (i, c) in classes.iter().take(4).enumerate() {
            assert_eq!(*c, PointClass::OnBoundary { edge: 0 }, "pt {i} on bottom");
        }
        for (i, c) in classes.iter().enumerate().skip(4) {
            assert_eq!(*c, PointClass::Interior, "pt {i} interior");
        }
        let r = runs(&classes);
        assert_eq!(r.len(), 2, "one boundary run + one interior run");
        assert!(matches!(&r[0], Run::OnBoundary { edges, .. } if edges == &[0]));
        assert!(matches!(r[1], Run::Interior { .. }));
    }

    /// A loop carving the bottom-right CORNER: the run hugs the bottom edge,
    /// rounds the corner, and continues up the right edge — a single on-boundary
    /// run that spans two edges. Then it cuts back through the interior.
    #[test]
    fn corner_l_hugs_two_edges_around_the_corner() {
        let outline = square();
        let stroke = vec![
            p(4., 0.),
            p(6., 0.),
            p(8., 0.),
            p(10., 0.), // bottom (edge 0) to the corner
            p(10., 2.),
            p(10., 4.), // up the right (edge 1)
            p(8., 4.),
            p(6., 4.),
            p(4., 4.),
            p(4., 2.), // interior
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        for (i, c) in classes.iter().take(6).enumerate() {
            assert!(
                matches!(c, PointClass::OnBoundary { .. }),
                "pt {i} on boundary, got {c:?}"
            );
        }
        for (i, c) in classes.iter().enumerate().skip(6) {
            assert_eq!(*c, PointClass::Interior, "pt {i} interior");
        }
        let r = runs(&classes);
        assert!(
            matches!(&r[0], Run::OnBoundary { edges, .. } if edges == &[0, 1]),
            "boundary run spans the corner (edges 0→1): {:?}",
            r[0]
        );
        assert!(matches!(r.last().unwrap(), Run::Interior { .. }));
    }

    /// An interior wall drawn deliberately INSET from the boundary (well beyond
    /// the band) must NOT be snapped onto the wall — it stays interior.
    #[test]
    fn inset_wall_is_not_snapped() {
        let outline = square();
        // A loop sitting ~2 m in from every wall — entirely interior.
        let stroke = vec![p(2., 2.), p(8., 2.), p(8., 5.), p(2., 5.)];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        assert!(
            classes.iter().all(|c| *c == PointClass::Interior),
            "inset loop stays interior (no over-snapping): {classes:?}"
        );
    }

    /// A wall-hug that wobbles in and out within the hysteresis band stays ONE
    /// on-boundary run — the hysteresis is what stops a noisy hand fragmenting it.
    /// (A single on/off threshold would split at the 0.8 point.)
    #[test]
    fn noisy_wall_hug_stays_one_run() {
        let outline = square();
        let stroke = vec![
            p(2., 0.0),
            p(4., 0.3),
            p(6., 0.8), // within [0.5, 1.5] → hysteresis keeps it ON
            p(8., 0.3),
            p(9., 0.0),
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        assert!(
            classes
                .iter()
                .all(|c| matches!(c, PointClass::OnBoundary { .. })),
            "stays on the wall through the wobble: {classes:?}"
        );
        assert_eq!(runs(&classes).len(), 1, "one run, not fragmented");
    }
}
