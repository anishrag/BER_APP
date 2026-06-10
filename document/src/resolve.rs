//! The resolve projection: editing model → resolved BER geometry. The inverse
//! of the load path (`load.rs`). PURE over its inputs — the shell owns *when*
//! to project (commit/materialize/persist) and the revision bookkeeping.
use crate::{CeilingStepRegion, PhaseRegion, StepRegion, StoreyDoc};
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::{ConstructionPhase, ItemId};
use datatypes::ber::structural::{CeilingRegions, FloorRegions, Storey, WallRegions};
use domain::points::{to_f64, Point2f64};

/// Placeholder top for derived wall quads until the storey's ceiling height is
/// set — never visible in 3D (gated); walls re-extrude on the next materialize
/// after the height is entered.
pub(crate) const PROVISIONAL_WALL_EXTRUSION_M: f64 = 2.4;

/// The `&mut` prerequisites of the otherwise read-only [`resolve_storey`]:
/// lazy-create the storey slot if missing, and register (find-or-insert) every
/// construction the projection will look up — the default floor + wall, plus
/// the default ceiling, plus one floor per phase region on this storey.
/// Idempotent.
pub fn ensure_storey_constructions(ber: &mut Ber, idx: usize, phase_regions: &[PhaseRegion]) {
    while ber.storeys.len() <= idx {
        ber.storeys.push(Storey::default());
    }
    ber.ensure_floor_construction(Default::default(), Default::default(), Default::default());
    ber.ensure_wall_construction(Default::default(), Default::default(), Default::default());
    ber.ensure_ceiling_construction(Default::default(), Default::default(), Default::default());
    for region in phase_regions {
        ber.ensure_floor_construction(Default::default(), Default::default(), region.phase.clone());
    }
}

/// PURE projection of one storey from the editing model into a `Storey`. The
/// caller has already synced the active buffer and ensured the storey + its
/// constructions exist ([`ensure_storey_constructions`]), so this only LOOKS UP
/// construction ids and never mutates `ber` — letting it run for ANY storey,
/// not just the active one. `phase_regions`, `step_regions`, and
/// `ceiling_steps` are the storey's session areas: phases PARTITION the floor
/// by construction, then steps RE-SPLIT the resulting regions and stamp
/// `level_m` on the pieces (sequential composition — the phase partition is
/// untouched); ceiling steps RE-SPLIT the derived flat roofs and shift the
/// pieces' z (only once the storey's ceiling height is set — there's no
/// default to be relative to before that).
pub fn resolve_storey(
    s: &StoreyDoc,
    ber: &Ber,
    idx: usize,
    phase_regions: &[PhaseRegion],
    step_regions: &[StepRegion],
    ceiling_steps: &[CeilingStepRegion],
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
    // distinction survives reload). Each is extruded below to the storey's
    // ceiling height, or `PROVISIONAL_WALL_EXTRUSION_M` until it's entered.
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
            FloorRegions::from_vertex_lists(polygons_f64.clone(), floor_cid)
        } else {
            // Tagged → partition each outline ring into phase zones.
            let mut regions = Vec::new();
            for ring in &polygons_f64 {
                if ring.len() < 3 {
                    continue;
                }
                let part = FloorRegions::partition(ring, &areas_with_cids, floor_cid);
                regions.extend(part.regions);
            }
            FloorRegions { regions }
        };
    }
    // Roofs: once the storey's ceiling height is set, derive the default flat
    // ceiling — one region per outline ring, every vertex at z = height — then
    // re-split it by the session ceiling-step areas (`with_z_deltas`: pieces
    // get their z shifted by the delta, areas compose in order). Re-deriving
    // every materialize is safe because the steps are reconstructed from the
    // persisted regions on reload (`ceiling_step_regions_from_roofs` — the
    // step/phase pattern), so the refinement re-applies; an outline change
    // re-derives AND re-clips the steps to the new footprint. While the height
    // is unset, leave `roofs` untouched (3D is gated on the height anyway) and
    // IGNORE the steps — they're deltas against a default that doesn't exist
    // yet. Hand-refined SLOPED ceilings remain the later roofs feature.
    if let Some(height_m) = storey.ceiling_height_m {
        let ceiling_cid = ber
            .ceiling_construction_id(Default::default(), Default::default(), Default::default())
            .expect("base ceiling construction ensured before resolve");
        if polygons_f64.iter().any(|p| p.len() >= 3) {
            let mut roofs =
                CeilingRegions::default_for_footprint(polygons_f64, height_m, ceiling_cid);
            if !ceiling_steps.is_empty() {
                let areas: Vec<(Vec<Point2f64>, f64)> = ceiling_steps
                    .iter()
                    .map(|r| {
                        let ring: Vec<Point2f64> = r.polygon.iter().copied().map(to_f64).collect();
                        (ring, r.delta_m)
                    })
                    .collect();
                roofs = roofs.with_z_deltas(&areas);
                // Composed-delta sanity: a step is validated against the
                // default at assign time only, so stacked steps can drive a
                // piece's ceiling to (or through) the floor. Accepted — the
                // assessor may be mid-edit — but never silently.
                for region in &roofs.regions {
                    if let Some(z) = region.flat_z() {
                        if z <= 0.1 {
                            log::warn!(
                                "ceiling-step composition left a region at z = {z:.3} m \
                                 (≤ 0.1 m) on storey {idx} — stacked deltas have all but \
                                 eliminated the ceiling height there"
                            );
                        }
                    }
                }
            }
            storey.roofs = roofs;
        }
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
    storey.living_areas = FloorRegions::from_vertex_lists(living_area_lists, floor_cid);
    // Re-derive walls from exposure (empty when nothing is marked),
    // preserving the construction link of the region a split came from.
    // SEAM (deferred until after roofs): riser WALL regions will be derived HERE,
    // from adjacent floor regions with differing `level_m` — one wall region per
    // shared boundary segment, spanning the two levels; segments whose lower side
    // sits below grade (level_m < 0) get an exposed-to-ground exposure, plus
    // below-grade perimeter strips along the outline of lowered regions. The
    // shared-edge extraction belongs in datatypes (a FloorRegions method); the
    // derivation belongs here, alongside the rebuild it must merge with.
    let wall_top = storey
        .ceiling_height_m
        .unwrap_or(PROVISIONAL_WALL_EXTRUSION_M);
    storey.walls =
        WallRegions::rebuild_preserving(wall_segments, &storey.walls, 0.0, wall_top, wall_cid);
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
    /// SemiExposed projects to one floor region (exact ring round-trip), one
    /// wall region carrying the mark, no living areas.
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
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        // Floor: the whole outline verbatim.
        assert_eq!(storey.floors.regions.len(), 1);
        let ring = &storey.floors.to_vertex_lists()[0];
        assert_eq!(ring.len(), 4);
        assert!(ring
            .iter()
            .any(|p| (p.x - 4.0).abs() < 1e-9 && (p.y - 4.0).abs() < 1e-9));

        // Walls: only the marked wall becomes a region, carrying its exposure
        // and reaching the provisional extrusion (no ceiling height set yet).
        assert_eq!(storey.walls.regions.len(), 1);
        assert_eq!(storey.walls.regions[0].exposure, WallExposure::SemiExposed);
        assert!(storey.walls.regions[0]
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - PROVISIONAL_WALL_EXTRUSION_M).abs() < 1e-9));
        // No ceiling height set → roofs stay untouched (empty default).
        assert!(storey.roofs.regions.is_empty());

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
        let storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);

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
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

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
        // The default ceiling construction is ensured too (roofs derivation
        // looks it up read-only).
        assert_eq!(ber.ceiling_constructions.len(), 1);
        assert!(ber
            .ceiling_construction_id(Default::default(), Default::default(), Default::default())
            .is_some());
    }

    /// Ceiling height set → roofs derive to one flat region per outline ring
    /// (every vertex at z = height) and walls extrude to the same height.
    #[test]
    fn height_set_derives_roofs_and_extrudes_walls() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_2d_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);

        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.7);
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        assert_eq!(storey.roofs.regions.len(), 1, "one region per outline ring");
        let roof = &storey.roofs.regions[0];
        assert_eq!(roof.polygon.exterior.len(), 4);
        assert!(roof
            .polygon
            .exterior
            .iter()
            .all(|v| (v.z - 2.7).abs() < 1e-9));
        let ceiling_cid = ber
            .ceiling_construction_id(Default::default(), Default::default(), Default::default())
            .unwrap();
        assert_eq!(roof.construction_id, ceiling_cid);
        // Walls extrude to the set height, not the provisional placeholder.
        assert!(storey.walls.regions[0]
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - 2.7).abs() < 1e-9));
    }

    /// Ceiling height unset → roofs untouched (whatever the stored storey had)
    /// and walls extrude to the provisional placeholder.
    #[test]
    fn height_unset_leaves_roofs_untouched_and_walls_provisional() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_2d_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);

        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        // Pre-existing roof content on the stored storey survives untouched.
        let marker = datatypes::ber::structural::CeilingRegions::default_for_footprint(
            vec![vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(1.0, 0.0),
                Point2f64::new(1.0, 1.0),
            ]],
            9.9,
            Default::default(),
        );
        ber.storeys[0].roofs = marker.clone();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        assert_eq!(storey.roofs.regions.len(), 1);
        assert!((storey.roofs.regions[0].polygon.exterior[0].z - 9.9).abs() < 1e-9);
        assert!(storey.walls.regions[0]
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - PROVISIONAL_WALL_EXTRUSION_M).abs() < 1e-9));
    }

    /// Re-resolving after an outline change re-derives the roofs to the new
    /// footprint (derived content tracks the floor plan).
    #[test]
    fn re_resolve_after_outline_change_re_derives_roofs() {
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.5);

        let doc1 = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let storey1 = resolve_storey(&doc1, &ber, 0, &[], &[], &[]);
        ber.storeys[0] = storey1;

        // Outline grows to 6×4 → the derived roof follows.
        let doc2 = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(6.0, 0.0),
            Point2f32::new(6.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let storey2 = resolve_storey(&doc2, &ber, 0, &[], &[], &[]);
        assert_eq!(storey2.roofs.regions.len(), 1);
        let roof = &storey2.roofs.regions[0];
        assert!(roof
            .polygon
            .exterior
            .iter()
            .any(|v| (v.x - 6.0).abs() < 1e-6 && (v.y - 4.0).abs() < 1e-6));
        assert!(roof
            .polygon
            .exterior
            .iter()
            .all(|v| (v.z - 2.5).abs() < 1e-9));
    }

    // ── Ceiling-step derivation ─────────────────────────────────────────────

    /// Unsigned shoelace area of a roof region's plan projection (no geo
    /// dependency in this crate).
    fn roof_plan_area(region: &datatypes::ber::structural::CeilingRegion) -> f64 {
        let ring = &region.polygon.exterior;
        let n = ring.len();
        let mut s = 0.0;
        for i in 0..n {
            let (a, b) = (&ring[i], &ring[(i + 1) % n]);
            s += a.x * b.y - b.x * a.y;
        }
        s.abs() / 2.0
    }

    /// Total plan area of the roof regions whose flat height is `z`.
    fn roof_area_at(storey: &Storey, z: f64) -> f64 {
        storey
            .roofs
            .regions
            .iter()
            .filter(|r| r.flat_z().is_some_and(|rz| (rz - z).abs() < 1e-9))
            .map(roof_plan_area)
            .sum()
    }

    /// A 4×4 doc with the ceiling height set, ready for ceiling-step resolves.
    fn ceiling_test_setup(height: f64) -> (StoreyDoc, Ber) {
        let doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), height);
        (doc, ber)
    }

    /// One ceiling-step region (the line at x = 1, Greater side, +0.3) splits
    /// the derived roofs into the default piece at h and the stepped piece at
    /// h + Δ — and the overhanging half-rect is clipped to the footprint.
    #[test]
    fn one_ceiling_step_splits_roofs_into_default_and_stepped() {
        let (doc, ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                1.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);

        assert_eq!(storey.roofs.regions.len(), 2);
        assert!((roof_area_at(&storey, 2.4) - 4.0).abs() < 1e-6, "left 1×4");
        assert!(
            (roof_area_at(&storey, 2.7) - 12.0).abs() < 1e-6,
            "right 3×4"
        );
        // Clipped to the footprint: no vertex from the half-rect's overhang.
        for r in &storey.roofs.regions {
            for v in &r.polygon.exterior {
                assert!((-1e-6..=4.0 + 1e-6).contains(&v.x));
                assert!((-1e-6..=4.0 + 1e-6).contains(&v.y));
            }
        }
    }

    /// IDEMPOTENT RELOAD: derive → reconstruct the session areas from the
    /// derived roofs → re-derive is a FIXPOINT — the reconstruction and the
    /// region set are unchanged by the round trip (canonicalize-at-commit
    /// makes in-session == post-reload).
    #[test]
    fn ceiling_step_reconstruct_then_re_resolve_is_a_fixpoint() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::Y,
                3.0,
                crate::LineSide::Less,
                -0.2,
            )
            .unwrap();
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);

        // Reconstruct (what reload / canonicalize does) → exact pieces.
        let rec1 = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(rec1.len(), 1, "one off-default piece");
        assert!((rec1[0].delta_m - (-0.2)).abs() < 1e-9);

        // Re-derive from the reconstruction → identical region set.
        let before: Vec<(f64, f64)> = ber.storeys[0]
            .roofs
            .regions
            .iter()
            .map(|r| (r.flat_z().unwrap(), roof_plan_area(r)))
            .collect();
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &rec1);
        let mut after: Vec<(f64, f64)> = ber.storeys[0]
            .roofs
            .regions
            .iter()
            .map(|r| (r.flat_z().unwrap(), roof_plan_area(r)))
            .collect();
        let mut before = before;
        before.sort_by(|a, b| a.partial_cmp(b).unwrap());
        after.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert_eq!(before.len(), after.len());
        for ((bz, ba), (az, aa)) in before.iter().zip(&after) {
            assert!((bz - az).abs() < 1e-9 && (ba - aa).abs() < 1e-6);
        }
        // And the reconstruction itself is stable across the round trip.
        let rec2 = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(rec2.len(), 1);
        assert!((rec2[0].delta_m - (-0.2)).abs() < 1e-9);
        assert!(
            (crate::step::ring_area2(&rec2[0].polygon) - crate::step::ring_area2(&rec1[0].polygon))
                .abs()
                < 1e-6
        );
    }

    /// Ceiling steps are deltas against the storey default: while the height
    /// is unset there is no default, so they're IGNORED (roofs untouched).
    #[test]
    fn ceiling_steps_ignored_while_height_unset() {
        let doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
        assert!(
            storey.roofs.regions.is_empty(),
            "no height → roofs untouched, steps ignored"
        );
    }

    /// Two crossing steps COMPOSE: a vertical then a horizontal line produce
    /// four quadrants at h, h+a, h+b, h+a+b.
    #[test]
    fn two_crossing_ceiling_steps_compose_into_four_heights() {
        let h = 2.4;
        let (a, b) = (0.3, -0.2);
        let (doc, ber) = ceiling_test_setup(h);
        let right = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                a,
            )
            .unwrap();
        let top = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::Y,
                2.0,
                crate::LineSide::Greater,
                b,
            )
            .unwrap();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[right, top]);

        assert_eq!(storey.roofs.regions.len(), 4, "four quadrants");
        for z in [h, h + a, h + b, h + a + b] {
            assert!(
                (roof_area_at(&storey, z) - 4.0).abs() < 1e-6,
                "one 2×2 quadrant at z = {z}"
            );
        }
    }

    /// An outline change re-derives the roofs to the new footprint AND
    /// re-clips the session step areas against it: the grown band comes back
    /// at the default height (un-stepped), the step keeps its own extent.
    #[test]
    fn outline_change_re_derives_and_re_clips_ceiling_steps() {
        let (doc1, mut ber) = ceiling_test_setup(2.4);
        let step = doc1
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Less,
                0.3,
            )
            .unwrap();
        ber.storeys[0] = resolve_storey(&doc1, &ber, 0, &[], &[], &[step]);
        // Canonicalized session list — the exact stepped piece (x ∈ [0, 2]).
        let steps = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(steps.len(), 1);

        // Outline grows to 8×4 → re-resolve with the SAME session list.
        let doc2 = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(8.0, 0.0),
            Point2f32::new(8.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let storey = resolve_storey(&doc2, &ber, 0, &[], &[], &steps);
        // Stepped piece keeps its extent (2×4 = 8 m²); everything else —
        // including the grown band — is at the default.
        assert!((roof_area_at(&storey, 2.7) - 8.0).abs() < 1e-6, "step kept");
        assert!(
            (roof_area_at(&storey, 2.4) - 24.0).abs() < 1e-6,
            "remainder + grown band at the default"
        );
    }

    /// Stacked deltas that drive a piece's ceiling to z ≤ 0.1 m are ACCEPTED
    /// (warn-logged, not rejected) — single-delta validation happens at assign.
    #[test]
    fn composed_ceiling_z_at_or_below_floor_is_accepted() {
        let (doc, ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                -2.4,
            )
            .unwrap();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
        assert!(
            (roof_area_at(&storey, 0.0) - 8.0).abs() < 1e-6,
            "the z = 0 piece survives (warned, not dropped)"
        );
    }
}
