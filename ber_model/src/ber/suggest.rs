//! Upper-storey OUTLINE SUGGESTIONS — given the storey below, propose
//! fully-finalised outline rings the new storey could adopt instead of being
//! drawn. A pure data transform over the below storey's resolved geometry
//! (flat ceiling regions + footprint): the document layer adopts a chosen
//! ring, the controller sequences the flow. Heuristics are the extension
//! point — see [`HEURISTICS`].

use geo::bool_ops::BooleanOps;
use geo::{Area, BoundingRect, Intersects, MultiPolygon, Polygon};

use domain::points::Point2f64;

use super::structural::{
    ring_to_poly, split_to_simple, strip_collinear, Storey, RISER_Z_EPS_M, SLIVER_AREA_M2,
};

/// Candidate rings below this plan area (m²) are rejected by the pipeline.
/// This floor exists to kill SLIVERS (boolean-noise fragments), not small
/// rooms — anything under ~0.7 m × 0.7 m is geometry noise, but a small
/// flat ceiling is still a legitimate storey to build on.
pub(crate) const MIN_SUGGESTION_AREA_M2: f64 = 0.5;

/// Scoring weights (per the approved spec these live in code, not
/// `tuning.yaml`). The score B/C consume — higher is better:
///
/// `score = W_COVERAGE · area / footprint_area`
/// `      + W_COMPACTNESS · area / bbox_area`
/// `      − LEVEL_PENALTY · (levels − 1)`
/// `      − VERTEX_PENALTY · max(verts − 4, 0)`
///
/// i.e. prefer covering more of the storey below, prefer compact
/// (bbox-filling) shapes, mildly penalise each inherited floor step beyond
/// the first level, and mildly penalise vertices beyond a rectangle's 4.
const W_COVERAGE: f64 = 1.0;
const W_COMPACTNESS: f64 = 0.5;
const LEVEL_PENALTY: f64 = 0.15;
const VERTEX_PENALTY: f64 = 0.02;

/// Two suggestions are DUPLICATES when their symmetric-difference area is
/// below this fraction of the larger one's area; the lower-scored is dropped.
const DEDUP_SYM_DIFF_FRAC: f64 = 0.02;

/// One suggested outline for the storey above, ready to adopt as a
/// fully-finalised outline (the `from_storey` pattern — no carve, no
/// collinearity review).
#[derive(Clone, Debug)]
pub struct OutlineSuggestion {
    /// The closed outline ring in world metres, OPEN representation (no
    /// closing duplicate), f32-quantized at this boundary (the
    /// `Ber::add_ceiling_apex` precedent — the document layer round-trips rings
    /// through `Point2f32`) and collinear-stripped.
    pub ring: Vec<Point2f64>,
    /// Plan area of `ring` (m²).
    pub area_m2: f64,
    /// Number of DISTINCT flat ceiling levels of the storey below under the
    /// ring (distinct within [`RISER_Z_EPS_M`]; ≥ 1). Each level beyond the
    /// first becomes an inherited floor step on the new storey.
    pub levels: usize,
    /// The label of the heuristic that produced it (its [`HEURISTICS`]
    /// entry).
    pub source: &'static str,
    /// Ranking score — see the weight constants for the exact formula.
    pub score: f64,
}

/// Everything a heuristic may look at, derived once per
/// [`suggest_storey_outlines`] call from the storey below.
#[derive(Clone, Debug)]
pub(crate) struct SuggestionContext {
    /// The below storey's FLAT ceiling regions as `(plan ring, flat z)`, in
    /// region order. Rings are open (no closing duplicate).
    pub flats: Vec<(Vec<Point2f64>, f64)>,
    /// Plan union of all `flats`, as SIMPLE exterior rings (holes decomposed
    /// via `split_to_simple`, sub-sliver pieces dropped) — the area the new
    /// storey can bear on.
    pub flat_union: Vec<Vec<Point2f64>>,
    /// The below storey's footprint ring (`FloorRegions::footprint_ring`).
    pub footprint: Vec<Point2f64>,
    /// Plan area of `footprint` (m²).
    pub footprint_area: f64,
    /// True when EVERY below-ceiling region is flat (no sloped pieces —
    /// stepped flats still count as flat).
    pub all_flat: bool,
}

/// A suggestion heuristic: a label (becomes [`OutlineSuggestion::source`])
/// plus a function from the context to RAW candidate rings.
pub(crate) type OutlineHeuristic = (&'static str, fn(&SuggestionContext) -> Vec<Vec<Point2f64>>);

/// The suggestion heuristics, in the order they run.
///
/// TO ADD A HEURISTIC: write one `fn(&SuggestionContext) ->
/// Vec<Vec<Point2f64>>` returning raw candidate rings (open, world metres)
/// and append one `(label, fn)` entry here — nothing else. The pipeline
/// ([`suggest_storey_outlines`]) validates every ring (≥ 3 vertices, area ≥
/// [`MIN_SUGGESTION_AREA_M2`], collinear-strip, f32-quantize), scores,
/// sorts, dedups and caps; a heuristic never needs to do any of that, and
/// returning an empty/degenerate ring is always safe.
pub(crate) const HEURISTICS: &[OutlineHeuristic] = &[
    ("all-flat-union", h1_all_flat_union),
    ("largest-flat", h2_largest_flat_region),
    ("same-z-group", h3_same_z_groups),
    ("grown-from-largest", h4_grown_from_largest),
    ("inscribed-rect", h5_inscribed_rect),
    ("full-footprint", h6_full_footprint),
];

/// Whether [`suggest_storey_outlines`] has anything to work from — the CHEAP
/// mirror of its empty-guards (no unions, no scoring): `below` has its
/// ceiling height set, a non-sliver footprint, and at least one flat ceiling
/// region. KEEP IN LOCK-STEP with the guards at the top of
/// [`suggest_storey_outlines`]. The engine can still come up empty on
/// validation/dedup, so a caller gating on this must tolerate an empty
/// result from the engine itself.
pub fn has_suggestion_inputs(below: &Storey) -> bool {
    below.ceiling_height_m.is_some()
        && below
            .floors
            .footprint_ring()
            .is_some_and(|fp| ring_to_poly(&fp).unsigned_area() > SLIVER_AREA_M2)
        && below
            .ceilings
            .regions
            .iter()
            .any(|r| r.polygon.exterior.len() >= 3 && r.flat_z().is_some())
}

/// Suggest outline rings for the storey above `below`, best first, at most
/// `max` of them.
///
/// Pipeline: run every [`HEURISTICS`] entry over the [`SuggestionContext`]
/// → VALIDATE each candidate (collinear-strip, f32-quantize, then require
/// ≥ 3 vertices and area ≥ [`MIN_SUGGESTION_AREA_M2`]) → SCORE (see the
/// weight constants) → SORT by score descending (stable, so earlier
/// heuristics win ties) → DEDUP (symmetric-difference area <
/// [`DEDUP_SYM_DIFF_FRAC`] of the larger — keep the higher-scored) → CAP at
/// `max`.
///
/// Empty when `below` has no default ceiling height yet (its ceilings
/// aren't derived — the controller's gate, defended here too), no usable
/// footprint, or no flat ceiling region.
pub fn suggest_storey_outlines(below: &Storey, max: usize) -> Vec<OutlineSuggestion> {
    if below.ceiling_height_m.is_none() {
        return Vec::new();
    }
    let Some(footprint) = below.floors.footprint_ring() else {
        return Vec::new();
    };
    let footprint_area = ring_to_poly(&footprint).unsigned_area();
    if footprint_area <= SLIVER_AREA_M2 {
        return Vec::new();
    }
    let mut flats: Vec<(Vec<Point2f64>, f64)> = Vec::new();
    let mut all_flat = true;
    for r in &below.ceilings.regions {
        if r.polygon.exterior.len() < 3 {
            continue; // degenerate — neither flat nor sloped
        }
        match r.flat_z() {
            Some(z) => {
                let ring: Vec<Point2f64> = r
                    .polygon
                    .exterior
                    .iter()
                    .map(|v| Point2f64::new(v.x, v.y))
                    .collect();
                flats.push((ring, z));
            }
            None => all_flat = false,
        }
    }
    if flats.is_empty() {
        return Vec::new();
    }
    let mut acc = MultiPolygon::<f64>(Vec::new());
    for (ring, _) in &flats {
        acc = acc.union(&MultiPolygon(vec![ring_to_poly(ring)]));
    }
    let flat_union: Vec<Vec<Point2f64>> = simple_pieces(&acc).iter().map(open_ring).collect();
    let ctx = SuggestionContext {
        flats,
        flat_union,
        footprint,
        footprint_area,
        all_flat,
    };

    let mut suggestions: Vec<OutlineSuggestion> = HEURISTICS
        .iter()
        .flat_map(|(label, heuristic)| {
            heuristic(&ctx)
                .into_iter()
                .filter_map(|ring| validate_and_score(&ctx, ring, label))
        })
        .collect();
    suggestions.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<OutlineSuggestion> = Vec::new();
    for s in suggestions {
        if kept.len() >= max {
            break;
        }
        if kept.iter().any(|k| is_duplicate(k, &s)) {
            continue;
        }
        kept.push(s);
    }
    kept
}

// ── The six heuristics ──────────────────────────────────────────────────────

/// H1 — the union of ALL flat regions, largest piece: "everything level the
/// new storey could bear on", v1's single-ring rule (the largest union piece).
fn h1_all_flat_union(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    largest_ring(&ctx.flat_union).into_iter().collect()
}

/// H2 — the largest SINGLE flat region: one level, no inherited steps.
fn h2_largest_flat_region(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    ctx.flats
        .iter()
        .max_by(|(a, _), (b, _)| {
            let (aa, ab) = (
                ring_to_poly(a).unsigned_area(),
                ring_to_poly(b).unsigned_area(),
            );
            aa.partial_cmp(&ab).unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(ring, _)| ring.clone())
        .into_iter()
        .collect()
}

/// H3 — SAME-Z groups: per distinct level (within [`RISER_Z_EPS_M`]), the
/// union of that level's regions, one candidate per connected component of
/// at least [`MIN_SUGGESTION_AREA_M2`] — "the storey-above sits on one roof
/// level".
fn h3_same_z_groups(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    let mut zs: Vec<f64> = Vec::new();
    for (_, z) in &ctx.flats {
        if !zs.iter().any(|seen| (seen - z).abs() <= RISER_Z_EPS_M) {
            zs.push(*z);
        }
    }
    let mut out = Vec::new();
    for z0 in zs {
        let mut acc = MultiPolygon::<f64>(Vec::new());
        for (ring, z) in &ctx.flats {
            if (z - z0).abs() <= RISER_Z_EPS_M {
                acc = acc.union(&MultiPolygon(vec![ring_to_poly(ring)]));
            }
        }
        for piece in simple_pieces(&acc) {
            if piece.unsigned_area() >= MIN_SUGGESTION_AREA_M2 {
                out.push(open_ring(&piece));
            }
        }
    }
    out
}

/// H4 — GROWN from the largest flat region by greedy adjacent merge: start
/// at the largest region, repeatedly merge the largest remaining flat region
/// that touches the accumulated shape, and EMIT AFTER EVERY MERGE (per the
/// approved spec) — the in-between footprints between H2 and H1.
fn h4_grown_from_largest(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    let polys: Vec<(Polygon<f64>, f64)> = ctx
        .flats
        .iter()
        .map(|(ring, _)| {
            let p = ring_to_poly(ring);
            let a = p.unsigned_area();
            (p, a)
        })
        .collect();
    let Some(start) = polys
        .iter()
        .enumerate()
        .max_by(|(_, (_, a)), (_, (_, b))| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(i, _)| i)
    else {
        return Vec::new();
    };
    let mut acc = MultiPolygon(vec![polys[start].0.clone()]);
    let mut remaining: Vec<usize> = (0..polys.len()).filter(|&i| i != start).collect();
    let mut out = Vec::new();
    loop {
        let Some(next) = remaining
            .iter()
            .copied()
            .filter(|&i| acc.intersects(&polys[i].0))
            .max_by(|&i, &j| {
                polys[i]
                    .1
                    .partial_cmp(&polys[j].1)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
        else {
            break; // nothing adjacent left (disjoint leftovers never merge)
        };
        remaining.retain(|&i| i != next);
        acc = acc.union(&MultiPolygon(vec![polys[next].0.clone()]));
        out.extend(largest_ring(
            &simple_pieces(&acc)
                .iter()
                .map(open_ring)
                .collect::<Vec<_>>(),
        ));
    }
    out
}

/// H5 — the largest INSCRIBED axis-aligned rectangle in H1's piece: the
/// clean rectangular storey hiding inside an irregular flat area. Coordinate
/// grid search, O(n⁴) over the piece's distinct vertex x/y values;
/// containment by `area(rect ∩ piece) ≈ area(rect)`.
fn h5_inscribed_rect(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    let Some(piece_ring) = largest_ring(&ctx.flat_union) else {
        return Vec::new();
    };
    let piece = ring_to_poly(&piece_ring);
    let mut xs: Vec<f64> = piece_ring.iter().map(|p| p.x).collect();
    let mut ys: Vec<f64> = piece_ring.iter().map(|p| p.y).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup();
    ys.dedup();
    let mut best: Option<(f64, [f64; 4])> = None;
    for (i, &x0) in xs.iter().enumerate() {
        for &x1 in &xs[i + 1..] {
            for (k, &y0) in ys.iter().enumerate() {
                for &y1 in &ys[k + 1..] {
                    let area = (x1 - x0) * (y1 - y0);
                    if area < MIN_SUGGESTION_AREA_M2
                        || best.as_ref().is_some_and(|(b, _)| area <= *b)
                    {
                        continue;
                    }
                    let rect = ring_to_poly(&[
                        Point2f64::new(x0, y0),
                        Point2f64::new(x1, y0),
                        Point2f64::new(x1, y1),
                        Point2f64::new(x0, y1),
                    ]);
                    if rect.intersection(&piece).unsigned_area() >= area - 1e-6 {
                        best = Some((area, [x0, y0, x1, y1]));
                    }
                }
            }
        }
    }
    best.map(|(_, [x0, y0, x1, y1])| {
        vec![
            Point2f64::new(x0, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0, y1),
        ]
    })
    .into_iter()
    .collect()
}

/// H6 — the FULL footprint of the storey below, iff every ceiling region is
/// flat (a same-footprint upper storey only makes sense when nothing slopes).
fn h6_full_footprint(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    if ctx.all_flat {
        vec![ctx.footprint.clone()]
    } else {
        Vec::new()
    }
}

// ── Pipeline internals ──────────────────────────────────────────────────────

/// Validate one raw candidate ring and score it — `None` when it fails
/// validation (collinear-strip + f32-quantize, then ≥ 3 vertices and area ≥
/// [`MIN_SUGGESTION_AREA_M2`]).
fn validate_and_score(
    ctx: &SuggestionContext,
    ring: Vec<Point2f64>,
    source: &'static str,
) -> Option<OutlineSuggestion> {
    let mut ring: Vec<Point2f64> = strip_collinear(&ring)
        .iter()
        .map(|p| Point2f64::new(p.x as f32 as f64, p.y as f32 as f64))
        .collect();
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    ring.dedup(); // quantization can collapse near-identical vertices
    if ring.len() > 1 && ring.first() == ring.last() {
        ring.pop();
    }
    if ring.len() < 3 {
        return None;
    }
    let poly = ring_to_poly(&ring);
    let area_m2 = poly.unsigned_area();
    if area_m2 < MIN_SUGGESTION_AREA_M2 {
        return None;
    }
    let bbox = poly.bounding_rect()?;
    let bbox_area = bbox.width() * bbox.height();
    if bbox_area <= 0.0 {
        return None;
    }
    let levels = levels_under(ctx, &poly);
    let score = W_COVERAGE * area_m2 / ctx.footprint_area + W_COMPACTNESS * area_m2 / bbox_area
        - LEVEL_PENALTY * (levels as f64 - 1.0)
        - VERTEX_PENALTY * ring.len().saturating_sub(4) as f64;
    Some(OutlineSuggestion {
        ring,
        area_m2,
        levels,
        source,
        score,
    })
}

/// Number of distinct flat ceiling levels (within [`RISER_Z_EPS_M`]) whose
/// regions overlap `ring_poly` by more than a sliver; at least 1.
fn levels_under(ctx: &SuggestionContext, ring_poly: &Polygon<f64>) -> usize {
    let mut zs: Vec<f64> = Vec::new();
    for (ring, z) in &ctx.flats {
        let p = ring_to_poly(ring);
        if p.intersection(ring_poly).unsigned_area() <= SLIVER_AREA_M2 {
            continue;
        }
        if !zs.iter().any(|seen| (seen - z).abs() <= RISER_Z_EPS_M) {
            zs.push(*z);
        }
    }
    zs.len().max(1)
}

/// The dedup predicate: symmetric-difference area below
/// [`DEDUP_SYM_DIFF_FRAC`] of the larger suggestion's area.
fn is_duplicate(a: &OutlineSuggestion, b: &OutlineSuggestion) -> bool {
    let (pa, pb) = (ring_to_poly(&a.ring), ring_to_poly(&b.ring));
    let sym_diff = pa.union(&pb).unsigned_area() - pa.intersection(&pb).unsigned_area();
    sym_diff < DEDUP_SYM_DIFF_FRAC * a.area_m2.max(b.area_m2)
}

/// Decompose a multipolygon into SIMPLE polygons (`split_to_simple` per
/// piece), dropping sub-sliver debris.
fn simple_pieces(mp: &MultiPolygon<f64>) -> Vec<Polygon<f64>> {
    mp.0.iter()
        .flat_map(split_to_simple)
        .filter(|p| p.unsigned_area() >= SLIVER_AREA_M2)
        .collect()
}

/// A polygon's exterior as an OPEN ring (closing duplicate dropped).
fn open_ring(p: &Polygon<f64>) -> Vec<Point2f64> {
    let mut pts: Vec<Point2f64> = p
        .exterior()
        .coords()
        .map(|c| Point2f64::new(c.x, c.y))
        .collect();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    pts
}

/// The largest of `rings` by plan area, cloned. `None` when empty.
fn largest_ring(rings: &[Vec<Point2f64>]) -> Option<Vec<Point2f64>> {
    rings
        .iter()
        .max_by(|a, b| {
            let (aa, ab) = (
                ring_to_poly(a).unsigned_area(),
                ring_to_poly(b).unsigned_area(),
            );
            aa.partial_cmp(&ab).unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::common::ItemId;
    use crate::ber::structural::{CeilingRegions, FloorRegions};

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(x0, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0, y1),
        ]
    }

    /// L-shape: bottom arm x∈[0,10] y∈[0,4] (40 m²) + upper arm x∈[0,4]
    /// y∈[4,10] (24 m²) = 64 m², bbox 10 × 10.
    fn l_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(4.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ]
    }

    /// The slice-A fixture: an L-shape storey whose ceiling is STEPPED via
    /// `with_z_deltas` — the bottom arm raised to 2.7, the upper arm left at
    /// 2.4, so the flats sit at two z's.
    fn l_storey() -> Storey {
        let cid = ItemId::default();
        Storey {
            floors: FloorRegions::new(l_ring(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![l_ring()], 2.4, cid)
                .with_z_deltas(&[(square(0.0, 0.0, 10.0, 4.0), 0.3)]),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        }
    }

    #[test]
    fn l_shape_stepped_ceiling_is_ranked_and_deduped() {
        let out = suggest_storey_outlines(&l_storey(), 10);
        // Distinct survivors: the full L (H1; H4's final merge and H6's
        // footprint dedup onto it), the bottom arm (H2; H3's z = 2.7 group
        // dedups onto it), the inscribed left column (H5 — x∈[0,4] y∈[0,10],
        // found before the equal-area bottom arm in grid order), and the
        // upper arm (H3's z = 2.4 group).
        assert_eq!(out.len(), 4, "{out:?}");
        let areas: Vec<f64> = out.iter().map(|s| s.area_m2).collect();
        for (got, want) in areas.iter().zip([64.0, 40.0, 40.0, 24.0]) {
            assert!((got - want).abs() < 1e-6, "areas {areas:?}");
        }
        // Ranked best-first.
        for pair in out.windows(2) {
            assert!(pair[0].score >= pair[1].score, "{out:?}");
        }
        // The exact scores B/C consume.
        let expect = |s: &OutlineSuggestion, want: f64| {
            assert!(
                (s.score - want).abs() < 1e-9,
                "{} score {}",
                s.source,
                s.score
            );
        };
        // Full L: 64/64 + 0.5·64/100 − 0.15·(2−1) − 0.02·(6−4) = 1.13.
        expect(&out[0], 1.13);
        assert_eq!(out[0].source, "all-flat-union");
        assert_eq!(out[0].levels, 2);
        // Bottom arm: 40/64 + 0.5 − 0 − 0 = 1.125 (one level, 4 verts).
        expect(&out[1], 1.125);
        assert_eq!(out[1].source, "largest-flat");
        assert_eq!(out[1].levels, 1);
        // Left column rect: 40/64 + 0.5 − 0.15 (it spans BOTH levels) = 0.975.
        expect(&out[2], 0.975);
        assert_eq!(out[2].source, "inscribed-rect");
        assert_eq!(out[2].levels, 2);
        // Upper arm: 24/64 + 0.5 = 0.875.
        expect(&out[3], 0.875);
        assert_eq!(out[3].source, "same-z-group");
        // Every ring is f32-exact at the boundary.
        for s in &out {
            for p in &s.ring {
                assert_eq!(p.x, p.x as f32 as f64);
                assert_eq!(p.y, p.y as f32 as f64);
            }
        }
    }

    #[test]
    fn cap_truncates_after_ranking() {
        let capped = suggest_storey_outlines(&l_storey(), 2);
        assert_eq!(capped.len(), 2);
        let full = suggest_storey_outlines(&l_storey(), 10);
        assert_eq!(capped[0].source, full[0].source);
        assert_eq!(capped[1].source, full[1].source);
    }

    #[test]
    fn all_flat_yields_the_footprint_exactly_once() {
        let cid = ItemId::default();
        let ring = square(0.0, 0.0, 6.0, 8.0);
        let below = Storey {
            floors: FloorRegions::new(ring.clone(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![ring], 2.4, cid),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        // H1/H2/H5/H6 all produce the footprint — dedup keeps ONE.
        let out = suggest_storey_outlines(&below, 10);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!((out[0].area_m2 - 48.0).abs() < 1e-6);
        assert_eq!(out[0].levels, 1);
        // Full coverage + full compactness, no penalties.
        assert!((out[0].score - 1.5).abs() < 1e-9);
    }

    /// THE LIVE-REPRO REGRESSION: a SMALL storey below (1 m² flat ceiling
    /// at 2.4 m plus a 0.0875 m² pavilion flat top at 4.0 m) still gets
    /// the obvious 1 m² suggestion — [`MIN_SUGGESTION_AREA_M2`] kills
    /// slivers, not rooms. The sub-floor 4.0 m top alone stays filtered.
    #[test]
    fn small_storey_still_yields_the_obvious_suggestion() {
        let cid = ItemId::default();
        let ring = square(0.7728, 0.7387, 1.7728, 1.7387);
        let mut ceilings = CeilingRegions::default_for_footprint(vec![ring.clone()], 2.4, cid);
        ceilings.regions.extend(
            CeilingRegions::default_for_footprint(
                vec![square(1.0978, 1.1137, 1.4478, 1.3637)],
                4.0,
                cid,
            )
            .regions,
        );
        let below = Storey {
            floors: FloorRegions::new(ring, cid),
            ceilings,
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        assert!(has_suggestion_inputs(&below));
        let out = suggest_storey_outlines(&below, 10);
        assert!(!out.is_empty(), "the 1 m² storey suggests");
        assert!(
            (out[0].area_m2 - 1.0).abs() < 1e-4,
            "the full 1 m² square ranks first (got {})",
            out[0].area_m2
        );
        assert!(
            out.iter().all(|s| s.area_m2 >= MIN_SUGGESTION_AREA_M2),
            "the 0.0875 m² top alone stays filtered: {out:?}"
        );
    }

    /// A TRUE sliver below ([`MIN_SUGGESTION_AREA_M2`] is the floor) still
    /// yields no suggestions even though the inputs are present — the
    /// caller's "too small" message path is reachable.
    #[test]
    fn sub_floor_flat_areas_are_still_filtered() {
        let cid = ItemId::default();
        let ring = square(0.0, 0.0, 0.6, 0.6); // 0.36 m² < the 0.5 floor
        let below = Storey {
            floors: FloorRegions::new(ring.clone(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![ring], 2.4, cid),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        assert!(
            has_suggestion_inputs(&below),
            "inputs present — only the area floor filters"
        );
        assert!(suggest_storey_outlines(&below, 10).is_empty());
    }

    #[test]
    fn unset_ceiling_height_yields_empty() {
        let mut below = l_storey();
        below.ceiling_height_m = None;
        assert!(suggest_storey_outlines(&below, 10).is_empty());
    }

    #[test]
    fn missing_footprint_or_flats_yield_empty() {
        // No floors at all.
        let below = Storey {
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        assert!(suggest_storey_outlines(&below, 10).is_empty());
        // Floors but no ceiling regions.
        let below = Storey {
            floors: FloorRegions::new(square(0.0, 0.0, 6.0, 8.0), ItemId::default()),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        assert!(suggest_storey_outlines(&below, 10).is_empty());
    }
}
