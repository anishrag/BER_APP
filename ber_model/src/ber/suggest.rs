//! Upper-storey OUTLINE SUGGESTIONS — given the storey below, propose
//! fully-finalised outline rings the new storey could adopt instead of being
//! drawn. A pure data transform over the below storey's resolved geometry
//! (flat ceiling regions + footprint): the document layer adopts a chosen
//! ring, the controller sequences the flow. Heuristics are the extension
//! point — see [`HEURISTICS`].
//!
//! PIPELINE INVARIANT: a suggestion never covers a SLOPED ceiling region —
//! it's unlikely the new storey will be built over a sloped ceiling, so
//! every candidate ring, from every heuristic, is clipped to the
//! flat-ceiling union before validation/scoring.

use geo::bool_ops::BooleanOps;
use geo::{Area, BoundingRect, Intersects, MultiPolygon, Polygon};

use domain::points::Point2f64;

use super::common::ConstructionPhase;
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
/// `score = W_COVERAGE · coverage`
/// `      + W_COMPACTNESS · area / bbox_area`
/// `      − OUTSIDE_ORIGINAL_PENALTY · outside_original / footprint_area`
/// `      − LEVEL_PENALTY · (levels − 1)`
/// `      − VERTEX_PENALTY · max(verts − 4, 0)`
///
/// `coverage` is phase-aware: when the storey below has ORIGINAL-phase
/// zones, it's the fraction of the Original area the candidate covers
/// (`area(∩ Original) / area(Original)`) and `outside_original` is the
/// candidate area beyond those zones — an upper storey most plausibly sits
/// over the original house, so "comprises the biggest section of the
/// original" outranks sheer size. Without an Original zoning it falls back
/// to whole-footprint coverage (`area / footprint_area`) with no outside
/// penalty. Then prefer compact (bbox-filling) shapes, mildly penalise each
/// inherited floor step beyond the first level, and mildly penalise vertices
/// beyond a rectangle's 4.
const W_COVERAGE: f64 = 2.0;
const W_COMPACTNESS: f64 = 0.5;
const OUTSIDE_ORIGINAL_PENALTY: f64 = 0.25;
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
    /// The same union as one `MultiPolygon` (pre-decomposition) — the
    /// BUILDABLE area every candidate is clipped to before validation (the
    /// sloped-ceiling invariant; see the module doc).
    pub flat_acc: MultiPolygon<f64>,
    /// The below storey's footprint ring (`FloorRegions::footprint_ring`).
    pub footprint: Vec<Point2f64>,
    /// Plan area of `footprint` (m²).
    pub footprint_area: f64,
    /// True when EVERY below-ceiling region is flat (no sloped pieces —
    /// stepped flats still count as flat).
    pub all_flat: bool,
    /// The below storey's construction-PHASE zone rings (open, world metres)
    /// — one per connected component per assigned phase, as handed to
    /// [`suggest_storey_outlines`] (typically `Ber::storey_phase_zones`;
    /// `Unassigned` is already skipped there). Empty when no phase has been
    /// assigned — the phase heuristics then contribute nothing.
    pub phase_zones: Vec<Vec<Point2f64>>,
    /// The plan union of the below storey's ORIGINAL-phase zones and its
    /// area (m²), when it has any (non-sliver): the phase-aware coverage
    /// baseline. `None` falls scoring back to whole-footprint coverage.
    pub original: Option<(MultiPolygon<f64>, f64)>,
}

/// A suggestion heuristic: a label (becomes [`OutlineSuggestion::source`])
/// plus a function from the context to RAW candidate rings.
pub(crate) type OutlineHeuristic = (&'static str, fn(&SuggestionContext) -> Vec<Vec<Point2f64>>);

/// The suggestion heuristics, in the order they run.
///
/// TO ADD A HEURISTIC: write one `fn(&SuggestionContext) ->
/// Vec<Vec<Point2f64>>` returning raw candidate rings (open, world metres)
/// and append one `(label, fn)` entry here — nothing else. The pipeline
/// ([`suggest_storey_outlines`]) clips every ring to the flat-ceiling union
/// (the sloped-ceiling invariant), validates it (≥ 3 vertices, area ≥
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
    ("phase", h7_phase_zones),
    ("phase-flat", h8_phase_flat),
    ("phase-combo", h9_phase_combos),
    ("minus-protrusions", h10_minus_protrusions),
    ("phase-minus-protrusions", h11_phase_minus_protrusions),
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
/// `phase_zones` is the below storey's construction-phase zoning
/// (`Ber::storey_phase_zones` — the `Storey` alone can't resolve a
/// `construction_id` to its phase, that registry lives on `Ber`); it feeds
/// the phase heuristics only, and an empty slice behaves exactly like the
/// pre-phase engine.
///
/// Pipeline: run every [`HEURISTICS`] entry over the [`SuggestionContext`]
/// → CLIP each raw candidate to the flat-ceiling union, keeping the largest
/// simple piece (the sloped-ceiling invariant — a suggestion never covers a
/// sloped ceiling region; a candidate entirely over slopes vanishes) →
/// VALIDATE (collinear-strip, f32-quantize, then require ≥ 3 vertices and
/// area ≥ [`MIN_SUGGESTION_AREA_M2`]) → SCORE (see the weight constants) →
/// SORT by score descending (stable, so earlier heuristics win ties) →
/// DEDUP (symmetric-difference area < [`DEDUP_SYM_DIFF_FRAC`] of the larger
/// — keep the higher-scored) → CAP at `max`.
///
/// Empty when `below` has no default ceiling height yet (its ceilings
/// aren't derived — the controller's gate, defended here too), no usable
/// footprint, or no flat ceiling region.
pub fn suggest_storey_outlines(
    below: &Storey,
    phase_zones: &[(ConstructionPhase, Vec<Point2f64>)],
    max: usize,
) -> Vec<OutlineSuggestion> {
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
    let mut original_acc = MultiPolygon::<f64>(Vec::new());
    for (phase, ring) in phase_zones {
        if *phase == ConstructionPhase::Original && ring.len() >= 3 {
            original_acc = original_acc.union(&MultiPolygon(vec![ring_to_poly(ring)]));
        }
    }
    let original_area = original_acc.unsigned_area();
    let ctx = SuggestionContext {
        flats,
        flat_union,
        flat_acc: acc,
        footprint,
        footprint_area,
        all_flat,
        phase_zones: phase_zones
            .iter()
            .map(|(_, ring)| ring.clone())
            .filter(|ring| ring.len() >= 3)
            .collect(),
        original: (original_area > SLIVER_AREA_M2).then_some((original_acc, original_area)),
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

// ── The heuristics ──────────────────────────────────────────────────────────

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

/// H7 — each construction-PHASE zone of the storey below as its own
/// candidate: "the storey above sits on the original house / on the
/// extension".
fn h7_phase_zones(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    ctx.phase_zones.clone()
}

/// H8 — each phase zone intersected with the flat-ceiling union, each
/// resulting simple piece a candidate: the BUILDABLE part of a phase (the
/// zone minus whatever slopes above it).
fn h8_phase_flat(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    let mut out = Vec::new();
    for zone in &ctx.phase_zones {
        let zone_poly = ring_to_poly(zone);
        for flat in &ctx.flat_union {
            let inter = ring_to_poly(flat).intersection(&zone_poly);
            for piece in simple_pieces(&inter) {
                out.push(open_ring(&piece));
            }
        }
    }
    out
}

/// [`h9_phase_combos`] skips enumeration above this many distinct phase
/// zones (2^n subsets; phases are few in practice, so past this it's noise).
const MAX_COMBO_ZONES: usize = 6;

/// H9 — every subset (size ≥ 2) of the distinct phase zones, unioned:
/// "original + extension 1", "both extensions", …. A subset only emits when
/// its union comes out ONE connected simple piece — a disconnected union is
/// not a drawable storey outline, and its pieces are already the
/// individual-phase candidates (H7).
fn h9_phase_combos(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    let n = ctx.phase_zones.len();
    if !(2..=MAX_COMBO_ZONES).contains(&n) {
        return Vec::new();
    }
    let polys: Vec<Polygon<f64>> = ctx.phase_zones.iter().map(|r| ring_to_poly(r)).collect();
    let mut out = Vec::new();
    for mask in 1u32..(1 << n) {
        if mask.count_ones() < 2 {
            continue;
        }
        let mut acc = MultiPolygon::<f64>(Vec::new());
        for (i, poly) in polys.iter().enumerate() {
            if mask & (1 << i) != 0 {
                acc = acc.union(&MultiPolygon(vec![poly.clone()]));
            }
        }
        if acc.0.len() != 1 {
            continue; // disconnected — not a drawable outline
        }
        let pieces = simple_pieces(&acc);
        if let [piece] = pieces.as_slice() {
            out.push(open_ring(piece));
        }
    }
    out
}

/// [`trim_protrusions`] never trims more than this fraction of the ring's
/// area at one edge line — past it the cut is amputating a wing, not
/// removing a porch.
const PROTRUSION_MAX_FRAC: f64 = 0.25;

/// H10 — the footprint minus small protruding appendages: a porch or bay is
/// single-storey; the main body is what the new storey most plausibly sits
/// on.
fn h10_minus_protrusions(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    trim_protrusions(&ctx.footprint)
}

/// H11 — each construction-PHASE zone minus its small protruding appendages:
/// a zone (typically the Original house) with a small bump-out — the porch
/// is part of the zone but single-storey; the zone's main body is what the
/// upper storey most plausibly sits on.
fn h11_phase_minus_protrusions(ctx: &SuggestionContext) -> Vec<Vec<Point2f64>> {
    ctx.phase_zones
        .iter()
        .flat_map(|z| trim_protrusions(z))
        .collect()
}

/// The H10/H11 core — per-edge protrusion trimming over any OPEN ring. For
/// each edge, clip the ring's polygon to the INNER half-plane of the edge's
/// infinite line (the side holding the majority of the ring's area — the
/// line may pass through the body at a reflex edge); whatever the line
/// shaves off is a protrusion candidate for removal, kept only while the
/// total trim stays within [`PROTRUSION_MAX_FRAC`] of the ring's area.
/// Returns the trimmed main-body candidates, raw (the pipeline validates).
fn trim_protrusions(ring: &[Point2f64]) -> Vec<Vec<Point2f64>> {
    let n = ring.len();
    if n < 3 {
        return Vec::new();
    }
    let body = MultiPolygon(vec![ring_to_poly(ring)]);
    let ring_area = body.unsigned_area();
    if ring_area <= SLIVER_AREA_M2 {
        return Vec::new();
    }
    let Some(bbox) = body.bounding_rect() else {
        return Vec::new();
    };
    // Half-plane stand-in: a rectangle overhanging the edge by L on every
    // side, L = 2× the bbox diagonal, so it covers the ring entirely.
    let l = 2.0 * bbox.width().hypot(bbox.height());
    let mut out = Vec::new();
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = dx.hypot(dy);
        if len <= f64::EPSILON {
            continue; // degenerate edge — no line to clip against
        }
        let (ux, uy) = (dx / len, dy / len);
        // Clip to one side of the edge line: rectangle a−L·u → b+L·u, then
        // L along the normal (nx, ny).
        let clip = |nx: f64, ny: f64| -> MultiPolygon<f64> {
            let p0 = Point2f64::new(a.x - ux * l, a.y - uy * l);
            let p1 = Point2f64::new(b.x + ux * l, b.y + uy * l);
            let p2 = Point2f64::new(p1.x + nx * l, p1.y + ny * l);
            let p3 = Point2f64::new(p0.x + nx * l, p0.y + ny * l);
            body.intersection(&MultiPolygon(vec![ring_to_poly(&[p0, p1, p2, p3])]))
        };
        // The INNER side is whichever half-plane keeps more of the area.
        let (left, right) = (clip(-uy, ux), clip(uy, -ux));
        let clipped = if left.unsigned_area() >= right.unsigned_area() {
            left
        } else {
            right
        };
        if ring_area - clipped.unsigned_area() <= SLIVER_AREA_M2 {
            continue; // a supporting line (any convex edge) — nothing cut
        }
        // The clip can split the body (a U-shape): keep the largest simple
        // piece; the discarded pieces are part of what's being removed, so
        // the trimmed budget is measured against the largest piece alone.
        let Some(largest) = simple_pieces(&clipped).into_iter().max_by(|p, q| {
            p.unsigned_area()
                .partial_cmp(&q.unsigned_area())
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            continue;
        };
        let trimmed = ring_area - largest.unsigned_area();
        if trimmed > PROTRUSION_MAX_FRAC * ring_area {
            continue; // cutting off a wing is not "removing a porch"
        }
        out.push(open_ring(&largest));
    }
    out
}

// ── Pipeline internals ──────────────────────────────────────────────────────

/// Validate one raw candidate ring and score it — `None` when it fails
/// validation (collinear-strip + f32-quantize, then ≥ 3 vertices and area ≥
/// [`MIN_SUGGESTION_AREA_M2`]).
///
/// This is also the SLOPED-CEILING invariant's seam: every raw candidate,
/// from every heuristic, is first clipped to the flat-ceiling union (largest
/// simple piece kept; a candidate entirely over slopes vanishes), so no
/// suggestion can extend over a sloped ceiling region. Clipping along a
/// shared edge can mint collinear/sliver noise — the collinear-strip,
/// quantize and area-floor steps right below absorb it.
fn validate_and_score(
    ctx: &SuggestionContext,
    ring: Vec<Point2f64>,
    source: &'static str,
) -> Option<OutlineSuggestion> {
    // Fast path: when EVERY ceiling region below is flat there is no sloped
    // area to exclude — the clip is a no-op by the invariant's own terms.
    let ring = if ctx.all_flat {
        ring
    } else {
        let clipped = MultiPolygon(vec![ring_to_poly(&ring)]).intersection(&ctx.flat_acc);
        let largest = simple_pieces(&clipped).into_iter().max_by(|p, q| {
            p.unsigned_area()
                .partial_cmp(&q.unsigned_area())
                .unwrap_or(std::cmp::Ordering::Equal)
        })?;
        open_ring(&largest)
    };
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
    // Phase-aware coverage: against the Original zones when the below storey
    // has them (with a penalty for spilling beyond them), else against the
    // whole footprint — see the weight constants.
    let (coverage, outside_original) = match &ctx.original {
        Some((orig, orig_area)) => {
            let inside = MultiPolygon(vec![poly.clone()])
                .intersection(orig)
                .unsigned_area();
            (inside / orig_area, area_m2 - inside)
        }
        None => (area_m2 / ctx.footprint_area, 0.0),
    };
    let score = W_COVERAGE * coverage + W_COMPACTNESS * area_m2 / bbox_area
        - OUTSIDE_ORIGINAL_PENALTY * outside_original / ctx.footprint_area
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
        let out = suggest_storey_outlines(&l_storey(), &[], 10);
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
        // Full L: 2·64/64 + 0.5·64/100 − 0.15·(2−1) − 0.02·(6−4) = 2.13.
        expect(&out[0], 2.13);
        assert_eq!(out[0].source, "all-flat-union");
        assert_eq!(out[0].levels, 2);
        // Bottom arm: 2·40/64 + 0.5 − 0 − 0 = 1.75 (one level, 4 verts).
        expect(&out[1], 1.75);
        assert_eq!(out[1].source, "largest-flat");
        assert_eq!(out[1].levels, 1);
        // Left column rect: 2·40/64 + 0.5 − 0.15 (it spans BOTH levels) = 1.6.
        expect(&out[2], 1.6);
        assert_eq!(out[2].source, "inscribed-rect");
        assert_eq!(out[2].levels, 2);
        // Upper arm: 2·24/64 + 0.5 = 1.25.
        expect(&out[3], 1.25);
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
        let capped = suggest_storey_outlines(&l_storey(), &[], 2);
        assert_eq!(capped.len(), 2);
        let full = suggest_storey_outlines(&l_storey(), &[], 10);
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
        let out = suggest_storey_outlines(&below, &[], 10);
        assert_eq!(out.len(), 1, "{out:?}");
        assert!((out[0].area_m2 - 48.0).abs() < 1e-6);
        assert_eq!(out[0].levels, 1);
        // Full coverage + full compactness, no penalties: 2·1 + 0.5·1 = 2.5.
        assert!((out[0].score - 2.5).abs() < 1e-9);
        // The ring is EXACTLY the footprint's corners — the sloped-ceiling
        // clip is a no-op on an all-flat storey (the fast path).
        let mut got: Vec<(f64, f64)> = out[0].ring.iter().map(|p| (p.x, p.y)).collect();
        got.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(got, vec![(0.0, 0.0), (0.0, 8.0), (6.0, 0.0), (6.0, 8.0)]);
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
        let out = suggest_storey_outlines(&below, &[], 10);
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
        assert!(suggest_storey_outlines(&below, &[], 10).is_empty());
    }

    #[test]
    fn unset_ceiling_height_yields_empty() {
        let mut below = l_storey();
        below.ceiling_height_m = None;
        assert!(suggest_storey_outlines(&below, &[], 10).is_empty());
    }

    #[test]
    fn missing_footprint_or_flats_yield_empty() {
        // No floors at all.
        let below = Storey {
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        assert!(suggest_storey_outlines(&below, &[], 10).is_empty());
        // Floors but no ceiling regions.
        let below = Storey {
            floors: FloorRegions::new(square(0.0, 0.0, 6.0, 8.0), ItemId::default()),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        assert!(suggest_storey_outlines(&below, &[], 10).is_empty());
    }

    /// PHASE-derived candidates: three phase zones A|B|C tiling a 12×4
    /// footprint (built through the REAL partition → `phase_zone_rings`
    /// pipeline), A being the ORIGINAL. Expect each zone as a "phase"
    /// suggestion, each CONNECTED two-zone combo (A∪B, B∪C) as a
    /// "phase-combo", the DISCONNECTED combo (A∪C) absent, the all-zone
    /// combo deduped onto the full-footprint candidate, and the ranking
    /// ORIGINAL-coverage-first: A itself (2·1 + 0.5 = 2.5) beats A∪B
    /// (2.5 − 0.25·16/48 ≈ 2.417) beats the full footprint
    /// (2.5 − 0.25·32/48 ≈ 2.333); the zones/combos covering none of A
    /// (coverage 0) sink to the bottom.
    #[test]
    fn phase_zones_yield_rings_and_connected_combos_ranked_by_coverage() {
        use geo::Contains;
        let ring = square(0.0, 0.0, 12.0, 4.0);
        let cid = ItemId::default();
        let below = Storey {
            floors: FloorRegions::new(ring.clone(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![ring.clone()], 2.4, cid),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        // Partition the footprint by two tagged areas; the remainder (C) gets
        // its own id — then group by phase exactly like `storey_phase_zones`.
        let (cid_a, cid_b, cid_c) = (ItemId::default(), ItemId::default(), ItemId::default());
        let floors = FloorRegions::partition(
            &ring,
            &[
                (square(0.0, 0.0, 4.0, 4.0), cid_a),
                (square(4.0, 0.0, 8.0, 4.0), cid_b),
            ],
            cid_c,
        );
        let zones = floors.phase_zone_rings(|id| {
            if id == cid_a {
                ConstructionPhase::Original
            } else if id == cid_b {
                ConstructionPhase::Extension1
            } else {
                ConstructionPhase::Extension2
            }
        });
        assert_eq!(zones.len(), 3, "{zones:?}");

        let out = suggest_storey_outlines(&below, &zones, 12);
        // Survivors: full footprint (H1; H2/H5/H6 and the A∪B∪C combo dedup
        // onto it), the two connected combos, the three zones.
        assert_eq!(out.len(), 6, "{out:?}");
        // ORIGINAL-coverage-first ranking: the Original zone A (16), then
        // A∪B (32), then the full footprint (48), then the zero-coverage
        // B and C (16 each, compactness only), then B∪C (32, biggest
        // outside-Original penalty).
        let areas: Vec<f64> = out.iter().map(|s| s.area_m2).collect();
        for (got, want) in areas.iter().zip([16.0, 32.0, 48.0, 16.0, 16.0, 32.0]) {
            assert!((got - want).abs() < 1e-6, "areas {areas:?}");
        }
        for pair in out.windows(2) {
            assert!(pair[0].score >= pair[1].score, "{out:?}");
        }
        // #0 IS the Original zone, produced by the "phase" heuristic.
        assert_eq!(out[0].source, "phase");
        assert!((out[0].score - 2.5).abs() < 1e-9, "{:?}", out[0]);
        assert!(out.iter().any(|s| s.source == "all-flat-union"), "{out:?}");
        // Each phase zone appears as its own "phase" suggestion.
        assert_eq!(
            out.iter().filter(|s| s.source == "phase").count(),
            3,
            "{out:?}"
        );
        // Exactly the two CONNECTED combos appear; both contain B's interior
        // (the middle zone) — the disconnected A∪C never emits.
        let combos: Vec<_> = out.iter().filter(|s| s.source == "phase-combo").collect();
        assert_eq!(combos.len(), 2, "{out:?}");
        for c in &combos {
            assert!((c.area_m2 - 32.0).abs() < 1e-6);
            assert!(
                ring_to_poly(&c.ring).contains(&geo::Point::new(6.0, 2.0)),
                "a connected combo spans the middle zone B: {c:?}"
            );
        }
    }

    /// The empty phase slice is byte-identical to the pre-phase engine: same
    /// suggestions as passing zones covering nothing new would NOT be.
    #[test]
    fn empty_phase_slice_changes_nothing() {
        let plain = suggest_storey_outlines(&l_storey(), &[], 10);
        assert_eq!(plain.len(), 4);
        assert!(plain.iter().all(|s| !s.source.starts_with("phase")));
    }

    /// A [`SuggestionContext`] over a bare footprint with one flat ceiling —
    /// enough for the footprint-only heuristics (H10).
    fn ctx_for(footprint: Vec<Point2f64>) -> SuggestionContext {
        let footprint_area = ring_to_poly(&footprint).unsigned_area();
        SuggestionContext {
            flats: vec![(footprint.clone(), 2.4)],
            flat_union: vec![footprint.clone()],
            flat_acc: MultiPolygon(vec![ring_to_poly(&footprint)]),
            footprint,
            footprint_area,
            all_flat: true,
            phase_zones: Vec::new(),
            original: None,
        }
    }

    /// THE LIVE-REPRO for H10: a ~106 m² footprint whose only irregularity
    /// is a ~2 × 1.5 m porch bump past the top edge. It shares the single
    /// flat ceiling and has no phase zoning, so no other heuristic can
    /// propose "the footprint minus the porch" — H10 must.
    fn porch_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(1.4446527, 4.293071),
            Point2f64::new(9.444653, 4.293071),
            Point2f64::new(9.444653, 16.89307),
            Point2f64::new(6.9446526, 16.89307),
            Point2f64::new(6.9446526, 18.79307),
            Point2f64::new(4.9446526, 18.79307),
            Point2f64::new(4.9446526, 17.29307),
            Point2f64::new(1.4446527, 17.29307),
        ]
    }

    #[test]
    fn minus_protrusions_offers_the_footprint_without_the_porch() {
        let cid = ItemId::default();
        let ring = porch_ring();
        let below = Storey {
            floors: FloorRegions::new(ring.clone(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![ring], 2.4, cid),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        let out = suggest_storey_outlines(&below, &[], 10);
        // The body clipped at the y = 17.29307 edge line: the porch's top
        // 2 × 1.5 m goes, the 5.5 × 0.4 m strip stays → ≈ 103 m².
        let body = out
            .iter()
            .find(|s| s.source == "minus-protrusions" && (s.area_m2 - 103.0).abs() < 0.5)
            .unwrap_or_else(|| panic!("expected the ~103 m² minus-porch candidate: {out:?}"));
        assert!(
            body.ring.iter().all(|p| p.y <= 17.3),
            "the porch is gone: {body:?}"
        );
        // The full footprint is still on offer from another source.
        assert!(
            out.iter()
                .any(|s| s.source != "minus-protrusions" && (s.area_m2 - 106.0).abs() < 0.5),
            "{out:?}"
        );
    }

    /// A LARGE protrusion (the L's 24 m² arm — 37.5% of 64 m²) is a wing,
    /// not a porch: every non-supporting edge line of the L trims past
    /// [`PROTRUSION_MAX_FRAC`], so H10 contributes nothing.
    #[test]
    fn minus_protrusions_never_amputates_a_wing() {
        assert!(trim_protrusions(&l_ring()).is_empty());
        assert!(h10_minus_protrusions(&ctx_for(l_ring())).is_empty());
        let out = suggest_storey_outlines(&l_storey(), &[], 10);
        assert!(
            out.iter().all(|s| s.source != "minus-protrusions"),
            "{out:?}"
        );
    }

    /// A plain rectangle has only supporting edge lines — the shared
    /// [`trim_protrusions`] core is silent, so neither H10 (rectangle
    /// footprint) nor H11 (rectangle phase zone) contributes anything.
    #[test]
    fn minus_protrusions_is_silent_on_a_rectangle() {
        assert!(trim_protrusions(&square(0.0, 0.0, 6.0, 8.0)).is_empty());
        assert!(h10_minus_protrusions(&ctx_for(square(0.0, 0.0, 6.0, 8.0))).is_empty());
        let mut ctx = ctx_for(square(0.0, 0.0, 12.0, 4.0));
        ctx.phase_zones = vec![square(0.0, 0.0, 4.0, 4.0), square(4.0, 0.0, 12.0, 4.0)];
        assert!(h11_phase_minus_protrusions(&ctx).is_empty());
    }

    /// THE H11 LIVE-REPRO: the ORIGINAL phase zone is a 5.5 × 8.4 m
    /// rectangle plus a 2.0 × 1.5 m porch bump at its top (49.2 m²), inside
    /// a larger all-flat footprint whose single ceiling does NOT align with
    /// the zone — so no flat-ceiling heuristic can produce "the zone minus
    /// its porch"; only "phase-minus-protrusions" offers the 46.2 m² body.
    #[test]
    fn phase_zone_minus_protrusions_offers_the_zone_body() {
        let cid = ItemId::default();
        let footprint = square(0.0, 0.0, 8.0, 18.0);
        let below = Storey {
            floors: FloorRegions::new(footprint.clone(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![footprint], 2.4, cid),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        let zone = vec![
            Point2f64::new(1.0612326, 14.87429),
            Point2f64::new(1.0612326, 6.474291),
            Point2f64::new(6.5612326, 6.474291),
            Point2f64::new(6.5612326, 16.37429),
            Point2f64::new(4.5612326, 16.37429),
            Point2f64::new(4.5612326, 14.87429),
        ];
        let out = suggest_storey_outlines(&below, &[(ConstructionPhase::Original, zone)], 10);
        let body = out
            .iter()
            .find(|s| s.source == "phase-minus-protrusions" && (s.area_m2 - 46.2).abs() < 0.3)
            .unwrap_or_else(|| panic!("expected the ~46.2 m² zone-minus-porch candidate: {out:?}"));
        assert!(
            body.ring.iter().all(|p| p.y <= 14.9),
            "the bump is gone: {body:?}"
        );
        // The full zone itself is still on offer from the plain "phase"
        // heuristic.
        assert!(
            out.iter()
                .any(|s| s.source == "phase" && (s.area_m2 - 49.2).abs() < 0.3),
            "{out:?}"
        );
    }

    /// THE SLOPED-CEILING INVARIANT: a 10 × 4 footprint with a flat ceiling
    /// over x ∈ [0, 6] and a SLOPED region over x ∈ [6, 10] (vertex z rises
    /// 2.4 → 3.4, so `flat_z()` is `None`), plus an Original phase zone
    /// x ∈ [2, 9] (28 m²) straddling the slope. EVERY suggestion, from every
    /// heuristic, must stay within the flat area, and the phase-zone
    /// candidate comes out TRIMMED to zone ∩ flat (16 m²), never the full
    /// 28 m² zone.
    #[test]
    fn suggestions_never_cover_a_sloped_ceiling_region() {
        use crate::ber::structural::{CeilingExposure, CeilingRegion, Opes, Polygon3D};
        use domain::points::Point3f64;
        let cid = ItemId::default();
        let footprint = square(0.0, 0.0, 10.0, 4.0);
        let mut ceilings =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 6.0, 4.0)], 2.4, cid);
        ceilings.regions.push(CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(6.0, 0.0, 2.4),
                Point3f64::new(10.0, 0.0, 3.4), // rises along x — sloped
                Point3f64::new(10.0, 4.0, 3.4),
                Point3f64::new(6.0, 4.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        });
        let below = Storey {
            floors: FloorRegions::new(footprint, cid),
            ceilings,
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        let zone = square(2.0, 0.0, 9.0, 4.0); // 28 m², 12 of them sloped-over
        let out = suggest_storey_outlines(&below, &[(ConstructionPhase::Original, zone)], 10);
        assert!(!out.is_empty(), "the flat part still suggests");
        // No suggestion covers any of the sloped rectangle, and no ring
        // vertex sits strictly inside it.
        let sloped = ring_to_poly(&square(6.0, 0.0, 10.0, 4.0));
        for s in &out {
            let overlap = MultiPolygon(vec![ring_to_poly(&s.ring)])
                .intersection(&MultiPolygon(vec![sloped.clone()]))
                .unsigned_area();
            assert!(
                overlap <= SLIVER_AREA_M2,
                "suggestion overlaps the sloped area by {overlap}: {s:?}"
            );
            for p in &s.ring {
                assert!(
                    !(p.x > 6.0 && p.x < 10.0 && p.y > 0.0 && p.y < 4.0),
                    "vertex strictly inside the sloped rectangle: {s:?}"
                );
            }
        }
        // The phase candidate was TRIMMED to zone ∩ flat = x ∈ [2, 6] =
        // 16 m² — and nothing offers the full 28 m² zone.
        let phase = out
            .iter()
            .find(|s| s.source == "phase")
            .unwrap_or_else(|| panic!("expected the trimmed phase candidate: {out:?}"));
        assert!((phase.area_m2 - 16.0).abs() < 1e-6, "{phase:?}");
        assert!(
            out.iter().all(|s| (s.area_m2 - 28.0).abs() > 1.0),
            "{out:?}"
        );
    }
}
