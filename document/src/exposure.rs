//! Wall-exposure model operations: painting / clearing per-wall heat-loss
//! sections on the storey outline, deriving full coverage, and classifying
//! the resolved storey's derived RISER walls (the dashed-line picker's
//! data). Pure model logic — the shell owns pointer selection, undo, and
//! persistence.
use crate::{centroid_key, CentroidKey, ExposureSection, StoreyDoc};
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::{
    CeilingExposure, FloorExposure, Storey, SurfaceKind, WallExposure, WallRegions,
};
use domain::points::{Point2f32, Point2f64};
use manip_plan::{project_distance_along, ring_within_ring, segment_len, VertexId};
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

    /// This storey's heat-loss wall SEGMENTS over the FULL perimeter: every
    /// outline wall split into its coverage ranges — painted sections plus
    /// `Unset` gaps, an unmarked wall yielding one full-length `Unset` segment
    /// — as `(start, end, exposure)` in world coords (endpoints interpolated
    /// along the wall by distance). The shell projects these into BER
    /// `WallRegion`s, so every wall exists in the resolved geometry whether
    /// it's been marked or not.
    pub fn wall_exposure_segments(&self) -> Vec<(Point2f64, Point2f64, WallExposure)> {
        self.outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .flat_map(|(vid, a, b)| {
                let sections = self
                    .wall_sections
                    .get(&vid)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                let lerp = move |d: f32| {
                    let t = if len > 1e-6 { d / len } else { 0.0 };
                    Point2f64::new(
                        (a.x + t * (b.x - a.x)) as f64,
                        (a.y + t * (b.y - a.y)) as f64,
                    )
                };
                wall_coverage(len, sections)
                    .into_iter()
                    .map(move |(s, e, exposure)| (lerp(s), lerp(e), exposure))
                    .collect::<Vec<_>>()
            })
            .collect()
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
        let perim = self.outline.floor_plan.perimeter_walls();
        let mut out: HashMap<VertexId, Vec<ExposureSection>> = HashMap::new();
        for region in &walls.regions {
            if matches!(region.exposure, WallExposure::Unset | WallExposure::Ground) {
                continue; // derived, not painted — never recovers to sections
            }
            // The TRUE footprint base edge a→b (via `base_frame`), NOT raw
            // `exterior[0]/[1]`: over a floor-step-crossed wall the base is a
            // stepped profile, so `exterior[1]` is the first breakpoint, not
            // the far endpoint — reading it raw would recover the painted
            // section as `0..step` and silently drop the paint beyond the step.
            let Some((origin, (ux, uy), wlen, _base_z)) = region.base_frame() else {
                continue;
            };
            let p0 = Point2f32::new(origin.x as f32, origin.y as f32);
            let p1 = Point2f32::new((origin.x + ux * wlen) as f32, (origin.y + uy * wlen) as f32);
            let mid = Point2f32::new((p0.x + p1.x) * 0.5, (p0.y + p1.y) * 0.5);
            let Some((vid, a, b)) = perim.iter().copied().find(|(_, a, b)| {
                domain::points::point_to_segment_distance(
                    mid.x as f64,
                    mid.y as f64,
                    a.x as f64,
                    a.y as f64,
                    b.x as f64,
                    b.y as f64,
                ) < 1e-2
            }) else {
                continue; // region's base edge isn't on any current wall
            };
            let len = segment_len(a, b);
            let d0 = project_distance_along(a, b, len, p0.x, p0.y);
            let d1 = project_distance_along(a, b, len, p1.x, p1.y);
            out.entry(vid).or_default().push(ExposureSection {
                start_m: d0.min(d1),
                end_m: d0.max(d1),
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
///  - storey-0 floor → `Ground`; an intermediate floor fully covered by the
///    storey BELOW → `NonHeatLoss` (party floor to the dwelling below);
///  - top-storey ceiling → `ExposedRoof`; an intermediate ceiling fully covered
///    by the storey ABOVE → `Party` (party ceiling to the dwelling above).
pub fn auto_derive_horizontal_exposure(
    doc: &mut StoreyDoc,
    ber: &Ber,
    idx: usize,
    n_storeys: usize,
) {
    let Some(storey) = ber.storeys.get(idx) else {
        return;
    };
    // Floors: storey 0 is ground-bearing; an intermediate floor fully over the
    // storey below is a party floor.
    let below_footprint = idx
        .checked_sub(1)
        .and_then(|b| ber.storeys.get(b))
        .and_then(|s| s.floors.footprint_ring());
    let floor_rings = storey.floors.to_vertex_lists();
    for (region, ring) in storey.floors.regions.iter().zip(&floor_rings) {
        let derived = if idx == 0 {
            Some(FloorExposure::Ground)
        } else if let Some(below) = &below_footprint {
            ring_within_ring(ring, below, CONTAINMENT_TOL_M).then_some(FloorExposure::NonHeatLoss)
        } else {
            None
        };
        if let Some(e) = derived {
            doc.floor_exposure
                .entry(centroid_key(region.centroid()))
                .or_insert(e);
        }
    }
    // Ceilings: the top storey's ceiling is the exposed roof; an intermediate
    // ceiling fully under the storey above is a party ceiling.
    let above_footprint = (idx + 1 < n_storeys)
        .then(|| ber.storeys.get(idx + 1))
        .flatten()
        .and_then(|s| s.floors.footprint_ring());
    for region in &storey.roofs.regions {
        let ring = ceiling_ring_xy(region);
        let derived = if idx + 1 >= n_storeys {
            Some(CeilingExposure::ExposedRoof)
        } else if let Some(above) = &above_footprint {
            ring_within_ring(&ring, above, CONTAINMENT_TOL_M).then_some(CeilingExposure::Party)
        } else {
            None
        };
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

    /// Auto-derive on a clean 2-storey stack: storey-0 floor → Ground, the top
    /// (storey-1) ceiling → ExposedRoof. The intermediate surfaces between the
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

        assert_eq!(d0.floor_exposure.get(&f0), Some(&FloorExposure::Ground));
        assert_eq!(d0.ceiling_exposure.get(&c0), Some(&CeilingExposure::Party));
        assert_eq!(
            d1.floor_exposure.get(&f1),
            Some(&FloorExposure::NonHeatLoss)
        );
        assert_eq!(
            d1.ceiling_exposure.get(&c1),
            Some(&CeilingExposure::ExposedRoof)
        );
    }

    /// Auto-derive never clobbers a manual value: a manual Exposed floor on
    /// storey 0 stays Exposed even though the auto-pass would write Ground.
    #[test]
    fn auto_derive_does_not_clobber_a_manual_value() {
        let mut ber = Ber::default();
        ber.storeys.push(storey_with(square_ring(0.0, 4.0)));

        let mut doc = StoreyDoc::default();
        let key = centroid_key(ber.storeys[0].floors.regions[0].centroid());
        doc.floor_exposure.insert(key, FloorExposure::Exposed);

        auto_derive_horizontal_exposure(&mut doc, &ber, 0, 1);
        assert_eq!(
            doc.floor_exposure.get(&key),
            Some(&FloorExposure::Exposed),
            "manual override wins over the auto Ground"
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
}
