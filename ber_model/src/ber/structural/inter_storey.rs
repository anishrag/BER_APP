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
const CONTAINMENT_TOL_M: f64 = 0.05;

/// How far INSIDE the neighbour footprint the band-top plan sample is nudged
/// off the wall base line (m). Falls back to a tenth of this when the
/// footprint is too thin at the full nudge.
const SAMPLE_NUDGE_M: f64 = 0.05;

/// Recursion cap for the wall worklist — a wall is never genuinely divided
/// into anywhere near this many sub-ranges; the cap only guards against a
/// pathological non-terminating split.
const MAX_WALL_SPLIT_DEPTH: usize = 16;

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
///   neighbour's backing band in absolute z —
///   `[datum_j − slab_j, top_z_j(p)]` where `slab_j` is the neighbour's
///   `floor_thickness_m` and `top_z_j(p)` its ceiling surface
///   ([`CeilingRegions::top_z_at`]) over a plan sample just inside its
///   footprint. Non-degenerate overlap → split vertically at the sub-range
///   ends ([`split_wall_by_segment`] semantics, [`MIN_SUBWALL_M`] guard) and
///   horizontally at the band bounds ([`split_wall_by_line`], with a
///   [`MIN_SUBWALL_M`]-sized absolute-z sliver guard so re-derivation over
///   non-regenerated walls never shaves a sliver); stamp covered pieces.
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
        for r in &mut storey.floors.regions {
            let ring = floor_plan_ring(r);
            if ring.len() >= 3 && fp.contains_ring(&ring) {
                r.exposure = FloorExposure::InterStorey;
                // An inter-storey surface is OUT of structure scope: it is
                // never attributed, offered, reviewed or counted — so it
                // must not keep (or share) a construction. Clearing here
                // SELF-HEALS records where a pre-scope-fix session left an
                // in-scope region sharing its construction with a buried
                // one (the "final house" report). Stable nil id, so the
                // derive stays exactly idempotent.
                r.construction_id = ItemId::nil();
            }
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
            if ring.len() >= 3 && fp.contains_ring(&ring) {
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
                datum: datums[j],
                band_lo: datums[j] - slabs[j],
            };
            let walls = std::mem::take(&mut ber.storeys[i].walls.regions);
            let mut out = Vec::with_capacity(walls.len());
            for wall in walls {
                process_wall(wall, &nb, datums[i], 0, &mut out);
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
struct PlanFootprint {
    /// Open rings (no closing duplicate), one per union piece.
    rings: Vec<Vec<Point2f64>>,
    polys: Vec<Polygon<f64>>,
}

impl PlanFootprint {
    fn of(floors: &FloorRegions) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let mut acc = MultiPolygon::<f64>(Vec::new());
        for r in &floors.regions {
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

    fn is_empty(&self) -> bool {
        self.rings.is_empty()
    }

    /// `p` strictly inside any footprint piece (boundary excluded).
    fn contains(&self, p: Point2f64) -> bool {
        let gp = GeoPoint::new(p.x, p.y);
        self.polys.iter().any(|poly| poly.contains(&gp))
    }

    /// `p` inside any piece, or within `tol` of any piece's boundary — the
    /// "on/inside the footprint" test for a wall base sample.
    fn contains_within(&self, p: Point2f64, tol: f64) -> bool {
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
    fn contains_ring(&self, ring: &[Point2f64]) -> bool {
        self.rings
            .iter()
            .any(|outer| ring_within_ring(ring, outer, CONTAINMENT_TOL_M))
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
fn split_floors_intact(floors: FloorRegions, rings: &[Vec<Point2f64>]) -> FloorRegions {
    use geo::bool_ops::BooleanOps;
    use geo::Area;
    let mut regions = floors.regions;
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let cut = ring_to_poly(ring);
        let mut next: Vec<FloorRegion> = Vec::new();
        for region in regions {
            let inside = region.polygon.intersection(&cut);
            let inside_area: f64 = inside.0.iter().map(|p| p.unsigned_area()).sum();
            if inside_area < SLIVER_AREA_M2 {
                next.push(region); // cut misses this region — keep it intact
                continue;
            }
            let outside = region.polygon.difference(&cut);
            let outside_area: f64 = outside.0.iter().map(|p| p.unsigned_area()).sum();
            if outside_area < SLIVER_AREA_M2 {
                next.push(region); // cut fully covers it — keep it intact
                continue;
            }
            let mk = |poly: &Polygon<f64>| FloorRegion {
                id: ItemId::default(),
                polygon: poly.clone(),
                construction_id: region.construction_id,
                level_m: region.level_m,
                exposure: region.exposure,
            };
            for poly in inside.0.iter().chain(outside.0.iter()) {
                if poly.unsigned_area() >= SLIVER_AREA_M2 {
                    next.push(mk(poly));
                }
            }
        }
        regions = next;
    }
    FloorRegions { regions }
}

/// A floor region's exterior plan ring, open (closing duplicate dropped).
fn floor_plan_ring(r: &FloorRegion) -> Vec<Point2f64> {
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
    use geo::bool_ops::BooleanOps;
    use geo::Area;
    const Z_EPS_M: f64 = 1e-6;

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
                let buried_cid = if ra.exposure == CeilingExposure::InterStorey {
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
                let a_is = ra.exposure == CeilingExposure::InterStorey;
                let b_is = rb.exposure == CeilingExposure::InterStorey;
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
    /// The neighbour's floor datum (absolute z).
    datum: f64,
    /// Absolute z of the neighbour volume's underside: `datum − slab`.
    band_lo: f64,
}

impl NeighbourBand<'_> {
    /// Absolute band-top z over the wall-base sub-range midpoint `m`
    /// (`nrm` = the base's unit plan normal): the neighbour ceiling surface
    /// sampled just inside its footprint on each side of the base line, the
    /// HIGHER side winning (a wall on a step line is backed by the taller
    /// neighbour volume beside it). `None` when no sample lands inside a
    /// ceiling-covered part of the footprint.
    fn band_top_at(&self, m: Point2f64, nrm: (f64, f64)) -> Option<f64> {
        let mut best: Option<f64> = None;
        for s in [1.0, -1.0] {
            let z = [SAMPLE_NUDGE_M, SAMPLE_NUDGE_M * 0.1].iter().find_map(|d| {
                let p = Point2f64::new(m.x + s * nrm.0 * d, m.y + s * nrm.1 * d);
                if !self.fp.contains(p) {
                    return None;
                }
                self.ceilings.top_z_at(p).map(|z| self.datum + z)
            });
            if let Some(z) = z {
                best = Some(best.map_or(z, |b: f64| b.max(z)));
            }
        }
        best
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

/// Process one wall (of the storey at floor datum `own_datum`, storey-local
/// z) against a neighbour band: find the along-base sub-ranges inside the
/// neighbour footprint whose z-band overlap is real, split vertically at
/// sub-range ends and horizontally at the band bounds, stamp buried pieces
/// `InterStorey`, and push the resulting piece(s) onto `out` in place of the
/// wall. A wall the band misses (or one with no usable base frame — e.g. the
/// upper remainder of a previous horizontal cut, whose ring opens on the cut
/// line) passes through UNTOUCHED — same id, same polygon — which is what
/// makes re-derivation exact on walls that are not regenerated every
/// materialize.
fn process_wall(
    wall: WallRegion,
    nb: &NeighbourBand,
    own_datum: f64,
    depth: usize,
    out: &mut Vec<WallRegion>,
) {
    if depth > MAX_WALL_SPLIT_DEPTH {
        log::warn!("inter-storey wall split exceeded its depth cap — piece left unsplit");
        out.push(wall);
        return;
    }
    let (Some((_origin, (ux, uy), len, _bz)), Some((a3, b3))) =
        (wall.base_frame(), wall.base_edge())
    else {
        out.push(wall);
        return;
    };
    let (a, b) = (Point2f64::new(a3.x, a3.y), Point2f64::new(b3.x, b3.y));
    let nrm = (-uy, ux);
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
    ts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));

    // Maximal along-base runs whose midpoint lies on/inside the footprint,
    // each carrying ITS OWN band top (sampled at its midpoint — now always
    // strictly between two neighbour boundaries — nudged off the base line
    // by `band_top_at`'s two-side probe). Contiguous runs sharing one band
    // top merge back, so a neighbour-ceiling seam that does NOT change the
    // band (a coplanar carve, a phase split) never splits the wall.
    struct Run {
        t0: f64,
        t1: f64,
        band_top: f64, // ABSOLUTE z
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
        let Some(top) = nb.band_top_at(mid, nrm) else {
            continue;
        };
        match runs.last_mut() {
            // Contiguous with the previous run (only sub-µm noise spans
            // between them) at the SAME band top — extend it.
            Some(r) if (t0 - r.t1) * len <= 1e-6 && (r.band_top - top).abs() <= RISER_Z_EPS_M => {
                r.t1 = t1
            }
            _ => runs.push(Run {
                t0,
                t1,
                band_top: top,
            }),
        }
    }

    // Keep only runs whose z-band overlap with the wall is real (this is
    // what stops a neighbour footprint edge crossing a wall base from
    // splitting a wall the band never touches).
    let (zmin, zmax) = z_extent(&wall);
    struct Active {
        t0: f64,
        t1: f64,
        band_lo: f64,
        band_top: f64, // storey-LOCAL z
    }
    let mut active: Vec<Active> = Vec::new();
    for r in &runs {
        let (t0, t1) = (r.t0, r.t1);
        if (t1 - t0) * len < MIN_SUBWALL_M {
            continue; // sliver guard along the base
        }
        let band_lo = nb.band_lo - own_datum;
        let band_top = r.band_top - own_datum;
        // The wall's top over the run, sampled at quarter points (never a
        // breakpoint) — enough for the overlap gate; the exact geometry is
        // cut by `split_by_line`, not by these samples.
        let ztop_run = [0.25, 0.5, 0.75]
            .iter()
            .map(|f| wall.top_z_at_along((t0 + (t1 - t0) * f) * len))
            .fold(f64::NEG_INFINITY, f64::max);
        let overlap = band_top.min(ztop_run) - band_lo.max(zmin);
        if overlap <= MIN_SUBWALL_M {
            continue; // degenerate band — the neighbour only grazes this wall
        }
        active.push(Active {
            t0,
            t1,
            band_lo,
            band_top,
        });
    }
    let Some(run) = active.first() else {
        out.push(wall);
        return;
    };

    // Vertical split first: if the active run covers only part of the base,
    // cut at its interior end and recurse on both halves (each re-derives its
    // own runs). `split_wall_by_segment` carries the MIN_SUBWALL_M graze
    // guard, so a run end within a sliver of the wall end never re-splits —
    // the idempotence guard along the base.
    let guard_t = MIN_SUBWALL_M / len;
    let cut_t = if run.t0 > guard_t {
        Some(run.t0)
    } else if run.t1 < 1.0 - guard_t {
        Some(run.t1)
    } else {
        None
    };
    if let Some(t) = cut_t {
        let p = at(t);
        let cut = [
            Point2f64::new(p.x - nrm.0, p.y - nrm.1),
            Point2f64::new(p.x + nrm.0, p.y + nrm.1),
        ];
        if let Some((h0, h1)) = split_wall_by_segment(&wall, &cut) {
            process_wall(h0, nb, own_datum, depth + 1, out);
            process_wall(h1, nb, own_datum, depth + 1, out);
            return;
        }
        // Graze — fall through and treat the whole wall against this band.
    }

    // Horizontal split at the band bounds (only where a bound strictly
    // bisects, with the absolute-z sliver guard for idempotence), then stamp
    // the pieces buried inside the band.
    let (band_lo, band_top) = (run.band_lo, run.band_top);
    let cuts: Vec<f64> = [band_lo, band_top]
        .into_iter()
        .filter(|zc| zc - zmin > MIN_SUBWALL_M && zmax - zc > MIN_SUBWALL_M)
        .collect();
    let mut pieces = vec![wall];
    for zc in cuts {
        let mut next: Vec<WallRegion> = Vec::new();
        for piece in pieces {
            let (pmin, pmax) = z_extent(&piece);
            if zc - pmin > MIN_SUBWALL_M && pmax - zc > MIN_SUBWALL_M {
                // Two points on the wall plane at the cut height — the base
                // endpoints lifted to zc (valid for every piece: they all
                // share the parent's vertical plane).
                let p0 = Point3f64::new(a.x, a.y, zc);
                let p1 = Point3f64::new(b.x, b.y, zc);
                // Rebase each piece's ring onto the vertex-order contract:
                // the UPPER piece's raw ring opens on the cut line (no base
                // frame), which would make it unable to host an opening or
                // recover its painted span — `rebase_ring_start` gives it a
                // genuine base edge along the cut at `zc`, so the exposed
                // remainder above a buried band is a first-class wall.
                next.extend(
                    split_wall_by_line(&piece, p0, p1)
                        .into_iter()
                        .map(rebase_ring_start),
                );
            } else {
                next.push(piece);
            }
        }
        pieces = next;
    }
    for mut piece in pieces {
        let (pmin, pmax) = z_extent(&piece);
        if pmin >= band_lo - MIN_SUBWALL_M && pmax <= band_top + MIN_SUBWALL_M {
            piece.exposure = WallExposure::InterStorey;
        }
        out.push(piece);
    }
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
            if zmax < 1.3 + 1e-6 {
                assert!((zmin - 0.0).abs() < 1e-9 && (zmax - 1.3).abs() < 1e-9);
                lower = Some(w);
            } else {
                assert!((zmin - 1.3).abs() < 1e-9 && (zmax - 2.4).abs() < 1e-9);
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

    /// The horizontal split's UPPER piece (the exposed remainder above the
    /// buried band) is a FIRST-CLASS wall: its ring is rebased onto the
    /// vertex-order contract, so `base_frame()` yields the full plan span at
    /// the cut height — it can host openings, recover its painted span, and
    /// clamp sills at the real wall top. (The raw `split_by_line` ring opens
    /// on the cut line and degenerates `base_edge`.)
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
        assert!(
            (base_z - 1.3).abs() < 1e-9,
            "base sits at the cut height (storey-local 1.3), got {base_z}"
        );
        assert!((len - 4.0).abs() < 1e-9, "full plan span, got {len}");
        assert!((origin.x - 4.0).abs() < 1e-9);
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

    /// The rotated fixture: the neighbour's ceiling height varies ALONG the
    /// wall, so the wall must split along its LENGTH at the neighbour's step
    /// line, each side getting its own band top — one midpoint sample per
    /// footprint run would smear one section's band over the other.
    #[test]
    fn rotated_step_splits_wall_along_length_with_per_side_band_tops() {
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
            4,
            "step-line-perpendicular wall → 2 length pieces × 2 bands"
        );
        // Classify by x-side and z-band; assert each quadrant exactly once.
        let mut seen = [false; 4];
        for w in &pieces {
            let (x0, x1) = x_extent(w);
            let (zmin, zmax) = z_extent(w);
            let top = if x1 <= 4.0 + 1e-6 { low_top } else { tall_top };
            assert!(
                (x0 - if x1 <= 4.0 + 1e-6 { 0.0 } else { 4.0 }).abs() < 1e-6,
                "piece spans one side of the step line, got x [{x0}, {x1}]"
            );
            let quadrant = match (x1 <= 4.0 + 1e-6, zmax <= top + 1e-6) {
                (true, true) => 0,   // low side, buried band
                (true, false) => 1,  // low side, remainder
                (false, true) => 2,  // tall side, buried band
                (false, false) => 3, // tall side, remainder
            };
            assert!(!seen[quadrant], "duplicate piece in quadrant {quadrant}");
            seen[quadrant] = true;
            if zmax <= top + 1e-6 {
                // Buried band: floor up to EXACTLY this side's band top.
                assert!((zmin - 0.0).abs() < 1e-9 && (zmax - top).abs() < 1e-9);
                assert_eq!(w.exposure, WallExposure::InterStorey);
            } else {
                // Remainder: from EXACTLY the band top to the wall top — no
                // Unset sliver between the band and the remainder.
                assert!((zmin - top).abs() < 1e-9 && (zmax - 2.4).abs() < 1e-9);
                assert_eq!(w.exposure, WallExposure::Exposed);
            }
        }
        assert!(seen.iter().all(|s| *s), "all four quadrants present");
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
        assert_eq!(p.len(), 4, "the wall split in the plain orientation");
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
}
