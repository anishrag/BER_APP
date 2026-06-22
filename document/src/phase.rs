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
/// authoritative zone map: the Original remainder reads `Original`, each
/// extension zone reads its own phase.
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
                    .unwrap_or(ConstructionPhase::Original),
            );
        }
    }
    None
}

/// Sort key for "older wins": smaller = older. `Original` is the oldest fabric
/// by definition (it predates every extension), so it floors the scale; an
/// extension orders by its construction year. A phase with no year set (can't
/// normally happen — only dated phases are assignable) sorts as newest.
fn phase_age(ber: &Ber, phase: &ConstructionPhase) -> u32 {
    match phase {
        ConstructionPhase::Original => u32::MIN,
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
        let (ty, makeup, cur_phase) = match ber.wall_construction(cur_cid) {
            Some(c) => (
                c.construction_type,
                c.base.construction_makeup.clone(),
                c.base.construction_phase.clone(),
            ),
            None => (
                Default::default(),
                Default::default(),
                ConstructionPhase::Original,
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
            let cid = ber.ensure_wall_construction(ty, makeup, new_phase);
            ber.storeys[idx].walls.regions[i].construction_id = cid;
        }
    }
}

/// Stamp the construction PHASE onto storey `idx`'s ROOF/CEILING regions from
/// the already phase-partitioned FLOORS — the area analogue of
/// [`stamp_wall_phases`]. The phase areas CARVE the roofs in `resolve_storey`
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
    let n = ber.storeys[idx].roofs.regions.len();
    for i in 0..n {
        let (c, cur_cid) = {
            let r = &ber.storeys[idx].roofs.regions[i];
            (r.centroid(), r.construction_id)
        };
        let (ty, makeup, cur_phase) = match ber.ceiling_construction(cur_cid) {
            Some(cc) => (
                cc.construction_type,
                cc.base.construction_makeup.clone(),
                cc.base.construction_phase.clone(),
            ),
            None => (
                Default::default(),
                Default::default(),
                ConstructionPhase::Original,
            ),
        };
        let new_phase =
            floor_phase_at(ber, idx, c.x as f32, c.y as f32).unwrap_or_else(|| cur_phase.clone());
        if new_phase != cur_phase {
            let cid = ber.ensure_ceiling_construction(ty, makeup, new_phase);
            ber.storeys[idx].roofs.regions[i].construction_id = cid;
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
    if ber.storeys.len() <= idx || doc.wall_constructions.is_empty() {
        return;
    }
    const TOL_M: f32 = 0.05;
    let plan = &doc.outline.floor_plan;
    let n = ber.storeys[idx].walls.regions.len();
    for i in 0..n {
        // The wall's perimeter SurfaceRef (by base midpoint) + its current phase.
        let (sref, phase) = {
            let r = &ber.storeys[idx].walls.regions[i];
            let Some((e0, e1)) = r.base_edge() else {
                continue;
            };
            let mid = domain::points::Point2f32::new(
                ((e0.x + e1.x) * 0.5) as f32,
                ((e0.y + e1.y) * 0.5) as f32,
            );
            let Some(sref) = plan.surface_ref_at(mid, TOL_M) else {
                continue; // riser / non-perimeter — no parametric wall key
            };
            let phase = ber
                .wall_construction(r.construction_id)
                .map(|c| c.base.construction_phase.clone())
                .unwrap_or_default();
            (sref, phase)
        };
        if let Some((ty, makeup)) = doc.wall_constructions.get(&sref) {
            let cid = ber.ensure_wall_construction(*ty, makeup.clone(), phase);
            ber.storeys[idx].walls.regions[i].construction_id = cid;
        }
    }
}

/// Compose each FLOOR region's ASSIGNED construction (type + make-up, from
/// `doc.floor_constructions` keyed by [`CentroidKey`]) with its DERIVED phase
/// (already set on the partitioned region's construction), re-`ensure`ing the
/// construction and pointing the region at it. The area analogue of
/// [`stamp_wall_constructions`]: a floor region's quantized centroid is its
/// durable key, so a user-set floor construction survives the re-derivation. A
/// region with no stored assignment keeps its derived (phase-only) construction.
pub fn stamp_floor_constructions(ber: &mut Ber, idx: usize, doc: &crate::StoreyDoc) {
    if ber.storeys.len() <= idx || doc.floor_constructions.is_empty() {
        return;
    }
    let n = ber.storeys[idx].floors.regions.len();
    for i in 0..n {
        let (key, phase) = {
            let r = &ber.storeys[idx].floors.regions[i];
            let phase = ber
                .floor_construction(r.construction_id)
                .map(|c| c.base.construction_phase.clone())
                .unwrap_or_default();
            (centroid_key(r.centroid()), phase)
        };
        if let Some((ty, makeup)) = doc.floor_constructions.get(&key) {
            let cid = ber.ensure_floor_construction(*ty, makeup.clone(), phase);
            ber.storeys[idx].floors.regions[i].construction_id = cid;
        }
    }
}

/// Compose each CEILING/ROOF region's ASSIGNED construction (type + make-up,
/// from `doc.ceiling_constructions` keyed by [`CentroidKey`]) with its DERIVED
/// phase, re-`ensure`ing the construction and pointing the region at it. Mirror
/// of [`stamp_floor_constructions`] over the storey's `roofs` regions.
pub fn stamp_ceiling_constructions(ber: &mut Ber, idx: usize, doc: &crate::StoreyDoc) {
    if ber.storeys.len() <= idx || doc.ceiling_constructions.is_empty() {
        return;
    }
    let n = ber.storeys[idx].roofs.regions.len();
    for i in 0..n {
        let (key, phase) = {
            let r = &ber.storeys[idx].roofs.regions[i];
            let phase = ber
                .ceiling_construction(r.construction_id)
                .map(|c| c.base.construction_phase.clone())
                .unwrap_or_default();
            (centroid_key(r.centroid()), phase)
        };
        if let Some((ty, makeup)) = doc.ceiling_constructions.get(&key) {
            let cid = ber.ensure_ceiling_construction(*ty, makeup.clone(), phase);
            ber.storeys[idx].roofs.regions[i].construction_id = cid;
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
/// regions (each non-`Original` floor region → a phase region). Phase regions
/// are session-only, so on reload they'd otherwise be empty and the NEXT commit
/// would re-partition with no areas — wiping the split. Reconstructing keeps
/// the partition reproducible and lets a new selection still snap to the
/// existing zones. Empty when nothing is tagged.
pub fn phase_regions_from_floors(ber: &Ber, idx: usize) -> Vec<PhaseRegion> {
    let Some(storey) = ber.storeys.get(idx) else {
        return Vec::new();
    };
    let mut regions: Vec<PhaseRegion> = Vec::new();
    for (fr, verts) in storey
        .floors
        .regions
        .iter()
        .zip(storey.floors.to_vertex_lists())
    {
        let phase = ber
            .floor_construction(fr.construction_id)
            .map(|c| c.base.construction_phase.clone())
            .unwrap_or(ConstructionPhase::Original);
        if phase == ConstructionPhase::Original {
            continue;
        }
        let ring: Vec<Point2f32> = verts.iter().copied().map(to_f32).collect();
        if ring.len() >= 3 {
            regions.push(PhaseRegion {
                polygon: ring,
                phase,
            });
        }
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::structural::{
        CeilingRegions, FloorRegions, Storey, WallExposure, WallRegions,
    };
    use domain::points::Point2f64;

    /// A 10×5 storey split into an Original zone (x < 5) and an Extension1 zone
    /// (x > 5), with walls along `segs`. Years: Original 1980, Extension1 2008.
    fn split_storey(segs: Vec<(Point2f64, Point2f64, WallExposure)>) -> Ber {
        let mut ber = Ber::default();
        ber.construction_phases
            .set_year(ConstructionPhase::Original, 1980);
        ber.construction_phases
            .set_year(ConstructionPhase::Extension1, 2008);
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
    fn phase_age_orders_original_oldest_then_by_year() {
        let mut ber = Ber::default();
        ber.construction_phases
            .set_year(ConstructionPhase::Extension1, 2008);
        ber.construction_phases
            .set_year(ConstructionPhase::Extension2, 1995);
        // Original is the oldest fabric regardless of any extension's year.
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
            ConstructionPhase::Original,
            "left perimeter → original"
        );
        // The party wall straddles two ages → the OLDER (Original) wins.
        assert_eq!(
            wall_phase_by_base(&ber, (5.0, 0.0), (5.0, 5.0)),
            ConstructionPhase::Original,
            "party wall → older structure (original)"
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
    fn phase_regions_from_floors_skips_original_regions() {
        let mut ber = Ber::default();
        let original_cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            ConstructionPhase::Original,
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
        let mut floors = FloorRegions::from_vertex_lists(vec![square(0.0)], original_cid);
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
        let roofs = CeilingRegions::default_for_footprint(vec![left, right], 2.4, ceil_cid);
        ber.storeys.push(Storey {
            floors,
            roofs,
            ..Storey::default()
        });

        stamp_ceiling_phases(&mut ber, 0);

        let phase_at = |x: f64, y: f64| -> ConstructionPhase {
            let r = ber.storeys[0]
                .roofs
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
}
