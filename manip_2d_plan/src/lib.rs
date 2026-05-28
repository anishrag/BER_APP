//! Processing hand-drawn floor plan strokes into a precise parametric model.
//!
//! Pipeline (`FloorPlan::from_points_to_orthogonal`):
//!   1. RDP simplification (geo's `Simplify`)
//!   2. Orthogonalize each segment to horizontal or vertical
//!   3. For closed strokes: snap the implicit closing edge, rotate the start
//!      vertex to a real corner, remove collinear vertices
//!   4. Build a `ParametricPlan` and apply orthogonal constraints
//!
//! After the plan is built, callers can ask for collinearity suggestions
//! (`suggest_collinear`) and apply user decisions (`apply_collinear_suggestion`).

mod error;
mod extend;
mod geom;
mod orthogonal;
mod parametric;
mod simplification;

pub use error::Error;
pub use geom::{find_flipped_edge, intersect_circles, project_to_circle, solve_axis_wall};
pub use parametric::{Axis, CollinearitySuggestion, ParameterAxis, ParameterId, VertexId};

use domain::points::Point2f32;
use std::collections::HashSet;

/// A processed floor plan: orthogonal polygon as a parametric representation
/// (shared coordinate expressions for collinear walls, one parameter per
/// independent dimension).
#[derive(Debug, Clone, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct FloorPlan {
    parametric: parametric::ParametricPlan,
}

impl FloorPlan {
    /// Create a floor plan from a hand-drawn stroke with orthogonal constraint.
    /// Automatically creates a parametric representation with H/V constraints applied.
    ///
    /// `closed` indicates whether the stroke is an implicitly-closed polygon
    /// (the assessor lifted near the start and `check_closed` fired). When
    /// closed, RDP runs first to collapse collinear closing-edge drag points,
    /// then we drop the final trailing vertex so the closing edge becomes
    /// implicit and orthogonalization doesn't introduce a spurious corner.
    pub fn from_points_to_orthogonal(
        points: &[Point2f32],
        tolerance: f32,
        closed: bool,
    ) -> Result<Self, Error> {
        let mut simplified = simplification::simplify_stroke(points, tolerance)?;

        if closed && simplified.len() > 3 {
            // Drop the trailing drag point. RDP just collapsed everything along
            // the closing edge into one segment; the remaining endpoint is
            // along that edge, not a corner. Removing it lets the implicit
            // edge from the new last vertex to first be axis-aligned by
            // virtue of the user's drawing, with no spurious L-corner added
            // during orthogonalization.
            simplified.pop();
        }

        let orthog_points = orthogonal::make_orthogonal(&simplified, tolerance)?;

        // For a closed polygon, finalize the closing structure before the
        // parametric layer sees it:
        //   1. snap the implicit closing edge to be exactly axis-aligned
        //   2. rotate so the first vertex sits at a real corner (handles the
        //      "user started mid-wall" case — otherwise the start is collinear
        //      and shows up as a spurious extra vertex)
        //   3. run remove_collinear one more time to drop the previously-
        //      anchored start, now demoted to a mid-list vertex
        //
        // Note: we deliberately do NOT auto-merge near-collinear walls here.
        // Walls that are close but not adjacent in the polygon are left
        // as separate, and the user gets asked about them via the
        // `suggest_collinear` review. This keeps the parametric model honest
        // about what the user actually drew. `snap_near_collinear_walls`
        // remains available as a function for callers that want auto-merge
        // back (e.g. a future "auto-clean" mode).
        let final_points = if closed {
            let snapped = orthogonal::snap_closing_edge(orthog_points, tolerance);
            let rotated = orthogonal::rotate_to_corner_start(snapped, tolerance);
            orthogonal::remove_collinear(&rotated, tolerance)
        } else {
            orthog_points
        };

        // Create parametric representation
        let mut parametric = parametric::ParametricPlan::from_points(&final_points);

        // Apply orthogonal constraints to reduce parameters
        parametric.apply_orthogonal_constraints()?;

        Ok(Self { parametric })
    }

    /// Add another disjoint polygon to this floor plan (e.g. an extension
    /// section on an upper floor). The new polygon shares the same parameter
    /// namespace as existing ones, so cross-polygon collinearity suggestions
    /// and merges work uniformly across the whole plan.
    pub fn add_polygon(
        &mut self,
        points: &[Point2f32],
        tolerance: f32,
        closed: bool,
    ) -> Result<(), Error> {
        let mut simplified = simplification::simplify_stroke(points, tolerance)?;
        if closed && simplified.len() > 3 {
            simplified.pop();
        }
        let orthog_points = orthogonal::make_orthogonal(&simplified, tolerance)?;
        let final_points = if closed {
            let snapped = orthogonal::snap_closing_edge(orthog_points, tolerance);
            let rotated = orthogonal::rotate_to_corner_start(snapped, tolerance);
            orthogonal::remove_collinear(&rotated, tolerance)
        } else {
            orthog_points
        };

        self.parametric.add_ring(&final_points);
        self.parametric.apply_orthogonal_constraints()?;
        Ok(())
    }

    /// Extend the primary ring by splicing a drawn path onto a wall, *adding*
    /// the enclosed area (the SHORTER arc between the attach points is replaced
    /// by the path). `extend_ortho` orthogonalizes the path (a stepped porch);
    /// see [`extend_curve`](Self::extend_curve) for a faceted arc. Untouched
    /// vertices keep their identity/parameters; returns the newly-minted
    /// parameter IDs. See [`splice_path`](Self::splice_path) for the mechanics.
    pub fn extend_ortho(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
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
        use crate::extend::SpliceElem;
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

        let diag = self
            .bounding_box()
            .map(|((mnx, mny), (mxx, mxy))| ((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt())
            .unwrap_or(0.0);
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

    /// Trim with an orthogonal cut PATH (removing area): the stroke is
    /// orthogonalized, its endpoints attach to the outline, and the
    /// smaller-area side is replaced by the stepped cut — an orthogonal
    /// notch/chamfer (vs the straight diagonal [`trim_by_stroke`](Self::trim_by_stroke)
    /// cuts). See [`splice_path`](Self::splice_path).
    pub fn trim_ortho(&mut self, stroke: &[Point2f32]) -> Result<Vec<ParameterId>, Error> {
        self.splice_path(stroke, true, true)
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
    fn splice_path(
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
        let diag = self
            .bounding_box()
            .map(|((mnx, mny), (mxx, mxy))| ((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt())
            .unwrap_or(0.0);
        let merge_tol = if diag > 0.0 {
            (diag * 0.01).max(1e-4)
        } else {
            10.0
        };
        // Orthogonalize (Ortho tools) snaps to H/V; simplify (Curve tools) just
        // RDP-facets the hand-drawn arc, keeping its curvature.
        let path = if orthogonalize {
            extend::orthogonalize_open_path(stroke, merge_tol)?
        } else {
            simplification::simplify_stroke(stroke, merge_tol)?
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
    fn splice_cleaned(
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
        let diag = self
            .bounding_box()
            .map(|((mnx, mny), (mxx, mxy))| ((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt())
            .unwrap_or(0.0);
        let vertex_tol = (diag * 0.04).max(1e-4);
        let max_tol = (diag * 0.25).max(1e-3);

        // Snap the path's endpoints to find the attach edges, then pin them onto
        // those edges so they share the edges' perpendicular coordinate exprs.
        // `snapped*` is true when the endpoint landed on an existing CORNER.
        let start = path[0];
        let end = path[path.len() - 1];
        let (e1, t1, s_dist, snapped1) = extend::snap_to_polygon(&ring, start, vertex_tol);
        let (e2, t2, e_dist, snapped2) = extend::snap_to_polygon(&ring, end, vertex_tol);
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
        extend::pin_to_edge(&mut path[0], &ring, e1);
        extend::pin_to_edge(&mut path[last], &ring, e2);

        let mut chosen = if keep_larger {
            // Trim: build both arc-replacements; keep whichever leaves more area.
            let area_of = |seq: &[crate::extend::SpliceElem]| -> f32 {
                let pts: Vec<Point2f32> = seq
                    .iter()
                    .map(|e| match e {
                        crate::extend::SpliceElem::Keep(idx) => ring[*idx],
                        crate::extend::SpliceElem::New(pp) => *pp,
                    })
                    .collect();
                polygon_area(&pts).abs()
            };
            let seq_a = extend::splice_sequence(&ring, e1, t1, e2, t2, &path, true);
            let seq_b = extend::splice_sequence(&ring, e1, t1, e2, t2, &path, false);
            if area_of(&seq_a) >= area_of(&seq_b) {
                seq_a
            } else {
                seq_b
            }
        } else {
            // Extend: replace the SHORTER arc with the path (adds the feature).
            extend::plan_splice_sequence(&ring, e1, t1, e2, t2, &path)
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
        use crate::extend::SpliceElem;
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
        self.parametric.splice_into_edge(0, &chosen)
    }

    /// Begin a PROVISIONAL curved wall. Fits the freehand `stroke` to a circular
    /// arc (chord + signed bulge), then splices ONLY the two endpoints onto the
    /// outline as pinned vertices — a straight placeholder edge between them,
    /// leaving the shape otherwise UNCHANGED. Returns `(start_vertex,
    /// end_vertex, bulge_ratio)` where `bulge_ratio` is the signed sagitta as a
    /// fraction of the chord (left of start→end), so it survives both the
    /// pixel→metre rescale and any move of the endpoints (the bulge scales with
    /// the chord). The caller dimensions the two attach stubs through the normal
    /// flow, then calls [`finish_arc`](Self::finish_arc) to bake the arc in —
    /// nothing about the geometry changes until then.
    pub fn begin_arc(&mut self, stroke: &[Point2f32]) -> Result<(VertexId, VertexId, f32), Error> {
        let p1 = *stroke.first().ok_or(Error::InsufficientPoints)?;
        let p2 = *stroke.last().ok_or(Error::InsufficientPoints)?;
        let dx = p2.x - p1.x;
        let dy = p2.y - p1.y;
        let chord = (dx * dx + dy * dy).sqrt();
        if chord < 1e-4 {
            return Err(Error::ExtendError("curve endpoints coincide".into()));
        }
        let bulge_ratio = geom::stroke_sagitta(stroke) / chord;
        // Splice the straight chord (no bulge): inserts the two endpoints as
        // pinned vertices. An endpoint landing on an existing corner reuses that
        // corner vertex (see `splice_cleaned`) rather than floating a new point.
        self.splice_cleaned(&[p1, p2], false)?;
        let v1 = self
            .vertex_near(p1)
            .ok_or_else(|| Error::ExtendError("lost arc start vertex".into()))?;
        let v2 = self
            .vertex_near(p2)
            .ok_or_else(|| Error::ExtendError("lost arc end vertex".into()))?;
        Ok((v1, v2, bulge_ratio))
    }

    /// Bake a provisional arc (from [`begin_arc`](Self::begin_arc)) into the
    /// model now that its endpoints are positioned: facet the circular arc
    /// between `v1` and `v2` (bulge = `bulge_ratio` × the current chord) and
    /// replace the straight placeholder edge with the facet vertices. The two
    /// endpoints (and their dimensioned attach stubs) are kept untouched; only
    /// the placeholder edge is subdivided. A near-straight arc is left flat.
    ///
    /// Returns the freshly-minted parameter IDs of the facet vertices. The
    /// caller should treat these as KNOWN (locked): the curve's shape is fully
    /// determined once its attach lines are set, so the dimensioning solver must
    /// not later redistribute the facet vertices (`repair`/`aesthetic_rescale`
    /// only move un-set params) — doing so would flatten the arc back to a line.
    pub fn finish_arc(
        &mut self,
        v1: VertexId,
        v2: VertexId,
        bulge_ratio: f32,
    ) -> Result<Vec<ParameterId>, Error> {
        use crate::extend::SpliceElem;
        let (r1, i1) = self
            .find_vertex(v1)
            .ok_or_else(|| Error::ExtendError("arc start vertex gone".into()))?;
        let (r2, i2) = self
            .find_vertex(v2)
            .ok_or_else(|| Error::ExtendError("arc end vertex gone".into()))?;
        if r1 != r2 {
            return Err(Error::ExtendError(
                "arc endpoints on different rings".into(),
            ));
        }
        let ring = self.parametric.ring_points(r1);
        let n = ring.len();
        let p1 = ring[i1];
        let p2 = ring[i2];
        let chord = ((p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2)).sqrt();
        let facets = geom::circular_arc_points(p1, p2, bulge_ratio * chord, 5.0);
        if facets.len() <= 2 {
            return Ok(Vec::new()); // ~straight: leave the flat placeholder edge.
        }
        let interior = &facets[1..facets.len() - 1];

        // The placeholder is the edge joining v1 and v2 directly; find its ring
        // direction so the facets are inserted in between in the right order.
        let (start_idx, forward) = if (i1 + 1) % n == i2 {
            (i1, true) // ring goes v1 → v2
        } else if (i2 + 1) % n == i1 {
            (i2, false) // ring goes v2 → v1
        } else {
            return Err(Error::ExtendError(
                "arc endpoints are no longer adjacent".into(),
            ));
        };
        let mut seq: Vec<SpliceElem> = Vec::with_capacity(n + interior.len());
        for idx in 0..n {
            seq.push(SpliceElem::Keep(idx));
            if idx == start_idx {
                if forward {
                    seq.extend(interior.iter().map(|f| SpliceElem::New(*f)));
                } else {
                    seq.extend(interior.iter().rev().map(|f| SpliceElem::New(*f)));
                }
            }
        }
        self.parametric.splice_into_edge(r1, &seq)
    }

    /// World-space facet points of a provisional arc (between two pinned
    /// endpoints), for drawing the preview before it's committed. Uses the same
    /// sampling [`finish_arc`](Self::finish_arc) will. `None` if a vertex is gone.
    pub fn arc_preview(
        &self,
        v1: VertexId,
        v2: VertexId,
        bulge_ratio: f32,
    ) -> Option<Vec<Point2f32>> {
        let (_, i1) = self.find_vertex(v1)?;
        let (_, i2) = self.find_vertex(v2)?;
        let ring = self.parametric.ring_points(0);
        let p1 = *ring.get(i1)?;
        let p2 = *ring.get(i2)?;
        let chord = ((p2.x - p1.x).powi(2) + (p2.y - p1.y).powi(2)).sqrt();
        Some(geom::circular_arc_points(p1, p2, bulge_ratio * chord, 5.0))
    }

    /// `VertexId` of the primary ring's vertex closest to `pt` (used to recover
    /// the endpoints `begin_arc` just spliced in by their pinned position).
    fn vertex_near(&self, pt: Point2f32) -> Option<VertexId> {
        let ring = self.parametric.ring_points(0);
        let mut best: Option<(f32, usize)> = None;
        for (i, v) in ring.iter().enumerate() {
            let d = (v.x - pt.x).powi(2) + (v.y - pt.y).powi(2);
            if best.is_none_or(|(bd, _)| d < bd) {
                best = Some((d, i));
            }
        }
        best.and_then(|(_, i)| self.vertex_id_at(0, i))
    }

    /// Concrete vertex positions of the first polygon (backwards-compatible
    /// for single-polygon callers). For multi-polygon access, use `polygons()`.
    pub fn get_points(&self) -> Vec<Point2f32> {
        self.parametric.ring_points(0)
    }

    /// All polygons' vertex positions, in ring order.
    pub fn polygons(&self) -> Vec<Vec<Point2f32>> {
        self.parametric.all_ring_points()
    }

    /// Number of polygons (rings) in this floor plan.
    pub fn ring_count(&self) -> usize {
        self.parametric.ring_count()
    }

    /// Number of independent parameters controlling the plan's geometry.
    /// Drops when collinearity merges reduce the degrees of freedom.
    pub fn active_parameter_count(&self) -> usize {
        self.parametric.active_parameter_count
    }

    /// Detect pairs of axis-aligned edges that are close to being collinear
    /// but currently aren't. Suggestions can span rings (cross-polygon).
    /// See `ParametricPlan::suggest_collinear` for details.
    pub fn suggest_collinear(
        &self,
        min_separation: f32,
        max_separation: f32,
    ) -> Vec<CollinearitySuggestion> {
        self.parametric
            .suggest_collinear(min_separation, max_separation)
    }

    /// Collinearity suggestions that involve at least one of `new_params` —
    /// the walls a just-applied extend/trim added. Lets the caller
    /// ask whether a freshly-drawn wall lines up with an existing one, WITHOUT
    /// re-surfacing pairs among the pre-existing walls. Separation thresholds
    /// scale with the bounding-box diagonal, so it behaves the same in pixel
    /// units (pre-dimensioning) and metres (post-dimensioning).
    pub fn collinear_suggestions_for_params(
        &self,
        new_params: &[ParameterId],
    ) -> Vec<CollinearitySuggestion> {
        let diag = self
            .bounding_box()
            .map(|((mnx, mny), (mxx, mxy))| ((mxx - mnx).powi(2) + (mxy - mny).powi(2)).sqrt())
            .unwrap_or(0.0);
        if diag <= 0.0 {
            return Vec::new();
        }
        // No lower bound: freehand drawing isn't precise, so two walls landing
        // close together — even exactly coincident — is chance, not intent.
        // Those are still independent parameters, so we ask rather than assume
        // (`suggest_collinear` only auto-skips walls that already SHARE an
        // expression, i.e. are already the same parameter). Beyond ~12% of the
        // diagonal the walls are clearly distinct — that stays the upper cutoff.
        let min_sep = 0.0;
        let max_sep = (diag * 0.12).max(1e-3);
        let new: std::collections::HashSet<ParameterId> = new_params.iter().copied().collect();
        self.parametric
            .suggest_collinear(min_sep, max_sep)
            .into_iter()
            .filter(|s| {
                let involves = |ring: usize, edge: usize| {
                    self.parametric
                        .wall_movable_param(ring, edge)
                        .is_some_and(|(p, _)| new.contains(&p))
                };
                involves(s.ring_a, s.edge_a) || involves(s.ring_b, s.edge_b)
            })
            .collect()
    }

    /// Apply a collinearity decision returned from `suggest_collinear`.
    /// Merges the two edges so they share a single coordinate expression.
    pub fn apply_collinear_suggestion(&mut self, suggestion: &CollinearitySuggestion) {
        self.parametric.apply_collinear_suggestion(suggestion);
    }

    /// Are these two edges currently collinear along the given axis?
    /// Takes ring indices so cross-polygon queries are possible.
    pub fn edges_collinear(
        &self,
        ring_a: usize,
        edge_a: usize,
        ring_b: usize,
        edge_b: usize,
        axis: &Axis,
    ) -> bool {
        self.parametric
            .edges_collinear(ring_a, edge_a, ring_b, edge_b, axis)
    }

    /// Get the stable ID of the vertex at `(ring, vertex_idx)`. Use this when
    /// you need a reference that survives later edits (e.g. for dimension
    /// prompts that should outlive intervening reflows).
    pub fn vertex_id_at(&self, ring: usize, vertex_idx: usize) -> Option<VertexId> {
        self.parametric.vertex_id_at(ring, vertex_idx)
    }

    /// Look up a vertex by stable ID. Returns `(ring_index, vertex_index)`.
    pub fn find_vertex(&self, id: VertexId) -> Option<(usize, usize)> {
        self.parametric.find_vertex(id)
    }

    /// Current length of the wall starting at `(ring, edge_start)` in
    /// whatever units the plan is currently in (pixels before the first
    /// dimension, real units after).
    pub fn wall_length(&self, ring: usize, edge_start: usize) -> Option<f32> {
        self.parametric.wall_length(ring, edge_start)
    }

    /// Parameter IDs the wall's length depends on. For an axis-aligned wall,
    /// usually one (e.g. a wall whose start is at a constant and end is at
    /// a parameter) or two (a wall between two parameter coordinates).
    pub fn wall_param_deps(&self, ring: usize, edge_start: usize) -> Vec<ParameterId> {
        self.parametric.wall_param_deps(ring, edge_start)
    }

    /// All parameter IDs currently referenced by any vertex.
    pub fn all_active_params(&self) -> std::collections::HashSet<ParameterId> {
        self.parametric.all_active_params()
    }

    /// Multiply every parameter value (and every constant) by `factor`. Used
    /// when the first user-entered dimension converts the pixel-scale polygon
    /// into real-world units uniformly.
    pub fn scale_all(&mut self, factor: f32) {
        self.parametric.scale_all(factor);
    }

    /// Set the value of a single parameter directly. Used to apply a user-
    /// entered dimension to the unknown parameter a wall constrains.
    pub fn set_parameter(&mut self, param_id: ParameterId, value: f32) {
        self.parametric.set_parameter(param_id, value);
    }

    /// Change `unknown_param` so the wall has the given length. Returns an
    /// error if the wall isn't axis-aligned or doesn't reference the param.
    pub fn set_wall_length(
        &mut self,
        ring: usize,
        edge_start: usize,
        new_length: f32,
        unknown_param: ParameterId,
    ) -> Result<(), Error> {
        self.parametric
            .set_wall_length(ring, edge_start, new_length, unknown_param)
    }

    /// Validate that every ring is a simple axis-aligned polygon. Used after
    /// applying a dimension to confirm the user's input is consistent with the
    /// shape — e.g. for an E, the right-side vertical segments must collectively
    /// span the same height as the spine.
    pub fn validate_simple(&self) -> Result<(), Error> {
        self.parametric.validate_simple()
    }

    /// Which Cartesian axis the given parameter controls.
    pub fn param_axis(&self, p: ParameterId) -> Option<ParameterAxis> {
        self.parametric.param_axis(p)
    }

    /// All walls in the plan whose length expression matches the wall at
    /// `(ring, edge_start)`. Returns `(ring, edge_start)` tuples including
    /// the input wall. The UI uses this to show the user every wall they
    /// can equivalently measure to satisfy a dimension prompt.
    pub fn equivalent_length_walls(&self, ring: usize, edge_start: usize) -> Vec<(usize, usize)> {
        self.parametric.equivalent_length_walls(ring, edge_start)
    }

    /// For an axis-aligned wall, the parameter that translates it under a
    /// perpendicular drag and the axis that parameter controls. None if the
    /// wall's perpendicular coord is a constant (anchored to V0).
    pub fn wall_movable_param(
        &self,
        ring: usize,
        edge_start: usize,
    ) -> Option<(ParameterId, ParameterAxis)> {
        self.parametric.wall_movable_param(ring, edge_start)
    }

    /// Insert a new vertex on the wall at `(ring, edge_start)`, placing it
    /// at world position `(world_x, world_y)`. Splits one edge into two
    /// and allocates two fresh parameters (one per axis) for the new
    /// vertex. Returns its new `VertexId`, or `None` if the ring/edge
    /// indices are out of range.
    pub fn insert_vertex_on_wall(
        &mut self,
        ring: usize,
        edge_start: usize,
        world_x: f32,
        world_y: f32,
    ) -> Option<VertexId> {
        self.parametric
            .insert_vertex_on_wall(ring, edge_start, world_x, world_y)
    }

    /// Whether the wall starting at `(ring, edge_start)` is horizontal,
    /// vertical, or non-axis-aligned (`None`). See
    /// [`ParametricPlan::wall_axis`](parametric::ParametricPlan::wall_axis).
    pub fn wall_axis(&self, ring: usize, edge_start: usize) -> Option<Axis> {
        self.parametric.wall_axis(ring, edge_start)
    }

    /// Remove the vertex at `(ring, idx)`, collapsing the walls around it. See
    /// [`ParametricPlan::remove_vertex`](parametric::ParametricPlan::remove_vertex).
    pub fn remove_vertex(&mut self, ring: usize, idx: usize) -> bool {
        self.parametric.remove_vertex(ring, idx)
    }

    /// Drop spurious collinear vertices the outline doesn't need (e.g. the
    /// corner a flush porch leaves mid-wall), keeping any in `preserve` (marked
    /// subdivision points such as a flooring-type boundary). Surviving vertices
    /// keep their IDs/params; returns the removed vertex IDs. See
    /// [`ParametricPlan::remove_collinear_vertices`](parametric::ParametricPlan::remove_collinear_vertices).
    pub fn remove_collinear_vertices(&mut self, preserve: &HashSet<VertexId>) -> Vec<VertexId> {
        self.parametric.remove_collinear_vertices(preserve)
    }

    /// Collapse a trim cut wall to zero (the wall identified by `edge_start` /
    /// `controlled_param`). See
    /// [`ParametricPlan::collapse_trim_wall`](parametric::ParametricPlan::collapse_trim_wall).
    pub fn collapse_trim_wall(
        &mut self,
        ring: usize,
        edge_start: usize,
        controlled_param: ParameterId,
    ) -> Result<(), Error> {
        self.parametric
            .collapse_trim_wall(ring, edge_start, controlled_param)
    }

    /// Every wall whose length is fully determined by `known` parameters — all
    /// the parameters in its length expression are known. A superset of the
    /// user-dimensioned walls (also catches lengths implied by shared
    /// parameters, e.g. the opposite side of a rectangle). Walls with no
    /// parameter deps (pre-dimensioning pixel artefacts) are excluded.
    pub fn walls_with_known_length(&self, known: &HashSet<ParameterId>) -> Vec<(usize, usize)> {
        let polygons = self.polygons();
        let mut result = Vec::new();
        for (ri, ring) in polygons.iter().enumerate() {
            for i in 0..ring.len() {
                let deps = self.wall_param_deps(ri, i);
                if !deps.is_empty() && deps.iter().all(|p| known.contains(p)) {
                    result.push((ri, i));
                }
            }
        }
        result
    }

    /// Pick the next wall to ask the user to dimension: the first wall (in ring
    /// order) whose length depends on exactly ONE not-yet-`known` parameter
    /// (the rest known) and whose start vertex isn't in `skipped`. Returns
    /// `(start vertex id, the one unknown parameter)`, or `None` when every
    /// active parameter is known or no clean single-unknown wall remains. The
    /// caller decides what to do with the result (the dimensioning workflow).
    pub fn next_promptable_wall(
        &self,
        known: &HashSet<ParameterId>,
        skipped: &HashSet<VertexId>,
    ) -> Option<(VertexId, ParameterId)> {
        let to_ask: HashSet<ParameterId> = self
            .all_active_params()
            .into_iter()
            .filter(|p| !known.contains(p))
            .collect();
        if to_ask.is_empty() {
            return None;
        }
        let polygons = self.polygons();
        for (ring_idx, ring_points) in polygons.iter().enumerate() {
            for edge_start in 0..ring_points.len() {
                let Some(vid) = self.vertex_id_at(ring_idx, edge_start) else {
                    continue;
                };
                if skipped.contains(&vid) {
                    continue;
                }
                // Don't prompt facet walls of a curve. A non-axis-aligned wall
                // is dimensionable (a Trim diagonal) only when it's ISOLATED —
                // both neighbours axis-aligned. A curve is a RUN of consecutive
                // non-axis-aligned facets, so each facet has a non-aligned
                // neighbour; those aren't individually dimensioned.
                if self.wall_axis(ring_idx, edge_start).is_none() {
                    let len = ring_points.len();
                    let prev = (edge_start + len - 1) % len;
                    let next = (edge_start + 1) % len;
                    if self.wall_axis(ring_idx, prev).is_none()
                        || self.wall_axis(ring_idx, next).is_none()
                    {
                        continue;
                    }
                }
                // Promptable iff exactly one dep is in `to_ask` and the rest are
                // known. A dep that's neither blocks this wall (its only setter
                // was skipped / never set).
                let mut to_ask_count = 0usize;
                let mut to_ask_param: Option<ParameterId> = None;
                let mut blocked = false;
                for d in self.wall_param_deps(ring_idx, edge_start) {
                    if to_ask.contains(&d) {
                        to_ask_count += 1;
                        to_ask_param = Some(d);
                    } else if !known.contains(&d) {
                        blocked = true;
                        break;
                    }
                }
                if !blocked && to_ask_count == 1 {
                    return Some((vid, to_ask_param.unwrap()));
                }
            }
        }
        None
    }

    /// For a wall whose perpendicular coord is a constant (V0's anchor or
    /// similar), the current constant value and the axis it lives on.
    /// Edit-mode drag uses this together with `shift_constant` to translate
    /// the polygon's anchor walls (leftmost / bottommost on an E, etc.).
    pub fn wall_anchor_constant(
        &self,
        ring: usize,
        edge_start: usize,
    ) -> Option<(f32, ParameterAxis)> {
        self.parametric.wall_anchor_constant(ring, edge_start)
    }

    /// Shift every vertex whose `axis`-coord equals `old_value` (within a
    /// small tolerance) to `new_value`. Companion to `wall_anchor_constant`.
    pub fn shift_constant(&mut self, old_value: f32, axis: ParameterAxis, new_value: f32) {
        self.parametric.shift_constant(old_value, axis, new_value);
    }

    /// Hit-test a world-coord point against the polygon's walls. Returns
    /// the `(ring, edge_start)` of the closest wall within `tol` (same
    /// units as the polygon coords), or `None` if every wall is farther.
    pub fn hit_test_wall(&self, world_x: f64, world_y: f64, tol: f64) -> Option<(usize, usize)> {
        let polygons = self.polygons();
        let mut best: Option<(f64, usize, usize)> = None;
        for (ri, ring) in polygons.iter().enumerate() {
            let n = ring.len();
            for i in 0..n {
                let a = ring[i];
                let b = ring[(i + 1) % n];
                let d = point_to_segment_distance(
                    world_x, world_y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                );
                if d <= tol && (best.is_none() || d < best.unwrap().0) {
                    best = Some((d, ri, i));
                }
            }
        }
        best.map(|(_, r, e)| (r, e))
    }

    /// The ParameterId controlling vertex `(ring, idx)`'s x coord, or None.
    pub fn vertex_x_param(&self, ring: usize, idx: usize) -> Option<ParameterId> {
        self.parametric.vertex_x_param(ring, idx)
    }

    /// The ParameterId controlling vertex `(ring, idx)`'s y coord, or None.
    pub fn vertex_y_param(&self, ring: usize, idx: usize) -> Option<ParameterId> {
        self.parametric.vertex_y_param(ring, idx)
    }

    /// Current value of a parameter (in the plan's current units), or None.
    pub fn parameter_value(&self, p: ParameterId) -> Option<f32> {
        self.parametric.parameter_value(p)
    }

    /// Multiply a single parameter's value by `factor`.
    pub fn scale_parameter(&mut self, param: ParameterId, factor: f32) {
        self.parametric.scale_parameter(param, factor);
    }

    /// Rebuild every ring with zero-length edges and collinear middle vertices
    /// removed, then re-run the orthogonal-constraints pass so coordinate
    /// expressions are clean. Used after the collinearity review has merged
    /// edges that were "the same line" — the merges can leave duplicate
    /// vertices (zero-length edges) and three-on-a-line vertices that
    /// shouldn't appear in dimensioning prompts or downstream area calcs.
    ///
    /// Side effect: vertex IDs are reassigned (the parametric layer is rebuilt
    /// from scratch). Only safe to call before dimensioning has started —
    /// otherwise the controller's `known_params` set would dangle. Caller
    /// is responsible for committing the simplified plan to the BER.
    pub fn simplify_degenerate(&mut self) {
        let tolerance: f32 = 0.5;
        let new_rings: Vec<Vec<Point2f32>> = self
            .polygons()
            .into_iter()
            .map(|ring| {
                let deduped = orthogonal::remove_duplicates(&ring, tolerance);
                orthogonal::remove_collinear(&deduped, tolerance)
            })
            .filter(|r| r.len() >= 3)
            .collect();

        if new_rings.is_empty() {
            self.parametric = parametric::ParametricPlan::default();
            return;
        }

        let mut new_parametric = parametric::ParametricPlan::from_points(&new_rings[0]);
        for ring in new_rings.iter().skip(1) {
            new_parametric.add_ring(ring);
        }
        let _ = new_parametric.apply_orthogonal_constraints();
        self.parametric = new_parametric;
    }

    /// Bounding box of the entire plan (across all rings) in current units.
    pub fn bounding_box(&self) -> Option<((f32, f32), (f32, f32))> {
        let mut iter = self.polygons().into_iter().flatten();
        let first = iter.next()?;
        let mut min_x = first.x;
        let mut min_y = first.y;
        let mut max_x = first.x;
        let mut max_y = first.y;
        for p in iter {
            min_x = min_x.min(p.x);
            min_y = min_y.min(p.y);
            max_x = max_x.max(p.x);
            max_y = max_y.max(p.y);
        }
        Some(((min_x, min_y), (max_x, max_y)))
    }

    /// Per-edge direction signs, for the direction-flip guard. See
    /// [`ParametricPlan::edge_signs`](parametric::ParametricPlan::edge_signs).
    pub fn edge_signs(&self) -> Vec<Vec<(i8, i8)>> {
        self.parametric.edge_signs()
    }

    /// Repair an invalid polygon by redistributing the parameters not in
    /// `known` along `axis`. See
    /// [`ParametricPlan::repair_unset_params`](parametric::ParametricPlan::repair_unset_params).
    pub fn repair_unset_params(
        &mut self,
        axis: ParameterAxis,
        known: &HashSet<ParameterId>,
        snapshot: &FloorPlan,
    ) -> bool {
        self.parametric
            .repair_unset_params(axis, known, &snapshot.parametric)
    }

    /// Nudge an extreme aspect ratio back toward ~3:1 by scaling un-set
    /// parameters. See
    /// [`ParametricPlan::aesthetic_rescale`](parametric::ParametricPlan::aesthetic_rescale).
    pub fn aesthetic_rescale(&mut self, known: &HashSet<ParameterId>) {
        self.parametric.aesthetic_rescale(known)
    }
}

/// Signed area of a closed polygon (shoelace). Sign reflects winding;
/// `trim_by_line` only uses the magnitude to pick the larger side.
fn polygon_area(points: &[Point2f32]) -> f32 {
    let n = points.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0_f32;
    for i in 0..n {
        let a = points[i];
        let b = points[(i + 1) % n];
        sum += a.x * b.y - b.x * a.y;
    }
    sum * 0.5
}

/// Perpendicular distance from `(px, py)` to the line segment from
/// `(ax, ay)` to `(bx, by)`. Returns the distance to the nearest endpoint
/// when the point projects outside the segment.
fn point_to_segment_distance(px: f64, py: f64, ax: f64, ay: f64, bx: f64, by: f64) -> f64 {
    let abx = bx - ax;
    let aby = by - ay;
    let apx = px - ax;
    let apy = py - ay;
    let ab_len_sq = abx * abx + aby * aby;
    if ab_len_sq < 1e-12 {
        return (apx * apx + apy * apy).sqrt();
    }
    let t = ((apx * abx + apy * aby) / ab_len_sq).clamp(0.0, 1.0);
    let proj_x = ax + t * abx;
    let proj_y = ay + t * aby;
    let dx = px - proj_x;
    let dy = py - proj_y;
    (dx * dx + dy * dy).sqrt()
}

#[cfg(all(test, feature = "serde"))]
mod serde_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A baked (faceted) curve is just extra straight-edged vertices + params in
    /// the parametric model, so saving and reopening a BER must reproduce the
    /// curve — facets, geometry, and parameter count — not a straight line.
    #[test]
    fn baked_curve_survives_json_roundtrip() {
        let square = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        let arc = vec![
            p(3.0, 10.0),
            p(4.0, 11.0),
            p(5.0, 11.5),
            p(6.0, 11.0),
            p(7.0, 10.0),
        ];
        let (v1, v2, bulge) = plan.begin_arc(&arc).unwrap();
        plan.finish_arc(v1, v2, bulge).unwrap();

        let diag = |pl: &FloorPlan| {
            let r = &pl.polygons()[0];
            let n = r.len();
            (0..n)
                .filter(|&i| {
                    let (a, b) = (r[i], r[(i + 1) % n]);
                    (a.x - b.x).abs() > 1e-3 && (a.y - b.y).abs() > 1e-3
                })
                .count()
        };
        let verts_before = plan.polygons()[0].len();
        let params_before = plan.active_parameter_count();
        let diag_before = diag(&plan);
        assert!(
            diag_before >= 2,
            "precondition: the curve baked into facets"
        );

        let json = serde_json::to_string(&plan).expect("serialize");
        let back: FloorPlan = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(
            back.polygons()[0].len(),
            verts_before,
            "facet vertices kept"
        );
        assert_eq!(back.active_parameter_count(), params_before);
        assert_eq!(diag(&back), diag_before, "the curve's facets must survive");
        for (a, b) in back
            .polygons()
            .iter()
            .flatten()
            .zip(plan.polygons().iter().flatten())
        {
            assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
        }
    }

    /// The persisted snapshot must preserve the parametric structure, not just
    /// the points: collinearity equivalences are shared parameters, so the
    /// active-parameter count (which drops when walls share a coord) is the
    /// signal. A square has 2 params; after a porch extend it gains a few. The
    /// round-trip must reproduce that exactly — otherwise reopening a saved BER
    /// would lose the equivalences.
    #[test]
    fn floor_plan_json_roundtrip_preserves_parametric_structure() {
        // Last point mimics the user's "lift near the start" drag point that
        // `from_points_to_orthogonal` drops for an implicitly-closed stroke.
        let square = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&square, 0.5, true).unwrap();
        // Porch out of the top edge — creates new shared-param equivalences.
        let porch = vec![p(3.0, 0.0), p(3.0, -4.0), p(7.0, -4.0), p(7.0, 0.0)];
        plan.extend_ortho(&porch).unwrap();

        let params_before = plan.active_parameter_count();
        let rings_before: Vec<usize> = plan.polygons().iter().map(|r| r.len()).collect();

        let json = serde_json::to_string(&plan).expect("serialize");
        let back: FloorPlan = serde_json::from_str(&json).expect("deserialize");

        // The private id counters MUST persist: if they reset, a post-reload
        // edit would mint param/vertex ids that collide with existing ones and
        // silently corrupt `known_params` / vertex identity.
        assert!(
            json.contains("next_param_id") && json.contains("next_vertex_id"),
            "id counters must be serialized to prevent post-reload id collisions"
        );

        assert_eq!(
            back.active_parameter_count(),
            params_before,
            "shared-parameter equivalences must survive the round-trip"
        );
        let rings_after: Vec<usize> = back.polygons().iter().map(|r| r.len()).collect();
        assert_eq!(rings_after, rings_before);
        // Geometry matches too (within f32 round-trip tolerance).
        for (a, b) in back
            .polygons()
            .iter()
            .flatten()
            .zip(plan.polygons().iter().flatten())
        {
            assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
        }
    }
}

#[cfg(test)]
mod trim_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
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

#[cfg(all(test, feature = "serde"))]
mod trim_serde_tests {
    use super::*;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    #[test]
    fn trimmed_storey_survives_json_roundtrip() {
        let stroke = vec![
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 10.0),
            p(0.0, 10.0),
            p(0.2, 0.2),
        ];
        let mut plan = FloorPlan::from_points_to_orthogonal(&stroke, 0.5, true).unwrap();
        plan.trim_by_line(p(10.0, 4.0), p(6.0, 0.0)).unwrap();

        let rings_before = plan.polygons();
        let params_before = plan.active_parameter_count();

        let json = serde_json::to_string(&plan).expect("serialize");
        let back: FloorPlan = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(back.active_parameter_count(), params_before);
        let rings_after = back.polygons();
        assert_eq!(rings_after.len(), rings_before.len());
        assert_eq!(rings_after[0].len(), rings_before[0].len());
        // The diagonal cut edge comes back intact.
        for (a, b) in rings_after[0].iter().zip(rings_before[0].iter()) {
            assert!((a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3);
        }
    }
}
