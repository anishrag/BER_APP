//! Construction-phase area logic: session-only tagged areas that stamp a
//! construction phase (= year of construction) onto fabric surfaces at commit.
//!
//! A phase region is NOT persisted in the BER — the durable record of a
//! surface's phase is its `construction_id`. The shell owns the per-storey
//! region *storage* (session state, deliberately outside undo and the editing
//! snapshot); the logic over the regions — stamping walls, rebuilding regions
//! from persisted floors — lives here.
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::ConstructionPhase;
use domain::points::Point2f32;

/// A session-only tagged area: a world-space (metre) ring + the phase to stamp
/// onto surfaces inside it.
#[derive(Debug, Clone)]
pub struct PhaseRegion {
    pub polygon: Vec<Point2f32>,
    pub phase: ConstructionPhase,
}

/// The construction phase to stamp on a surface at `(x, y)` (storey metre
/// space), given its `existing` phase. The agreed rule: a phase region
/// containing the point WINS (latest tag first); else preserve a non-`Original`
/// existing phase; else `Original`.
fn resolved_phase(
    regions: &[PhaseRegion],
    x: f32,
    y: f32,
    existing: ConstructionPhase,
) -> ConstructionPhase {
    if let Some(r) = regions
        .iter()
        .rev()
        .find(|r| manip_2d_plan::ring_contains_point(&r.polygon, x, y))
    {
        r.phase.clone()
    } else if existing != ConstructionPhase::Original {
        existing
    } else {
        ConstructionPhase::Original
    }
}

/// Stamp the construction PHASE onto storey `idx`'s WALLS from the session
/// phase regions (floors are already phase-correct from the commit-time
/// partition). Per wall, by its base midpoint: a region containing it WINS
/// (latest first); else the wall keeps its existing non-`Original` phase
/// (rebuild inherited it); else `Original`. Type/make-up are preserved — only
/// the phase axis changes, so the construction is re-`ensure`d with the same
/// `(type, makeup)`.
pub fn stamp_wall_phases(ber: &mut Ber, idx: usize, regions: &[PhaseRegion]) {
    if ber.storeys.len() <= idx {
        return;
    }
    let n_walls = ber.storeys[idx].walls.regions.len();
    for i in 0..n_walls {
        let Some(mid) = ber.storeys[idx].walls.regions[i].base_midpoint() else {
            continue;
        };
        let cur_cid = ber.storeys[idx].walls.regions[i].construction_id;
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
        let new_phase = resolved_phase(regions, mid.x as f32, mid.y as f32, cur_phase.clone());
        if new_phase != cur_phase {
            let cid = ber.ensure_wall_construction(ty, makeup, new_phase);
            ber.storeys[idx].walls.regions[i].construction_id = cid;
        }
    }
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
        let ring: Vec<Point2f32> = verts
            .iter()
            .map(|p| Point2f32::new(p.x as f32, p.y as f32))
            .collect();
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
    use datatypes::ber::common::ItemId;
    use datatypes::ber::structural::{FloorRegions, Storey};
    use domain::points::Point2f64;

    fn ring(side: f32) -> Vec<Point2f32> {
        vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(side, 0.0),
            Point2f32::new(side, side),
            Point2f32::new(0.0, side),
        ]
    }

    fn region(side: f32, phase: ConstructionPhase) -> PhaseRegion {
        PhaseRegion {
            polygon: ring(side),
            phase,
        }
    }

    #[test]
    fn containing_region_wins_latest_first() {
        let regions = vec![
            region(10.0, ConstructionPhase::Extension1),
            region(5.0, ConstructionPhase::Extension2),
        ];
        // Inside both → the LATER tag wins.
        assert_eq!(
            resolved_phase(&regions, 1.0, 1.0, ConstructionPhase::Original),
            ConstructionPhase::Extension2
        );
        // Inside only the first.
        assert_eq!(
            resolved_phase(&regions, 8.0, 8.0, ConstructionPhase::Original),
            ConstructionPhase::Extension1
        );
    }

    #[test]
    fn outside_preserves_existing_non_original_phase() {
        let regions = vec![region(5.0, ConstructionPhase::Extension1)];
        assert_eq!(
            resolved_phase(&regions, 20.0, 20.0, ConstructionPhase::Extension3),
            ConstructionPhase::Extension3
        );
        assert_eq!(
            resolved_phase(&regions, 20.0, 20.0, ConstructionPhase::Original),
            ConstructionPhase::Original
        );
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

    #[test]
    fn stamp_wall_phases_reassigns_contained_walls_only() {
        use datatypes::ber::structural::WallRegions;
        let mut ber = Ber::default();
        let wall_cid = ber.ensure_wall_construction(
            Default::default(),
            Default::default(),
            Default::default(),
        );
        // Two walls: one inside the 5×5 phase region, one far outside.
        let segs = vec![
            (
                Point2f64::new(1.0, 1.0),
                Point2f64::new(3.0, 1.0),
                datatypes::ber::structural::WallExposure::Exposed,
            ),
            (
                Point2f64::new(20.0, 20.0),
                Point2f64::new(24.0, 20.0),
                datatypes::ber::structural::WallExposure::Exposed,
            ),
        ];
        // This fixture tests phase stamping, not roofs — empty roofs give the
        // flat 2.4 m top, which is all the base-midpoint hit-test needs.
        let walls = WallRegions::regenerate(
            segs,
            &WallRegions::default(),
            &datatypes::ber::structural::CeilingRegions::default(),
            0.0,
            2.4,
            wall_cid,
        );
        ber.storeys.push(Storey {
            walls,
            ..Storey::default()
        });

        stamp_wall_phases(&mut ber, 0, &[region(5.0, ConstructionPhase::Extension1)]);

        let phase_of = |i: usize| {
            let cid = ber.storeys[0].walls.regions[i].construction_id;
            ber.wall_construction(cid)
                .unwrap()
                .base
                .construction_phase
                .clone()
        };
        assert_eq!(phase_of(0), ConstructionPhase::Extension1);
        assert_eq!(phase_of(1), ConstructionPhase::Original);
    }
}
