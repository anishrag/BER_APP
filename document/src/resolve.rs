//! The resolve projection: editing model → resolved BER geometry. The inverse
//! of the load path (`load.rs`). PURE over its inputs — the shell owns *when*
//! to project (commit/materialize/persist) and the revision bookkeeping.
use crate::{CeilingStepRegion, PhaseRegion, StepRegion, StoreyDoc};
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::{ConstructionPhase, ItemId};
use datatypes::ber::structural::{
    CeilingRegions, FloorRegions, Polygon3D, Storey, WallExposure, WallRegions,
};
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
/// pieces' z, then the storey's durable `roof_apexes` FAN the stepped result
/// (sloped ceilings are an INPUT of the projection now, not a later feature)
/// — all of it only once the storey's ceiling height is set, since steps and
/// apexes refine a default that doesn't exist before that. The derived roofs
/// then shape the walls: each marked wall's top traces the roof profile.
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

    // The wall coverage segments over the FULL perimeter: one per coverage
    // range of each wall — painted sections plus `Unset` gaps, a whole-wall
    // `Unset` segment for an unmarked wall. EVERY wall becomes a region
    // (`Unset` included), so unmarked walls render and trace the roof like
    // marked ones; the unpainted state still survives reload because recovery
    // (`sections_from_wall_regions`) skips `Unset` regions instead of turning
    // them back into sections. Each is extruded below to the storey's ceiling
    // height, or `PROVISIONAL_WALL_EXTRUSION_M` until it's entered.
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
    // get their z shifted by the delta, areas compose in order), then FAN it
    // by the storey's durable roof apexes (`with_apexes`). Composition order
    // is law: default → steps → apexes — `with_z_deltas` must never see a
    // sloped region (its flat-input stamp would corrupt fan z). Re-deriving
    // every materialize is safe because the steps are reconstructed
    // APEX-AWARE from the persisted regions on reload
    // (`ceiling_step_regions_from_roofs` — the step/phase pattern), so the
    // refinement re-applies; an outline change re-derives AND re-clips the
    // steps to the new footprint (an apex the outline shrank away from goes
    // dormant — warn-skipped by `with_apexes`, never deleted). While the
    // height is unset, leave `roofs` untouched (3D is gated on the height
    // anyway) and IGNORE the steps and apexes — they're refinements of a
    // default that doesn't exist yet.
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
            // Apexes LAST: each fans the region containing it up (or down) to
            // its ABSOLUTE z. The apexes ride the cloned storey — durable
            // input, like the ceiling height itself. Like the composed-delta
            // sanity above, a near-floor apex is accepted but never silently.
            if !storey.roof_apexes.is_empty() {
                for apex in &storey.roof_apexes {
                    if apex.z_m <= 0.1 {
                        log::warn!(
                            "roof apex at ({:.3}, {:.3}) on storey {idx} has z = {:.3} m \
                             (≤ 0.1 m) — it all but eliminates the ceiling height there",
                            apex.point.x,
                            apex.point.y,
                            apex.z_m
                        );
                    }
                }
                roofs = roofs.with_apexes(&storey.roof_apexes);
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
    // Re-derive walls from the full-perimeter coverage (one region per
    // coverage range — `Unset` ranges included):
    // `WallRegions::regenerate` traces each wall's TOP along the derived roof
    // profile just set above — piecewise-linear over flat pieces and fan
    // triangles, a vertical jump where a ceiling step crosses the wall — and
    // carries identity over from the previous regions: a segment whose base
    // edge matches keeps its id + construction_id + opes (a roof change AMENDS
    // a marked wall), a covered split inherits the construction only. While
    // the ceiling height is unset there are no derived roofs (`storey.roofs`
    // may hold stale stored content — never feed it to the walls), so the
    // empty default keeps every top flat at the provisional extrusion; once
    // it's set, spans no roof region covers fall back flat at the storey's
    // default height. `fallback_top_z` is ABSOLUTE z.
    let no_roofs = CeilingRegions::default();
    let (roofs_for_walls, fallback_top_z) = match storey.ceiling_height_m {
        Some(h) => (&storey.roofs, h),
        None => (&no_roofs, PROVISIONAL_WALL_EXTRUSION_M),
    };
    // DEAP measures upper-storey wall area floor-to-floor: the perimeter
    // walls extend DOWN through the slab band, so their base drops by the
    // storey's floor thickness (storey-LOCAL coords — the datum is the
    // floor's top). Storey 0 and legacy records have no thickness (`None`)
    // → base_z = 0, byte-identical output. `fallback_top_z` stays the
    // ABSOLUTE ceiling height; the riser pass below is untouched (an
    // interior step riser spans between two slabs of the SAME storey, not
    // down to the storey below).
    // (Map None AND an explicit 0.0 to exactly +0.0 — negating a 0.0
    // default would stamp `-0.0` into every base vertex and break the
    // byte-identity of the legacy resolve under serialization.)
    let wall_base_z = match storey.floor_thickness_m {
        Some(t) if t > 0.0 => -t,
        _ => 0.0,
    };
    // The walls the storey held BEFORE this projection: the identity source
    // for BOTH passes — the perimeter regenerate and the riser append.
    let previous_walls = std::mem::take(&mut storey.walls);
    let walls = WallRegions::regenerate(
        wall_segments,
        &previous_walls,
        roofs_for_walls,
        wall_base_z,
        fallback_top_z,
        wall_cid,
    );
    // RISER regions append after the perimeter rebuild — derived geometry,
    // re-derived on every materialize like the walls themselves:
    //  - FLOOR risers: the vertical faces between adjacent floor regions
    //    whose `level_m` differ, derived whether or not the ceiling height
    //    is set (floor steps don't depend on it).
    //  - CEILING risers: the interior vertical faces between adjacent
    //    ceiling regions whose z profiles jump (a ceiling step's face;
    //    fan/ridge seams are continuous and emit nothing). Derived off
    //    `roofs_for_walls`, so they're empty until the height is set — no
    //    extra gating needed.
    // Perimeter walls never duplicate, by construction: outline edges border
    // ONE region, so `riser_faces` can't emit there — a ceiling step CROSSING
    // a wall shows up as that wall's vertical top jump, while the riser face
    // runs along the step line itself. Identity rides `with_risers`' amend
    // matching against the SAME previous set the perimeter pass consumed,
    // minus the ids that pass already kept.
    // Every FRESH riser defaults to `Unset`, like every other wall — these
    // are the derived DEFAULTS only: `with_risers`' amend PRESERVES a
    // matched previous region's exposure, so a riser the user set (selected
    // via the 2D dashed-line picker — see `StoreyDoc::riser_walls`) keeps
    // the choice across every re-derivation, and across reload (the first
    // materialize after load amends against the persisted set). The old v1
    // approximation (floor risers `Ground`, ceiling risers `Exposed`) is
    // gone: with no grade model a guessed default was wrong as often as
    // right, so ground contact is now the assessor's EXPLICIT choice (the
    // riser toolbar's Ground button; `Ground` stays in the enum as that
    // choice, never a default).
    let risers: Vec<(Polygon3D, WallExposure)> = storey
        .floors
        .riser_faces()
        .into_iter()
        .chain(roofs_for_walls.riser_faces())
        .map(|f| (f, WallExposure::Unset))
        .collect();
    storey.walls = walls.with_risers(risers, &previous_walls, wall_cid);
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
    /// SemiExposed projects to one floor region (exact ring round-trip), FOUR
    /// wall regions — the marked wall carrying its exposure plus one Unset
    /// region per unmarked wall (the full perimeter resolves) — and no living
    /// areas.
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

        // Walls: the FULL perimeter resolves — the marked wall carries its
        // exposure, the three unmarked walls come out Unset; every region
        // reaches the provisional extrusion (no ceiling height set yet).
        // Select by EXPOSURE, not index: region order is a derivation detail.
        assert_eq!(storey.walls.regions.len(), 4);
        let by_exposure =
            |e: WallExposure| storey.walls.regions.iter().filter(move |r| r.exposure == e);
        assert_eq!(by_exposure(WallExposure::SemiExposed).count(), 1);
        assert_eq!(by_exposure(WallExposure::Unset).count(), 3);
        assert!(storey.walls.regions.iter().all(|r| r
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - PROVISIONAL_WALL_EXTRUSION_M).abs() < 1e-9)));
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

    /// A floor step derives a riser wall (defaulting `Unset`, like every
    /// other wall) along its interior boundary — regardless of the ceiling
    /// height (unset here) — with the base edge at the LOWER level, ids
    /// unique against the perimeter regions, and the riser's id STABLE
    /// across re-resolves (amend).
    #[test]
    fn floor_step_derives_an_unset_riser_with_stable_identity() {
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

        // 4 perimeter (Unset) regions + ONE riser on the step line. The
        // riser defaults Unset too, so pick it out GEOMETRICALLY: its base
        // edge sits on the interior step line x = 2 (no perimeter wall does).
        assert_eq!(storey.walls.regions.len(), 5);
        let on_step_line = |r: &&datatypes::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 2.0).abs() < 1e-6 && (e[1].x - 2.0).abs() < 1e-6
        };
        let risers: Vec<_> = storey.walls.regions.iter().filter(on_step_line).collect();
        assert_eq!(risers.len(), 1, "one riser on the interior boundary");
        assert_eq!(
            risers[0].exposure,
            WallExposure::Unset,
            "a fresh riser defaults Unset like every other wall"
        );
        let ext = &risers[0].polygon.exterior;
        assert_eq!(ext.len(), 4, "a vertical quad");
        assert!(
            (ext[0].z + 0.3).abs() < 1e-9 && (ext[1].z + 0.3).abs() < 1e-9,
            "base edge at the LOWER level: {ext:?}"
        );
        assert!(ext[2].z.abs() < 1e-9 && ext[3].z.abs() < 1e-9);
        assert!(
            ext.iter().all(|v| (v.x - 2.0).abs() < 1e-6),
            "the face sits on the step line x = 2, never the outline: {ext:?}"
        );
        let mut ids: Vec<_> = storey.walls.regions.iter().map(|r| r.id).collect();
        ids.sort_by_key(|id| id.0);
        ids.dedup();
        assert_eq!(ids.len(), 5, "no duplicate ids");

        // Re-resolve with the derived storey as the previous state: the
        // riser AMENDS (deterministic re-derivation ⇒ stable identity).
        let riser_id = risers[0].id;
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let riser2 = storey2
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .expect("riser re-derives");
        assert_eq!(riser2.id, riser_id, "riser id stable across re-resolves");
    }

    /// THE RISER DURABILITY RULE at resolve level: an overridden riser
    /// exposure survives a plain re-resolve AND a re-resolve after a
    /// geometry-affecting edit (the ceiling height lands, re-extruding every
    /// wall) — the amend base-matches the unchanged step line and preserves
    /// the override instead of re-stamping the derived `Unset` default.
    #[test]
    fn riser_exposure_override_survives_re_resolves() {
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
        let mut storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        // The riser defaults Unset like the perimeter regions — find it by
        // its base edge on the interior step line x = 2.
        let riser = storey
            .walls
            .regions
            .iter_mut()
            .find(|r| {
                let e = &r.polygon.exterior;
                (e[0].x - 2.0).abs() < 1e-6 && (e[1].x - 2.0).abs() < 1e-6
            })
            .expect("the floor riser derives");
        assert_eq!(riser.exposure, WallExposure::Unset, "fresh → Unset");
        let riser_id = riser.id;
        riser.exposure = WallExposure::SemiExposed; // the user's override
        ber.storeys[0] = storey;

        // Plain re-resolve: the override survives the amend.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let r = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| r.id == riser_id)
            .expect("riser identity stable");
        assert_eq!(r.exposure, WallExposure::SemiExposed, "override survives");

        // Geometry-affecting edit: the height lands, walls re-extrude — the
        // riser's base edge is unchanged, so it still amends.
        ber.storeys[0].ceiling_height_m = Some(2.7);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let r = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| r.id == riser_id)
            .expect("riser identity stable across the height change");
        assert_eq!(
            r.exposure,
            WallExposure::SemiExposed,
            "override survives the re-extrusion re-derive"
        );
    }

    // ── Floor thickness in the perimeter walls (DEAP floor-to-floor) ───────

    /// LEGACY GUARD: a storey without a floor thickness (`None` — storey 0
    /// and every pre-thickness record) AND one with an explicit 0.0 resolve
    /// with the perimeter base at EXACTLY +0.0 — bit-for-bit, so the legacy
    /// projection stays byte-identical under serialization (`-0.0` would
    /// serialize differently).
    #[test]
    fn no_floor_thickness_keeps_exact_positive_zero_wall_bases() {
        for thickness in [None, Some(0.0)] {
            let doc = square_doc(&[
                Point2f32::new(0.0, 0.0),
                Point2f32::new(4.0, 0.0),
                Point2f32::new(4.0, 4.0),
                Point2f32::new(0.0, 4.0),
            ]);
            let mut ber = Ber::default();
            ensure_storey_constructions(&mut ber, 0, &[]);
            ber.set_default_ceiling_height(Some(0), 2.4);
            ber.storeys[0].floor_thickness_m = thickness;
            let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
            assert_eq!(storey.walls.regions.len(), 4);
            for r in &storey.walls.regions {
                for v in &r.polygon.exterior[..2] {
                    assert_eq!(
                        v.z.to_bits(),
                        0.0f64.to_bits(),
                        "{thickness:?}: base must be exactly +0.0, got {:?}",
                        v.z
                    );
                }
            }
        }
    }

    /// THE SLAB BAND (design §6): a 0.225 m floor thickness on storey 1
    /// drops every PERIMETER wall's base to z = −0.225 (storey-local — the
    /// datum is the floor's top) while the tops stay at the absolute ceiling
    /// height; an interior floor-step RISER face is untouched (it spans
    /// between two slabs of the SAME storey); and the thickness EDIT amends
    /// every wall — ids stable against the thickness-less resolve.
    #[test]
    fn floor_thickness_drops_perimeter_wall_bases_but_not_step_risers() {
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
        ensure_storey_constructions(&mut ber, 1, &[]);
        ber.set_default_ceiling_height(Some(1), 2.4);

        // Baseline WITHOUT the thickness: 4 perimeter walls + 1 riser at
        // base 0. (No perimeter wall shares the riser's plan edge here —
        // the rank-only |Δz| term in the amend matcher keeps a shared-edge
        // riser and perimeter wall apart; see `match_bases_to_previous`.)
        let storey0 = resolve_storey(&doc, &ber, 1, &[], &steps, &[]);
        assert_eq!(storey0.walls.regions.len(), 5);
        let mut ids0: Vec<_> = storey0.walls.regions.iter().map(|r| r.id).collect();
        ids0.sort_by_key(|id| id.0);

        // The thickness edit: every wall re-extrudes downward and AMENDS.
        ber.storeys[1] = storey0;
        ber.storeys[1].floor_thickness_m = Some(0.225);
        let storey = resolve_storey(&doc, &ber, 1, &[], &steps, &[]);
        assert_eq!(storey.walls.regions.len(), 5);
        let is_riser = |r: &datatypes::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 2.0).abs() < 1e-6 && (e[1].x - 2.0).abs() < 1e-6
        };
        for r in storey.walls.regions.iter().filter(|r| !is_riser(r)) {
            let e = &r.polygon.exterior;
            assert!(
                (e[0].z + 0.225).abs() < 1e-9 && (e[1].z + 0.225).abs() < 1e-9,
                "perimeter base drops by the thickness: {e:?}"
            );
            assert!(
                e.iter().any(|v| (v.z - 2.4).abs() < 1e-9),
                "top stays at the ABSOLUTE ceiling height: {e:?}"
            );
        }
        let riser = storey
            .walls
            .regions
            .iter()
            .find(|r| is_riser(r))
            .expect("the floor riser still derives");
        let rext = &riser.polygon.exterior;
        assert!(
            (rext[0].z + 0.3).abs() < 1e-9 && (rext[1].z + 0.3).abs() < 1e-9,
            "riser base UNCHANGED at the lower floor level: {rext:?}"
        );
        assert!(
            rext.iter().all(|v| v.z < 1e-9),
            "riser tops at the upper floor level, not the slab band: {rext:?}"
        );
        let mut ids: Vec<_> = storey.walls.regions.iter().map(|r| r.id).collect();
        ids.sort_by_key(|id| id.0);
        assert_eq!(ids, ids0, "the thickness edit AMENDS every wall");
    }

    /// RELOAD of a thickness-set record AMENDS every perimeter wall (the
    /// plan-only match gate — design risk #7): the first re-resolve against
    /// the persisted walls keeps id, construction AND openings, and the
    /// painted exposure re-applies — nothing is recreated.
    #[test]
    fn reload_of_a_thickness_set_record_amends_every_perimeter_wall() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_2d_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::SemiExposed);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 1, &[]);
        ber.set_default_ceiling_height(Some(1), 2.4);
        ber.storeys[1].floor_thickness_m = Some(0.225);

        let mut storey = resolve_storey(&doc, &ber, 1, &[], &[], &[]);
        assert_eq!(storey.walls.regions.len(), 4);
        let marked_idx = storey
            .walls
            .regions
            .iter()
            .position(|r| r.exposure == WallExposure::SemiExposed)
            .unwrap();
        let door_id = ItemId::default();
        storey.walls.regions[marked_idx].opes.doors.push(door_id);
        let (id0, cid0) = (
            storey.walls.regions[marked_idx].id,
            storey.walls.regions[marked_idx].construction_id,
        );
        let mut ids0: Vec<_> = storey.walls.regions.iter().map(|r| r.id).collect();
        ids0.sort_by_key(|id| id.0);

        // What reload does: rebuild the doc from points (same `doc` here)
        // and re-resolve against the PERSISTED region set.
        ber.storeys[1] = storey;
        let storey2 = resolve_storey(&doc, &ber, 1, &[], &[], &[]);
        let mut ids2: Vec<_> = storey2.walls.regions.iter().map(|r| r.id).collect();
        ids2.sort_by_key(|id| id.0);
        assert_eq!(ids2, ids0, "every perimeter wall AMENDS, none recreated");
        let wall = storey2
            .walls
            .regions
            .iter()
            .find(|r| r.id == id0)
            .expect("the marked wall keeps its identity");
        assert_eq!(wall.opes.doors, vec![door_id], "openings survive");
        assert_eq!(wall.construction_id, cid0, "construction survives");
        assert_eq!(
            wall.exposure,
            WallExposure::SemiExposed,
            "the painted exposure re-applies"
        );
        assert!(
            (wall.polygon.exterior[0].z + 0.225).abs() < 1e-9,
            "the slab band persists across the reload"
        );
    }

    /// `riser_walls` classifies geometrically: the floor-step storey yields
    /// exactly one riser (kind `Floor`, on the step line), no perimeter wall
    /// region is ever misclassified, and the classification is independent
    /// of the riser's (overridable) exposure.
    #[test]
    fn riser_walls_classify_floor_risers_and_skip_perimeter_walls() {
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
        let mut storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);

        let risers = doc.riser_walls(&storey);
        assert_eq!(risers.len(), 1, "exactly the step riser, no perimeter");
        let r = &risers[0];
        assert_eq!(r.kind, crate::RiserKind::Floor);
        assert_eq!(r.exposure, WallExposure::Unset, "default Unset");
        assert!(
            (r.a.x - 2.0).abs() < 1e-6 && (r.b.x - 2.0).abs() < 1e-6,
            "base edge on the step line x = 2"
        );

        // Override the exposure — classification doesn't change (geometric
        // discriminator, not exposure-based).
        storey
            .walls
            .regions
            .iter_mut()
            .find(|reg| reg.id == r.id)
            .unwrap()
            .exposure = WallExposure::Exposed;
        let risers = doc.riser_walls(&storey);
        assert_eq!(risers.len(), 1);
        assert_eq!(risers[0].kind, crate::RiserKind::Floor);
        assert_eq!(risers[0].exposure, WallExposure::Exposed);
    }

    /// REGRESSION (live report, 2026-06): a floor-step rect carved with its
    /// bottom line drawn ~2 cm above an outline edge it only PARTIALLY
    /// overlapped used to keep TWO bottom coordinates (the overlapped part
    /// pinned onto the edge, the interior remainder at its drawn y) — and the
    /// 2 cm jog derived a sliver riser wall. End-to-end over the session
    /// dump's exact numbers: carve → step region → resolve emits NO sliver
    /// riser (the carve aligns the whole drawn line onto the outline edge).
    #[test]
    fn step_carve_aligned_onto_an_outline_edge_derives_no_sliver_riser() {
        use manip_2d_plan::{SubareaOutcome, SubareaTuning};
        let p = Point2f32::new;
        let outline = [
            p(2.8664, 7.6222),
            p(5.8664, 7.6222),
            p(5.8664, 10.6222),
            p(8.8664, 10.6222),
            p(8.8664, 4.6222),
            p(6.8664, 4.6222),
            p(6.8664, 1.6222),
            p(2.8664, 1.6222),
        ];
        let doc = square_doc(&outline);
        // The drawn step rect: bottom at y = 4.6422, 2 cm above the outline
        // edge y = 4.6222 (whose span [6.8664, 8.8664] it only partly covers).
        let stroke = vec![
            p(5.8664, 10.6222),
            p(5.8664, 7.5),
            p(5.8664, 4.6422),
            p(7.0, 4.6422),
            p(8.8664, 4.6422),
            p(8.8664, 7.5),
            p(8.8664, 10.6222),
            p(7.0, 10.6222),
            p(5.9, 10.6222),
        ];
        let outcome = doc
            .outline
            .floor_plan
            .carve_or_cut(&stroke, &SubareaTuning::default(), false)
            .expect("the closed rect carves");
        let SubareaOutcome::Area(plan) = outcome else {
            panic!("a closed loop carves an area");
        };
        let steps = vec![crate::StepRegion {
            polygon: plan.get_points(),
            level_m: 0.3,
        }];
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        let storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        // The floor split carries the step level...
        assert!(storey
            .floors
            .regions
            .iter()
            .any(|r| (r.level_m - 0.3).abs() < 1e-9));
        // ...and no riser face is a 2 cm sliver: every wall region's base
        // edge is a real wall length.
        for r in &storey.walls.regions {
            let e = &r.polygon.exterior;
            let (a, b) = (e[0], e[1]);
            let len = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
            assert!(
                len > 0.05,
                "sliver wall region (base len {len:.4}) derived at {a:?} -> {b:?}"
            );
        }
        // The riser sits on the outline edge's y, never the drawn 4.6422.
        assert!(
            !storey
                .walls
                .regions
                .iter()
                .flat_map(|r| r.polygon.exterior.iter())
                .any(|v| (v.y - 4.6422).abs() < 1e-3),
            "the drawn (un-aligned) y = 4.6422 must not survive into any wall"
        );
    }

    /// A ceiling step's riser classifies as kind `Ceiling` (no matching
    /// floor riser face), defaulting `Unset` like every other wall.
    #[test]
    fn riser_walls_classify_ceiling_risers() {
        let (doc, ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
        let risers = doc.riser_walls(&storey);
        assert_eq!(risers.len(), 1, "one ceiling riser on the step line");
        assert_eq!(risers[0].kind, crate::RiserKind::Ceiling);
        assert_eq!(risers[0].exposure, WallExposure::Unset);
        assert_eq!(
            risers[0].kind.default_exposure(),
            WallExposure::Unset,
            "clear reverts a ceiling riser to Unset"
        );
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
        // Walls extrude to the set height, not the provisional placeholder —
        // the full perimeter (the Exposed mark + 3 Unset walls), so unmarked
        // walls reach the roof too.
        assert_eq!(storey.walls.regions.len(), 4);
        assert!(storey.walls.regions.iter().all(|r| r
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - 2.7).abs() < 1e-9)));
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
        assert!(storey.walls.regions.iter().all(|r| r
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - PROVISIONAL_WALL_EXTRUSION_M).abs() < 1e-9)));
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

    // ── Roof apexes (fans) + roof-aware walls ───────────────────────────────

    use datatypes::ber::structural::RoofApex;

    /// Multiset fingerprint of the roof SURFACE: per region `(plan area,
    /// min z, max z)`, sorted — robust to region order and to collinear
    /// vertices boolean ops insert.
    fn roof_surface_multiset(storey: &Storey) -> Vec<(f64, f64, f64)> {
        let mut v: Vec<(f64, f64, f64)> = storey
            .roofs
            .regions
            .iter()
            .map(|r| {
                let zs = r.polygon.exterior.iter().map(|p| p.z);
                (
                    roof_plan_area(r),
                    zs.clone().fold(f64::INFINITY, f64::min),
                    zs.fold(f64::NEG_INFINITY, f64::max),
                )
            })
            .collect();
        v.sort_by(|a, b| a.partial_cmp(b).unwrap());
        v
    }

    fn assert_same_surface(before: &[(f64, f64, f64)], after: &[(f64, f64, f64)]) {
        assert_eq!(before.len(), after.len(), "same region count");
        for ((ba, bl, bh), (aa, al, ah)) in before.iter().zip(after) {
            assert!(
                (ba - aa).abs() < 1e-4 && (bl - al).abs() < 1e-6 && (bh - ah).abs() < 1e-6,
                "region changed: before ({ba:.6}, {bl:.3}, {bh:.3}) vs after \
                 ({aa:.6}, {al:.3}, {ah:.3})"
            );
        }
    }

    /// THE APEX-OVER-STEP RELOAD FIXPOINT (the load-bearing invariant):
    /// resolve with a step + an apex over the stepped piece, reconstruct the
    /// session steps group-aware (the fan's group merges back into ONE step
    /// area at the step delta), re-resolve from the reconstruction → the
    /// identical surface.
    #[test]
    fn apex_over_step_reload_fixpoint() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        // Right half (x ≥ 2) stepped to 2.7, apex at (3, 2) fanning it to 4.2.
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        ber.storeys[0].roof_apexes = vec![RoofApex {
            point: Point2f64::new(3.0, 2.0),
            z_m: 4.2,
        }];
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);

        // Derived: the flat left half + one fan triangle per stepped-piece
        // edge, bases at 2.7, apex at 4.2.
        assert_eq!(ber.storeys[0].roofs.regions.len(), 5);
        let before = roof_surface_multiset(&ber.storeys[0]);
        assert!((before.iter().map(|r| r.0).sum::<f64>() - 16.0).abs() < 1e-4);

        // GROUP-AWARE reconstruction: the fan's group merges back into ONE
        // step area (the whole 2×4 stepped half) at delta 0.3.
        let rec1 = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(rec1.len(), 1, "one merged area, not the fan's triangles");
        assert!((rec1[0].delta_m - 0.3).abs() < 1e-9);
        assert!(
            (crate::step::ring_area2(&rec1[0].polygon) / 2.0 - 8.0).abs() < 1e-4,
            "the merged ring is the 2×4 half"
        );

        // Re-resolve from the reconstruction → identical surface (the apex
        // re-fans its own triangles to themselves — sliver-drop identity).
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &rec1);
        let after = roof_surface_multiset(&ber.storeys[0]);
        assert_same_surface(&before, &after);

        // And the reconstruction itself is stable across the round trip.
        let rec2 = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(rec2.len(), rec1.len());
        assert!(rec2.iter().all(|r| (r.delta_m - 0.3).abs() < 1e-9));
    }

    /// A fan in the DEFAULT (unstepped) region reconstructs to NO step areas
    /// (its base equals the default) and re-resolves to the same surface.
    #[test]
    fn fan_in_default_region_reload_fixpoint() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        ber.storeys[0].roof_apexes = vec![RoofApex {
            point: Point2f64::new(1.0, 1.0),
            z_m: 4.0,
        }];
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(ber.storeys[0].roofs.regions.len(), 4, "one fan per edge");
        let before = roof_surface_multiset(&ber.storeys[0]);

        let rec = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert!(rec.is_empty(), "fan over the default base is not a step");

        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &rec);
        let after = roof_surface_multiset(&ber.storeys[0]);
        assert_same_surface(&before, &after);
    }

    /// Total plan area per (min z, max z) signature, sorted — the "z → area
    /// multiset" view of the roof surface. Coarser than the per-region
    /// multiset: robust to how the surface is tiled into regions, which is
    /// exactly what the FIRST resolve→reconstruct→re-resolve round may
    /// change (the union can canonicalize ring vertices).
    fn roof_z_area_multiset(storey: &Storey) -> Vec<(f64, f64, f64)> {
        let mut agg: Vec<(f64, f64, f64)> = Vec::new();
        for (area, lo, hi) in roof_surface_multiset(storey) {
            match agg
                .iter_mut()
                .find(|(l, h, _)| (l - lo).abs() < 1e-6 && (h - hi).abs() < 1e-6)
            {
                Some((_, _, a)) => *a += area,
                None => agg.push((lo, hi, area)),
            }
        }
        agg.sort_by(|a, b| a.partial_cmp(b).unwrap());
        agg
    }

    fn assert_same_z_areas(before: &[(f64, f64, f64)], after: &[(f64, f64, f64)]) {
        assert_eq!(before.len(), after.len(), "same set of z signatures");
        for ((bl, bh, ba), (al, ah, aa)) in before.iter().zip(after) {
            assert!(
                (bl - al).abs() < 1e-6 && (bh - ah).abs() < 1e-6 && (ba - aa).abs() < 1e-4,
                "z→area changed: before ({bl:.3}, {bh:.3}, {ba:.6}) vs after \
                 ({al:.3}, {ah:.3}, {aa:.6})"
            );
        }
    }

    /// THE END-TO-END RIDGE FIXPOINT: a step + a RIDGE (two apexes) over the
    /// stepped area → resolve → reconstruct (ONE merged step area — the
    /// fragmentation wart is dead) → re-resolve → geometrically equal roofs
    /// (z → area multiset) → reconstruct + re-resolve once more → still
    /// equal (settled after the first canonicalize).
    #[test]
    fn ridge_over_step_reload_fixpoint() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        // Right half (x ≥ 2) stepped to 2.7; a ridge along x = 3 over it.
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        ber.add_roof_apex(
            0,
            RoofApex {
                point: Point2f64::new(3.0, 1.0),
                z_m: 4.2,
            },
        );
        ber.add_roof_apex(
            0,
            RoofApex {
                point: Point2f64::new(3.0, 3.0),
                z_m: 4.2,
            },
        );
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
        // The flat left half + the ridge's hub/transition triangles, with the
        // ridge tops at 4.2 and the eaves at the STEPPED base 2.7.
        let before = roof_z_area_multiset(&ber.storeys[0]);
        assert!(
            (before.iter().map(|r| r.2).sum::<f64>() - 16.0).abs() < 1e-4,
            "the surface tiles the footprint"
        );
        assert!(
            ber.storeys[0].roofs.regions.iter().any(|r| r
                .polygon
                .exterior
                .iter()
                .any(|v| (v.z - 4.2).abs() < 1e-9)),
            "the ridge rises"
        );
        // The coplanar merge gives the classic shape: the stepped half comes
        // out as 2 planar trapezoids (each holding BOTH ridge ends) + 2 end
        // triangles, plus the flat left half — 5 regions, not 9 triangles.
        assert_eq!(ber.storeys[0].roofs.regions.len(), 5);
        let sloped: Vec<_> = ber.storeys[0]
            .roofs
            .regions
            .iter()
            .filter(|r| r.flat_z().is_none())
            .collect();
        let mut counts: Vec<usize> = sloped.iter().map(|r| r.polygon.exterior.len()).collect();
        counts.sort();
        assert_eq!(counts, vec![3, 3, 4, 4], "2 trapezoids + 2 end triangles");
        for r in sloped.iter().filter(|r| r.polygon.exterior.len() == 4) {
            for ay in [1.0, 3.0] {
                assert!(
                    r.polygon.exterior.iter().any(|v| (v.x - 3.0).abs() < 1e-9
                        && (v.y - ay).abs() < 1e-9
                        && (v.z - 4.2).abs() < 1e-9),
                    "trapezoid holds the ridge end at y = {ay}"
                );
            }
        }

        // Reconstruct: ONE merged step area (the whole 2×4 stepped half).
        let rec1 = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(rec1.len(), 1, "one merged area under the ridge");
        assert!((rec1[0].delta_m - 0.3).abs() < 1e-9);
        assert!((crate::step::ring_area2(&rec1[0].polygon) / 2.0 - 8.0).abs() < 1e-4);

        // Re-resolve from the reconstruction → geometrically equal roofs.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &rec1);
        let after = roof_z_area_multiset(&ber.storeys[0]);
        assert_same_z_areas(&before, &after);
        // Round 1 is the SETTLED state (the f32 quantization has happened):
        // from here the spec's claim is the STRONG per-region invariant, not
        // just the z→area aggregate — a tiling oscillation must fail this.
        let settled = roof_surface_multiset(&ber.storeys[0]);

        // Once more: reconstruct again, re-resolve again → still equal.
        let rec2 = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(rec2.len(), 1);
        assert!((rec2[0].delta_m - 0.3).abs() < 1e-9);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &rec2);
        assert_same_z_areas(&before, &roof_z_area_multiset(&ber.storeys[0]));
        assert_same_surface(&settled, &roof_surface_multiset(&ber.storeys[0]));
    }

    /// While the ceiling height is unset there is no default ceiling to fan:
    /// apexes are IGNORED (roofs untouched, walls flat-provisional).
    #[test]
    fn apexes_ignored_while_height_unset() {
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
        ber.storeys[0].roof_apexes = vec![RoofApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.0,
        }];
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert!(storey.roofs.regions.is_empty(), "no height → no roofs");
        assert!(storey.walls.regions.iter().all(|r| r
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - PROVISIONAL_WALL_EXTRUSION_M).abs() < 1e-9)));
    }

    /// An apex the outline shrank away from goes DORMANT: warn-skipped by
    /// `with_apexes` (flat ceiling derives), but never deleted — it stays on
    /// the storey and re-fans if the outline grows back over it.
    #[test]
    fn apex_outside_the_outline_is_dormant_not_deleted() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        ber.storeys[0].roof_apexes = vec![RoofApex {
            point: Point2f64::new(5.0, 2.0), // outside the 4×4 footprint
            z_m: 4.0,
        }];
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey.roofs.regions.len(), 1, "flat — the fan is dormant");
        assert!((roof_area_at(&storey, 2.4) - 16.0).abs() < 1e-6);
        assert_eq!(storey.roof_apexes.len(), 1, "the apex is kept");
    }

    /// The user's amend scenario at resolve level: a marked wall under a NEW
    /// ceiling step keeps its id, construction_id, and openings across the
    /// re-resolve (same base edge → AMEND), while its top gains the step's
    /// vertical jump — and the step's interior RISER face derives as ONE
    /// `Unset` region that COEXISTS with that jump without double-counting
    /// (the jump is part of the perimeter wall's polygon; the riser is the
    /// separate face along the step line itself).
    #[test]
    fn wall_under_a_ceiling_step_keeps_identity_across_re_resolves() {
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
        ber.set_default_ceiling_height(Some(0), 2.4);

        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(ber.storeys[0].roofs.regions.len(), 1);
        // The full perimeter resolves (the marked wall + 3 Unset) — select
        // the marked wall by EXPOSURE, not index.
        assert_eq!(ber.storeys[0].walls.regions.len(), 4);
        let marked_idx = ber.storeys[0]
            .walls
            .regions
            .iter()
            .position(|r| r.exposure == WallExposure::Exposed)
            .unwrap();
        let wall0 = &ber.storeys[0].walls.regions[marked_idx];
        assert_eq!(wall0.polygon.exterior.len(), 4, "flat top: a quad");
        let (id0, cid0) = (wall0.id, wall0.construction_id);
        // Hang a door on the wall — an amend must not orphan it.
        let door_id = ItemId::default();
        ber.storeys[0].walls.regions[marked_idx]
            .opes
            .doors
            .push(door_id);

        // A step at x = 2 (+0.3) crosses every horizontal wall → re-resolve.
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &[], &[step.clone()]);

        // 4 perimeter regions + the step's ONE interior riser face.
        assert_eq!(storey2.walls.regions.len(), 5);
        // The riser: base edge ON the step line (x = 2, interior), spanning
        // the two ceiling heights, Unset (the riser default, like any wall).
        let is_riser = |r: &&datatypes::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 2.0).abs() < 1e-4 && (e[1].x - 2.0).abs() < 1e-4
        };
        let risers: Vec<_> = storey2.walls.regions.iter().filter(is_riser).collect();
        assert_eq!(risers.len(), 1, "exactly one riser — no double-counting");
        let riser = risers[0];
        assert_eq!(riser.exposure, WallExposure::Unset);
        let rext = &riser.polygon.exterior;
        assert!(
            (rext[0].z - 2.4).abs() < 1e-9 && (rext[1].z - 2.4).abs() < 1e-9,
            "riser base at the lower ceiling: {rext:?}"
        );
        assert!(
            rext.iter().any(|v| (v.z - 2.7).abs() < 1e-9),
            "riser tops at the stepped ceiling: {rext:?}"
        );
        // Every id unique across perimeter + riser.
        let mut ids: Vec<_> = storey2.walls.regions.iter().map(|r| r.id).collect();
        ids.sort_by_key(|id| id.0);
        ids.dedup();
        assert_eq!(ids.len(), 5, "no duplicate ids");

        let wall = storey2
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::Exposed && !is_riser(r))
            .unwrap();
        assert_eq!(wall.id, id0, "same base edge → same wall (amend)");
        assert_eq!(wall.construction_id, cid0);
        assert_eq!(wall.opes.doors, vec![door_id], "openings survive");
        // Vertex-order contract intact: ext[0]/ext[1] are the base edge…
        let ext = &wall.polygon.exterior;
        assert!((ext[0].z).abs() < 1e-9 && (ext[1].z).abs() < 1e-9);
        // …and the top now jumps at the step: both heights present, 6 verts.
        assert_eq!(ext.len(), 6, "vertical jump where the step crosses");
        for z in [2.4, 2.7] {
            assert!(
                ext.iter().any(|q| (q.z - z).abs() < 1e-9),
                "top reaches {z}"
            );
        }

        // Re-resolve once more from storey2: the riser AMENDS — its id is
        // stable across re-derivations.
        let riser_id = riser.id;
        let mut ber2 = ber.clone();
        ber2.storeys[0] = storey2.clone();
        let storey3 = resolve_storey(&doc, &ber2, 0, &[], &[], &[step]);
        let riser3 = storey3
            .walls
            .regions
            .iter()
            .find(|r| is_riser(r))
            .expect("riser re-derives");
        assert_eq!(riser3.id, riser_id, "riser id stable across re-resolves");
    }

    /// WALL-UNDER-RIDGE confidence: a marked wall keeps its identity when a
    /// ridge forms over the storey (same base edge → amend), its top traces
    /// the eaves (a strictly-interior ridge is a hip — perimeter walls stay
    /// at the base height), and `regenerate`'s plane sampler handles ridge
    /// triangles: a segment crossing UNDER the ridge line gets a genuinely
    /// sloped top rising to the ridge height.
    #[test]
    fn wall_under_a_ridge_keeps_identity_and_samples_ridge_planes() {
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
        ber.set_default_ceiling_height(Some(0), 2.4);

        // Flat resolve first — capture the marked wall's identity (the full
        // perimeter resolves; select by exposure, not index).
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let id0 = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::Exposed)
            .unwrap()
            .id;

        // A ridge along x = 2 (apexes (2,1) and (2,3) at 4.2) → re-resolve.
        for y in [1.0, 3.0] {
            ber.add_roof_apex(
                0,
                RoofApex {
                    point: Point2f64::new(2.0, y),
                    z_m: 4.2,
                },
            );
        }
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey.walls.regions.len(), 4);
        let wall = storey
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::Exposed)
            .unwrap();
        assert_eq!(wall.id, id0, "same base edge → same wall (amend)");
        // The marked wall is on the footprint boundary: every roof triangle
        // meets it along an eaves edge at the base height — flat top (hip).
        assert!(wall
            .polygon
            .exterior
            .iter()
            .all(|q| q.z.abs() < 1e-9 || (q.z - 2.4).abs() < 1e-9));

        // The plane-sampler confidence half: a segment along y = 2 crosses
        // under the ridge line — its top slopes up the hub/transition planes
        // (z = 2.4 + 0.9·x on the left), peaks at the ridge (x = 2, z = 4.2),
        // and slopes back down.
        use datatypes::ber::structural::WallRegions;
        let wall_cid = ber
            .wall_construction_id(Default::default(), Default::default(), Default::default())
            .unwrap();
        let walls = WallRegions::regenerate(
            vec![(
                Point2f64::new(0.0, 2.0),
                Point2f64::new(4.0, 2.0),
                WallExposure::Exposed,
            )],
            &WallRegions::default(),
            &storey.roofs,
            0.0,
            2.4,
            wall_cid,
        );
        let ext = &walls.regions[0].polygon.exterior;
        let top_at = |x: f64| {
            ext.iter()
                .filter(|q| (q.x - x).abs() < 1e-6 && q.z > 1e-9)
                .map(|q| q.z)
                .fold(f64::NEG_INFINITY, f64::max)
        };
        assert!((top_at(0.0) - 2.4).abs() < 1e-6, "eaves at the left end");
        assert!((top_at(4.0) - 2.4).abs() < 1e-6, "eaves at the right end");
        assert!(
            (top_at(2.0) - 4.2).abs() < 1e-6,
            "the top reaches the ridge height where it crosses the ridge"
        );
    }

    /// THE USER'S RAISED-AND-RIDGED-JUT SCENARIO: an L-shaped storey (main
    /// 4×4 + a 2×2 jut east at x ∈ [4,6], y ∈ [0,2]), the jut's ceiling
    /// raised 1 m by a ceiling step, then a ridge over the raised jut
    /// (apexes (4.5, 1) hip end and (6, 1) gable end, both at 4.4 m). The
    /// perimeter walls under the raised section must JOIN the roof: eaves at
    /// the RAISED base (3.4), the gable-end wall rising to the ridge, and
    /// the long south wall jumping 2.4 → 3.4 where the step crosses it.
    #[test]
    fn walls_under_a_raised_and_ridged_jut_join_the_roof() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(6.0, 0.0),
            Point2f32::new(6.0, 2.0),
            Point2f32::new(4.0, 2.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        // Mark EVERY perimeter wall exposed — the user painted the envelope.
        for (vid, a, b) in doc.outline.floor_plan.perimeter_walls() {
            let len = manip_2d_plan::segment_len(a, b);
            doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
        }
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        // Raise the jut: step at x = 4, Greater side, +1.0 → jut ceiling 3.4.
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                4.0,
                crate::LineSide::Greater,
                1.0,
            )
            .unwrap();
        // Ridge over the raised jut along y = 1: hip end inside, gable end
        // ON the east perimeter wall.
        for apex in [
            RoofApex {
                point: Point2f64::new(4.5, 1.0),
                z_m: 4.4,
            },
            RoofApex {
                point: Point2f64::new(6.0, 1.0),
                z_m: 4.4,
            },
        ] {
            ber.add_roof_apex(0, apex);
        }
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
        // One region per perimeter edge + the raised jut's interior riser
        // face along the step line x = 4 (the ridge's own seams are
        // continuous and emit nothing).
        assert_eq!(storey.walls.regions.len(), 7, "6 walls + 1 riser");
        let riser = storey
            .walls
            .regions
            .iter()
            .find(|r| {
                let e = &r.polygon.exterior;
                (e[0].x - 4.0).abs() < 1e-4
                    && (e[1].x - 4.0).abs() < 1e-4
                    && e[0].y.max(e[1].y) < 2.0 + 1e-4
            })
            .expect("the step line carries a riser face");
        assert_eq!(riser.exposure, WallExposure::Unset);
        assert!(
            (riser.polygon.exterior[0].z - 2.4).abs() < 1e-6,
            "riser base at the main ceiling: {:?}",
            riser.polygon.exterior
        );
        assert!(
            riser
                .polygon
                .exterior
                .iter()
                .any(|v| (v.z - 3.4).abs() < 1e-6),
            "riser tops at the raised eaves: {:?}",
            riser.polygon.exterior
        );

        // Find a wall region by its base edge (either orientation).
        let wall_with_base = |a: Point2f64, b: Point2f64| {
            storey
                .walls
                .regions
                .iter()
                .find(|r| {
                    let e = &r.polygon.exterior;
                    let close = |p: &domain::points::Point3f64, q: Point2f64| {
                        (p.x - q.x).abs() < 1e-6 && (p.y - q.y).abs() < 1e-6
                    };
                    close(&e[0], a) && close(&e[1], b) || close(&e[0], b) && close(&e[1], a)
                })
                .unwrap_or_else(|| panic!("no wall with base ({a:?}, {b:?})"))
        };
        // Top vertices (above the base) of a wall region.
        let tops = |r: &datatypes::ber::structural::WallRegion| -> Vec<domain::points::Point3f64> {
            r.polygon
                .exterior
                .iter()
                .filter(|v| v.z > 1e-9)
                .copied()
                .collect()
        };

        // East gable-end wall (6,0)→(6,2): eaves at 3.4 at both ends, rising
        // to the ridge (6, 1, 4.4) in between.
        let east = wall_with_base(Point2f64::new(6.0, 0.0), Point2f64::new(6.0, 2.0));
        let east_tops = tops(east);
        let z_at_y = |ts: &[domain::points::Point3f64], y: f64| {
            ts.iter()
                .filter(|v| (v.y - y).abs() < 1e-6)
                .map(|v| v.z)
                .fold(f64::NEG_INFINITY, f64::max)
        };
        assert!(
            (z_at_y(&east_tops, 0.0) - 3.4).abs() < 1e-6,
            "east wall eaves at the RAISED base at (6,0): {east_tops:?}"
        );
        assert!(
            (z_at_y(&east_tops, 2.0) - 3.4).abs() < 1e-6,
            "east wall eaves at the RAISED base at (6,2): {east_tops:?}"
        );
        assert!(
            (z_at_y(&east_tops, 1.0) - 4.4).abs() < 1e-6,
            "east wall rises to the ridge under the gable: {east_tops:?}"
        );

        // North jut wall (6,2)→(4,2): flat eaves along the raised base.
        let jut_north = wall_with_base(Point2f64::new(6.0, 2.0), Point2f64::new(4.0, 2.0));
        let jn_tops = tops(jut_north);
        assert!(
            jn_tops.iter().all(|v| (v.z - 3.4).abs() < 1e-6),
            "jut north wall tops out at the raised eaves: {jn_tops:?}"
        );

        // South wall (0,0)→(6,0): 2.4 over the main span, a vertical jump at
        // x = 4, then the RAISED eaves 3.4 over the jut span.
        let south = wall_with_base(Point2f64::new(0.0, 0.0), Point2f64::new(6.0, 0.0));
        let s_tops = tops(south);
        let z_at_x = |ts: &[domain::points::Point3f64], x: f64| {
            ts.iter()
                .filter(|v| (v.x - x).abs() < 1e-6)
                .map(|v| v.z)
                .fold(f64::NEG_INFINITY, f64::max)
        };
        assert!(
            (z_at_x(&s_tops, 0.0) - 2.4).abs() < 1e-6,
            "south wall at the default height on the main span: {s_tops:?}"
        );
        assert!(
            (z_at_x(&s_tops, 4.0) - 3.4).abs() < 1e-6,
            "south wall jumps to the raised eaves at the step: {s_tops:?}"
        );
        assert!(
            (z_at_x(&s_tops, 6.0) - 3.4).abs() < 1e-6,
            "south wall at the raised eaves over the jut: {s_tops:?}"
        );
        assert!(
            s_tops
                .iter()
                .all(|v| v.x > 4.0 - 1e-6 || (v.z - 2.4).abs() < 1e-6),
            "main-span top stays flat at 2.4: {s_tops:?}"
        );
        assert!(
            s_tops
                .iter()
                .all(|v| v.x < 4.0 + 1e-6 || (v.z - 3.4).abs() < 1e-6),
            "jut-span top stays flat at the raised eaves 3.4: {s_tops:?}"
        );

        // Main-only walls stay at the default height.
        let west = wall_with_base(Point2f64::new(0.0, 4.0), Point2f64::new(0.0, 0.0));
        assert!(
            tops(west).iter().all(|v| (v.z - 2.4).abs() < 1e-6),
            "west wall flat at the default height"
        );
    }

    /// The SAME scenario through the APP'S ACTUAL LOOP: non-integer (real
    /// sketch) coordinates, interior ridge apexes (the two-apex flow
    /// validates them strictly inside the stepped region), and the shell's
    /// canonicalize step after every assign (resolve → reconstruct the
    /// session ceiling-step list from the derived roofs → re-resolve). The
    /// jut's perimeter walls must still top out at the RAISED eaves.
    #[test]
    fn walls_join_the_raised_ridged_roof_through_the_canonicalize_loop() {
        let mut doc = square_doc(&[
            Point2f32::new(0.13, 0.27),
            Point2f32::new(6.41, 0.27),
            Point2f32::new(6.41, 2.33),
            Point2f32::new(4.22, 2.33),
            Point2f32::new(4.22, 4.51),
            Point2f32::new(0.13, 4.51),
        ]);
        for (vid, a, b) in doc.outline.floor_plan.perimeter_walls() {
            let len = manip_2d_plan::segment_len(a, b);
            doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
        }
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.43);

        // Step assign: raise the jut (x ≥ 4.22, snapped to the inner wall)
        // by 1 m, then canonicalize (materialize → reconstruct → re-resolve).
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                4.22,
                crate::LineSide::Greater,
                1.0,
            )
            .unwrap();
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
        let steps = crate::ceiling_step_regions_from_roofs(&ber, 0);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &steps);

        // Ridge assign: two apexes strictly inside the raised jut piece
        // (validate_roof_ridge guarantees this in the app), then the
        // canonicalize loop again.
        let raised_eaves = 2.43 + 1.0;
        for apex in [
            RoofApex {
                point: Point2f64::new(4.8, 1.3),
                z_m: 4.4,
            },
            RoofApex {
                point: Point2f64::new(5.9, 1.3),
                z_m: 4.4,
            },
        ] {
            ber.add_roof_apex(0, apex);
        }
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &steps);
        let steps2 = crate::ceiling_step_regions_from_roofs(&ber, 0);
        assert_eq!(steps2.len(), 1, "one merged step area under the ridge");
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &steps2);
        let storey = &ber.storeys[0];

        let wall_with_base = |a: Point2f64, b: Point2f64| {
            storey
                .walls
                .regions
                .iter()
                .find(|r| {
                    let e = &r.polygon.exterior;
                    let close = |p: &domain::points::Point3f64, q: Point2f64| {
                        (p.x - q.x).abs() < 1e-3 && (p.y - q.y).abs() < 1e-3
                    };
                    close(&e[0], a) && close(&e[1], b) || close(&e[0], b) && close(&e[1], a)
                })
                .unwrap_or_else(|| panic!("no wall with base ({a:?}, {b:?})"))
        };
        let tops = |r: &datatypes::ber::structural::WallRegion| -> Vec<domain::points::Point3f64> {
            r.polygon
                .exterior
                .iter()
                .filter(|v| v.z > 1e-9)
                .copied()
                .collect()
        };

        // The jut's own perimeter walls (east + jut-north): hip ends are
        // strictly inside, so the whole top is the RAISED eaves — flat 3.43.
        for (a, b, name) in [
            (
                Point2f64::new(6.41, 0.27),
                Point2f64::new(6.41, 2.33),
                "east",
            ),
            (
                Point2f64::new(6.41, 2.33),
                Point2f64::new(4.22, 2.33),
                "jut north",
            ),
        ] {
            let w = wall_with_base(a, b);
            let ts = tops(w);
            assert!(
                ts.iter().all(|v| (v.z - raised_eaves).abs() < 1e-3),
                "{name} wall must top out at the raised eaves {raised_eaves}: {ts:?}"
            );
        }

        // The long south wall: default height over the main span, raised
        // eaves over the jut span, one vertical jump at the step.
        let south = wall_with_base(Point2f64::new(0.13, 0.27), Point2f64::new(6.41, 0.27));
        let s_tops = tops(south);
        let max_z_where = |pred: &dyn Fn(&domain::points::Point3f64) -> bool| {
            s_tops
                .iter()
                .filter(|v| pred(v))
                .map(|v| v.z)
                .fold(f64::NEG_INFINITY, f64::max)
        };
        assert!(
            (max_z_where(&|v| v.x < 4.0) - 2.43).abs() < 1e-3,
            "south wall main span at the default height: {s_tops:?}"
        );
        assert!(
            (max_z_where(&|v| v.x > 4.5) - raised_eaves).abs() < 1e-3,
            "south wall jut span at the RAISED eaves: {s_tops:?}"
        );
    }

    /// THE REGRESSION GUARD for the raised+ridged-jut wall bug: 200
    /// pseudo-random L-footprints (messy real-sketch f32 coordinates)
    /// through the full step → canonicalize → ridge → canonicalize loop.
    /// Pre-fix, ~20% of trials left the jut's perimeter walls flat at the
    /// DEFAULT height: the boolean-op output (i_overlay grid snapping, plus
    /// the f32 round-trip of canonicalization) displaces the stepped roof
    /// piece's boundary a few 1e-7 m off the wall line, the exact
    /// boundary-inclusive containment pick in `covering_roof_plane` then
    /// misses, and `fallback_top_z` silently flattens the wall. Fixed by the
    /// metric-tolerance sampling in datatypes (`PROFILE_EPS_M`).
    #[test]
    fn stress_walls_join_raised_ridged_roof() {
        let mut seed: u64 = 0x9E3779B97F4A7C15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let mut failures = 0;
        for trial in 0..200 {
            // Messy f32 sketch-like coords.
            let x0 = (0.0 + next() * 0.5) as f32;
            let y0 = (0.0 + next() * 0.5) as f32;
            let xm = x0 + (3.5 + next()) as f32; // inner corner x
            let x1 = xm + (1.5 + next()) as f32; // east extent
            let y1 = y0 + (1.5 + next()) as f32; // jut north
            let y2 = y1 + (1.5 + next()) as f32; // main north
            let doc = {
                let mut doc = square_doc(&[
                    Point2f32::new(x0, y0),
                    Point2f32::new(x1, y0),
                    Point2f32::new(x1, y1),
                    Point2f32::new(xm, y1),
                    Point2f32::new(xm, y2),
                    Point2f32::new(x0, y2),
                ]);
                for (vid, a, b) in doc.outline.floor_plan.perimeter_walls() {
                    let len = manip_2d_plan::segment_len(a, b);
                    doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
                }
                doc
            };
            let h = 2.4;
            let mut ber = Ber::default();
            ensure_storey_constructions(&mut ber, 0, &[]);
            ber.set_default_ceiling_height(Some(0), h);
            let step = doc
                .ceiling_step_region(
                    manip_2d_plan::ParameterAxis::X,
                    xm,
                    crate::LineSide::Greater,
                    1.0,
                )
                .unwrap();
            ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
            let steps = crate::ceiling_step_regions_from_roofs(&ber, 0);
            ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &steps);
            // Ridge strictly inside the jut, along its mid-y.
            let my = (y0 as f64 + y1 as f64) / 2.0;
            for apex in [
                RoofApex {
                    point: Point2f64::new(xm as f64 + 0.4, my),
                    z_m: h + 2.0,
                },
                RoofApex {
                    point: Point2f64::new(x1 as f64 - 0.4, my),
                    z_m: h + 2.0,
                },
            ] {
                ber.add_roof_apex(0, apex);
            }
            ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &steps);
            let steps2 = crate::ceiling_step_regions_from_roofs(&ber, 0);
            ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &steps2);
            let storey = &ber.storeys[0];
            let raised = h + 1.0;
            // Jut perimeter walls: east (x1,y0)-(x1,y1) and north (x1,y1)-(xm,y1).
            for r in &storey.walls.regions {
                let e = &r.polygon.exterior;
                let (a, b) = (e[0], e[1]);
                let on_jut = (a.x - x1 as f64).abs() < 1e-6 && (b.x - x1 as f64).abs() < 1e-6
                    || (a.y - y1 as f64).abs() < 1e-6
                        && (b.y - y1 as f64).abs() < 1e-6
                        && a.x > xm as f64 - 1e-6
                        && b.x > xm as f64 - 1e-6;
                if !on_jut {
                    continue;
                }
                let bad: Vec<_> = e
                    .iter()
                    .filter(|v| v.z > 1e-9 && (v.z - raised).abs() > 1e-3)
                    .collect();
                if !bad.is_empty() {
                    failures += 1;
                    eprintln!(
                        "trial {trial}: wall base ({:.4},{:.4})-({:.4},{:.4}) tops wrong: {bad:?} \
                         [xm={xm}, x1={x1}, y0={y0}, y1={y1}]",
                        a.x, a.y, b.x, b.y
                    );
                }
            }
        }
        assert_eq!(failures, 0, "{failures} failing trials");
    }

    /// Exposure recovery round-trips SLOPED-TOP walls: the base edge stays at
    /// `exterior[0..2]` whatever the top does, so `sections_from_wall_regions`
    /// reads back exactly the painted sections — and the Unset regions (the
    /// gaps + the unmarked walls) recover to NO sections at all.
    #[test]
    fn sections_from_wall_regions_round_trips_sloped_top_walls() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_2d_plan::segment_len(a, b);
        // Paint the middle SemiExposed → ONE stored section; the rest of the
        // wall is Unset coverage (single representation).
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        // A step crossing the wall AND an apex fanning the stepped piece —
        // every section's top is now non-trivial (slopes and/or jumps).
        ber.storeys[0].roof_apexes = vec![RoofApex {
            point: Point2f64::new(3.0, 2.0),
            z_m: 4.2,
        }];
        let step = doc
            .ceiling_step_region(
                manip_2d_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);

        // One region per coverage range: gap/Semi/gap on the painted wall +
        // one whole-wall Unset region for each of the three unmarked walls +
        // the ceiling step's interior riser face (also Unset — the riser
        // default is Unset like every other wall).
        assert_eq!(storey.walls.regions.len(), 7);
        assert_eq!(
            storey
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure == WallExposure::Unset)
                .count(),
            6
        );
        assert!(
            storey
                .walls
                .regions
                .iter()
                .any(|r| r.polygon.exterior.len() > 4),
            "at least one top is genuinely non-flat"
        );
        let recovered = doc.sections_from_wall_regions(&storey.walls);
        assert_eq!(
            recovered.len(),
            1,
            "only the painted wall recovers — Unset regions and the riser \
             (an interior base edge on no perimeter wall) yield NO sections"
        );
        let secs = &recovered[&vid];
        let expected = &doc.wall_sections[&vid];
        assert_eq!(expected.len(), 1, "only the paint is stored");
        assert_eq!(secs.len(), expected.len());
        for (got, want) in secs.iter().zip(expected) {
            assert!((got.start_m - want.start_m).abs() < 1e-3);
            assert!((got.end_m - want.end_m).abs() < 1e-3);
            assert_eq!(got.exposure, want.exposure);
        }
    }
}
