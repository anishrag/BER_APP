//! Wall-exposure model operations: painting / clearing per-wall heat-loss
//! sections on the storey outline, deriving full coverage, and classifying
//! the resolved storey's derived RISER walls (the dashed-line picker's
//! data). Pure model logic — the shell owns pointer selection, undo, and
//! persistence.
use crate::{
    centroid_key, CentroidKey, ExposureSection, PartitionCut, StoreyDoc, WallFacePartition,
};
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::{
    CeilingExposure, FloorExposure, Storey, SurfaceKind, WallExposure, WallRegion, WallRegions,
};
use domain::points::{Point2f32, Point2f64};
use manip_plan::{
    project_distance_along, ring_contains_point, ring_within_ring, segment_len, SurfaceRef,
    VertexId,
};
use std::collections::HashMap;

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
    /// in the derived coverage instead. Single-representation rule, with ONE
    /// canonicalize pass: reload's `sections_from_wall_regions` coalesces
    /// adjacent sections whose paint AND construction agree (the painted-
    /// staircase heal) — so two same-exposure sections painted separately
    /// in-session merge into one on the next reload. An agreeing seam carries
    /// no durable payload, so nothing DEAP-relevant changes; only the
    /// tap-selectable sub-ranges unify.
    ///
    /// [`WallExposure::InterStorey`] is DERIVED-only (`derive_inter_storey`
    /// stamps and clears it) — painting it is refused as a no-op.
    pub fn set_wall_section(
        &mut self,
        vid: VertexId,
        start: f32,
        end: f32,
        len: f32,
        exposure: WallExposure,
    ) {
        if exposure == WallExposure::InterStorey {
            return; // derived-only classification — never assessor-painted
        }
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
    ///
    /// [`FloorExposure::InterStorey`] is DERIVED-only: passing it as `e` is
    /// refused (`None`, nothing written), and a tap on a region that currently
    /// resolves `InterStorey` is a no-op too (`None`) — the derivation is
    /// authoritative over that surface.
    pub fn set_floor_exposure_at(
        &mut self,
        storey: &Storey,
        x: f64,
        y: f64,
        e: FloorExposure,
    ) -> Option<CentroidKey> {
        if e == FloorExposure::InterStorey {
            return None; // derived-only classification — never assessor-set
        }
        let id = storey.surface_at(SurfaceKind::Floors, x, y, 0.0)?;
        let region = storey.floors.regions.iter().find(|r| r.id == id)?;
        if region.exposure == FloorExposure::InterStorey {
            return None; // the derivation owns this surface — refuse the write
        }
        let key = centroid_key(region.centroid());
        self.floor_exposure.insert(key, e);
        Some(key)
    }

    /// Set the CEILING exposure of the resolved ceiling/roof region under plan
    /// point `(x, y)` — the ceiling mirror of
    /// [`set_floor_exposure_at`](StoreyDoc::set_floor_exposure_at). `None` when
    /// no ceiling region is under the tap.
    ///
    /// [`CeilingExposure::InterStorey`] is DERIVED-only: passing it as `e` is
    /// refused (`None`, nothing written), and a tap on a region that currently
    /// resolves `InterStorey` is a no-op too (`None`) — the derivation is
    /// authoritative over that surface.
    pub fn set_ceiling_exposure_at(
        &mut self,
        storey: &Storey,
        x: f64,
        y: f64,
        e: CeilingExposure,
    ) -> Option<CentroidKey> {
        if e == CeilingExposure::InterStorey {
            return None; // derived-only classification — never assessor-set
        }
        let id = storey.surface_at(SurfaceKind::Ceilings, x, y, 0.0)?;
        let region = storey.ceilings.regions.iter().find(|r| r.id == id)?;
        if region.exposure == CeilingExposure::InterStorey {
            return None; // the derivation owns this surface — refuse the write
        }
        let key = centroid_key(region.centroid());
        self.ceiling_exposure.insert(key, e);
        Some(key)
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
    /// Matches the wall through the TRUE footprint base edge (via
    /// `base_frame`), NOT raw `exterior[0]/[1]` — over a floor-step-crossed
    /// wall the base is a stepped profile, so `exterior[1]` is the first
    /// breakpoint, not the far endpoint. The SPAN, however, is the region's
    /// FULL plan extent (every ring vertex projected onto the wall, clamped
    /// to it), not the base edge's: `base_frame` confines its far endpoint to
    /// the FLOOR BAND (within `MAX_FLOOR_STEP_M` of the base run), so a
    /// remainder whose bottom steps deeper than that — e.g. the L left by an
    /// inter-storey band notching >0.6 m above the wall base — has a
    /// deliberately TRUNCATED base edge. Its painted face still covers the
    /// whole extent; recovering only the truncated span would reload part of
    /// the wall `Unset` (exposure churn), and orientation-dependently so (a
    /// ring starting on the high shoulder truncates differently from its
    /// mirror wall). The clamp keeps a coplanar-flush MERGED guest's forward
    /// arm (which projects past the wall's ends) from stretching the span.
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
        let (mut d0, mut d1) = (f32::INFINITY, f32::NEG_INFINITY);
        for v in &region.polygon.exterior {
            let d = project_distance_along(a, b, len, v.x as f32, v.y as f32);
            d0 = d0.min(d);
            d1 = d1.max(d);
        }
        Some((vid, d0.max(0.0), d1.min(len), len))
    }

    /// After a splice (dormer extend / trim) has split one outline wall into two
    /// FLANKS, copy the source wall's per-wall attributes onto the flank that got
    /// a FRESHLY-MINTED start [`VertexId`], so both flanks — pieces of the SAME
    /// source wall — inherit its exposure and construction rather than only the
    /// flank that kept the original start vid.
    ///
    /// The side-tables key off the wall's start vid ([`wall_sections`],
    /// [`wall_face_partitions`]) or its [`SurfaceRef`] ([`wall_constructions`]).
    /// `splice_into_edge` keeps the near flank's start vid but mints a new one for
    /// the far flank, so that flank's start vid has no side-table entry and
    /// resolves to `Unset`/default — the bug this repairs.
    ///
    /// `old_perimeter` / `new_perimeter` are the ring-0 perimeter walls
    /// ([`FloorPlan::perimeter_walls`]) BEFORE and AFTER the splice, as
    /// `(start_vid, a, b)`. (The caller passes the POST-splice perimeter
    /// explicitly rather than letting this read `self.outline`, because the
    /// splice edits the live editing buffer — the outline slot isn't synced until
    /// the resolve pass, which runs *after* this.) A new perimeter edge whose
    /// start vid is absent from the old set is a fresh flank; the OLD edge whose
    /// segment its midpoint lies on (the 1e-2 m match used by
    /// [`recover_wall_region_span`](Self::recover_wall_region_span) and
    /// [`riser_walls`](Self::riser_walls)) is its source. The construction
    /// (position-independent) is cloned as-is; the wall-local exposure sections
    /// and face partition are TRANSLATED by the flank's start offset along the
    /// old edge and CLIPPED to the flank's length, so a partial paint lands on
    /// the flank(s) it actually covers (a `0..1` paint does NOT leak onto a far
    /// flank whose own span was unpainted). Both flanks inherit; the near flank
    /// keeps the un-touched old entry (`wall_coverage` clamps its shorter length).
    /// A new vid that already carries an entry is left alone. Openings are not
    /// touched.
    ///
    /// [`wall_sections`]: StoreyDoc::wall_sections
    /// [`wall_face_partitions`]: StoreyDoc::wall_face_partitions
    /// [`wall_constructions`]: StoreyDoc::wall_constructions
    /// [`FloorPlan::perimeter_walls`]: manip_plan::FloorPlan::perimeter_walls
    pub fn rehome_split_wall_attributes(
        &mut self,
        old_perimeter: &[(VertexId, Point2f32, Point2f32)],
        new_perimeter: &[(VertexId, Point2f32, Point2f32)],
    ) {
        let old_vids: std::collections::HashSet<VertexId> =
            old_perimeter.iter().map(|(v, _, _)| *v).collect();
        for (new_vid, a, b) in new_perimeter.iter().copied() {
            // Only freshly-minted flank starts need re-homing.
            if old_vids.contains(&new_vid) {
                continue;
            }
            let mid = Point2f32::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            let Some((old_vid, oa, ob)) = old_perimeter.iter().copied().find(|(_, oa, ob)| {
                domain::points::point_to_segment_distance(
                    mid.x as f64,
                    mid.y as f64,
                    oa.x as f64,
                    oa.y as f64,
                    ob.x as f64,
                    ob.y as f64,
                ) < 1e-2
            }) else {
                continue; // a genuinely new wall (not a flank of an old one)
            };
            // The flank is a SUB-SEGMENT of the old edge, starting `offset` metres
            // along it FROM THE OLD WALL'S START VERTEX. Wall-local coordinates
            // (1-D section ranges, 2-D face-partition along-x) are measured from
            // that start vertex, so a verbatim copy would place the paint at the
            // WRONG position on the flank (e.g. a `0..1` paint would show on a far
            // flank whose own span `3..4` was unpainted). Translate every
            // along-coordinate by −offset onto the flank and clip to its own
            // length, dropping ranges outside it. (The NEAR flank keeps the old
            // vid at offset 0; `wall_coverage` already clamps its now-shorter
            // length, so it needs no translation.) The splice preserves ring
            // winding, so the flank is same-oriented as the old edge and a plain
            // subtract is the correct along-mapping.
            let old_len = segment_len(oa, ob);
            let flank_len = segment_len(a, b);
            let offset = project_distance_along(oa, ob, old_len, a.x, a.y);

            // 1-D exposure sections: translate + clip, drop empties.
            if !self.wall_sections.contains_key(&new_vid) {
                if let Some(secs) = self.wall_sections.get(&old_vid) {
                    let moved: Vec<ExposureSection> = secs
                        .iter()
                        .filter_map(|s| {
                            let ns = (s.start_m - offset).max(0.0);
                            let ne = (s.end_m - offset).min(flank_len);
                            (ne > ns + 1e-4).then_some(ExposureSection {
                                start_m: ns,
                                end_m: ne,
                                exposure: s.exposure,
                            })
                        })
                        .collect();
                    if !moved.is_empty() {
                        self.wall_sections.insert(new_vid, moved);
                    }
                }
            }
            // 2-D face partition (supersedes the 1-D sections for a wall):
            // translate along-x by −offset, drop cuts/regions outside the flank
            // span. Skip the whole partition if no region sample survives.
            if !self.wall_face_partitions.contains_key(&new_vid) {
                if let Some(part) = self.wall_face_partitions.get(&old_vid) {
                    let shift = |p: Point2f32| Point2f32::new(p.x - offset, p.y);
                    let cuts: Vec<PartitionCut> = part
                        .cuts
                        .iter()
                        .filter_map(|c| {
                            let (ca, cb) = (shift(c.a), shift(c.b));
                            // Keep a cut that still crosses the flank span.
                            (ca.x.max(cb.x) > 0.0 && ca.x.min(cb.x) < flank_len)
                                .then_some(PartitionCut { a: ca, b: cb })
                        })
                        .collect();
                    let regions: Vec<(Point2f32, WallExposure)> = part
                        .regions
                        .iter()
                        .filter_map(|(p, e)| {
                            let sp = shift(*p);
                            (sp.x >= 0.0 && sp.x <= flank_len).then_some((sp, *e))
                        })
                        .collect();
                    if !regions.is_empty() {
                        self.wall_face_partitions
                            .insert(new_vid, WallFacePartition { cuts, regions });
                    }
                }
            }
            // Construction (keyed by SurfaceRef::wall).
            let new_ref = SurfaceRef::wall(new_vid);
            if !self.wall_constructions.contains_key(&new_ref) {
                if let Some(con) = self
                    .wall_constructions
                    .get(&SurfaceRef::wall(old_vid))
                    .cloned()
                {
                    self.wall_constructions.insert(new_ref, con);
                }
            }
        }
    }

    /// Rebuild the runtime section model from the persisted `WallRegions` (the
    /// source of truth) on load. Each region's base edge is projected onto the
    /// perimeter wall it lies on to recover its `(start_m, end_m, exposure)`;
    /// regions are grouped by wall and sorted. `Unset` and `Ground` regions
    /// are SKIPPED: `Unset` is derived coverage (recovering it would mark
    /// every wall after one save/load) and `Ground` is riser-only (the
    /// assessor's explicit choice in the riser toolbar — recovering it would
    /// turn risers into phantom painted sections). Riser regions never recover
    /// anyway — their base edges lie on no perimeter wall, so the geometric
    /// match below drops them whatever their exposure (that's what keeps an
    /// overridden Exposed/Semi/NonHeatLoss riser out too); the `Ground` skip
    /// is belt-and-braces on top. Inverse of the commit-time derivation.
    ///
    /// A derived `InterStorey` BAND never recovers as its own mark
    /// (`derive_inter_storey` re-stamps it on every re-derive — recovering it
    /// would freeze a derived mark into a painted section), BUT it may own the
    /// wall's only recoverable base span: the derivation's HORIZONTAL split
    /// leaves the assessor's painted remainder STACKED ABOVE the band with no
    /// base edge of its own. That paint must survive a reload, so the band's
    /// span recovers with the stacked piece's exposure
    /// ([`stacked_exposure_above`]) — the next resolve then repaints the whole
    /// span and the derivation re-splits it, reproducing band + remainder
    /// exactly (the reload fixpoint).
    ///
    /// The same applies to an ORPHANED-`Unset` band: when the neighbour is
    /// redrawn/shrunk while THIS storey is non-active, its walls keep the
    /// stale split and the derivation's clear pass reverts the band piece to
    /// `Unset` — the painted remainder is still stacked above it with no base
    /// span of its own. An `Unset` piece with painted stack above therefore
    /// recovers exactly like an `InterStorey` band (same duplicate guards); a
    /// plain `Unset` piece with nothing painted above still never recovers.
    ///
    /// A FACE-PARTITIONED wall ([`wall_face_partitions`]) never recovers 1-D
    /// sections at all — its resolved pieces are exposure carriers of the 2-D
    /// partition recipe (each piece a first-class wall with a real base span
    /// since the resolve rebases them), and recovering those spans here would
    /// turn the partition into overlapping phantom `wall_sections`. The
    /// caller must have the working partition map populated before recovery
    /// (`from_storey` does; the shell reconstructs partitions before
    /// sections).
    ///
    /// AGREEMENT HEAL (field report 17e15dc7 / ff5a5ee3): adjacent recovered
    /// sections on the SAME wall whose paint AND construction attribution
    /// (`WallRegion::construction_id`) both AGREE are COALESCED into one
    /// section. Rationale: two adjacent same-paint, same-construction
    /// sections carry exactly the same durable information as their union —
    /// their seam encodes nothing — yet the seam pre-splits the regenerated
    /// wall, which is how an OBSOLETE derived split (the per-facet staircase
    /// an older derivation baked in, then the assessor painted) survives
    /// every re-derive: painted pieces recover per piece, the seams become
    /// section boundaries, and regenerate re-splits at them forever. Seams
    /// the CURRENT derivation justifies (footprint crossings, band-profile
    /// discontinuities) need no protection here — the derive re-cuts them on
    /// the regenerated wall — and durable `wall_splits` / face partitions
    /// re-apply at resolve regardless of 1-D coverage. Genuinely meaningful
    /// seams survive via the disagreement guards: mixed paint or differing
    /// construction attributions never merge, and a band section recovered by
    /// pass 2 (the stacked-paint fixpoint machinery) never participates.
    ///
    /// [`wall_face_partitions`]: StoreyDoc::wall_face_partitions
    pub fn sections_from_wall_regions(
        &self,
        walls: &WallRegions,
    ) -> HashMap<VertexId, Vec<ExposureSection>> {
        // Adjacency tolerance (m) for the agreement heal: recovered spans are
        // f32 projections of one shared cut coordinate, so a real seam pair
        // agrees to well under this; real gaps (openings never split sections;
        // distinct paints) are centimetres+.
        const SEAM_EPS_M: f32 = 1e-3;
        // Per section: (section, source construction, healable) — pass-1
        // painted pieces are healable, pass-2 band spans never are.
        let mut out: HashMap<VertexId, Vec<(ExposureSection, ItemId, bool)>> = HashMap::new();
        // Pass 1 — every directly recoverable painted region (remembering
        // which ones DID recover, so a band never duplicates their span).
        let mut recovered_ids: std::collections::HashSet<ItemId> = std::collections::HashSet::new();
        let mut bands: Vec<&WallRegion> = Vec::new();
        for region in &walls.regions {
            match region.exposure {
                WallExposure::Ground => continue, // riser-only, never recovers
                // An InterStorey band — or an Unset piece, which may be a
                // pass-0-reverted (orphaned) band — may carry stacked paint;
                // pass 2 decides. Neither ever recovers as its OWN mark.
                WallExposure::Unset | WallExposure::InterStorey => {
                    bands.push(region);
                    continue;
                }
                _ => {}
            }
            let Some((vid, start_m, end_m, _len)) = self.recover_wall_region_span(region) else {
                continue; // region's base edge isn't on any current wall (a riser)
            };
            if self.is_wall_partitioned(vid) {
                continue; // 2-D partition piece — the recipe owns its exposure
            }
            recovered_ids.insert(region.id);
            out.entry(vid).or_default().push((
                ExposureSection {
                    start_m,
                    end_m,
                    exposure: region.exposure,
                },
                region.construction_id,
                true,
            ));
        }
        // Pass 2 — each band's span carries the STACKED remainder's paint (see
        // the doc above), unless that remainder already recovered itself or a
        // painted sibling BELOW owns the span.
        for band in bands {
            let Some(exposure) = stacked_exposure_above(walls, band, &recovered_ids) else {
                continue;
            };
            let Some((vid, start_m, end_m, _len)) = self.recover_wall_region_span(band) else {
                continue;
            };
            if self.is_wall_partitioned(vid) {
                continue; // 2-D partition piece — the recipe owns its exposure
            }
            out.entry(vid).or_default().push((
                ExposureSection {
                    start_m,
                    end_m,
                    exposure,
                },
                ItemId::nil(),
                false,
            ));
        }
        // Sort, then apply the AGREEMENT HEAL (see the doc above): coalesce
        // adjacent healable sections with equal paint + equal construction.
        out.into_iter()
            .map(|(vid, mut entries)| {
                entries.sort_by(|x, y| {
                    x.0.start_m
                        .partial_cmp(&y.0.start_m)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                let mut secs: Vec<(ExposureSection, ItemId, bool)> = Vec::new();
                for e in entries {
                    match secs.last_mut() {
                        Some((last, cid, healable))
                            if *healable
                                && e.2
                                && last.exposure == e.0.exposure
                                && *cid == e.1
                                && e.0.start_m <= last.end_m + SEAM_EPS_M =>
                        {
                            last.end_m = last.end_m.max(e.0.end_m);
                        }
                        _ => secs.push(e),
                    }
                }
                (vid, secs.into_iter().map(|(s, _, _)| s).collect())
            })
            .collect()
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

/// The assessor's paint carried by the piece STACKED directly above a `band`
/// — an `InterStorey` piece, or an ORPHANED `Unset` one (the derivation's
/// clear pass reverted a stale band on a non-active storey; the split
/// geometry survives) — i.e. the horizontal-split remainder sharing the
/// band's plan span, its bottom edge on the band's top. `None` when nothing
/// painted sits above (the band reaches the wall top, or the piece above is
/// `Unset`/derived), when the piece above already recovered its OWN span
/// (`recovered_ids`), or when a PAINTED sibling sits BELOW the band (its own
/// base edge already recovers the span) — in each case recovering the band
/// too would duplicate the section. Support for
/// [`StoreyDoc::sections_from_wall_regions`]'s reload fixpoint.
fn stacked_exposure_above(
    walls: &WallRegions,
    band: &WallRegion,
    recovered_ids: &std::collections::HashSet<ItemId>,
) -> Option<WallExposure> {
    const EPS: f64 = 1e-3;
    let (a, b) = band.base_edge()?;
    let (band_lo, band_hi) = band
        .polygon
        .exterior
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
            (lo.min(v.z), hi.max(v.z))
        });
    // A region whose ring touches BOTH of the band's plan endpoints at `z`.
    let shares_edge_at = |r: &WallRegion, z: f64| {
        let has = |x: f64, y: f64| {
            r.polygon
                .exterior
                .iter()
                .any(|v| (v.x - x).abs() < EPS && (v.y - y).abs() < EPS && (v.z - z).abs() < EPS)
        };
        has(a.x, a.y) && has(b.x, b.y)
    };
    let painted = |e: WallExposure| {
        !matches!(
            e,
            WallExposure::Unset | WallExposure::Ground | WallExposure::InterStorey
        )
    };
    // A painted sibling below the band owns the span via its own base edge.
    if walls
        .regions
        .iter()
        .any(|r| r.id != band.id && painted(r.exposure) && shares_edge_at(r, band_lo))
    {
        return None;
    }
    walls
        .regions
        .iter()
        .find(|r| {
            r.id != band.id
                // Already recovered its own span in pass 1 — don't duplicate.
                && !recovered_ids.contains(&r.id)
                && painted(r.exposure)
                && shares_edge_at(r, band_hi)
        })
        .map(|r| r.exposure)
}

/// Tolerance for the conservative full-containment overlap test (metres): a
/// region vertex within this of the neighbour footprint's edge still counts as
/// contained, so a shared/coincident stack boundary reads as fully overlapping.
const CONTAINMENT_TOL_M: f64 = 0.05;

/// Drop the LEGACY party-carve `ceiling_splits` rings a removed neighbour left
/// on `below`: every ring FULLY contained in `removed_fp` (the deleted storey's
/// footprint, or the pre-redraw one — party carve rings were
/// `ceiling ∩ above_footprint` by construction, so containment identifies
/// them). The ring-retraction half of the retired (phase-3-deleted)
/// `retract_party_ceiling_from_below`; the classification half is gone —
/// `derive_inter_storey`'s clear pass retracts stale `InterStorey` stamps
/// automatically on the next re-derive. Returns whether any ring was dropped
/// (the caller then recomposes the ceilings). Accepted edge (unchanged): a
/// MANUAL ceiling-exposure carve lying fully under the removed footprint is
/// dropped with them — its purpose died with the storey above.
pub fn retract_legacy_party_rings(below: &mut Storey, removed_fp: &[Point2f64]) -> bool {
    let n = below.ceiling_splits.len();
    below
        .ceiling_splits
        .retain(|ring| !ring_within_ring(ring, removed_fp, CONTAINMENT_TOL_M));
    below.ceiling_splits.len() != n
}

/// True when storey `idx`'s FLOOR regions are fully classified: at least one
/// floor region, and EVERY one carries a non-`Unset` exposure. Reads the
/// RESOLVED storey (Store A) — the checklist's per-storey "Floor exposures"
/// rollup. `Unset` floors are the still-to-label ones the floor-exposure tool
/// shows; a derived `InterStorey` piece counts as classified.
pub fn floor_exposure_complete(ber: &Ber, idx: usize) -> bool {
    floor_exposure_reasons(ber, idx).is_empty()
}

/// Every reason [`floor_exposure_complete`] is false — empty exactly when it
/// is true. Plain messages for the tappable checklist row; no storey prefix
/// (the controller adds the storey name itself).
pub fn floor_exposure_reasons(ber: &Ber, idx: usize) -> Vec<String> {
    let Some(storey) = ber.storeys.get(idx) else {
        return vec!["Storey not resolved yet".to_string()];
    };
    let regions = &storey.floors.regions;
    if regions.is_empty() {
        return vec![
            "No floor areas resolved yet — draw the storey and set its height".to_string(),
        ];
    }
    let unset: Vec<_> = regions
        .iter()
        .filter(|r| r.exposure == FloorExposure::Unset)
        .collect();
    if unset.is_empty() {
        return Vec::new();
    }
    let area: f64 = unset.iter().map(|r| r.area()).sum();
    vec![format!(
        "{} of {} floor area(s) still unclassified ({area:.1} m²)",
        unset.len(),
        regions.len(),
    )]
}

/// True when storey `idx`'s CEILING/roof regions are fully classified: at least
/// one ceiling region, and EVERY one carries a non-`Unset` exposure. The ceiling
/// mirror of [`floor_exposure_complete`] — the checklist's "Ceiling exposures"
/// rollup.
pub fn ceiling_exposure_complete(ber: &Ber, idx: usize) -> bool {
    ceiling_exposure_reasons(ber, idx).is_empty()
}

/// Every reason [`ceiling_exposure_complete`] is false — empty exactly when it
/// is true. The ceiling mirror of [`floor_exposure_reasons`].
pub fn ceiling_exposure_reasons(ber: &Ber, idx: usize) -> Vec<String> {
    let Some(storey) = ber.storeys.get(idx) else {
        return vec!["Storey not resolved yet".to_string()];
    };
    let regions = &storey.ceilings.regions;
    if regions.is_empty() {
        return vec![
            "No ceiling areas resolved yet — draw the storey and set its height".to_string(),
        ];
    }
    let unset: Vec<_> = regions
        .iter()
        .filter(|r| r.exposure == CeilingExposure::Unset)
        .collect();
    if unset.is_empty() {
        return Vec::new();
    }
    let area: f64 = unset.iter().map(|r| r.area()).sum();
    vec![format!(
        "{} of {} ceiling area(s) still unclassified ({area:.1} m²)",
        unset.len(),
        regions.len(),
    )]
}

/// True when storey `idx`'s horizontal surfaces are fully classified — both
/// [`floor_exposure_complete`] AND [`ceiling_exposure_complete`]. (Kept as the
/// combined predicate; the Mapping checklist now shows floor and ceiling as
/// separate rows.)
pub fn surfaces_exposure_complete(ber: &Ber, idx: usize) -> bool {
    floor_exposure_complete(ber, idx) && ceiling_exposure_complete(ber, idx)
}

/// True when storey `idx`'s WALL regions are fully classified: at least one
/// wall region, and EVERY one — perimeter pieces AND risers alike — carries a
/// non-`Unset` exposure. Reads the RESOLVED storey (Store A), like
/// [`floor_exposure_complete`] — the checklist's per-storey "Wall exposures"
/// rollup. A derived [`WallExposure::InterStorey`] band counts as classified
/// (the derivation already accounted for it); only genuine `Unset` remnants
/// block completion.
pub fn wall_exposure_complete(ber: &Ber, idx: usize) -> bool {
    wall_exposure_reasons(ber, idx).is_empty()
}

/// Every reason [`wall_exposure_complete`] is false — empty exactly when it is
/// true. Counts only (no area: [`WallRegion`] carries no cheap plan area).
pub fn wall_exposure_reasons(ber: &Ber, idx: usize) -> Vec<String> {
    let Some(storey) = ber.storeys.get(idx) else {
        return vec!["Storey not resolved yet".to_string()];
    };
    let regions = &storey.walls.regions;
    if regions.is_empty() {
        return vec![
            "No wall surfaces resolved yet — draw the storey and set its height".to_string(),
        ];
    }
    let unset = regions
        .iter()
        .filter(|r| r.exposure == WallExposure::Unset)
        .count();
    if unset == 0 {
        return Vec::new();
    }
    vec![format!(
        "{unset} of {} wall surface(s) still unclassified",
        regions.len(),
    )]
}

/// Drop the floor/ceiling exposure-map entries of `doc` whose keyed plan point
/// lies INSIDE a region currently resolving [`FloorExposure::InterStorey`] /
/// [`CeilingExposure::InterStorey`] on the resolved `storey` — the load-time
/// purge of entries the derivation shadows (it always stamps last, so a
/// shadowed entry is dead weight that would spring back to life if the storey
/// above ever moved away). The match is CONTAINMENT (point-in-ring), not
/// centroid equality: a PRE-FEATURE auto-seeded entry is keyed by the UNSPLIT
/// region's centroid (no carve ring existed), which coincides with no
/// post-split piece's centroid — but it does lie inside the covered piece, and
/// must go, or deleting the storey above would merge the pieces back and
/// resurrect the stale `Party`/`NonHeatLoss` mark.
///
/// Deliberately provenance-blind: an ASSESSOR-painted `Party`/`NonHeatLoss`
/// mark on a covered surface is dropped too (the map carries no provenance;
/// while covered, the mark is inert anyway, and the setters refuse new writes
/// there). Entries whose keyed point lies inside NO `InterStorey` piece —
/// including assessor marks on surfaces the derivation does not cover — are
/// kept. Returns whether anything was dropped. Called once per storey from the
/// load paths after the first `refresh_all_storeys`.
pub fn purge_shadowed_exposure_entries(doc: &mut StoreyDoc, storey: &Storey) -> bool {
    // The InterStorey pieces' plan rings, f32 for `ring_contains_point`.
    let ring32 = |ring: &[Point2f64]| -> Vec<Point2f32> {
        ring.iter()
            .map(|p| Point2f32::new(p.x as f32, p.y as f32))
            .collect()
    };
    let floor_rings: Vec<Vec<Point2f32>> = storey
        .floors
        .regions
        .iter()
        .zip(storey.floors.to_vertex_lists())
        .filter(|(r, _)| r.exposure == FloorExposure::InterStorey)
        .map(|(_, ring)| ring32(&ring))
        .collect();
    let ceiling_rings: Vec<Vec<Point2f32>> = storey
        .ceilings
        .regions
        .iter()
        .filter(|r| r.exposure == CeilingExposure::InterStorey)
        .map(|r| {
            r.polygon
                .exterior
                .iter()
                .map(|v| Point2f32::new(v.x as f32, v.y as f32))
                .collect()
        })
        .collect();
    // A CentroidKey is the keyed point quantized to mm — de-quantize and test.
    let shadowed = |rings: &[Vec<Point2f32>], key: &CentroidKey| {
        let (x, y) = (key.0 as f32 / 1000.0, key.1 as f32 / 1000.0);
        rings.iter().any(|ring| ring_contains_point(ring, x, y))
    };
    let before = doc.floor_exposure.len() + doc.ceiling_exposure.len();
    doc.floor_exposure
        .retain(|key, _| !shadowed(&floor_rings, key));
    doc.ceiling_exposure
        .retain(|key, _| !shadowed(&ceiling_rings, key));
    doc.floor_exposure.len() + doc.ceiling_exposure.len() != before
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

    /// When a splice SPLITS a painted wall into two flanks — the far one getting
    /// a freshly-minted start vid — `rehome_split_wall_attributes` copies the
    /// source wall's exposure AND construction onto the new vid, so BOTH flanks
    /// inherit rather than only the flank that kept the original start vid.
    #[test]
    fn rehome_split_wall_copies_exposure_and_construction_to_new_flank() {
        use ber_model::ber::common::ConstructionPhase;
        use ber_model::ber::structural::{ConstructionMakeup, WallConstructionType};
        use manip_plan::SurfaceRef;

        let mut doc = square_doc(); // 4×4
        let old_perim = doc.floor_plan.perimeter_walls();
        // The bottom wall (both endpoints on y=0).
        let (vid, a, b) = old_perim
            .iter()
            .copied()
            .find(|(_, a, b)| a.y.abs() < 1e-4 && b.y.abs() < 1e-4)
            .expect("a y=0 wall");
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
        doc.wall_constructions.insert(
            SurfaceRef::wall(vid),
            (
                WallConstructionType::TimberConstruction,
                ConstructionMakeup::Default,
                ConstructionPhase::default(),
                None,
            ),
        );

        // Bump a dormer off the MIDDLE of that wall (outward, y<0): the splice
        // splits it into a near flank keeping `vid` and a far flank with a
        // freshly-minted start vid.
        doc.floor_plan
            .extend_ortho(&[
                Point2f32::new(1.0, 0.0),
                Point2f32::new(1.0, -1.0),
                Point2f32::new(3.0, -1.0),
                Point2f32::new(3.0, 0.0),
            ])
            .expect("dormer extend splits the wall");
        let new_perim = doc.floor_plan.perimeter_walls();

        let old_vids: std::collections::HashSet<_> = old_perim.iter().map(|(v, _, _)| *v).collect();
        let flank_vids: Vec<VertexId> = new_perim
            .iter()
            .filter(|(_, a, b)| a.y.abs() < 1e-4 && b.y.abs() < 1e-4)
            .map(|(v, _, _)| *v)
            .collect();
        assert_eq!(flank_vids.len(), 2, "the y=0 wall is now two flanks");
        assert!(
            flank_vids.iter().any(|v| !old_vids.contains(v)),
            "the splice minted a fresh vid for the far flank"
        );

        doc.rehome_split_wall_attributes(&old_perim, &new_perim);

        // BOTH y=0 flanks now carry the Exposed section AND the construction —
        // not just the flank that kept the original vid.
        for v in &flank_vids {
            let secs = doc
                .wall_sections
                .get(v)
                .unwrap_or_else(|| panic!("flank {v:?} has an exposure section"));
            assert!(
                secs.iter().any(|s| s.exposure == WallExposure::Exposed),
                "flank {v:?} inherits Exposed"
            );
            assert!(
                doc.wall_constructions.contains_key(&SurfaceRef::wall(*v)),
                "flank {v:?} inherits the construction"
            );
        }
    }

    /// A PARTIAL paint must land only on the flank it covers: paint `0..1` of the
    /// (4 m) wall Exposed, dormer-split it at the FAR end → the near flank keeps
    /// Exposed, the far flank (whose own span was unpainted) resolves UNSET. Without
    /// the offset-translation the far flank would inherit the `0..1` range verbatim
    /// (clamped onto its start) and read Exposed — this test fails pre-fix.
    #[test]
    fn rehome_partial_paint_translates_to_the_flank_it_covers() {
        let mut doc = square_doc(); // 4×4
        let old_perim = doc.floor_plan.perimeter_walls();
        let (vid, a, b) = old_perim
            .iter()
            .copied()
            .find(|(_, a, b)| a.y.abs() < 1e-4 && b.y.abs() < 1e-4)
            .expect("a y=0 wall");
        let len = segment_len(a, b);
        // Paint only 0..1 (near the START vertex `a`) Exposed.
        doc.set_wall_section(vid, 0.0, 1.0, len, WallExposure::Exposed);

        // Dormer bump off the FAR portion (along 2.5..3.5 from `a`), outward (−y).
        let (ux, uy) = ((b.x - a.x) / len, (b.y - a.y) / len);
        let at = |d: f32| Point2f32::new(a.x + ux * d, a.y + uy * d);
        let (p1, p2) = (at(2.5), at(3.5));
        doc.floor_plan
            .extend_ortho(&[
                Point2f32::new(p1.x, p1.y),
                Point2f32::new(p1.x, p1.y - 1.0),
                Point2f32::new(p2.x, p2.y - 1.0),
                Point2f32::new(p2.x, p2.y),
            ])
            .expect("dormer extend splits the wall");
        let new_perim = doc.floor_plan.perimeter_walls();
        doc.rehome_split_wall_attributes(&old_perim, &new_perim);

        // Split the y=0 wall into near (contains `a`) + far by midpoint distance.
        let flanks: Vec<(VertexId, Point2f32, Point2f32)> = new_perim
            .iter()
            .copied()
            .filter(|(_, fa, fb)| fa.y.abs() < 1e-4 && fb.y.abs() < 1e-4)
            .collect();
        assert_eq!(flanks.len(), 2, "the y=0 wall split into two flanks");
        let dist_a = |fa: Point2f32, fb: Point2f32| {
            let m = Point2f32::new((fa.x + fb.x) * 0.5, (fa.y + fb.y) * 0.5);
            (m.x - a.x).hypot(m.y - a.y)
        };
        let (near, far) = if dist_a(flanks[0].1, flanks[0].2) < dist_a(flanks[1].1, flanks[1].2) {
            (flanks[0].0, flanks[1].0)
        } else {
            (flanks[1].0, flanks[0].0)
        };
        let exposed = |v: VertexId| {
            doc.wall_sections
                .get(&v)
                .is_some_and(|s| s.iter().any(|x| x.exposure == WallExposure::Exposed))
        };
        assert!(
            exposed(near),
            "near flank keeps the Exposed paint (its 0..1 span)"
        );
        assert!(
            !exposed(far),
            "far flank's own span was UNPAINTED → not Exposed (offset-translated + clipped away): {:?}",
            doc.wall_sections.get(&far)
        );
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
            ceilings: CeilingRegions::default_for_footprint(vec![footprint], 2.4, cid),
            ..Storey::default()
        }
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
        ber.storeys[0].ceilings.regions[0].exposure = CeilingExposure::ExposedRoof;
        assert!(surfaces_exposure_complete(&ber, 0));

        // Empty / missing storey → never complete.
        ber.storeys.push(Storey::default());
        assert!(!surfaces_exposure_complete(&ber, 1));
        assert!(!surfaces_exposure_complete(&ber, 9));
    }

    /// The exposure reasons carry the unset counts (and m² for floors and
    /// ceilings), and each bool is exactly `reasons.is_empty()` across the
    /// module's fixtures.
    #[test]
    fn exposure_reasons_carry_counts_and_match_the_bools() {
        use ber_model::ber::structural::{Opes, Polygon3D};
        use domain::points::Point3f64;

        let mut ber = Ber::default();
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));
        // Fresh 4 × 4 storey: one Unset floor + one Unset ceiling (16 m² each),
        // no walls resolved.
        assert_eq!(
            floor_exposure_reasons(&ber, 0),
            vec!["1 of 1 floor area(s) still unclassified (16.0 m²)".to_string()]
        );
        assert_eq!(
            ceiling_exposure_reasons(&ber, 0),
            vec!["1 of 1 ceiling area(s) still unclassified (16.0 m²)".to_string()]
        );
        assert_eq!(
            wall_exposure_reasons(&ber, 0),
            vec!["No wall surfaces resolved yet — draw the storey and set its height".to_string()]
        );

        // Two walls, one classified → "1 of 2" (counts only, no area).
        let wall = |exposure| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            exposure,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        };
        ber.storeys[0].walls = WallRegions {
            regions: vec![wall(WallExposure::Exposed), wall(WallExposure::Unset)],
        };
        assert_eq!(
            wall_exposure_reasons(&ber, 0),
            vec!["1 of 2 wall surface(s) still unclassified".to_string()]
        );

        // Classify everything → no reasons ⇔ complete.
        ber.storeys[0].floors.regions[0].exposure = FloorExposure::Ground;
        ber.storeys[0].ceilings.regions[0].exposure = CeilingExposure::ExposedRoof;
        ber.storeys[0].walls.regions[1].exposure = WallExposure::Exposed;
        assert!(floor_exposure_reasons(&ber, 0).is_empty());
        assert!(ceiling_exposure_reasons(&ber, 0).is_empty());
        assert!(wall_exposure_reasons(&ber, 0).is_empty());

        // Missing storey → the not-resolved reason.
        assert_eq!(
            floor_exposure_reasons(&ber, 9),
            vec!["Storey not resolved yet".to_string()]
        );

        // Equivalence sweep: complete ⇔ reasons empty, for every kind, over a
        // classified storey, an empty storey, and a missing index.
        ber.storeys.push(Storey::default());
        for idx in [0, 1, 9] {
            assert_eq!(
                floor_exposure_complete(&ber, idx),
                floor_exposure_reasons(&ber, idx).is_empty()
            );
            assert_eq!(
                ceiling_exposure_complete(&ber, idx),
                ceiling_exposure_reasons(&ber, idx).is_empty()
            );
            assert_eq!(
                wall_exposure_complete(&ber, idx),
                wall_exposure_reasons(&ber, idx).is_empty()
            );
        }
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

    // ---- InterStorey (derived storey-junction) guards --------------------

    use ber_model::ber::structural::{Opes, Polygon3D};
    use domain::points::Point3f64;

    /// A flat wall region over base edge `a`→`b` (z 0..2.4) with `exposure`.
    fn wall_region(a: Point2f64, b: Point2f64, exposure: WallExposure) -> WallRegion {
        WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(a.x, a.y, 0.0),
                Point3f64::new(b.x, b.y, 0.0),
                Point3f64::new(b.x, b.y, 2.4),
                Point3f64::new(a.x, a.y, 2.4),
            ]),
            exposure,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        }
    }

    /// Every setter REFUSES `InterStorey` as an input value — it's the
    /// derivation's stamp, never assessor-paintable: `set_wall_section` is a
    /// no-op (no section stored), `set_floor_exposure_at` /
    /// `set_ceiling_exposure_at` return `None` and write nothing.
    #[test]
    fn setters_refuse_inter_storey_as_input() {
        let mut doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::InterStorey);
        assert!(
            !doc.wall_sections.contains_key(&vid),
            "InterStorey is never painted as a section"
        );

        let storey = storey_with(square_ring(0.0, 4.0));
        assert!(
            doc.set_floor_exposure_at(&storey, 2.0, 2.0, FloorExposure::InterStorey)
                .is_none(),
            "InterStorey refused as a floor input"
        );
        assert!(
            doc.set_ceiling_exposure_at(&storey, 2.0, 2.0, CeilingExposure::InterStorey)
                .is_none(),
            "InterStorey refused as a ceiling input"
        );
        assert!(doc.floor_exposure.is_empty(), "nothing written");
        assert!(doc.ceiling_exposure.is_empty(), "nothing written");
    }

    /// A tap on a region the derivation currently classifies `InterStorey` is a
    /// no-op (`None`, nothing written) — the derivation is authoritative over
    /// that surface, whatever value the assessor tried to paint.
    #[test]
    fn setters_no_op_on_an_inter_storey_region() {
        let mut storey = storey_with(square_ring(0.0, 4.0));
        storey.floors.regions[0].exposure = FloorExposure::InterStorey;
        storey.ceilings.regions[0].exposure = CeilingExposure::InterStorey;

        let mut doc = StoreyDoc::default();
        assert!(
            doc.set_floor_exposure_at(&storey, 2.0, 2.0, FloorExposure::Ground)
                .is_none(),
            "the derivation owns the floor region — write refused"
        );
        assert!(
            doc.set_ceiling_exposure_at(&storey, 2.0, 2.0, CeilingExposure::Party)
                .is_none(),
            "the derivation owns the ceiling region — write refused"
        );
        assert!(doc.floor_exposure.is_empty(), "no spurious write");
        assert!(doc.ceiling_exposure.is_empty(), "no spurious write");
    }

    /// AGREEMENT HEAL (field report 17e15dc7 / ff5a5ee3): an OBSOLETE derived
    /// split — the per-facet staircase an older derivation baked into a wall
    /// the assessor then painted — recovers as ONE section when the adjacent
    /// pieces' paint AND construction attribution agree: the seam encodes
    /// nothing, so keeping it would pre-split the regenerated wall forever.
    /// Mixed paint or differing construction attributions still recover per
    /// piece (a real assessor boundary is never dissolved).
    #[test]
    fn sections_recovery_coalesces_agreeing_adjacent_pieces() {
        let doc = square_doc();
        let (vid, _, _) = doc.outline.floor_plan.perimeter_walls()[0];
        let cid = ItemId::default();
        let mk = |x0: f64, x1: f64, e: WallExposure| {
            let mut r = wall_region(Point2f64::new(x0, 0.0), Point2f64::new(x1, 0.0), e);
            r.construction_id = cid;
            r
        };
        // The painted-agreeing staircase: three adjacent Exposed pieces, one
        // shared construction → ONE healed section over the full span.
        let walls = WallRegions {
            regions: vec![
                mk(0.0, 1.5, WallExposure::Exposed),
                mk(1.5, 3.0, WallExposure::Exposed),
                mk(3.0, 4.0, WallExposure::Exposed),
            ],
        };
        let recovered = doc.sections_from_wall_regions(&walls);
        let secs = recovered.get(&vid).expect("the painted pieces recover");
        assert_eq!(secs.len(), 1, "agreeing seams coalesce into one section");
        assert_eq!(secs[0].exposure, WallExposure::Exposed);
        assert!(
            (secs[0].start_m - 0.0).abs() < 1e-3 && (secs[0].end_m - 4.0).abs() < 1e-3,
            "the healed section spans the whole staircase, got {secs:?}"
        );

        // MIXED PAINT: the seam is a real assessor boundary — never merged.
        let walls = WallRegions {
            regions: vec![
                mk(0.0, 2.0, WallExposure::Exposed),
                mk(2.0, 4.0, WallExposure::SemiExposed),
            ],
        };
        let recovered = doc.sections_from_wall_regions(&walls);
        assert_eq!(
            recovered.get(&vid).unwrap().len(),
            2,
            "mixed paint keeps the seam"
        );

        // SAME paint, DIFFERENT construction attributions: treated like
        // mixed paint — the attribution boundary survives.
        let mut other = mk(0.0, 2.0, WallExposure::Exposed);
        other.construction_id = ItemId::default();
        let walls = WallRegions {
            regions: vec![other, mk(2.0, 4.0, WallExposure::Exposed)],
        };
        let recovered = doc.sections_from_wall_regions(&walls);
        assert_eq!(
            recovered.get(&vid).unwrap().len(),
            2,
            "differing construction attributions keep the seam"
        );
    }

    /// `sections_from_wall_regions` never recovers an `InterStorey` region into
    /// a painted section — like `Unset`/`Ground`, it's derived, and the
    /// derivation re-stamps it on every re-derive. A painted `Exposed` piece on
    /// the same wall still recovers.
    #[test]
    fn sections_recovery_skips_inter_storey_regions() {
        let doc = square_doc();
        // Wall 0 is the bottom edge (0,0)→(4,0): an Exposed piece over [0,2]
        // and a derived InterStorey band over [2,4].
        let walls = WallRegions {
            regions: vec![
                wall_region(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(2.0, 0.0),
                    WallExposure::Exposed,
                ),
                wall_region(
                    Point2f64::new(2.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    WallExposure::InterStorey,
                ),
            ],
        };
        let recovered = doc.sections_from_wall_regions(&walls);
        let (vid, _, _) = doc.outline.floor_plan.perimeter_walls()[0];
        let secs = recovered.get(&vid).expect("the Exposed piece recovers");
        assert_eq!(secs.len(), 1, "the InterStorey band does NOT recover");
        assert_eq!(secs[0].exposure, WallExposure::Exposed);
        assert!((secs[0].start_m - 0.0).abs() < 1e-3);
        assert!((secs[0].end_m - 2.0).abs() < 1e-3);
    }

    /// `recover_wall_region_span` reads the region's FULL plan extent, not
    /// its floor-band base edge: an L-shaped remainder whose bottom steps
    /// DEEPER than `MAX_FLOOR_STEP_M` (here 0 → 1.0 m at x=2) has a base
    /// edge/frame confined near the low run, but its painted face covers the
    /// whole wall — the span must recover `[0, 4]` in BOTH ring orientations
    /// (the truncation is orientation-dependent: a ring starting on the high
    /// shoulder yields a different frame than its mirror). Regression for
    /// the Elmwood migration churn: the truncated span reloaded the notched
    /// half of the wall `Unset` on every open.
    #[test]
    fn recover_span_covers_a_deep_notched_remainder_fully() {
        let doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        // The L in both orientations: low run first, high shoulder first.
        let l_low_first = WallRegion {
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(2.0, 0.0, 0.0),
                Point3f64::new(2.0, 0.0, 1.0),
                Point3f64::new(4.0, 0.0, 1.0),
                Point3f64::new(4.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            ..wall_region(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                WallExposure::Exposed,
            )
        };
        let l_high_first = WallRegion {
            polygon: Polygon3D::new(vec![
                Point3f64::new(4.0, 0.0, 1.0),
                Point3f64::new(2.0, 0.0, 1.0),
                Point3f64::new(2.0, 0.0, 0.0),
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(0.0, 0.0, 2.4),
                Point3f64::new(4.0, 0.0, 2.4),
            ]),
            ..wall_region(
                Point2f64::new(4.0, 0.0),
                Point2f64::new(0.0, 0.0),
                WallExposure::Exposed,
            )
        };
        for l in [&l_low_first, &l_high_first] {
            let (got_vid, s, e, wall_len) =
                doc.recover_wall_region_span(l).expect("the L recovers");
            assert_eq!(got_vid, vid);
            assert!((wall_len - len).abs() < 1e-4);
            assert!(
                s < 1e-3 && (e - len).abs() < 1e-3,
                "full plan extent despite the >MAX_FLOOR_STEP_M notch, got [{s}, {e}]"
            );
        }
        // And through the recovery pass: the deep-notched painted remainder
        // reloads as ONE full-span section (no Unset back half).
        let walls = WallRegions {
            regions: vec![l_low_first],
        };
        let recovered = doc.sections_from_wall_regions(&walls);
        let secs = recovered.get(&vid).expect("recovers");
        assert_eq!(secs.len(), 1);
        assert!(secs[0].start_m < 1e-3 && (secs[0].end_m - len).abs() < 1e-3);
    }

    /// The recovered span CLAMPS to the matched wall: a coplanar-flush MERGED
    /// guest's forward arm (ring vertices projecting past the wall's far end)
    /// must not stretch the span beyond the wall's own length.
    #[test]
    fn recover_span_clamps_a_merged_forward_arm_to_the_wall() {
        let doc = square_doc();
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = segment_len(a, b);
        // A wall over the full bottom edge whose plane continues 1 m past the
        // far corner (the merged step-riser arm), riding above the floor band.
        let merged = WallRegion {
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 1.0),
                Point3f64::new(5.0, 0.0, 1.0),
                Point3f64::new(5.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            ..wall_region(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                WallExposure::Exposed,
            )
        };
        let (got_vid, s, e, _) = doc
            .recover_wall_region_span(&merged)
            .expect("the merged wall recovers");
        assert_eq!(got_vid, vid);
        assert!(
            s < 1e-3 && (e - len).abs() < 1e-3,
            "the guest arm never stretches the span past the wall, got [{s}, {e}]"
        );
    }

    /// A flat wall region over base edge `a`→`b` spanning z `z0..z1` with
    /// `exposure` — for building horizontally-split (band + remainder) stacks.
    fn wall_region_z(
        a: Point2f64,
        b: Point2f64,
        z0: f64,
        z1: f64,
        exposure: WallExposure,
    ) -> WallRegion {
        WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(a.x, a.y, z0),
                Point3f64::new(b.x, b.y, z0),
                Point3f64::new(b.x, b.y, z1),
                Point3f64::new(a.x, a.y, z1),
            ]),
            exposure,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        }
    }

    /// A HORIZONTALLY split wall — the derivation's InterStorey band at the
    /// bottom, the assessor's Exposed remainder stacked above (which has no
    /// base edge of its own) — recovers ONE section over the band's span with
    /// the REMAINDER's exposure, so the paint survives a reload and the next
    /// derive reproduces band + remainder (the fixpoint).
    #[test]
    fn sections_recovery_carries_the_paint_stacked_above_a_band() {
        let doc = square_doc();
        // Wall 0 = (0,0)→(4,0): InterStorey band z 0..1.3, Exposed remainder
        // z 1.3..2.4 stacked on top.
        let walls = WallRegions {
            regions: vec![
                wall_region_z(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    0.0,
                    1.3,
                    WallExposure::InterStorey,
                ),
                wall_region_z(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    1.3,
                    2.4,
                    WallExposure::Exposed,
                ),
            ],
        };
        let recovered = doc.sections_from_wall_regions(&walls);
        let (vid, _, _) = doc.outline.floor_plan.perimeter_walls()[0];
        let secs = recovered.get(&vid).expect("the stacked paint recovers");
        assert_eq!(secs.len(), 1, "one section — the band's span, once");
        assert_eq!(
            secs[0].exposure,
            WallExposure::Exposed,
            "the band's span carries the stacked remainder's paint"
        );
        assert!((secs[0].start_m - 0.0).abs() < 1e-3);
        assert!((secs[0].end_m - 4.0).abs() < 1e-3);

        // With a PAINTED piece BELOW the band instead (a semi-buried wall —
        // the below piece owns the span via its own base edge), the band
        // recovers nothing extra: still exactly one section.
        let walls = WallRegions {
            regions: vec![
                wall_region_z(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    0.0,
                    2.1,
                    WallExposure::Exposed,
                ),
                wall_region_z(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    2.1,
                    2.4,
                    WallExposure::InterStorey,
                ),
            ],
        };
        let recovered = doc.sections_from_wall_regions(&walls);
        let secs = recovered.get(&vid).expect("the below paint recovers");
        assert_eq!(secs.len(), 1, "no duplicate section from the band");
        assert_eq!(secs[0].exposure, WallExposure::Exposed);
    }

    /// An ORPHANED-`Unset` band still carries its stacked paint through a
    /// reload. Sequence: the derivation splits the wall and stamps the band
    /// `InterStorey`; the assessor paints the remainder; the neighbour is then
    /// redrawn away while THIS storey is non-active, so its walls keep the
    /// stale split and the derivation's clear pass reverts the band to `Unset`
    /// (the exact state pass-0 leaves) — save, reload: the recovery must
    /// still carry the remainder's paint over the band's span, once, with no
    /// duplicates. A plain `Unset` wall (nothing stacked above) still
    /// recovers nothing.
    #[test]
    fn sections_recovery_carries_paint_above_a_pass0_reverted_band() {
        let doc = square_doc();
        let (vid, _, _) = doc.outline.floor_plan.perimeter_walls()[0];
        // The stale split: band z 0..1.3 stamped InterStorey, remainder
        // z 1.3..2.4 painted Exposed…
        let mut walls = WallRegions {
            regions: vec![
                wall_region_z(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    0.0,
                    1.3,
                    WallExposure::InterStorey,
                ),
                wall_region_z(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    1.3,
                    2.4,
                    WallExposure::Exposed,
                ),
            ],
        };
        // …then the neighbour goes away and pass 0 reverts the band to Unset
        // (the split geometry survives on the non-active storey).
        walls.regions[0].exposure = WallExposure::Unset;

        let recovered = doc.sections_from_wall_regions(&walls);
        let secs = recovered
            .get(&vid)
            .expect("the stacked paint survives the pass-0 revert");
        assert_eq!(secs.len(), 1, "recovered once — no duplicates");
        assert_eq!(
            secs[0].exposure,
            WallExposure::Exposed,
            "the orphaned band's span carries the stacked remainder's paint"
        );
        assert!((secs[0].start_m - 0.0).abs() < 1e-3);
        assert!((secs[0].end_m - 4.0).abs() < 1e-3);

        // Regression guard: a plain full-height Unset wall (no stacked paint)
        // still recovers NOTHING — Unset never becomes a painted section.
        let walls = WallRegions {
            regions: vec![wall_region_z(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                0.0,
                2.4,
                WallExposure::Unset,
            )],
        };
        assert!(
            doc.sections_from_wall_regions(&walls).is_empty(),
            "a plain Unset wall recovers no sections"
        );
    }

    /// `wall_exposure_complete`: an `Unset` perimeter piece blocks completion;
    /// once every wall region — painted perimeter, derived `InterStorey` band,
    /// classified riser — is non-`Unset`, the storey is complete. Empty or
    /// missing storeys are never complete.
    #[test]
    fn wall_exposure_complete_counts_inter_storey_as_classified() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey {
            walls: WallRegions {
                regions: vec![
                    // Painted perimeter piece.
                    wall_region(
                        Point2f64::new(0.0, 0.0),
                        Point2f64::new(2.0, 0.0),
                        WallExposure::Unset,
                    ),
                    // Derived inter-storey band (buried behind the neighbour).
                    wall_region(
                        Point2f64::new(2.0, 0.0),
                        Point2f64::new(4.0, 0.0),
                        WallExposure::InterStorey,
                    ),
                    // A riser wall (base off the perimeter), classified.
                    wall_region(
                        Point2f64::new(1.0, 2.0),
                        Point2f64::new(3.0, 2.0),
                        WallExposure::Ground,
                    ),
                ],
            },
            ..Storey::default()
        });
        assert!(
            !wall_exposure_complete(&ber, 0),
            "an Unset perimeter piece blocks completion"
        );
        ber.storeys[0].walls.regions[0].exposure = WallExposure::Exposed;
        assert!(
            wall_exposure_complete(&ber, 0),
            "painted + InterStorey band + classified riser → complete"
        );
        // Empty / missing storey → never complete.
        ber.storeys.push(Storey::default());
        assert!(!wall_exposure_complete(&ber, 1));
        assert!(!wall_exposure_complete(&ber, 9));
    }

    /// `porch_walls` never enter the Mapping completeness predicates: their
    /// permanently-`Unset` regions (a porch inner wall awaits the resolve-phase
    /// porch decision) must NOT block `wall_exposure_complete`, which reads
    /// `walls.regions` only — the reason the collection is kept separate.
    #[test]
    fn porch_walls_do_not_block_wall_exposure_completeness() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey {
            walls: WallRegions {
                regions: vec![wall_region(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    WallExposure::Exposed,
                )],
            },
            porch_walls: WallRegions {
                regions: vec![wall_region(
                    Point2f64::new(1.0, 1.0),
                    Point2f64::new(3.0, 1.0),
                    WallExposure::Unset,
                )],
            },
            ..Storey::default()
        });
        assert!(
            wall_exposure_complete(&ber, 0),
            "an Unset porch inner wall is invisible to wall completeness"
        );
    }

    /// `purge_shadowed_exposure_entries` drops exactly the floor/ceiling map
    /// entries whose keyed region currently resolves `InterStorey` (pre-feature
    /// auto-seeds now shadowed by the derivation), and KEEPS the assessor's
    /// `Party`/`NonHeatLoss` entries on un-shadowed regions. Idempotent.
    #[test]
    fn purge_drops_shadowed_entries_keeps_unshadowed() {
        // Two disjoint floor rings → two floor regions and two ceiling regions.
        let cid = ItemId::default();
        let (ring_a, ring_b) = (square_ring(0.0, 4.0), square_ring(6.0, 4.0));
        let mut storey = Storey {
            floors: FloorRegions::from_vertex_lists(vec![ring_a.clone(), ring_b.clone()], cid),
            ceilings: CeilingRegions::default_for_footprint(vec![ring_a, ring_b], 2.4, cid),
            ..Storey::default()
        };
        // The derivation covered region 0 of each kind; region 1 is untouched.
        storey.floors.regions[0].exposure = FloorExposure::InterStorey;
        storey.ceilings.regions[0].exposure = CeilingExposure::InterStorey;

        let f_shadowed = centroid_key(storey.floors.regions[0].centroid());
        let f_kept = centroid_key(storey.floors.regions[1].centroid());
        let c_shadowed = centroid_key(storey.ceilings.regions[0].centroid());
        let c_kept = centroid_key(storey.ceilings.regions[1].centroid());

        let mut doc = StoreyDoc::default();
        // Pre-feature auto-seeds on the now-shadowed regions…
        doc.floor_exposure
            .insert(f_shadowed, FloorExposure::NonHeatLoss);
        doc.ceiling_exposure
            .insert(c_shadowed, CeilingExposure::Party);
        // …and genuine assessor marks on un-shadowed ones.
        doc.floor_exposure
            .insert(f_kept, FloorExposure::NonHeatLoss);
        doc.ceiling_exposure.insert(c_kept, CeilingExposure::Party);

        assert!(
            purge_shadowed_exposure_entries(&mut doc, &storey),
            "shadowed entries dropped"
        );
        assert_eq!(doc.floor_exposure.len(), 1);
        assert_eq!(
            doc.floor_exposure.get(&f_kept),
            Some(&FloorExposure::NonHeatLoss),
            "un-shadowed NonHeatLoss floor mark kept"
        );
        assert_eq!(doc.ceiling_exposure.len(), 1);
        assert_eq!(
            doc.ceiling_exposure.get(&c_kept),
            Some(&CeilingExposure::Party),
            "un-shadowed Party ceiling mark kept"
        );
        assert!(
            !purge_shadowed_exposure_entries(&mut doc, &storey),
            "idempotent — nothing left to drop"
        );
    }

    /// The purge matches by CONTAINMENT, not centroid equality: a LEGACY
    /// auto-Party entry from a pre-feature record is keyed by the UNSPLIT
    /// region's centroid (no carve ring existed), which equals no post-split
    /// piece's centroid — but it lies inside the covered piece and must be
    /// dropped, or deleting the storey above would merge the pieces back and
    /// resurrect the stale mark. An entry keyed inside the UNCOVERED remainder
    /// is kept.
    #[test]
    fn purge_drops_legacy_entry_keyed_by_unsplit_centroid() {
        let p = Point2f64::new;
        // A 4×4 storey whose left 3 m are covered by the storey above: the
        // derivation split each surface into covered [0..3]×[0..4] (stamped
        // InterStorey) + remainder [3..4]×[0..4].
        let covered = vec![p(0.0, 0.0), p(3.0, 0.0), p(3.0, 4.0), p(0.0, 4.0)];
        let rest = vec![p(3.0, 0.0), p(4.0, 0.0), p(4.0, 4.0), p(3.0, 4.0)];
        let cid = ItemId::default();
        let mut storey = Storey {
            floors: FloorRegions::from_vertex_lists(vec![covered.clone(), rest.clone()], cid),
            ceilings: CeilingRegions::default_for_footprint(vec![covered, rest], 2.4, cid),
            ..Storey::default()
        };
        storey.floors.regions[0].exposure = FloorExposure::InterStorey;
        storey.ceilings.regions[0].exposure = CeilingExposure::InterStorey;

        // The legacy entries are keyed by the UNSPLIT 4×4 region's centroid
        // (2, 2) — inside the covered piece, equal to NEITHER piece centroid.
        let legacy = centroid_key(p(2.0, 2.0));
        assert_ne!(legacy, centroid_key(storey.floors.regions[0].centroid()));
        assert_ne!(legacy, centroid_key(storey.floors.regions[1].centroid()));
        // Assessor marks keyed inside the uncovered remainder survive.
        let kept = centroid_key(p(3.5, 2.0));

        let mut doc = StoreyDoc::default();
        doc.floor_exposure
            .insert(legacy, FloorExposure::NonHeatLoss);
        doc.ceiling_exposure.insert(legacy, CeilingExposure::Party);
        doc.floor_exposure.insert(kept, FloorExposure::NonHeatLoss);
        doc.ceiling_exposure.insert(kept, CeilingExposure::Party);

        assert!(
            purge_shadowed_exposure_entries(&mut doc, &storey),
            "the unsplit-keyed legacy entries are dropped by containment"
        );
        assert_eq!(doc.floor_exposure.len(), 1);
        assert!(
            doc.floor_exposure.contains_key(&kept),
            "remainder mark kept"
        );
        assert_eq!(doc.ceiling_exposure.len(), 1);
        assert!(doc.ceiling_exposure.contains_key(&kept));
        assert!(
            !purge_shadowed_exposure_entries(&mut doc, &storey),
            "idempotent"
        );
    }

    /// `retract_legacy_party_rings` drops exactly the `ceiling_splits` rings
    /// fully inside the removed footprint (the legacy party-carve rings) and
    /// keeps the rest; a second call reports no change.
    #[test]
    fn retract_legacy_party_rings_drops_contained_rings_only() {
        let mut below = storey_with(square_ring(0.0, 8.0));
        let inside = square_ring(1.0, 2.0); // fully inside the removed fp
        let outside = square_ring(10.0, 2.0); // fully outside it
        below.ceiling_splits = vec![inside, outside.clone()];

        let removed_fp = square_ring(0.0, 5.0);
        assert!(
            retract_legacy_party_rings(&mut below, &removed_fp),
            "the contained ring is dropped"
        );
        assert_eq!(below.ceiling_splits, vec![outside]);
        assert!(
            !retract_legacy_party_rings(&mut below, &removed_fp),
            "nothing left to retract"
        );
    }
}
