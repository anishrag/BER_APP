//! Porch inclusion/exclusion — the DEAP Table 2.2 derivation.
//!
//! DEAP (Guide 4.3.1, *fabric/dimensions/porches*) excludes a porch from the
//! assessment ONLY when all three hold: it is UNHEATED (no fixed heating
//! device), SEPARATED from the dwelling by a door, and protrudes ENTIRELY
//! beyond the line of the external wall. A corner / within-line / partially-
//! within porch is always included, as is any porch with direct open access.
//!
//! The three facts are DERIVED from the captured record — never entered as an
//! office-side opinion:
//!  - *separating door*: a recorded [`Door`] anchored to the porch's inner
//!    wall ([`WallAnchor::porch`]);
//!  - *heated*: a fixed heat EMITTER object pinned inside the porch region
//!    (radiator / panel heater / open chimney — a boiler pin does NOT heat
//!    the space it stands in, per DEAP), OR its absence CONFIRMED on site
//!    ([`PorchCheck::heating_confirmed_absent`] — absence of a pin alone
//!    proves nothing: underfloor heating has no pinnable object);
//!  - *fully protruding*: plan geometry (below).
//!
//! Incomplete evidence → [`PorchVerdict::Undecided`], which blocks the DEAP
//! export exactly like an `Unset` exposure.
//!
//! [`derive_porch_exclusions`] applies the verdicts to the derived geometry,
//! mirroring [`derive_inter_storey`](super::derive_inter_storey)'s contract
//! (pass-0 self-retraction, fixpoint-stable splits, derived-only stamps):
//! an EXCLUDED porch's own floor/ceiling/outer-wall pieces are split out and
//! stamped `Irrelevant` (outside the assessment boundary), its inner
//! `porch_walls` become `SemiExposed` (the new heat-loss envelope), and a
//! storey above it has its overlapping floor re-stamped `SemiExposed`
//! (over an unheated space) instead of `InterStorey`. An INCLUDED porch's
//! inner walls become `NonHeatLoss` (an internal partition).

use geo::{Contains, Point as GeoPoint};
use serde::{Deserialize, Serialize};

use super::inter_storey::{
    coalesce_stamped_ceiling_cuts, floor_plan_ring, split_floors_intact, PlanFootprint,
    CONTAINMENT_TOL_M,
};
use super::{
    CeilingExposure, ConstructionMakeup, FloorConstructionType, FloorExposure, FloorRegion,
    FloorRegions, WallConstructionType, WallExposure,
};
use crate::ber::ber_struct::Ber;
use crate::ber::common::ItemId;
use crate::ber::fenestration::PorchKey;
use crate::ber::objects::{ChimneyState, ObjectKind};
use crate::ber::structural::Storey;
use domain::points::{point_to_segment_distance, Point2f64};

/// The mm-quantized area-weighted centroid key of a plan RING (open or
/// closed) — THE durable porch identity ([`PorchKey`]). Canonical single
/// source: `document::phase::ring_centroid_key` delegates here, so a key
/// stamped at resolve (from the slot ring) and one computed Ber-side (from
/// the round-tripped `Storey.porches` region ring) always agree. Degenerate
/// rings (< 3 distinct points / zero area) fall back to the vertex mean.
pub fn ring_centroid_key(ring: &[Point2f64]) -> PorchKey {
    let n = ring.len();
    if n == 0 {
        return (0, 0);
    }
    let mut a = 0.0;
    let mut cx = 0.0;
    let mut cy = 0.0;
    for i in 0..n {
        let p = ring[i];
        let q = ring[(i + 1) % n];
        let cross = p.x * q.y - q.x * p.y;
        a += cross;
        cx += (p.x + q.x) * cross;
        cy += (p.y + q.y) * cross;
    }
    let c = if a.abs() > 1e-12 {
        Point2f64::new(cx / (3.0 * a), cy / (3.0 * a))
    } else {
        let sx: f64 = ring.iter().map(|p| p.x).sum();
        let sy: f64 = ring.iter().map(|p| p.y).sum();
        Point2f64::new(sx / n as f64, sy / n as f64)
    };
    let q = |v: f64| (v * 1000.0).round() as i64;
    (q(c.x), q(c.y))
}

/// One porch's on-site CONFIRMED-ABSENT answers — the capture-side record
/// behind the two evidence-from-absence facts. Only NEGATIVES are ever
/// stored: a positive answer is recorded as the real object/opening it
/// asserts (pin the radiator, record the door), so the record can never hold
/// a boolean contradicting its own geometry. Keyed by [`PorchKey`]; a check
/// whose porch was deleted is simply ignored (and healed by containment on
/// lookup after a re-dimension, like the opening anchors).
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PorchCheck {
    /// The porch this check answers — its mm-quantized ring-centroid key.
    pub key: PorchKey,
    /// Surveyor confirmed NO fixed heating in the porch (asked because no
    /// heat-emitter pin proves nothing — underfloor heating has no pin).
    #[serde(default)]
    pub heating_confirmed_absent: bool,
    /// Surveyor confirmed NO separating door between dwelling and porch.
    #[serde(default)]
    pub door_confirmed_absent: bool,
}

/// The DEAP Table 2.2 verdict for one porch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PorchVerdict {
    /// Evidence incomplete — blocks the DEAP export like an `Unset` exposure.
    Undecided,
    /// Part of the dwelling: heated, or open to it, or not fully beyond the
    /// external-wall line.
    Included,
    /// Thermally separated, unheated, fully protruding — out of the
    /// assessment; the dwelling-side boundary becomes the (semi-exposed)
    /// heat-loss envelope.
    Excluded,
}

/// The three Table 2.2 facts for one porch, each `None` while the record
/// holds neither positive evidence nor an on-site confirmation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PorchFacts {
    /// The porch's durable identity (current ring key).
    pub key: PorchKey,
    /// `Some(true)`: a fixed heat emitter is pinned inside the porch.
    /// `Some(false)`: absence confirmed on site. `None`: unanswered.
    pub heated: Option<bool>,
    /// `Some(true)`: a door is recorded on the porch's inner wall.
    /// `Some(false)`: absence confirmed on site. `None`: unanswered.
    pub separating_door: Option<bool>,
    /// Plan geometry: the porch protrudes ENTIRELY beyond the line of the
    /// external wall (one straight dwelling-side boundary, with the whole
    /// dwelling remainder behind its line).
    pub fully_protruding: bool,
}

impl PorchFacts {
    /// DEAP Table 2.2. Order matters: a not-fully-protruding porch is
    /// included regardless of anything else; an open (no-door) porch is
    /// included whether heated or not; a heated porch is included whether
    /// separated or not. Only unheated + door + protruding excludes.
    pub fn verdict(&self) -> PorchVerdict {
        if !self.fully_protruding {
            return PorchVerdict::Included;
        }
        match (self.heated, self.separating_door) {
            (Some(true), _) => PorchVerdict::Included,
            (_, Some(false)) => PorchVerdict::Included,
            (Some(false), Some(true)) => PorchVerdict::Excluded,
            _ => PorchVerdict::Undecided,
        }
    }
}

/// Whether a recorded object is a fixed heat EMITTER for the porch-heated
/// test. Radiators and panel heaters heat the space they stand in; an OPEN
/// chimney is a usable fireplace. Boilers/programmers/cylinders do NOT make
/// the space heated (DEAP: a boiler in an unheated garage needs no special
/// treatment), and underfloor heating has no object at all — that absence is
/// exactly what [`PorchCheck::heating_confirmed_absent`] answers.
fn is_heat_emitter(kind: &ObjectKind) -> bool {
    matches!(
        kind,
        ObjectKind::Radiator(_) | ObjectKind::PanelHeater | ObjectKind::Chimney(ChimneyState::Open)
    )
}

/// The porch's open exterior plan ring.
pub fn porch_ring(porch: &FloorRegion) -> Vec<Point2f64> {
    floor_plan_ring(porch)
}

/// This storey's [`PorchCheck`] for the porch with ring `ring` / current key
/// `key`: exact key match first, then the containment heal (a stored key
/// point inside the current ring — a re-dimension drifted the centroid),
/// mirroring the opening-anchor heal in `document`.
pub fn porch_check_for<'a>(
    storey: &'a Storey,
    ring: &[Point2f64],
    key: PorchKey,
) -> Option<&'a PorchCheck> {
    if let Some(c) = storey.porch_checks.iter().find(|c| c.key == key) {
        return Some(c);
    }
    let poly = super::ring_to_poly(ring);
    storey.porch_checks.iter().find(|c| {
        let p = GeoPoint::new(c.key.0 as f64 / 1000.0, c.key.1 as f64 / 1000.0);
        poly.contains(&p)
    })
}

/// Merge the porch ring's edges into maximal COLLINEAR runs classified
/// against the storey footprint boundary: `(flush, run)` — `flush` = the run
/// lies ALONG the dwelling outline (the porch's outdoor faces), else it is a
/// dwelling-side INNER boundary. Runs preserve ring order.
fn classified_edge_runs(
    ring: &[Point2f64],
    footprint: &PlanFootprint,
) -> Vec<(bool, Vec<[Point2f64; 2]>)> {
    let n = ring.len();
    let mut edges: Vec<(bool, [Point2f64; 2])> = Vec::with_capacity(n);
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let mid = Point2f64::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
        let flush = footprint.rings.iter().any(|outer| {
            let m = outer.len();
            (0..m).any(|j| {
                let (p, q) = (outer[j], outer[(j + 1) % m]);
                point_to_segment_distance(mid.x, mid.y, p.x, p.y, q.x, q.y) <= CONTAINMENT_TOL_M
            })
        });
        edges.push((flush, [a, b]));
    }
    // Fold consecutive same-class collinear edges into runs (ring-circular:
    // rotate so index 0 starts a run, then sweep once).
    let collinear = |e1: &[Point2f64; 2], e2: &[Point2f64; 2]| {
        let d1 = Point2f64::new(e1[1].x - e1[0].x, e1[1].y - e1[0].y);
        let d2 = Point2f64::new(e2[1].x - e2[0].x, e2[1].y - e2[0].y);
        (d1.x * d2.y - d1.y * d2.x).abs() <= 1e-6 * (d1.x.hypot(d1.y) * d2.x.hypot(d2.y)).max(1e-12)
    };
    let start = (0..n)
        .find(|&i| {
            let prev = (i + n - 1) % n;
            edges[prev].0 != edges[i].0 || !collinear(&edges[prev].1, &edges[i].1)
        })
        .unwrap_or(0);
    let mut runs: Vec<(bool, Vec<[Point2f64; 2]>)> = Vec::new();
    for k in 0..n {
        let (flush, e) = edges[(start + k) % n];
        match runs.last_mut() {
            Some((f, run)) if *f == flush && collinear(run.last().unwrap(), &e) => run.push(e),
            _ => runs.push((flush, vec![e])),
        }
    }
    runs
}

/// The porch protrudes ENTIRELY beyond the line of the external wall
/// (DEAP Figure 4.17) iff it has exactly ONE inner (dwelling-side) boundary
/// run, that run is straight, and the WHOLE dwelling remainder sits on the
/// far side of its line (within [`CONTAINMENT_TOL_M`] — points ON the line
/// are the continuing front wall). Corner porches (two inner runs,
/// Fig 4.18), within-line lobbies (Fig 4.16) and partially-within porches
/// (L-shaped inner boundary, Fig 4.19) all fail this and stay included.
fn fully_protruding(storey: &Storey, ring: &[Point2f64], footprint: &PlanFootprint) -> bool {
    let runs = classified_edge_runs(ring, footprint);
    let inner: Vec<&Vec<[Point2f64; 2]>> = runs
        .iter()
        .filter(|(flush, _)| !flush)
        .map(|(_, r)| r)
        .collect();
    if inner.len() != 1 {
        return false;
    }
    // The single straight inner boundary: its infinite line, porch on one side.
    let run = inner[0];
    let a = run.first().unwrap()[0];
    let b = run.last().unwrap()[1];
    let d = Point2f64::new(b.x - a.x, b.y - a.y);
    let len = d.x.hypot(d.y);
    if len < 1e-9 {
        return false;
    }
    let side = |p: Point2f64| ((p.x - a.x) * d.y - (p.y - a.y) * d.x) / len;
    let porch_poly = super::ring_to_poly(ring);
    let centre = ring_centroid_key(ring);
    let porch_side = side(Point2f64::new(
        centre.0 as f64 / 1000.0,
        centre.1 as f64 / 1000.0,
    ));
    if porch_side.abs() <= CONTAINMENT_TOL_M {
        return false; // degenerate porch — centroid on its own boundary line
    }
    // Every floors vertex outside the porch must NOT sit on the porch's side
    // of the line (the dwelling remainder stays behind the wall line).
    for r in &storey.floors.regions {
        for v in floor_plan_ring(r) {
            let s = side(v);
            if s.abs() <= CONTAINMENT_TOL_M || s.signum() != porch_side.signum() {
                continue;
            }
            let gp = GeoPoint::new(v.x, v.y);
            if !porch_poly.contains(&gp) && !point_on_ring(ring, v, CONTAINMENT_TOL_M) {
                return false;
            }
        }
    }
    true
}

/// `p` within `tol` of any segment of `ring`.
fn point_on_ring(ring: &[Point2f64], p: Point2f64, tol: f64) -> bool {
    let n = ring.len();
    (0..n).any(|i| {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        point_to_segment_distance(p.x, p.y, a.x, a.y, b.x, b.y) <= tol
    })
}

/// Derive the three Table 2.2 facts for one porch of `storey` from the
/// captured record (pins, door anchors, on-site checks, plan geometry).
pub fn porch_facts(storey: &Storey, porch: &FloorRegion) -> PorchFacts {
    let ring = porch_ring(porch);
    let key = ring_centroid_key(&ring);
    let footprint = PlanFootprint::of(&storey.floors);
    let poly = super::ring_to_poly(&ring);

    // Heated: a fixed heat emitter pinned inside the porch region.
    let emitter_pinned = storey.object_rooms.iter().any(|room| {
        if room.on_roof {
            return false;
        }
        let p = GeoPoint::new(room.plan_position.x as f64, room.plan_position.y as f64);
        poly.contains(&p) && room.objects.iter().any(|o| is_heat_emitter(&o.kind))
    });
    // Separating door: a Door anchored to THIS porch's inner wall — exact key
    // match, or the anchor's key point inside the current ring (the same
    // containment heal the resolve-side anchor lookup uses).
    let door_recorded = storey.doors.iter().any(|d| {
        d.wall_anchor
            .as_ref()
            .and_then(|a| a.porch)
            .is_some_and(|k| {
                k == key || poly.contains(&GeoPoint::new(k.0 as f64 / 1000.0, k.1 as f64 / 1000.0))
            })
    });
    let check = porch_check_for(storey, &ring, key);
    let heated = if emitter_pinned {
        Some(true)
    } else if check.is_some_and(|c| c.heating_confirmed_absent) {
        Some(false)
    } else {
        None
    };
    let separating_door = if door_recorded {
        Some(true)
    } else if check.is_some_and(|c| c.door_confirmed_absent) {
        Some(false)
    } else {
        None
    };
    PorchFacts {
        key,
        heated,
        separating_door,
        fully_protruding: fully_protruding(storey, &ring, &footprint),
    }
}

/// Facts + verdict for every porch on every storey — the read-only query the
/// capture UI (pending checks) and export gate consume. Entries are
/// `(storey index, facts, verdict)` in porch order.
pub fn porch_verdicts(ber: &Ber) -> Vec<(usize, PorchFacts, PorchVerdict)> {
    let mut out = Vec::new();
    for (i, s) in ber.storeys.iter().enumerate() {
        for porch in &s.porches.regions {
            let facts = porch_facts(s, porch);
            let verdict = facts.verdict();
            out.push((i, facts, verdict));
        }
    }
    out
}

/// Apply the porch verdicts to the derived geometry. Runs AFTER
/// [`derive_inter_storey`](super::derive_inter_storey) on every materialize,
/// with the same `dimensioned` gate and the same self-retraction contract:
/// pass 0 reverses every prior porch stamp (and coalesces the prior ceiling
/// cuts — ceilings are regions-as-truth, so a stale cut would otherwise
/// accumulate), then the per-porch stamps re-derive from the CURRENT
/// verdicts. All stamps are derived-only; nothing here is assessor-set.
pub fn derive_porch_exclusions(ber: &mut Ber, dimensioned: &[bool]) {
    let dim = |i: usize| dimensioned.get(i).copied().unwrap_or(false);

    // ── Pass 0 — self-retraction on every storey, gate-independent ────────
    for storey in &mut ber.storeys {
        for r in &mut storey.walls.regions {
            if r.exposure == WallExposure::Irrelevant {
                r.exposure = WallExposure::Unset;
            }
        }
        for r in &mut storey.floors.regions {
            if r.exposure == FloorExposure::Irrelevant {
                r.exposure = FloorExposure::Unset;
            }
        }
        // Porch inner walls are ONLY ever stamped here (resolve re-mints them
        // `Unset`) — reset them all so an undecided porch reads `Unset`.
        for r in &mut storey.porch_walls.regions {
            r.exposure = WallExposure::Unset;
        }
        storey.ceilings = coalesce_stamped_ceiling_cuts(
            std::mem::take(&mut storey.ceilings),
            CeilingExposure::Irrelevant,
        );
        for r in &mut storey.ceilings.regions {
            if r.exposure == CeilingExposure::Irrelevant {
                r.exposure = CeilingExposure::Unset;
            }
        }
    }

    let n = ber.storeys.len();
    for i in 0..n {
        if !dim(i) {
            continue; // un-dimensioned geometry is still moving — don't stamp
        }
        // Verdicts first (immutable), then stamp (mutable).
        let porches: Vec<(Vec<Point2f64>, PorchVerdict)> = ber.storeys[i]
            .porches
            .regions
            .iter()
            .map(|p| {
                let facts = porch_facts(&ber.storeys[i], p);
                (porch_ring(p), facts.verdict())
            })
            .collect();
        for (ring, verdict) in &porches {
            if ring.len() < 3 {
                continue;
            }
            // Inner porch walls: the porch's dwelling-side boundary. Excluded
            // → the new heat-loss envelope (semi-exposed, DEAP 1/U = 1/Uo+Ru);
            // included → an internal partition (no heat loss). Undecided →
            // left `Unset` by pass 0, which blocks export.
            let inner_exposure = match verdict {
                PorchVerdict::Excluded => WallExposure::SemiExposed,
                PorchVerdict::Included => WallExposure::NonHeatLoss,
                PorchVerdict::Undecided => continue,
            };
            for r in &mut ber.storeys[i].porch_walls.regions {
                if let Some((a, b)) = r.base_edge() {
                    let mid = Point2f64::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                    if point_on_ring(ring, mid, CONTAINMENT_TOL_M) {
                        r.exposure = inner_exposure;
                    }
                }
            }
            if *verdict != PorchVerdict::Excluded {
                continue;
            }
            let porch_fp = PlanFootprint::of(&FloorRegions::from_vertex_lists(
                vec![ring.clone()],
                ItemId::nil(),
            ));
            if porch_fp.is_empty() {
                continue;
            }

            // ── Floors: carve the porch footprint out, stamp it Irrelevant.
            let storey = &mut ber.storeys[i];
            storey.floors =
                split_floors_intact(std::mem::take(&mut storey.floors), &[ring.clone()]);
            let mut buried: Vec<usize> = Vec::new();
            for (ri, r) in storey.floors.regions.iter_mut().enumerate() {
                let rr = floor_plan_ring(r);
                if rr.len() >= 3 && porch_fp.contains_ring(&rr) {
                    r.exposure = FloorExposure::Irrelevant;
                    buried.push(ri);
                }
            }
            // Out-of-assessment surfaces carry no fabric — but the floor
            // construction is ALSO the phase-zone record, so reset to the
            // PHASE-ONLY construction (the inter-storey buried-floor rule).
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

            // ── Ceilings: split (regions-as-truth — pass 0 reverses), stamp.
            let storey = &mut ber.storeys[i];
            if !storey.ceilings.regions.is_empty() {
                storey.ceilings = std::mem::take(&mut storey.ceilings)
                    .split_faces_coplanar(std::slice::from_ref(ring));
                for r in &mut storey.ceilings.regions {
                    let rr: Vec<Point2f64> = r
                        .polygon
                        .exterior
                        .iter()
                        .map(|v| Point2f64::new(v.x, v.y))
                        .collect();
                    if rr.len() >= 3 && porch_fp.contains_ring(&rr) {
                        r.exposure = CeilingExposure::Irrelevant;
                        r.construction_id = ItemId::nil();
                    }
                }
            }

            // ── Outer walls: split the perimeter walls at the porch's
            // outline-flush edge ENDPOINTS (a perpendicular tick through each
            // crossing point — `apply_splits` semantics), then stamp every
            // wall whose base lies along a flush edge.
            let footprint = PlanFootprint::of(&ber.storeys[i].floors);
            let runs = classified_edge_runs(ring, &footprint);
            let flush_edges: Vec<[Point2f64; 2]> = runs
                .iter()
                .filter(|(flush, _)| *flush)
                .flat_map(|(_, r)| r.iter().copied())
                .collect();
            let mut cuts: Vec<[Point2f64; 2]> = Vec::new();
            for e in &flush_edges {
                let d = Point2f64::new(e[1].x - e[0].x, e[1].y - e[0].y);
                let len = d.x.hypot(d.y);
                if len < 1e-9 {
                    continue;
                }
                let nrm = Point2f64::new(-d.y / len, d.x / len);
                for p in [e[0], e[1]] {
                    cuts.push([
                        Point2f64::new(p.x - nrm.x * 0.1, p.y - nrm.y * 0.1),
                        Point2f64::new(p.x + nrm.x * 0.1, p.y + nrm.y * 0.1),
                    ]);
                }
            }
            let storey = &mut ber.storeys[i];
            storey.walls = std::mem::take(&mut storey.walls).apply_splits(&cuts);
            let mut buried_walls: Vec<usize> = Vec::new();
            for (ri, r) in storey.walls.regions.iter_mut().enumerate() {
                let Some((a, b)) = r.base_edge() else { continue };
                let mid = Point2f64::new((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
                let on_flush = flush_edges.iter().any(|e| {
                    point_to_segment_distance(mid.x, mid.y, e[0].x, e[0].y, e[1].x, e[1].y)
                        <= CONTAINMENT_TOL_M
                });
                if on_flush {
                    r.exposure = WallExposure::Irrelevant;
                    buried_walls.push(ri);
                }
            }
            // Same fabric rule as the inter-storey buried band: an
            // out-of-assessment wall keeps only its phase.
            for ri in buried_walls {
                let cid = ber.storeys[i].walls.regions[ri].construction_id;
                let phase = ber
                    .wall_construction(cid)
                    .map(|c| c.base.construction_phase.clone())
                    .unwrap_or_default();
                let reset = ber.ensure_wall_construction(
                    WallConstructionType::default(),
                    ConstructionMakeup::default(),
                    phase,
                );
                ber.storeys[i].walls.regions[ri].construction_id = reset;
            }

            // ── Storey above: its floor over the excluded porch is over an
            // UNHEATED space — semi-exposed (DEAP), not an inter-storey
            // junction. `derive_inter_storey` ran first and stamped it
            // `InterStorey` (the porch is inside this storey's footprint);
            // re-stamp just the porch overlap. The construction was reset to
            // phase-only by the inter-storey pass — correct here too: the
            // now-heat-loss floor is back IN structure scope, awaiting
            // attribution.
            if i + 1 < n && dim(i + 1) {
                let above = &mut ber.storeys[i + 1];
                above.floors =
                    split_floors_intact(std::mem::take(&mut above.floors), &[ring.clone()]);
                for r in &mut above.floors.regions {
                    let rr = floor_plan_ring(r);
                    if rr.len() >= 3
                        && porch_fp.contains_ring(&rr)
                        && r.exposure == FloorExposure::InterStorey
                    {
                        r.exposure = FloorExposure::SemiExposed;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::fenestration::{Door, WallAnchor};
    use crate::ber::objects::{BuildingObject, ObjectRoom, RadiatorTrv};
    use crate::ber::structural::{
        derive_inter_storey, CeilingRegion, CeilingRegions, WallRegions,
    };
    use domain::points::Point2f32;
    use manip_plan::SurfaceRef;

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

    /// A 10×8 house whose front wall (y = 0) carries a 2×1.5 m porch bump
    /// protruding at x ∈ [4, 6] — the DEAP Figure 4.17 shape. The porch's
    /// single inner edge is (6,0)→(4,0), collinear with the front wall.
    fn porch_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(4.0, -1.5),
            Point2f64::new(6.0, -1.5),
            Point2f64::new(6.0, 0.0),
            Point2f64::new(4.0, 0.0),
        ]
    }

    fn porch_house() -> Ber {
        let outline = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, -1.5),
            Point2f64::new(6.0, -1.5),
            Point2f64::new(6.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 8.0),
            Point2f64::new(0.0, 8.0),
        ];
        let cid = ItemId::default();
        let floors = FloorRegions::new(outline.clone(), cid);
        let ceilings =
            CeilingRegions::default_for_footprint(vec![outline.clone()], 2.4, cid);
        let walls =
            WallRegions::regenerate(perimeter(&outline), &ceilings, &floors, 0.0, 2.4, cid);
        let inner_edge = vec![(
            Point2f64::new(6.0, 0.0),
            Point2f64::new(4.0, 0.0),
            WallExposure::Unset,
        )];
        let porch_walls =
            WallRegions::regenerate(inner_edge, &ceilings, &floors, 0.0, 2.4, cid);
        let mut storey = Storey {
            floors,
            walls,
            ceilings,
            porch_walls,
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        storey.porches = FloorRegions::new(porch_ring(), cid);
        let mut ber = Ber::default();
        ber.storeys.push(storey);
        ber
    }

    fn porch_key() -> PorchKey {
        ring_centroid_key(&porch_ring())
    }

    /// A door recorded on the porch's inner wall (the separating door).
    fn separating_door() -> Door {
        Door {
            id: ItemId::default(),
            type_name: "Front door".into(),
            vertices: Vec::new(),
            photos: Vec::new(),
            width_m: 0.9,
            height_m: 2.0,
            kind: None,
            placement: None,
            wall_anchor: Some(WallAnchor {
                wall: SurfaceRef::wall(Default::default()),
                along_fraction: 0.5,
                porch: Some(porch_key()),
            }),
            assembly_id: None,
            draught_stripped: None,
        }
    }

    fn confirm_unheated(ber: &mut Ber) {
        ber.storeys[0].porch_checks.push(PorchCheck {
            key: porch_key(),
            heating_confirmed_absent: true,
            door_confirmed_absent: false,
        });
    }

    #[test]
    fn key_is_mm_quantized_centroid() {
        assert_eq!(porch_key(), (5000, -750));
        // Closed ring (duplicate final point) gives the identical key.
        let mut closed = porch_ring();
        closed.push(closed[0]);
        assert_eq!(ring_centroid_key(&closed), (5000, -750));
    }

    /// The Table 2.2 truth table, protrusion held true.
    #[test]
    fn verdict_table() {
        let f = |heated, door| PorchFacts {
            key: (0, 0),
            heated,
            separating_door: door,
            fully_protruding: true,
        };
        use PorchVerdict::*;
        assert_eq!(f(Some(true), Some(true)).verdict(), Included); // heated
        assert_eq!(f(Some(true), None).verdict(), Included); // heated wins
        assert_eq!(f(None, Some(false)).verdict(), Included); // open access
        assert_eq!(f(Some(false), Some(false)).verdict(), Included);
        assert_eq!(f(Some(false), Some(true)).verdict(), Excluded);
        assert_eq!(f(None, Some(true)).verdict(), Undecided);
        assert_eq!(f(Some(false), None).verdict(), Undecided);
        assert_eq!(f(None, None).verdict(), Undecided);
        // Not fully protruding → included regardless of everything else.
        let within = PorchFacts {
            fully_protruding: false,
            ..f(Some(false), Some(true))
        };
        assert_eq!(within.verdict(), Included);
    }

    /// The Figure 4.17 bump porch derives `fully_protruding`; a Figure 4.18
    /// corner porch (two inner edges) does not.
    #[test]
    fn protrusion_geometry() {
        let ber = porch_house();
        let facts = porch_facts(&ber.storeys[0], &ber.storeys[0].porches.regions[0]);
        assert!(facts.fully_protruding, "bump porch protrudes entirely");

        let cid = ItemId::default();
        let mut corner = Storey {
            floors: FloorRegions::new(square(0.0, 0.0, 10.0, 8.0), cid),
            ..Storey::default()
        };
        corner.porches = FloorRegions::new(square(0.0, 0.0, 2.0, 1.5), cid);
        let facts = porch_facts(&corner, &corner.porches.regions[0]);
        assert!(!facts.fully_protruding, "corner porch is within the line");
        assert_eq!(facts.verdict(), PorchVerdict::Included);
    }

    /// Door recorded + heating confirmed absent + protruding → EXCLUDED, and
    /// the derive stamps every surface: porch floor/ceiling/outer walls
    /// `Irrelevant`, inner wall `SemiExposed`. Idempotent across re-derives.
    #[test]
    fn excluded_porch_stamps_geometry() {
        let mut ber = porch_house();
        ber.storeys[0].doors.push(separating_door());
        confirm_unheated(&mut ber);
        let verdicts = porch_verdicts(&ber);
        assert_eq!(verdicts.len(), 1);
        assert_eq!(verdicts[0].2, PorchVerdict::Excluded);

        derive_porch_exclusions(&mut ber, &[true]);
        let s = &ber.storeys[0];
        use geo::Area;
        let irr_floor: f64 = s
            .floors
            .regions
            .iter()
            .filter(|r| r.exposure == FloorExposure::Irrelevant)
            .map(|r| r.polygon.unsigned_area())
            .sum();
        assert!((irr_floor - 3.0).abs() < 1e-6, "porch slab = 2×1.5 m²");
        let kept_floor: f64 = s
            .floors
            .regions
            .iter()
            .filter(|r| r.exposure != FloorExposure::Irrelevant)
            .map(|r| r.polygon.unsigned_area())
            .sum();
        assert!((kept_floor - 80.0).abs() < 1e-6, "dwelling keeps 10×8 m²");
        let irr_ceiling = s
            .ceilings
            .regions
            .iter()
            .filter(|r| r.exposure == CeilingExposure::Irrelevant)
            .count();
        assert_eq!(irr_ceiling, 1, "porch roof piece stamped");
        let irr_walls = s
            .walls
            .regions
            .iter()
            .filter(|r| r.exposure == WallExposure::Irrelevant)
            .count();
        assert_eq!(irr_walls, 3, "the porch's three outdoor faces");
        assert!(s
            .porch_walls
            .regions
            .iter()
            .all(|r| r.exposure == WallExposure::SemiExposed));

        // Idempotent: a second derive reproduces the exact same stamp set.
        let descriptor = |s: &Storey| {
            (
                s.floors.regions.len(),
                s.ceilings.regions.len(),
                s.walls.regions.len(),
                s.walls
                    .regions
                    .iter()
                    .filter(|r| r.exposure == WallExposure::Irrelevant)
                    .count(),
            )
        };
        let before = descriptor(&ber.storeys[0]);
        derive_porch_exclusions(&mut ber, &[true]);
        assert_eq!(descriptor(&ber.storeys[0]), before);
    }

    /// A heat emitter pinned inside the porch → INCLUDED: inner walls become
    /// `NonHeatLoss`, nothing is stamped `Irrelevant`.
    #[test]
    fn heated_porch_is_included() {
        let mut ber = porch_house();
        ber.storeys[0].doors.push(separating_door());
        let mut room = ObjectRoom::new(Point2f32::new(5.0, -0.75));
        room.objects
            .push(BuildingObject::new(ObjectKind::Radiator(RadiatorTrv::WithTrv), 1));
        ber.storeys[0].object_rooms.push(room);
        assert_eq!(porch_verdicts(&ber)[0].2, PorchVerdict::Included);

        derive_porch_exclusions(&mut ber, &[true]);
        let s = &ber.storeys[0];
        assert!(s
            .porch_walls
            .regions
            .iter()
            .all(|r| r.exposure == WallExposure::NonHeatLoss));
        assert!(s
            .floors
            .regions
            .iter()
            .all(|r| r.exposure != FloorExposure::Irrelevant));
        assert!(s
            .walls
            .regions
            .iter()
            .all(|r| r.exposure != WallExposure::Irrelevant));
    }

    /// No door recorded, nothing confirmed → UNDECIDED: inner walls stay
    /// `Unset` (blocking export) and no geometry is stamped. A later EXCLUDED
    /// verdict then stamps, and flipping back (heater discovered) retracts
    /// every stamp — the self-retraction contract.
    #[test]
    fn undecided_leaves_unset_and_verdict_flip_retracts() {
        let mut ber = porch_house();
        assert_eq!(porch_verdicts(&ber)[0].2, PorchVerdict::Undecided);
        derive_porch_exclusions(&mut ber, &[true]);
        assert!(ber.storeys[0]
            .porch_walls
            .regions
            .iter()
            .all(|r| r.exposure == WallExposure::Unset));
        assert!(ber.storeys[0]
            .floors
            .regions
            .iter()
            .all(|r| r.exposure != FloorExposure::Irrelevant));

        // Evidence lands → excluded → stamps.
        ber.storeys[0].doors.push(separating_door());
        confirm_unheated(&mut ber);
        derive_porch_exclusions(&mut ber, &[true]);
        assert!(ber.storeys[0]
            .walls
            .regions
            .iter()
            .any(|r| r.exposure == WallExposure::Irrelevant));

        // A radiator turns up after all → included → full retraction.
        let mut room = ObjectRoom::new(Point2f32::new(5.0, -0.75));
        room.objects
            .push(BuildingObject::new(ObjectKind::Radiator(RadiatorTrv::WithTrv), 1));
        ber.storeys[0].object_rooms.push(room);
        derive_porch_exclusions(&mut ber, &[true]);
        let s = &ber.storeys[0];
        assert!(s.walls.regions.iter().all(|r| r.exposure != WallExposure::Irrelevant));
        assert!(s.floors.regions.iter().all(|r| r.exposure != FloorExposure::Irrelevant));
        assert!(s
            .ceilings
            .regions
            .iter()
            .all(|r| r.exposure != CeilingExposure::Irrelevant));
        assert!(s
            .porch_walls
            .regions
            .iter()
            .all(|r| r.exposure == WallExposure::NonHeatLoss));
    }

    /// The Structure-phase porch exception: a porch inner wall is OUT of
    /// structure scope while its verdict is pending (`Unset` — unlike a
    /// normal wall, where Unset stays reachable) and while the porch is
    /// included (`NonHeatLoss`), and enters scope — selectable, counted,
    /// flagged incomplete — exactly when the exclusion derive stamps it
    /// `SemiExposed`.
    #[test]
    fn porch_wall_enters_structure_scope_only_when_semi_exposed() {
        use crate::ber::structural::SurfaceKind;
        let mut ber = porch_house();
        let wall_id = ber.storeys[0].porch_walls.regions[0].id;
        let walls_before = ber.storeys[0].surface_counts().walls;

        // Undecided → Unset → out of scope, uncounted.
        derive_porch_exclusions(&mut ber, &[true]);
        let s = &ber.storeys[0];
        assert!(!s.surface_in_structure_scope(SurfaceKind::Walls, wall_id));
        assert_eq!(s.surface_counts().walls, walls_before);
        assert!(!ber
            .incomplete_surface_ids(0, SurfaceKind::Walls)
            .contains(&wall_id));

        // Heated → Included → NonHeatLoss → still out of scope.
        let mut room = ObjectRoom::new(Point2f32::new(5.0, -0.75));
        room.objects
            .push(BuildingObject::new(ObjectKind::Radiator(RadiatorTrv::WithTrv), 1));
        ber.storeys[0].object_rooms.push(room);
        derive_porch_exclusions(&mut ber, &[true]);
        assert!(!ber.storeys[0].surface_in_structure_scope(SurfaceKind::Walls, wall_id));

        // Excluded → SemiExposed → in scope, counted, and needing attribution.
        ber.storeys[0].object_rooms.clear();
        ber.storeys[0].doors.push(separating_door());
        confirm_unheated(&mut ber);
        derive_porch_exclusions(&mut ber, &[true]);
        let s = &ber.storeys[0];
        assert!(s.surface_in_structure_scope(SurfaceKind::Walls, wall_id));
        assert_eq!(s.surface_kind_of(wall_id), Some(SurfaceKind::Walls));
        assert!(s.is_porch_wall(wall_id));
        assert!(s.surface_ids(SurfaceKind::Walls).contains(&wall_id));
        assert!(s.surface_counts().walls > walls_before);
        assert!(
            ber.incomplete_surface_ids(0, SurfaceKind::Walls)
                .contains(&wall_id),
            "an unattributed semi-exposed porch wall must show in completeness"
        );
    }

    /// A first storey extends over the excluded ground-floor porch: after the
    /// inter-storey pass stamps the junction, the porch derive re-stamps the
    /// overlap SemiExposed (a floor over an unheated space) and the ceiling
    /// below it Irrelevant.
    #[test]
    fn storey_above_excluded_porch_goes_semi_exposed() {
        let mut ber = porch_house();
        ber.storeys[0].doors.push(separating_door());
        confirm_unheated(&mut ber);
        // First storey covering the whole footprint INCLUDING the porch bump.
        let cid = ItemId::default();
        let ring = vec![
            Point2f64::new(0.0, -1.5),
            Point2f64::new(10.0, -1.5),
            Point2f64::new(10.0, 8.0),
            Point2f64::new(0.0, 8.0),
        ];
        let floors = FloorRegions::new(ring.clone(), cid);
        let ceilings = CeilingRegions::default_for_footprint(vec![ring.clone()], 2.4, cid);
        let walls =
            WallRegions::regenerate(perimeter(&ring), &ceilings, &floors, 0.0, 2.4, cid);
        ber.storeys.push(Storey {
            floors,
            walls,
            ceilings,
            ceiling_height_m: Some(2.4),
            floor_thickness_m: Some(0.3),
            ..Storey::default()
        });
        derive_inter_storey(&mut ber, &[true, true]);
        derive_porch_exclusions(&mut ber, &[true, true]);

        use geo::Area;
        let semi: f64 = ber.storeys[1]
            .floors
            .regions
            .iter()
            .filter(|r| r.exposure == FloorExposure::SemiExposed)
            .map(|r| r.polygon.unsigned_area())
            .sum();
        assert!(
            (semi - 3.0).abs() < 1e-6,
            "the 2×1.5 m over-porch floor is semi-exposed, got {semi}"
        );
        // The ground-storey ceiling piece under it is out of the assessment.
        assert!(ber.storeys[0]
            .ceilings
            .regions
            .iter()
            .any(|r| r.exposure == CeilingExposure::Irrelevant));
    }
}
