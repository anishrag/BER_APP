//! Wall-exposure model operations: painting / clearing per-wall heat-loss
//! sections on the storey outline, deriving full coverage, and classifying
//! the resolved storey's derived RISER walls (the dashed-line picker's
//! data). Pure model logic — the shell owns pointer selection, undo, and
//! persistence.
use crate::{centroid_key, CentroidKey, ExposureSection, StoreyDoc};
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::{
    CeilingExposure, FloorExposure, Storey, SurfaceKind, WallExposure, WallRegion, WallRegions,
};
use domain::points::{Point2f32, Point2f64};
use manip_plan::{project_distance_along, ring_within_ring, segment_len, VertexId};
use std::collections::HashMap;

/// A floor/ceiling carve below an area epsilon (m²) is boolean-op debris (or a
/// negligible sliver where two footprints barely touch) — not a real party
/// surface. Mirrors the `SLIVER_AREA_M2` filter the floor-region splits use.
const CARVE_AREA_EPS_M2: f64 = 1e-3;

/// A party-wall sub-range shorter than this (m) is a sliver where a wall just
/// grazes a neighbour footprint corner — not a genuine shared wall.
const PARTY_WALL_EPS_M: f32 = 1e-2;

/// Unsigned shoelace area of a plan ring (m²); `< 3` vertices → 0.
fn ring_area(ring: &[Point2f64]) -> f64 {
    if ring.len() < 3 {
        return 0.0;
    }
    let n = ring.len();
    let mut s = 0.0;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        s += a.x * b.y - b.x * a.y;
    }
    s.abs() / 2.0
}

/// Plan centroid of a ring (area-weighted; falls back to the vertex average for
/// a degenerate zero-area ring). Matches `FloorRegion::centroid`'s sampling so a
/// carve's sample point lands inside the region that resolve re-derives from it.
fn ring_centroid(ring: &[Point2f64]) -> Point2f64 {
    let n = ring.len();
    if n == 0 {
        return Point2f64::new(0.0, 0.0);
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
    if a.abs() < 1e-12 {
        let sx: f64 = ring.iter().map(|p| p.x).sum();
        let sy: f64 = ring.iter().map(|p| p.y).sum();
        return Point2f64::new(sx / n as f64, sy / n as f64);
    }
    Point2f64::new(cx / (3.0 * a), cy / (3.0 * a))
}

/// A `geo::Polygon` (no holes) from a plan ring.
fn ring_poly(ring: &[Point2f64]) -> geo::Polygon<f64> {
    geo::Polygon::new(
        geo::LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
        vec![],
    )
}

/// The exterior ring of a `geo::Polygon` as plan points (drops the closing
/// repeat so it matches the stored split-ring shape).
fn poly_exterior_ring(poly: &geo::Polygon<f64>) -> Vec<Point2f64> {
    let mut ring: Vec<Point2f64> = poly
        .exterior()
        .coords()
        .map(|c| Point2f64::new(c.x, c.y))
        .collect();
    if ring.len() >= 2 && ring.first() == ring.last() {
        ring.pop();
    }
    ring
}

/// Clip wall segment `a`→`b` (plan metres) by footprint polygon `fp`, returning
/// the along-wall `(start_m, end_m)` sub-ranges of the segment that lie WITHIN
/// `fp` — the shared length of a party wall. Robust to a wall partly in / out
/// (geo 1-D `clip` returns one range per inside run). `fp` with `< 3` vertices →
/// empty. Distances are measured from `a` along `a`→`b`.
fn wall_ranges_within(a: Point2f32, b: Point2f32, fp: &[Point2f64]) -> Vec<(f32, f32)> {
    use geo::bool_ops::BooleanOps;
    if fp.len() < 3 {
        return Vec::new();
    }
    let len = segment_len(a, b);
    if len < 1e-6 {
        return Vec::new();
    }
    let seg = geo::MultiLineString::new(vec![geo::LineString::from(vec![
        (a.x as f64, a.y as f64),
        (b.x as f64, b.y as f64),
    ])]);
    let inside = ring_poly(fp).clip(&seg, false);
    let mut ranges: Vec<(f32, f32)> = Vec::new();
    for ls in &inside.0 {
        let mut d_min = f32::MAX;
        let mut d_max = f32::MIN;
        for c in ls.coords() {
            let d = project_distance_along(a, b, len, c.x as f32, c.y as f32);
            d_min = d_min.min(d);
            d_max = d_max.max(d);
        }
        if d_max - d_min > PARTY_WALL_EPS_M {
            ranges.push((d_min.max(0.0), d_max.min(len)));
        }
    }
    ranges
}

/// The party pieces of floor/ceiling region ring `r` against neighbour footprint
/// `neighbour`: a genuine PARTIAL overlap yields the `r ∩ neighbour` pieces (each
/// as `(boundary ring, centroid)`); FULL containment yields none (the
/// full-containment auto-derive already marks it); no overlap yields none. A
/// piece below [`CARVE_AREA_EPS_M2`], or one that is effectively the whole region
/// (the `rest = r − neighbour` is a negligible sliver), is dropped.
fn party_overlap_pieces(
    r: &[Point2f64],
    neighbour: &[Point2f64],
) -> Vec<(Vec<Point2f64>, Point2f64)> {
    use geo::bool_ops::BooleanOps;
    if r.len() < 3 || neighbour.len() < 3 {
        return Vec::new();
    }
    // Full containment is the auto-derive's job — emit no carve.
    if ring_within_ring(r, neighbour, CONTAINMENT_TOL_M) {
        return Vec::new();
    }
    let r_poly = ring_poly(r);
    let party = r_poly.intersection(&ring_poly(neighbour));
    let rest = r_poly.difference(&ring_poly(neighbour));
    let rest_area: f64 = rest
        .0
        .iter()
        .map(|p| ring_area(&poly_exterior_ring(p)))
        .sum();
    // The non-party remainder is a negligible sliver → treat as full containment.
    if rest_area < CARVE_AREA_EPS_M2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for piece in &party.0 {
        let ring = poly_exterior_ring(piece);
        if ring_area(&ring) < CARVE_AREA_EPS_M2 {
            continue; // sliver
        }
        let c = ring_centroid(&ring);
        out.push((ring, c));
    }
    out
}

/// Durable identity for a RISER (interior step-face) wall: its base edge as a
/// quantized, orientation-independent plan segment (millimetre grid). Risers
/// have no perimeter `VertexId`, so this geometric key is how a riser's exposure
/// override persists across the wall re-derivation (the source of truth once the
/// amend-match is retired).
pub type RiserKey = (i64, i64, i64, i64);

/// Quantize a riser base edge `(a, b)` to a [`RiserKey`]: each endpoint to the
/// nearest millimetre, with the two endpoints ordered so the key is independent
/// of the `a`→`b` direction.
pub fn riser_key(a: Point2f64, b: Point2f64) -> RiserKey {
    let q = |v: f64| (v * 1000.0).round() as i64;
    let pa = (q(a.x), q(a.y));
    let pb = (q(b.x), q(b.y));
    let (lo, hi) = if pa <= pb { (pa, pb) } else { (pb, pa) };
    (lo.0, lo.1, hi.0, hi.1)
}

/// Paint `[start, end]` of a wall with `exposure`: the new mark WINS over any
/// overlap (existing sections are trimmed to their non-overlapping remnants),
/// then the new section is inserted and the list re-sorted. No merging of
/// adjacent same-type ranges — each stays its own region.
fn paint_section(
    sections: &mut Vec<ExposureSection>,
    start: f32,
    end: f32,
    exposure: WallExposure,
) {
    let (s, e) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    if e - s < 1e-4 {
        return; // zero-length selection paints nothing
    }
    let mut out: Vec<ExposureSection> = Vec::new();
    for sec in sections.iter() {
        if sec.end_m <= s || sec.start_m >= e {
            out.push(*sec); // no overlap
            continue;
        }
        if sec.start_m < s - 1e-4 {
            out.push(ExposureSection {
                start_m: sec.start_m,
                end_m: s,
                exposure: sec.exposure,
            });
        }
        if sec.end_m > e + 1e-4 {
            out.push(ExposureSection {
                start_m: e,
                end_m: sec.end_m,
                exposure: sec.exposure,
            });
        }
        // The overlapped middle is dropped — the new mark wins.
    }
    out.push(ExposureSection {
        start_m: s,
        end_m: e,
        exposure,
    });
    out.sort_by(|a, b| {
        a.start_m
            .partial_cmp(&b.start_m)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    *sections = out;
}

/// Full coverage of a wall of length `len` as `(start, end, exposure)` ranges:
/// the painted sections in order, with every gap (and the whole wall when
/// nothing is painted) filled with `Unset` — unmarked length is genuinely
/// unset, never assumed `Exposed`. `Unset` is DERIVED coverage only: it never
/// enters the stored sections (single-representation rule). Used to derive one
/// BER `WallRegion` per range (via [`StoreyDoc::wall_exposure_segments`]).
pub fn wall_coverage(len: f32, sections: &[ExposureSection]) -> Vec<(f32, f32, WallExposure)> {
    let mut out: Vec<(f32, f32, WallExposure)> = Vec::new();
    let mut cursor = 0.0_f32;
    for sec in sections {
        let s = sec.start_m.clamp(0.0, len);
        let e = sec.end_m.clamp(0.0, len);
        if s > cursor + 1e-4 {
            out.push((cursor, s, WallExposure::Unset)); // gap before this section
        }
        if e > s + 1e-4 {
            out.push((s, e, sec.exposure));
            cursor = e;
        }
    }
    if cursor < len - 1e-4 {
        out.push((cursor, len, WallExposure::Unset)); // trailing gap
    }
    if out.is_empty() {
        out.push((0.0, len, WallExposure::Unset)); // nothing painted → all unset
    }
    out
}

/// Whether a derived riser wall stands between two FLOOR levels (a floor
/// step's vertical face) or two CEILING profiles (a ceiling step's). Drives
/// the picker's kind label; the derived DEFAULT exposure (which "clear"
/// reverts to) is `Unset` for both kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RiserKind {
    Floor,
    Ceiling,
}

impl RiserKind {
    /// The exposure the resolve projection derives for a fresh riser of
    /// either kind — `Unset`, like every other wall, and what "clear"
    /// reverts to. Ground contact is the assessor's explicit choice (the
    /// riser toolbar's Ground button), no longer a derived floor-riser
    /// default. Must stay in lock-step with `resolve_storey`'s riser
    /// derivation.
    pub fn default_exposure(self) -> WallExposure {
        WallExposure::Unset
    }
}

/// One derived riser wall of the resolved storey, projected to its plan BASE
/// edge — what the 2D editor draws (dashed) and selects. `id` is the
/// region's stable identity (`with_risers`' amend keeps it across
/// re-derivations), so it's a safe selection key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RiserWall {
    pub id: ItemId,
    pub a: Point2f64,
    pub b: Point2f64,
    pub exposure: WallExposure,
    pub kind: RiserKind,
}

/// Both endpoints of two plan edges within `eps` of each other, in either
/// orientation — the same edge-identity test as the riser amend matching.
fn plan_edges_match(a: Point2f64, b: Point2f64, c: Point2f64, d: Point2f64, eps: f64) -> bool {
    let dist = |p: Point2f64, q: Point2f64| ((p.x - q.x).powi(2) + (p.y - q.y).powi(2)).sqrt();
    (dist(a, c).max(dist(b, d)) <= eps) || (dist(a, d).max(dist(b, c)) <= eps)
}

/// The coverage range `(start, end)` — a marked section or an un-marked gap —
/// containing distance `d` along a wall of length `len`. Lets a tap select just
/// the piece under it rather than the whole wall.
pub fn range_at(len: f32, sections: &[ExposureSection], d: f32) -> (f32, f32) {
    for (s, e, _) in wall_coverage(len, sections) {
        if d >= s - 1e-4 && d <= e + 1e-4 {
            return (s, e);
        }
    }
    (0.0, len)
}

impl StoreyDoc {
    /// Paint a section `[start, end]` (clamped to the wall's length `len`) with
    /// `exposure` onto outline wall `vid`. ONLY painted sections are stored —
    /// unmarked length never materializes into sections; it shows up as `Unset`
    /// in the derived coverage instead (single-representation rule: the
    /// in-session model equals the post-reload one with no canonicalize pass).
    pub fn set_wall_section(
        &mut self,
        vid: VertexId,
        start: f32,
        end: f32,
        len: f32,
        exposure: WallExposure,
    ) {
        let (s, e) = (start.clamp(0.0, len), end.clamp(0.0, len));
        // A zero-length paint is a no-op — bail BEFORE creating the map entry,
        // so an untouched wall stays ABSENT (matching what a reload rebuilds;
        // an empty Vec entry would be in-session-only noise).
        if (e - s).abs() < 1e-4 {
            return;
        }
        let sections = self.wall_sections.entry(vid).or_default();
        paint_section(sections, s, e, exposure);
    }

    /// Remove all exposure sections from outline wall `vid`.
    pub fn clear_wall_section(&mut self, vid: VertexId) {
        self.wall_sections.remove(&vid);
    }

    /// Set the FLOOR exposure of the resolved floor region under plan point
    /// `(x, y)` — the manual-marking op behind a canvas tap. The region is
    /// located in the resolved `storey` ([`Storey::surface_at`]); its quantized
    /// centroid keys the durable override ([`floor_exposure`](StoreyDoc::floor_exposure)),
    /// which `resolve` stamps back onto the re-derived region. A manual write
    /// takes precedence over the auto-derive (which only `or_insert`s). Returns
    /// the [`CentroidKey`] written, or `None` when no floor region is under the
    /// tap.
    pub fn set_floor_exposure_at(
        &mut self,
        storey: &Storey,
        x: f64,
        y: f64,
        e: FloorExposure,
    ) -> Option<CentroidKey> {
        let id = storey.surface_at(SurfaceKind::Floors, x, y, 0.0)?;
        let region = storey.floors.regions.iter().find(|r| r.id == id)?;
        let key = centroid_key(region.centroid());
        self.floor_exposure.insert(key, e);
        Some(key)
    }

    /// Set the CEILING exposure of the resolved ceiling/roof region under plan
    /// point `(x, y)` — the ceiling mirror of
    /// [`set_floor_exposure_at`](StoreyDoc::set_floor_exposure_at). `None` when
    /// no ceiling region is under the tap.
    pub fn set_ceiling_exposure_at(
        &mut self,
        storey: &Storey,
        x: f64,
        y: f64,
        e: CeilingExposure,
    ) -> Option<CentroidKey> {
        let id = storey.surface_at(SurfaceKind::Roofs, x, y, 0.0)?;
        let region = storey.roofs.regions.iter().find(|r| r.id == id)?;
        let key = centroid_key(region.centroid());
        self.ceiling_exposure.insert(key, e);
        Some(key)
    }

    /// Mark the still-`Unset` sub-ranges of every outline perimeter wall that lie
    /// over a neighbour footprint (above or below) as [`WallExposure::NonHeatLoss`]
    /// — a party wall shared with the storey above/below. The one-shot
    /// inter-storey auto-pass (vs. the per-frame [`auto_derive_horizontal_exposure`]
    /// for FLOORS/CEILINGS), so it preserves manual marks: a FACE-PARTITIONED wall
    /// is skipped entirely, and on every other wall only the gaps that
    /// [`wall_coverage`] still reports `Unset` are painted (an existing section
    /// survives). `None` footprints contribute nothing.
    pub fn auto_mark_party_walls(
        &mut self,
        below_fp: Option<&[Point2f64]>,
        above_fp: Option<&[Point2f64]>,
    ) {
        for (vid, a, b) in self.outline.floor_plan.perimeter_walls() {
            // A face-partitioned wall is owned by its 2-D regions, not 1-D
            // sections — leave it for the partition machinery.
            if self.is_wall_partitioned(vid) {
                continue;
            }
            let len = segment_len(a, b);
            if len < 1e-6 {
                continue;
            }
            // The sub-ranges shared with EITHER neighbour (a party wall is shared
            // whether the overlap is above or below).
            let mut shared: Vec<(f32, f32)> = Vec::new();
            if let Some(fp) = below_fp {
                shared.extend(wall_ranges_within(a, b, fp));
            }
            if let Some(fp) = above_fp {
                shared.extend(wall_ranges_within(a, b, fp));
            }
            if shared.is_empty() {
                continue;
            }
            // Only paint the parts that are STILL `Unset` — clip each shared range
            // against the wall's current `Unset` coverage gaps (preserve marks).
            let sections = self
                .wall_sections
                .get(&vid)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let unset: Vec<(f32, f32)> = wall_coverage(len, sections)
                .into_iter()
                .filter(|(s, e, exp)| *exp == WallExposure::Unset && e - s > PARTY_WALL_EPS_M)
                .map(|(s, e, _)| (s, e))
                .collect();
            for (gs, ge) in &unset {
                for (ss, se) in &shared {
                    let s = ss.max(*gs);
                    let e = se.min(*ge);
                    if e - s > PARTY_WALL_EPS_M {
                        self.set_wall_section(vid, s, e, len, WallExposure::NonHeatLoss);
                    }
                }
            }
        }
    }

    /// Plan the party-FLOOR carves of resolved `storey` against the storey BELOW's
    /// footprint `below_fp`: returns `(split rings to push onto
    /// `floor_splits`, party sample points)`. A floor region FULLY within
    /// `below_fp` yields nothing (the full-containment auto-derive marks it
    /// `NonHeatLoss`); a genuine PARTIAL overlap (cantilever / setback) yields the
    /// party piece's boundary ring (the durable split) plus its centroid (the
    /// stamp sample). Negligible slivers are dropped. Pure — the shell pushes the
    /// rings, re-resolves, then stamps the samples.
    pub fn plan_party_floor_carves(
        &self,
        storey: &Storey,
        below_fp: &[Point2f64],
    ) -> (Vec<Vec<Point2f64>>, Vec<Point2f64>) {
        let mut rings = Vec::new();
        let mut samples = Vec::new();
        for ring in storey.floors.to_vertex_lists() {
            for (piece, c) in party_overlap_pieces(&ring, below_fp) {
                rings.push(piece);
                samples.push(c);
            }
        }
        (rings, samples)
    }

    /// Plan the party-CEILING carves of resolved `storey` against the storey
    /// ABOVE's footprint `above_fp` — the ceiling mirror of
    /// [`plan_party_floor_carves`](Self::plan_party_floor_carves). Returns
    /// `(split rings for `roof_splits`, party sample points)` over the storey's
    /// ceiling/roof region rings; a partial overlap with the storey above yields
    /// the `ceiling ∩ above_fp` piece (its boundary ring + centroid).
    pub fn plan_party_ceiling_carves(
        &self,
        storey: &Storey,
        above_fp: &[Point2f64],
    ) -> (Vec<Vec<Point2f64>>, Vec<Point2f64>) {
        let mut rings = Vec::new();
        let mut samples = Vec::new();
        for region in &storey.roofs.regions {
            let ring = ceiling_ring_xy(region);
            for (piece, c) in party_overlap_pieces(&ring, above_fp) {
                rings.push(piece);
                samples.push(c);
            }
        }
        (rings, samples)
    }

    /// This storey's heat-loss wall SEGMENTS over the FULL perimeter: every
    /// outline wall split into its coverage ranges — painted sections plus
    /// `Unset` gaps, an unmarked wall yielding one full-length `Unset` segment
    /// — as `(start, end, exposure)` in world coords (endpoints interpolated
    /// along the wall by distance). The shell projects these into BER
    /// `WallRegion`s, so every wall exists in the resolved geometry whether
    /// it's been marked or not.
    ///
    /// A FACE-PARTITIONED wall (present in
    /// [`wall_face_partitions`](StoreyDoc::wall_face_partitions)) emits a SINGLE
    /// full-length `Unset` segment — just the base extent. Its 2-D face split and
    /// per-region exposure carve happen later in the resolve pass (from
    /// [`partition_face_regions`](StoreyDoc::partition_face_regions)), not here in
    /// the 1-D coverage path.
    pub fn wall_exposure_segments(&self) -> Vec<(Point2f64, Point2f64, WallExposure)> {
        self.outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .flat_map(|(vid, a, b)| {
                let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                let lerp = move |d: f32| {
                    let t = if len > 1e-6 { d / len } else { 0.0 };
                    Point2f64::new(
                        (a.x + t * (b.x - a.x)) as f64,
                        (a.y + t * (b.y - a.y)) as f64,
                    )
                };
                // Partitioned walls bypass the 1-D section expansion: one
                // full-length Unset base segment; the 2-D carve is resolve's job.
                if self.is_wall_partitioned(vid) {
                    return vec![(lerp(0.0), lerp(len), WallExposure::Unset)];
                }
                let sections = self
                    .wall_sections
                    .get(&vid)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                wall_coverage(len, sections)
                    .into_iter()
                    .map(move |(s, e, exposure)| (lerp(s), lerp(e), exposure))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// Recover the perimeter wall + along-wall span a resolved `WallRegion`
    /// covers: `(vid, start_m, end_m, len)`, where `len` is the matched
    /// perimeter wall's full length and `[start_m, end_m]` is this region's
    /// extent along it. `None` when the region's base edge lies on NO current
    /// perimeter wall — i.e. it's a riser. The shared base-edge → perimeter
    /// match used by [`sections_from_wall_regions`](Self::sections_from_wall_regions)
    /// (recovering painted sections) and by the shell to repaint just one
    /// region's span via [`set_wall_section`](Self::set_wall_section).
    ///
    /// Uses the TRUE footprint base edge a→b (via `base_frame`), NOT raw
    /// `exterior[0]/[1]`: over a floor-step-crossed wall the base is a stepped
    /// profile, so `exterior[1]` is the first breakpoint, not the far endpoint —
    /// reading it raw would recover the span as `0..step` and drop the rest.
    pub fn recover_wall_region_span(
        &self,
        region: &WallRegion,
    ) -> Option<(VertexId, f32, f32, f32)> {
        let perim = self.outline.floor_plan.perimeter_walls();
        let (origin, (ux, uy), wlen, _base_z) = region.base_frame()?;
        let p0 = Point2f32::new(origin.x as f32, origin.y as f32);
        let p1 = Point2f32::new((origin.x + ux * wlen) as f32, (origin.y + uy * wlen) as f32);
        let mid = Point2f32::new((p0.x + p1.x) * 0.5, (p0.y + p1.y) * 0.5);
        let (vid, a, b) = perim.iter().copied().find(|(_, a, b)| {
            domain::points::point_to_segment_distance(
                mid.x as f64,
                mid.y as f64,
                a.x as f64,
                a.y as f64,
                b.x as f64,
                b.y as f64,
            ) < 1e-2
        })?;
        let len = segment_len(a, b);
        let d0 = project_distance_along(a, b, len, p0.x, p0.y);
        let d1 = project_distance_along(a, b, len, p1.x, p1.y);
        Some((vid, d0.min(d1), d0.max(d1), len))
    }

    /// Rebuild the runtime section model from the persisted `WallRegions` (the
    /// source of truth) on load. Each region's base edge is projected onto the
    /// perimeter wall it lies on to recover its `(start_m, end_m, exposure)`;
    /// regions are grouped by wall and sorted. `Unset` and `Ground` regions are
    /// SKIPPED: `Unset` is derived coverage (recovering it would mark every
    /// wall after one save/load) and `Ground` is riser-only (the assessor's
    /// explicit choice in the riser toolbar — recovering it would turn risers
    /// into phantom painted sections). Riser regions never recover anyway —
    /// their base edges lie on no perimeter wall, so the geometric match below
    /// drops them whatever their exposure (that's what keeps an overridden
    /// Exposed/Semi/NonHeatLoss riser out too); the `Ground` skip is
    /// belt-and-braces on top. Inverse of the commit-time derivation.
    pub fn sections_from_wall_regions(
        &self,
        walls: &WallRegions,
    ) -> HashMap<VertexId, Vec<ExposureSection>> {
        let mut out: HashMap<VertexId, Vec<ExposureSection>> = HashMap::new();
        for region in &walls.regions {
            if matches!(region.exposure, WallExposure::Unset | WallExposure::Ground) {
                continue; // derived, not painted — never recovers to sections
            }
            let Some((vid, start_m, end_m, _len)) = self.recover_wall_region_span(region) else {
                continue; // region's base edge isn't on any current wall (a riser)
            };
            out.entry(vid).or_default().push(ExposureSection {
                start_m,
                end_m,
                exposure: region.exposure,
            });
        }
        for secs in out.values_mut() {
            secs.sort_by(|x, y| {
                x.start_m
                    .partial_cmp(&y.start_m)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }
        out
    }

    /// The resolved storey's derived RISER walls, projected to their plan
    /// base edges — the data behind the 2D dashed-line riser picker.
    ///
    /// DISCRIMINATOR (geometric, deliberately not exposure-based — a riser's
    /// exposure can be overridden away from the `Unset` default, so exposure
    /// can't identify one): a wall region whose base edge does NOT lie on
    /// any current outline perimeter wall is a riser. The midpoint-on-wall
    /// test and 1e-2 m tolerance are exactly the recovery matching of
    /// [`sections_from_wall_regions`](Self::sections_from_wall_regions);
    /// perimeter regions always pass it by construction (their bases are cut
    /// from the perimeter walls), and `resolve_storey` only ever appends
    /// riser faces beyond them (riser faces never emit on outline edges —
    /// those border one region).
    ///
    /// KIND: a riser whose base edge matches one of `storey.floors`'
    /// re-derived floor riser faces (same edge identity as the amend
    /// matching) is a `Floor` riser; everything else is `Ceiling`. The floor
    /// faces re-derive deterministically, so the match is exact.
    pub fn riser_walls(&self, storey: &Storey) -> Vec<RiserWall> {
        const EPS_M: f64 = 1e-2;
        let perim = self.outline.floor_plan.perimeter_walls();
        let floor_bases: Vec<(Point2f64, Point2f64)> = storey
            .floors
            .riser_faces()
            .iter()
            .filter(|f| f.exterior.len() >= 2)
            .map(|f| {
                (
                    Point2f64::new(f.exterior[0].x, f.exterior[0].y),
                    Point2f64::new(f.exterior[1].x, f.exterior[1].y),
                )
            })
            .collect();
        let mut out = Vec::new();
        for region in &storey.walls.regions {
            // The TRUE footprint base edge a→b (via `base_frame`), NOT raw
            // `exterior[0]/[1]`: a riser face is a 2-vertex base so the two
            // agree for risers, but routing through `base_frame` keeps the
            // perimeter-exclusion test correct for stepped perimeter walls
            // and avoids relying on the raw-index assumption elsewhere.
            let Some((origin, (ux, uy), wlen, _base_z)) = region.base_frame() else {
                continue;
            };
            let a = origin;
            let b = Point2f64::new(origin.x + ux * wlen, origin.y + uy * wlen);
            let mid = Point2f64::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            let on_perimeter = perim.iter().any(|(_, pa, pb)| {
                domain::points::point_to_segment_distance(
                    mid.x,
                    mid.y,
                    pa.x as f64,
                    pa.y as f64,
                    pb.x as f64,
                    pb.y as f64,
                ) < EPS_M
            });
            if on_perimeter {
                continue; // a perimeter wall region, not a riser
            }
            let kind = if floor_bases
                .iter()
                .any(|(fa, fb)| plan_edges_match(a, b, *fa, *fb, EPS_M))
            {
                RiserKind::Floor
            } else {
                RiserKind::Ceiling
            };
            out.push(RiserWall {
                id: region.id,
                a,
                b,
                exposure: region.exposure,
                kind,
            });
        }
        out
    }

    /// Paint every `Unset` coverage RANGE on the outline `Exposed` — whole
    /// unmarked walls AND the gaps around painted sections (the gap case is
    /// what makes this work after a reload, where gaps are the only `Unset`
    /// representation). Returns whether anything changed (so the shell can
    /// skip undo/persist).
    pub fn set_all_remaining_exposed(&mut self) -> bool {
        let mut changed = false;
        for (vid, a, b) in self.outline.floor_plan.perimeter_walls() {
            // A FACE-PARTITIONED wall is classified by its 2-D regions, not 1-D
            // sections — skip it. Painting a section here would both miscount it
            // as "remaining" and corrupt it (it has no `wall_sections`).
            if self.wall_face_partitions.contains_key(&vid) {
                continue;
            }
            let len = segment_len(a, b);
            let unset: Vec<(f32, f32)> = wall_coverage(
                len,
                self.wall_sections
                    .get(&vid)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
            .into_iter()
            .filter(|(s, e, exp)| *exp == WallExposure::Unset && e - s > 1e-4)
            .map(|(s, e, _)| (s, e))
            .collect();
            for (s, e) in unset {
                self.set_wall_section(vid, s, e, len, WallExposure::Exposed);
                changed = true;
            }
        }
        changed
    }

    /// True when every outline perimeter wall is fully classified — no `Unset`
    /// coverage range remains (a whole unmarked wall OR a gap around painted
    /// sections both count as `Unset`, the inverse of what
    /// [`set_all_remaining_exposed`](Self::set_all_remaining_exposed) would
    /// paint). An outline with no perimeter walls (nothing drawn yet) is NOT
    /// complete. Reads the outline SLOT, so the active storey must be synced
    /// (`sync_active`, which `materialize` runs) before this is meaningful.
    pub fn exposure_complete(&self) -> bool {
        let walls = self.outline.floor_plan.perimeter_walls();
        if walls.is_empty() {
            return false;
        }
        walls.into_iter().all(|(vid, a, b)| {
            let len = segment_len(a, b);
            // A FACE-PARTITIONED wall is complete iff none of its face regions is
            // left `Unset`. The stored samples/cuts carry ABSOLUTE heights, so
            // subdivide against a face height tall enough to contain them all
            // (region arrangement is height-invariant; the height only has to
            // keep every sample inside its region).
            if let Some(part) = self.wall_face_partitions.get(&vid) {
                let mut h = 1.0_f32;
                for c in &part.cuts {
                    h = h.max(c.a.y).max(c.b.y);
                }
                for (s, _) in &part.regions {
                    h = h.max(s.y);
                }
                h += 1.0;
                return self
                    .partition_face_regions(vid, len, h)
                    .iter()
                    .all(|(_, e)| *e != WallExposure::Unset);
            }
            wall_coverage(
                len,
                self.wall_sections
                    .get(&vid)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )
            .into_iter()
            // An `Unset` remnant only counts if it has real length (mirrors the
            // `e - s > 1e-4` filter `set_all_remaining_exposed` paints against).
            .all(|(s, e, exp)| exp != WallExposure::Unset || e - s <= 1e-4)
        })
    }
}

/// Plan ring (world metres) of a ceiling region — its `Polygon3D` exterior
/// projected to xy. `< 3` vertices → empty.
fn ceiling_ring_xy(region: &ber_model::ber::structural::CeilingRegion) -> Vec<Point2f64> {
    region
        .polygon
        .exterior
        .iter()
        .map(|v| Point2f64::new(v.x, v.y))
        .collect()
}

/// Tolerance for the conservative full-containment overlap test (metres): a
/// region vertex within this of the neighbour footprint's edge still counts as
/// contained, so a shared/coincident stack boundary reads as fully overlapping.
const CONTAINMENT_TOL_M: f64 = 0.05;

/// Auto-derive this storey's floor/ceiling exposure from the resolved stack and
/// write it into the StoreyDoc maps — a controller-driven pre-pass run AFTER all
/// storeys resolve (it needs the neighbours' resolved footprints). Precedence is
/// **manual > auto > Unset**: every write is `entry(key).or_insert(derived)`, so
/// a value the assessor set manually is never clobbered.
///
/// Derivation (CONSERVATIVE FULL-CONTAINMENT — a region only auto-classifies as
/// Party / NonHeatLoss when it sits ENTIRELY over the neighbour footprint;
/// partial overlap from a cantilever/setback stays absent → resolves `Unset`,
/// left for manual marking):
///  - a floor fully covered by the storey BELOW → `NonHeatLoss` (party floor to
///    the dwelling below); the lowest floor is left `Unset` for the assessor;
///  - an intermediate ceiling fully covered by the storey ABOVE → `Party` (party
///    ceiling to the dwelling above); the TOP ceiling is left `Unset` for the
///    assessor (it could be an exposed roof OR sit under an unheated attic).
pub fn auto_derive_horizontal_exposure(
    doc: &mut StoreyDoc,
    ber: &Ber,
    idx: usize,
    n_storeys: usize,
) {
    let Some(storey) = ber.storeys.get(idx) else {
        return;
    };
    // Floors: a floor fully over the storey BELOW is a party floor (NonHeatLoss).
    // The lowest storey's floor is deliberately NOT auto-classified — "ground"
    // isn't always right (it could sit over a basement/garage, or be exposed), so
    // it's left `Unset` for the assessor to label in the floor-exposure view.
    let below_footprint = idx
        .checked_sub(1)
        .and_then(|b| ber.storeys.get(b))
        .and_then(|s| s.floors.footprint_ring());
    let floor_rings = storey.floors.to_vertex_lists();
    for (region, ring) in storey.floors.regions.iter().zip(&floor_rings) {
        let derived = below_footprint
            .as_ref()
            .filter(|below| ring_within_ring(ring, below, CONTAINMENT_TOL_M))
            .map(|_| FloorExposure::NonHeatLoss);
        if let Some(e) = derived {
            doc.floor_exposure
                .entry(centroid_key(region.centroid()))
                .or_insert(e);
        }
    }
    // Ceilings: an intermediate ceiling fully under the storey above is a party
    // ceiling. The TOP storey's ceiling is deliberately NOT auto-classified —
    // it could be an exposed roof OR sit under an unheated attic, so it's left
    // `Unset` for the assessor to label in the ceiling-exposure view (mirrors
    // the lowest floor).
    let above_footprint = (idx + 1 < n_storeys)
        .then(|| ber.storeys.get(idx + 1))
        .flatten()
        .and_then(|s| s.floors.footprint_ring());
    for region in &storey.roofs.regions {
        let ring = ceiling_ring_xy(region);
        let derived = above_footprint
            .as_ref()
            .filter(|above| ring_within_ring(&ring, above, CONTAINMENT_TOL_M))
            .map(|_| CeilingExposure::Party);
        if let Some(e) = derived {
            doc.ceiling_exposure
                .entry(centroid_key(region.centroid()))
                .or_insert(e);
        }
    }
}

/// True when storey `idx`'s horizontal surfaces are fully classified: EVERY
/// resolved floor region AND every ceiling/roof region carries a non-`Unset`
/// exposure. An empty / missing storey (or one with no floor or no ceiling
/// regions) is NOT complete. Reads the RESOLVED storey (Store A) — the
/// checklist's per-storey "Surfaces" rollup, kept separate from wall Exposures.
pub fn surfaces_exposure_complete(ber: &Ber, idx: usize) -> bool {
    let Some(storey) = ber.storeys.get(idx) else {
        return false;
    };
    if storey.floors.regions.is_empty() || storey.roofs.regions.is_empty() {
        return false;
    }
    storey
        .floors
        .regions
        .iter()
        .all(|r| r.exposure != FloorExposure::Unset)
        && storey
            .roofs
            .regions
            .iter()
            .all(|r| r.exposure != CeilingExposure::Unset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sec(start: f32, end: f32, e: WallExposure) -> ExposureSection {
        ExposureSection {
            start_m: start,
            end_m: end,
            exposure: e,
        }
    }

    #[test]
    fn paint_mid_range_splits_into_three() {
        let mut s = vec![sec(0.0, 5.0, WallExposure::Exposed)];
        paint_section(&mut s, 2.0, 3.0, WallExposure::SemiExposed);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], sec(0.0, 2.0, WallExposure::Exposed));
        assert_eq!(s[1], sec(2.0, 3.0, WallExposure::SemiExposed));
        assert_eq!(s[2], sec(3.0, 5.0, WallExposure::Exposed));
    }

    #[test]
    fn paint_whole_wall_is_one_section() {
        let mut s = Vec::new();
        paint_section(&mut s, 0.0, 4.0, WallExposure::NonHeatLoss);
        assert_eq!(s, vec![sec(0.0, 4.0, WallExposure::NonHeatLoss)]);
    }

    #[test]
    fn overlapping_repaint_overrides_the_overlap() {
        let mut s = vec![sec(0.0, 4.0, WallExposure::Exposed)];
        paint_section(&mut s, 1.0, 2.0, WallExposure::SemiExposed);
        // now exposed[0,1], semi[1,2], exposed[2,4]
        paint_section(&mut s, 1.5, 3.0, WallExposure::NonHeatLoss);
        // semi shrinks to [1,1.5], the [1.5,2] exposed-middle... actually [1.5,3]
        // wins: exposed[0,1], semi[1,1.5], non[1.5,3], exposed[3,4]
        assert_eq!(s[0], sec(0.0, 1.0, WallExposure::Exposed));
        assert_eq!(s[1], sec(1.0, 1.5, WallExposure::SemiExposed));
        assert_eq!(s[2], sec(1.5, 3.0, WallExposure::NonHeatLoss));
        assert_eq!(s[3], sec(3.0, 4.0, WallExposure::Exposed));
    }

    #[test]
    fn coverage_fills_gaps_with_unset() {
        let s = vec![sec(2.0, 3.0, WallExposure::SemiExposed)];
        let cov = wall_coverage(5.0, &s);
        assert_eq!(
            cov,
            vec![
                (0.0, 2.0, WallExposure::Unset),
                (2.0, 3.0, WallExposure::SemiExposed),
                (3.0, 5.0, WallExposure::Unset),
            ]
        );
    }

    #[test]
    fn coverage_of_no_sections_is_all_unset() {
        assert_eq!(
            wall_coverage(4.0, &[]),
            vec![(0.0, 4.0, WallExposure::Unset)]
        );
    }

    /// A finalised 4×4 m square StoreyDoc for the model-level tests below.
    fn square_doc() -> crate::StoreyDoc {
        use domain::points::Point2f32;
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ];
        let plan = manip_plan::FloorPlan::from_clean_ring(&pts).unwrap();
        let mut doc = crate::StoreyDoc::default();
        doc.dimensioning.scale_set = true;
        doc.floor_plan = plan.clone();
        doc.outline = crate::ShapeSlot {
            floor_plan: plan,
            dimensioning: doc.dimensioning.clone(),
        };
        doc
    }

    /// SINGLE-REPRESENTATION RULE: painting part of a wall stores ONLY the
    /// painted section — the gaps are never materialized into sections; they
    /// exist only as `Unset` ranges in the derived coverage.
    #[test]
    fn set_wall_section_stores_only_painted_sections() {
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);

        let secs = &doc.wall_sections[&vid];
        assert_eq!(secs, &vec![sec(1.0, 3.0, WallExposure::SemiExposed)]);
        assert_eq!(
            wall_coverage(len, secs),
            vec![
                (0.0, 1.0, WallExposure::Unset),
                (1.0, 3.0, WallExposure::SemiExposed),
                (3.0, len, WallExposure::Unset),
            ]
        );
    }

    /// `wall_exposure_segments` covers the FULL perimeter: unmarked walls
    /// yield one full-length `Unset` segment; a partially marked wall yields
    /// its painted section plus `Unset` gaps.
    #[test]
    fn wall_exposure_segments_cover_the_full_perimeter() {
        let mut doc = square_doc();
        // Nothing painted → one Unset segment per wall, tiling the perimeter.
        let segs = doc.wall_exposure_segments();
        assert_eq!(segs.len(), 4);
        assert!(segs.iter().all(|(_, _, e)| *e == WallExposure::Unset));
        let total: f64 = segs
            .iter()
            .map(|(a, b, _)| ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt())
            .sum();
        assert!((total - 16.0).abs() < 1e-6, "tiles the 4×4 perimeter");

        // Paint the middle of wall0 → its segment splits into gap/mark/gap.
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);
        let segs = doc.wall_exposure_segments();
        assert_eq!(segs.len(), 6, "3 ranges on wall0 + 1 per other wall");
        assert_eq!(
            segs.iter()
                .filter(|(_, _, e)| *e == WallExposure::SemiExposed)
                .count(),
            1
        );
        assert_eq!(
            segs.iter()
                .filter(|(_, _, e)| *e == WallExposure::Unset)
                .count(),
            5
        );
    }

    /// A face-PARTITIONED wall emits exactly ONE full-length `Unset` segment,
    /// regardless of any prior 1-D sections — vs. the multi-range expansion a
    /// sectioned (non-partitioned) wall yields. The 2-D carve is resolve's job.
    #[test]
    fn partitioned_wall_emits_single_unset_segment() {
        use crate::PartitionCut;
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        // Sectioned wall: a painted mid-range expands to gap/mark/gap (3 ranges).
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);
        let n_sectioned = doc
            .wall_exposure_segments()
            .into_iter()
            .filter(|(p, q, _)| {
                // segments belonging to wall0 lie along its span
                domain::points::point_to_segment_distance(
                    p.x, p.y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                ) < 1e-6
                    && domain::points::point_to_segment_distance(
                        q.x, q.y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                    ) < 1e-6
            })
            .count();
        assert_eq!(n_sectioned, 3, "sectioned wall expands to 3 ranges");

        // Partition the wall → commit clears its sections; now ONE Unset segment.
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 1.0),
                b: Point2f32::new(len, 1.0),
            },
        );
        let wall0_segs: Vec<_> = doc
            .wall_exposure_segments()
            .into_iter()
            .filter(|(p, q, _)| {
                domain::points::point_to_segment_distance(
                    p.x, p.y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                ) < 1e-6
                    && domain::points::point_to_segment_distance(
                        q.x, q.y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                    ) < 1e-6
            })
            .collect();
        assert_eq!(wall0_segs.len(), 1, "partitioned wall → single segment");
        assert_eq!(wall0_segs[0].2, WallExposure::Unset);
    }

    /// "Set remaining → Exposed" paints every Unset coverage range: whole
    /// unmarked walls AND the gaps around painted sections. Idempotent.
    #[test]
    fn set_all_remaining_exposed_paints_unset_walls_and_gaps() {
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);

        assert!(doc.set_all_remaining_exposed(), "something to paint");
        // Wall0: the mark survives, its two gaps are now explicit Exposed.
        let secs = &doc.wall_sections[&vid];
        assert_eq!(secs.len(), 3);
        assert_eq!(secs[0], sec(0.0, 1.0, WallExposure::Exposed));
        assert_eq!(secs[1], sec(1.0, 3.0, WallExposure::SemiExposed));
        assert_eq!(secs[2], sec(3.0, len, WallExposure::Exposed));
        // Other walls: one full-length Exposed section each.
        for (v, a, b) in doc.outline.floor_plan.perimeter_walls() {
            if v == vid {
                continue;
            }
            let secs = &doc.wall_sections[&v];
            assert_eq!(secs.len(), 1);
            assert_eq!(secs[0], sec(0.0, segment_len(a, b), WallExposure::Exposed));
        }
        // Nothing Unset remains → a second pass changes nothing.
        assert!(!doc.set_all_remaining_exposed(), "idempotent");
    }

    use ber_model::ber::structural::{CeilingRegions, FloorRegions, Storey};

    fn square_ring(off: f64, size: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(off, 0.0),
            Point2f64::new(off + size, 0.0),
            Point2f64::new(off + size, size),
            Point2f64::new(off, size),
        ]
    }

    /// Build a storey whose floor + ceiling are the single ring `footprint`.
    fn storey_with(footprint: Vec<Point2f64>) -> Storey {
        let cid = ItemId::default();
        Storey {
            floors: FloorRegions::from_vertex_lists(vec![footprint.clone()], cid),
            roofs: CeilingRegions::default_for_footprint(vec![footprint], 2.4, cid),
            ..Storey::default()
        }
    }

    /// Auto-derive on a clean 2-storey stack: the lowest (storey-0) floor is NOT
    /// auto-classified (left Unset for the assessor — "ground" isn't always
    /// right), and neither is the top (storey-1) ceiling (it could be an exposed
    /// roof OR under an unheated attic). The intermediate surfaces between the
    /// two FULLY-overlapping footprints derive Party (ceiling under the upper)
    /// and NonHeatLoss (floor over the lower).
    #[test]
    fn auto_derive_ground_roof_and_party_on_a_clean_stack() {
        let mut ber = Ber::default();
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));

        let mut d0 = StoreyDoc::default();
        let mut d1 = StoreyDoc::default();
        auto_derive_horizontal_exposure(&mut d0, &ber, 0, 2);
        auto_derive_horizontal_exposure(&mut d1, &ber, 1, 2);

        let f0 = centroid_key(ber.storeys[0].floors.regions[0].centroid());
        let c0 = centroid_key(ber.storeys[0].roofs.regions[0].centroid());
        let f1 = centroid_key(ber.storeys[1].floors.regions[0].centroid());
        let c1 = centroid_key(ber.storeys[1].roofs.regions[0].centroid());

        assert_eq!(
            d0.floor_exposure.get(&f0),
            None,
            "the lowest floor is left for the assessor, not auto-classified"
        );
        assert_eq!(d0.ceiling_exposure.get(&c0), Some(&CeilingExposure::Party));
        assert_eq!(
            d1.floor_exposure.get(&f1),
            Some(&FloorExposure::NonHeatLoss)
        );
        assert_eq!(
            d1.ceiling_exposure.get(&c1),
            None,
            "the top ceiling is left for the assessor, not auto-classified"
        );
    }

    /// Auto-derive never clobbers a manual value: a manual Exposed floor on the
    /// upper storey stays Exposed even though the auto-pass would write
    /// NonHeatLoss (it sits fully over the storey below).
    #[test]
    fn auto_derive_does_not_clobber_a_manual_value() {
        let mut ber = Ber::default();
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));

        let mut doc = StoreyDoc::default();
        let key = centroid_key(ber.storeys[1].floors.regions[0].centroid());
        doc.floor_exposure.insert(key, FloorExposure::Exposed);

        auto_derive_horizontal_exposure(&mut doc, &ber, 1, 2);
        assert_eq!(
            doc.floor_exposure.get(&key),
            Some(&FloorExposure::Exposed),
            "manual override wins over the auto NonHeatLoss"
        );
    }

    /// PARTIAL overlap stays Unset (absent): an upper storey whose footprint
    /// only partly covers the lower (a cantilever / setback) does NOT
    /// auto-classify the lower floor as NonHeatLoss — it's left for manual
    /// marking. The lower floor (storey 1 here) overhangs the storey below
    /// (storey 0), so the conservative full-containment test fails.
    #[test]
    fn auto_derive_partial_overlap_stays_unset() {
        let mut ber = Ber::default();
        // Storey 0 footprint is the small 0..4 square; storey 1 is a LARGER
        // 0..6 square that overhangs it — so storey-1's floor is NOT fully
        // within storey-0's footprint.
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));
        ber.storeys.push(storey_with(square_ring(0.0, 6.0)));

        let mut d1 = StoreyDoc::default();
        auto_derive_horizontal_exposure(&mut d1, &ber, 1, 2);
        let f1 = centroid_key(ber.storeys[1].floors.regions[0].centroid());
        assert_eq!(
            d1.floor_exposure.get(&f1),
            None,
            "partial overlap leaves the floor absent → resolves Unset"
        );
    }

    /// `set_floor_exposure_at` / `set_ceiling_exposure_at` round-trip: a tap
    /// inside a region writes the centroid-keyed override and returns the key;
    /// a tap outside any region returns None and writes nothing.
    #[test]
    fn set_floor_ceiling_exposure_at_round_trips_via_centroid() {
        let storey = storey_with(square_ring(0.0, 4.0));
        let mut doc = StoreyDoc::default();

        let fkey = doc
            .set_floor_exposure_at(&storey, 2.0, 2.0, FloorExposure::Ground)
            .expect("a floor region under (2, 2)");
        assert_eq!(fkey, centroid_key(storey.floors.regions[0].centroid()));
        assert_eq!(doc.floor_exposure.get(&fkey), Some(&FloorExposure::Ground));

        let ckey = doc
            .set_ceiling_exposure_at(&storey, 2.0, 2.0, CeilingExposure::ExposedRoof)
            .expect("a ceiling region under (2, 2)");
        assert_eq!(
            doc.ceiling_exposure.get(&ckey),
            Some(&CeilingExposure::ExposedRoof)
        );

        // A tap outside the footprint hits nothing and writes nothing.
        assert!(doc
            .set_floor_exposure_at(&storey, 99.0, 99.0, FloorExposure::Exposed)
            .is_none());
        assert_eq!(doc.floor_exposure.len(), 1, "no spurious write");
    }

    /// `surfaces_exposure_complete` requires EVERY floor AND ceiling region
    /// non-Unset; an Unset region or an empty storey fails.
    #[test]
    fn surfaces_exposure_complete_requires_all_surfaces_classified() {
        let mut ber = Ber::default();
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));
        // Fresh storey: every surface Unset → not complete.
        assert!(!surfaces_exposure_complete(&ber, 0));

        // Classify the floor only → still not complete (ceiling Unset).
        ber.storeys[0].floors.regions[0].exposure = FloorExposure::Ground;
        assert!(!surfaces_exposure_complete(&ber, 0));

        // Classify the ceiling too → complete.
        ber.storeys[0].roofs.regions[0].exposure = CeilingExposure::ExposedRoof;
        assert!(surfaces_exposure_complete(&ber, 0));

        // Empty / missing storey → never complete.
        ber.storeys.push(Storey::default());
        assert!(!surfaces_exposure_complete(&ber, 1));
        assert!(!surfaces_exposure_complete(&ber, 9));
    }

    /// A FACE-PARTITIONED wall is owned by the 2-D partition, not the 1-D
    /// "remaining → Exposed" machinery: it's excluded from the bulk paint (which
    /// must not corrupt it), and `exposure_complete` keys off whether its face
    /// regions are all classified — not its (cleared) `wall_sections`. Regression
    /// for the "partition a wall → it shows as 1 remaining" report.
    #[test]
    fn partitioned_wall_excluded_from_remaining_and_completes_when_set() {
        let mut doc = square_doc();
        let walls = doc.outline.floor_plan.perimeter_walls();
        let (vid, a, b) = walls[0];
        let len = segment_len(a, b);
        // Classify the other three walls fully Exposed.
        for (v, a2, b2) in &walls {
            if *v != vid {
                let l = segment_len(*a2, *b2);
                doc.set_wall_section(*v, 0.0, l, l, WallExposure::Exposed);
            }
        }
        // Partition wall 0 with a vertical cut into two halves (sections cleared).
        doc.commit_partition_cut(
            vid,
            crate::PartitionCut {
                a: Point2f32::new(len * 0.5, 0.0),
                b: Point2f32::new(len * 0.5, 2.4),
            },
        );
        // One half still Unset → storey not complete; the bulk button leaves the
        // partitioned wall alone (no spurious section, no change).
        assert!(
            !doc.exposure_complete(),
            "an unset face region blocks completion"
        );
        assert!(
            !doc.set_all_remaining_exposed(),
            "partitioned wall isn't bulk-painted"
        );
        assert!(
            !doc.wall_sections.contains_key(&vid),
            "partitioned wall stays section-free"
        );
        // Set both halves → the wall (and storey) is complete.
        doc.set_partition_region_exposure(
            vid,
            Point2f32::new(len * 0.25, 1.0),
            WallExposure::Exposed,
            len,
            2.4,
        );
        doc.set_partition_region_exposure(
            vid,
            Point2f32::new(len * 0.75, 1.0),
            WallExposure::SemiExposed,
            len,
            2.4,
        );
        assert!(
            doc.exposure_complete(),
            "all face regions classified → complete"
        );
    }

    // ---- Stage 1: inter-storey (party) auto-exposure pass ---------------

    /// `auto_mark_party_walls` marks ONLY the wall sub-range that lies over the
    /// below footprint, as `NonHeatLoss`; the rest of the wall stays `Unset`.
    /// The 4×4 square's wall 0 is the bottom edge (0,0)→(4,0); a below footprint
    /// covering its left half (x∈[0,2]) makes only [0,2] m a party wall.
    #[test]
    fn auto_mark_party_walls_marks_only_the_shared_subrange() {
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        assert!((len - 4.0).abs() < 1e-6, "wall 0 is the 4 m bottom edge");
        // Below footprint = a band straddling wall 0 over its [0.5,2.5] m run,
        // inset from the side walls (x=0 / x=4) so only wall 0 is shared.
        let below = vec![
            Point2f64::new(0.5, -1.0),
            Point2f64::new(2.5, -1.0),
            Point2f64::new(2.5, 1.0),
            Point2f64::new(0.5, 1.0),
        ];
        doc.auto_mark_party_walls(Some(&below), None);

        let cov = wall_coverage(len, &doc.wall_sections[&vid]);
        // [0,0.5] Unset, [0.5,2.5] NonHeatLoss, [2.5,4] Unset.
        assert_eq!(cov.len(), 3);
        assert_eq!(cov[0].2, WallExposure::Unset);
        assert_eq!(cov[1], (0.5, 2.5, WallExposure::NonHeatLoss));
        assert_eq!(cov[2].2, WallExposure::Unset);
        // Walls 1/2/3 don't overlap the band → no sections written.
        for (v, _, _) in doc.outline.floor_plan.perimeter_walls() {
            if v != vid {
                assert!(
                    !doc.wall_sections.contains_key(&v),
                    "untouched wall stays bare"
                );
            }
        }
    }

    /// A FACE-PARTITIONED wall is skipped by `auto_mark_party_walls` — it's owned
    /// by its 2-D partition, never the 1-D party pass.
    #[test]
    fn auto_mark_party_walls_skips_partitioned_wall() {
        use crate::PartitionCut;
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.commit_partition_cut(
            vid,
            PartitionCut {
                a: Point2f32::new(0.0, 1.0),
                b: Point2f32::new(len, 1.0),
            },
        );
        // A below footprint that fully covers wall 0.
        let below = vec![
            Point2f64::new(-1.0, -1.0),
            Point2f64::new(5.0, -1.0),
            Point2f64::new(5.0, 1.0),
            Point2f64::new(-1.0, 1.0),
        ];
        doc.auto_mark_party_walls(Some(&below), None);
        assert!(
            !doc.wall_sections.contains_key(&vid),
            "partitioned wall is never section-painted by the party pass"
        );
    }

    /// An EXISTING user mark on part of a wall is preserved: the party pass fills
    /// only the still-`Unset` remainder, never overwriting the manual section.
    #[test]
    fn auto_mark_party_walls_preserves_existing_marks() {
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        // Assessor marked [0,1] Exposed by hand.
        doc.set_wall_section(vid, 0.0, 1.0, len, WallExposure::Exposed);
        // Below footprint fully covers wall 0.
        let below = vec![
            Point2f64::new(-1.0, -1.0),
            Point2f64::new(5.0, -1.0),
            Point2f64::new(5.0, 1.0),
            Point2f64::new(-1.0, 1.0),
        ];
        doc.auto_mark_party_walls(Some(&below), None);

        let cov = wall_coverage(len, &doc.wall_sections[&vid]);
        assert_eq!(
            cov[0],
            (0.0, 1.0, WallExposure::Exposed),
            "manual mark kept"
        );
        assert_eq!(
            cov[1],
            (1.0, 4.0, WallExposure::NonHeatLoss),
            "only the Unset remainder filled party"
        );
    }

    /// `plan_party_floor_carves` on a PARTIAL overlap returns one split ring whose
    /// area equals the overlap, plus a sample inside that overlap. The 4×4 storey
    /// over a below footprint covering its left half (x∈[0,2]) carves an 8 m²
    /// party piece.
    #[test]
    fn plan_party_floor_carves_returns_overlap_ring_and_sample() {
        let doc = square_doc();
        let storey = storey_with(square_ring(0.0, 4.0));
        let below = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let (rings, samples) = doc.plan_party_floor_carves(&storey, &below);
        assert_eq!(rings.len(), 1, "one party piece");
        assert_eq!(samples.len(), 1);
        assert!(
            (ring_area(&rings[0]) - 8.0).abs() < 1e-3,
            "the party piece is the left half (8 m²)"
        );
        // The sample sits in the overlap (left half: x < 2).
        assert!(samples[0].x < 2.0 && samples[0].x > 0.0);
    }

    /// FULL containment carves nothing — the full-containment auto-derive owns it.
    #[test]
    fn plan_party_floor_carves_full_containment_returns_none() {
        let doc = square_doc();
        let storey = storey_with(square_ring(0.0, 4.0));
        // Below footprint fully contains the 4×4 floor.
        let below = square_ring(-1.0, 6.0);
        let (rings, samples) = doc.plan_party_floor_carves(&storey, &below);
        assert!(
            rings.is_empty() && samples.is_empty(),
            "no carve when contained"
        );
    }

    /// The ceiling mirror: a partial overlap with the storey ABOVE carves the
    /// party ceiling piece.
    #[test]
    fn plan_party_ceiling_carves_returns_overlap_piece() {
        let doc = square_doc();
        let storey = storey_with(square_ring(0.0, 4.0));
        let above = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let (rings, samples) = doc.plan_party_ceiling_carves(&storey, &above);
        assert_eq!(rings.len(), 1);
        assert!((ring_area(&rings[0]) - 8.0).abs() < 1e-3);
        assert!(samples[0].x < 2.0);
    }
}
