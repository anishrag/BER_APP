//! Boundary-aware stroke segmentation — the core of the two-pass carve.
//!
//! A freehand sub-area stroke is part *along the storey outline* (walls the area
//! shares with the house) and part *cutting through the interior* (the new
//! dividing walls). This module classifies each stroke point as ON the boundary
//! (hugging an outline edge) or INTERIOR, with hysteresis so a wobbly hand near
//! a wall doesn't shatter the run. The carve then snaps the on-boundary runs onto
//! the real outline sub-path (corners preserved) and orthogonalizes only the
//! interior runs — so it follows the boundary even on rectilinear plans, keeps
//! the side you actually drew along (no "shorter arc" porch-drop), and never
//! needs a clip afterward.
//!
//! This is the SOLE closed-loop carve path: it follows the real outline wherever
//! the stroke hugged it — corners, diagonals and curve facets walked in order —
//! and orthogonalizes the interior with cut-style steps. (It replaced the old
//! `boundary_aware_loop_points` diagonal special case, which could only follow a
//! single slanted edge.)
#![allow(dead_code)]

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

/// Project `p` onto outline `edge` (clamped to the segment).
fn project_onto_edge(outline: &[Point2f32], p: Point2f32, edge: usize) -> Point2f32 {
    let n = outline.len();
    let a = outline[edge];
    let b = outline[(edge + 1) % n];
    geom::project_point_to_segment(p, a, b).0
}

/// The real outline polyline from `entry` (on the run's first edge) to `exit`
/// (on its last edge), inserting the shared corner vertex between each pair of
/// consecutive edges the run hugged — so a run that rounds a corner keeps the
/// corner. Non-adjacent edge jumps are just connected directly.
fn outline_subpath(
    outline: &[Point2f32],
    edges: &[usize],
    entry: Point2f32,
    exit: Point2f32,
) -> Vec<Point2f32> {
    let n = outline.len();
    let mut out = vec![entry];
    for w in edges.windows(2) {
        let (a, b) = (w[0], w[1]);
        if b == (a + 1) % n {
            out.push(outline[b]); // forward corner
        } else if a == (b + 1) % n {
            out.push(outline[a]); // backward corner
        }
    }
    out.push(exit);
    out
}

/// The nearest on-boundary run to run `i`, searching cyclically in `dir`
/// (+1 forward, -1 back). Returns its `(entry, exit, edges)`.
fn neighbor_boundary(
    bverts: &[Option<(Point2f32, Point2f32, Vec<usize>)>],
    i: usize,
    dir: isize,
) -> Option<(Point2f32, Point2f32)> {
    let m = bverts.len() as isize;
    for step in 1..=m {
        let j = (i as isize + dir * step).rem_euclid(m) as usize;
        if let Some((entry, exit, _)) = &bverts[j] {
            return Some((*entry, *exit));
        }
    }
    None
}

/// Append `pts` to `ring`, skipping a point equal (within eps) to the current
/// last — so shared transition vertices aren't duplicated.
fn push_ring(ring: &mut Vec<Point2f32>, pts: &[Point2f32]) {
    for &p in pts {
        if ring.last().map_or(true, |l| {
            (l.x - p.x).abs() > 1e-3 || (l.y - p.y).abs() > 1e-3
        }) {
            ring.push(p);
        }
    }
}

/// PASS 2: build the sub-area ring from a closed loop's runs. On-boundary runs
/// become the real outline sub-path (corners preserved); interior runs are
/// orthogonalized between the bounding transition vertices (which sit on the
/// boundary). The result follows the storey edge exactly where you drew along
/// it, so it needs no clip afterward. (Open paths + the all-interior free-room
/// case are handled when this is wired into `carve_subarea`.)
pub(crate) fn build_region_ring(
    outline: &[Point2f32],
    stroke: &[Point2f32],
    runs: &[Run],
    tol: f32,
    tuning: &crate::SubareaTuning,
) -> Vec<Point2f32> {
    // Entry/exit (projected onto the boundary) for each on-boundary run.
    let bverts: Vec<Option<(Point2f32, Point2f32, Vec<usize>)>> = runs
        .iter()
        .map(|r| match r {
            Run::OnBoundary { start, end, edges } => {
                let entry = project_onto_edge(outline, stroke[*start], edges[0]);
                let exit = project_onto_edge(outline, stroke[*end], *edges.last().unwrap());
                Some((entry, exit, edges.clone()))
            }
            Run::Interior { .. } => None,
        })
        .collect();

    let mut ring: Vec<Point2f32> = Vec::new();
    for (i, r) in runs.iter().enumerate() {
        match r {
            Run::OnBoundary { .. } => {
                let (entry, exit, edges) = bverts[i].as_ref().unwrap();
                let sub = outline_subpath(outline, edges, *entry, *exit);
                push_ring(&mut ring, &sub);
            }
            Run::Interior { start, end } => {
                // Pin the interior walls to the adjacent boundary runs' vertices
                // (cyclic); fall back to the raw stroke ends if there are none.
                let pin_start = neighbor_boundary(&bverts, i, -1)
                    .map(|(_, exit)| exit)
                    .unwrap_or(stroke[*start]);
                let pin_end = neighbor_boundary(&bverts, i, 1)
                    .map(|(entry, _)| entry)
                    .unwrap_or(stroke[*end]);
                // Coarse open-polyline RDP (allow a 2-point collapse) so a roughly
                // straight wall is a clean line, not a staircase; then orthogonalize
                // with the pinned ends.
                let raw = &stroke[*start..=*end];
                let coarse = tuning.coarse_simplify_tol(geom::bbox_diagonal(raw), tol);
                let simplified = crate::simplification::simplify_open(raw, coarse);
                let mut input = vec![pin_start];
                input.extend_from_slice(&simplified);
                input.push(pin_end);
                let ortho = crate::snap_open_fixed_ends_stepped(&input);
                push_ring(&mut ring, &ortho);
            }
        }
    }
    // Drop a closing duplicate.
    if ring.len() > 1 {
        let (f, l) = (ring[0], *ring.last().unwrap());
        if (f.x - l.x).abs() < 1e-3 && (f.y - l.y).abs() < 1e-3 {
            ring.pop();
        }
    }
    ring
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    fn ring_area(ring: &[Point2f32]) -> f32 {
        let n = ring.len();
        let mut a2 = 0.0;
        for i in 0..n {
            let (u, v) = (ring[i], ring[(i + 1) % n]);
            a2 += u.x * v.y - v.x * u.y;
        }
        a2.abs() / 2.0
    }

    fn has_vertex(ring: &[Point2f32], x: f32, y: f32) -> bool {
        ring.iter()
            .any(|q| (q.x - x).abs() < 1e-2 && (q.y - y).abs() < 1e-2)
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

    /// PASS 2: the bottom-strip loop builds a region whose bottom edge lies on
    /// the real wall (y=0) — not a free-floating box.
    #[test]
    fn bottom_strip_builds_region_on_the_wall() {
        let outline = square();
        let stroke = vec![
            p(2., 0.),
            p(4., 0.),
            p(6., 0.),
            p(8., 0.),
            p(8., 2.),
            p(8., 3.),
            p(6., 3.),
            p(4., 3.),
            p(2., 3.),
            p(2., 2.),
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 18.0).abs() < 0.5,
            "strip [2,8]x[0,3] ≈ 18 m², got {} (ring {:?})",
            ring_area(&ring),
            ring
        );
        // The two bottom corners sit exactly on the wall y=0.
        assert!(has_vertex(&ring, 2.0, 0.0), "left end on the wall");
        assert!(has_vertex(&ring, 8.0, 0.0), "right end on the wall");
        assert!(
            ring.iter().filter(|q| q.y.abs() < 1e-2).count() >= 2,
            "≥2 vertices on the bottom wall"
        );
    }

    /// PASS 2: the corner loop keeps the REAL outline corner (10,0) — the run
    /// rounded it, so the region's boundary follows the house through the corner.
    #[test]
    fn corner_loop_keeps_the_real_corner() {
        let outline = square();
        let stroke = vec![
            p(4., 0.),
            p(6., 0.),
            p(8., 0.),
            p(10., 0.),
            p(10., 2.),
            p(10., 4.),
            p(8., 4.),
            p(6., 4.),
            p(4., 4.),
            p(4., 2.),
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 24.0).abs() < 0.5,
            "corner rect [4,10]x[0,4] ≈ 24 m², got {} (ring {:?})",
            ring_area(&ring),
            ring
        );
        assert!(
            has_vertex(&ring, 10.0, 0.0),
            "real outline corner (10,0) preserved in the region: {:?}",
            ring
        );
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

    /// REGRESSION: a DENSE freehand interior (hundreds of points, as a real
    /// stroke) must collapse to a clean few-segment cut — not one stair-step per
    /// point. (`snap_open_fixed_ends` steps per point, so the interior must be
    /// RDP-simplified first.)
    #[test]
    fn dense_interior_does_not_staircase() {
        let outline = square();
        let mut stroke = vec![p(2., 0.), p(4., 0.), p(6., 0.), p(8., 0.)]; // bottom, on wall
                                                                           // dense, slightly wobbly right wall going up
        for k in 1..=20 {
            let wob = if k % 2 == 0 { 0.05 } else { -0.05 };
            stroke.push(p(8.0 + wob, 0.2 + 2.6 * k as f32 / 20.0));
        }
        // dense top
        for k in 1..=10 {
            stroke.push(p(8.0 - 6.0 * k as f32 / 10.0, 3.0));
        }
        // dense left wall going down
        for k in 1..=20 {
            stroke.push(p(2.0, 3.0 - 2.6 * k as f32 / 20.0));
        }
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            ring.len() <= 8,
            "dense interior collapses to a clean cut, got {} vertices: {:?}",
            ring.len(),
            ring
        );
        assert!(
            (ring_area(&ring) - 18.0).abs() < 1.5,
            "strip ≈ 18 m², got {}",
            ring_area(&ring)
        );
    }

    /// REGRESSION (the notch bug): drawing a loop AROUND the whole outline — all
    /// boundary, no interior cut — must reproduce the full outline including a
    /// notch, NOT collapse to a box.
    #[test]
    fn draw_around_whole_outline_keeps_the_notch() {
        // 10x10 square with a [7,10]x[3,6] notch cut out of the right side.
        let outline = vec![
            p(0., 0.),
            p(10., 0.),
            p(10., 3.),
            p(7., 3.),
            p(7., 6.),
            p(10., 6.),
            p(10., 10.),
            p(0., 10.),
        ];
        let stroke = vec![
            p(0.5, 0.),
            p(5., 0.),
            p(9., 0.), // bottom
            p(10., 1.),
            p(10., 2.5), // right-lower
            p(9., 3.),
            p(7.5, 3.), // notch bottom
            p(7., 4.),
            p(7., 5.5), // notch back
            p(8., 6.),
            p(9.5, 6.), // notch top
            p(10., 7.),
            p(10., 9.5), // right-upper
            p(9., 10.),
            p(5., 10.),
            p(1., 10.), // top
            p(0., 9.),
            p(0., 5.),
            p(0., 1.), // left
        ];
        let classes = classify_points(&outline, &stroke, 0.5, 1.5);
        assert!(
            classes
                .iter()
                .all(|c| matches!(c, PointClass::OnBoundary { .. })),
            "the whole trace hugs the boundary: {classes:?}"
        );
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 91.0).abs() < 3.0,
            "L-shape ≈ 91 m² (notch kept), got {} — a box would be 100 (ring {:?})",
            ring_area(&ring),
            ring
        );
        assert!(
            has_vertex(&ring, 7.0, 3.0) && has_vertex(&ring, 7.0, 6.0),
            "notch corners preserved: {:?}",
            ring
        );
    }

    /// `build_region_ring` reproduces a DIAGONAL edge when the whole loop hugs
    /// the boundary (it walks the real outline vertices). NB: the live carve
    /// still routes diagonal *rooms* through `boundary_aware_loop_points`, which
    /// is more robust on small slanted-wall rooms — this just documents that the
    /// general builder handles a diagonal in the simple draw-around case.
    #[test]
    fn draw_around_diagonal_outline_keeps_the_diagonal() {
        // Square with the top-right corner cut by a diagonal (10,7)->(7,10).
        let outline = vec![p(0., 0.), p(10., 0.), p(10., 7.), p(7., 10.), p(0., 10.)];
        let stroke = vec![
            p(1., 0.),
            p(5., 0.),
            p(9., 0.), // bottom
            p(10., 2.),
            p(10., 5.), // right
            p(9.25, 7.75),
            p(8.5, 8.5),
            p(7.75, 9.25), // along the diagonal
            p(5., 10.),
            p(2., 10.), // top
            p(0., 7.),
            p(0., 3.),
            p(0., 0.5), // left
        ];
        let classes = classify_points(&outline, &stroke, 0.6, 1.6);
        let ring = build_region_ring(
            &outline,
            &stroke,
            &runs(&classes),
            0.3,
            &crate::SubareaTuning::default(),
        );
        assert!(
            (ring_area(&ring) - 95.5).abs() < 3.0,
            "pentagon ≈ 95.5 m² (diagonal kept), got {} (ring {:?})",
            ring_area(&ring),
            ring
        );
        assert!(
            has_vertex(&ring, 10.0, 7.0) && has_vertex(&ring, 7.0, 10.0),
            "diagonal endpoints preserved (not flattened): {:?}",
            ring
        );
    }
}
