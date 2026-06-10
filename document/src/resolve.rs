//! The resolve projection: editing model → resolved BER geometry. The inverse
//! of the load path (`load.rs`). PURE over its inputs — the shell owns *when*
//! to project (commit/materialize/persist) and the revision bookkeeping.
use crate::{PhaseRegion, StepRegion, StoreyDoc};
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::{ConstructionPhase, ItemId};
use datatypes::ber::structural::{FloorRegions, Storey, WallRegions};
use domain::points::{to_f64, Point2f64};

/// Default storey ceiling height in metres, used as a placeholder until the UI
/// collects it from the assessor.
pub const DEFAULT_CEILING_HEIGHT_M: f64 = 2.4;

/// The `&mut` prerequisites of the otherwise read-only [`resolve_storey`]:
/// lazy-create the storey slot if missing, and register (find-or-insert) every
/// construction the projection will look up — the default floor + wall, plus
/// one floor per phase region on this storey. Idempotent.
pub fn ensure_storey_constructions(ber: &mut Ber, idx: usize, phase_regions: &[PhaseRegion]) {
    while ber.storeys.len() <= idx {
        ber.storeys.push(Storey::default());
    }
    ber.ensure_floor_construction(Default::default(), Default::default(), Default::default());
    ber.ensure_wall_construction(Default::default(), Default::default(), Default::default());
    for region in phase_regions {
        ber.ensure_floor_construction(Default::default(), Default::default(), region.phase.clone());
    }
}

/// PURE projection of one storey from the editing model into a `Storey`. The
/// caller has already synced the active buffer and ensured the storey + its
/// constructions exist ([`ensure_storey_constructions`]), so this only LOOKS UP
/// construction ids and never mutates `ber` — letting it run for ANY storey,
/// not just the active one. `phase_regions` and `step_regions` are the
/// storey's session areas: phases PARTITION the floor by construction, then
/// steps RE-SPLIT the resulting regions and stamp `level_m` on the pieces
/// (sequential composition — the phase partition is untouched).
pub fn resolve_storey(
    s: &StoreyDoc,
    ber: &Ber,
    idx: usize,
    phase_regions: &[PhaseRegion],
    step_regions: &[StepRegion],
) -> Storey {
    // The storey footprint is always the OUTLINE (never a living area, even
    // when one is active). One ring → one FloorRegion.
    let polygons_f64: Vec<Vec<Point2f64>> = s
        .outline
        .floor_plan
        .polygons()
        .into_iter()
        .map(|ring| ring.into_iter().map(to_f64).collect())
        .collect();

    // The marked heat-loss wall segments (one per exposure section of each
    // marked wall). ONLY marked walls become regions; an un-marked wall stays
    // "unset" (and `WallRegions` is the single source of truth, so that
    // distinction survives reload). Each is extruded below to
    // `DEFAULT_CEILING_HEIGHT_M` (the provisional height) until real heights
    // are entered.
    let wall_segments = s.wall_exposure_segments();

    // Resolved living-area polygons (each slot is current after sync_active).
    // Stored in `Storey.living_areas`, NOT the snapshot — they're
    // non-editable on reload, so their resolved geometry suffices.
    let living_area_lists: Vec<Vec<Point2f64>> = s
        .living_areas
        .iter()
        .map(|slot| {
            slot.floor_plan
                .get_points()
                .into_iter()
                .map(to_f64)
                .collect()
        })
        .collect();

    // Phase areas for this storey (world/metre rings + phase) — they drive the
    // floor PARTITION below.
    let phase_area_rings: Vec<(Vec<Point2f64>, ConstructionPhase)> = phase_regions
        .iter()
        .map(|r| {
            let ring = r.polygon.iter().copied().map(to_f64).collect();
            (ring, r.phase.clone())
        })
        .collect();
    // Construction ids are LOOKED UP, not created — the caller ensured them
    // (`ensure_storey_constructions`), keeping this read-only. `floor_cid` is
    // the default (unset, `Original`) floor: the partition REMAINDER + living
    // areas. A phase area whose floor somehow wasn't ensured falls back to it.
    let floor_cid = ber
        .floor_construction_id(Default::default(), Default::default(), Default::default())
        .expect("base floor construction ensured before resolve");
    let wall_cid = ber
        .wall_construction_id(Default::default(), Default::default(), Default::default())
        .expect("base wall construction ensured before resolve");
    // PARTITION each outline ring by the phase areas → one region per phase
    // zone + the remainder.
    let areas_with_cids: Vec<(Vec<Point2f64>, ItemId)> = phase_area_rings
        .iter()
        .map(|(ring, phase)| {
            let cid = ber
                .floor_construction_id(Default::default(), Default::default(), phase.clone())
                .unwrap_or(floor_cid);
            (ring.clone(), cid)
        })
        .collect();
    // Clone the existing storey (keeps id/name/number/roofs/openings) and
    // override only the derived geometry — so this RETURNS a Storey rather
    // than mutating the record in place.
    let mut storey = ber.storeys[idx].clone();
    if polygons_f64.iter().any(|p| p.len() >= 3) {
        storey.floors = if areas_with_cids.is_empty() {
            // No phase zones → keep the whole-outline floor verbatim (NO geo
            // boolean ops, so the stored points round-trip exactly and the
            // rebuilt outline isn't re-normalised — which broke collinearity).
            FloorRegions::from_vertex_lists(polygons_f64, DEFAULT_CEILING_HEIGHT_M, floor_cid)
        } else {
            // Tagged → partition each outline ring into phase zones.
            let mut regions = Vec::new();
            for ring in &polygons_f64 {
                if ring.len() < 3 {
                    continue;
                }
                let part = FloorRegions::partition(
                    ring,
                    &areas_with_cids,
                    floor_cid,
                    DEFAULT_CEILING_HEIGHT_M,
                );
                regions.extend(part.regions);
            }
            FloorRegions { regions }
        };
    }
    // Step areas re-split the (possibly phase-partitioned) floor regions and
    // stamp `level_m` on the intersected pieces. Skipped entirely when empty —
    // `with_levels` would be a no-op anyway, but not calling it at all keeps
    // the verbatim branch above free of geo ops (exact point round-trip).
    if !step_regions.is_empty() {
        let step_areas: Vec<(Vec<Point2f64>, f64)> = step_regions
            .iter()
            .map(|r| {
                let ring: Vec<Point2f64> = r.polygon.iter().copied().map(to_f64).collect();
                (ring, r.level_m)
            })
            .collect();
        storey.floors = std::mem::take(&mut storey.floors).with_levels(&step_areas);
    }
    storey.living_areas =
        FloorRegions::from_vertex_lists(living_area_lists, DEFAULT_CEILING_HEIGHT_M, floor_cid);
    // Re-derive walls from exposure (empty when nothing is marked),
    // preserving the construction link of the region a split came from.
    // SEAM (deferred until after roofs): riser WALL regions will be derived HERE,
    // from adjacent floor regions with differing `level_m` — one wall region per
    // shared boundary segment, spanning the two levels; segments whose lower side
    // sits below grade (level_m < 0) get an exposed-to-ground exposure, plus
    // below-grade perimeter strips along the outline of lowered regions. The
    // shared-edge extraction belongs in datatypes (a FloorRegions method); the
    // derivation belongs here, alongside the rebuild it must merge with.
    storey.walls = WallRegions::rebuild_preserving(
        wall_segments,
        &storey.walls,
        0.0,
        DEFAULT_CEILING_HEIGHT_M,
        wall_cid,
    );
    storey
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShapeSlot;
    use datatypes::ber::structural::WallExposure;
    use domain::points::Point2f32;
    use manip_2d_plan::FloorPlan;

    /// Golden projection: a finalised 4×4 m square outline with one wall marked
    /// SemiExposed projects to one floor region (default ceiling height, exact
    /// ring round-trip), one wall region carrying the mark, no living areas.
    #[test]
    fn golden_projection_of_a_square_storey() {
        let pts = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ];
        let plan = FloorPlan::from_clean_ring(&pts).unwrap();
        let mut doc = StoreyDoc::default();
        doc.dimensioning.scale_set = true;
        doc.floor_plan = plan.clone();
        doc.outline = ShapeSlot {
            floor_plan: plan,
            dimensioning: doc.dimensioning.clone(),
        };
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_2d_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::SemiExposed);

        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        let storey = resolve_storey(&doc, &ber, 0, &[], &[]);

        // Floor: the whole outline verbatim, extruded to the default height.
        assert_eq!(storey.floors.regions.len(), 1);
        assert!((storey.floors.regions[0].height - DEFAULT_CEILING_HEIGHT_M).abs() < 1e-9);
        let ring = &storey.floors.to_vertex_lists()[0];
        assert_eq!(ring.len(), 4);
        assert!(ring
            .iter()
            .any(|p| (p.x - 4.0).abs() < 1e-9 && (p.y - 4.0).abs() < 1e-9));

        // Walls: only the marked wall becomes a region, carrying its exposure
        // and reaching the provisional ceiling height.
        assert_eq!(storey.walls.regions.len(), 1);
        assert_eq!(storey.walls.regions[0].exposure, WallExposure::SemiExposed);
        assert!(storey.walls.regions[0]
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - DEFAULT_CEILING_HEIGHT_M).abs() < 1e-9));

        assert!(storey.living_areas.regions.is_empty());
    }

    /// A finalised square StoreyDoc with the given corner points as its outline.
    fn square_doc(pts: &[Point2f32]) -> StoreyDoc {
        let plan = FloorPlan::from_clean_ring(pts).unwrap();
        let mut doc = StoreyDoc::default();
        doc.dimensioning.scale_set = true;
        doc.floor_plan = plan.clone();
        doc.outline = ShapeSlot {
            floor_plan: plan,
            dimensioning: doc.dimensioning.clone(),
        };
        doc
    }

    /// One step region over the left half of a 4×4 storey splits the floor into
    /// two regions: the stepped half carrying its `level_m`, the untouched half
    /// at 0.0.
    #[test]
    fn step_region_splits_floor_into_level_regions() {
        let doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let steps = vec![crate::StepRegion {
            polygon: vec![
                Point2f32::new(0.0, 0.0),
                Point2f32::new(2.0, 0.0),
                Point2f32::new(2.0, 4.0),
                Point2f32::new(0.0, 4.0),
            ],
            level_m: -0.3,
        }];

        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        let storey = resolve_storey(&doc, &ber, 0, &[], &steps);

        assert_eq!(storey.floors.regions.len(), 2);
        // Unsigned shoelace area of a ring (no geo dependency in this crate).
        let ring_area = |ring: &[Point2f64]| {
            let n = ring.len();
            let mut s = 0.0;
            for i in 0..n {
                let (a, b) = (ring[i], ring[(i + 1) % n]);
                s += a.x * b.y - b.x * a.y;
            }
            s.abs() / 2.0
        };
        let rings = storey.floors.to_vertex_lists();
        let area_at_level = |level: f64| {
            storey
                .floors
                .regions
                .iter()
                .zip(&rings)
                .filter(|(r, _)| (r.level_m - level).abs() < 1e-9)
                .map(|(_, ring)| ring_area(ring))
                .sum::<f64>()
        };
        assert!((area_at_level(-0.3) - 8.0).abs() < 1e-6, "stepped half");
        assert!((area_at_level(0.0) - 8.0).abs() < 1e-6, "untouched half");
    }

    /// Empty `step_regions` performs NO geo ops: the floor ring's stored points
    /// are exactly the outline's points (round-trip / collinearity guard).
    #[test]
    fn empty_step_regions_round_trip_floor_points_exactly() {
        let pts = vec![
            Point2f32::new(0.1, 0.2),
            Point2f32::new(4.3, 0.2),
            Point2f32::new(4.3, 3.7),
            Point2f32::new(0.1, 3.7),
        ];
        let doc = square_doc(&pts);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        let storey = resolve_storey(&doc, &ber, 0, &[], &[]);

        let expected: Vec<Vec<Point2f64>> = doc
            .outline
            .floor_plan
            .polygons()
            .into_iter()
            .map(|ring| ring.into_iter().map(domain::points::to_f64).collect())
            .collect();
        assert_eq!(storey.floors.to_vertex_lists(), expected);
    }

    /// ensure_storey_constructions grows the storeys vec and registers the
    /// lookups resolve_storey expects; idempotent.
    #[test]
    fn ensure_constructions_is_idempotent_and_grows_storeys() {
        let mut ber = Ber::default();
        let regions = vec![PhaseRegion {
            polygon: vec![
                Point2f32::new(0.0, 0.0),
                Point2f32::new(1.0, 0.0),
                Point2f32::new(1.0, 1.0),
            ],
            phase: ConstructionPhase::Extension1,
        }];
        ensure_storey_constructions(&mut ber, 1, &regions);
        ensure_storey_constructions(&mut ber, 1, &regions);
        assert_eq!(ber.storeys.len(), 2);
        // Default floor + one per phase; default wall. Idempotent → no dupes.
        assert_eq!(ber.floor_constructions.len(), 2);
        assert_eq!(ber.wall_constructions.len(), 1);
        assert!(ber
            .floor_construction_id(
                Default::default(),
                Default::default(),
                ConstructionPhase::Extension1
            )
            .is_some());
    }
}
