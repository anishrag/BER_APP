//! Derived INTER-STOREY classification: the stateless, cross-storey,
//! post-resolve pass that stamps `InterStorey` on every floor / ceiling /
//! wall piece backed by another storey of the SAME dwelling (a storey
//! junction), splitting surfaces where a junction covers only part of them.
//!
//! The derivation is authoritative — no override table, no new durable
//! state. Wall and floor splits are derived-only (never pushed into
//! `floor_splits` / `wall_splits`); ceiling splits become durable via
//! regions-as-truth, which is acceptable because re-splitting an
//! already-carved face is a no-op (`split_faces_coplanar`'s intact
//! guarantee). Stamped exposures persist in the record, but every load
//! re-derives and pass 0 clears stale stamps, so the pass is idempotent AND
//! self-retracting.

use geo::{Contains, MultiPolygon, Point as GeoPoint, Polygon};

use crate::ber::ber_struct::Ber;
use crate::ber::common::ItemId;
use domain::points::{point_to_segment_distance, Point2f64, Point3f64};
use manip_plan::ring_within_ring;

#[allow(unused_imports)]
use super::*;

/// Plan tolerance (m) for "on/inside the neighbour footprint" — matches the
/// conservative containment tolerance of the document layer's horizontal
/// exposure derivation (`document/src/exposure.rs` `CONTAINMENT_TOL_M`).
/// Storeys are sketched separately, so a wall base / region boundary meant to
/// coincide with the neighbour footprint can sit a few cm off it.
pub(crate) const CONTAINMENT_TOL_M: f64 = 0.05;

/// How far INSIDE the neighbour footprint the band-top plan sample is nudged
/// off the wall base line (m). Falls back to a tenth of this when the
/// footprint is too thin at the full nudge.
const SAMPLE_NUDGE_M: f64 = 0.05;

/// Derive `InterStorey` across the whole resolved stack, splitting surfaces
/// where a junction covers only part of them.
///
/// PIPELINE LAW: runs AFTER every per-storey resolve + assessor-map stamp —
/// the cross-storey final stage (per-storey order stays ceiling_height →
/// steps → apexes → walls; this composes on top), so the derivation always
/// wins over an assessor stamp on a junction piece. Idempotent and
/// self-retracting: pass 0 clears every existing `InterStorey` stamp
/// (stale → `Unset`), pass 1 re-splits and re-stamps. World metres; absolute
/// z via [`Ber::storey_floor_datums`]. Neighbour scope is `j = i ± 1`.
///
/// Passes:
/// - **Pass 0 — clear**: every region of every kind with
///   `exposure == InterStorey` → `Unset`.
/// - **Floors** (storey `i`, `i > 0`): split `storey.floors` by the
///   below-storey footprint ring(s) — derived-only, the mechanism class of
///   [`FloorRegions::with_splits`] but keeping unstraddled regions INTACT
///   (same id) — and stamp pieces contained in the below footprint
///   ([`CONTAINMENT_TOL_M`]). Plan containment only.
/// - **Ceilings** (storey `i` with a storey above): split via the
///   fixpoint-stable [`CeilingRegions::split_faces_coplanar`], stamp covered
///   pieces.
/// - **Walls** (perimeter walls AND risers, storey `i` vs `j = i ± 1`):
///   along-base sub-ranges inside the neighbour footprint intersect the
///   neighbour's backing band in absolute z — `[datum_j − slab_j,
///   band-top PROFILE]` where `slab_j` is the neighbour's `floor_thickness_m`
///   and the band top is the neighbour's ceiling surface traced ALONG the
///   wall base as a continuous polyline (each breakpoint interval evaluated
///   on its covering region's plane, [`covering_roof_plane`], sampled just
///   inside the footprint). Non-degenerate overlap → ONE boolean clip of
///   the wall face against EVERY active band run at once
///   ([`clip_wall_by_bands`], SUBTRACT-don't-split): each run's covered
///   piece(s) under its band-top profile become `InterStorey` band regions
///   (bands stay PER-RUN), and the exposed wall is the single remainder —
///   the face MINUS the union of the covers — whose BOTTOM traces the band
///   tops. The wall is NEVER split vertically at a run end or a neighbour
///   ceiling step: one physical wall stays ONE exposed region with a
///   stepped underside; only a band fully crossing the face leaves
///   disconnected remainders (one region per connected component). Pieces
///   are cut only where the exposure CLASS changes.
///
/// GATE (`dimensioned`): a junction between storeys `i` and `j` is derived only
/// when BOTH are `dimensioned[·]` (the controller passes each
/// [`StoreyDoc::measurements_complete`] result). A PROVISIONAL (un-dimensioned)
/// storey's footprint still moves as it is dimensioned, so deriving against it
/// would stamp — and, for ceilings (regions-as-truth), durably SPLIT — the
/// neighbour on geometry that is about to change, leaving stale sliver cuts.
/// A skipped junction resolves `Unset` (un-split) until both sides are
/// dimensioned. Pass 0 still runs for EVERY storey, so a junction that goes
/// provisional is cleaned back rather than frozen. An index past the slice end
/// is treated as provisional (`false`).
///
/// [`StoreyDoc::measurements_complete`]: (document crate)
pub fn derive_inter_storey(ber: &mut Ber, dimensioned: &[bool]) {
    // Junction gate — both storeys must be dimensioned (settled geometry).
    let dim = |i: usize| dimensioned.get(i).copied().unwrap_or(false);

    // ── Pass 0 — clear stale stamps + reverse prior inter-storey ceiling
    // cuts (self-retraction). Runs for every storey regardless of the gate.
    for storey in &mut ber.storeys {
        for r in &mut storey.walls.regions {
            if r.exposure == WallExposure::InterStorey {
                r.exposure = WallExposure::Unset;
            }
        }
        for r in &mut storey.floors.regions {
            if r.exposure == FloorExposure::InterStorey {
                r.exposure = FloorExposure::Unset;
            }
        }
        // Ceilings are regions-as-truth (resolve keeps them verbatim), so a
        // prior inter-storey split would ACCUMULATE across re-derives against a
        // moved neighbour footprint (the stale-sliver bug). Coalesce reverses
        // it — merging each buried piece back into the exposed remainder it was
        // split from, using the still-live `InterStorey` stamp as the signal —
        // BEFORE clearing residual (fully-buried) stamps.
        storey.ceilings = coalesce_inter_storey_ceiling_cuts(std::mem::take(&mut storey.ceilings));
        for r in &mut storey.ceilings.regions {
            if r.exposure == CeilingExposure::InterStorey {
                r.exposure = CeilingExposure::Unset;
            }
            // Normalise away boolean-op SPIKE debris (46 Hazelton Road: a 6 m
            // zero-width detour on the main ceiling piece). Ceilings are
            // regions-as-truth, so a spike a past split baked into the record
            // is durable — stripping here heals it on every re-derive (and is
            // a no-op on clean rings, keeping the pass exactly idempotent).
            strip_spike_vertices(&mut r.polygon.exterior);
        }
    }
    let n = ber.storeys.len();
    if n < 2 {
        return;
    }

    // Cross-storey inputs, snapshotted before any mutation. All are invariant
    // under the splits below: footprint UNIONS don't change when a floor
    // splits, and `split_faces_coplanar` preserves the ceiling surface.
    let datums = ber.storey_floor_datums();
    let footprints: Vec<PlanFootprint> = ber
        .storeys
        .iter()
        .map(|s| PlanFootprint::of(&s.floors))
        .collect();
    let ceilings: Vec<CeilingRegions> = ber.storeys.iter().map(|s| s.ceilings.clone()).collect();
    // The neighbour's FLOORS, for the band BOTTOM: a stepped floor puts the
    // neighbour's underside at a different z over different plan area, exactly
    // as a stepped ceiling does for the band top.
    let floors: Vec<FloorRegions> = ber.storeys.iter().map(|s| s.floors.clone()).collect();
    let slabs: Vec<f64> = ber
        .storeys
        .iter()
        .map(|s| s.floor_thickness_m.unwrap_or(0.0))
        .collect();

    // ── Floors: storey i over the below storey's footprint ────────────────
    for i in 1..n {
        if !(dim(i) && dim(i - 1)) {
            continue; // gate: derive only settled junctions
        }
        let fp = &footprints[i - 1];
        if fp.is_empty() {
            continue;
        }
        let storey = &mut ber.storeys[i];
        storey.floors = split_floors_intact(std::mem::take(&mut storey.floors), &fp.rings);
        let mut buried: Vec<usize> = Vec::new();
        for (ri, r) in storey.floors.regions.iter_mut().enumerate() {
            let ring = floor_plan_ring(r);
            if ring.len() >= 3 && fp.contains_ring(&ring) {
                r.exposure = FloorExposure::InterStorey;
                buried.push(ri);
            }
        }
        // An inter-storey floor is OUT of structure scope: it is never
        // attributed, offered, reviewed or counted — so it must not keep
        // (or share) the FABRIC it was never assessed for (the "final
        // house" stale-share report). But a floor region's construction is
        // ALSO the durable record of the Mapping-phase CONSTRUCTION-PHASE
        // zones (`phase_regions_from_floors` / `construction_phase_complete`
        // read it) — clearing it to nil wiped the upper storey's phase
        // tagging on every materialize and turned the "Construction phase"
        // checklist row red (field report 8a4fcf1e). Reset to the
        // PHASE-ONLY construction instead — fabric dropped, phase kept —
        // the floors mirror of the buried inter-storey wall-band reset
        // below. `ensure` is find-or-create, so the id is stable and the
        // derive stays exactly idempotent.
        for ri in buried {
            let cid = ber.storeys[i].floors.regions[ri].construction_id;
            let phase = ber
                .floor_construction(cid)
                .map(|c| c.base.construction_phase.clone())
                .unwrap_or_default();
            let reset = ber.ensure_floor_construction(
                FloorConstructionType::default(),
                ConstructionMakeup::default(),
                phase,
            );
            ber.storeys[i].floors.regions[ri].construction_id = reset;
        }
    }

    // ── Ceilings: storey i under the above storey's footprint ─────────────
    for i in 0..n - 1 {
        if !(dim(i) && dim(i + 1)) {
            continue; // gate: derive only settled junctions
        }
        let fp = &footprints[i + 1];
        if fp.is_empty() {
            continue;
        }
        let storey = &mut ber.storeys[i];
        storey.ceilings = std::mem::take(&mut storey.ceilings).split_faces_coplanar(&fp.rings);
        for r in &mut storey.ceilings.regions {
            let ring: Vec<Point2f64> = r
                .polygon
                .exterior
                .iter()
                .map(|v| Point2f64::new(v.x, v.y))
                .collect();
            if ring.len() >= 3 && (fp.contains_ring(&ring) || fp.covers_ring_area(&ring)) {
                r.exposure = CeilingExposure::InterStorey;
                // See the floors arm: out-of-scope surfaces carry no
                // construction — clears stale shares for good.
                r.construction_id = ItemId::nil();
            }
        }
    }

    // ── Walls: storey i against each neighbour j = i ± 1 ──────────────────
    for i in 0..n {
        for j in [i.wrapping_sub(1), i + 1] {
            if j >= n {
                continue; // wrapping_sub(1) at i = 0 lands here too
            }
            if !(dim(i) && dim(j)) {
                continue; // gate: derive only settled junctions
            }
            let fp = &footprints[j];
            if fp.is_empty() {
                continue;
            }
            let nb = NeighbourBand {
                fp,
                // Breakpoint probe over the neighbour footprint rings —
                // reuses the wall-base crossing scan of `floor_breakpoints`.
                bp_floors: FloorRegions::from_vertex_lists(fp.rings.clone(), ItemId::default()),
                ceilings: &ceilings[j],
                floors: &floors[j],
                datum: datums[j],
                slab: slabs[j],
                band_lo: datums[j] - slabs[j],
            };
            let walls = std::mem::take(&mut ber.storeys[i].walls.regions);
            let mut out = Vec::with_capacity(walls.len());
            for wall in walls {
                process_wall(wall, &nb, datums[i], &mut out);
            }
            ber.storeys[i].walls.regions = out;
        }
    }

    // ── Buried InterStorey wall bands carry NO fabric ─────────────────────
    // A wall the pass above SPLITS copies the parent's (already-stamped)
    // construction onto BOTH pieces. The buried InterStorey band is out of
    // structure scope — never offered for tagging in Phase 2 — so it must not
    // keep the exposed band's build-up it was never assessed for. Reset it to
    // the default UNASSIGNED construction (keeping only its derived phase, like
    // a natively inter-storey wall). Idempotent, and — running on every
    // materialize — it also heals records an earlier version left stamped.
    let bands: Vec<(usize, usize, ItemId)> = ber
        .storeys
        .iter()
        .enumerate()
        .flat_map(|(si, s)| {
            s.walls
                .regions
                .iter()
                .enumerate()
                .filter(|(_, r)| r.exposure == WallExposure::InterStorey)
                .map(move |(ri, r)| (si, ri, r.construction_id))
        })
        .collect();
    for (si, ri, cid) in bands {
        let phase = ber
            .wall_construction(cid)
            .map(|c| c.base.construction_phase.clone())
            .unwrap_or_default();
        let reset = ber.ensure_wall_construction(
            WallConstructionType::default(),
            ConstructionMakeup::default(),
            phase,
        );
        ber.storeys[si].walls.regions[ri].construction_id = reset;
    }
}

// ── Neighbour footprint (plan) ─────────────────────────────────────────────

/// A neighbour storey's plan footprint: the UNION of its floor regions as
/// simple exterior rings (+ prebuilt `geo` polygons for containment tests).
pub(crate) struct PlanFootprint {
    /// Open rings (no closing duplicate), one per union piece.
    pub(crate) rings: Vec<Vec<Point2f64>>,
    polys: Vec<Polygon<f64>>,
}

impl PlanFootprint {
    pub(crate) fn of(floors: &FloorRegions) -> Self {
        Self::of_many(std::iter::once(floors))
    }

    /// The union footprint of SEVERAL region sets — e.g. floors ∪ porches for
    /// the "inside the dwelling at all" test (a porch is carved out of `floors`
    /// once excluded, but a pin in it is still on the dwelling).
    pub(crate) fn of_many<'a>(region_sets: impl IntoIterator<Item = &'a FloorRegions>) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let mut acc = MultiPolygon::<f64>(Vec::new());
        for r in region_sets.into_iter().flat_map(|f| f.regions.iter()) {
            if r.polygon.exterior().0.len() < 4 {
                continue;
            }
            acc = acc.union(&MultiPolygon(vec![r.polygon.clone()]));
        }
        let mut rings = Vec::new();
        let mut polys = Vec::new();
        for poly in acc.0.iter().flat_map(split_to_simple) {
            if poly.unsigned_area() < SLIVER_AREA_M2 {
                continue;
            }
            let mut ring: Vec<Point2f64> = poly
                .exterior()
                .coords()
                .map(|c| Point2f64::new(c.x, c.y))
                .collect();
            if ring.len() > 1 && ring.first() == ring.last() {
                ring.pop();
            }
            if ring.len() < 3 {
                continue;
            }
            polys.push(ring_to_poly(&ring));
            rings.push(ring);
        }
        Self { rings, polys }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    /// `p` strictly inside any footprint piece (boundary excluded).
    pub(crate) fn contains(&self, p: Point2f64) -> bool {
        let gp = GeoPoint::new(p.x, p.y);
        self.polys.iter().any(|poly| poly.contains(&gp))
    }

    /// `p` inside any piece, or within `tol` of any piece's boundary — the
    /// "on/inside the footprint" test for a wall base sample.
    pub(crate) fn contains_within(&self, p: Point2f64, tol: f64) -> bool {
        if self.contains(p) {
            return true;
        }
        self.rings.iter().any(|ring| {
            let n = ring.len();
            (0..n).any(|i| {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                point_to_segment_distance(p.x, p.y, a.x, a.y, b.x, b.y) <= tol
            })
        })
    }

    /// The plan ring is fully contained in one footprint piece
    /// (conservative full containment, [`CONTAINMENT_TOL_M`]).
    pub(crate) fn contains_ring(&self, ring: &[Point2f64]) -> bool {
        self.rings
            .iter()
            .any(|outer| ring_within_ring(ring, outer, CONTAINMENT_TOL_M))
    }

    /// Area-coverage fallback for [`contains_ring`](Self::contains_ring): the
    /// ring counts as covered when all but a boundary sliver of its AREA lies
    /// inside the footprint (uncovered ≤ perimeter × [`CONTAINMENT_TOL_M`] —
    /// the same width of slack the vertex test grants, expressed as area).
    /// Robust where the strict per-vertex test is brittle: one zero-area
    /// boolean-op artefact poking outside (46 Hazelton Road's ceiling spike)
    /// fails every vertex test while covering nothing real.
    pub(crate) fn covers_ring_area(&self, ring: &[Point2f64]) -> bool {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        if ring.len() < 3 {
            return false;
        }
        let poly = ring_to_poly(ring);
        let area = poly.unsigned_area();
        if area < SLIVER_AREA_M2 {
            return false;
        }
        // Footprint pieces are disjoint (union output), so per-piece
        // intersection areas sum without double-counting.
        let covered: f64 = self
            .polys
            .iter()
            .map(|p| poly.intersection(p).unsigned_area())
            .sum();
        let perimeter: f64 = (0..ring.len())
            .map(|i| {
                let (a, b) = (ring[i], ring[(i + 1) % ring.len()]);
                ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt()
            })
            .sum();
        area - covered <= perimeter * CONTAINMENT_TOL_M
    }
}

// ── Floors ─────────────────────────────────────────────────────────────────

/// Split floor regions by each footprint `ring`, keeping every region the
/// ring does NOT straddle fully INTACT (same id, same polygon) — the
/// idempotence guarantee [`CeilingRegions::split_faces_coplanar`] gives
/// ceilings, applied to the flat 2D floors. Straddled regions split into
/// inside/outside pieces copying `construction_id` / `level_m` / `exposure`
/// (fresh ids). DERIVED-ONLY: the rings are never pushed into
/// `Storey::floor_splits`.
pub(crate) fn split_floors_intact(floors: FloorRegions, rings: &[Vec<Point2f64>]) -> FloorRegions {
    use geo::bool_ops::BooleanOps;
    let mut regions = floors.regions;
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let cut = ring_to_poly(ring);
        let mut next: Vec<FloorRegion> = Vec::new();
        for region in regions {
            // Thickness-aware: keep only NON-degenerate pieces per side, so a
            // reconstructed cut a hair off a shared edge can't shave a strip
            // (field report 797a7ff5). A degenerate-only side means the cut
            // misses / fully covers → keep the region intact (preserving id).
            let inside_pieces: Vec<Polygon<f64>> = region
                .polygon
                .intersection(&cut)
                .0
                .iter()
                .flat_map(split_to_simple)
                .filter(|p| !plan_piece_is_degenerate(p))
                .collect();
            if inside_pieces.is_empty() {
                next.push(region); // cut misses this region — keep it intact
                continue;
            }
            // A cut spanning the region wall-to-wall leaves the remainder as a
            // self-touching ring with a zero-width neck; `split_to_simple`
            // decomposes that pinch into two clean pieces (same fix as the
            // ceiling `split_faces_coplanar`).
            let outside_pieces: Vec<Polygon<f64>> = region
                .polygon
                .difference(&cut)
                .0
                .iter()
                .flat_map(split_to_simple)
                .filter(|p| !plan_piece_is_degenerate(p))
                .collect();
            if outside_pieces.is_empty() {
                next.push(region); // cut fully covers it — keep it intact
                continue;
            }
            let mk = |poly: &Polygon<f64>| FloorRegion {
                id: ItemId::default(),
                polygon: poly.clone(),
                construction_id: region.construction_id,
                level_m: region.level_m,
                exposure: region.exposure,
                height_m: region.height_m,
            };
            for poly in inside_pieces.iter().chain(&outside_pieces) {
                next.push(mk(poly));
            }
        }
        regions = next;
    }
    FloorRegions { regions }
}

/// A floor region's exterior plan ring, open (closing duplicate dropped).
pub(crate) fn floor_plan_ring(r: &FloorRegion) -> Vec<Point2f64> {
    let mut pts: Vec<Point2f64> = r
        .polygon
        .exterior()
        .coords()
        .map(|c| Point2f64::new(c.x, c.y))
        .collect();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    pts
}

// ── Ceilings: reverse a prior inter-storey cut ──────────────────────────────

/// Reverse a previous inter-storey CEILING split so a re-derive against a
/// MOVED neighbour footprint can't ACCUMULATE stale sliver regions (the
/// ceiling-sliver bug). The floor split is derived-only because
/// [`resolve_storey`] rebuilds `storey.floors` every pass; ceilings are
/// regions-as-truth (kept verbatim), so the cut must be reversed HERE instead.
///
/// Coalesces two adjacent FLAT ceiling regions that share a `construction_id`
/// and z-height when EXACTLY ONE carries the derived
/// [`CeilingExposure::InterStorey`] stamp — the boundary between a buried piece
/// and the exposed remainder it was split from. The merged region is rebuilt
/// FLAT at the shared z (no plane-eval, so no z drift) and takes the EXPOSED
/// side's exposure.
///
/// Deliberately conservative, to protect the ROOF and the assessor's own
/// ceiling carves:
///  - only FLAT regions participate, so apex / fan / gable geometry (sloped,
///    boolean-op-fragile) is NEVER unioned — its z is untouched;
///  - a boundary between two NON-`InterStorey` regions (an assessor
///    `ceiling_splits` carve) is never crossed (needs exactly one buried side);
///  - a boundary between two `InterStorey` regions is never crossed either;
///  - a region of a different height or construction is never crossed
///    (a real ceiling step / construction change).
///
/// A fully-buried WHOLE region with no exposed flat sibling is left for the
/// caller's clear pass. Idempotent: with no `InterStorey` stamp it is a no-op.
/// [`resolve_storey`]: (document crate)
fn coalesce_inter_storey_ceiling_cuts(ceilings: CeilingRegions) -> CeilingRegions {
    coalesce_stamped_ceiling_cuts(ceilings, CeilingExposure::InterStorey)
}

/// The stamp-generic core of [`coalesce_inter_storey_ceiling_cuts`]: reverses
/// a prior derived ceiling split whose buried side carries `stamp` — shared
/// with the porch-exclusion derivation, whose cuts carry
/// [`CeilingExposure::Irrelevant`] instead (same accumulate-on-re-derive
/// hazard, same fixpoint contract).
pub(crate) fn coalesce_stamped_ceiling_cuts(
    ceilings: CeilingRegions,
    stamp: CeilingExposure,
) -> CeilingRegions {
    use geo::bool_ops::BooleanOps;
    use geo::Area;
    // NOISE ABSORBER: fuses a BURIED piece with the EXPOSED remainder it was
    // split FROM by the inter-storey stamp — the two carry the SAME flat z by
    // construction (they were one region), so the compare is between
    // exactly-equal values. 1e-4 swallows any rounding while staying ~100×
    // below the smallest REAL ceiling step (steps are cm-scale; a near-zero
    // delta is snapped to an exact same-height divider), so no real step is
    // fused away.
    const Z_EPS_M: f64 = 1e-4;

    let mut regions = ceilings.regions;
    // Repeatedly fuse a buried piece with the coplanar-flat exposed remainder
    // it shares an edge with, until no such pair remains. Region counts are
    // tiny, so the O(n²) rescan per fuse is negligible.
    loop {
        let mut fused = false;
        'scan: for a in 0..regions.len() {
            for b in (a + 1)..regions.len() {
                let (ra, rb) = (&regions[a], &regions[b]);
                // Same construction — OR the buried side carries the NIL id
                // the inter-storey stamp now clears to (the split's own
                // trace): the fuse restores the exposed side's construction
                // either way.
                let nil = ItemId::nil();
                let buried_cid = if ra.exposure == stamp {
                    ra.construction_id
                } else {
                    rb.construction_id
                };
                if ra.construction_id != rb.construction_id && buried_cid != nil {
                    continue;
                }
                // Both flat (never touch a sloped apex/fan face) at one height.
                let (Some(za), Some(zb)) = (ra.flat_z(), rb.flat_z()) else {
                    continue;
                };
                if (za - zb).abs() > Z_EPS_M {
                    continue; // a real ceiling step between them
                }
                let a_is = ra.exposure == stamp;
                let b_is = rb.exposure == stamp;
                if a_is == b_is {
                    continue; // both buried, or neither — not a cut edge to reverse
                }
                // Edge-adjacent iff the union is ONE simple polygon whose area
                // is the sum (disjoint faces union to two pieces → rejected;
                // any overlap/gap → area mismatch → rejected).
                let pa: Polygon<f64> = ra.polygon.clone().into();
                let pb: Polygon<f64> = rb.polygon.clone().into();
                let simple: Vec<Polygon<f64>> =
                    pa.union(&pb).0.iter().flat_map(split_to_simple).collect();
                if simple.len() != 1 {
                    continue;
                }
                if (simple[0].unsigned_area() - (pa.unsigned_area() + pb.unsigned_area())).abs()
                    > SLIVER_AREA_M2
                {
                    continue;
                }
                // Rebuild one FLAT region at the shared z, taking the EXPOSED
                // side's exposure. Replace `a`, drop `b`.
                let exposure = if a_is { rb.exposure } else { ra.exposure };
                // The EXPOSED side's construction survives the fuse (the
                // buried side's is nil-cleared by the stamp).
                let cid = if a_is {
                    rb.construction_id
                } else {
                    ra.construction_id
                };
                let mut pts: Vec<Point3f64> = simple[0]
                    .exterior()
                    .coords()
                    .map(|c| Point3f64::new(c.x, c.y, za))
                    .collect();
                if pts.len() > 1 && pts.first() == pts.last() {
                    pts.pop();
                }
                regions[a] = CeilingRegion {
                    id: ItemId::default(),
                    polygon: Polygon3D::new(pts),
                    opes: Default::default(),
                    construction_id: cid,
                    exposure,
                };
                regions.remove(b);
                fused = true;
                break 'scan;
            }
        }
        if !fused {
            break;
        }
    }
    CeilingRegions { regions }
}

// ── Walls ──────────────────────────────────────────────────────────────────

/// One neighbour storey's backing volume, as the wall pass consumes it.
struct NeighbourBand<'a> {
    fp: &'a PlanFootprint,
    /// The footprint rings wrapped as a `FloorRegions` so the along-base
    /// crossing scan ([`floor_breakpoints`]) can run against them.
    bp_floors: FloorRegions,
    /// The neighbour's resolved ceilings (storey-LOCAL z).
    ceilings: &'a CeilingRegions,
    /// The neighbour's resolved FLOORS, whose `level_m` steps the underside.
    floors: &'a FloorRegions,
    /// The neighbour's floor datum (absolute z).
    datum: f64,
    /// The neighbour's floor-slab thickness.
    slab: f64,
    /// Absolute z of the neighbour volume's underside at its DATUM level:
    /// `datum − slab`. The fallback for [`band_lo_at`](Self::band_lo_at) when
    /// no floor region backs the wall.
    band_lo: f64,
}

impl NeighbourBand<'_> {
    /// The covering PLANE of the neighbour ceiling surface backing the wall
    /// near the wall-base point `m` (`nrm` = the base's unit plan normal), as
    /// `z = c0 + cx·x + cy·y` in ABSOLUTE z: the surface is probed just
    /// inside the neighbour footprint on each side of the base line
    /// ([`covering_roof_plane`]), the HIGHER side (plane z at `m`) winning —
    /// a wall on a step line is backed by the taller neighbour volume beside
    /// it. Evaluating the returned plane AT points of the base line gives the
    /// band-top PROFILE exactly, without the perpendicular-nudge error a
    /// point sample carries on a sloped facet. `None` when no probe lands
    /// inside a ceiling-covered part of the footprint.
    fn band_plane_at(&self, m: Point2f64, nrm: (f64, f64)) -> Option<(f64, f64, f64)> {
        let mut best: Option<(f64, (f64, f64, f64))> = None;
        for s in [1.0, -1.0] {
            let plane = [SAMPLE_NUDGE_M, SAMPLE_NUDGE_M * 0.1].iter().find_map(|d| {
                let p = Point2f64::new(m.x + s * nrm.0 * d, m.y + s * nrm.1 * d);
                if !self.fp.contains(p) {
                    return None;
                }
                covering_roof_plane(self.ceilings, p, nrm).map(|(_, pl)| pl)
            });
            if let Some((c0, cx, cy)) = plane {
                let z = c0 + cx * m.x + cy * m.y;
                if best.as_ref().is_none_or(|&(bz, _)| z > bz) {
                    best = Some((z, (c0, cx, cy)));
                }
            }
        }
        best.map(|(_, (c0, cx, cy))| (c0 + self.datum, cx, cy))
    }

    /// Absolute z of the neighbour volume's UNDERSIDE near the wall-base point
    /// `m` — the mirror of [`band_plane_at`](Self::band_plane_at) for the
    /// BOTTOM. A stepped neighbour floor sits at `datum + level_m − slab`, so
    /// the underside is not one horizontal plane: over a dropped area it can be
    /// most of a storey lower, and a wall it buries there would otherwise be
    /// missed entirely (field: 56 Elmwood Road, the riser beside a dropped
    /// first-floor area reading Exposed).
    ///
    /// Probed just inside the footprint on each side of the base line, the
    /// LOWER side winning — the symmetric counterpart of the top's higher-side
    /// rule, so a wall on a step line takes the FULL band the neighbour buries
    /// it under from either side. Falls back to the datum-level underside when
    /// no floor region backs the wall.
    fn band_lo_at(&self, m: Point2f64, nrm: (f64, f64)) -> f64 {
        let mut best: Option<f64> = None;
        for s in [1.0, -1.0] {
            let lo = [SAMPLE_NUDGE_M, SAMPLE_NUDGE_M * 0.1].iter().find_map(|d| {
                let p = Point2f64::new(m.x + s * nrm.0 * d, m.y + s * nrm.1 * d);
                if !self.fp.contains(p) {
                    return None;
                }
                let gp = GeoPoint::new(p.x, p.y);
                self.floors
                    .regions
                    .iter()
                    .find(|r| r.polygon.contains(&gp))
                    .map(|r| self.datum + r.level_m - self.slab)
            });
            if let Some(z) = lo {
                best = Some(best.map_or(z, |b: f64| b.min(z)));
            }
        }
        best.unwrap_or(self.band_lo)
    }
}

/// Min/max z over a wall ring.
fn z_extent(w: &WallRegion) -> (f64, f64) {
    w.polygon
        .exterior
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v.z), hi.max(v.z))
        })
}

/// One ACTIVE neighbour-band run over a wall: an along-base sub-range
/// `[t0, t1]` (base-parameter space) whose z-band overlap with the wall is
/// real, carrying the band bottom and the band-top PROFILE. The unit
/// [`clip_wall_by_bands`] builds its per-run cover polygons from.
struct ActiveRun {
    t0: f64,
    t1: f64,
    /// Absolute bottom of the neighbour volume, storey-LOCAL z.
    band_lo: f64,
    /// Band-top polyline `(t, z)`, storey-LOCAL z, ascending t.
    profile: Vec<(f64, f64)>,
}

/// Process one wall (of the storey at floor datum `own_datum`, storey-local
/// z) against a neighbour band: find the along-base runs inside the
/// neighbour footprint whose z-band overlap is real, then hand the wall and
/// ALL active runs to [`clip_wall_by_bands`] in one call — covered band
/// piece(s) stamped `InterStorey` per run, one exposed remainder per
/// connected component, bottoms tracing the band tops. A wall the band
/// misses (or one with no usable base frame) passes through UNTOUCHED —
/// same id, same polygon — which is what makes re-derivation exact on walls
/// that are not regenerated every materialize.
fn process_wall(wall: WallRegion, nb: &NeighbourBand, own_datum: f64, out: &mut Vec<WallRegion>) {
    let (Some((_origin, (ux, uy), len, _bz)), Some((a3, b3))) =
        (wall.base_frame(), wall.base_edge())
    else {
        out.push(wall);
        return;
    };
    let (a0, b0) = (Point2f64::new(a3.x, a3.y), Point2f64::new(b3.x, b3.y));
    let nrm = (-uy, ux);
    // Scan the wall's FULL plan extent along its base line, not just its base
    // EDGE. `merge_coplanar_flush` grows a wall's ring while the HOST keeps its
    // short base edge, so a merged wall reaches well past it — and
    // `clip_wall_by_bands`'s "ring outruns the scanned base frame" guard then
    // leaves the whole thing alone. At 56 Elmwood Road the step down to the
    // dropped area merged into a 0.37 m outline stub, so 4.4 m of buried face
    // never got stamped InterStorey.
    let base_len = len;
    let (mut lo, mut hi) = (0.0_f64, len);
    for v in &wall.polygon.exterior {
        let t = (v.x - a0.x) * ux + (v.y - a0.y) * uy;
        lo = lo.min(t);
        hi = hi.max(t);
    }
    let (a, b) = (
        Point2f64::new(a0.x + ux * lo, a0.y + uy * lo),
        Point2f64::new(a0.x + ux * hi, a0.y + uy * hi),
    );
    let len = hi - lo;
    if len <= MIN_SUBWALL_M {
        out.push(wall);
        return;
    }
    let _ = b0;
    let at = |t: f64| Point2f64::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);

    // Along-base breakpoints: neighbour FOOTPRINT crossings AND the
    // neighbour's CEILING-REGION plan boundaries projected onto the base
    // (`profile_breakpoints` — the same scan `regenerate` traces wall tops
    // with). The band top follows the neighbour's ceiling, so a run must
    // never straddle one of its steps: one midpoint sample would smear one
    // section's band top over the other (the neighbour's step line running
    // PERPENDICULAR to the wall — the riser fixture rotated 90°), and a
    // midpoint exactly ON the step line would make the result
    // region-order-dependent via `top_z_at`'s first-region-wins tie-break.
    let mut ts = floor_breakpoints(&nb.bp_floors, a, b);
    ts.extend(profile_breakpoints(nb.ceilings, a, b));
    // The neighbour's FLOOR-region boundaries too: the band BOTTOM follows its
    // underside, so a run must no more straddle one of its floor steps than one
    // of its ceiling steps — a single midpoint sample would smear one level's
    // underside across both.
    ts.extend(floor_breakpoints(nb.floors, a, b));
    ts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));

    // Maximal along-base runs whose midpoint lies on/inside the footprint,
    // each carrying the neighbour's band-top PROFILE over the run: a (t, z)
    // polyline, one linear piece per breakpoint interval, z evaluated at the
    // interval ENDS on the covering ceiling region's plane (region picked at
    // the interval midpoint — always strictly between two neighbour
    // boundaries — by `band_plane_at`'s two-side probe). Contiguous
    // intervals whose profile is CONTINUOUS at the shared breakpoint merge
    // into ONE run — a fanned/sloped neighbour ceiling meeting the wall is
    // one continuous profile (so a facet seam, a coplanar carve or a phase
    // split never splits the wall) — while a REAL neighbour ceiling STEP
    // (discontinuous band top) still starts a new run, keeping the vertical
    // split at the step line.
    struct Run {
        t0: f64,
        t1: f64,
        /// Band-top polyline over `[t0, t1]`: `(t, z)`, ABSOLUTE z,
        /// ascending t.
        profile: Vec<(f64, f64)>,
    }
    let mut runs: Vec<Run> = Vec::new();
    for w in ts.windows(2) {
        let (t0, t1) = (w[0], w[1]);
        if (t1 - t0) * len <= 1e-6 {
            continue; // sub-µm span — boolean-op noise
        }
        let mid = at((t0 + t1) / 2.0);
        if !nb.fp.contains_within(mid, CONTAINMENT_TOL_M) {
            continue;
        }
        let Some((c0, cx, cy)) = nb.band_plane_at(mid, nrm) else {
            continue;
        };
        let (p0, p1) = (at(t0), at(t1));
        let (z0, z1) = (c0 + cx * p0.x + cy * p0.y, c0 + cx * p1.x + cy * p1.y);
        match runs.last_mut() {
            // Contiguous with the previous run (only sub-µm noise spans
            // between them) and CONTINUOUS in z — extend its profile.
            Some(r)
                if (t0 - r.t1) * len <= 1e-6
                    && r.profile
                        .last()
                        .is_some_and(|&(_, ze)| (ze - z0).abs() <= RISER_Z_EPS_M) =>
            {
                r.t1 = t1;
                r.profile.push((t1, z1));
            }
            _ => runs.push(Run {
                t0,
                t1,
                profile: vec![(t0, z0), (t1, z1)],
            }),
        }
    }

    // Keep only runs whose z-band overlap with the wall is real (this is
    // what stops a neighbour footprint edge crossing a wall base from
    // splitting a wall the band never touches).
    let (zmin, zmax) = z_extent(&wall);
    // `top_z_at_along` only reads the top profile over the wall's BASE EDGE; a
    // merged wall reaches past it and samples there come back 0, which would
    // fail the overlap gate below on a wall the band really does bury. Fall
    // back to the ring's own top there — the gate is a coarse "does the band
    // touch this wall at all" test, and `clip_wall_by_bands` cuts the exact
    // geometry regardless.
    let merged = lo < -MIN_SUBWALL_M || hi > base_len + MIN_SUBWALL_M;
    let mut active: Vec<ActiveRun> = Vec::new();
    for r in &runs {
        let (t0, t1) = (r.t0, r.t1);
        if (t1 - t0) * len < MIN_SUBWALL_M {
            continue; // sliver guard along the base
        }
        // Sampled at the run midpoint, like the band top — always strictly
        // between two neighbour boundaries thanks to the breakpoints above.
        let band_lo = nb.band_lo_at(at((t0 + t1) / 2.0), nrm) - own_datum;
        let profile: Vec<(f64, f64)> = r.profile.iter().map(|&(t, z)| (t, z - own_datum)).collect();
        // The run's HIGHEST band top gates the overlap — a profile anywhere
        // above the wall base makes the band real; the profile cut below
        // trims where it is not.
        let band_top = profile
            .iter()
            .fold(f64::NEG_INFINITY, |m, &(_, z)| m.max(z));
        // The wall's top over the run, sampled at quarter points (never a
        // breakpoint) — enough for the overlap gate; the exact geometry is
        // cut by `split_by_line` / the profile clip, not by these samples.
        // `top_z_at_along` is measured in the wall's OWN base frame, whose
        // origin is `exterior[0]`; the scan above runs from `lo` metres before
        // it (the merged reach), so shift the sample back into that frame.
        let ztop_run = if merged {
            zmax
        } else {
            [0.25, 0.5, 0.75]
                .iter()
                .map(|f| wall.top_z_at_along((t0 + (t1 - t0) * f) * len + lo))
                .fold(f64::NEG_INFINITY, f64::max)
        };
        let overlap = band_top.min(ztop_run) - band_lo.max(zmin);
        if overlap <= MIN_SUBWALL_M {
            continue; // degenerate band — the neighbour only grazes this wall
        }
        active.push(ActiveRun {
            t0,
            t1,
            band_lo,
            profile,
        });
    }
    if active.is_empty() {
        out.push(wall);
        return;
    }
    clip_wall_by_bands(wall, &active, (a, (ux, uy), len), out);
}

/// ONE boolean clip of the wall face against EVERY active neighbour-band
/// run at once — the wall pass's SINGLE piece factory (SUBTRACT-don't-split,
/// user ruling 2026-07-25). The wall is UNROLLED onto its plane space (`d` =
/// metres along the base from `frame`'s origin, `z` unchanged — exact for
/// the vertical planar wall) and each run contributes a COVER polygon there:
///
/// - TOP: the run's band-top profile polyline. A flat neighbour ceiling is
///   just the 2-point flat profile, so the old flat and sloped paths are ONE
///   path — a fanned neighbour roof yields a covered piece tracing the
///   slope, never a per-facet staircase.
/// - SIDES: at `t0·len` / `t1·len` — except within [`MIN_SUBWALL_M`] of a
///   wall end, where the side extends 1 m PAST the end (the old `guard_t`
///   logic expressed as cover extension: a run grazing a wall end must not
///   cut there), the profile's end z held flat across the extension.
/// - BOTTOM: at `band_lo` when it strictly bisects the wall (the
///   [`MIN_SUBWALL_M`] guard), else extended below `zmin.min(profile_lo)` −
///   1 m (a merged continuous profile can dip more than 1 m under the wall
///   bottom — a neighbour ceiling falling away along the wall — and a floor
///   above that dip would make the cover ring self-intersect).
///
/// `covered_i = face ∩ cover_i` per run — buried `InterStorey` bands stay
/// PER-RUN, exactly as the old splitter emitted them. The EXPOSED wall is
/// the remainder `face − ⋃ cover_i`: its BOTTOM traces the band tops,
/// STEPPING where the neighbour ceiling steps — the wall is never split
/// vertically at a run end or a step. Only a band fully crossing the face
/// leaves disconnected remainders — one exposed region per connected
/// component ([`split_to_simple`]).
///
/// SLIVER + FIXPOINT guarantees (§5): every piece goes through
/// [`split_to_simple`] + [`band_piece_is_degenerate`]; a run whose covered
/// piece is degenerate is dropped WHOLE (its cover never subtracts from the
/// remainder — a re-derive must not shave a hairline off an already-exposed
/// wall). No surviving cover → the wall passes through INTACT (same id,
/// same ring); no surviving remainder → the wall is stamped `InterStorey`
/// WHOLE (same id, same ring). That identity is what makes a re-derive over
/// previously-cut pieces exact: a band piece re-covers fully (→ stamped
/// whole) and the remainder's covered overlap is a hairline (→ intact), so
/// splits never accumulate.
///
/// A ring extending past the base frame — a floor-band-truncated frame (a
/// steep-profile remainder whose base run rises OUT of the band in the
/// +along direction) or a coplanar-flush merged host whose guest arm
/// continues the line — is left WHOLE: the band beyond the frame is
/// unscanned, and continuing a profile flat there would carve real wall
/// face.
fn clip_wall_by_bands(
    wall: WallRegion,
    runs: &[ActiveRun],
    frame: (Point2f64, (f64, f64), f64),
    out: &mut Vec<WallRegion>,
) {
    use geo::bool_ops::BooleanOps;
    let (origin, (ux, uy), len) = frame;
    if len < 1e-9 || runs.is_empty() {
        out.push(wall);
        return;
    }
    // Unroll the wall ring: plane space doubles as a "plan" for the 2D
    // boolean helpers (x := along-base metres, y := z).
    let ring2d: Vec<Point2f64> = wall
        .polygon
        .exterior
        .iter()
        .map(|v| Point2f64::new((v.x - origin.x) * ux + (v.y - origin.y) * uy, v.z))
        .collect();
    let (dmin, dmax) = ring2d
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
            (lo.min(p.x), hi.max(p.x))
        });
    if dmin < -MIN_SUBWALL_M || dmax > len + MIN_SUBWALL_M {
        out.push(wall); // ring outruns the scanned base frame — leave whole
        return;
    }
    let wall2d = ring_to_poly(&ring2d);
    let (zmin, zmax) = z_extent(&wall);

    // Per-run cover + covered piece(s). A run whose intersection is
    // degenerate only GRAZES: it emits nothing and its cover is discarded.
    let mut covered: Vec<Polygon<f64>> = Vec::new();
    let mut covers: Vec<Polygon<f64>> = Vec::new();
    for run in runs {
        let (Some(&(t_first, z_first)), Some(&(t_last, z_last))) =
            (run.profile.first(), run.profile.last())
        else {
            continue;
        };
        let d_lo = if run.t0 * len < MIN_SUBWALL_M {
            -1.0
        } else {
            run.t0 * len
        };
        let d_hi = if (1.0 - run.t1) * len < MIN_SUBWALL_M {
            len + 1.0
        } else {
            run.t1 * len
        };
        let bottom = if run.band_lo - zmin > MIN_SUBWALL_M && zmax - run.band_lo > MIN_SUBWALL_M {
            run.band_lo
        } else {
            // Band bottom doesn't strictly bisect — no bottom cut; extend the
            // cover floor below the wall AND below the profile's own minimum.
            let profile_lo = run
                .profile
                .iter()
                .fold(f64::INFINITY, |m, &(_, z)| m.min(z));
            zmin.min(profile_lo) - 1.0
        };
        let mut cover: Vec<Point2f64> =
            vec![Point2f64::new(d_lo, bottom), Point2f64::new(d_hi, bottom)];
        if d_hi > t_last * len + 1e-9 {
            cover.push(Point2f64::new(d_hi, z_last));
        }
        for &(t, z) in run.profile.iter().rev() {
            cover.push(Point2f64::new(t * len, z));
        }
        if d_lo < t_first * len - 1e-9 {
            cover.push(Point2f64::new(d_lo, z_first));
        }
        let cover = ring_to_poly(&cover);
        let pieces: Vec<Polygon<f64>> = wall2d
            .intersection(&cover)
            .0
            .iter()
            .flat_map(split_to_simple)
            .filter(|p| !band_piece_is_degenerate(p))
            .collect();
        if pieces.is_empty() {
            continue; // this run only grazes — no real covered piece
        }
        covered.extend(pieces);
        covers.push(cover);
    }
    if covers.is_empty() {
        out.push(wall); // no band engages — intact, byte-identical
        return;
    }
    // remainder = face − ⋃ cover_i (only covers that produced a real piece).
    let mut cover_union = MultiPolygon::<f64>(Vec::new());
    for cover in &covers {
        cover_union = cover_union.union(&MultiPolygon(vec![cover.clone()]));
    }
    let remainder: Vec<Polygon<f64>> = MultiPolygon(vec![wall2d])
        .difference(&cover_union)
        .0
        .iter()
        .flat_map(split_to_simple)
        .filter(|p| !band_piece_is_degenerate(p))
        .collect();
    if remainder.is_empty() {
        // Fully buried — stamp the wall WHOLE (same id, same ring).
        out.push(WallRegion {
            exposure: WallExposure::InterStorey,
            ..wall
        });
        return;
    }
    for (polys, buried) in [(&covered, true), (&remainder, false)] {
        for poly in polys.iter() {
            // §5 sliver invariant: this loop is the wall pass's ONLY piece
            // factory, and both vectors were filtered above.
            debug_assert!(
                !band_piece_is_degenerate(poly),
                "clip_wall_by_bands must never emit a degenerate piece"
            );
            if let Some(piece) = wall_piece_from_plane_poly(poly, origin, (ux, uy), &wall, buried) {
                out.push(piece);
            }
        }
    }
}

/// Whether a wall-plane boolean piece is a degenerate SLIVER the profile cut
/// must never emit: sub-`MIN_SUBWALL_M²` area, or a bounding-box minor
/// dimension under [`MIN_SUBWALL_M`] (a long-thin strip the area guard can
/// miss — e.g. the remainder left where the profile hugs the wall top). The
/// wall-scale sibling of [`plan_piece_is_degenerate`].
fn band_piece_is_degenerate(poly: &Polygon<f64>) -> bool {
    use geo::{Area, BoundingRect};
    if poly.unsigned_area() < MIN_SUBWALL_M * MIN_SUBWALL_M {
        return true;
    }
    match poly.bounding_rect() {
        Some(bb) => bb.width().min(bb.height()) < MIN_SUBWALL_M,
        None => true,
    }
}

/// Rebuild a wall piece from its unrolled plane-space polygon (x = metres
/// along the base from `origin` toward `u`, y = z): closing/consecutive
/// duplicates dropped, winding normalised to CCW in (along, z) — the wall
/// vertex-order contract's winding (base forward, top back) — and the ring
/// ROTATED to start at its leftmost-lowest vertex, so `base_frame()` finds
/// the piece's real base (for the remainder above a sloped covered band
/// that base is the profile polyline itself — a first-class wall that can
/// host openings; starting at the global z-min could land on a short
/// bottom-corner scrap instead). CAVEAT: `base_edge()` restricts the far
/// endpoint to the FLOOR BAND (`MAX_FLOOR_STEP_M` = 0.6 m above the base
/// run's MINIMUM z — so a ring opening at a profile's tall end still spans
/// to the floor-level far corner, see
/// `steep_profile_remainder_keeps_the_full_plan_span`); a remainder whose
/// profile rises out of the band in the +along direction still gets a frame
/// truncated to the in-band run, not the full plan width — the
/// opening-placement span shrinks accordingly. `None` for
/// a degenerate ring. Fresh id, parent's construction, `InterStorey` when
/// `buried`, else the parent's exposure.
fn wall_piece_from_plane_poly(
    poly: &Polygon<f64>,
    origin: Point2f64,
    (ux, uy): (f64, f64),
    parent: &WallRegion,
    buried: bool,
) -> Option<WallRegion> {
    let mut pts: Vec<Point2f64> = poly
        .exterior()
        .coords()
        .map(|c| Point2f64::new(c.x, c.y))
        .collect();
    let near = |a: &Point2f64, b: &Point2f64| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9;
    pts.dedup_by(|b, a| near(a, b));
    while pts.len() > 1 && near(&pts[0], pts.last().unwrap()) {
        pts.pop();
    }
    if pts.len() < 3 {
        return None;
    }
    // CCW in (along, z) == the contract's winding.
    let doubled_area: f64 = (0..pts.len())
        .map(|i| {
            let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
            p.x * q.y - q.x * p.y
        })
        .sum();
    if doubled_area < 0.0 {
        pts.reverse();
    }
    // Start the ring at the leftmost-lowest vertex (min along, then min z).
    let start = (0..pts.len())
        .min_by(|&i, &j| {
            (pts[i].x, pts[i].y)
                .partial_cmp(&(pts[j].x, pts[j].y))
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap_or(0);
    pts.rotate_left(start);
    let exterior: Vec<Point3f64> = pts
        .iter()
        .map(|p| Point3f64::new(origin.x + ux * p.x, origin.y + uy * p.x, p.y))
        .collect();
    let region = WallRegion {
        id: ItemId::default(),
        polygon: Polygon3D::new(exterior),
        exposure: if buried {
            WallExposure::InterStorey
        } else {
            parent.exposure
        },
        opes: Opes::default(),
        construction_id: parent.construction_id,
    };
    // Belt: if the leftmost-lowest start still yields no base frame (an
    // exotic ring), fall back to the generic contract rebase.
    Some(rebase_ring_start(region))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::structural::{Storey, WallRegions};

    /// The "final house" STALE-SHARE self-heal: a ground storey whose whole
    /// flat ceiling shares ONE construction, partly buried under the upper
    /// storey. The derive stamps the buried piece `InterStorey` AND clears
    /// its `construction_id` to the stable NIL id — out-of-scope surfaces
    /// carry no construction, so a legacy in-scope/out-of-scope share can't
    /// survive a re-derive — while the exposed remainder keeps the
    /// construction. Idempotent: a second derive reproduces the exact state.
    #[test]
    fn derive_clears_construction_on_interstorey_stamp_and_heals_shares() {
        let mut ber = Ber::default();
        let ground = flat_storey(square(0.0, 0.0, 12.0, 12.0), 2.4, None);
        let shared_cid = ground.ceilings.regions[0].construction_id;
        ber.storeys.push(ground);
        ber.storeys
            .push(flat_storey(square(6.0, 6.0, 12.0, 12.0), 2.4, Some(0.3)));
        derive_all(&mut ber);
        let nil = ItemId::nil();
        let s0 = &ber.storeys[0];
        let buried: Vec<&CeilingRegion> = s0
            .ceilings
            .regions
            .iter()
            .filter(|r| r.exposure == CeilingExposure::InterStorey)
            .collect();
        assert!(!buried.is_empty(), "the covered piece derives InterStorey");
        for r in &buried {
            assert_eq!(
                r.construction_id, nil,
                "a buried piece carries NO construction"
            );
        }
        let exposed: Vec<&CeilingRegion> = s0
            .ceilings
            .regions
            .iter()
            .filter(|r| r.exposure != CeilingExposure::InterStorey)
            .collect();
        assert!(!exposed.is_empty());
        for r in &exposed {
            assert_eq!(
                r.construction_id, shared_cid,
                "the exposed remainder keeps its construction"
            );
        }
        // Idempotent — the coalesce/re-split/clear cycle reproduces itself.
        let before = ceiling_descriptor(&ber.storeys[0].ceilings);
        let cids_before: Vec<ItemId> = ber.storeys[0]
            .ceilings
            .regions
            .iter()
            .map(|r| r.construction_id)
            .collect();
        derive_all(&mut ber);
        assert_eq!(ceiling_descriptor(&ber.storeys[0].ceilings), before);
        let cids_after: Vec<ItemId> = ber.storeys[0]
            .ceilings
            .regions
            .iter()
            .map(|r| r.construction_id)
            .collect();
        let mut a: Vec<String> = cids_before.iter().map(|c| format!("{:?}", c)).collect();
        let mut b: Vec<String> = cids_after.iter().map(|c| format!("{:?}", c)).collect();
        a.sort();
        b.sort();
        assert_eq!(a, b, "construction links are re-derive stable");
    }

    /// Field report 8a4fcf1e ("construction phase went red on the second
    /// storey"): a buried inter-storey FLOOR must keep its construction
    /// PHASE. The floor partition is the durable record of the Mapping-phase
    /// zones (`storey_phase_zones` / the checklist's
    /// `construction_phase_complete` read the floor constructions' phases) —
    /// nil-clearing the buried regions wiped the upper storey's tagging on
    /// every materialize. The derive must drop the FABRIC (out-of-scope
    /// hygiene) but re-home the region onto the phase-only construction,
    /// mirroring the buried wall-band reset. Idempotent across re-derives.
    #[test]
    fn buried_inter_storey_floor_keeps_its_construction_phase() {
        use crate::ber::common::ConstructionPhase;
        use crate::ber::structural::{ConstructionMakeup, FloorConstructionType};
        let mut ber = Ber::default();
        let ring = square(0.0, 0.0, 8.0, 4.0);
        ber.storeys.push(flat_storey(ring.clone(), 2.4, None));
        ber.storeys.push(flat_storey(ring, 2.4, Some(0.3)));
        // The upper floor was phase-tagged Original WITH real fabric (the
        // Mapping partition + a Structure-phase attribution).
        let tagged = ber.ensure_floor_construction_named(
            FloorConstructionType::SolidFloor,
            ConstructionMakeup::Default,
            ConstructionPhase::Original,
            Some("old floor"),
        );
        ber.storeys[1].floors.regions[0].construction_id = tagged;

        derive_all(&mut ber);

        let r = &ber.storeys[1].floors.regions[0];
        assert_eq!(r.exposure, FloorExposure::InterStorey, "buried → stamped");
        assert_ne!(r.construction_id, ItemId::nil(), "phase record survives");
        let c = ber
            .floor_construction(r.construction_id)
            .expect("buried floor still references a construction");
        assert_eq!(
            c.base.construction_phase,
            ConstructionPhase::Original,
            "the construction PHASE is kept"
        );
        assert_eq!(
            c.construction_type,
            FloorConstructionType::default(),
            "the FABRIC is dropped (out-of-scope hygiene)"
        );
        // Zone reconstruction still sees the phase (what the session rebuild
        // and the checklist read).
        let zones = ber.storey_phase_zones(1);
        assert_eq!(zones.len(), 1, "the Original zone survives the derive");
        assert_eq!(zones[0].0, ConstructionPhase::Original);
        // Idempotent: a second derive reproduces the exact construction link.
        let cid = r.construction_id;
        derive_all(&mut ber);
        assert_eq!(
            ber.storeys[1].floors.regions[0].construction_id, cid,
            "re-derive is exactly stable"
        );
    }

    /// Run the derivation treating EVERY storey as dimensioned — the settled
    /// geometry these fixtures build directly. (The gate itself is exercised by
    /// the controller tests, which own the `measurements_complete` signal.)
    fn derive_all(ber: &mut Ber) {
        let mask = vec![true; ber.storeys.len()];
        derive_inter_storey(ber, &mask);
    }

    /// Region COUNT + per-region (zmin, zmax) of a ceiling, µm-quantized and
    /// sorted — the region-order-independent fixpoint key (a new sliver changes
    /// the count; a flattened/lost apex changes a z bound).
    fn ceiling_descriptor(cs: &CeilingRegions) -> Vec<(i64, i64)> {
        let q = |v: f64| (v * 1e6).round() as i64;
        let mut d: Vec<(i64, i64)> = cs
            .regions
            .iter()
            .map(|r| {
                let (zmin, zmax) = r
                    .polygon
                    .exterior
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| {
                        (a.min(v.z), b.max(v.z))
                    });
                (q(zmin), q(zmax))
            })
            .collect();
        d.sort();
        d
    }

    fn ceiling_x_extent(r: &CeilingRegion) -> (f64, f64) {
        r.polygon
            .exterior
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), v| {
                (a.min(v.x), b.max(v.x))
            })
    }

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(x0, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0, y1),
        ]
    }

    fn perimeter(ring: &[Point2f64]) -> Vec<(Point2f64, Point2f64, WallExposure)> {
        (0..ring.len())
            .map(|i| (ring[i], ring[(i + 1) % ring.len()], WallExposure::Exposed))
            .collect()
    }

    /// A resolved storey over `ring`: flat ceiling at `height`, one floor
    /// region, perimeter walls (all `Exposed`) + any ceiling-riser faces
    /// (`Unset`), storey-local z.
    fn flat_storey(ring: Vec<Point2f64>, height: f64, slab: Option<f64>) -> Storey {
        stepped_storey(ring, height, slab, &[])
    }

    /// Like [`flat_storey`] but with ceiling-step `areas` (`(ring, z delta)`)
    /// composed onto the default height, plus the derived step risers.
    fn stepped_storey(
        ring: Vec<Point2f64>,
        height: f64,
        slab: Option<f64>,
        areas: &[(Vec<Point2f64>, f64)],
    ) -> Storey {
        let cid = ItemId::default();
        let floors = FloorRegions::new(ring.clone(), cid);
        let ceilings = CeilingRegions::default_for_footprint(vec![ring.clone()], height, cid)
            .with_z_deltas(areas);
        let riser_faces: Vec<(Polygon3D, WallExposure)> = ceilings
            .riser_faces()
            .into_iter()
            .map(|p| (p, WallExposure::Unset))
            .collect();
        let walls = WallRegions::regenerate(perimeter(&ring), &ceilings, &floors, 0.0, height, cid)
            .with_risers(riser_faces, cid);
        Storey {
            floors,
            walls,
            ceilings,
            ceiling_height_m: Some(height),
            floor_thickness_m: slab,
            ..Storey::default()
        }
    }

    /// The USER'S RISER FIXTURE (spec §6 test 3): a ground storey with a
    /// ceiling step (low 2.4 m over x∈[0,4], tall 4.0 m over x∈[4,8], riser
    /// wall on the step line x=4) and a first floor over the LOW section
    /// (slab 0.3 m → datum 2.7 m, ceiling 2.4 m).
    fn riser_fixture() -> Ber {
        let ground = stepped_storey(
            square(0.0, 0.0, 8.0, 4.0),
            2.4,
            None,
            &[(square(4.0, 0.0, 8.0, 4.0), 1.6)],
        );
        let first = flat_storey(square(0.0, 0.0, 4.0, 4.0), 2.4, Some(0.3));
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];
        ber
    }

    /// Walls whose whole ring lies on the plan line x = `x` (ring-based, not
    /// `base_edge` — a horizontal-cut remainder's base-edge convention can
    /// degenerate while its ring is still plainly on the wall plane).
    fn walls_on_x(storey: &Storey, x: f64) -> Vec<&WallRegion> {
        storey
            .walls
            .regions
            .iter()
            .filter(|w| w.polygon.exterior.iter().all(|v| (v.x - x).abs() < 1e-6))
            .collect()
    }

    /// Walls whose whole ring lies on the plan line y = `y` (see
    /// [`walls_on_x`] for why this is ring-based).
    fn walls_on_y(storey: &Storey, y: f64) -> Vec<&WallRegion> {
        storey
            .walls
            .regions
            .iter()
            .filter(|w| w.polygon.exterior.iter().all(|v| (v.y - y).abs() < 1e-6))
            .collect()
    }

    /// The riser fixture ROTATED 90°: the neighbour's ceiling step line runs
    /// PERPENDICULAR to the wall under test. Ground storey 8×4 with its
    /// ceiling at 2.4 m over the back strip y∈[2,4] (the anchor the first
    /// floor sits on) and TWO raised sections over the front strip y∈[0,2] —
    /// 3.2 m for x∈[0,4], 4.0 m for x∈[4,8] (step line x=4). First floor over
    /// the back strip (slab 0.3 → datum 2.7, ceiling 2.4): its y=2 wall faces
    /// the stepped ground ceiling along its LENGTH, and the wall's midpoint
    /// (x=4) lies exactly ON the neighbour's step line — the symmetric case.
    fn rotated_step_fixture() -> Ber {
        let ground = stepped_storey(
            square(0.0, 0.0, 8.0, 4.0),
            2.4,
            None,
            &[
                (square(0.0, 0.0, 4.0, 2.0), 0.8),
                (square(4.0, 0.0, 8.0, 2.0), 1.6),
            ],
        );
        let first = flat_storey(square(0.0, 2.0, 8.0, 4.0), 2.4, Some(0.3));
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];
        ber
    }

    /// Plan x-extent of a wall's ring. (NOT `base_edge`: the upper remainder
    /// of a horizontal cut opens its ring on the cut line, so its base-edge
    /// convention degenerates to a plan point — the ring itself is the
    /// reliable extent.)
    fn x_extent(w: &WallRegion) -> (f64, f64) {
        w.polygon
            .exterior
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                (lo.min(v.x), hi.max(v.x))
            })
    }

    /// Canonical descriptors of a storey's walls on the plan line y = `y`:
    /// `(x0, x1, zmin, zmax, exposure)` per piece, µm-quantized and sorted —
    /// the region-order-independent comparison key.
    fn y_wall_descriptors(storey: &Storey, y: f64) -> Vec<(i64, i64, i64, i64, String)> {
        let q = |v: f64| (v * 1e6).round() as i64;
        let mut out: Vec<_> = walls_on_y(storey, y)
            .iter()
            .map(|w| {
                let (zmin, zmax) = z_extent(w);
                let (x0, x1) = x_extent(w);
                (q(x0), q(x1), q(zmin), q(zmax), format!("{:?}", w.exposure))
            })
            .collect();
        out.sort();
        out
    }

    // ── §6 test 1: clean two-storey stack ────────────────────────────────

    #[test]
    fn clean_stack_stamps_upper_floor_and_lower_ceiling_walls_untouched() {
        let ring = square(0.0, 0.0, 8.0, 4.0);
        let mut ber = Ber::default();
        ber.storeys = vec![
            flat_storey(ring.clone(), 2.4, None),
            flat_storey(ring, 2.4, Some(0.3)),
        ];
        let walls_before: Vec<String> = ber
            .storeys
            .iter()
            .map(|s| serde_json::to_string(&s.walls).unwrap())
            .collect();

        derive_all(&mut ber);

        // Upper floor: fully over the storey below → InterStorey.
        assert_eq!(ber.storeys[1].floors.regions.len(), 1);
        assert!(ber.storeys[1]
            .floors
            .regions
            .iter()
            .all(|r| r.exposure == FloorExposure::InterStorey));
        // Lower storey's own floor untouched.
        assert!(ber.storeys[0]
            .floors
            .regions
            .iter()
            .all(|r| r.exposure == FloorExposure::Unset));
        // Lower ceiling: fully under the storey above → InterStorey, INTACT
        // (no split — the footprints coincide).
        assert_eq!(ber.storeys[0].ceilings.regions.len(), 1);
        assert!(ber.storeys[0]
            .ceilings
            .regions
            .iter()
            .all(|r| r.exposure == CeilingExposure::InterStorey));
        // Top ceiling left for the assessor.
        assert!(ber.storeys[1]
            .ceilings
            .regions
            .iter()
            .all(|r| r.exposure == CeilingExposure::Unset));
        // Walls: byte-identical — the neighbour volumes only graze them in z.
        let walls_after: Vec<String> = ber
            .storeys
            .iter()
            .map(|s| serde_json::to_string(&s.walls).unwrap())
            .collect();
        assert_eq!(walls_before, walls_after, "walls must be untouched");
    }

    // ── §6 test 2: partial plan overlap splits the lower ceiling ─────────

    #[test]
    fn partial_overlap_splits_lower_ceiling_into_inter_storey_and_remainder() {
        let mut ber = Ber::default();
        ber.storeys = vec![
            flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, None),
            flat_storey(square(0.0, 0.0, 4.0, 4.0), 2.4, Some(0.3)),
        ];

        derive_all(&mut ber);

        // The single lower ceiling splits into the covered piece + remainder.
        let regions = &ber.storeys[0].ceilings.regions;
        assert_eq!(regions.len(), 2, "one ceiling → covered piece + remainder");
        for r in regions {
            let c = r.centroid();
            if c.x < 4.0 {
                assert_eq!(r.exposure, CeilingExposure::InterStorey);
            } else {
                assert_eq!(r.exposure, CeilingExposure::Unset);
            }
        }
        // Both pieces keep the flat 2.4 surface (coplanar split).
        assert!(regions.iter().all(|r| r.flat_z() == Some(2.4)));
        // Upper floor fully over the below storey → InterStorey.
        assert!(ber.storeys[1]
            .floors
            .regions
            .iter()
            .all(|r| r.exposure == FloorExposure::InterStorey));
    }

    /// THE STEPPED-NEIGHBOUR BAND BOTTOM (field: 56 Elmwood Road — the riser
    /// beside a dropped first-floor area reading Exposed).
    ///
    /// Ground storey: low ceiling 1.958 over x ∈ [0,3], tall 2.548 over
    /// x ∈ [3,9], with a riser wall on x = 3. First floor covers the WHOLE
    /// plan, its floor STEPPED down 0.59 over the low half so it rests on both
    /// ceilings (datum 2.548, slab 0.25 → underside 2.548 over the tall half,
    /// 1.958 over the low one).
    ///
    /// The ground riser spans 1.958..2.548 — entirely BELOW `datum − slab`, so
    /// a single flat band bottom misses it completely. It is buried by the
    /// first floor's dropped slab and must read InterStorey.
    #[test]
    fn a_stepped_neighbour_floor_buries_the_riser_below_its_datum() {
        let ground = stepped_storey(
            square(0.0, 0.0, 9.0, 4.0),
            2.548,
            None,
            &[(square(0.0, 0.0, 3.0, 4.0), -0.59)],
        );
        let mut first = flat_storey(square(0.0, 0.0, 9.0, 4.0), 2.0, Some(0.25));
        // Step the first floor's own slab down over the low half.
        first.floors = FloorRegions::from_vertex_lists(
            vec![square(0.0, 0.0, 3.0, 4.0), square(3.0, 0.0, 9.0, 4.0)],
            ItemId::default(),
        );
        first.floors.regions[0].level_m = -0.59;
        first.floor_datum_z_m = Some(2.548);
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];
        assert!(
            (ber.storey_floor_datums()[1] - 2.798).abs() < 1e-9,
            "datum 2.548 + 0.25: {:?}",
            ber.storey_floor_datums()
        );
        derive_inter_storey(&mut ber, &[true, true]);

        let risers = walls_on_x(&ber.storeys[0], 3.0);
        assert_eq!(risers.len(), 1, "the riser stays whole: {}", risers.len());
        let (lo, hi) = z_extent(risers[0]);
        assert!(
            (lo - 1.958).abs() < 1e-6 && (hi - 2.548).abs() < 1e-6,
            "riser spans the ceiling step: {lo}..{hi}"
        );
        assert_eq!(
            risers[0].exposure,
            WallExposure::InterStorey,
            "buried by the neighbour's DROPPED slab, which sits below its datum \
             underside — the band bottom has to follow the neighbour floor"
        );
    }

    // ── §6 test 3: the user's riser fixture ──────────────────────────────

    #[test]
    fn riser_fixture_splits_step_line_wall_and_buries_the_step_riser() {
        let mut ber = riser_fixture();
        let datums = ber.storey_floor_datums();
        assert!(
            datums.len() == 2 && datums[0] == 0.0 && (datums[1] - 2.7).abs() < 1e-9,
            "want datums ~[0, 2.7], got {datums:?}"
        );
        assert_eq!(
            walls_on_x(&ber.storeys[0], 4.0).len(),
            1,
            "ground storey has its step riser on x=4"
        );
        assert_eq!(ber.storeys[1].walls.regions.len(), 4);

        derive_all(&mut ber);

        // First-floor wall on the step line splits into the buried band
        // (top at the TALL ground-roof z: 4.0 abs = 1.3 storey-local) and the
        // remainder keeping its prior exposure.
        let step_walls = walls_on_x(&ber.storeys[1], 4.0);
        assert_eq!(step_walls.len(), 2, "step-line wall split into two bands");
        let (mut lower, mut upper) = (None, None);
        for w in &step_walls {
            let (zmin, zmax) = z_extent(w);
            // RE-PIN (subtract-don't-split): tolerance 1e-9 → 1e-6. The
            // pieces are now geo boolean-clip output (`clip_wall_by_bands`),
            // whose grid snapping drifts a few 1e-9 m — the same tolerance
            // class every sloped-profile pin already uses. Same two pieces,
            // same z bounds: a full-base flat band still yields band +
            // remainder (the horizontal cut is an exposure-CLASS change).
            if zmax < 1.3 + 1e-6 {
                assert!((zmin - 0.0).abs() < 1e-6 && (zmax - 1.3).abs() < 1e-6);
                lower = Some(w);
            } else {
                assert!((zmin - 1.3).abs() < 1e-6 && (zmax - 2.4).abs() < 1e-6);
                upper = Some(w);
            }
        }
        assert_eq!(
            lower.expect("lower band").exposure,
            WallExposure::InterStorey,
            "buried band → InterStorey"
        );
        assert_eq!(
            upper.expect("upper band").exposure,
            WallExposure::Exposed,
            "remainder keeps its prior exposure"
        );
        // The first floor's other perimeter walls are untouched.
        assert_eq!(ber.storeys[1].walls.regions.len(), 5);
        for w in &ber.storeys[1].walls.regions {
            if w.base_edge()
                .is_some_and(|(a, b)| (a.x - 4.0).abs() > 1e-6 || (b.x - 4.0).abs() > 1e-6)
            {
                assert_eq!(w.exposure, WallExposure::Exposed);
            }
        }
        // The ground storey's step riser is fully inside the first floor's
        // backing band [2.4, 5.1] → stamped whole, no split.
        let risers = walls_on_x(&ber.storeys[0], 4.0);
        assert_eq!(risers.len(), 1, "riser stays whole");
        let (rmin, rmax) = z_extent(risers[0]);
        assert!((rmin - 2.4).abs() < 1e-9 && (rmax - 4.0).abs() < 1e-9);
        assert_eq!(risers[0].exposure, WallExposure::InterStorey);
        // Ground perimeter walls untouched.
        for w in &ber.storeys[0].walls.regions {
            if w.id != risers[0].id {
                assert_eq!(w.exposure, WallExposure::Exposed);
            }
        }
        // Horizontal surfaces: low ceiling section buried, tall one not.
        for r in &ber.storeys[0].ceilings.regions {
            match r.flat_z() {
                Some(z) if (z - 2.4).abs() < 1e-9 => {
                    assert_eq!(r.exposure, CeilingExposure::InterStorey)
                }
                Some(z) if (z - 4.0).abs() < 1e-9 => {
                    assert_eq!(r.exposure, CeilingExposure::Unset)
                }
                other => panic!("unexpected ceiling z {other:?}"),
            }
        }
        assert!(ber.storeys[1]
            .floors
            .regions
            .iter()
            .all(|r| r.exposure == FloorExposure::InterStorey));
    }

    /// The clip's UPPER piece (the exposed remainder above the buried band)
    /// is a FIRST-CLASS wall: `wall_piece_from_plane_poly` rebuilds its ring
    /// onto the vertex-order contract, so `base_frame()` yields the full
    /// plan span at the band-top height — it can host openings, recover its
    /// painted span, and clamp sills at the real wall top.
    #[test]
    fn riser_fixture_remainder_piece_has_a_real_base_frame_at_the_cut() {
        let mut ber = riser_fixture();
        derive_all(&mut ber);
        let step_walls = walls_on_x(&ber.storeys[1], 4.0);
        assert_eq!(step_walls.len(), 2);
        let remainder = step_walls
            .iter()
            .find(|w| w.exposure != WallExposure::InterStorey)
            .expect("the exposed remainder");
        let (origin, _, len, base_z) = remainder
            .base_frame()
            .expect("the remainder is a first-class wall (real base frame)");
        // RE-PIN (subtract-don't-split): tolerance 1e-9 → 1e-6 — the piece
        // is boolean-clip output now (see the note in the fixture test
        // above). What it still proves: the remainder above the buried band
        // is a FIRST-CLASS wall — full-span base frame at the cut height.
        assert!(
            (base_z - 1.3).abs() < 1e-6,
            "base sits at the cut height (storey-local 1.3), got {base_z}"
        );
        assert!((len - 4.0).abs() < 1e-6, "full plan span, got {len}");
        assert!((origin.x - 4.0).abs() < 1e-6);
        // And the sill clamp now reaches the real wall top, not the band top.
        let max_sill = remainder.max_sill_for(2.0, 0.5);
        assert!(
            (max_sill - (2.4 - 1.3 - 0.5)).abs() < 1e-6,
            "sill clamps under the wall top, got {max_sill}"
        );
    }

    /// r05 field-report follow-up: the buried InterStorey band must not INHERIT
    /// the fabric of the wall it was split from. Assign a real construction to
    /// the step-line wall, derive, and the lower (InterStorey) band resets to
    /// the default/unassigned construction while the exposed remainder keeps it
    /// — so the out-of-scope band reads "not applicable", never carrying a
    /// build-up it was never assessed for.
    #[test]
    fn buried_inter_storey_wall_band_drops_the_exposed_fabric() {
        use crate::ber::structural::{ConstructionMakeup, WallConstructionType};
        let mut ber = riser_fixture();

        // Tag the whole (pre-split) step-line wall with a real construction.
        let cid = ber.ensure_wall_construction(
            WallConstructionType::ConcreteHollowBlocks,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        {
            let pre = walls_on_x(&ber.storeys[1], 4.0);
            assert_eq!(pre.len(), 1, "one step-line wall before the split");
            let id = pre[0].id;
            ber.storeys[1]
                .walls
                .regions
                .iter_mut()
                .find(|w| w.id == id)
                .unwrap()
                .construction_id = cid;
        }

        derive_all(&mut ber);

        let bands = walls_on_x(&ber.storeys[1], 4.0);
        assert_eq!(bands.len(), 2, "split into buried band + exposed remainder");
        for w in &bands {
            let ty = ber
                .wall_construction(w.construction_id)
                .map(|c| c.construction_type)
                .unwrap_or_default();
            if w.exposure == WallExposure::InterStorey {
                assert_eq!(
                    ty,
                    WallConstructionType::default(),
                    "buried band drops the exposed fabric (reset to unassigned)"
                );
            } else {
                assert_eq!(
                    ty,
                    WallConstructionType::ConcreteHollowBlocks,
                    "the exposed remainder keeps the assigned construction"
                );
            }
        }
    }

    // ── §6 test 4: exact idempotence, incl. a serde round-trip ───────────

    #[test]
    fn derive_is_exactly_idempotent_including_serde_round_trip() {
        let mut ber = riser_fixture();
        derive_all(&mut ber);
        let once = serde_json::to_string(&ber.storeys).unwrap();

        // Second derive on the same value: identical region set — count,
        // geometry, ids, exposures (FULL equality via the serialized form).
        derive_all(&mut ber);
        let twice = serde_json::to_string(&ber.storeys).unwrap();
        assert_eq!(once, twice, "derive must be exactly idempotent");

        // And after a serde round-trip (the load path: stamped InterStorey
        // persists, the re-derive clears + re-stamps to the same fixpoint).
        let mut reloaded: Ber =
            serde_json::from_str(&serde_json::to_string(&ber).unwrap()).unwrap();
        derive_all(&mut reloaded);
        let after_reload = serde_json::to_string(&reloaded.storeys).unwrap();
        assert_eq!(once, after_reload, "reload + re-derive must be a fixpoint");
    }

    // ── §6 test 5: self-retraction ───────────────────────────────────────

    #[test]
    fn removing_the_storey_above_retracts_every_stamp() {
        let mut ber = riser_fixture();
        derive_all(&mut ber);
        assert!(ber.storeys[0]
            .ceilings
            .regions
            .iter()
            .any(|r| r.exposure == CeilingExposure::InterStorey));

        ber.storeys.pop();
        derive_all(&mut ber);

        let ground = &ber.storeys[0];
        assert!(
            ground
                .walls
                .regions
                .iter()
                .all(|r| r.exposure != WallExposure::InterStorey),
            "no wall stamp survives"
        );
        assert!(ground
            .floors
            .regions
            .iter()
            .all(|r| r.exposure != FloorExposure::InterStorey));
        assert!(
            ground
                .ceilings
                .regions
                .iter()
                .all(|r| r.exposure == CeilingExposure::Unset),
            "stale ceiling stamps reset to Unset — no residue"
        );
    }

    // ── review fix: neighbour ceiling step PERPENDICULAR to the wall ─────

    /// RE-PIN (subtract-don't-split): the OLD pass split this wall
    /// VERTICALLY at the neighbour's step line — 2 length pieces × 2 bands
    /// = 4 pieces. What the test proves NOW: the per-side band ATTRIBUTION
    /// survives (one buried `InterStorey` band per neighbour-ceiling run,
    /// each rising to its OWN band top — the smear the run-splitting
    /// prevents is still prevented), while the exposed remainder is ONE
    /// wall whose BOTTOM steps low_top → tall_top at the step line: pieces
    /// are cut only where the exposure CLASS changes, never between two
    /// same-class spans.
    #[test]
    fn rotated_step_keeps_bands_per_side_and_one_stepped_remainder() {
        let mut ber = rotated_step_fixture();
        assert_eq!(walls_on_y(&ber.storeys[1], 2.0).len(), 1);

        derive_all(&mut ber);

        // Local band tops on the first floor (datum 2.7): low side
        // 3.2 − 2.7 = 0.5, tall side 4.0 − 2.7 = 1.3.
        let datum = ber.storey_floor_datums()[1];
        let (low_top, tall_top) = (3.2 - datum, 4.0 - datum);
        let pieces = walls_on_y(&ber.storeys[1], 2.0);
        assert_eq!(
            pieces.len(),
            3,
            "two per-side buried bands + ONE stepped remainder"
        );
        // Buried bands: one per neighbour-ceiling run, each to its own top.
        let bands: Vec<&&WallRegion> = pieces
            .iter()
            .filter(|w| w.exposure == WallExposure::InterStorey)
            .collect();
        assert_eq!(bands.len(), 2, "bands stay PER-RUN");
        let mut seen = [false; 2];
        for w in &bands {
            let (x0, x1) = x_extent(w);
            let (zmin, zmax) = z_extent(w);
            let low_side = x1 <= 4.0 + 1e-6;
            let top = if low_side { low_top } else { tall_top };
            assert!(
                (x0 - if low_side { 0.0 } else { 4.0 }).abs() < 1e-6
                    && (x1 - if low_side { 4.0 } else { 8.0 }).abs() < 1e-6,
                "band spans one side of the step line, got x [{x0}, {x1}]"
            );
            // Floor up to EXACTLY this side's band top — no smeared sample.
            assert!((zmin - 0.0).abs() < 1e-6 && (zmax - top).abs() < 1e-6);
            let side = usize::from(!low_side);
            assert!(!seen[side], "duplicate band on one side");
            seen[side] = true;
        }
        assert!(seen.iter().all(|s| *s), "one band per side");
        // The remainder: ONE wall across the whole junction span, its bottom
        // stepping 0.5 → 1.3 at x=4 — no vertical same-class split, and no
        // Unset sliver between the bands and the remainder.
        let rem = pieces
            .iter()
            .find(|w| w.exposure == WallExposure::Exposed)
            .expect("the exposed remainder");
        let (x0, x1) = x_extent(rem);
        assert!(
            (x0 - 0.0).abs() < 1e-6 && (x1 - 8.0).abs() < 1e-6,
            "one remainder across the junction span, got x [{x0}, {x1}]"
        );
        let (zmin, zmax) = z_extent(rem);
        assert!((zmin - low_top).abs() < 1e-6 && (zmax - 2.4).abs() < 1e-6);
        // The underside traces the step exactly: both step-corner vertices
        // sit on the ring, and the per-along underside query reads the
        // per-side band tops (the parent base runs +x from x=0, so along=x).
        for (wx, wz) in [(4.0, low_top), (4.0, tall_top)] {
            assert!(
                rem.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - wx).abs() < 1e-6 && (v.z - wz).abs() < 1e-6),
                "remainder underside misses step vertex ({wx}, {wz})"
            );
        }
        assert!((rem.base_z_at_along(2.0) - low_top).abs() < 1e-6);
        assert!((rem.base_z_at_along(6.0) - tall_top).abs() < 1e-6);
        // The ground risers under the first floor's slab band are buried.
        for w in walls_on_y(&ber.storeys[0], 2.0) {
            assert_eq!(w.exposure, WallExposure::InterStorey);
        }
        // The ground's x=4 riser (between the two RAISED sections, outside
        // the first floor's footprint) is untouched.
        for w in walls_on_x(&ber.storeys[0], 4.0) {
            assert_eq!(w.exposure, WallExposure::Unset);
        }

        // Exact idempotence still holds on the rotated fixture.
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        let twice = serde_json::to_string(&ber.storeys).unwrap();
        assert_eq!(once, twice, "re-derive must be a no-op");
    }

    /// The symmetric case: the wall's midpoint (x=4) lies exactly ON the
    /// neighbour's step line, where `top_z_at`'s boundary-inclusive
    /// first-region-wins pick is region-order-dependent. Breaking the runs at
    /// the neighbour's ceiling boundaries moves every sample strictly inside
    /// one section, so the outcome must be identical when the neighbour's
    /// ceiling REGION ORDER is reversed.
    #[test]
    fn rotated_step_result_is_independent_of_neighbour_region_order() {
        let mut plain = rotated_step_fixture();
        derive_all(&mut plain);

        let mut reversed = rotated_step_fixture();
        reversed.storeys[0].ceilings.regions.reverse();
        derive_all(&mut reversed);

        let (p, r) = (
            y_wall_descriptors(&plain.storeys[1], 2.0),
            y_wall_descriptors(&reversed.storeys[1], 2.0),
        );
        // RE-PIN (subtract-don't-split): 4 → 3 pieces — two per-run bands +
        // ONE stepped remainder (see the fixture re-pin above). The point of
        // THIS test is unchanged: the piece set must not depend on the
        // neighbour's ceiling region order.
        assert_eq!(p.len(), 3, "the wall split in the plain orientation");
        assert_eq!(
            p, r,
            "piece set (geometry + exposure) must not depend on the \
             neighbour's ceiling region order"
        );
    }

    // ── serde: the new variants are additive ─────────────────────────────

    #[test]
    fn exposure_serde_is_backward_compatible_and_round_trips() {
        // Every OLD-shape value still loads (adding a variant is additive).
        for old in ["Exposed", "SemiExposed", "NonHeatLoss", "Unset", "Ground"] {
            let json = format!("\"{old}\"");
            let _: WallExposure = serde_json::from_str(&json).unwrap();
        }
        for old in ["Unset", "Ground", "Exposed", "SemiExposed", "NonHeatLoss"] {
            let json = format!("\"{old}\"");
            let _: FloorExposure = serde_json::from_str(&json).unwrap();
        }
        for old in ["Unset", "ExposedRoof", "Party", "Unheated"] {
            let json = format!("\"{old}\"");
            let _: CeilingExposure = serde_json::from_str(&json).unwrap();
        }
        // The new variant round-trips.
        let w = serde_json::to_string(&WallExposure::InterStorey).unwrap();
        assert_eq!(w, "\"InterStorey\"");
        assert_eq!(
            serde_json::from_str::<WallExposure>(&w).unwrap(),
            WallExposure::InterStorey
        );
        let f = serde_json::to_string(&FloorExposure::InterStorey).unwrap();
        assert_eq!(
            serde_json::from_str::<FloorExposure>(&f).unwrap(),
            FloorExposure::InterStorey
        );
        let c = serde_json::to_string(&CeilingExposure::InterStorey).unwrap();
        assert_eq!(
            serde_json::from_str::<CeilingExposure>(&c).unwrap(),
            CeilingExposure::InterStorey
        );
    }

    // ── Fix #2: the inter-storey ceiling cut is non-accumulating ─────────────

    /// The reported bug: an upper footprint that MOVES between two derives
    /// (5.42 → 5.21) must leave ONE clean lower-ceiling boundary at the FINAL
    /// position — the stale 5.42 cut is reversed, no 0.2 m sliver accumulates.
    /// Regions-as-truth: the lower ceiling is kept verbatim between derives
    /// (the controller does not re-resolve the lower storey), so mutating only
    /// the upper floor footprint reproduces the controller re-resolve exactly.
    #[test]
    fn moved_upper_footprint_leaves_one_clean_lower_ceiling_boundary() {
        let cid = ItemId::default();
        let mut ber = Ber::default();
        ber.storeys = vec![
            flat_storey(square(0.0, 0.0, 10.0, 4.0), 2.4, None),
            flat_storey(square(0.0, 0.0, 5.42, 4.0), 2.4, Some(0.3)),
        ];
        derive_all(&mut ber);
        assert_eq!(
            ber.storeys[0].ceilings.regions.len(),
            2,
            "first derive cuts the lower ceiling once at 5.42"
        );

        // The upper footprint moves to 5.21 (a re-dimension); the lower ceiling
        // stays verbatim.
        ber.storeys[1].floors = FloorRegions::new(square(0.0, 0.0, 5.21, 4.0), cid);
        derive_all(&mut ber);

        let cs = &ber.storeys[0].ceilings.regions;
        assert_eq!(
            cs.len(),
            2,
            "still exactly two regions — the stale 5.42 cut did NOT accumulate"
        );
        let buried: Vec<&CeilingRegion> = cs
            .iter()
            .filter(|r| r.exposure == CeilingExposure::InterStorey)
            .collect();
        assert_eq!(buried.len(), 1, "one buried piece");
        let (_, b_hi) = ceiling_x_extent(buried[0]);
        assert!(
            (b_hi - 5.21).abs() < 1e-4,
            "boundary at the FINAL position 5.21, got {b_hi}"
        );
        // No region occupies the stale [5.21, 5.42] band as a sliver of its own.
        assert!(
            !cs.iter().any(|r| {
                let (lo, hi) = ceiling_x_extent(r);
                lo > 5.205 && hi < 5.425
            }),
            "no 0.2 m sliver region survives"
        );
        // Fixpoint: a third derive at the SAME footprint changes nothing.
        let before = ceiling_descriptor(&ber.storeys[0].ceilings);
        derive_all(&mut ber);
        assert_eq!(
            ceiling_descriptor(&ber.storeys[0].ceilings),
            before,
            "same-footprint re-derive is idempotent"
        );
    }

    /// ROOF INTEGRITY: a lower storey with an APEX-fanned (sloped) ceiling that
    /// gets an inter-storey cut, re-derived TWICE (a re-dimension), keeps its
    /// sloped-region COUNT and z-heights EXACTLY — the coalesce is flat-only,
    /// so it never unions a fan face. The apex is neither lost nor flattened.
    #[test]
    fn apex_ceiling_survives_inter_storey_re_derive() {
        // Ground 8×4 fanned to a central apex (4,2) at z = 4.0 over a 2.4 base.
        let mut ground = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, None);
        ground.ceilings = ground.ceilings.with_apexes(&[CeilingApex {
            point: Point2f64::new(4.0, 2.0),
            z_m: 4.0,
        }]);
        assert!(
            ground.ceilings.regions.len() >= 4,
            "the fan produced multiple sloped faces"
        );
        let apex_zmax = |cs: &CeilingRegions| {
            cs.regions
                .iter()
                .flat_map(|r| r.polygon.exterior.iter())
                .fold(f64::NEG_INFINITY, |m, v| m.max(v.z))
        };
        assert!((apex_zmax(&ground.ceilings) - 4.0).abs() < 1e-6);

        let first = flat_storey(square(0.0, 0.0, 4.0, 4.0), 2.4, Some(0.3));
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];

        derive_all(&mut ber);
        let after1 = ceiling_descriptor(&ber.storeys[0].ceilings);
        derive_all(&mut ber);
        let after2 = ceiling_descriptor(&ber.storeys[0].ceilings);
        assert_eq!(
            after1, after2,
            "re-derive is a fixpoint — no new slivers, no lost/flattened apex"
        );
        assert!(
            (apex_zmax(&ber.storeys[0].ceilings) - 4.0).abs() < 1e-6,
            "apex height preserved across the inter-storey cut"
        );
    }

    /// The assessor's own ceiling carve (two DIFFERENT-construction regions)
    /// survives the derive + coalesce: coalesce only fuses across a
    /// buried↔exposed edge of the SAME construction, so a genuine carve at x=5
    /// is never dissolved even when the derive splits the region beside it.
    #[test]
    fn assessor_ceiling_carve_survives_re_derive() {
        let cid_a = ItemId::default();
        let cid_b = ItemId::default();
        let flat = |ring: Vec<Point2f64>, cid: ItemId| CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(
                ring.into_iter()
                    .map(|p| Point3f64::new(p.x, p.y, 2.4))
                    .collect(),
            ),
            opes: Default::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        let mut ground = flat_storey(square(0.0, 0.0, 10.0, 4.0), 2.4, None);
        ground.ceilings = CeilingRegions {
            regions: vec![
                flat(square(0.0, 0.0, 5.0, 4.0), cid_a),
                flat(square(5.0, 0.0, 10.0, 4.0), cid_b),
            ],
        };
        // Upper storey covers [0,3] — fully within construction-A's half.
        let first = flat_storey(square(0.0, 0.0, 3.0, 4.0), 2.4, Some(0.3));
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];

        derive_all(&mut ber);
        derive_all(&mut ber); // re-dimension

        let cs = &ber.storeys[0].ceilings.regions;
        assert!(
            cs.iter().any(|r| r.construction_id == cid_a),
            "construction A survives"
        );
        // Construction B is untouched: one region spanning [5,10], not buried.
        let bs: Vec<&CeilingRegion> = cs.iter().filter(|r| r.construction_id == cid_b).collect();
        assert_eq!(bs.len(), 1, "B stays one region");
        let (b_lo, b_hi) = ceiling_x_extent(bs[0]);
        assert!(
            (b_lo - 5.0).abs() < 1e-4 && (b_hi - 10.0).abs() < 1e-4,
            "B unchanged over [5,10]"
        );
        assert_ne!(
            bs[0].exposure,
            CeilingExposure::InterStorey,
            "B is not buried"
        );
        // A is split by the [0,3] footprint but never fused into B: the carve
        // boundary at x=5 still bounds every A piece.
        let a_hi = cs
            .iter()
            .filter(|r| r.construction_id == cid_a)
            .flat_map(|r| r.polygon.exterior.iter())
            .fold(f64::NEG_INFINITY, |m, v| m.max(v.x));
        assert!(
            (a_hi - 5.0).abs() < 1e-4,
            "the assessor carve at x=5 holds, got {a_hi}"
        );
    }

    // ── Sloped-neighbour band-top PROFILE (field report ff5a5ee3) ─────────

    /// A two-storey stack whose GROUND ceiling meets the shared BACK wall
    /// line (y = 4) along a continuous TENT profile: flat 2.4 over the front
    /// strip y∈[0,2] (the datum anchor the first floor sits on) and two
    /// sloped facets over the back strip y∈[2,4] whose planes depend only on
    /// x — z runs `zl` → `zm` over x∈[0,4] and `zm` → `zr` over x∈[4,8], so
    /// the profile along y=4 is the piecewise-linear polyline
    /// (0,zl)–(4,zm)–(8,zr). First floor: full 8×4 footprint, flat 2.4
    /// ceiling, 0.3 slab → datum 2.7.
    fn sloped_back_fixture(zl: f64, zm: f64, zr: f64) -> Ber {
        let z_at = move |x: f64| {
            if x <= 4.0 {
                zl + (zm - zl) * x / 4.0
            } else {
                zm + (zr - zm) * (x - 4.0) / 4.0
            }
        };
        let region = |ring: Vec<Point2f64>, zf: &dyn Fn(f64) -> f64| CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(
                ring.into_iter()
                    .map(|p| Point3f64::new(p.x, p.y, zf(p.x)))
                    .collect(),
            ),
            opes: Default::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        };
        let mut ground = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, None);
        ground.ceilings = CeilingRegions {
            regions: vec![
                region(square(0.0, 0.0, 8.0, 2.0), &|_| 2.4),
                region(square(0.0, 2.0, 4.0, 4.0), &z_at),
                region(square(4.0, 2.0, 8.0, 4.0), &z_at),
            ],
        };
        let first = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, Some(0.3));
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];
        ber
    }

    /// The ff5a5ee3 mechanism in miniature: the first floor's back wall is
    /// backed by the ground's TENT roof — profile local z = −0.3 + 0.2x
    /// rising to 0.5 at the ridge (x=4), falling back — so the wall must get
    /// ONE cut along that profile: the covered triangle under it (top
    /// tracing the slope, `InterStorey`) plus ONE remainder — never the old
    /// per-facet staircase (vertical cut at x=4, one flat step per facet).
    #[test]
    fn sloped_neighbour_profile_cuts_wall_into_covered_slope_and_one_remainder() {
        let mut ber = sloped_back_fixture(2.4, 3.2, 2.4);
        derive_all(&mut ber);
        let datums = ber.storey_floor_datums();
        assert!(
            (datums[1] - 2.7).abs() < 1e-9,
            "datum anchors on the flat strip, got {datums:?}"
        );
        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        assert_eq!(
            pieces.len(),
            2,
            "ONE profile cut: covered piece + remainder — no per-facet staircase"
        );
        let covered = pieces
            .iter()
            .find(|w| w.exposure == WallExposure::InterStorey)
            .expect("covered piece");
        let remainder = pieces
            .iter()
            .find(|w| w.exposure != WallExposure::InterStorey)
            .expect("remainder");
        // The covered piece is EXACTLY the triangle under the profile: base
        // on the wall bottom between the profile's z=0 crossings (x = 1.5 /
        // 6.5), apex at the tent ridge (x = 4, local z = 0.5).
        let mut cov: Vec<(f64, f64)> = covered
            .polygon
            .exterior
            .iter()
            .map(|v| (v.x, v.z))
            .collect();
        cov.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let want = [(1.5, 0.0), (4.0, 0.5), (6.5, 0.0)];
        assert_eq!(cov.len(), want.len(), "a clean triangle, got {cov:?}");
        for ((x, z), (wx, wz)) in cov.iter().zip(want) {
            assert!(
                (x - wx).abs() < 1e-6 && (z - wz).abs() < 1e-6,
                "covered top traces the profile vertices: got {cov:?}"
            );
        }
        // ONE remainder across the whole junction span, full height, its
        // underside tracing the same profile vertices — a z-step staircase
        // would have shown flat tops at the per-facet sample heights instead.
        let (x0, x1) = x_extent(remainder);
        assert!(
            (x0 - 0.0).abs() < 1e-6 && (x1 - 8.0).abs() < 1e-6,
            "one remainder across the junction span, got x [{x0}, {x1}]"
        );
        let (rzmin, rzmax) = z_extent(remainder);
        assert!((rzmin - 0.0).abs() < 1e-6 && (rzmax - 2.4).abs() < 1e-6);
        assert_eq!(remainder.exposure, WallExposure::Exposed);
        for (wx, wz) in want {
            assert!(
                remainder
                    .polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - wx).abs() < 1e-6 && (v.z - wz).abs() < 1e-6),
                "remainder underside misses profile vertex ({wx}, {wz})"
            );
        }
        // The remainder is a FIRST-CLASS wall: a full-span base frame (its
        // base is the profile polyline), so it can host the wide window the
        // field report was refused.
        let (_, _, rlen, _) = remainder
            .base_frame()
            .expect("the remainder has a real base frame");
        assert!((rlen - 8.0).abs() < 1e-6, "full plan span, got {rlen}");
    }

    /// Deriving twice over the profile cut must be byte-identical — the
    /// covered piece re-covers whole (stamped intact, never re-cut) and the
    /// remainder's overlap with the band is a hairline (left intact), so
    /// profile splits never accumulate.
    #[test]
    fn sloped_profile_derive_is_an_exact_fixpoint() {
        let mut ber = sloped_back_fixture(2.4, 3.2, 2.4);
        derive_all(&mut ber);
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        let twice = serde_json::to_string(&ber.storeys).unwrap();
        assert_eq!(once, twice, "profile cut must be re-derive stable");
    }

    /// Degenerate profile: entirely ABOVE the wall top → the wall is buried
    /// WHOLE — one region, same ring, no cut.
    #[test]
    fn profile_above_the_wall_top_buries_the_wall_whole() {
        let mut ber = sloped_back_fixture(5.2, 5.4, 5.2); // local 2.5..2.7 > 2.4
        let before: Vec<Vec<Point3f64>> = walls_on_y(&ber.storeys[1], 4.0)
            .iter()
            .map(|w| w.polygon.exterior.clone())
            .collect();
        assert_eq!(before.len(), 1);
        derive_all(&mut ber);
        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        assert_eq!(
            pieces.len(),
            1,
            "no cut — the whole wall is under the profile"
        );
        assert_eq!(pieces[0].exposure, WallExposure::InterStorey);
        assert_eq!(pieces[0].polygon.exterior, before[0], "ring untouched");
    }

    /// Degenerate profile: entirely BELOW the wall base → the wall (and every
    /// other first-floor wall) is untouched, byte-for-byte.
    #[test]
    fn profile_below_the_wall_base_leaves_the_wall_untouched() {
        let mut ber = sloped_back_fixture(2.3, 2.38, 2.3); // local −0.4..−0.32
        let before = serde_json::to_string(&ber.storeys[1].walls).unwrap();
        derive_all(&mut ber);
        assert_eq!(
            serde_json::to_string(&ber.storeys[1].walls).unwrap(),
            before,
            "a band below the base never splits or stamps"
        );
    }

    /// Sliver guard: a profile hugging the wall TOP (within [`MIN_SUBWALL_M`])
    /// must never emit the hairline remainder strip — the wall buries WHOLE
    /// instead, and stays a fixpoint.
    #[test]
    fn profile_hugging_the_wall_top_never_emits_a_sliver_remainder() {
        // Local band top 2.392..2.398 — 2–8 mm under the 2.4 wall top.
        let mut ber = sloped_back_fixture(5.092, 5.098, 5.092);
        derive_all(&mut ber);
        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        assert_eq!(
            pieces.len(),
            1,
            "the sub-[`MIN_SUBWALL_M`] strip above the profile is never emitted"
        );
        assert_eq!(
            pieces[0].exposure,
            WallExposure::InterStorey,
            "the wall buries whole instead"
        );
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        assert_eq!(serde_json::to_string(&ber.storeys).unwrap(), once);
    }

    /// A profile dipping FAR (> 1 m) below the wall bottom at both ends: the
    /// cover polygon's floor must extend below the profile's own minimum —
    /// a floor pegged 1 m under the WALL bottom alone would sit above the
    /// dipped profile ends and make the cover ring self-intersecting. The
    /// cut still yields exactly the covered triangle between the profile's
    /// base crossings plus one remainder, and stays a fixpoint.
    #[test]
    fn profile_dipping_far_below_the_base_still_cuts_cleanly() {
        // Tent 0.7 → 3.2 → 0.7 abs = local −2.0 → 0.5 → −2.0 (datum 2.7):
        // slope 0.625/m, base crossings at x = 3.2 / 4.8, ridge (4, 0.5).
        let mut ber = sloped_back_fixture(0.7, 3.2, 0.7);
        derive_all(&mut ber);
        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        assert_eq!(pieces.len(), 2, "covered triangle + one remainder");
        let covered = pieces
            .iter()
            .find(|w| w.exposure == WallExposure::InterStorey)
            .expect("covered piece");
        let mut cov: Vec<(f64, f64)> = covered
            .polygon
            .exterior
            .iter()
            .map(|v| (v.x, v.z))
            .collect();
        cov.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let want = [(3.2, 0.0), (4.0, 0.5), (4.8, 0.0)];
        assert_eq!(cov.len(), want.len(), "a clean triangle, got {cov:?}");
        for ((x, z), (wx, wz)) in cov.iter().zip(want) {
            assert!(
                (x - wx).abs() < 1e-6 && (z - wz).abs() < 1e-6,
                "covered top traces the profile: got {cov:?}"
            );
        }
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        assert_eq!(serde_json::to_string(&ber.storeys).unwrap(), once);
    }

    /// A neighbour ceiling rising MONOTONICALLY by more than
    /// `MAX_FLOOR_STEP_M` (0.6 m) along the wall cuts cleanly into covered
    /// piece + remainder, and the remainder's `base_frame()` keeps the FULL
    /// plan span: `base_edge`'s floor band anchors to the base run's MINIMUM
    /// z, so a ring opening at the profile's TALL end still reaches the
    /// floor-level far corner. (Before the band was anchored to the START
    /// vertex's z, which truncated this frame to the in-band leading run —
    /// a 4 m span. The mirrored orientation — ring opening at the LOW end,
    /// profile rising out of the band in +along — can still truncate; that
    /// residual limitation goes away with the profile-aware base work.)
    #[test]
    fn steep_profile_remainder_keeps_the_full_plan_span() {
        // Monotonic rise 2.4 → 3.2 → 4.0 abs = local −0.3 → 0.5 → 1.3:
        // base crossing at x = 1.5, total rise 1.6 m ≫ the 0.6 m floor band.
        let mut ber = sloped_back_fixture(2.4, 3.2, 4.0);
        derive_all(&mut ber);
        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        assert_eq!(pieces.len(), 2, "one profile cut even when steep");
        let remainder = pieces
            .iter()
            .find(|w| w.exposure != WallExposure::InterStorey)
            .expect("remainder");
        let (x0, x1) = x_extent(remainder);
        assert!(
            (x0 - 0.0).abs() < 1e-6 && (x1 - 8.0).abs() < 1e-6,
            "the remainder GEOMETRY spans the whole wall, got x [{x0}, {x1}]"
        );
        // The parent wall's base runs x = 8 → 0 (perimeter direction), so
        // the ring's leftmost-lowest start in the wall's own frame is the
        // profile's TALL end (8, 1.3) — `base_frame`'s base_z stays the z
        // AT that start vertex — but the far endpoint is the floor-level
        // corner (0, 0): the floor band anchors at the base run's minimum
        // (z = 0), which the 1.3 start no longer truncates.
        let (_, _, rlen, base_z) = remainder
            .base_frame()
            .expect("the remainder has a base frame");
        assert!(
            (base_z - 1.3).abs() < 1e-6,
            "frame anchors at the profile's tall end, got base_z {base_z}"
        );
        assert!(
            (rlen - 8.0).abs() < 1e-6,
            "full plan span even for a steep profile, got {rlen}"
        );
        // Steep or not, re-derivation stays exact.
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        assert_eq!(serde_json::to_string(&ber.storeys).unwrap(), once);
    }

    // ── Gap pins: risers / multi-component profiles through the profile clip ──

    /// A wall piece is a valid simple face: ≥ 3 vertices, a real (non-sliver)
    /// area, and no two consecutive vertices coincide (a self-touching neck).
    fn assert_piece_sane(w: &WallRegion) {
        let e = &w.polygon.exterior;
        assert!(e.len() >= 3, "piece has {} vertices", e.len());
        assert!(
            w.polygon.area() > MIN_SUBWALL_M * MIN_SUBWALL_M,
            "degenerate piece area {}",
            w.polygon.area()
        );
        let n = e.len();
        for i in 0..n {
            let (p, q) = (e[i], e[(i + 1) % n]);
            let d = ((p.x - q.x).powi(2) + (p.y - q.y).powi(2) + (p.z - q.z).powi(2)).sqrt();
            assert!(d > 1e-6, "piece self-touches at vertex {i}");
        }
    }

    /// One ceiling region over `ring` whose z follows `zf(x)` — a sloped facet
    /// for the profile-clip gap tests (mirrors `sloped_back_fixture`'s builder).
    fn sloped_ceiling(ring: Vec<Point2f64>, zf: &dyn Fn(f64) -> f64) -> CeilingRegion {
        CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(
                ring.into_iter()
                    .map(|p| Point3f64::new(p.x, p.y, zf(p.x)))
                    .collect(),
            ),
            opes: Default::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        }
    }

    /// A MERGED wall (`merge_coplanar_flush` grows the host's ring while the
    /// host keeps its short base EDGE) must be scanned over its FULL plan
    /// extent, not just that edge — otherwise `clip_wall_by_bands`'s
    /// "ring outruns the scanned base frame" guard leaves the whole wall alone
    /// and the buried part is never stamped.
    ///
    /// Field case (56 Elmwood Road): the step down to a dropped first-floor
    /// area merged into a 0.37 m outline stub, so 4.4 m of face buried by the
    /// storey below kept reading Exposed.
    #[test]
    fn a_merged_wall_is_scanned_past_its_base_edge() {
        let cid = ItemId::default();
        // A short OUTLINE wall on x = 0 over y ∈ [4.5, 5], full height...
        let host = WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 4.5, 0.0),
                Point3f64::new(0.0, 5.0, 0.0),
                Point3f64::new(0.0, 5.0, 3.0),
                Point3f64::new(0.0, 4.5, 3.0),
            ]),
            exposure: WallExposure::Exposed,
            opes: Default::default(),
            construction_id: cid,
        };
        // ...and a low RISER coplanar with it running the rest of the way,
        // y ∈ [0, 4.5] × z ∈ [0, 1] — flush along y = 4.5, so `with_risers`
        // merges it in and the host keeps its 0.5 m base edge.
        let riser = Polygon3D::new(vec![
            Point3f64::new(0.0, 0.0, 0.0),
            Point3f64::new(0.0, 4.5, 0.0),
            Point3f64::new(0.0, 4.5, 1.0),
            Point3f64::new(0.0, 0.0, 1.0),
        ]);
        let merged = WallRegions {
            regions: vec![host],
        }
        .with_risers(vec![(riser, WallExposure::Unset)], cid);
        assert_eq!(merged.regions.len(), 1, "the riser merged into the host");
        let wall = merged.regions.into_iter().next().unwrap();
        let base_len = wall
            .base_edge()
            .map(|(a, b)| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt())
            .expect("base edge");
        assert!((base_len - 0.5).abs() < 1e-9, "short host base: {base_len}");
        let reach = wall
            .polygon
            .exterior
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| {
                (l.min(v.y), h.max(v.y))
            });
        assert!(
            reach.0 < 1e-9 && reach.1 > 4.9,
            "ring reaches past it: {reach:?}"
        );

        // A neighbour covering the whole span with its ceiling at 1.0 → the
        // wall's bottom metre is buried along its ENTIRE length.
        let ring = square(-2.0, 0.0, 2.0, 5.0);
        let fp = PlanFootprint::of(&FloorRegions::new(ring.clone(), cid));
        let bp_floors = FloorRegions::new(ring.clone(), cid);
        let ceilings = CeilingRegions {
            regions: vec![sloped_ceiling(ring, &|_| 1.0)],
        };
        let empty_floors = FloorRegions::default();
        let nb = NeighbourBand {
            fp: &fp,
            bp_floors,
            ceilings: &ceilings,
            floors: &empty_floors,
            datum: 0.0,
            slab: 0.0,
            band_lo: -1.0,
        };
        let mut out = Vec::new();
        process_wall(wall, &nb, 0.0, &mut out);
        assert!(
            out.len() >= 2,
            "the merged wall is split, got {}",
            out.len()
        );
        let buried = out
            .iter()
            .find(|w| w.exposure == WallExposure::InterStorey)
            .unwrap_or_else(|| panic!("a buried band is stamped: {out:?}"));
        let (y0, y1) = buried
            .polygon
            .exterior
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(l, h), v| {
                (l.min(v.y), h.max(v.y))
            });
        assert!(
            y0 < 0.5 && y1 > 4.0,
            "stamped across the merged reach, not just the base edge: y[{y0}, {y1}]"
        );
    }

    /// GAP PIN (risers vs a SLOPED neighbour): a RISER-shaped wall (a trapezoid,
    /// not a full-height quad) backed by a sloped neighbour ceiling must go
    /// through `clip_wall_by_band_profile` and come out a sane covered piece +
    /// remainder — the riser analogue of
    /// `sloped_neighbour_profile_cuts_wall_into_covered_slope_and_one_remainder`.
    /// It exercises `process_wall` DIRECTLY so the sloped clip is guaranteed to
    /// run (the assertions fail loudly if the band never engages).
    #[test]
    fn riser_face_against_a_sloped_neighbour_ceiling_splits_cleanly() {
        // Riser: base along x∈[0,4] at y=2, z=0; top slopes (4,1.2)→(0,2.4).
        let riser = WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 2.0, 0.0),
                Point3f64::new(4.0, 2.0, 0.0),
                Point3f64::new(4.0, 2.0, 1.2),
                Point3f64::new(0.0, 2.0, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Default::default(),
            construction_id: ItemId::default(),
        };
        // Neighbour footprint straddles the base line; TENT ceiling peaking at
        // x=2 (local z 1.6), falling to 1.0 at x=0 / x=4 — one continuous run.
        let footprint = FloorRegions::new(square(0.0, 0.0, 4.0, 4.0), ItemId::default());
        let fp = PlanFootprint::of(&footprint);
        let bp_floors = FloorRegions::from_vertex_lists(fp.rings.clone(), ItemId::default());
        let ceilings = CeilingRegions {
            regions: vec![
                sloped_ceiling(square(0.0, 0.0, 2.0, 4.0), &|x| 1.0 + 0.3 * x),
                sloped_ceiling(square(2.0, 0.0, 4.0, 4.0), &|x| 2.2 - 0.3 * x),
            ],
        };
        // No floor regions → `band_lo_at` falls back to the flat `band_lo`,
        // which is what these band-TOP fixtures are exercising.
        let empty_floors = FloorRegions::default();
        let nb = NeighbourBand {
            fp: &fp,
            bp_floors,
            ceilings: &ceilings,
            floors: &empty_floors,
            datum: 0.0,
            slab: 0.0,
            band_lo: -1.0,
        };
        let mut out = Vec::new();
        process_wall(riser, &nb, 0.0, &mut out);
        // The sloped profile cuts the riser into a covered piece + a remainder.
        assert!(out.len() >= 2, "riser split into {} pieces", out.len());
        let covered = out
            .iter()
            .filter(|w| w.exposure == WallExposure::InterStorey)
            .count();
        assert_eq!(covered, 1, "one covered piece under the sloped profile");
        for w in &out {
            assert_piece_sane(w);
        }
        // Areas conserve: the pieces sum to the riser's 7.2 m² face.
        let sum: f64 = out.iter().map(|w| w.polygon.area()).sum();
        assert!(
            (sum - 7.2).abs() < 1e-3,
            "areas sum to the riser face: {sum}"
        );
    }

    /// GAP PIN (MULTI-COMPONENT profile): an M-profile neighbour ceiling — two
    /// humps rising above the wall base with a valley dipping BELOW it between
    /// them — covers the wall in TWO disjoint pieces, so the split emits N > 2
    /// regions. The "exactly 2" contract only holds for a single hump; here the
    /// result must still be sane (areas conserve, every piece valid).
    #[test]
    fn multi_component_profile_emits_more_than_two_sane_regions() {
        // Wall base at z=1.0 (so the valley can dip below it), 8 m × 2.4 m tall.
        let wall = WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 2.0, 1.0),
                Point3f64::new(8.0, 2.0, 1.0),
                Point3f64::new(8.0, 2.0, 3.4),
                Point3f64::new(0.0, 2.0, 3.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Default::default(),
            construction_id: ItemId::default(),
        };
        let footprint = FloorRegions::new(square(0.0, 0.0, 8.0, 4.0), ItemId::default());
        let fp = PlanFootprint::of(&footprint);
        let bp_floors = FloorRegions::from_vertex_lists(fp.rings.clone(), ItemId::default());
        // z(x) = 0.5, 2.0, 0.8, 2.0, 0.5 at x = 0,2,4,6,8 — humps at x=2/6 (above
        // the 1.0 base), valley at x=4 (0.8, below it). One continuous run.
        let seg = |x0: f64, z0: f64, z1: f64| move |x: f64| z0 + (z1 - z0) * (x - x0) / 2.0;
        let ceilings = CeilingRegions {
            regions: vec![
                sloped_ceiling(square(0.0, 0.0, 2.0, 4.0), &seg(0.0, 0.5, 2.0)),
                sloped_ceiling(square(2.0, 0.0, 4.0, 4.0), &seg(2.0, 2.0, 0.8)),
                sloped_ceiling(square(4.0, 0.0, 6.0, 4.0), &seg(4.0, 0.8, 2.0)),
                sloped_ceiling(square(6.0, 0.0, 8.0, 4.0), &seg(6.0, 2.0, 0.5)),
            ],
        };
        // No floor regions → `band_lo_at` falls back to the flat `band_lo`,
        // which is what these band-TOP fixtures are exercising.
        let empty_floors = FloorRegions::default();
        let nb = NeighbourBand {
            fp: &fp,
            bp_floors,
            ceilings: &ceilings,
            floors: &empty_floors,
            datum: 0.0,
            slab: 0.0,
            band_lo: -1.0,
        };
        let mut out = Vec::new();
        process_wall(wall, &nb, 0.0, &mut out);
        assert!(
            out.len() >= 3,
            "M-profile emits N>2 regions, got {}",
            out.len()
        );
        let covered = out
            .iter()
            .filter(|w| w.exposure == WallExposure::InterStorey)
            .count();
        assert_eq!(covered, 2, "two disjoint covered humps, got {covered}");
        for w in &out {
            assert_piece_sane(w);
        }
        let sum: f64 = out.iter().map(|w| w.polygon.area()).sum();
        assert!(
            (sum - 19.2).abs() < 1e-3,
            "areas sum to the wall face: {sum}"
        );
    }

    // ── Subtract-don't-split (plan §6 step 2): Elmwood & friends ─────────

    /// §5 SLIVER INVARIANT, asserted over EVERY wall of every storey: the
    /// wall pass's only piece factory is the filtered clip
    /// ([`clip_wall_by_bands`]), so no derived wall may be degenerate —
    /// sub-[`MIN_SUBWALL_M`]² area, or a bounding-box minor dimension
    /// (plan extent vs z extent) under [`MIN_SUBWALL_M`].
    fn assert_no_degenerate_walls(ber: &Ber) {
        for (si, s) in ber.storeys.iter().enumerate() {
            for w in &s.walls.regions {
                let area = w.polygon.area();
                assert!(
                    area > MIN_SUBWALL_M * MIN_SUBWALL_M,
                    "storey {si}: wall {:?} has sliver area {area}",
                    w.id
                );
                let (zmin, zmax) = z_extent(w);
                let (xmin, xmax, ymin, ymax) = w.polygon.exterior.iter().fold(
                    (
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                    ),
                    |(x0, x1, y0, y1), v| (x0.min(v.x), x1.max(v.x), y0.min(v.y), y1.max(v.y)),
                );
                let plan_extent = (xmax - xmin).hypot(ymax - ymin);
                assert!(
                    plan_extent.min(zmax - zmin) > MIN_SUBWALL_M,
                    "storey {si}: wall {:?} has a sliver minor dimension \
                     (plan {plan_extent}, z {})",
                    w.id,
                    zmax - zmin
                );
            }
        }
    }

    /// A FLAT ceiling region over `ring` at height `z` — the constant-z
    /// convenience over [`sloped_ceiling`], for hand-built neighbour stacks.
    fn flat_ceiling(ring: Vec<Point2f64>, z: f64) -> CeilingRegion {
        sloped_ceiling(ring, &|_| z)
    }

    /// THE ELMWOOD FIXTURE (fixture of record — 56 Elmwood Road, storey
    /// idx1's y=1.42 run): a lower storey with a RAISED-CEILING zone under
    /// the middle x-span of an upper-storey wall run. Scaled onto the test
    /// grid: ground 8×4 with the front strip y∈[0,2] flat at 2.4 (the datum
    /// anchor), back strip y∈[2,4] at 3.04 with the raised zone x∈[2,6] at
    /// 3.49; first storey 8×4, slab 0.3 → datum 2.7 — so the first storey's
    /// y=4 wall is backed by band tops 0.34 / 0.79 / 0.34 storey-local, the
    /// field record's exact bottoms (real ceilings 2.548 / 2.998: the
    /// 0.45 m step is preserved verbatim).
    /// Any vertex whose edges double back (near-anti-parallel in plan) — the
    /// zero-width SPIKE artefact `strip_spike_vertices` exists to kill.
    fn has_spike(ring: &[Point3f64]) -> bool {
        let n = ring.len();
        (0..n).any(|i| {
            let (p, c, q) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
            let (ax, ay) = (c.x - p.x, c.y - p.y);
            let (bx, by) = (q.x - c.x, q.y - c.y);
            let (la, lb) = ((ax * ax + ay * ay).sqrt(), (bx * bx + by * by).sqrt());
            la > f64::EPSILON && lb > f64::EPSILON && (ax * bx + ay * by) / (la * lb) < -0.9
        })
    }

    /// 46 Hazelton Road, ground truth from the field record: main house and
    /// extension whose right edges sit ~4 mm apart (a drawn step the assessor
    /// tried to remove by TYPING a length — the residual jog), under an upper
    /// storey whose footprint edge runs near-coincident with both.
    fn hazelton_ber(ground_ceilings: CeilingRegions) -> Ber {
        let cid = ItemId::default();
        let main_ring = vec![
            Point2f64::new(1.4436, 13.8630),
            Point2f64::new(1.4436, 5.5020),
            Point2f64::new(6.3336, 5.5020),
            Point2f64::new(6.3336, 12.2620),
            Point2f64::new(3.0236, 12.2620),
            Point2f64::new(3.0236, 13.8630),
        ];
        let ext_ring = vec![
            Point2f64::new(1.6746, 5.5020),
            Point2f64::new(1.6746, -0.4400),
            Point2f64::new(6.3376, -0.4400),
            Point2f64::new(6.3358, 5.5020),
        ];
        let mut floors = FloorRegions::new(main_ring, cid);
        floors
            .regions
            .extend(FloorRegions::new(ext_ring, cid).regions);
        let ground = Storey {
            floors,
            ceilings: ground_ceilings,
            ceiling_height_m: Some(2.41),
            ..Storey::default()
        };
        let mut upper_floors = FloorRegions::new(square(1.4436, 5.5020, 6.3376, 12.2620), cid);
        upper_floors
            .regions
            .extend(FloorRegions::new(square(2.4936, 0.4580, 5.6136, 5.5020), cid).regions);
        let upper = Storey {
            floors: upper_floors,
            ceilings: CeilingRegions::default_for_footprint(
                vec![square(1.4436, 5.5020, 6.3376, 12.2620)],
                2.42,
                cid,
            ),
            ceiling_height_m: Some(2.42),
            floor_thickness_m: Some(0.3),
            ..Storey::default()
        };
        let mut ber = Ber::default();
        ber.storeys = vec![ground, upper];
        ber
    }

    /// The ground ceiling region under a plan point (any z).
    fn ceiling_at(ber: &Ber, x: f64, y: f64) -> &CeilingRegion {
        ber.storeys[0]
            .ceilings
            .regions
            .iter()
            .find(|r| {
                let ring: Vec<Point2f64> = r
                    .polygon
                    .exterior
                    .iter()
                    .map(|v| Point2f64::new(v.x, v.y))
                    .collect();
                ring_to_poly(&ring).contains(&GeoPoint::new(x, y))
            })
            .expect("a ceiling region covers the probe point")
    }

    #[test]
    fn hazelton_spiked_record_heals_on_re_derive() {
        // The corrupted region exactly as the field record stored it: the
        // main-house ceiling rectangle dragging a 6 m zero-width spike down
        // to the extension's front-right corner. It failed the containment
        // stamp (stayed Unset) and drew a stray roof line in every 3D view.
        let spiked = flat_ceiling(
            vec![
                Point2f64::new(1.4436, 12.2620),
                Point2f64::new(1.4436, 5.5020),
                Point2f64::new(6.3358, 5.5020),
                Point2f64::new(6.3376, -0.4400), // the spike tip
                Point2f64::new(6.3336, 12.2620),
            ],
            2.41,
        );
        let ceilings = CeilingRegions {
            regions: vec![
                flat_ceiling(square(1.4436, 12.2620, 3.0236, 13.8630), 2.262),
                flat_ceiling(square(2.4936, 0.4580, 5.6136, 5.5020), 2.41),
                flat_ceiling(
                    vec![
                        Point2f64::new(1.6746, 5.5020),
                        Point2f64::new(1.6746, -0.4400),
                        Point2f64::new(6.3376, -0.4400),
                        Point2f64::new(6.3358, 5.5020),
                        Point2f64::new(5.6136, 5.5020),
                        Point2f64::new(5.6136, 0.4580),
                        Point2f64::new(2.4936, 0.4580),
                        Point2f64::new(2.4936, 5.5020),
                    ],
                    2.41,
                ),
                spiked,
            ],
        };
        let mut ber = hazelton_ber(ceilings);
        derive_all(&mut ber);
        for r in &ber.storeys[0].ceilings.regions {
            assert!(
                !has_spike(&r.polygon.exterior),
                "spike survived the re-derive: {:?}",
                r.polygon.exterior
            );
        }
        // The healed main piece sits fully under the upper storey — it must
        // stamp InterStorey (it was prompted as a roof in the field).
        let main = ceiling_at(&ber, 4.0, 9.0);
        assert_eq!(main.exposure, CeilingExposure::InterStorey);
        assert_eq!(main.construction_id, ItemId::nil());
        // The extension's exposed frame (nothing above) must stay unstamped.
        assert_ne!(
            ceiling_at(&ber, 2.0, 2.5).exposure,
            CeilingExposure::InterStorey
        );
    }

    #[test]
    fn hazelton_jogged_edge_split_emits_no_spike() {
        // From-scratch derivation: ONE ceiling face over the jogged union
        // footprint, split live by the upper storey's near-coincident rings —
        // whatever debris the boolean op emits must never reach the record.
        let union_ring = vec![
            Point2f64::new(1.4436, 13.8630),
            Point2f64::new(1.4436, 5.5020),
            Point2f64::new(1.6746, 5.5020),
            Point2f64::new(1.6746, -0.4400),
            Point2f64::new(6.3376, -0.4400),
            Point2f64::new(6.3358, 5.5020),
            Point2f64::new(6.3336, 5.5020),
            Point2f64::new(6.3336, 12.2620),
            Point2f64::new(3.0236, 12.2620),
            Point2f64::new(3.0236, 13.8630),
        ];
        let ceilings = CeilingRegions {
            regions: vec![flat_ceiling(union_ring, 2.41)],
        };
        let mut ber = hazelton_ber(ceilings);
        derive_all(&mut ber);
        for r in &ber.storeys[0].ceilings.regions {
            assert!(
                !has_spike(&r.polygon.exterior),
                "boolean split emitted a spike: {:?}",
                r.polygon.exterior
            );
        }
        assert_eq!(
            ceiling_at(&ber, 4.0, 9.0).exposure,
            CeilingExposure::InterStorey,
            "the main-house ceiling sits fully under the first floor"
        );
        assert_eq!(
            ceiling_at(&ber, 4.0, 2.0).exposure,
            CeilingExposure::InterStorey,
            "the extension's covered middle sits under the first floor"
        );
        assert_ne!(
            ceiling_at(&ber, 2.0, 2.5).exposure,
            CeilingExposure::InterStorey,
            "the extension's exposed frame has nothing above it"
        );
    }

    fn elmwood_fixture() -> Ber {
        let mut ground = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, None);
        ground.ceilings = CeilingRegions {
            regions: vec![
                flat_ceiling(square(0.0, 0.0, 8.0, 2.0), 2.4),
                flat_ceiling(square(0.0, 2.0, 2.0, 4.0), 3.04),
                flat_ceiling(square(2.0, 2.0, 6.0, 4.0), 3.49),
                flat_ceiling(square(6.0, 2.0, 8.0, 4.0), 3.04),
            ],
        };
        let first = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, Some(0.3));
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];
        ber
    }

    /// THE target of the rewrite (user ruling 2026-07-25, Elmwood field
    /// report): the raised-ceiling zone under the middle of the wall run
    /// leaves exactly ONE exposed wall on that base line — its base tracing
    /// 0.34 → 0.79 → 0.34 — plus the per-run `InterStorey` bands. The old
    /// pass split it into THREE exposed strips (bottoms 0.34/0.79/0.34),
    /// scattering the run's four windows across three hosts.
    ///
    /// The four windows' RE-LINK goes through `document`'s
    /// `reanchor_openings` / `host_wall_for_rect` — that assertion belongs
    /// to Step 3's controller test (ber_model cannot reach `document` from
    /// here). THIS test asserts the ber_model half of the relink contract:
    /// ONE host spanning the FULL plan run (base frame length 8), whose
    /// underside query yields the right clamp at all four window stations —
    /// so every window's world rect has exactly one candidate host.
    #[test]
    fn elmwood_raised_ceiling_zone_leaves_one_stepped_wall() {
        let mut ber = elmwood_fixture();
        derive_all(&mut ber);
        let datums = ber.storey_floor_datums();
        assert!(
            (datums[1] - 2.7).abs() < 1e-9,
            "datum anchors on the front strip, got {datums:?}"
        );

        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        // Exactly ONE non-InterStorey wall on the base line.
        let exposed: Vec<_> = pieces
            .iter()
            .filter(|w| w.exposure != WallExposure::InterStorey)
            .collect();
        assert_eq!(exposed.len(), 1, "ONE exposed wall on the y=4 base line");
        let host = exposed[0];
        assert_eq!(host.exposure, WallExposure::Exposed);
        let (x0, x1) = x_extent(host);
        assert!(
            (x0 - 0.0).abs() < 1e-6 && (x1 - 8.0).abs() < 1e-6,
            "the host spans the whole run, got x [{x0}, {x1}]"
        );
        let (zmin, zmax) = z_extent(host);
        assert!((zmin - 0.34).abs() < 1e-6 && (zmax - 2.4).abs() < 1e-6);
        // The base run traces 0.34 → 0.79 → 0.34: both notch corners are
        // ring vertices…
        for (wx, wz) in [(2.0, 0.34), (2.0, 0.79), (6.0, 0.79), (6.0, 0.34)] {
            assert!(
                host.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - wx).abs() < 1e-6 && (v.z - wz).abs() < 1e-6),
                "host underside misses notch vertex ({wx}, {wz})"
            );
        }
        // …and the underside query reads the profile at the span midpoints
        // (stations symmetric in x, so the base direction doesn't matter).
        let (_, _, len, base_z) = host.base_frame().expect("full-span base frame");
        assert!(
            (len - 8.0).abs() < 1e-6,
            "base frame spans the whole run, got {len}"
        );
        assert!((base_z - 0.34).abs() < 1e-6);
        for (along, want) in [(1.0, 0.34), (4.0, 0.79), (7.0, 0.34)] {
            let z = host.base_z_at_along(along);
            assert!(
                (z - want).abs() < 1e-6,
                "base_z_at_along({along}) = {z}, want {want}"
            );
        }
        // Four window stations across the three spans: the ONE host takes
        // them all — headroom under the top at each, and the min-sill clamp
        // riding the notch over the raised zone (0.79 − 0.34 = 0.45 above
        // the frame's base_z).
        for (along, want_clamp) in [(1.0, 0.0), (3.0, 0.45), (5.0, 0.45), (7.0, 0.0)] {
            assert!(
                host.max_sill_for(along, 1.0) > 0.0,
                "a 1 m window fits at along {along}"
            );
            let clamp = host.min_sill_for(along);
            assert!(
                (clamp - want_clamp).abs() < 1e-6,
                "min_sill_for({along}) = {clamp}, want {want_clamp}"
            );
        }
        // Band pieces: per-run InterStorey, one per neighbour-ceiling span.
        let bands: Vec<_> = pieces
            .iter()
            .filter(|w| w.exposure == WallExposure::InterStorey)
            .collect();
        assert_eq!(bands.len(), 3, "per-run bands survive the one-clip pass");
        for w in &bands {
            let (bx0, bx1) = x_extent(w);
            let (bz0, bz1) = z_extent(w);
            let want_top = if bx0 >= 2.0 - 1e-6 && bx1 <= 6.0 + 1e-6 {
                0.79
            } else {
                0.34
            };
            assert!(
                (bz0 - 0.0).abs() < 1e-6 && (bz1 - want_top).abs() < 1e-6,
                "band x [{bx0}, {bx1}] z [{bz0}, {bz1}]"
            );
        }
        assert_no_degenerate_walls(&ber);

        // Double-derive: byte-stable, still sliver-free.
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        assert_eq!(
            serde_json::to_string(&ber.storeys).unwrap(),
            once,
            "double derive is byte-stable"
        );
        assert_no_degenerate_walls(&ber);
    }

    /// A neighbour footprint ending MID-WALL: the first storey's wall runs
    /// past the ground storey's footprint end (x=8 of 12). The OLD pass
    /// split the wall vertically at the run end; the exposed remainder is
    /// now ONE L-shaped region — raised bottom over the covered span,
    /// floor-level bottom past it — plus the single buried band.
    #[test]
    fn footprint_ending_mid_wall_leaves_one_l_shaped_exposed_region() {
        // Ground 8×4: anchor strip y∈[0,2] at 2.4, back strip y∈[2,4] at
        // 3.2 (local band top 0.5). First storey 12×4.
        let mut ground = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, None);
        ground.ceilings = CeilingRegions {
            regions: vec![
                flat_ceiling(square(0.0, 0.0, 8.0, 2.0), 2.4),
                flat_ceiling(square(0.0, 2.0, 8.0, 4.0), 3.2),
            ],
        };
        let first = flat_storey(square(0.0, 0.0, 12.0, 4.0), 2.4, Some(0.3));
        let mut ber = Ber::default();
        ber.storeys = vec![ground, first];
        derive_all(&mut ber);

        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        assert_eq!(
            pieces.len(),
            2,
            "band + ONE L-shaped remainder — no vertical cut at the run end"
        );
        let band = pieces
            .iter()
            .find(|w| w.exposure == WallExposure::InterStorey)
            .expect("the buried band");
        let (bx0, bx1) = x_extent(band);
        let (bz0, bz1) = z_extent(band);
        assert!((bx0 - 0.0).abs() < 1e-6 && (bx1 - 8.0).abs() < 1e-6);
        assert!((bz0 - 0.0).abs() < 1e-6 && (bz1 - 0.5).abs() < 1e-6);
        let rem = pieces
            .iter()
            .find(|w| w.exposure == WallExposure::Exposed)
            .expect("the L-shaped remainder");
        let (x0, x1) = x_extent(rem);
        assert!(
            (x0 - 0.0).abs() < 1e-6 && (x1 - 12.0).abs() < 1e-6,
            "one region over the whole run, got x [{x0}, {x1}]"
        );
        // The L: bottom 0.5 over the covered span, dropping to 0.0 at the
        // footprint end — the run-end step is part of ONE region's underside.
        for (wx, wz) in [(8.0, 0.5), (8.0, 0.0), (12.0, 0.0)] {
            assert!(
                rem.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - wx).abs() < 1e-6 && (v.z - wz).abs() < 1e-6),
                "L underside misses vertex ({wx}, {wz})"
            );
        }
        assert_no_degenerate_walls(&ber);
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        assert_eq!(serde_json::to_string(&ber.storeys).unwrap(), once);
        assert_no_degenerate_walls(&ber);
    }

    /// The user ruling on disconnection: a band fully crossing the wall
    /// face leaves TWO disconnected remainders — legitimately two regions
    /// (connected components, [`split_to_simple`]) — and with TWO crossings,
    /// three. The class rule still holds: every exposed component spans
    /// exactly between crossings, never split within itself.
    #[test]
    fn full_height_band_crossings_split_remainder_into_components() {
        // Ground 8×4: anchor strip at 2.4; back strip 3.04 (local 0.34)
        // except a tower zone x∈[3,5] topping ABOVE the first-storey wall
        // top (5.2 abs = 2.5 local > 2.4) — a full-height crossing.
        let stack = |tower_spans: &[(f64, f64)]| {
            let mut regions = vec![flat_ceiling(square(0.0, 0.0, 8.0, 2.0), 2.4)];
            let mut x = 0.0;
            for &(t0, t1) in tower_spans {
                if t0 > x {
                    regions.push(flat_ceiling(square(x, 2.0, t0, 4.0), 3.04));
                }
                regions.push(flat_ceiling(square(t0, 2.0, t1, 4.0), 5.2));
                x = t1;
            }
            if x < 8.0 {
                regions.push(flat_ceiling(square(x, 2.0, 8.0, 4.0), 3.04));
            }
            let mut ground = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, None);
            ground.ceilings = CeilingRegions { regions };
            let first = flat_storey(square(0.0, 0.0, 8.0, 4.0), 2.4, Some(0.3));
            let mut ber = Ber::default();
            ber.storeys = vec![ground, first];
            ber
        };

        // ONE crossing → TWO exposed components.
        let mut ber = stack(&[(3.0, 5.0)]);
        derive_all(&mut ber);
        let pieces = walls_on_y(&ber.storeys[1], 4.0);
        let exposed: Vec<_> = pieces
            .iter()
            .filter(|w| w.exposure == WallExposure::Exposed)
            .collect();
        assert_eq!(
            exposed.len(),
            2,
            "a full-height crossing → two exposed components"
        );
        for w in &exposed {
            let (x0, x1) = x_extent(w);
            let (zmin, zmax) = z_extent(w);
            assert!((zmin - 0.34).abs() < 1e-6 && (zmax - 2.4).abs() < 1e-6);
            let (want0, want1) = if x0 < 1e-6 { (0.0, 3.0) } else { (5.0, 8.0) };
            assert!(
                (x0 - want0).abs() < 1e-6 && (x1 - want1).abs() < 1e-6,
                "component x [{x0}, {x1}]"
            );
        }
        let bands: Vec<_> = pieces
            .iter()
            .filter(|w| w.exposure == WallExposure::InterStorey)
            .collect();
        assert_eq!(bands.len(), 3, "three per-run bands");
        assert!(
            bands.iter().any(|w| {
                let (z0, z1) = z_extent(w);
                z0.abs() < 1e-6 && (z1 - 2.4).abs() < 1e-6
            }),
            "the crossing band reaches the wall top"
        );
        assert_no_degenerate_walls(&ber);
        let once = serde_json::to_string(&ber.storeys).unwrap();
        derive_all(&mut ber);
        assert_eq!(serde_json::to_string(&ber.storeys).unwrap(), once);
        assert_no_degenerate_walls(&ber);

        // TWO crossings → THREE exposed components, five per-run bands.
        let mut ber2 = stack(&[(2.0, 3.0), (5.0, 6.0)]);
        derive_all(&mut ber2);
        let pieces2 = walls_on_y(&ber2.storeys[1], 4.0);
        assert_eq!(
            pieces2
                .iter()
                .filter(|w| w.exposure == WallExposure::Exposed)
                .count(),
            3,
            "two crossings → three exposed components"
        );
        assert_eq!(
            pieces2
                .iter()
                .filter(|w| w.exposure == WallExposure::InterStorey)
                .count(),
            5,
            "five per-run bands"
        );
        assert_no_degenerate_walls(&ber2);
    }
}
