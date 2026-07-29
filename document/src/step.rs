//! Floor-step area logic: session-only areas that raise or lower part of the
//! storey floor at commit.
//!
//! A step region is NOT persisted as a region of its own — the durable record
//! is `FloorRegion.level_m` on the persisted floor regions it re-splits. The
//! shell owns the per-storey region *storage* (session state, deliberately
//! outside undo and the editing snapshot); the logic over the regions —
//! rebuilding them from persisted floors — lives here.
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::structural::Storey;
use domain::points::{to_f32, to_f64, Point2f32};

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
/// Degenerate (sub-mm²) regions are skipped — boolean-op debris from a record
/// written before `with_levels` filtered slivers, not real floors.
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
        let ring: Vec<Point2f32> = verts.iter().copied().map(to_f32).collect();
        if ring.len() >= 3 && ring_area2(&ring) >= 2e-6 {
            regions.push(StepRegion {
                polygon: ring,
                level_m: fr.level_m,
            });
        }
    }
    regions
}

/// The step regions a NEW upper storey INHERITS from the storey `below` it:
/// each flat below-ceiling piece standing proud of the anchor level under
/// `footprint` becomes one session step region (the floor above a raised
/// ceiling is itself raised — the slab thickness cancels, so the level is
/// just `z − anchor`). A thin f64↔f32 wrapper over the ber_model query
/// ([`CeilingRegions::inherited_floor_levels`]) so the shell seeds the same
/// canonical pieces a reload would reconstruct. Empty when nothing below
/// stands proud (single flat ceiling), the below storey has no derived
/// ceilings yet, or the footprint is degenerate.
///
/// [`CeilingRegions::inherited_floor_levels`]:
/// ber_model::ber::structural::CeilingRegions::inherited_floor_levels
pub fn inherited_step_regions(below: &Storey, footprint: &[Point2f32]) -> Vec<StepRegion> {
    inherited_step_regions_with(below, footprint, &[]).1
}

/// [`inherited_step_regions`] with the overhang answers supplied: a height per
/// [`CeilingRegions::inherited_floor_pieces`] piece, by index. `None`/absent
/// leaves a piece on its own ceiling, or an overhang on its longest-bordering
/// neighbour. Also returns the DATUM the levels were measured against, which
/// the caller must stamp on the storey (`Storey::floor_datum_z_m`) so
/// `Ber::storey_floor_datums` reads back the same one.
///
/// [`CeilingRegions::inherited_floor_pieces`]:
/// ber_model::ber::structural::CeilingRegions::inherited_floor_pieces
pub fn inherited_step_regions_with(
    below: &Storey,
    footprint: &[Point2f32],
    answers: &[Option<f64>],
) -> (Option<f64>, Vec<StepRegion>) {
    let fp: Vec<_> = footprint.iter().copied().map(to_f64).collect();
    let pieces = below.ceilings.inherited_floor_pieces(&fp);
    let (datum, levels) = ber_model::ber::structural::resolve_floor_levels(&pieces, answers);
    (
        datum,
        levels
            .into_iter()
            .map(|(ring, level_m)| StepRegion {
                polygon: ring.into_iter().map(to_f32).collect(),
                level_m,
            })
            .collect(),
    )
}

/// The floor pieces a storey above `footprint` would take from `below`, for
/// the seed's ONE question: which neighbour an overhang copies. Returns
/// `(piece index, area m², the levels it adjoins)` for every piece that needs
/// asking — empty when the levels follow from the ceilings alone.
pub fn overhang_choices(below: &Storey, footprint: &[Point2f32]) -> Vec<(usize, f64, Vec<f64>)> {
    let fp: Vec<_> = footprint.iter().copied().map(to_f64).collect();
    below
        .ceilings
        .inherited_floor_pieces(&fp)
        .into_iter()
        .enumerate()
        .filter(|(_, p)| p.needs_choice())
        .map(|(i, p)| (i, p.area_m2, p.bordering))
        .collect()
}

/// Per-storey junction-mismatch reasons, index-aligned with `ber.storeys`:
/// for every zone where storey `i`'s floor does not rest on storey `i − 1`'s
/// ceiling ([`junction_floor_ceiling_mismatches`] — the pure companion of the
/// inter-storey derive, gated on both sides being `dimensioned`), one reason
/// on the UPPER storey naming the neighbour, the gap and the zone size. A
/// storey's list is empty exactly when its junctions are aligned — the
/// checklist-row contract (`document::*_reasons`). The overlap case (floor
/// BELOW the ceiling it rests on) is the dangerous one: the junction derive
/// cannot bury the seam, mints degenerate wall slivers and churns settled
/// exposure back to Unset (field case "56 Elmwood Road").
///
/// [`junction_floor_ceiling_mismatches`]:
/// ber_model::ber::structural::junction_floor_ceiling_mismatches
pub fn junction_mismatch_reasons_by_storey(ber: &Ber, dimensioned: &[bool]) -> Vec<Vec<String>> {
    use ber_model::ber::structural::junction_floor_ceiling_mismatches;
    let mut per: Vec<Vec<String>> = vec![Vec::new(); ber.storeys.len()];
    for m in junction_floor_ceiling_mismatches(ber, dimensioned) {
        let below = storey_label(ber, m.below_idx);
        let msg = if m.gap_m < 0.0 {
            format!(
                "Floor sits {:.2} m below {below}'s ceiling over a {:.1} m² zone",
                -m.gap_m, m.area_m2
            )
        } else {
            format!(
                "Floor floats {:.2} m above {below}'s ceiling over a {:.1} m² zone",
                m.gap_m, m.area_m2
            )
        };
        if let Some(list) = per.get_mut(m.storey_idx) {
            list.push(msg);
        }
    }
    per
}

/// A storey's display label for reason text: its name, or "storey N" (1-based
/// `storey_number`) when unnamed.
fn storey_label(ber: &Ber, idx: usize) -> String {
    match ber.storeys.get(idx) {
        Some(s) if !s.name.is_empty() => s.name.clone(),
        Some(s) => format!("storey {}", s.storey_number + 1),
        None => format!("storey {}", idx + 1),
    }
}

/// Twice the unsigned shoelace area of `ring` (so the sliver threshold above
/// is 1e-6 m²). Local on purpose — `document` has no geo dependency.
/// `pub(crate)`: the ceiling-step reconstruction applies the same filter, and
/// the carve piece-pick (`lib.rs`) tie-breaks on it.
pub(crate) fn ring_area2(ring: &[Point2f32]) -> f64 {
    let n = ring.len();
    let mut s = 0.0_f64;
    for i in 0..n {
        let a = ring[i];
        let b = ring[(i + 1) % n];
        s += a.x as f64 * b.y as f64 - b.x as f64 * a.y as f64;
    }
    s.abs()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ensure_storey_constructions, resolve_storey, LineSide, ShapeSlot, StoreyDoc};
    use ber_model::ber::structural::FloorRegions;
    use ber_model::ber::suggest::suggest_storey_outlines;
    use domain::points::Point2f64;
    use manip_plan::{FloorPlan, ParameterAxis};

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
        let mut floors = FloorRegions::from_vertex_lists(vec![square(0.0)], cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![square(4.0)], cid).regions);
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

    /// The junction-mismatch reason names the neighbour storey, the gap and
    /// the zone size — the Elmwood wording — and lands on the UPPER storey.
    /// Aligned junctions (and the below storey) report nothing.
    #[test]
    fn junction_mismatch_reasons_name_gap_neighbour_and_zone() {
        use ber_model::ber::structural::{CeilingRegions, Storey};
        use domain::points::Point2f64;
        let cid = Default::default();
        let p = Point2f64::new;
        let footprint = vec![p(0.0, 0.0), p(8.0, 0.0), p(8.0, 4.0), p(0.0, 4.0)];
        let zone = vec![p(0.66, 0.9), p(3.04, 0.9), p(3.04, 4.0), p(0.66, 4.0)];
        let remainder = vec![
            p(0.0, 0.0),
            p(8.0, 0.0),
            p(8.0, 4.0),
            p(3.04, 4.0),
            p(3.04, 0.9),
            p(0.66, 0.9),
            p(0.66, 4.0),
            p(0.0, 4.0),
        ];
        let ground = Storey {
            floors: FloorRegions::new(footprint.clone(), cid),
            ceilings: CeilingRegions::default_for_footprint(vec![footprint], 2.548, cid)
                .with_z_deltas(&[(zone.clone(), -0.59)]),
            ceiling_height_m: Some(2.548),
            ..Storey::default()
        };
        let mut upper_floors = FloorRegions::from_vertex_lists(vec![zone, remainder], cid);
        // The main body (over the 2.548 ceiling, the larger area) is the datum
        // at 0; the low zone steps DOWN to −0.59.
        upper_floors.regions[0].level_m = -0.59;
        upper_floors.regions[1].level_m = 0.0;
        let upper = Storey {
            floors: upper_floors,
            ceiling_height_m: Some(2.4),
            floor_thickness_m: Some(0.2),
            ..Storey::default()
        };
        let mut ber = Ber::default();
        ber.storeys = vec![ground, upper];

        // Seeded correctly → every storey's list is empty.
        let per = junction_mismatch_reasons_by_storey(&ber, &[true, true]);
        assert!(per.iter().all(|l| l.is_empty()), "aligned: {per:?}");

        // The Elmwood hand-step: the low zone's floor driven 0.247 m past its
        // correct −0.59, so its underside sits below the ceiling it rests on.
        ber.storeys[1].floors.regions[0].level_m = -0.837;
        let per = junction_mismatch_reasons_by_storey(&ber, &[true, true]);
        assert!(per[0].is_empty(), "the below storey reports nothing");
        assert_eq!(
            per[1],
            vec!["Floor sits 0.25 m below storey 1's ceiling over a 7.4 m² zone".to_string()]
        );

        // A named neighbour is named; a void gap reads as floating.
        ber.storeys[0].name = "Ground Floor".into();
        ber.storeys[1].floors.regions[0].level_m = -0.29; // 0.3 above its correct −0.59
        let per = junction_mismatch_reasons_by_storey(&ber, &[true, true]);
        assert_eq!(
            per[1],
            vec!["Floor floats 0.30 m above Ground Floor's ceiling over a 7.4 m² zone".to_string()]
        );
    }

    /// A finalised square StoreyDoc with the given corner points as its outline.
    fn square_doc(pts: &[Point2f32]) -> StoreyDoc {
        let plan = FloorPlan::from_clean_ring(pts).unwrap();
        let mut doc = StoreyDoc::default();
        doc.dimensioning.scale_set = true;
        // Finalised = every active param known (resolve defers on an
        // outline the dimensioning loop would still prompt for).
        doc.dimensioning.known_params = plan.all_active_params();
        doc.floor_plan = plan.clone();
        doc.outline = ShapeSlot {
            floor_plan: plan,
            dimensioning: doc.dimensioning.clone(),
        };
        doc
    }

    /// `(level_m, plan area)` per floor region, sorted — the fingerprint the
    /// fixpoint assertions compare.
    fn floor_level_areas(storey: &ber_model::ber::structural::Storey) -> Vec<(f64, f64)> {
        let mut v: Vec<(f64, f64)> = storey
            .floors
            .regions
            .iter()
            .zip(storey.floors.to_vertex_lists())
            .map(|(r, ring)| {
                let ring32: Vec<Point2f32> = ring.iter().copied().map(to_f32).collect();
                (r.level_m, ring_area2(&ring32) / 2.0)
            })
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    }

    /// `inherited_step_regions` projects the below storey's proud flat
    /// ceiling pieces into upper-storey step regions at `z − anchor`, clipped
    /// to the footprint; a single flat ceiling (nothing proud) seeds nothing.
    #[test]
    fn inherited_step_regions_wrap_the_below_ceiling_levels() {
        let doc0 = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        // Flat ceiling → nothing stands proud → no inherited levels.
        ber.storeys[0] = resolve_storey(&doc0, &ber, 0, &[], &[], &[]);
        let footprint = doc0.outline.floor_plan.get_points();
        assert!(inherited_step_regions(&ber.storeys[0], &footprint).is_empty());

        // Step the right half (x ≥ 2) up by 0.3 → one inherited level there.
        let step = doc0
            .ceiling_step_region(ParameterAxis::X, 2.0, LineSide::Greater, 0.3)
            .unwrap();
        // Regions-as-truth: seed the flat default, then apply the ceiling step
        // as a one-shot onto the stored ceilings (resolve no longer replays it).
        ber.storeys[0] = resolve_storey(&doc0, &ber, 0, &[], &[], &[]);
        let areas = vec![(
            step.polygon
                .iter()
                .map(|p| domain::points::to_f64(*p))
                .collect::<Vec<_>>(),
            step.delta_m,
        )];
        ber.storeys[0].ceilings =
            std::mem::take(&mut ber.storeys[0].ceilings).with_z_deltas(&areas);
        let seeded = inherited_step_regions(&ber.storeys[0], &footprint);
        assert_eq!(seeded.len(), 1, "one proud piece");
        assert!((seeded[0].level_m - 0.3).abs() < 1e-6, "z − anchor");
        assert!((ring_area2(&seeded[0].polygon) / 2.0 - 8.0).abs() < 1e-4);
        assert!(
            seeded[0].polygon.iter().all(|p| p.x >= 2.0 - 1e-4),
            "the raised half only"
        );

        // Clipped to a footprint that only partly covers the proud piece.
        let half: Vec<Point2f32> = vec![
            Point2f32::new(0.0, 0.0),
            Point2f32::new(3.0, 0.0),
            Point2f32::new(3.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ];
        let clipped = inherited_step_regions(&ber.storeys[0], &half);
        assert_eq!(clipped.len(), 1);
        assert!((ring_area2(&clipped[0].polygon) / 2.0 - 4.0).abs() < 1e-4);
    }

    /// THE END-TO-END NEW-STOREY FIXPOINT (trap 1): below storey with a
    /// ceiling step → suggest → adopt the suggestion → seed the inherited
    /// levels → resolve → reconstruct the session steps from the persisted
    /// floors → re-resolve is IDENTICAL (level → area fingerprint), and the
    /// inherited step's riser keeps its id across the re-resolve.
    #[test]
    fn suggest_adopt_seed_resolve_reconstruct_is_a_fixpoint() {
        // Storey 0: 4×4, ceiling 2.4, right half (x ≥ 2) stepped to 2.7.
        let doc0 = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        let step = doc0
            .ceiling_step_region(ParameterAxis::X, 2.0, LineSide::Greater, 0.3)
            .unwrap();
        // Regions-as-truth: seed flat, then apply the ceiling step one-shot.
        ber.storeys[0] = resolve_storey(&doc0, &ber, 0, &[], &[], &[]);
        let areas = vec![(
            step.polygon
                .iter()
                .map(|p| domain::points::to_f64(*p))
                .collect::<Vec<_>>(),
            step.delta_m,
        )];
        ber.storeys[0].ceilings =
            std::mem::take(&mut ber.storeys[0].ceilings).with_z_deltas(&areas);

        // SUGGEST: take a two-level candidate (it spans both ceiling pieces).
        let suggestions = suggest_storey_outlines(&ber.storeys[0], &[], 6);
        let chosen = suggestions
            .iter()
            .find(|s| s.levels == 2)
            .expect("a suggestion spanning both levels");
        let ring32: Vec<Point2f32> = chosen
            .ring
            .iter()
            .copied()
            .map(domain::points::to_f32)
            .collect();

        // ADOPT + SEED — the exact ber_model pieces (canonical).
        let mut doc1 = StoreyDoc::default();
        doc1.adopt_finalised_outline(&ring32).expect("adopts");
        let seeded = inherited_step_regions(&ber.storeys[0], &ring32);
        assert_eq!(seeded.len(), 1, "the raised half seeds one step");
        assert!((seeded[0].level_m - 0.3).abs() < 1e-6);

        // RESOLVE storey 1 with the seeded steps.
        ensure_storey_constructions(&mut ber, 1, &[]);
        ber.storeys[1] = resolve_storey(&doc1, &ber, 1, &[], &seeded, &[]);
        let before = floor_level_areas(&ber.storeys[1]);
        assert_eq!(before.len(), 2, "floor split at the inherited line");
        assert!(before.iter().any(|(l, _)| (l - 0.3).abs() < 1e-9));
        // The inherited step derives a riser on the step line x = 2.
        let on_step_line = |r: &&ber_model::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 2.0).abs() < 1e-6 && (e[1].x - 2.0).abs() < 1e-6
        };
        let _riser_id = ber.storeys[1]
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .expect("riser derives on the inherited step line")
            .id;

        // RECONSTRUCT (what a reload does) → the same canonical pieces.
        let rec = step_regions_from_floors(&ber, 1);
        assert_eq!(rec.len(), 1, "one reconstructed step region");
        assert!((rec[0].level_m - 0.3).abs() < 1e-9);
        assert!(
            (ring_area2(&rec[0].polygon) - ring_area2(&seeded[0].polygon)).abs() < 1e-4,
            "reconstruction matches the seed"
        );

        // RE-RESOLVE from the reconstruction → identical floors, stable riser.
        ber.storeys[1] = resolve_storey(&doc1, &ber, 1, &[], &rec, &[]);
        let after = floor_level_areas(&ber.storeys[1]);
        assert_eq!(before.len(), after.len());
        for ((bl, ba), (al, aa)) in before.iter().zip(&after) {
            assert!(
                (bl - al).abs() < 1e-9 && (ba - aa).abs() < 1e-4,
                "floor fingerprint changed: ({bl}, {ba}) vs ({al}, {aa})"
            );
        }
        ber.storeys[1]
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .expect("riser re-derives");

        // And the reconstruction itself is stable across the round trip.
        let rec2 = step_regions_from_floors(&ber, 1);
        assert_eq!(rec2.len(), 1);
        assert!((ring_area2(&rec2[0].polygon) - ring_area2(&rec[0].polygon)).abs() < 1e-4);
    }
}
