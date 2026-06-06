//! Curved-wall (arc) capture — provisional bulge + bake-in.
//!
//! `begin_arc`/`finish_arc` mutate the ring (`&mut self`), so like `trim` these
//! stay `FloorPlan` methods; `arc_preview`/`vertex_near` are `&self` queries on
//! the model. None are pure `ring → FloorPlan` geometry, so none move out of
//! `FloorPlan` — this is organisational only.
use crate::geom;
use crate::{Error, FloorPlan, ParameterId, VertexId};
use domain::points::Point2f32;

impl FloorPlan {
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
        use crate::splice::SpliceElem;
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
}
