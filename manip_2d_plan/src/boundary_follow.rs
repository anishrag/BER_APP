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
//! SPIKE: this is the segmentation only, exercised by tests. Wiring it into
//! `carve_subarea` (snap on-boundary → outline sub-path, orthogonalize interior,
//! stitch) is the next step.
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

/// Classify each stroke point as on-boundary / interior, using a hysteresis band
/// `[on_tol, off_tol]` (with `off_tol > on_tol`): once hugging the wall, a point
/// only counts as having LEFT when it strays past `off_tol`; once interior, it
/// only RE-JOINS within `on_tol`. That keeps a noisy near-wall stroke as one run.
pub(crate) fn classify_points(
    outline: &[Point2f32],
    stroke: &[Point2f32],
    on_tol: f32,
    off_tol: f32,
) -> Vec<PointClass> {
    let mut out = Vec::with_capacity(stroke.len());
    let mut on = false;
    let mut started = false;
    for &p in stroke {
        let (edge, d) = nearest_edge(outline, p);
        if !started {
            on = d <= on_tol;
            started = true;
        } else if on {
            if d > off_tol {
                on = false;
            }
        } else if d <= on_tol {
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
                // RDP-simplify the raw freehand interior BEFORE orthogonalizing —
                // `snap_open_fixed_ends` emits one step PER POINT, so feeding it
                // hundreds of stroke points would staircase a straight cut.
                let raw = &stroke[*start..=*end];
                let simplified = crate::simplification::simplify_stroke(raw, tol)
                    .unwrap_or_else(|_| raw.to_vec());
                let mut input = vec![pin_start];
                input.extend_from_slice(&simplified);
                input.push(pin_end);
                let ortho = crate::snap_open_fixed_ends(&input, tol);
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
        let ring = build_region_ring(&outline, &stroke, &runs(&classes), 0.3);
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
        let ring = build_region_ring(&outline, &stroke, &runs(&classes), 0.3);
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
        let ring = build_region_ring(&outline, &stroke, &runs(&classes), 0.3);
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
}
