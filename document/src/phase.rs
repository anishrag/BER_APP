//! Construction-phase area logic: session-only tagged areas that stamp a
//! construction phase (= year of construction) onto fabric surfaces at commit.
//!
//! A phase region is NOT persisted in the BER — the durable record of a
//! surface's phase is its `construction_id`. The shell owns the per-storey
//! region *storage* (session state, deliberately outside undo and the editing
//! snapshot); the logic over the regions — stamping walls, rebuilding regions
//! from persisted floors — lives here.
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ConstructionPhase;
use domain::points::{to_f32, Point2f32, Point2f64};

/// Durable geometric key for an AREA region (floor / ceiling): its plan centroid
/// quantized to the mm grid. Area regions have no parametric `SurfaceRef`, so the
/// centroid is their stable identity across the re-derive — the area analogue of
/// [`riser_key`](crate::riser_key), with the same quantization and the same
/// caveat: it tracks a region that keeps its shape, but not one that gets
/// subdivided (the sub-regions' centroids differ).
pub type CentroidKey = (i64, i64);

/// Quantize a region centroid to a [`CentroidKey`] (each coord to the nearest mm).
pub fn centroid_key(c: Point2f64) -> CentroidKey {
    let q = |v: f64| (v * 1000.0).round() as i64;
    (q(c.x), q(c.y))
}

/// The [`CentroidKey`] of a plan RING (open or closed) — its area-weighted
/// polygon centroid quantized to the mm grid. Matches the key a resolved region
/// carved from this ring will carry, so it's the de-dup identity for a planned
/// carve against the durable `floor_splits`/`ceiling_splits`. Degenerate rings
/// (< 3 distinct pts / zero area) fall back to the vertex mean.
pub fn ring_centroid_key(ring: &[Point2f64]) -> CentroidKey {
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
    centroid_key(c)
}

/// Containment fallback for the floor centroid-keyed override side-tables
/// (`floor_exposure`, `floor_constructions`). The stamp passes look an override
/// up by the region's EXACT quantized centroid; when the assessor reshapes or
/// subdivides the floor AFTER attributing it, the region's centroid moves off the
/// stored key and the exact lookup misses. This recovers the override whose
/// stored centroid POINT still falls inside the region's polygon. Floor regions
/// partition the outline (non-overlapping), so a stored point lands in at most one
/// current region — no ambiguity. Uses `geo::Contains` on the full polygon (NOT
/// just its exterior ring), so a point in an interior carve hole correctly counts
/// as OUTSIDE a concave remainder. It only bridges the gap: the map re-keys itself
/// to the current centroids on the next save (the seed), and an exact match always
/// takes precedence, so unchanged geometry is unaffected.
///
/// LIMITATION: this fully covers RESHAPE (the moved centroid is still inside the
/// region). For a SUBDIVISION that splits one attributed region into several, only
/// the child piece CONTAINING the original centroid re-inherits; sibling pieces
/// fall back to the default. Full subdivision coverage would need the override to
/// store its region extent (overlap-match), not just a centroid point — deferred.
pub(crate) fn centroid_override_by_containment<'a, V>(
    map: &'a std::collections::HashMap<CentroidKey, V>,
    poly: &geo::Polygon<f64>,
) -> Option<&'a V> {
    use geo::Contains;
    map.iter().find_map(|(k, v)| {
        let p = geo::Coord {
            x: k.0 as f64 / 1000.0,
            y: k.1 as f64 / 1000.0,
        };
        poly.contains(&p).then_some(v)
    })
}

/// A session-only tagged area: a world-space (metre) ring + the phase to stamp
/// onto surfaces inside it.
#[derive(Debug, Clone)]
pub struct PhaseRegion {
    pub polygon: Vec<Point2f32>,
    pub phase: ConstructionPhase,
}

/// How far (metres) off a wall's centre-line we sample to read the floor zone
/// on each side. Small enough to stay within the adjacent floor region (rooms
/// are ≫ this), large enough to clear the shared boundary the wall sits on.
const SIDE_SAMPLE_OFFSET_M: f64 = 0.02;

/// The construction phase of the partitioned FLOOR region containing `(x, y)`
/// on storey `idx`, or `None` when the point lies outside the building (in no
/// floor region — e.g. the open-air side of a perimeter wall). The floors are
/// already phase-correct from the commit-time partition, so they are the
/// authoritative zone map: the untagged remainder reads `Unassigned`, each
/// tagged zone (Original / an extension) reads its own phase.
fn floor_phase_at(ber: &Ber, idx: usize, x: f32, y: f32) -> Option<ConstructionPhase> {
    let storey = ber.storeys.get(idx)?;
    for (fr, verts) in storey
        .floors
        .regions
        .iter()
        .zip(storey.floors.to_vertex_lists())
    {
        let ring: Vec<Point2f32> = verts.iter().copied().map(to_f32).collect();
        if ring.len() >= 3 && manip_plan::ring_contains_point(&ring, x, y) {
            return Some(
                ber.floor_construction(fr.construction_id)
                    .map(|c| c.base.construction_phase.clone())
                    .unwrap_or(ConstructionPhase::Unassigned),
            );
        }
    }
    None
}

/// Sort key for "older wins": smaller = older. `Unassigned` is the untagged
/// base fabric — older than anything dated — so it floors the scale; `Original`
/// is the oldest DATED fabric (it predates every extension), one notch above;
/// an extension orders by its construction year. A phase with no year set
/// (can't normally happen — only dated phases are assignable) sorts as newest.
fn phase_age(ber: &Ber, phase: &ConstructionPhase) -> u32 {
    match phase {
        ConstructionPhase::Unassigned => u32::MIN,
        ConstructionPhase::Original => u32::MIN + 1,
        p => ber.construction_phases.year_of(p).unwrap_or(u32::MAX),
    }
}

/// Stamp the construction PHASE onto storey `idx`'s WALLS from the already
/// phase-partitioned FLOORS (floors are stamped at commit-time partition; this
/// projects their zones onto the walls). Per wall, sample the floor zone a
/// hair off each side of its centre-line:
///  - a PERIMETER wall has building on one side and open air (no floor region)
///    on the other → it takes the interior side's phase, reliably, even though
///    its centre-line sits exactly on the zone boundary;
///  - a wall straddling two zones of DIFFERENT ages takes the OLDER phase (the
///    shared/party wall belongs to the earlier structure).
/// Type/make-up are preserved — only the phase axis changes, so the
/// construction is re-`ensure`d with the same `(type, makeup)`.
pub fn stamp_wall_phases(ber: &mut Ber, idx: usize) {
    if ber.storeys.len() <= idx {
        return;
    }
    let n_walls = ber.storeys[idx].walls.regions.len();
    for i in 0..n_walls {
        // Pull the wall's base edge + current construction out as owned values
        // so the floor reads below don't alias the (later) mutable reassign.
        let (s1, s2, cur_cid) = {
            let e = &ber.storeys[idx].walls.regions[i].polygon.exterior;
            if e.len() < 2 {
                continue;
            }
            let (ax, ay, bx, by) = (e[0].x, e[0].y, e[1].x, e[1].y);
            let (dx, dy) = (bx - ax, by - ay);
            let len = (dx * dx + dy * dy).sqrt();
            if len < 1e-9 {
                continue;
            }
            // Unit normal to the base edge; sample the floor a hair to each side.
            let (nx, ny) = (-dy / len, dx / len);
            let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
            let o = SIDE_SAMPLE_OFFSET_M;
            let s1 = ((mx + nx * o) as f32, (my + ny * o) as f32);
            let s2 = ((mx - nx * o) as f32, (my - ny * o) as f32);
            (s1, s2, ber.storeys[idx].walls.regions[i].construction_id)
        };
        let (ty, makeup, cur_phase, cur_name) = match ber.wall_construction(cur_cid) {
            Some(c) => (
                c.construction_type,
                c.base.construction_makeup.clone(),
                c.base.construction_phase.clone(),
                c.base.name.clone(),
            ),
            None => (
                Default::default(),
                Default::default(),
                ConstructionPhase::Unassigned,
                None,
            ),
        };
        // The zones on each side (open-air samples drop out); the OLDER wins.
        // No floor region on either side (degenerate) → keep the current phase.
        let new_phase = [
            floor_phase_at(ber, idx, s1.0, s1.1),
            floor_phase_at(ber, idx, s2.0, s2.1),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|p| phase_age(ber, p))
        .unwrap_or_else(|| cur_phase.clone());
        if new_phase != cur_phase {
            // Re-phase keeps the wall in ITS group: the name rides along (the
            // name is part of the construction identity now).
            let cid =
                ber.ensure_wall_construction_named(ty, makeup, new_phase, cur_name.as_deref());
            ber.storeys[idx].walls.regions[i].construction_id = cid;
        }
    }
}

/// Stamp the construction PHASE onto storey `idx`'s ROOF/CEILING regions from
/// the already phase-partitioned FLOORS — the area analogue of
/// [`stamp_wall_phases`]. The phase areas CARVE the ceilings in `resolve_storey`
/// (mirroring the floor partition), so each roof region lands fully within one
/// floor phase zone; this reads the floor phase under the region's CENTROID and
/// re-`ensure`s the ceiling construction with it. Type/make-up are preserved —
/// only the phase axis changes. A region whose centroid is over no floor zone
/// (degenerate) keeps its current phase. Runs BEFORE
/// [`stamp_ceiling_constructions`], so the user's assigned make-up composes on
/// top of the derived phase (same order as the wall pair).
pub fn stamp_ceiling_phases(ber: &mut Ber, idx: usize) {
    if ber.storeys.len() <= idx {
        return;
    }
    let n = ber.storeys[idx].ceilings.regions.len();
    for i in 0..n {
        let (c, cur_cid) = {
            let r = &ber.storeys[idx].ceilings.regions[i];
            (r.centroid(), r.construction_id)
        };
        let (ty, makeup, cur_phase, cur_name) = match ber.ceiling_construction(cur_cid) {
            Some(cc) => (
                cc.construction_type,
                cc.base.construction_makeup.clone(),
                cc.base.construction_phase.clone(),
                cc.base.name.clone(),
            ),
            None => (
                Default::default(),
                Default::default(),
                ConstructionPhase::Unassigned,
                None,
            ),
        };
        let new_phase =
            floor_phase_at(ber, idx, c.x as f32, c.y as f32).unwrap_or_else(|| cur_phase.clone());
        if new_phase != cur_phase {
            let cid =
                ber.ensure_ceiling_construction_named(ty, makeup, new_phase, cur_name.as_deref());
            ber.storeys[idx].ceilings.regions[i].construction_id = cid;
        }
    }
}

/// Compose each perimeter wall's ASSIGNED construction (type + make-up, from
/// `doc.wall_constructions` keyed by [`SurfaceRef`](manip_plan::SurfaceRef)) with
/// its DERIVED phase (already set by [`stamp_wall_phases`]), re-`ensure`ing the
/// construction and pointing the wall at it. Runs AFTER `stamp_wall_phases`. A
/// wall with no stored assignment keeps its derived construction. This is what
/// makes a user-set wall construction survive the re-derivation: its durable
/// home is the parametric `SurfaceRef` key, not the resolved wall id.
pub fn stamp_wall_constructions(ber: &mut Ber, idx: usize, doc: &crate::StoreyDoc) {
    if ber.storeys.len() <= idx
        || (doc.wall_constructions.is_empty() && doc.riser_constructions.is_empty())
    {
        return;
    }
    use domain::points::{Point2f32, Point2f64};
    const TOL_M: f32 = 0.05;
    let plan = &doc.outline.floor_plan;
    let n = ber.storeys[idx].walls.regions.len();
    for i in 0..n {
        let Some((e0, e1)) = ber.storeys[idx].walls.regions[i].base_edge() else {
            continue;
        };
        let mid = Point2f32::new(((e0.x + e1.x) * 0.5) as f32, ((e0.y + e1.y) * 0.5) as f32);
        // A perimeter wall is keyed by its parametric SurfaceRef; a RISER (interior
        // step face, no perimeter ref) by its quantized base edge — mirroring
        // riser exposure. The stored entry carries its own (editable) phase.
        let assigned = if let Some(sref) = plan.surface_ref_at(mid, TOL_M) {
            doc.wall_constructions.get(&sref)
        } else {
            let key = crate::riser_key(Point2f64::new(e0.x, e0.y), Point2f64::new(e1.x, e1.y));
            doc.riser_constructions.get(&key)
        };
        if let Some((ty, makeup, phase, name)) = assigned {
            let cid = ber.ensure_wall_construction_named(
                *ty,
                makeup.clone(),
                phase.clone(),
                name.as_deref(),
            );
            ber.storeys[idx].walls.regions[i].construction_id = cid;
        }
    }
}

/// Apply each FLOOR region's ASSIGNED construction (type + make-up + phase, from
/// `doc.floor_constructions` keyed by [`CentroidKey`]), re-`ensure`ing the
/// construction and pointing the region at it. The area analogue of
/// [`stamp_wall_constructions`]: a floor region's quantized centroid is its
/// durable key, so a user-set floor construction survives the re-derivation. The
/// stored phase (editable; defaulted to the derived one) is used directly. A
/// region with no stored assignment keeps its derived (phase-only) construction.
pub fn stamp_floor_constructions(ber: &mut Ber, idx: usize, doc: &crate::StoreyDoc) {
    if ber.storeys.len() <= idx || doc.floor_constructions.is_empty() {
        return;
    }
    let n = ber.storeys[idx].floors.regions.len();
    for i in 0..n {
        let key = centroid_key(ber.storeys[idx].floors.regions[i].centroid());
        // Exact centroid key first; containment fallback on a miss (reshape/subdivide).
        let entry = doc.floor_constructions.get(&key).or_else(|| {
            centroid_override_by_containment(
                &doc.floor_constructions,
                &ber.storeys[idx].floors.regions[i].polygon,
            )
        });
        if let Some((ty, makeup, phase, name)) = entry {
            let cid = ber.ensure_floor_construction_named(
                *ty,
                makeup.clone(),
                phase.clone(),
                name.as_deref(),
            );
            ber.storeys[idx].floors.regions[i].construction_id = cid;
        }
    }
}

/// Apply each CEILING/ROOF region's ASSIGNED construction (type + make-up +
/// phase, from `doc.ceiling_constructions` keyed by [`CentroidKey`]), re-
/// `ensure`ing the construction and pointing the region at it. Mirror of
/// [`stamp_floor_constructions`] over the storey's `ceilings` regions.
pub fn stamp_ceiling_constructions(ber: &mut Ber, idx: usize, doc: &crate::StoreyDoc) {
    if ber.storeys.len() <= idx || doc.ceiling_constructions.is_empty() {
        return;
    }
    let n = ber.storeys[idx].ceilings.regions.len();
    for i in 0..n {
        let key = centroid_key(ber.storeys[idx].ceilings.regions[i].centroid());
        // Exact centroid key first; containment fallback on a miss (the roof was
        // recomposed/re-carved on load, drifting its centroid off the seeded key).
        let entry = doc.ceiling_constructions.get(&key).or_else(|| {
            let ring: Vec<(f64, f64)> = ber.storeys[idx].ceilings.regions[i]
                .polygon
                .exterior
                .iter()
                .map(|v| (v.x, v.y))
                .collect();
            let plan = geo::Polygon::new(geo::LineString::from(ring), vec![]);
            centroid_override_by_containment(&doc.ceiling_constructions, &plan)
        });
        if let Some((ty, makeup, phase, name)) = entry {
            let cid = ber.ensure_ceiling_construction_named(
                *ty,
                makeup.clone(),
                phase.clone(),
                name.as_deref(),
            );
            ber.storeys[idx].ceilings.regions[i].construction_id = cid;
        }
    }
}

/// The phase regions a NEW upper storey INHERITS from the storey below it: the
/// below storey's tagged zones (rebuilt from its persisted floors), seeded onto
/// the new storey so a two-storey extension carries its construction date up
/// without re-drawing. The seed is clipped to the new outline by the resolve
/// partition and canonicalized by the post-commit reconstruction (the same
/// seed → commit → reconstruct flow as inherited floor steps), so a zone the
/// upper outline doesn't reach simply drops out. The phase's YEAR is BER-level
/// (entered once in Key Details), so nothing extra is propagated — only the
/// zone + its phase tag. Empty when nothing below is tagged.
pub fn inherited_phase_regions(ber: &Ber, below_idx: usize) -> Vec<PhaseRegion> {
    phase_regions_from_floors(ber, below_idx)
}

/// Rebuild the session phase regions for storey `idx` from its persisted floor
/// regions — ONE zone per distinct phase (the union of all same-phase floor
/// area), NOT one per floor region. Phase regions are session-only, so on
/// reload they'd otherwise be empty and the NEXT commit would re-partition with
/// no areas — wiping the split. Reconstructing keeps the partition reproducible
/// and lets a new selection still snap to the existing zones.
///
/// Crucially the zones are unioned PER PHASE (`FloorRegions::phase_zone_rings`):
/// a step or a differing make-up splits the floor into several regions that are
/// still one phase, and deriving a zone per region would seed those spurious
/// sub-divisions onto the storey above (which has no step) — splitting its
/// floor where the phase is actually uniform. `Unassigned` (the implicit
/// remainder) is skipped. Empty when nothing is tagged.
///
/// The zone derivation itself is [`Ber::storey_phase_zones`] (the data-model
/// home — the suggestion engine reads the same zones); this fn just projects
/// its f64 rings into the session `PhaseRegion` (f32) shape.
pub fn phase_regions_from_floors(ber: &Ber, idx: usize) -> Vec<PhaseRegion> {
    ber.storey_phase_zones(idx)
        .into_iter()
        .map(|(phase, ring)| PhaseRegion {
            polygon: ring.iter().copied().map(to_f32).collect(),
            phase,
        })
        .collect()
}

/// A segment counts as lying ALONG the outline when its endpoints AND midpoint
/// all sit within this of the ring (metres). 1 cm — far above the float noise
/// of the resolve partition (whose zone edges coincide with the outline
/// exactly), far below any real interior division's clearance.
const ON_OUTLINE_EPS_M: f32 = 0.01;

/// The boundary segments of storey `idx`'s phase zones that cross the
/// footprint INTERIOR — the snap references for a NEW upper storey's outline
/// stroke, so a wall traced beside the storey-below's construction-phase
/// division lands exactly ON it (and the inherited phase seed splits the new
/// floor along the real line, not a freehand offset).
///
/// Zones are rebuilt from the persisted floors ([`phase_regions_from_floors`]);
/// each zone ring contributes its consecutive vertex pairs (closing edge
/// included). Segments running ALONG `outline` (the storey's footprint ring)
/// are dropped — the outline carve already snaps those, and double targets
/// would fight. "Along" tests the endpoints AND the midpoint: a partition's
/// interior division runs wall to wall, so its ENDS touch the outline while
/// its middle crosses the interior — the midpoint keeps it alive. Empty when
/// nothing below is tagged.
pub fn phase_snap_segments(ber: &Ber, idx: usize, outline: &[Point2f32]) -> Vec<[Point2f32; 2]> {
    let on_outline = |p: Point2f32| manip_plan::dist_to_ring(outline, p) <= ON_OUTLINE_EPS_M;
    let mut segments: Vec<[Point2f32; 2]> = Vec::new();
    for region in phase_regions_from_floors(ber, idx) {
        let ring = &region.polygon;
        let n = ring.len();
        for i in 0..n {
            let (a, b) = (ring[i], ring[(i + 1) % n]);
            let mid = Point2f32::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
            if on_outline(a) && on_outline(b) && on_outline(mid) {
                continue;
            }
            segments.push([a, b]);
        }
    }
    segments
}

/// True when storey `idx`'s construction phase is fully assigned: every resolved
/// FLOOR region carries a construction whose `construction_phase` is NOT
/// `Unassigned` (the gate's "left unassigned" state). An empty / missing storey
/// is NOT complete. Reads the RESOLVED floors (Store A), so the caller must have
/// committed/materialized before this is meaningful — the checklist's
/// per-storey "Construction phase" rollup.
pub fn construction_phase_complete(ber: &Ber, idx: usize) -> bool {
    let Some(storey) = ber.storeys.get(idx) else {
        return false;
    };
    if storey.floors.regions.is_empty() {
        return false;
    }
    storey.floors.regions.iter().all(|fr| {
        ber.floor_construction(fr.construction_id)
            .map(|c| c.base.construction_phase != ConstructionPhase::Unassigned)
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::structural::{
        CeilingRegions, FloorRegions, Storey, WallExposure, WallRegions,
    };
    use domain::points::Point2f64;

    /// A 10×5 storey split into an UNASSIGNED (untagged base fabric) zone
    /// (x < 5) and an Extension1 zone (x > 5), with walls along `segs`. Years:
    /// Original 1980, Extension1 2008. The base zone is `Unassigned` (the
    /// untagged-remainder default), not `Original`.
    fn split_storey(segs: Vec<(Point2f64, Point2f64, WallExposure)>) -> Ber {
        let mut ber = Ber::default();
        ber.construction_phases
            .set_year(ConstructionPhase::Original, 1980);
        ber.construction_phases
            .set_year(ConstructionPhase::Extension1, 2008);
        let orig_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Unassigned,
        );
        let ext_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Extension1,
        );
        let wall_cid = ber.ensure_wall_construction(
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let left = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0),
            Point2f64::new(5.0, 5.0),
            Point2f64::new(0.0, 5.0),
        ];
        let right = vec![
            Point2f64::new(5.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 5.0),
            Point2f64::new(5.0, 5.0),
        ];
        let mut floors = FloorRegions::from_vertex_lists(vec![left], orig_cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![right], ext_cid).regions);
        let walls = WallRegions::regenerate(
            segs,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            wall_cid,
        );
        ber.storeys.push(Storey {
            floors,
            walls,
            ..Storey::default()
        });
        ber
    }

    /// The phase of the wall whose base edge is `a–b` (either orientation).
    fn wall_phase_by_base(ber: &Ber, a: (f64, f64), b: (f64, f64)) -> ConstructionPhase {
        let near = |p: &domain::points::Point3f64, x: f64, y: f64| {
            (p.x - x).abs() < 1e-6 && (p.y - y).abs() < 1e-6
        };
        let r = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| {
                let e = &r.polygon.exterior;
                (near(&e[0], a.0, a.1) && near(&e[1], b.0, b.1))
                    || (near(&e[0], b.0, b.1) && near(&e[1], a.0, a.1))
            })
            .expect("a wall with the given base edge");
        ber.wall_construction(r.construction_id)
            .unwrap()
            .base
            .construction_phase
            .clone()
    }

    #[test]
    fn phase_age_orders_unassigned_then_original_then_by_year() {
        let mut ber = Ber::default();
        ber.construction_phases
            .set_year(ConstructionPhase::Extension1, 2008);
        ber.construction_phases
            .set_year(ConstructionPhase::Extension2, 1995);
        // Unassigned (untagged base fabric) is older than the oldest DATED
        // fabric, Original.
        assert!(
            phase_age(&ber, &ConstructionPhase::Unassigned)
                < phase_age(&ber, &ConstructionPhase::Original)
        );
        // Original is the oldest DATED fabric regardless of any extension's year.
        assert!(
            phase_age(&ber, &ConstructionPhase::Original)
                < phase_age(&ber, &ConstructionPhase::Extension2)
        );
        // Among extensions, the earlier YEAR is older.
        assert!(
            phase_age(&ber, &ConstructionPhase::Extension2)
                < phase_age(&ber, &ConstructionPhase::Extension1)
        );
    }

    #[test]
    fn perimeter_walls_take_their_interior_zone_party_wall_takes_older() {
        let exposed = WallExposure::Exposed;
        let mut ber = split_storey(vec![
            // Right perimeter: building (extension zone) inside, open air outside.
            (
                Point2f64::new(10.0, 0.0),
                Point2f64::new(10.0, 5.0),
                exposed,
            ),
            // Interior PARTY wall on the boundary x = 5 (original | extension).
            (Point2f64::new(5.0, 0.0), Point2f64::new(5.0, 5.0), exposed),
            // Left perimeter: original zone inside, open air outside.
            (Point2f64::new(0.0, 5.0), Point2f64::new(0.0, 0.0), exposed),
        ]);
        stamp_wall_phases(&mut ber, 0);
        // Perimeter walls take the phase of the zone on their interior side,
        // even though their centre-line sits exactly on the zone boundary.
        assert_eq!(
            wall_phase_by_base(&ber, (10.0, 0.0), (10.0, 5.0)),
            ConstructionPhase::Extension1,
            "right perimeter → extension"
        );
        assert_eq!(
            wall_phase_by_base(&ber, (0.0, 5.0), (0.0, 0.0)),
            ConstructionPhase::Unassigned,
            "left perimeter → unassigned (untagged base)"
        );
        // The party wall straddles two ages → the OLDER (Unassigned base) wins.
        assert_eq!(
            wall_phase_by_base(&ber, (5.0, 0.0), (5.0, 5.0)),
            ConstructionPhase::Unassigned,
            "party wall → older structure (unassigned base)"
        );
    }

    #[test]
    fn inherited_phase_regions_lift_the_below_storeys_zones() {
        let ber = split_storey(vec![]);
        let inherited = inherited_phase_regions(&ber, 0);
        assert_eq!(inherited.len(), 1, "the one extension zone");
        assert_eq!(inherited[0].phase, ConstructionPhase::Extension1);
    }

    #[test]
    fn phase_regions_from_floors_skips_unassigned_regions() {
        let mut ber = Ber::default();
        let base_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Unassigned,
        );
        let ext_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Extension1,
        );
        let square = |off: f64| {
            vec![
                Point2f64::new(off, 0.0),
                Point2f64::new(off + 4.0, 0.0),
                Point2f64::new(off + 4.0, 4.0),
                Point2f64::new(off, 4.0),
            ]
        };
        let mut floors = FloorRegions::from_vertex_lists(vec![square(0.0)], base_cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![square(4.0)], ext_cid).regions);
        ber.storeys.push(Storey {
            floors,
            ..Storey::default()
        });

        let regions = phase_regions_from_floors(&ber, 0);
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].phase, ConstructionPhase::Extension1);
        assert_eq!(regions[0].polygon.len(), 4);
        // Missing storey → empty.
        assert!(phase_regions_from_floors(&ber, 7).is_empty());
    }

    /// A floor split into two ADJACENT regions that share ONE phase but carry
    /// DIFFERENT construction ids (the step case: the lowered piece gets its own
    /// construction, same phase) must reconstruct as a SINGLE phase zone — the
    /// union — not one zone per floor region. Regression for the bug where a
    /// stepped storey seeded two spurious phase boundaries onto the storey above
    /// (no step), splitting its uniform-phase floor. The merged ring is the
    /// whole 10×4 rectangle (the dissolved internal edge stripped).
    #[test]
    fn phase_zones_union_same_phase_floor_splits_into_one_zone() {
        use ber_model::ber::structural::FloorConstructionType;
        let mut ber = Ber::default();
        ber.construction_phases
            .set_year(ConstructionPhase::Original, 1980);
        // Two DIFFERENT floor constructions, SAME phase (Original) — as a step
        // would produce (distinct construction id, identical phase).
        let cid_a = ber.ensure_floor_construction(
            FloorConstructionType::SolidFloor,
            Default::default(),
            ConstructionPhase::Original,
        );
        let cid_b = ber.ensure_floor_construction(
            FloorConstructionType::SuspendedFloor,
            Default::default(),
            ConstructionPhase::Original,
        );
        assert_ne!(
            cid_a, cid_b,
            "the two pieces have distinct construction ids"
        );
        let left = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0),
            Point2f64::new(5.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let right = vec![
            Point2f64::new(5.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(5.0, 4.0),
        ];
        let mut floors = FloorRegions::from_vertex_lists(vec![left], cid_a);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![right], cid_b).regions);
        ber.storeys.push(Storey {
            floors,
            ..Storey::default()
        });

        let regions = phase_regions_from_floors(&ber, 0);
        assert_eq!(
            regions.len(),
            1,
            "one Original phase zone, not one per floor region: {:?}",
            regions.iter().map(|r| &r.phase).collect::<Vec<_>>()
        );
        assert_eq!(regions[0].phase, ConstructionPhase::Original);
        // The union of the two 5×4 halves is the 10×4 rectangle — 4 corners, the
        // dissolved internal edge at x=5 stripped as collinear.
        assert_eq!(
            regions[0].polygon.len(),
            4,
            "merged into one rectangle (collinear seam stripped): {:?}",
            regions[0].polygon
        );
    }

    /// `stamp_ceiling_phases` reads the floor phase under each ROOF region's
    /// centroid and re-points the region's construction at the matching phase —
    /// the ceiling analogue of `stamp_wall_phases`. With the roof already carved
    /// to one region per floor zone (resolve does this), the left half stays
    /// Original and the right half takes Extension1.
    #[test]
    fn stamp_ceiling_phases_tags_roofs_from_floor_zones() {
        let mut ber = Ber::default();
        let orig_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Original,
        );
        let ext_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Extension1,
        );
        let ceil_cid = ber.ensure_ceiling_construction(
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let left = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0),
            Point2f64::new(5.0, 5.0),
            Point2f64::new(0.0, 5.0),
        ];
        let right = vec![
            Point2f64::new(5.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 5.0),
            Point2f64::new(5.0, 5.0),
        ];
        let mut floors = FloorRegions::from_vertex_lists(vec![left.clone()], orig_cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![right.clone()], ext_cid).regions);
        // Two flat roof regions over the two halves, both starting at the
        // default (Original) ceiling construction.
        let ceilings = CeilingRegions::default_for_footprint(vec![left, right], 2.4, ceil_cid);
        ber.storeys.push(Storey {
            floors,
            ceilings,
            ..Storey::default()
        });

        stamp_ceiling_phases(&mut ber, 0);

        let phase_at = |x: f64, y: f64| -> ConstructionPhase {
            let r = ber.storeys[0]
                .ceilings
                .regions
                .iter()
                .find(|r| {
                    let c = r.centroid();
                    (c.x - x).abs() < 0.6 && (c.y - y).abs() < 0.6
                })
                .expect("a roof region near that centroid");
            ber.ceiling_construction(r.construction_id)
                .unwrap()
                .base
                .construction_phase
                .clone()
        };
        assert_eq!(
            phase_at(2.5, 2.5),
            ConstructionPhase::Original,
            "left roof → original"
        );
        assert_eq!(
            phase_at(7.5, 2.5),
            ConstructionPhase::Extension1,
            "right roof → extension"
        );
    }

    /// `construction_phase_complete` is true only when EVERY floor region's
    /// construction is non-`Unassigned`: an Unassigned remainder fails the gate,
    /// an all-tagged storey passes, and an empty / missing storey fails.
    #[test]
    fn construction_phase_complete_requires_every_floor_tagged() {
        let mut ber = Ber::default();
        let unassigned_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Unassigned,
        );
        let ext_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Extension1,
        );
        let square = |off: f64| {
            vec![
                Point2f64::new(off, 0.0),
                Point2f64::new(off + 4.0, 0.0),
                Point2f64::new(off + 4.0, 4.0),
                Point2f64::new(off, 4.0),
            ]
        };
        // One Unassigned + one Extension1 region → NOT complete (the gate's
        // "left unassigned" state).
        let mut floors = FloorRegions::from_vertex_lists(vec![square(0.0)], unassigned_cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![square(4.0)], ext_cid).regions);
        ber.storeys.push(Storey {
            floors,
            ..Storey::default()
        });
        assert!(
            !construction_phase_complete(&ber, 0),
            "an Unassigned floor region fails the gate"
        );

        // Re-point the Unassigned region at the Extension1 construction → all
        // tagged → complete.
        ber.storeys[0].floors.regions[0].construction_id = ext_cid;
        assert!(
            construction_phase_complete(&ber, 0),
            "all floors tagged → complete"
        );

        // Empty / missing storey → never complete.
        ber.storeys.push(Storey::default());
        assert!(
            !construction_phase_complete(&ber, 1),
            "a storey with no floor regions fails"
        );
        assert!(
            !construction_phase_complete(&ber, 9),
            "a missing storey fails"
        );
    }
}
