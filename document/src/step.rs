//! Floor-step area logic: session-only areas that raise or lower part of the
//! storey floor at commit.
//!
//! A step region is NOT persisted as a region of its own — the durable record
//! is `FloorRegion.level_m` on the persisted floor regions it re-splits. The
//! shell owns the per-storey region *storage* (session state, deliberately
//! outside undo and the editing snapshot); the logic over the regions —
//! rebuilding them from persisted floors — lives here.
use datatypes::ber::ber_struct::Ber;
use domain::points::Point2f32;

/// A session-only step area: a world-space (metre) ring + the floor level
/// inside it (metres from the storey datum; + raised, − lowered).
#[derive(Debug, Clone)]
pub struct StepRegion {
    pub polygon: Vec<Point2f32>,
    pub level_m: f64,
}

/// Rebuild the session step regions for storey `idx` from its persisted floor
/// regions (each region with non-zero `level_m` → one step region). Step
/// regions are session-only; without this, the next commit would re-level
/// with no areas and flatten the floor. Empty when nothing is stepped.
pub fn step_regions_from_floors(ber: &Ber, idx: usize) -> Vec<StepRegion> {
    let Some(storey) = ber.storeys.get(idx) else {
        return Vec::new();
    };
    let mut regions: Vec<StepRegion> = Vec::new();
    for (fr, verts) in storey
        .floors
        .regions
        .iter()
        .zip(storey.floors.to_vertex_lists())
    {
        if fr.level_m == 0.0 {
            continue;
        }
        let ring: Vec<Point2f32> = verts
            .iter()
            .map(|p| Point2f32::new(p.x as f32, p.y as f32))
            .collect();
        if ring.len() >= 3 {
            regions.push(StepRegion {
                polygon: ring,
                level_m: fr.level_m,
            });
        }
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;
    use datatypes::ber::structural::{FloorRegions, Storey};
    use domain::points::Point2f64;

    #[test]
    fn step_regions_from_floors_skips_level_zero_regions() {
        let mut ber = Ber::default();
        let cid = ber.ensure_floor_construction(
            Default::default(),
            Default::default(),
            Default::default(),
        );
        let square = |off: f64| {
            vec![
                Point2f64::new(off, 0.0),
                Point2f64::new(off + 4.0, 0.0),
                Point2f64::new(off + 4.0, 4.0),
                Point2f64::new(off, 4.0),
            ]
        };
        let mut floors = FloorRegions::from_vertex_lists(vec![square(0.0)], 2.4, cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![square(4.0)], 2.4, cid).regions);
        floors.regions[1].level_m = -0.3;
        ber.storeys.push(Storey {
            floors,
            ..Storey::default()
        });

        let regions = step_regions_from_floors(&ber, 0);
        assert_eq!(regions.len(), 1);
        assert!((regions[0].level_m - (-0.3)).abs() < 1e-9);
        assert_eq!(regions[0].polygon.len(), 4);
        // The stepped region's ring, not the level-0 one.
        assert!(regions[0].polygon.iter().any(|p| (p.x - 8.0).abs() < 1e-6));
        // Missing storey → empty.
        assert!(step_regions_from_floors(&ber, 7).is_empty());
    }
}
