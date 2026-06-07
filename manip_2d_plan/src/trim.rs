//! Trim / extend / curved-wall edits — in-place mutations of a FloorPlan's ring.
//!
//! Unlike `carve` (pure `ring → FloorPlan`), every entry here is `&mut self`: it
//! splices a drawn path onto the outline (extend) or removes area (trim), editing
//! the parametric model in place. So these stay `FloorPlan` methods — the pure
//! splice mechanics already live in `extend.rs`.
use crate::polygon_area;
use crate::{geom, orthogonal, simplification, snap, splice};
use crate::{Error, FloorPlan, ParameterId};
use domain::points::Point2f32;
// Test-only: the functions fully-qualify `std::collections::HashSet`; these
// names are referenced only by the test module below (via `super::*`).
#[cfg(test)]
use crate::{SpliceTuning, SubareaTuning, VertexId};
#[cfg(test)]
use std::collections::HashSet;

impl FloorPlan {
    /// Extend the primary ring by splicing a drawn path onto a wall, *adding*
    /// the enclosed area (the SHORTER arc between the attach points is replaced
    /// by the path). `extend_ortho` orthogonalizes the path (a stepped porch);
    /// see [`extend_curve`](Self::extend_curve) for a faceted arc. Untouched
    /// vertices keep their identity/parameters; returns the newly-minted
    /// parameter IDs. See [`splice_path`](Self::splice_path) for the mechanics.
    pub fn extend_ortho(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        // NOT on the shared cut pipeline: a porch's exact corner-reuse (an end on
        // an existing corner, with its 2-param depth/attach structure) doesn't
        // survive the cut's snap-walls/anchor cleanup, so extend keeps its own
        // splice path. See `splice_cleaned`.
        self.splice_path(stroke, true, false)
    }

    /// Extend a wall with a faceted freeform CURVE (adds area): the drawn arc is
    /// RDP-simplified into a short polyline and spliced onto the shorter arc.
    /// The facet walls aren't individually dimensioned and scale with the plan.
    pub fn extend_curve(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        self.splice_path(stroke, false, false)
    }

    /// Trim the primary ring with a freehand cut `stroke`: the stroke is fitted
    /// to a straight line, the smaller-area side is discarded, and the two
    /// boundary crossings are joined by a single straight (generally diagonal,
    /// i.e. NON-orthogonal) cut edge. This is how angled walls are introduced
    /// after the orthogonal base is drawn.
    ///
    /// Robustness: the cut line is the total-least-squares fit of the whole
    /// stroke (so a wavy hand-drawn line still gives a clean straight cut), and
    /// when the fitted line crosses a concave outline more than twice we keep
    /// the two crossings nearest where the stroke was actually drawn. A
    /// crossing that lands within a small tolerance of a corner collapses that
    /// would-be zero-length remnant, so a cut "through" a corner is accepted.
    ///
    /// Kept vertices keep their identity and parameters (so dimensions on the
    /// surviving walls are preserved). Each crossing vertex sits on its host
    /// edge — sharing that edge's perpendicular coordinate — but is free along
    /// it; the cut edge shares neither coordinate, hence diagonal. The result
    /// is a `ParametricPlan` that serializes like any other, so a trimmed
    /// storey saves and reloads through the normal snapshot path. Returns the
    /// newly-minted parameter IDs; errors (leaving the plan unchanged) if the
    /// line doesn't cross the outline or the result isn't a simple polygon.
    pub fn trim_by_stroke(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        use crate::splice::SpliceElem;
        if stroke.len() < 2 {
            return Err(Error::TrimError(
                "trim stroke needs at least 2 points".into(),
            ));
        }
        let ring = self.parametric.ring_points(0);
        let n = ring.len();
        if n < 3 {
            return Err(Error::TrimError("no polygon to trim".into()));
        }

        // Fit a straight line to the (possibly wavy) stroke.
        let (c, dir) = geom::fit_line(stroke);
        if dir.0.abs() < 1e-9 && dir.1.abs() < 1e-9 {
            return Err(Error::TrimError("trim stroke is degenerate".into()));
        }
        let side = |p: Point2f32| -> f32 { dir.0 * (p.y - c.y) - dir.1 * (p.x - c.x) };
        let along = |p: Point2f32| -> f32 { (p.x - c.x) * dir.0 + (p.y - c.y) * dir.1 };

        let diag = self.bounding_box_diagonal();
        // Within this of a corner, the cut is treated as passing through it
        // (the would-be zero-length remnant wall collapses).
        let corner_tol = (diag * 0.02).max(1e-4);

        // All crossings of the infinite fitted line with the outline.
        let eps = 1e-6_f32;
        let mut crossings: Vec<(usize, Point2f32, f32)> = Vec::new(); // (edge, pt, along)
        for i in 0..n {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            let sa = side(a);
            let sb = side(b);
            if (sa > eps && sb < -eps) || (sa < -eps && sb > eps) {
                let t = sa / (sa - sb);
                let pt = Point2f32::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y));
                crossings.push((i, pt, along(pt)));
            }
        }
        // Crossings that pass exactly through a vertex (the boundary changes
        // side there — a transversal, not a tangent touch). The interior-edge
        // loop above skips these (it needs strictly-opposite signs). Recording
        // on the edge ENDING at the vertex lets the at-corner omission below
        // reuse the existing corner, so a cut "through" a corner is accepted
        // and leaves no zero-length remnant.
        for v in 0..n {
            if side(ring[v]).abs() < eps {
                let sp = side(ring[(v + n - 1) % n]);
                let sn = side(ring[(v + 1) % n]);
                if (sp > eps && sn < -eps) || (sp < -eps && sn > eps) {
                    crossings.push(((v + n - 1) % n, ring[v], along(ring[v])));
                }
            }
        }
        if crossings.len() < 2 {
            return Err(Error::TrimError(format!(
                "cut line must cross the outline (crossed {})",
                crossings.len()
            )));
        }

        // Keep the two crossings matching the stroke's drawn span — nearest the
        // start projection and nearest the end projection. This ignores the
        // extra crossings a concave outline produces far from the actual cut.
        let a_start = along(*stroke.first().unwrap());
        let a_end = along(*stroke.last().unwrap());
        let nearest = |target: f32, skip: Option<usize>| -> usize {
            let mut best = usize::MAX;
            let mut best_d = f32::INFINITY;
            for (i, cr) in crossings.iter().enumerate() {
                if Some(i) == skip {
                    continue;
                }
                let d = (cr.2 - target).abs();
                if d < best_d {
                    best_d = d;
                    best = i;
                }
            }
            best
        };
        let i0 = nearest(a_start, None);
        let i1 = nearest(a_end, Some(i0));
        let (e1, pa, _) = crossings[i0];
        let (e2, pb, _) = crossings[i1];
        if e1 == e2 {
            return Err(Error::TrimError(
                "cut enters and leaves through the same wall".into(),
            ));
        }

        // Two candidate kept polygons (one per arc), closed by the straight cut
        // edge; keep the larger. A boundary within `corner_tol` of its first
        // kept vertex is omitted so an at-corner cut collapses cleanly.
        let arc = |from_edge: usize, to_edge: usize, first: Point2f32, last: Point2f32| {
            let mut seq: Vec<SpliceElem> = Vec::new();
            let fk = ring[(from_edge + 1) % n];
            if ((first.x - fk.x).powi(2) + (first.y - fk.y).powi(2)).sqrt() > corner_tol {
                seq.push(SpliceElem::New(first));
            }
            let mut i = (from_edge + 1) % n;
            loop {
                seq.push(SpliceElem::Keep(i));
                if i == to_edge {
                    break;
                }
                i = (i + 1) % n;
            }
            let lk = ring[to_edge];
            if ((last.x - lk.x).powi(2) + (last.y - lk.y).powi(2)).sqrt() > corner_tol {
                seq.push(SpliceElem::New(last));
            }
            seq
        };
        let seq_a = arc(e1, e2, pa, pb);
        let seq_b = arc(e2, e1, pb, pa);

        let area_of = |seq: &[SpliceElem]| -> f32 {
            let pts: Vec<Point2f32> = seq
                .iter()
                .map(|e| match e {
                    SpliceElem::Keep(idx) => ring[*idx],
                    SpliceElem::New(p) => *p,
                })
                .collect();
            polygon_area(&pts).abs()
        };
        let chosen = if area_of(&seq_a) >= area_of(&seq_b) {
            seq_a
        } else {
            seq_b
        };
        self.parametric.splice_into_edge(0, &chosen)
    }

    /// Convenience: trim along the straight line through two explicit points.
    pub fn trim_by_line(
        &mut self,
        p0: Point2f32,
        p1: Point2f32,
    ) -> Result<Vec<ParameterId>, Error> {
        self.trim_by_stroke(&[p0, p1])
    }

    /// Trim with an orthogonal cut PATH (removing area): an orthogonal
    /// notch/chamfer (vs the straight diagonal [`trim_by_stroke`](Self::trim_by_stroke)).
    ///
    /// Shares the living-area/phase CUT's pipeline:
    /// [`cut_outline`](crate::carve::cut_outline) (orthogonalize to the storey
    /// axes + attach ends) → [`splice_sequence`](crate::splice::splice_sequence)
    /// (keep the larger side) → [`clean_region_ring`](crate::carve::clean_region_ring)
    /// (untangle / square / snap-walls). The cleanup is what makes the result
    /// valid where a raw in-place splice would self-cross. It then **re-derives
    /// the splice by position** and finalizes with `splice_into_edge`, so the
    /// surviving walls keep their `VertexId`/`ParameterId` (and dimensions) — only
    /// the new trim walls mint params. (The cut finalize, `build_region`, rebuilds
    /// fresh instead; that's the only divergence from the cut.)
    pub fn trim_ortho(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        use crate::splice::SpliceElem;
        let err = |m: &str| Error::TrimError(m.into());
        let ring = self.parametric.ring_points(0);
        if ring.len() < 3 {
            return Err(err("no existing polygon"));
        }
        let diag = self.bounding_box_diagonal();
        let merge_tol = if diag > 0.0 {
            (diag * 0.01).max(1e-4)
        } else {
            10.0
        };
        // Widen the reach to the historical tolerance so an end drawn a bit off
        // the wall still attaches.
        let tuning = crate::SubareaTuning {
            cut_reach_tol_ratio: 0.25,
            ..crate::SubareaTuning::default()
        };
        let plan = crate::carve::cut_outline(&ring, stroke, merge_tol, &tuning)?
            .ok_or_else(|| err("orthogonal path doesn't reach the outline"))?;

        // Splice BOTH sides; keep the larger-area one.
        let resolve = |seq: &[SpliceElem]| -> Vec<Point2f32> {
            seq.iter()
                .map(|el| match el {
                    SpliceElem::Keep(i) => ring[*i],
                    SpliceElem::New(p) => *p,
                })
                .collect()
        };
        let pts_a = resolve(&crate::splice::splice_sequence(
            &ring,
            plan.e1,
            plan.t1,
            plan.e2,
            plan.t2,
            &plan.opath,
            true,
        ));
        let pts_b = resolve(&crate::splice::splice_sequence(
            &ring,
            plan.e1,
            plan.t1,
            plan.e2,
            plan.t2,
            &plan.opath,
            false,
        ));
        let pts = if polygon_area(&pts_a).abs() >= polygon_area(&pts_b).abs() {
            pts_a
        } else {
            pts_b
        };
        record_phase("spliced ring", &pts, true);

        // Shared cut cleanup (records "cleaned ring").
        let cleaned = crate::carve::clean_region_ring(&ring, &pts);
        if cleaned.len() < 3 {
            return Err(err("collapsed the shape to a line"));
        }

        // Re-derive the splice by POSITION so identity is preserved: a cleaned
        // vertex coinciding with an existing ring vertex REUSES it (`Keep` → keeps
        // its IDs/dimension); only the genuinely-new walls mint params.
        let eps = (diag * 0.002).max(1e-4);
        let seq: Vec<SpliceElem> = cleaned
            .iter()
            .map(|p| {
                match ring
                    .iter()
                    .position(|v| (v.x - p.x).abs() < eps && (v.y - p.y).abs() < eps)
                {
                    Some(i) => SpliceElem::Keep(i),
                    None => SpliceElem::New(*p),
                }
            })
            .collect();
        self.parametric.splice_into_edge(0, &seq)
    }

    /// Trim a wall with a faceted freeform CURVE (removing area): the drawn arc
    /// is RDP-simplified into a short polyline and the smaller-area side is
    /// replaced by it — e.g. a wall recessed around a tower. Facet walls aren't
    /// individually dimensioned and scale with the plan.
    pub fn trim_curve(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        self.splice_path(stroke, false, true)
    }

    /// Shared mechanics for the four path-splice operations
    /// ([`extend_ortho`](Self::extend_ortho), [`extend_curve`](Self::extend_curve),
    /// [`trim_ortho`](Self::trim_ortho), [`trim_curve`](Self::trim_curve)). Snaps
    /// the stroke's endpoints to the outline, cleans the path (`orthogonalize` →
    /// stepped H/V; else RDP-faceted curve), pins the endpoints onto their
    /// edges, and splices the path in — replacing the SHORTER arc when extending
    /// (`!keep_larger`, adds area) or the smaller-area side when trimming
    /// (`keep_larger`, removes area). Untouched vertices keep their identity and
    /// parameters; returns the newly-minted parameter IDs. The plan is left
    /// unchanged on error. Tolerances scale with the bounding-box diagonal, so
    /// it works in pixels (pre-dimensioning) or metres (post-dimensioning).
    pub(crate) fn splice_path(
        &mut self,
        stroke: &[Point2f32],
        orthogonalize: bool,
        keep_larger: bool,
    ) -> Result<Vec<ParameterId>, Error> {
        let err = |m: String| {
            if keep_larger {
                Error::TrimError(m)
            } else {
                Error::ExtendError(m)
            }
        };
        let diag = self.bounding_box_diagonal();
        let merge_tol = if diag > 0.0 {
            (diag * 0.01).max(1e-4)
        } else {
            10.0
        };
        // DEBUG overlay — capture every pipeline stage, in order.
        // STAGE: the raw freehand stroke, before any processing.
        record_phase("raw stroke", stroke, false);

        // STAGE: RDP-simplify (drop jitter / redundant points). Both the Ortho
        // and Curve tools simplify; only Ortho then squares to H/V.
        let simplified = simplification::simplify_stroke(stroke, merge_tol)?;
        record_phase("simplified (RDP)", &simplified, false);

        // STAGE(S): Ortho squares to H/V — `make_orthogonal` records its OWN
        // internal sub-stages ("snapped to H/V", then "orthogonalized (cleaned)").
        // Curve keeps the faceted arc as-is.
        let path = if orthogonalize {
            // Snap to the STOREY's axes (base_angle = 0), NOT the stroke's own
            // dominant angle — otherwise a tilted hand-drawn notch squares to a
            // tilted grid instead of lining up with the (axis-aligned) outline.
            // Strictly OPEN — the two ends attach to the outline separately and
            // must never be joined into a loop.
            orthogonal::make_orthogonal_at(&simplified, merge_tol, 0.0, orthogonal::Closure::Open)?
        } else {
            simplified
        };
        if path.len() < 2 {
            return Err(err("path collapsed".into()));
        }

        self.splice_cleaned(&path, keep_larger)
    }

    /// Splice an already-clean `path` (no further simplification) onto the
    /// primary ring: snap its endpoints to the outline, pin them onto their
    /// attach edges, and replace the SHORTER arc when extending
    /// (`!keep_larger`) or the smaller-area side when trimming (`keep_larger`).
    /// The shared tail of every path-splice — `splice_path` cleans first, the
    /// arc flow ([`begin_arc`](Self::begin_arc)) feeds it a ready 2-point chord.
    pub(crate) fn splice_cleaned(
        &mut self,
        path: &[Point2f32],
        keep_larger: bool,
    ) -> Result<Vec<ParameterId>, Error> {
        let err = |m: String| {
            if keep_larger {
                Error::TrimError(m)
            } else {
                Error::ExtendError(m)
            }
        };
        let ring = self.parametric.ring_points(0);
        if ring.len() < 3 {
            return Err(err("no existing polygon to modify".into()));
        }
        if path.len() < 2 {
            return Err(err("path collapsed".into()));
        }
        let diag = self.bounding_box_diagonal();
        let vertex_tol = (diag * 0.04).max(1e-4);
        let max_tol = (diag * 0.25).max(1e-3);

        // Snap the path's endpoints to find the attach edges, then pin them onto
        // those edges so they share the edges' perpendicular coordinate exprs.
        // `snapped*` is true when the endpoint landed on an existing CORNER.
        let start = path[0];
        let end = path[path.len() - 1];
        let (e1, t1, s_dist, snapped1) = snap::snap_to_polygon(&ring, start, vertex_tol);
        let (e2, t2, e_dist, snapped2) = snap::snap_to_polygon(&ring, end, vertex_tol);
        // Elective: an extend/trim endpoint that landed on an existing CORNER
        // snapped to it (it could have attached mid-edge).
        if snapped1 {
            snap::elective_snap(snap::SnapKind::ToVertex, start, ring[e1]);
        }
        if snapped2 {
            snap::elective_snap(snap::SnapKind::ToVertex, end, ring[e2]);
        }
        if s_dist > max_tol || e_dist > max_tol {
            return Err(err(format!(
                "endpoints too far from the outline (start={:.3}, end={:.3}, max={:.3})",
                s_dist, e_dist, max_tol
            )));
        }
        if e1 == e2 && (t1 - t2).abs() < 1e-3 {
            return Err(err("stroke starts and ends at the same point".into()));
        }

        let mut path = path.to_vec();
        let last = path.len() - 1;
        snap::pin_to_edge(&mut path[0], &ring, e1);
        snap::pin_to_edge(&mut path[last], &ring, e2);

        // STAGE: the path with its two ENDPOINTS pinned onto the outline edges
        // (e1/e2) — its ends now sit on the boundary. (This is the #2→#3 seam
        // where a sideways pin can re-tilt a squared end into a diagonal.)
        record_phase("pinned to outline", &path, false);

        let mut chosen = if keep_larger {
            // Trim: build both arc-replacements; keep whichever leaves more area.
            let area_of = |seq: &[crate::splice::SpliceElem]| -> f32 {
                let pts: Vec<Point2f32> = seq
                    .iter()
                    .map(|e| match e {
                        crate::splice::SpliceElem::Keep(idx) => ring[*idx],
                        crate::splice::SpliceElem::New(pp) => *pp,
                    })
                    .collect();
                polygon_area(&pts).abs()
            };
            let seq_a = splice::splice_sequence(&ring, e1, t1, e2, t2, &path, true);
            let seq_b = splice::splice_sequence(&ring, e1, t1, e2, t2, &path, false);
            if area_of(&seq_a) >= area_of(&seq_b) {
                seq_a
            } else {
                seq_b
            }
        } else {
            // Extend: replace the SHORTER arc with the path (adds the feature).
            splice::plan_splice_sequence(&ring, e1, t1, e2, t2, &path)
        };

        // Corner reuse. When an attach endpoint snaps onto an EXISTING corner
        // vertex, reuse that vertex (Keep) instead of splicing a new point.
        // Otherwise the new point — pinned to one wall's perpendicular while its
        // surviving neighbour sits on the PERPENDICULAR wall — becomes a
        // free-floating vertex (both coords independent) that the dimensioning
        // solver can never isolate (it only prompts walls with exactly one
        // unknown). That strands a curve drawn across a corner, and makes an
        // orthogonal porch attached AT a corner over-parametrise and bend the
        // (already-fixed) wall it joins. Reusing the corner attaches at it, with
        // the corner's own already-known params and no extra param. Draw the
        // endpoint clearly off a corner to attach elsewhere. The guard avoids a
        // duplicate Keep when the corner happens to survive in the splice walk.
        use crate::splice::SpliceElem;
        let kept: std::collections::HashSet<usize> = chosen
            .iter()
            .filter_map(|el| match el {
                SpliceElem::Keep(i) => Some(*i),
                SpliceElem::New(_) => None,
            })
            .collect();
        let a1 = path[0];
        let a2 = path[last];
        let m = chosen.len();
        // Plan the reuses (and the realignment each implies) first, then apply —
        // so we don't borrow `chosen` mutably while reading neighbours.
        let mut ops: Vec<(usize, SpliceElem)> = Vec::new();
        for j in 0..m {
            let SpliceElem::New(pt) = chosen[j] else {
                continue;
            };
            let corner = if snapped1 && pt == a1 && !kept.contains(&e1) {
                Some(e1)
            } else if snapped2 && pt == a2 && !kept.contains(&e2) {
                Some(e2)
            } else {
                None
            };
            let Some(e) = corner else { continue };
            let c = ring[e];
            ops.push((j, SpliceElem::Keep(e)));
            // The path was orthogonalized to the DRAWN endpoint; snapping that
            // endpoint to the corner can leave the adjacent porch-wall point off
            // the corner's line, tilting the wall. Pull that neighbour onto the
            // corner's shared coordinate so the wall stays axis-aligned. Skip the
            // OTHER attach endpoint (a curve's chord, deliberately diagonal) and
            // any genuinely-diagonal neighbour.
            for nb in [(j + m - 1) % m, (j + 1) % m] {
                let SpliceElem::New(npt) = chosen[nb] else {
                    continue;
                };
                if npt == a1 || npt == a2 {
                    continue;
                }
                let mut np = npt;
                if (pt.x - npt.x).abs() < 1e-3 {
                    np.x = c.x; // vertical porch wall → share the corner's x
                    ops.push((nb, SpliceElem::New(np)));
                } else if (pt.y - npt.y).abs() < 1e-3 {
                    np.y = c.y; // horizontal porch wall → share the corner's y
                    ops.push((nb, SpliceElem::New(np)));
                }
            }
        }
        for (idx, val) in ops {
            chosen[idx] = val;
        }

        // STAGE: the SPLICED RING (the `chosen` sequence resolved to points) —
        // the candidate result handed to `splice_into_edge`. For a bad stroke
        // this is where the ring SELF-CROSSES and gets rejected.
        let spliced: Vec<Point2f32> = chosen
            .iter()
            .map(|el| match el {
                SpliceElem::Keep(i) => ring[*i],
                SpliceElem::New(p) => *p,
            })
            .collect();
        record_phase("spliced ring", &spliced, true);

        self.parametric.splice_into_edge(0, &chosen)
    }
}

/// Toggle capture of the splice-pipeline phases for the app's debug overlay.
const DEBUG_DRAW: bool = true;

/// One captured phase of the splice pipeline — a labelled path/ring the app
/// draws as a coloured debug overlay. `closed` joins the last point to the first.
#[derive(Clone, Debug)]
pub struct SplicePhase {
    pub label: String,
    pub points: Vec<Point2f32>,
    pub closed: bool,
}

thread_local! {
    static SPLICE_PHASES: std::cell::RefCell<Vec<SplicePhase>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Drain the splice-pipeline phases recorded since the last drain. The shell
/// drains BEFORE a splice op (to clear stale phases) and again AFTER to capture
/// that op's phases for the overlay.
pub fn take_splice_debug() -> Vec<SplicePhase> {
    SPLICE_PHASES.with(|p| std::mem::take(&mut *p.borrow_mut()))
}

/// Record one splice phase for the debug overlay, in CALL order (the UI numbers
/// them). Only the path/ring is captured. No-op unless `DEBUG_DRAW`. The shell
/// drains these with [`take_splice_debug`]. `pub(crate)` so the orthogonalizer
/// can record its own internal sub-stages.
pub(crate) fn record_phase(label: &str, shape: &[Point2f32], closed: bool) {
    if !DEBUG_DRAW || shape.is_empty() {
        return;
    }
    // Also dump the coordinates to the console so a stage can be inspected /
    // shared without the UI. Numbered to match the overlay's legend order is the
    // UI's job; here we print label + every point.
    log::info!(
        "[splice stage] {label} ({} pts, closed={closed}): {shape:?}",
        shape.len()
    );
    SPLICE_PHASES.with(|p| {
        p.borrow_mut().push(SplicePhase {
            label: label.to_string(),
            points: shape.to_vec(),
            closed,
        });
    });
}

#[cfg(test)]
mod trim_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// `outline_following_trace` keeps a CURVE the trace followed (the boundary
    /// is copied verbatim there) while squaring off only a wall the trace added —
    /// the partial-reproduce a whole-trace orthogonalize would have flattened.
    #[test]
    fn outline_following_trace_keeps_curve_and_squares_new_wall() {
        // Previous storey: a box whose RIGHT side bulges out via diagonal facets
        // (a persisted "curve"); everything else axis-aligned.
        let prev_ring = vec![
            p(0., 0.),
            p(4., 0.),
            p(4.3, 1.),
            p(4.5, 2.),
            p(4.3, 3.),
            p(4., 4.),
            p(0., 4.),
        ];
        let prev = FloorPlan::from_clean_ring(&prev_ring).unwrap();

        // Trace: hug the whole outline, but on the BOTTOM edge dip into a 1 m-deep
        // porch bump (a genuinely-new wall well off the boundary).
        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            let steps = 24;
            for s in 0..steps {
                let f = s as f32 / steps as f32;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        let mut trace = Vec::new();
        // bottom with a downward bump between x=1.5 and x=2.5
        sample(p(0., 0.), p(1.5, 0.), &mut trace);
        sample(p(1.5, 0.), p(1.5, -1.0), &mut trace);
        sample(p(1.5, -1.0), p(2.5, -1.0), &mut trace);
        sample(p(2.5, -1.0), p(2.5, 0.), &mut trace);
        sample(p(2.5, 0.), p(4., 0.), &mut trace);
        // up the curved right side, across the top, down the left — verbatim path
        sample(p(4., 0.), p(4.3, 1.), &mut trace);
        sample(p(4.3, 1.), p(4.5, 2.), &mut trace);
        sample(p(4.5, 2.), p(4.3, 3.), &mut trace);
        sample(p(4.3, 3.), p(4., 4.), &mut trace);
        sample(p(4., 4.), p(0., 4.), &mut trace);
        sample(p(0., 4.), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a follow/diverge trace should splice");
        let spliced = FloorPlan::from_clean_ring(&out).unwrap();

        // The curve survived: a diagonal (non-axis-aligned) edge remains.
        assert!(
            !spliced.non_orthogonal_edges().is_empty(),
            "the followed curve facets must be preserved verbatim, not squared off"
        );
        // The bulge vertices are carried up verbatim.
        let pts = spliced.get_points();
        assert!(
            pts.iter()
                .any(|q| (q.x - 4.5).abs() < 1e-3 && (q.y - 2.0).abs() < 1e-3),
            "the curve apex (4.5, 2.0) is copied from the storey below"
        );
        // The new porch bump is present (a wall well below the boundary).
        assert!(
            pts.iter().any(|q| q.y < -0.3),
            "the diverging porch wall is kept (squared off, but present)"
        );

        // Contrast: a pure full-follow (no bump) is NOT ours — it's a reproduce.
        let mut full = Vec::new();
        sample(p(0., 0.), p(4., 0.), &mut full);
        sample(p(4., 0.), p(4.3, 1.), &mut full);
        sample(p(4.3, 1.), p(4.5, 2.), &mut full);
        sample(p(4.5, 2.), p(4.3, 3.), &mut full);
        sample(p(4.3, 3.), p(4., 4.), &mut full);
        sample(p(4., 4.), p(0., 4.), &mut full);
        sample(p(0., 4.), p(0., 0.), &mut full);
        assert!(
            prev.outline_following_trace(&full, 0.3, 0.05, &SpliceTuning::default())
                .is_none(),
            "an all-following trace is a pure reproduce, not a splice"
        );
    }

    /// A single cut wall that GRAZES an interior step of the storey below (runs
    /// close and parallel to it for a stretch) must stay ONE wall — not split
    /// into two by the brief "following" graze, which would inject a spurious
    /// vertex and turn one dimension into three.
    #[test]
    fn outline_following_trace_keeps_a_grazing_cut_wall_whole() {
        // Storey below: a box with the LEFT side curved, and a SMALL step on the
        // right (an interior vertical edge at x=2.7, y 1.4→1.7).
        let prev = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(3., 0.),
            p(3., 1.4),
            p(2.7, 1.4),
            p(2.7, 1.7),
            p(3., 1.7),
            p(3., 3.),
            p(0., 3.),
            p(-0.25, 2.2),
            p(-0.35, 1.5),
            p(-0.25, 0.8),
        ])
        .unwrap();

        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            for s in 0..40 {
                let f = s as f32 / 40.0;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        // Cut at x=2.6 — 0.1 m left of the step, so the wall grazes it for
        // y 1.4→1.7 (within the follow tolerance).
        let mut trace = Vec::new();
        sample(p(0., 0.), p(2.6, 0.), &mut trace);
        sample(p(2.6, 0.), p(2.6, 3.), &mut trace);
        sample(p(2.6, 3.), p(0., 3.), &mut trace);
        sample(p(0., 3.), p(-0.25, 2.2), &mut trace);
        sample(p(-0.25, 2.2), p(-0.35, 1.5), &mut trace);
        sample(p(-0.35, 1.5), p(-0.25, 0.8), &mut trace);
        sample(p(-0.25, 0.8), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a cut past a step still splices");
        let n = out.len();
        // Exactly ONE long vertical wall (the cut) — not two split by the graze.
        let cut_walls = (0..n)
            .filter(|&i| {
                let (a, b) = (out[i], out[(i + 1) % n]);
                (a.x - b.x).abs() < 0.05 && (a.y - b.y).abs() > 1.5
            })
            .count();
        assert_eq!(cut_walls, 1, "the grazing cut wall stays one piece");
    }

    /// A new wall that CROSSES interior edges of the storey below (e.g. a cut
    /// that passes over a notch) must stay one clean wall — no spurious vertex
    /// injected at each crossing, and the notch's interior vertices (which the
    /// trace never visited) must not be pulled in.
    #[test]
    fn outline_following_trace_no_vertex_where_new_wall_crosses_below_storey() {
        // Storey below: a box with a NOTCH on the right (interior horizontal
        // edges at y=1 and y=2, x 2→4) and a curved LEFT side.
        let prev = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(4., 0.),
            p(4., 1.),
            p(2., 1.),
            p(2., 2.),
            p(4., 2.),
            p(4., 3.),
            p(0., 3.),
            p(-0.25, 2.2),
            p(-0.35, 1.5),
            p(-0.25, 0.8),
        ])
        .unwrap();

        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            for s in 0..40 {
                let f = s as f32 / 40.0;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        // Cut straight up at x=3 — crosses the notch edges at (3,1) and (3,2).
        let mut trace = Vec::new();
        sample(p(0., 0.), p(3., 0.), &mut trace);
        sample(p(3., 0.), p(3., 3.), &mut trace);
        sample(p(3., 3.), p(0., 3.), &mut trace);
        sample(p(0., 3.), p(-0.25, 2.2), &mut trace);
        sample(p(-0.25, 2.2), p(-0.35, 1.5), &mut trace);
        sample(p(-0.35, 1.5), p(-0.25, 0.8), &mut trace);
        sample(p(-0.25, 0.8), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a cut over a notch still splices");
        let n = out.len();
        // No vertex at the crossings (3,1) / (3,2) — the cut wall isn't split.
        for cross in [(3.0_f32, 1.0_f32), (3.0, 2.0)] {
            assert!(
                !out.iter()
                    .any(|q| (q.x - cross.0).abs() < 0.1 && (q.y - cross.1).abs() < 0.1),
                "no spurious vertex at the crossing {cross:?}"
            );
        }
        // The notch's interior corners (x=2) were never traced → not pulled in.
        assert!(
            !out.iter()
                .any(|q| (q.x - 2.0).abs() < 0.1 && (q.y - 1.0).abs() < 0.2),
            "the unvisited notch interior is not included"
        );
        // The cut is still exactly ONE long vertical wall.
        let cut_walls = (0..n)
            .filter(|&i| {
                let (a, b) = (out[i], out[(i + 1) % n]);
                (a.x - b.x).abs() < 0.05 && (a.y - b.y).abs() > 1.5
            })
            .count();
        assert_eq!(cut_walls, 1, "the cut crossing the notch stays one wall");
    }

    /// A new wall drawn TILTED (its two ends, where the trace left and rejoined
    /// the boundary, don't share a coordinate) is squared onto its axis — so
    /// dimensioning an adjacent wall later doesn't flip its lean and get rejected.
    #[test]
    fn outline_following_trace_squares_a_tilted_new_wall() {
        // 3x3 box with a curved LEFT side (diagonal facets to preserve).
        let prev = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(3., 0.),
            p(3., 3.),
            p(0., 3.),
            p(-0.25, 2.2),
            p(-0.35, 1.5),
            p(-0.25, 0.8),
        ])
        .unwrap();

        let sample = |a: Point2f32, b: Point2f32, out: &mut Vec<Point2f32>| {
            for s in 0..24 {
                let f = s as f32 / 24.0;
                out.push(p(a.x + f * (b.x - a.x), a.y + f * (b.y - a.y)));
            }
        };
        // Follow the bottom, then a clearly-set-back right wall drawn with a
        // TILT (x=2.5 at the bottom, x=2.6 at the top), then the top and the
        // curved left side.
        let mut trace = Vec::new();
        sample(p(0., 0.), p(2.5, 0.), &mut trace);
        sample(p(2.5, 0.), p(2.6, 3.), &mut trace);
        sample(p(2.6, 3.), p(0., 3.), &mut trace);
        sample(p(0., 3.), p(-0.25, 2.2), &mut trace);
        sample(p(-0.25, 2.2), p(-0.35, 1.5), &mut trace);
        sample(p(-0.35, 1.5), p(-0.25, 0.8), &mut trace);
        sample(p(-0.25, 0.8), p(0., 0.), &mut trace);

        let out = prev
            .outline_following_trace(&trace, 0.3, 0.05, &SpliceTuning::default())
            .expect("a set-back right wall splices");
        let n = out.len();
        // The long near-vertical new wall is now EXACTLY vertical (dx≈0).
        let tilted = (0..n).any(|i| {
            let (a, b) = (out[i], out[(i + 1) % n]);
            let (dx, dy) = ((a.x - b.x).abs(), (a.y - b.y).abs());
            dy > 2.0 && dx > 1e-3 // a long vertical-ish wall still off-axis
        });
        assert!(!tilted, "the new wall was squared onto its axis");
        // The left curve is still there.
        assert!(
            !FloorPlan::from_clean_ring(&out)
                .unwrap()
                .non_orthogonal_edges()
                .is_empty(),
            "the followed curve is preserved"
        );
    }

    /// `uncovered_perimeter_run` flags a side of the outline the trace never
    /// visited — what tells a real reproduce from a smaller storey that stops
    /// early on a side (even when its new walls snap onto the outline elsewhere).
    #[test]
    fn uncovered_perimeter_run_detects_a_skipped_side() {
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(10., 0.), p(10., 10.), p(0., 10.)]).unwrap();
        // Cover bottom, right, top — but NOT the left wall (x=0). Stay off the
        // corners so the left edge truly has nothing within tolerance.
        let mut pts = Vec::new();
        for k in 1..20 {
            let f = k as f32 * 0.5;
            pts.push(p(f, 0.0)); // bottom
            pts.push(p(10.0, f)); // right
            pts.push(p(f, 10.0)); // top
        }
        let un = outline.uncovered_perimeter_run(&pts, 0.3);
        assert!(
            (un - 10.0).abs() < 1.0,
            "the unvisited left wall (~10 m) is the gap, got {un}"
        );
        // Now trace the left wall too → fully covered.
        for k in 1..20 {
            pts.push(p(0.0, k as f32 * 0.5));
        }
        assert!(
            outline.uncovered_perimeter_run(&pts, 0.3) < 1.0,
            "fully covered now"
        );
    }

    /// A living area cut from a storey with a CURVED wall must follow the whole
    /// curve — including the curve-START corner where the straight wall meets the
    /// first (gentle, near-tangent) facet. That facet's small off-axis delta was
    /// previously mistaken for "collinear" with the straight wall and the corner
    /// dropped, so the region skipped a vertex and stopped hugging the boundary.
    #[test]
    fn cut_region_follows_curve_including_the_start_vertex() {
        // Box whose top-left is a curve down to (0, 2.5); the LAST facet
        // (0.015,2.6)->(0,2.5) is nearly tangent to the straight left wall below.
        let outline = FloorPlan::from_clean_ring(&[
            p(0., 0.),
            p(4., 0.),
            p(4., 4.),
            p(1., 4.),
            p(0.5, 3.6),
            p(0.2, 3.0),
            p(0.015, 2.6),
            p(0., 2.5),
        ])
        .unwrap();
        // Sanity: the outline itself keeps the curve-start corner.
        let has = |r: &[Point2f32], x: f32, y: f32| {
            r.iter()
                .any(|q| (q.x - x).abs() < 2e-2 && (q.y - y).abs() < 2e-2)
        };
        assert!(
            has(&outline.get_points(), 0.0, 2.5),
            "outline keeps the curve-start corner"
        );
        // Vertical cut at x=2 → left (curved) region + right region.
        let cut = vec![p(2., 0.2), p(2., 3.8)];
        let (ra, rb) =
            crate::carve::split_cut(&outline.get_points(), &cut, 0.05, &SubareaTuning::default())
                .unwrap()
                .expect("vertical cut splits the storey");
        // The region carrying the curve must still include the curve-start corner.
        let left = if has(&ra.get_points(), 1.0, 4.0) {
            ra.get_points()
        } else {
            rb.get_points()
        };
        assert!(has(&left, 1.0, 4.0), "curved region has a curve point");
        assert!(
            has(&left, 0.0, 2.5),
            "curved region includes the curve-start corner (not skipped): {left:?}"
        );
    }

    /// A living-area vertex that sits MID-facet on the storey's curved/slanted
    /// boundary is pinned (so the curve-following wall isn't prompted) even
    /// though it's neither an outline corner nor part of a region edge lying on a
    /// single outline diagonal — the cases the older passes handle.
    #[test]
    fn params_pinned_includes_a_vertex_on_a_curve_facet() {
        // Outline: box with a slanted (faceted) top-right edge (4,3)->(2,4).
        let outline =
            FloorPlan::from_clean_ring(&[p(0., 0.), p(4., 0.), p(4., 3.), p(2., 4.), p(0., 4.)])
                .unwrap();
        // Area whose top-right corner sits MID-facet at (3, 3.5); its other
        // corners are free interior points and its edges there are axis-aligned,
        // so only the per-vertex facet pass can pin it.
        let area =
            FloorPlan::from_clean_ring(&[p(1., 2.), p(3., 2.), p(3., 3.5), p(1., 3.5)]).unwrap();
        let pts = area.get_points();
        let idx = pts
            .iter()
            .position(|q| (q.x - 3.0).abs() < 1e-3 && (q.y - 3.5).abs() < 1e-3)
            .expect("area has the mid-facet vertex");
        let pinned = outline.params_pinned_to_outline(&area);
        let xp = area.vertex_x_param(0, idx);
        let yp = area.vertex_y_param(0, idx);
        assert!(
            xp.is_some_and(|p| pinned.contains(&p)),
            "mid-facet vertex's x param is pinned"
        );
        assert!(
            yp.is_some_and(|p| pinned.contains(&p)),
            "mid-facet vertex's y param is pinned"
        );
    }

    /// `from_points_to_world_orthogonal` aligns a rotated trace to the world
    /// axes, so every edge ends up horizontal or vertical — unlike the
    /// dominant-angle default, which (beyond its ~15° axis-snap window) keeps
    /// the whole shape tilted. This is what guarantees a new storey stays
    /// aligned with the (axis-aligned) storey below regardless of trace angle.
    #[test]
    fn world_orthogonal_removes_a_tilt() {
        // A ~10x10 square rotated ~20° about the origin — beyond the dominant-
        // angle path's 15° snap-to-axis window, so the two paths diverge.
        let theta = 0.35_f32; // ~20°
        let (c, s) = (theta.cos(), theta.sin());
        let rot = |x: f32, y: f32| p(x * c - y * s, x * s + y * c);
        let stroke = vec![
            rot(0.0, 0.0),
            rot(10.0, 0.0),
            rot(10.0, 10.0),
            rot(0.0, 10.0),
            rot(0.1, 0.1),
        ];

        // Dominant-angle orthogonalization keeps the tilt → edges aren't on the
        // world axes.
        let tilted = FloorPlan::from_points_to_orthogonal(&stroke, 0.1, true).unwrap();
        assert!(
            !tilted.non_orthogonal_edges().is_empty(),
            "dominant-angle orthogonalization preserves the tilt"
        );

        // World orthogonalization flattens it onto the world axes.
        let flat = FloorPlan::from_points_to_world_orthogonal(&stroke, 0.1, true).unwrap();
        assert!(
            flat.non_orthogonal_edges().is_empty(),
            "world orthogonalization aligns every edge to the world axes: {:?}",
            flat.non_orthogonal_edges()
        );
    }

    fn unit_square_plan() -> FloorPlan {
        // 5th point is the "lift near start" drag point dropped on close.
        let stroke = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        FloorPlan::from_points_to_orthogonal(&stroke, 0.5, true).unwrap()
    }

    /// A living area in a corner of the storey: the user draws only the two
    /// NEW interior walls (an open L from the left wall across to the bottom
    /// wall); the two shared house walls are filled in along the outline, so
    /// the result is a self-contained quad hugging the corner.
    #[test]
    fn sub_area_shares_corner_walls_with_the_outline() {
        let outline = unit_square_plan();
        let path = vec![p(0.0, 4.0), p(4.0, 4.0), p(4.0, 0.0)];
        let room =
            crate::carve::sub_area(&outline.get_points(), &path, 0.5, &SubareaTuning::default())
                .unwrap();
        assert_eq!(room.ring_count(), 1);
        let poly = room.polygons()[0].clone();
        assert_eq!(
            poly.len(),
            4,
            "corner room is a quad: 2 drawn + 2 shared walls"
        );
        let area = polygon_area(&poly).abs();
        assert!(
            (area - 16.0).abs() < 0.5,
            "4x4 corner room (~16), got {area}"
        );
        assert!(
            poly.iter().any(|v| v.x.abs() < 0.05 && v.y.abs() < 0.05),
            "it reuses the storey corner (0,0): {poly:?}"
        );
        // Two walls lie on the outline (x==0 and y==0).
        assert!(poly.iter().filter(|v| v.x.abs() < 0.05).count() >= 2);
        assert!(poly.iter().filter(|v| v.y.abs() < 0.05).count() >= 2);
        room.validate_simple().unwrap();
    }

    /// A living area drawn away from every wall doesn't share anything — it
    /// closes into a free-standing interior room.
    #[test]
    fn sub_area_interior_path_becomes_a_freestanding_room() {
        let outline = unit_square_plan();
        let path = vec![
            p(3.0, 3.0),
            p(6.0, 3.0),
            p(6.0, 6.0),
            p(3.0, 6.0),
            p(3.1, 3.1),
        ];
        let room =
            crate::carve::sub_area(&outline.get_points(), &path, 0.3, &SubareaTuning::default())
                .unwrap();
        assert_eq!(room.ring_count(), 1);
        assert!(room.polygons()[0].len() >= 4);
        let area = polygon_area(&room.polygons()[0]).abs();
        assert!(
            (area - 9.0).abs() < 1.0,
            "3x3 interior room (~9), got {area}"
        );
        room.validate_simple().unwrap();
    }

    /// Confirms the reported cause: if the top (closing) wall's perpendicular
    /// coordinate ends up off the anchor, the parametric build leaves it as a
    /// FREE parameter rather than tying it to the anchor — so the shape looks
    /// ~orthogonal but that free param can DRIFT when dimensioning nudges un-set
    /// params ("orthogonal until dimensioned, then less orthogonal"). The full
    /// `from_points_to_orthogonal` pipeline snaps the closing edge so the coord
    /// is tied (axis-aligned, no orphan free parameter).
    #[test]
    fn closing_edge_coord_is_tied_not_left_as_a_free_parameter() {
        // Staircase whose top (closing) wall is ~7 units off horizontal
        // (v7.y=284 vs v0.y=291).
        let stair8 = vec![
            p(73.0, 291.0),
            p(73.0, 619.0),
            p(202.0, 619.0),
            p(202.0, 711.0),
            p(397.0, 711.0),
            p(397.0, 615.0),
            p(621.0, 615.0),
            p(621.0, 284.0),
        ];

        // No-snap parametrize (what the orthogonalizer effectively produced for
        // the noisy stroke): the closing edge stays diagonal — its perpendicular
        // coord is a free parameter 7 units off the anchor.
        let untied = FloorPlan::from_clean_ring(&stair8).unwrap();
        let up = untied.polygons()[0].clone();
        let un = up.len();
        let untied_off = (up[un - 1].y - up[0].y)
            .abs()
            .min((up[un - 1].x - up[0].x).abs());
        assert!(
            untied_off > 1.0,
            "without the closing-edge snap the top wall is left diagonal \
             (free param, not tied): off={untied_off}"
        );

        // Full pipeline (with the closing-edge guarantee): the top wall is
        // snapped axis-aligned, so its coord ties to the anchor — no orphan
        // free param to drift.
        let mut stroke = stair8.clone();
        stroke.push(p(74.0, 290.0)); // trailing "lift near start" drag point
        let fixed = FloorPlan::from_points_to_orthogonal(&stroke, 8.0, true).unwrap();
        let fp = fixed.polygons()[0].clone();
        let m = fp.len();
        let fixed_off = (fp[m - 1].y - fp[0].y)
            .abs()
            .min((fp[m - 1].x - fp[0].x).abs());
        assert!(
            fixed_off < 1e-3,
            "the closing edge is snapped axis-aligned: off={fixed_off}"
        );
        fixed.validate_simple().unwrap();
    }

    /// Regression: a small room with a clear nook drawn inside a BIG house must
    /// keep the nook. The cleanup that strips collapse-remnants is scaled to the
    /// SUB-AREA's own size, not the house's — so the nook isn't wrongly erased
    /// just because the storey is large. (With the house-scaled tolerance this
    /// nook was flattened to a plain quad.)
    #[test]
    fn sub_area_keeps_a_real_nook_in_a_big_house() {
        let big_house = FloorPlan::from_points_to_orthogonal(
            &[
                p(0.0, 0.0),
                p(60.0, 0.0),
                p(60.0, 60.0),
                p(0.0, 60.0),
                p(0.5, 0.5),
            ],
            1.0,
            true,
        )
        .unwrap();
        // ~2m square room with a 0.3m-deep nook (15% of the side), free-standing.
        let path = vec![
            p(10.0, 10.0),
            p(12.0, 10.0),
            p(12.0, 10.7),
            p(12.3, 10.7),
            p(12.3, 11.3),
            p(12.0, 11.3),
            p(12.0, 12.0),
            p(10.0, 12.0),
            p(10.1, 10.1),
        ];
        let (mut mnx, mut mny, mut mxx, mut mxy) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for q in &path {
            mnx = mnx.min(q.x);
            mny = mny.min(q.y);
            mxx = mxx.max(q.x);
            mxy = mxy.max(q.y);
        }
        let tol = ((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt() * 0.02;
        let room = crate::carve::sub_area(
            &big_house.get_points(),
            &path,
            tol,
            &SubareaTuning::default(),
        )
        .unwrap();
        assert!(
            room.polygons()[0].len() > 4,
            "the nook survives even though the house is large: {:?}",
            room.polygons()[0]
        );
        room.validate_simple().unwrap();
    }

    /// Drawing a living area as a FULL closed loop: walls drawn close to a
    /// parallel house wall get corrected onto it, while interior walls are left
    /// alone. Here the left + bottom walls are drawn ~0.15 off the storey's
    /// left/bottom walls and must snap to x=0 / y=0.
    #[test]
    fn full_loop_living_area_walls_snap_onto_nearby_house_walls() {
        let outline = unit_square_plan(); // 10x10, corner at (0,0)
        let path = vec![
            p(0.15, 0.12), // near corner (0,0)
            p(4.0, 0.12),  // bottom wall, ~0.12 above the storey's y=0 wall
            p(4.0, 4.0),   // right wall — interior, far from any house wall
            p(0.15, 4.0),  // left wall, ~0.15 right of the storey's x=0 wall
            p(0.16, 0.13), // close
        ];
        let room =
            crate::carve::sub_area(&outline.get_points(), &path, 0.1, &SubareaTuning::default())
                .unwrap();
        let poly = room.polygons()[0].clone();
        assert!(
            poly.iter().any(|v| v.x.abs() < 0.01),
            "left wall snapped onto the storey wall x=0: {poly:?}"
        );
        assert!(
            poly.iter().any(|v| v.y.abs() < 0.01),
            "bottom wall snapped onto the storey wall y=0: {poly:?}"
        );
        // Interior walls untouched (still near x=4 / y=4, not pulled to x=10/y=10).
        assert!(
            poly.iter().any(|v| (v.x - 4.0).abs() < 0.2),
            "interior right wall left alone: {poly:?}"
        );
        assert!(room.non_orthogonal_edges().is_empty(), "stays orthogonal");
        room.validate_simple().unwrap();
    }

    /// The reported use case: an L (3/4 of a square, reflex corner) with a
    /// curve rounding the bottom-right notch — drawn from near the bottom corner
    /// (v5) to near the right corner (v3). Each endpoint lands on an existing
    /// corner, so it REUSES that corner vertex (its already-known params) rather
    /// than splicing a new point. Without that, an endpoint at a corner whose
    /// wall the curve deletes would float free and the dimensioning would stall.
    ///
    /// The L is sized so the deleted notch-top wall (length 5) is longer than
    /// the surviving right wall (length 1): an endpoint near v3 sits close
    /// enough to drop v3 yet far enough off the right wall to float — exactly
    /// the case corner-reuse turns into a clean attach.
    #[test]
    fn corner_curve_reuses_corner_vertices_and_bakes() {
        use std::collections::HashSet;
        let l = vec![
            p(0.0, 0.0),   // v0
            p(0.0, 5.0),   // v1
            p(6.0, 5.0),   // v2
            p(6.0, 4.0),   // v3  (right wall v2→v3 is SHORT, length 1)
            p(1.0, 4.0),   // v4  (notch-top v3→v4 is WIDE, length 5)
            p(1.0, 0.0),   // v5
            p(0.02, 0.02), // lift-near-start drag point, dropped on close
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&l, 0.05, true).unwrap();
        let old_vids: HashSet<VertexId> = (0..plan.polygons()[0].len())
            .filter_map(|i| plan.vertex_id_at(0, i))
            .collect();
        let known: HashSet<ParameterId> = plan.all_active_params();

        // Arc rounding the notch: start near corner v5 (bottom), end ~3mm from
        // corner v3 (right). Both endpoints land on existing corners.
        let arc = vec![
            p(0.95, 0.0),
            p(2.5, 1.2),
            p(4.2, 2.4),
            p(5.5, 3.4),
            p(5.997, 4.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();

        // Option #2: each endpoint REUSES the existing corner vertex — no new
        // floating param, the curve attaches AT the corner.
        assert!(
            old_vids.contains(&v1),
            "start endpoint should reuse an existing corner vertex, got {v1}"
        );
        assert!(
            old_vids.contains(&v2),
            "end endpoint should reuse an existing corner vertex, got {v2}"
        );

        // Both attach points are existing corners (known params), so the curve
        // is fully constrained immediately — nothing to dimension.
        let (_, i1) = plan.find_vertex(v1).unwrap();
        let (_, i2) = plan.find_vertex(v2).unwrap();
        let n = plan.polygons()[0].len();
        let edge_start = if (i1 + 1) % n == i2 { i1 } else { i2 };
        let deps = plan.wall_param_deps(0, edge_start);
        assert!(
            deps.iter().all(|q| known.contains(q)),
            "a corner-to-corner placeholder is already known; deps={deps:?} known={known:?}"
        );

        // It bakes into a real faceted curve, leaving a valid polygon.
        plan.finish_arc(v1, v2, bulge).unwrap();
        assert!(plan.validate_simple().is_ok());
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "the rounded corner should bake into facets, got {}",
            diagonal_edge_count(&plan)
        );
    }

    fn diagonal_edge_count(plan: &FloorPlan) -> usize {
        let ring = &plan.polygons()[0];
        let n = ring.len();
        let eps = 1e-3_f32;
        (0..n)
            .filter(|&i| {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                (a.x - b.x).abs() > eps && (a.y - b.y).abs() > eps
            })
            .count()
    }

    #[test]
    fn ortho_porch_at_a_corner_reuses_it_and_stays_orthogonal() {
        // A porch (orthogonal bump) added with ONE endpoint on an existing
        // square corner must reuse that corner — otherwise the corner endpoint
        // floats free, over-parametrises, and dimensioning bends the (fixed)
        // wall it joins. The porch is drawn FREEHAND-WONKY (as a stylus would):
        // orthogonalize cleans it to the drawn endpoint, then reusing the corner
        // must also pull the adjacent porch-wall point onto the corner so that
        // wall stays vertical. With both, the porch needs only 2 new params (its
        // other attach position + its depth) and everything stays orthogonal.
        let mut plan = unit_square_plan(); // corners incl. (10,10)
        let before = polygon_area(&plan.polygons()[0]).abs();
        // One end NEAR corner (10,10) but drawn a touch off; the rest wobbly.
        let porch = vec![p(10.3, 10.2), p(10.1, 14.1), p(5.8, 13.9), p(6.1, 10.3)];
        let minted = plan.extend_ortho(&porch).unwrap();

        assert_eq!(
            minted.len(),
            2,
            "a corner-anchored ortho porch needs exactly 2 params (depth + far attach), got {}",
            minted.len()
        );
        assert!(plan.validate_simple().is_ok());
        assert_eq!(
            diagonal_edge_count(&plan),
            0,
            "the porch and square must stay orthogonal (no tilted wall at the reused corner)"
        );
        assert!(
            polygon_area(&plan.polygons()[0]).abs() > before,
            "the porch adds area"
        );
        // The square corner (10,10) is preserved (reused), not replaced.
        assert!(plan.polygons()[0]
            .iter()
            .any(|q| (q.x - 10.0).abs() < 1e-3 && (q.y - 10.0).abs() < 1e-3));
    }

    #[test]
    fn remove_collinear_drops_flush_corner_but_keeps_preserved() {
        use std::collections::HashSet;
        // Freehand porch flush at corner (10,10): leaves that corner collinear
        // on the right wall (10,0)-(10,~14). It's spurious for the envelope.
        let mut plan = unit_square_plan();
        let porch = vec![p(10.3, 10.2), p(10.1, 14.1), p(5.8, 13.9), p(6.1, 10.3)];
        plan.extend_ortho(&porch).unwrap();
        let n_before = plan.polygons()[0].len();
        let area_before = polygon_area(&plan.polygons()[0]).abs();

        // Find the collinear corner's id.
        let corner_vid = {
            let pts = plan.polygons()[0].clone();
            let i = pts
                .iter()
                .position(|q| (q.x - 10.0).abs() < 1e-3 && (q.y - 10.0).abs() < 1e-3)
                .expect("reused corner present");
            plan.vertex_id_at(0, i).unwrap()
        };

        // Preserved → kept even though collinear (a future flooring boundary).
        let mut preserve = HashSet::new();
        preserve.insert(corner_vid);
        assert!(
            plan.remove_collinear_vertices(&preserve).is_empty(),
            "a preserved collinear vertex must be kept"
        );
        assert_eq!(plan.polygons()[0].len(), n_before);

        // Not preserved → the flush corner is dropped, envelope unchanged.
        let removed = plan.remove_collinear_vertices(&HashSet::new());
        assert_eq!(
            removed,
            vec![corner_vid],
            "exactly the flush corner is removed"
        );
        assert_eq!(plan.polygons()[0].len(), n_before - 1);
        assert!(plan.validate_simple().is_ok());
        assert_eq!(diagonal_edge_count(&plan), 0, "stays orthogonal");
        assert!(
            (polygon_area(&plan.polygons()[0]).abs() - area_before).abs() < 1e-2,
            "removing a collinear vertex must not change the shape"
        );
        // Idempotent — nothing collinear remains.
        assert!(plan.remove_collinear_vertices(&HashSet::new()).is_empty());
    }

    #[test]
    fn skipping_a_wall_keeps_prompting_others_and_the_same_param_elsewhere() {
        use std::collections::HashSet;
        // SKIP must be per-WALL, not per-parameter. Skipping a wall must not halt
        // the prompt loop, and a parameter the skipped wall would have set is
        // still offered via another wall that also controls it (e.g. the
        // opposite side of a rectangle). The user hit this twice: "I skip the
        // second of three and it won't ask me for the third / the derivable one."
        let plan = unit_square_plan(); // 2 params, each shared by an opposite pair
        let none: HashSet<VertexId> = HashSet::new();
        let nothing: HashSet<ParameterId> = HashSet::new();

        let (first_vid, first_param) = plan
            .next_promptable_wall(&nothing, &none)
            .expect("a wall should be promptable initially");

        // Skipping that wall does NOT stop the loop — another wall is offered.
        let mut skipped = HashSet::new();
        skipped.insert(first_vid);
        let (second_vid, _) = plan
            .next_promptable_wall(&nothing, &skipped)
            .expect("skipping one wall must not halt prompting");
        assert_ne!(second_vid, first_vid);

        // The skipped wall's parameter is still reachable through another wall
        // that controls it: with every OTHER parameter known, the opposite wall
        // is offered to set it (so a skipped value the user can measure
        // elsewhere is still asked for).
        let mut known: HashSet<ParameterId> = plan.all_active_params();
        known.remove(&first_param);
        let (vid, param) = plan
            .next_promptable_wall(&known, &skipped)
            .expect("the opposite wall should still set the skipped param");
        assert_eq!(
            param, first_param,
            "same param, reached via a different wall"
        );
        assert_ne!(vid, first_vid);
    }

    #[test]
    fn trim_cuts_corner_into_a_diagonal_wall() {
        let mut plan = unit_square_plan();
        assert_eq!(diagonal_edge_count(&plan), 0, "square starts orthogonal");
        // Cut across the top-right corner: right edge at y=4 to top edge at x=6.
        plan.trim_by_line(p(10.0, 4.0), p(6.0, 0.0)).unwrap();

        assert_eq!(
            diagonal_edge_count(&plan),
            1,
            "the cut edge should be the one non-orthogonal wall"
        );
        assert_eq!(plan.polygons()[0].len(), 5, "4 corners − 1 + 2 crossings");
        // Larger side kept: ~100 minus the small corner triangle (0.5*4*4=8).
        assert!(plan.validate_simple().is_ok());
    }

    #[test]
    fn trim_line_must_cross_the_outline() {
        let mut plan = unit_square_plan();
        // A line entirely outside the polygon never crosses.
        assert!(plan.trim_by_line(p(20.0, 0.0), p(20.0, 10.0)).is_err());
    }

    fn u_shape_plan() -> FloorPlan {
        // A U: a horizontal line at y=4 crosses its boundary FOUR times
        // (x = 0, 2, 4, 6) — the concave case that broke the old endpoint line.
        let stroke = vec![
            p(0.0, 0.0),
            p(6.0, 0.0),
            p(6.0, 6.0),
            p(4.0, 6.0),
            p(4.0, 2.0),
            p(2.0, 2.0),
            p(2.0, 6.0),
            p(0.0, 6.0),
            p(0.1, 0.1),
        ];
        FloorPlan::from_points_to_orthogonal(&stroke, 0.3, true).unwrap()
    }

    #[test]
    fn trim_picks_intended_two_of_four_crossings() {
        let mut plan = u_shape_plan();
        // Cut across the LEFT prong only (stroke spans x≈-1..3 at y≈4). The
        // fitted line crosses 4 edges, but we keep the two nearest the drawn
        // span (x=0 and x=2), not the far ones at x=4,6.
        let res = plan.trim_by_stroke(&[p(-1.0, 4.0), p(3.0, 4.0)]);
        assert!(
            res.is_ok(),
            "expected the left-prong cut to succeed: {:?}",
            res
        );
        assert!(plan.validate_simple().is_ok());
    }

    #[test]
    fn wavy_stroke_fits_to_the_same_cut_as_a_straight_one() {
        let mut wavy = unit_square_plan();
        let mut straight = unit_square_plan();
        // Wavy stroke wobbling around the straight line (10,4)→(6,0).
        let wavy_stroke = vec![
            p(10.0, 4.0),
            p(9.3, 2.8),
            p(8.2, 2.4),
            p(7.1, 1.2),
            p(6.0, 0.0),
        ];
        wavy.trim_by_stroke(&wavy_stroke).unwrap();
        straight.trim_by_line(p(10.0, 4.0), p(6.0, 0.0)).unwrap();
        let wp = &wavy.polygons()[0];
        let sp = &straight.polygons()[0];
        assert_eq!(wp.len(), sp.len());
        for (a, b) in wp.iter().zip(sp.iter()) {
            assert!((a.x - b.x).abs() < 0.3 && (a.y - b.y).abs() < 0.3);
        }
    }

    #[test]
    fn remove_vertex_collapses_to_a_valid_polygon() {
        // Collapsing a wall to zero removes a vertex; the result must stay a
        // valid (here non-orthogonal) simple polygon and keep ≥3 vertices.
        let mut plan = unit_square_plan();
        assert!(plan.remove_vertex(0, 1));
        assert_eq!(plan.polygons()[0].len(), 3);
        assert!(plan.validate_simple().is_ok());
        // Can't drop below a triangle.
        assert!(!plan.remove_vertex(0, 0));
    }

    #[test]
    fn trim_through_a_corner_is_accepted() {
        // Cut from corner (0,0) to the right wall: the would-be remnant wall at
        // the corner has length 0, which must be accepted (collapsed), not
        // rejected. Reuses the corner vertex rather than a zero-length edge.
        let mut plan = unit_square_plan();
        let res = plan.trim_by_stroke(&[p(0.0, 0.0), p(10.0, 4.0)]);
        assert!(res.is_ok(), "corner cut should be accepted: {:?}", res);
        assert!(plan.validate_simple().is_ok());
        assert_eq!(
            diagonal_edge_count(&plan),
            1,
            "the cut is the diagonal wall"
        );
    }

    #[test]
    fn trim_by_polygon_carves_an_orthogonal_notch() {
        let mut plan = unit_square_plan(); // 10x10, area 100
        let before = polygon_area(&plan.polygons()[0]).abs();
        assert!((before - 100.0).abs() < 1.0);

        // Rectangular notch into the top edge (y=10), down to y=7, x in [3,7].
        let notch = vec![p(3.0, 10.0), p(3.0, 7.0), p(7.0, 7.0), p(7.0, 10.0)];
        plan.trim_ortho(&notch).unwrap();

        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after < before,
            "notch should remove area ({after} vs {before})"
        );
        assert!(
            (before - after - 12.0).abs() < 1.0,
            "should remove ~12 (4x3); removed {}",
            before - after
        );
        assert!(plan.validate_simple().is_ok());
        // The notch's inner corners are present, and it stays orthogonal.
        let ring = &plan.polygons()[0];
        assert!(ring
            .iter()
            .any(|q| (q.x - 3.0).abs() < 1e-2 && (q.y - 7.0).abs() < 1e-2));
        assert!(ring
            .iter()
            .any(|q| (q.x - 7.0).abs() < 1e-2 && (q.y - 7.0).abs() < 1e-2));
        assert_eq!(
            diagonal_edge_count(&plan),
            0,
            "an ortho notch has no diagonal"
        );
    }

    #[test]
    fn curve_bulging_out_adds_a_faceted_arc() {
        let mut plan = unit_square_plan(); // 10x10, area 100
        let before = polygon_area(&plan.polygons()[0]).abs();
        // An arc bulging OUT of the top edge (y=10), apex ~(5,12).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 11.0),
            p(4.3, 11.7),
            p(5.0, 12.0),
            p(5.7, 11.7),
            p(6.4, 11.0),
            p(7.0, 10.0),
        ];
        plan.extend_curve(&arc).unwrap();

        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after > before,
            "outward curve adds area ({after} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        // Captured as a run of facets (>=2 non-axis-aligned edges).
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "curve should be multiple facets, got {}",
            diagonal_edge_count(&plan)
        );
    }

    #[test]
    fn provisional_arc_is_inert_until_finished() {
        let mut plan = unit_square_plan(); // 10x10, area 100
        let before = polygon_area(&plan.polygons()[0]).abs();
        let n_before = plan.polygons()[0].len();
        // Outward arc on the top edge, apex ~(5,12).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 11.0),
            p(5.0, 12.0),
            p(6.4, 11.0),
            p(7.0, 10.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        // begin_arc only splits the top edge — shape (area) is unchanged, but
        // the two endpoints now exist as vertices (2 more than before).
        let after_begin = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            (after_begin - before).abs() < 1e-2,
            "begin_arc must not change the area ({after_begin} vs {before})"
        );
        assert_eq!(plan.polygons()[0].len(), n_before + 2);
        assert_eq!(
            diagonal_edge_count(&plan),
            0,
            "still orthogonal while provisional"
        );
        assert!(bulge > 0.0, "outward bulge is positive (left of start→end)");
        // The preview is a multi-point arc bulging out past the top edge.
        let preview = plan.arc_preview(v1, v2, bulge).unwrap();
        assert!(preview.len() > 2 && preview.iter().any(|q| q.y > 10.5));

        // Finishing bakes the arc in: area grows and facets appear.
        plan.finish_arc(v1, v2, bulge).unwrap();
        let after_finish = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after_finish > before + 1.0,
            "finish_arc adds the bulge area ({after_finish} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "baked arc is a run of facets, got {}",
            diagonal_edge_count(&plan)
        );
    }

    #[test]
    fn small_scale_arc_facets_arent_rejected_as_collapsed() {
        // Regression for the reported extend-curve bug: on a small (metre-scale)
        // plan the faceted arc's edges are only a couple of cm — below the OLD
        // 3cm "collapsed edge" floor, which wrongly rejected the splice and left
        // the curve as a straight placeholder line.
        let mut plan = unit_square_plan(); // 10x10
        plan.scale_all(0.1); // 1m x 1m — walls ~1m, like the user's plan
        let before = polygon_area(&plan.polygons()[0]).abs();
        // Shallow outward bump on the top edge (y≈1), apex ~(0.5, 1.06).
        let arc = vec![
            p(0.3, 1.0),
            p(0.4, 1.04),
            p(0.5, 1.06),
            p(0.6, 1.04),
            p(0.7, 1.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        let baked = plan.finish_arc(v1, v2, bulge);
        assert!(
            baked.is_ok(),
            "a small-scale arc's cm facets must splice, not be rejected: {:?}",
            baked
        );
        assert!(
            diagonal_edge_count(&plan) >= 2,
            "curve should bake into facets, got {}",
            diagonal_edge_count(&plan)
        );
        assert!(
            polygon_area(&plan.polygons()[0]).abs() > before,
            "outward arc adds area"
        );
        assert!(plan.validate_simple().is_ok());
    }

    #[test]
    fn provisional_arc_inward_carves_a_recess_on_finish() {
        // TrimCurve uses the same begin/finish path; an inward (negative) bulge
        // carves area rather than adding it — no separate keep-larger step.
        let mut plan = unit_square_plan();
        let before = polygon_area(&plan.polygons()[0]).abs();
        // Arc dipping INTO the square from the top edge, apex ~(5,8).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 9.0),
            p(5.0, 8.0),
            p(6.4, 9.0),
            p(7.0, 10.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        assert!(bulge < 0.0, "inward bulge is negative (right of start→end)");
        assert!((polygon_area(&plan.polygons()[0]).abs() - before).abs() < 1e-2);

        plan.finish_arc(v1, v2, bulge).unwrap();
        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after < before - 1.0,
            "inward arc removes area ({after} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        assert!(diagonal_edge_count(&plan) >= 2);
    }

    #[test]
    fn curve_bulging_in_carves_a_faceted_recess() {
        let mut plan = unit_square_plan();
        let before = polygon_area(&plan.polygons()[0]).abs();
        // An arc dipping INTO the square from the top edge, apex ~(5,8).
        let arc = vec![
            p(3.0, 10.0),
            p(3.6, 9.0),
            p(4.3, 8.3),
            p(5.0, 8.0),
            p(5.7, 8.3),
            p(6.4, 9.0),
            p(7.0, 10.0),
        ];
        plan.trim_curve(&arc).unwrap();

        let after = polygon_area(&plan.polygons()[0]).abs();
        assert!(
            after < before,
            "inward curve removes area ({after} vs {before})"
        );
        assert!(plan.validate_simple().is_ok());
        assert!(diagonal_edge_count(&plan) >= 2);
    }
}

#[cfg(test)]
mod trim_self_cross {
    use super::*;
    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }
    /// The wiggly trim stroke from the field log used to fail with "edges 0 and 7
    /// intersect": the orthogonalizer's implicit-closure heuristic saw the two
    /// near endpoints and bolted on a spurious closing corner. With trim paths now
    /// orthogonalized as `Closure::Open`, no closing corner is added and the trim
    /// produces a valid simple polygon.
    #[test]
    fn wiggly_open_trim_no_longer_self_crosses() {
        // The dimensioned L-shape (≈3 m) from the log; inner corner at (3.156, 3.301).
        let outline = vec![
            p(1.157, 1.802),
            p(1.157, 4.802),
            p(4.156, 4.802),
            p(4.156, 3.301),
            p(3.156, 3.301),
            p(3.156, 1.802),
        ];
        let mut fp = FloorPlan::from_clean_ring(&outline).unwrap();
        let stroke = vec![
            p(3.134, 3.303),
            p(3.124, 3.310),
            p(3.113, 3.317),
            p(3.096, 3.331),
            p(3.078, 3.349),
            p(3.071, 3.367),
            p(3.050, 3.381),
            p(3.039, 3.391),
            p(3.025, 3.398),
            p(3.005, 3.412),
            p(2.998, 3.422),
            p(2.991, 3.432),
            p(2.984, 3.439),
            p(2.977, 3.446),
            p(2.970, 3.453),
            p(2.953, 3.467),
            p(2.935, 3.473),
            p(2.928, 3.480),
            p(2.918, 3.484),
            p(2.907, 3.491),
            p(2.894, 3.501),
            p(2.887, 3.508),
            p(2.874, 3.514),
            p(2.857, 3.521),
            p(2.846, 3.525),
            p(2.832, 3.528),
            p(2.818, 3.528),
            p(2.811, 3.535),
            p(2.811, 3.526),
            p(2.818, 3.505),
            p(2.818, 3.488),
            p(2.818, 3.460),
            p(2.825, 3.446),
            p(2.832, 3.429),
            p(2.832, 3.387),
            p(2.825, 3.373),
            p(2.818, 3.356),
            p(2.815, 3.326),
            p(2.811, 3.295),
            p(2.804, 3.252),
            p(2.796, 3.195),
            p(2.796, 3.149),
            p(2.789, 3.097),
            p(2.789, 3.062),
            p(2.789, 3.034),
            p(2.782, 3.017),
            p(2.782, 2.999),
            p(2.782, 2.979),
            p(2.782, 2.958),
            p(2.775, 2.937),
            p(2.775, 2.920),
            p(2.775, 2.909),
            p(2.762, 2.895),
            p(2.762, 2.885),
            p(2.755, 2.871),
            p(2.755, 2.864),
            p(2.764, 2.864),
            p(2.794, 2.872),
            p(2.851, 2.886),
            p(2.933, 2.914),
            p(3.027, 2.931),
            p(3.077, 2.937),
            p(3.098, 2.944),
            p(3.140, 2.944),
            p(3.183, 2.947),
            p(3.229, 2.951),
            p(3.253, 2.951),
        ];
        fp.trim_ortho(&stroke)
            .expect("open trim no longer self-crosses");
        fp.validate_simple()
            .expect("trim result is a simple polygon");
    }
}
