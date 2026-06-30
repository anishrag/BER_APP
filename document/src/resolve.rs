//! The resolve projection: editing model → resolved BER geometry. The inverse
//! of the load path (`load.rs`). PURE over its inputs — the shell owns *when*
//! to project (commit/materialize/persist) and the revision bookkeeping.
use crate::{CeilingStepRegion, PhaseRegion, StepRegion, StoreyDoc};
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::{ConstructionPhase, ItemId};
use ber_model::ber::structural::{
    split_wall_by_line, CeilingRegions, FloorRegions, Polygon3D, Storey, WallExposure, WallRegions,
};
use domain::points::{to_f64, Point2f32, Point2f64, Point3f64};

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
/// untouched); ceiling steps RE-SPLIT the derived flat ceilings and shift the
/// pieces' z, then the storey's durable `ceiling_apexes` FAN the stepped result
/// (sloped ceilings are an INPUT of the projection now, not a later feature)
/// — all of it only once the storey's ceiling height is set, since steps and
/// apexes refine a default that doesn't exist before that. The derived ceilings
/// then shape the walls: each marked wall's top traces the roof profile.
pub fn resolve_storey(
    s: &StoreyDoc,
    ber: &Ber,
    idx: usize,
    phase_regions: &[PhaseRegion],
    step_regions: &[StepRegion],
    // Regions-as-truth: ceiling steps are applied one-shot at edit time, not
    // replayed here. Param retained until the controller call sites are rewired
    // (then removed in cleanup); resolve no longer reads it.
    _ceiling_steps: &[CeilingStepRegion],
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
    // Clone the existing storey (keeps id/name/number/ceilings/openings) and
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
    // Ceilings: once the storey's ceiling height is set, derive the default flat
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
    // height is unset, leave `ceilings` untouched (3D is gated on the height
    // anyway) and IGNORE the steps and apexes — they're refinements of a
    // default that doesn't exist yet.
    if let Some(height_m) = storey.ceiling_height_m {
        let ceiling_cid = ber
            .ceiling_construction_id(Default::default(), Default::default(), Default::default())
            .expect("base ceiling construction ensured before resolve");
        if polygons_f64.iter().any(|p| p.len() >= 3) {
            // REGIONS-AS-TRUTH: the stored `ceilings` ARE the ceiling model, not a
            // cache re-derived from declarative inputs. Resolve only:
            //   - SEEDS a flat default ceiling when there are none yet (the
            //     height was just set), or
            //   - RE-FITS the existing regions to the current outline so an
            //     outline edit re-clips / extends them (idempotent when the
            //     footprint is unchanged — see the resolve-fixpoint test).
            // Ceiling steps, roof apexes and structure face-splits are applied
            // ONCE at edit time as one-shot mutations on `storey.ceilings`
            // (controller), never replayed here — so there is no reconstruction
            // and no reload fixpoint to maintain.
            // Re-fit only on a REAL outline change. An unchanged footprint keeps
            // the stored ceilings verbatim — a re-fit's boolean clip + plane-eval
            // would perturb a flat region's z by ~1e-15 and break exact reload
            // fixpoints. "Unchanged" = equal total plan area AND bounding box.
            fn outline_unchanged(ceilings: &CeilingRegions, footprint: &[Vec<Point2f64>]) -> bool {
                let shoelace = |xy: &[(f64, f64)]| -> f64 {
                    let n = xy.len();
                    if n < 3 {
                        return 0.0;
                    }
                    let mut s = 0.0;
                    for i in 0..n {
                        let (a, b) = (xy[i], xy[(i + 1) % n]);
                        s += a.0 * b.1 - b.0 * a.1;
                    }
                    s.abs() / 2.0
                };
                let fp: Vec<Vec<(f64, f64)>> = footprint
                    .iter()
                    .map(|r| r.iter().map(|p| (p.x, p.y)).collect())
                    .collect();
                let rf: Vec<Vec<(f64, f64)>> = ceilings
                    .regions
                    .iter()
                    .map(|r| r.polygon.exterior.iter().map(|v| (v.x, v.y)).collect())
                    .collect();
                let area = |ps: &[Vec<(f64, f64)>]| -> f64 { ps.iter().map(|r| shoelace(r)).sum() };
                if (area(&fp) - area(&rf)).abs() > 1e-6 {
                    return false;
                }
                let bbox = |ps: &[Vec<(f64, f64)>]| {
                    let (mut lx, mut ly, mut hx, mut hy) = (
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    );
                    for r in ps {
                        for &(x, y) in r {
                            lx = lx.min(x);
                            ly = ly.min(y);
                            hx = hx.max(x);
                            hy = hy.max(y);
                        }
                    }
                    (lx, ly, hx, hy)
                };
                let (a, b, c, d) = bbox(&fp);
                let (e, f, g, h) = bbox(&rf);
                (a - e).abs() < 1e-6
                    && (b - f).abs() < 1e-6
                    && (c - g).abs() < 1e-6
                    && (d - h).abs() < 1e-6
            }
            storey.ceilings = if storey.ceilings.regions.is_empty() {
                CeilingRegions::default_for_footprint(polygons_f64.clone(), height_m, ceiling_cid)
            } else if outline_unchanged(&storey.ceilings, &polygons_f64) {
                std::mem::take(&mut storey.ceilings)
            } else {
                std::mem::take(&mut storey.ceilings).refit_to_footprint(
                    &polygons_f64,
                    height_m,
                    ceiling_cid,
                )
            };
            // CARVE the ceilings by the phase areas — the ceiling analogue of the
            // floor partition above. A surface-preserving coplanar split (the
            // same primitive the structure roof-carve uses): a roof face the
            // boundary straddles cuts in two, the z-field is bit-identical, only
            // the region count grows — so each piece lands fully within ONE
            // floor phase zone and `stamp_ceiling_phases` (post-resolve) can tag
            // it. Replayed every resolve (a fixpoint: an already-carved face is
            // intact), so it survives `recompose_ceilings` overwrites the way the
            // floor partition survives — phases are session-driven, not durable
            // roof inputs. Skipped when untagged, keeping the no-phase roof
            // byte-identical (no geo ops).
            if !phase_area_rings.is_empty() {
                let rings: Vec<Vec<Point2f64>> = phase_area_rings
                    .iter()
                    .map(|(ring, _)| ring.clone())
                    .collect();
                storey.ceilings = std::mem::take(&mut storey.ceilings).split_faces_coplanar(&rings);
            }
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
    // Structure-phase FLOOR splits: divide the (phase/step-partitioned) floor
    // regions by the durable cut rings — both pieces keep construction + level,
    // only the id is fresh. After the partition so it composes on top; before
    // the walls read `storey.floors` below. Gated for a byte-identical legacy.
    if !storey.floor_splits.is_empty() {
        storey.floors = std::mem::take(&mut storey.floors).with_splits(&storey.floor_splits);
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
    // the ceiling height is unset there are no derived ceilings (`storey.ceilings`
    // may hold stale stored content — never feed it to the walls), so the
    // empty default keeps every top flat at the provisional extrusion; once
    // it's set, spans no roof region covers fall back flat at the storey's
    // default height. `fallback_top_z` is ABSOLUTE z.
    let no_roofs = CeilingRegions::default();
    let (roofs_for_walls, fallback_top_z) = match storey.ceiling_height_m {
        Some(h) => (&storey.ceilings, h),
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
    // The pre-projection walls — kept ONLY to seed opening anchors for openings
    // that don't have one yet (their placement still references a pre-projection
    // wall id). Not used for identity recovery (the amend is retired).
    let previous_walls = std::mem::take(&mut storey.walls);
    // The wall BASE follows the floor steps symmetric to how its TOP follows
    // the roof: a perimeter wall spanning a raised/lowered floor region starts
    // its base at that region's `level_m` (composed onto the slab-band
    // `wall_base_z`), with a vertical jump at a step line crossing the wall —
    // no phantom flat plane under a raised strip, and DEAP wall area no longer
    // over-counts the step. `storey.floors` is final here (the `with_levels`
    // step above ran). A flat base collapses to the byte-identical 2-vertex
    // `[a, b]`.
    let walls = WallRegions::regenerate(
        wall_segments,
        roofs_for_walls,
        &storey.floors,
        wall_base_z,
        fallback_top_z,
        wall_cid,
    );
    // WALL splits: divide any wall a cut segment crosses into two sub-walls
    // (each copies the parent's exposure + construction, fresh id). Openings
    // re-home onto the correct sub-wall from their parametric `wall_anchor` at
    // the resolve tail. Two sources, applied together:
    //  - durable structure-phase `wall_splits` (the assessor's per-wall carves);
    //  - construction-PHASE boundaries — each phase area's INTERIOR edges (the
    //    zone dividers, via `interior_cut_edges`: edges NOT running along the
    //    outline), so a perimeter wall a phase boundary CROSSES splits and each
    //    sub-wall takes its own zone's phase (`stamp_wall_phases`, post-resolve).
    //    A boundary running ALONG a wall (a party wall) is collinear, so it
    //    crosses nothing and stays one wall — `stamp_wall_phases` then gives it
    //    the OLDER zone's phase. The ceiling analogue of the floor partition +
    //    roof carve; session-driven, recomputed every resolve like those.
    let mut wall_cuts: Vec<[Point2f64; 2]> = storey.wall_splits.clone();
    for region in phase_regions {
        wall_cuts.extend(s.outline.floor_plan.interior_cut_edges(&region.polygon));
    }
    let walls = walls.apply_splits(&wall_cuts);
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
    // runs along the step line itself. Every riser is FRESH each resolve and
    // defaults to `Unset`; the user's choice (via the 2D dashed-line picker —
    // see `StoreyDoc::riser_walls`) is restored from the durable
    // `riser_exposure` table by `stamp_riser_exposure` below. The old v1
    // approximation (floor risers `Ground`, ceiling risers `Exposed`) is
    // gone: with no grade model a guessed default was wrong as often as
    // right, so ground contact is now the assessor's EXPLICIT choice (the
    // riser toolbar's Ground button; `Ground` stays in the enum as that
    // choice, never a default).
    // A riser that comes out a COPLANAR-FLUSH continuation of another wall
    // region (same vertical plane, sharing a positive-length edge — e.g. a
    // step line continuing a perimeter wall's plane) does not survive as a
    // separate region: `with_risers` merges it into that wall
    // (`merge_coplanar_flush` — host keeps id/base/exposure/opes), so one
    // physical wall resolves as ONE region and `riser_walls` only ever
    // lists the risers that remain separate.
    let risers: Vec<(Polygon3D, WallExposure)> = storey
        .floors
        .riser_faces()
        .into_iter()
        .chain(roofs_for_walls.riser_faces())
        .map(|f| (f, WallExposure::Unset))
        .collect();
    storey.walls = walls.with_risers(risers, wall_cid);
    // WALL FACE PARTITIONS: a perimeter wall the assessor split along arbitrary
    // (diagonal) in-plane lines resolves to one region per face partition, each
    // carrying its own painted exposure. Runs HERE — after `with_risers` (the
    // perimeter set is final + risers appended) and BEFORE the stamp passes /
    // openings tail (which see the now-final walls). Mirrors the floor/roof
    // carve-label: session-driven, recomputed every resolve.
    apply_face_partitions(s, &mut storey);
    // Stamp the partition RECIPE onto Layer 1 (durable, geometric key) so it
    // survives a snapshot-less / version-stale load — it is NOT invertible from
    // the split walls above, so the recipe itself must be stored.
    stamp_wall_face_partitions(s, &mut storey);
    // Stamp riser exposure from the durable side-table (keyed by base edge) —
    // risers have no perimeter VertexId, so this is their durable identity.
    stamp_riser_exposure(s, &mut storey);
    // Stamp floor/ceiling horizontal exposure from the durable centroid-keyed
    // side-tables — the area analogue of the riser stamp. Floors are final
    // (phase partition + steps + splits all ran), ceilings are final (the phase
    // carve ran), so each region's centroid keys its override.
    stamp_floor_exposure(s, &mut storey);
    stamp_ceiling_exposure(s, &mut storey);
    // OPENINGS tail: walls/ceilings are now final. Every wall id is freshly minted
    // (no amend), so re-derive each wall opening's `wall_id`/`along_m` from its
    // durable parametric `wall_anchor` (picking the right sub-wall after a
    // split), seeding the anchor — for openings that lack one — from the
    // pre-projection walls (whose ids still match the stored placement). Then
    // recompute each placed opening's `vertices` and drop any whose host the
    // edit removed.
    reanchor_openings(s, &mut storey, &previous_walls);
    ber_model::ber::openings::derive_opening_vertices(&mut storey);
    ber_model::ber::openings::prune_orphaned_openings(&mut storey);
    // Each wall/roof region's `opes` is a DERIVED index of the openings on it,
    // rebuilt from placements (ceiling ids regenerate, so it can't be carried).
    ber_model::ber::openings::rebuild_opes(&mut storey);
    storey
}

/// Rebuild `storey.walls.regions`, splitting every face-partitioned perimeter
/// wall into one [`WallRegion`] per drawn face region and attributing each its
/// painted exposure. Mirrors [`stamp_riser_exposure`]'s shape (read `s`, mutate
/// `storey`); runs after `with_risers` so the perimeter + risers are final.
///
/// A wall is matched to its perimeter edge by the SAME base-midpoint-on-segment
/// test the exposure recovery uses ([`StoreyDoc::sections_from_wall_regions`]):
/// the region's `base_frame` midpoint within 1e-2 m of a perimeter segment. A
/// region that matches no perimeter wall, or whose wall is not partitioned, is
/// kept unchanged (risers, plain perimeter walls, phase-split sub-walls of an
/// un-partitioned wall). A partitioned wall that enters as N ≥ 1 regions (e.g. a
/// phase boundary also crossed it) is processed region-by-region.
///
/// Each [`PartitionCut`](crate::PartitionCut) is wall-LOCAL `(along_m, height_m)`
/// from the wall's start `a` with z measured from the FLOOR (z = 0), so the two
/// cut endpoints lift to 3-D on the wall plane via the perimeter direction. The
/// cut is an INFINITE line (matching the 2-D `subdivide_face` model), so it
/// divides the whole running set of sub-polygons. Exposure is attributed
/// base_z-AGNOSTICALLY (the wall may extend below the floor on upper storeys):
/// each sub-poly vertex and each stored sample project to wall-local `(along, z)`
/// and the containing sample's exposure wins (default [`WallExposure::Unset`]).
fn apply_face_partitions(s: &StoreyDoc, storey: &mut Storey) {
    if s.wall_face_partitions.is_empty() {
        return;
    }
    const TOL_M: f64 = 1e-2;
    let perim = s.outline.floor_plan.perimeter_walls();
    let mut out: Vec<ber_model::ber::structural::WallRegion> = Vec::new();
    for region in std::mem::take(&mut storey.walls.regions) {
        // The perimeter wall this region's base lies on (midpoint-on-segment).
        let matched = region.base_frame().and_then(|(origin, (ux, uy), wlen, _)| {
            let mid = Point2f64::new(origin.x + ux * wlen * 0.5, origin.y + uy * wlen * 0.5);
            perim.iter().copied().find(|(_, a, b)| {
                domain::points::point_to_segment_distance(
                    mid.x, mid.y, a.x as f64, a.y as f64, b.x as f64, b.y as f64,
                ) < TOL_M
            })
        });
        let Some((vid, a, b)) = matched.filter(|(vid, _, _)| s.is_wall_partitioned(*vid)) else {
            out.push(region); // riser / plain / un-partitioned wall — unchanged
            continue;
        };
        out.extend(split_partitioned_wall(s, &region, vid, a, b));
    }
    storey.walls.regions = out;
}

/// Stamp the durable Layer-1 partition recipe onto `storey` from the editing
/// working copy `s.wall_face_partitions`, re-keying each entry from the
/// parametric `VertexId` to the wall's geometric [`WallFaceKey`] (base-edge
/// endpoints from the outline plan). The inverse of
/// [`StoreyDoc::wall_face_partitions_from_storey`]; recomputed every resolve so
/// it tracks the current outline geometry. An entry whose `VertexId` no longer
/// resolves to a wall (stale) is dropped.
fn stamp_wall_face_partitions(s: &StoreyDoc, storey: &mut Storey) {
    let plan = &s.outline.floor_plan;
    storey.wall_face_partitions = s
        .wall_face_partitions
        .iter()
        .filter_map(|(vid, part)| {
            let (a, b) = plan.wall_endpoints(*vid)?;
            Some((crate::wall_face_key(a, b), part.clone()))
        })
        .collect();
}

/// Split one partitioned perimeter `region` (matched to perimeter edge `a`→`b`,
/// wall `vid`) into one [`WallRegion`] per face region with its painted
/// exposure. The split + exposure-attribution body of [`apply_face_partitions`].
fn split_partitioned_wall(
    s: &StoreyDoc,
    region: &ber_model::ber::structural::WallRegion,
    vid: manip_plan::VertexId,
    a: Point2f32,
    b: Point2f32,
) -> Vec<ber_model::ber::structural::WallRegion> {
    let part = &s.wall_face_partitions[&vid];
    // Plan direction a→b (the partition's along-axis), in f64.
    let (a64, b64) = (
        Point2f64::new(a.x as f64, a.y as f64),
        Point2f64::new(b.x as f64, b.y as f64),
    );
    let (dx, dy) = (b64.x - a64.x, b64.y - a64.y);
    let plen = (dx * dx + dy * dy).sqrt();
    if plen < 1e-9 {
        return vec![region.clone()];
    }
    let (dirx, diry) = (dx / plen, dy / plen);
    // Iteratively cut the running sub-poly set by each cut's infinite line.
    let mut subs: Vec<Polygon3D> = vec![region.polygon.clone()];
    for cut in &part.cuts {
        // Lift each wall-local cut endpoint (along, height) onto the wall plane,
        // z = height from the floor (z = 0), NOT base_z.
        let p0 = Point3f64::new(
            a64.x + cut.a.x as f64 * dirx,
            a64.y + cut.a.x as f64 * diry,
            cut.a.y as f64,
        );
        let p1 = Point3f64::new(
            a64.x + cut.b.x as f64 * dirx,
            a64.y + cut.b.x as f64 * diry,
            cut.b.y as f64,
        );
        let mut next: Vec<Polygon3D> = Vec::new();
        for poly in &subs {
            // Reuse the wall-minting split to keep both halves' rings (≥ 3 v),
            // then take just the polygons back (exposure is attributed below).
            let tmp = ber_model::ber::structural::WallRegion {
                id: ItemId::default(),
                polygon: poly.clone(),
                exposure: region.exposure,
                opes: Default::default(),
                construction_id: region.construction_id,
            };
            next.extend(
                split_wall_by_line(&tmp, p0, p1)
                    .into_iter()
                    .map(|w| w.polygon),
            );
        }
        subs = next;
    }
    // Attribute exposure per sub-poly base_z-agnostically: project sub-poly
    // vertices to wall-local (along, height) and test each stored sample.
    subs.into_iter()
        .map(|poly| {
            let projected: Vec<Point2f32> = poly
                .exterior
                .iter()
                .map(|v| {
                    let along = (v.x - a64.x) * dirx + (v.y - a64.y) * diry;
                    Point2f32::new(along as f32, v.z as f32)
                })
                .collect();
            let exposure = part
                .regions
                .iter()
                .find(|(sample, _)| {
                    manip_plan::point_in_polygon(&projected, Point2f32::new(sample.x, sample.y))
                })
                .map(|(_, e)| *e)
                .unwrap_or(WallExposure::Unset);
            ber_model::ber::structural::WallRegion {
                id: ItemId::default(),
                polygon: poly,
                exposure,
                opes: Default::default(),
                construction_id: region.construction_id,
            }
        })
        .collect()
}

/// Stamp riser exposure overrides from the durable `s.riser_exposure` table onto
/// the just-built riser walls (keyed by quantized base edge). A riser is a wall
/// whose base midpoint is NOT on the outline perimeter (perimeter exposure comes
/// from `wall_sections`). An entry overrides the derived/amended exposure; an
/// absent key leaves it untouched (so un-seeded risers keep working off the
/// amend until Stage 2c retires it). Runs after `with_risers`.
fn stamp_riser_exposure(s: &StoreyDoc, storey: &mut Storey) {
    if s.riser_exposure.is_empty() {
        return;
    }
    const TOL_M: f32 = 0.05;
    let plan = &s.outline.floor_plan;
    for region in &mut storey.walls.regions {
        let Some((e0, e1)) = region.base_edge() else {
            continue;
        };
        let mid = Point2f32::new(((e0.x + e1.x) * 0.5) as f32, ((e0.y + e1.y) * 0.5) as f32);
        if plan.surface_ref_at(mid, TOL_M).is_some() {
            continue; // perimeter wall — exposure comes from wall_sections
        }
        let key = crate::riser_key(Point2f64::new(e0.x, e0.y), Point2f64::new(e1.x, e1.y));
        if let Some(e) = s.riser_exposure.get(&key) {
            region.exposure = *e;
        }
    }
}

/// Stamp floor exposure overrides from the durable `s.floor_exposure` table onto
/// the just-built FLOOR regions (keyed by quantized plan centroid). An entry
/// overrides the resolved region's `exposure`; an absent key leaves it at the
/// `Unset` default. The area analogue of [`stamp_riser_exposure`]; runs after
/// the floor partition / steps / splits are final.
fn stamp_floor_exposure(s: &StoreyDoc, storey: &mut Storey) {
    if s.floor_exposure.is_empty() {
        return;
    }
    for region in &mut storey.floors.regions {
        let key = crate::centroid_key(region.centroid());
        // Exact centroid key first; on a miss (region reshaped/subdivided after
        // attribution), fall back to the override whose stored centroid is inside
        // this region's polygon.
        if let Some(e) = s.floor_exposure.get(&key).or_else(|| {
            crate::phase::centroid_override_by_containment(&s.floor_exposure, &region.polygon)
        }) {
            region.exposure = *e;
        }
    }
}

/// Stamp ceiling exposure overrides from the durable `s.ceiling_exposure` table
/// onto the just-built CEILING/ROOF regions (keyed by quantized plan centroid).
/// Mirror of [`stamp_floor_exposure`]; runs after the phase roof-carve is final.
fn stamp_ceiling_exposure(s: &StoreyDoc, storey: &mut Storey) {
    if s.ceiling_exposure.is_empty() {
        return;
    }
    for region in &mut storey.ceilings.regions {
        let key = crate::centroid_key(region.centroid());
        // Exact centroid key first; on a miss (the roof was recomposed/re-carved
        // on load — e.g. by the phase-roof heal — drifting its centroid off the
        // seeded key) fall back to the override whose stored centroid is inside
        // this region's PLAN polygon (its 3-D exterior projected to xy).
        if let Some(e) = s.ceiling_exposure.get(&key).or_else(|| {
            let ring: Vec<(f64, f64)> =
                region.polygon.exterior.iter().map(|v| (v.x, v.y)).collect();
            let plan = geo::Polygon::new(geo::LineString::from(ring), vec![]);
            crate::phase::centroid_override_by_containment(&s.ceiling_exposure, &plan)
        }) {
            region.exposure = *e;
        }
    }
}

/// Re-apply the floor + ceiling horizontal-exposure overrides from `doc`'s
/// durable centroid-keyed maps onto an ALREADY-RESOLVED `storey` — the public
/// face of the two private stamp passes that run inside [`resolve_storey`]. The
/// controller's auto-derive pre-pass fills the doc maps AFTER every storey
/// resolves (it needs the neighbours' resolved footprints), then calls this so
/// the just-derived values land on the resolved regions in the SAME frame,
/// across non-active/lower storeys too — without a full re-resolve. Idempotent:
/// only regions whose centroid is keyed in the map change; the rest stay put.
pub fn stamp_horizontal_exposure(doc: &StoreyDoc, storey: &mut Storey) {
    stamp_floor_exposure(doc, storey);
    stamp_ceiling_exposure(doc, storey);
}

/// Keep each wall opening homed across edits via its durable parametric anchor.
/// For a wall opening whose `wall_id` is still live, SEED `wall_anchor` if unset
/// (the host wall's base midpoint maps back to a perimeter `VertexId`). For one
/// whose `wall_id` went stale (outline reshape), RE-POINT it onto the wall now
/// lying along the anchor's perimeter edge, clamping `along_m` to that wall's
/// length. A wall that's both split and reshaped is ambiguous (the perimeter
/// edge carries two sub-walls) and is left for the orphan-prune. Wall splits on
/// their own keep `wall_id` valid via `rehome`, so this no-ops there.
fn reanchor_openings(s: &StoreyDoc, storey: &mut Storey, previous: &WallRegions) {
    use ber_model::ber::common::ItemId;
    use ber_model::ber::fenestration::{OpeningPlacement, WallAnchor};

    const TOL_M: f64 = 0.05;
    let plan = &s.outline.floor_plan;
    let walls = &storey.walls;

    // The live wall region whose base CONTAINS world point `p`, plus the centre
    // distance along it — picks the correct sub-wall after a structure split.
    let host_at = |p: Point2f64| -> Option<(ItemId, f32)> {
        let mut best: Option<(f64, ItemId, f32)> = None;
        for r in &walls.regions {
            let Some((origin, (ux, uy), wlen, _)) = r.base_frame() else {
                continue;
            };
            let t = (p.x - origin.x) * ux + (p.y - origin.y) * uy;
            if t < -TOL_M || t > wlen + TOL_M {
                continue;
            }
            let (px, py) = (origin.x + ux * t, origin.y + uy * t);
            let d = ((p.x - px).powi(2) + (p.y - py).powi(2)).sqrt();
            if d <= TOL_M && best.is_none_or(|(bd, _, _)| d < bd) {
                best = Some((d, r.id, t.clamp(0.0, wlen) as f32));
            }
        }
        best.map(|(_, id, along)| (id, along))
    };

    // (anchor-to-set, (wall_id, along_m)-to-set) for one opening.
    let compute = |placement: &Option<OpeningPlacement>,
                   anchor: Option<WallAnchor>|
     -> (Option<WallAnchor>, Option<(ItemId, f32)>) {
        let Some(OpeningPlacement::Wall {
            wall_id, along_m, ..
        }) = placement
        else {
            return (None, None); // roof opening / unplaced — nothing to anchor
        };
        // Seed from the pre-projection wall when this opening has no anchor yet
        // (its placement still references that wall's id — legacy / just-placed).
        let seeded = if anchor.is_none() {
            previous
                .regions
                .iter()
                .find(|r| r.id == *wall_id)
                .and_then(|r| opening_wall_anchor(plan, r, *along_m))
        } else {
            None
        };
        // Re-derive the wall_id/along_m cache from the (existing or seeded)
        // anchor: the centre at the parent-relative fraction, then the sub-wall
        // containing it. Handles fresh ids, reshape, and split sub-walls.
        let repoint = anchor.or(seeded).and_then(|a| {
            a.wall
                .wall_start()
                .and_then(|st| plan.wall_point_at_fraction(st, a.along_fraction))
                .and_then(|c| host_at(Point2f64::new(c.x as f64, c.y as f64)))
        });
        (seeded, repoint)
    };

    let win: Vec<_> = storey
        .windows
        .iter()
        .map(|w| compute(&w.placement, w.wall_anchor))
        .collect();
    let drs: Vec<_> = storey
        .doors
        .iter()
        .map(|d| compute(&d.placement, d.wall_anchor))
        .collect();

    let apply = |placement: &mut Option<OpeningPlacement>,
                 anchor: &mut Option<WallAnchor>,
                 up: (Option<WallAnchor>, Option<(ItemId, f32)>)| {
        if let Some(a) = up.0 {
            *anchor = Some(a);
        }
        if let Some((id, along)) = up.1 {
            if let Some(OpeningPlacement::Wall {
                wall_id, along_m, ..
            }) = placement
            {
                *wall_id = id;
                *along_m = along;
            }
        }
    };
    for (w, up) in storey.windows.iter_mut().zip(win) {
        apply(&mut w.placement, &mut w.wall_anchor, up);
    }
    for (d, up) in storey.doors.iter_mut().zip(drs) {
        apply(&mut d.placement, &mut d.wall_anchor, up);
    }
}

/// The durable [`WallAnchor`] for an opening centred at `along_m` on the live
/// wall region `wall`: the parent perimeter wall (by `VertexId`) + the centre's
/// parent-relative along-fraction. `None` if the centre isn't on the outline
/// perimeter (e.g. a riser) or the plan can't place it. Shared by the resolve
/// tail and the controller (which seeds after each opening op), so both produce
/// identical anchors.
pub fn opening_wall_anchor(
    plan: &manip_plan::FloorPlan,
    wall: &ber_model::ber::structural::WallRegion,
    along_m: f32,
) -> Option<ber_model::ber::fenestration::WallAnchor> {
    use ber_model::ber::fenestration::WallAnchor;
    const TOL_M: f32 = 0.05;
    let (origin, (ux, uy), _wlen, _bz) = wall.base_frame()?;
    let centre = Point2f32::new(
        (origin.x + ux * along_m as f64) as f32,
        (origin.y + uy * along_m as f64) as f32,
    );
    let sref = plan.surface_ref_at(centre, TOL_M)?;
    let start = sref.wall_start()?;
    let frac = plan.wall_along_fraction(start, centre)?;
    Some(WallAnchor {
        wall: sref,
        along_fraction: frac,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShapeSlot;
    use ber_model::ber::structural::WallExposure;
    use domain::points::Point2f32;
    use manip_plan::FloorPlan;

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
        let len = manip_plan::segment_len(a, b);
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
        // No ceiling height set → ceilings stay untouched (empty default).
        assert!(storey.ceilings.regions.is_empty());

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

    /// Containment fallback: a floor exposure set BEFORE the assessor reshapes the
    /// outline survives even though the region's centroid drifts off the stored
    /// key. Floors are rebuilt-from-footprint each resolve and the override is
    /// keyed by EXACT centroid, so without the fallback a post-attribution reshape
    /// silently reverts the floor to `Unset` (the centroid-key fragility).
    #[test]
    fn floor_exposure_containment_survives_reshape() {
        use ber_model::ber::structural::FloorExposure;
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);

        // Attribute the 4×4 floor: capture its centroid — that's the stored key.
        let small = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let old_centroid = resolve_storey(&small, &ber, 0, &[], &[], &[])
            .floors
            .regions[0]
            .centroid();

        // The assessor then RESHAPES the outline to 6×6 — the new floor's centroid
        // is (3,3), so the exact key (2,2) misses. The old centroid is still INSIDE
        // the enlarged floor, so the containment fallback must re-apply Ground.
        let mut big = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(6.0, 0.0),
            Point2f32::new(6.0, 6.0),
            Point2f32::new(0.0, 6.0),
        ]);
        big.floor_exposure
            .insert(crate::centroid_key(old_centroid), FloorExposure::Ground);

        let resolved = resolve_storey(&big, &ber, 0, &[], &[], &[]);
        assert_eq!(
            resolved.floors.regions[0].exposure,
            FloorExposure::Ground,
            "containment fallback re-applies the override after the centroid drifted off its key"
        );
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

    /// A 4×4 m finalised square, the common scaffold for the split tests.
    fn unit_square_doc() -> StoreyDoc {
        square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ])
    }

    /// A structure-phase FLOOR split divides the floor in two, BOTH pieces
    /// keeping the source construction (only the id is fresh), and the split
    /// rides the durable `floor_splits` field so it survives a re-resolve.
    #[test]
    fn floor_split_divides_floor_and_survives_re_resolve() {
        let doc = unit_square_doc();
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.storeys[0].floor_splits = vec![vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ]];
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey.floors.regions.len(), 2, "the cut split the floor");
        let cid = storey.floors.regions[0].construction_id;
        assert!(
            storey
                .floors
                .regions
                .iter()
                .all(|r| r.construction_id == cid),
            "both pieces keep the source construction"
        );
        // Durability: feed the resolved storey back and re-resolve.
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey2.floors.regions.len(), 2, "split survives re-resolve");
    }

    /// Stage 1 party-floor flow end-to-end at resolve level: a `plan_party_floor_carves`
    /// split ring pushed onto `floor_splits` divides the floor, and stamping the
    /// returned party sample resolves THAT sub-region `NonHeatLoss` (the other
    /// half stays Unset). Mirrors `floor_split_divides_floor_and_survives_re_resolve`
    /// with the exposure stamp added.
    #[test]
    fn party_floor_carve_splits_and_stamps_nonheatloss() {
        use ber_model::ber::structural::FloorExposure;
        let mut doc = unit_square_doc();
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);

        // Resolve the storey once to get its single floor region, then plan the
        // party carve against a below footprint covering the left half (x∈[0,2]).
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let below = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let (rings, samples) = doc.plan_party_floor_carves(&storey, &below);
        assert_eq!(rings.len(), 1, "partial overlap → one split ring");

        // Push the durable split + re-resolve: the floor divides in two.
        ber.storeys[0].floor_splits = rings;
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey.floors.regions.len(), 2, "split divided the floor");

        // Stamp the party sample → that sub-region resolves NonHeatLoss.
        doc.set_floor_exposure_at(
            &storey,
            samples[0].x,
            samples[0].y,
            FloorExposure::NonHeatLoss,
        )
        .expect("a floor region under the party sample");
        let mut resolved = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        crate::stamp_horizontal_exposure(&doc, &mut resolved);
        assert_eq!(
            resolved.floors.regions.len(),
            2,
            "split survives re-resolve"
        );

        // The party half (centroid x<2) is NonHeatLoss; the other half stays Unset.
        let party = resolved
            .floors
            .regions
            .iter()
            .find(|r| r.centroid().x < 2.0)
            .expect("the left (party) half");
        let other = resolved
            .floors
            .regions
            .iter()
            .find(|r| r.centroid().x >= 2.0)
            .expect("the right half");
        assert_eq!(party.exposure, FloorExposure::NonHeatLoss);
        assert_eq!(other.exposure, FloorExposure::Unset);
    }

    /// A structure-phase ROOF split divides the ceiling in two at the SAME
    /// height (delta-0), and survives a re-resolve.
    #[test]
    fn roof_split_divides_ceiling_keeping_height() {
        let doc = unit_square_doc();
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.storeys[0].ceiling_height_m = Some(2.4);
        ber.storeys[0].ceiling_splits = vec![vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ]];
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[]);
        assert_eq!(storey.ceilings.regions.len(), 2, "the cut split the ceiling");
        assert!(
            storey
                .ceilings
                .regions
                .iter()
                .all(|r| r.flat_z().is_some_and(|z| (z - 2.4).abs() < 1e-6)),
            "both pieces stay at the ceiling height"
        );
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey2.ceilings.regions.len(), 2, "split survives re-resolve");
    }

    /// An L-shaped storey split into two phase zones whose divider edge ENDS at a
    /// perimeter wall (a T-junction) rather than crossing between two walls (the
    /// `phase_area_carves_roof_like_the_floor` case). The phase split still: cuts
    /// the spanning perimeter wall in two (6 → 7 walls), carves the roof in two,
    /// and partitions the floor in two. Regression for a stale save that kept the
    /// roof/walls un-split (the floor partition predated the roof+wall split).
    #[test]
    fn l_shape_phase_split_through_a_t_junction() {
        let pts = vec![
            Point2f32::new(1.3, 2.3),
            Point2f32::new(1.3, 7.3),
            Point2f32::new(6.3, 7.3),
            Point2f32::new(6.3, 5.3),
            Point2f32::new(4.3, 5.3),
            Point2f32::new(4.3, 2.3),
        ];
        let plan = FloorPlan::from_clean_ring(&pts).unwrap();
        let mut doc = StoreyDoc::default();
        doc.dimensioning.scale_set = true;
        doc.floor_plan = plan.clone();
        doc.outline = ShapeSlot {
            floor_plan: plan,
            dimensioning: doc.dimensioning.clone(),
        };
        // Only the Extension zone is a phase region (Original = remainder), as
        // `phase_regions_from_floors` reconstructs it.
        let phase = vec![PhaseRegion {
            polygon: vec![
                Point2f32::new(1.3, 5.3),
                Point2f32::new(1.3, 2.3),
                Point2f32::new(4.3, 2.3),
                Point2f32::new(4.3, 5.3),
            ],
            phase: ConstructionPhase::Extension1,
        }];
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &phase);
        ber.set_default_ceiling_height(Some(0), 2.0);
        let storey = resolve_storey(&doc, &ber, 0, &phase, &[], &[]);
        assert_eq!(
            storey.walls.regions.len(),
            7,
            "the left wall spanning the phase boundary splits in two"
        );
        assert_eq!(storey.ceilings.regions.len(), 2, "the roof carves in two");
        assert_eq!(
            storey.floors.regions.len(),
            2,
            "the floor partitions in two"
        );
    }

    /// A construction-PHASE area over half a storey CARVES the roof in two — the
    /// ceiling analogue of the floor partition — keeping both halves flat at the
    /// ceiling height (a surface-preserving coplanar split), with the plan area
    /// conserved. The carve is a re-resolve FIXPOINT: an already-carved face is
    /// kept intact, so feeding the result back never over-splits.
    #[test]
    fn phase_area_carves_roof_like_the_floor() {
        let doc = unit_square_doc();
        let phase = vec![PhaseRegion {
            polygon: vec![
                Point2f32::new(0.0, 0.0),
                Point2f32::new(2.0, 0.0),
                Point2f32::new(2.0, 4.0),
                Point2f32::new(0.0, 4.0),
            ],
            phase: ConstructionPhase::Extension1,
        }];
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &phase);
        ber.set_default_ceiling_height(Some(0), 2.4);
        let storey = resolve_storey(&doc, &ber, 0, &phase, &[], &[]);
        assert_eq!(
            storey.ceilings.regions.len(),
            2,
            "the phase area carved the roof in two"
        );
        assert!(
            storey
                .ceilings
                .regions
                .iter()
                .all(|r| r.flat_z().is_some_and(|z| (z - 2.4).abs() < 1e-6)),
            "both halves stay flat at the ceiling height"
        );
        assert!(
            (storey.ceilings.regions.iter().map(roof_plan_area).sum::<f64>() - 16.0).abs() < 1e-4,
            "plan area conserved"
        );
        // The floor partition still happened alongside the roof carve.
        assert_eq!(storey.floors.regions.len(), 2, "floor partitioned too");
        // Fixpoint: feed the carved storey back and re-resolve.
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &phase, &[], &[]);
        assert_eq!(
            storey2.ceilings.regions.len(),
            2,
            "carve replays as a fixpoint, no over-split"
        );
    }

    /// The roof surface z at plan `(x, y)` — the containing region's plane,
    /// evaluated there — for asserting a carve re-TILES the surface without
    /// re-deriving it. `None` when no region's plan polygon contains the point.
    fn roof_z_at(storey: &Storey, x: f64, y: f64) -> Option<f64> {
        let plane = |r: &ber_model::ber::structural::CeilingRegion| -> Option<(f64, f64, f64)> {
            let ext = &r.polygon.exterior;
            let o = ext.first()?;
            for i in 1..ext.len() {
                for j in (i + 1)..ext.len() {
                    let (ux, uy, uz) = (ext[i].x - o.x, ext[i].y - o.y, ext[i].z - o.z);
                    let (vx, vy, vz) = (ext[j].x - o.x, ext[j].y - o.y, ext[j].z - o.z);
                    let det = ux * vy - uy * vx;
                    if det.abs() > 1e-12 {
                        let cx = (uz * vy - vz * uy) / det;
                        let cy = (vz * ux - uz * vx) / det;
                        return Some((o.z - cx * o.x - cy * o.y, cx, cy));
                    }
                }
            }
            None
        };
        let contains = |r: &ber_model::ber::structural::CeilingRegion| -> bool {
            let ext = &r.polygon.exterior;
            let n = ext.len();
            let mut inside = false;
            let mut j = n - 1;
            for i in 0..n {
                let (xi, yi) = (ext[i].x, ext[i].y);
                let (xj, yj) = (ext[j].x, ext[j].y);
                if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                    inside = !inside;
                }
                j = i;
            }
            inside
        };
        storey
            .ceilings
            .regions
            .iter()
            .find(|r| contains(r))
            .and_then(|r| plane(r).map(|(c, cx, cy)| c + cx * x + cy * y))
    }

    /// Carving ACROSS a pitched roof's ridge SPLITS the resolved faces in place
    /// — the surface is bit-identical (the z-field at every interior point is
    /// unchanged), only the region count grows. Guards the bug where the carve
    /// ran BEFORE the apexes and re-pitched each half into its own roof (the
    /// ridge collapsed into two pyramids with a valley between the apexes).
    #[test]
    fn roof_carve_across_ridge_splits_faces_without_re_pitching() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        // A ridge along x = 2 (apexes (2,1) and (2,3) at 4.2) over the 4×4.
        ber.storeys[0].ceiling_apexes = vec![
            CeilingApex {
                point: Point2f64::new(2.0, 1.0),
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(2.0, 3.0),
                z_m: 4.2,
            },
        ];
        ber.storeys[0] = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[]);
        let before_n = ber.storeys[0].ceilings.regions.len();
        // Sample the surface at asymmetric interior points (off the ridge line
        // and region boundaries) — the tiling-independent fingerprint.
        let probes = [
            (0.7, 0.6),
            (1.3, 2.0),
            (1.1, 3.4),
            (2.9, 0.7),
            (3.4, 2.4),
            (2.6, 3.3),
        ];
        let before: Vec<f64> = probes
            .iter()
            .map(|&(x, y)| roof_z_at(&ber.storeys[0], x, y).expect("probe inside a region"))
            .collect();
        // Mid-ridge is the discriminator: 4.2 for one ridge, a valley for two
        // pyramids.
        assert!(
            (roof_z_at(&ber.storeys[0], 1.9, 2.0).unwrap() - (2.4 + 1.8 * 0.95)).abs() < 1e-6,
            "left slope rises toward the ridge"
        );

        // Carve the lower strip (y ≤ 1.5) — straddles the ridge's apex at y = 1,
        // exactly the "split in the middle of the ridge" case.
        ber.storeys[0].ceiling_splits = vec![vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 1.5),
            Point2f64::new(0.0, 1.5),
        ]];
        ber.storeys[0] = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[]);

        assert!(
            ber.storeys[0].ceilings.regions.len() > before_n,
            "the carve split faces (more regions): {} → {}",
            before_n,
            ber.storeys[0].ceilings.regions.len()
        );
        let after: Vec<f64> = probes
            .iter()
            .map(|&(x, y)| roof_z_at(&ber.storeys[0], x, y).expect("probe still inside a region"))
            .collect();
        for ((x, y), (b, a)) in probes.iter().zip(before.iter().zip(&after)) {
            assert!(
                (b - a).abs() < 1e-6,
                "surface changed at ({x}, {y}): {b:.4} → {a:.4} (the carve re-pitched it)"
            );
        }
        assert!(
            (roof_z_at(&ber.storeys[0], 1.9, 2.0).unwrap() - (2.4 + 1.8 * 0.95)).abs() < 1e-6,
            "the ridge is intact, not collapsed into two pyramids"
        );
        // Total plan area is conserved (nothing lost to the split).
        assert!(
            (ber.storeys[0]
                .ceilings
                .regions
                .iter()
                .map(roof_plan_area)
                .sum::<f64>()
                - 16.0)
                .abs()
                < 1e-4
        );

        // Re-resolve is a fixpoint: the split replays to the same region count.
        let n1 = ber.storeys[0].ceilings.regions.len();
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(ber.storeys[0].ceilings.regions.len(), n1, "split is stable");
    }

    /// SPIKE: outline re-fit GROWS the footprint — the ridge is coplanar-clipped
    /// (slope preserved where it survives) and the newly-exposed strip fills flat
    /// at the default height.
    #[test]
    fn refit_grows_footprint_and_fills_exposed_area_flat() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        ber.storeys[0].ceiling_apexes = vec![
            CeilingApex {
                point: Point2f64::new(2.0, 1.0),
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(2.0, 3.0),
                z_m: 4.2,
            },
        ];
        ber.storeys[0] = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[]);
        let cid = ber.storeys[0].ceilings.regions[0].construction_id;
        // Grow the 4×4 to 6×4 (extra 2×4 strip at x ∈ [4, 6]).
        let grown = ber.storeys[0].ceilings.clone().refit_to_footprint(
            &[vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(6.0, 0.0),
                Point2f64::new(6.0, 4.0),
                Point2f64::new(0.0, 4.0),
            ]],
            2.4,
            cid,
        );
        assert!(
            (grown.regions.iter().map(roof_plan_area).sum::<f64>() - 24.0).abs() < 1e-3,
            "covers the grown footprint"
        );
        assert!(
            grown.regions.iter().any(|r| r
                .polygon
                .exterior
                .iter()
                .any(|v| (v.z - 4.2).abs() < 1e-9)),
            "the ridge survived the clip (still rises to 4.2)"
        );
        // The exposed strip is flat default; its only flat-2.4 area is that 2×4.
        let flat_area: f64 = grown
            .regions
            .iter()
            .filter(|r| r.flat_z().is_some_and(|z| (z - 2.4).abs() < 1e-6))
            .map(roof_plan_area)
            .sum();
        assert!(
            (flat_area - 8.0).abs() < 1e-3,
            "exposed 2×4 strip filled flat"
        );
    }

    /// SPIKE: outline re-fit SHRINKS the footprint — regions clip down, no
    /// exposed fill, the ridge (apexes still inside) survives.
    #[test]
    fn refit_shrinks_footprint_clipping_regions() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        ber.storeys[0].ceiling_apexes = vec![
            CeilingApex {
                point: Point2f64::new(2.0, 1.0),
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(2.0, 3.0),
                z_m: 4.2,
            },
        ];
        ber.storeys[0] = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[]);
        let cid = ber.storeys[0].ceilings.regions[0].construction_id;
        // Shrink the 4×4 to 3×4 (drop x ∈ [3, 4]); apexes at x = 2 stay inside.
        let shrunk = ber.storeys[0].ceilings.clone().refit_to_footprint(
            &[vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(3.0, 0.0),
                Point2f64::new(3.0, 4.0),
                Point2f64::new(0.0, 4.0),
            ]],
            2.4,
            cid,
        );
        assert!(
            (shrunk.regions.iter().map(roof_plan_area).sum::<f64>() - 12.0).abs() < 1e-3,
            "clipped to the shrunk footprint"
        );
        assert!(
            shrunk.regions.iter().any(|r| r
                .polygon
                .exterior
                .iter()
                .any(|v| (v.z - 4.2).abs() < 1e-9)),
            "the ridge survives (apexes still inside)"
        );
    }

    /// A structure-phase WALL split divides the crossed wall into two sub-walls
    /// (4 → 5), and the sub-walls keep stable identity across a re-resolve.
    #[test]
    fn wall_split_divides_wall_and_keeps_identity_on_re_resolve() {
        let doc = unit_square_doc();
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.storeys[0].ceiling_height_m = Some(2.4);
        // A cut crossing the bottom wall (0,0)→(4,0) at x = 2.
        ber.storeys[0].wall_splits = vec![[Point2f64::new(2.0, -1.0), Point2f64::new(2.0, 1.0)]];
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey.walls.regions.len(), 5, "one wall split → 4 + 1");
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey2.walls.regions.len(), 5, "split survives re-resolve");
    }

    /// A construction-PHASE area covering the left half of a square SPLITS the
    /// two perimeter walls its interior boundary (x = 2) crosses — top + bottom
    /// — so each becomes two sub-walls: 4 → 6. The party-wall divider itself runs
    /// along no perimeter wall, so it adds no spurious split. This is the wall
    /// analogue of the floor partition + roof carve.
    #[test]
    fn phase_area_splits_the_walls_it_crosses() {
        let doc = unit_square_doc();
        let phase = vec![PhaseRegion {
            polygon: vec![
                Point2f32::new(0.0, 0.0),
                Point2f32::new(2.0, 0.0),
                Point2f32::new(2.0, 4.0),
                Point2f32::new(0.0, 4.0),
            ],
            phase: ConstructionPhase::Extension1,
        }];
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &phase);
        ber.storeys[0].ceiling_height_m = Some(2.4);
        let storey = resolve_storey(&doc, &ber, 0, &phase, &[], &[]);
        assert_eq!(
            storey.walls.regions.len(),
            6,
            "the boundary at x = 2 split the top + bottom walls"
        );
        // The split is a re-resolve fixpoint (walls regenerate full, then re-split).
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &phase, &[], &[]);
        assert_eq!(
            storey2.walls.regions.len(),
            6,
            "wall split survives re-resolve"
        );
    }

    /// A corner/notch carve yields TWO interior cut edges, each crossing a
    /// DIFFERENT wall — both must split (the live regression: a multi-edge carve
    /// was wrongly rejected as ambiguous). Notching the top-right corner splits
    /// the top + right walls: 4 → 6.
    #[test]
    fn two_wall_cuts_from_a_corner_notch_split_both_walls() {
        let doc = unit_square_doc();
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.storeys[0].ceiling_height_m = Some(2.4);
        ber.storeys[0].wall_splits = vec![
            [Point2f64::new(3.0, 4.0), Point2f64::new(3.0, 3.0)], // crosses the top wall at (3,4)
            [Point2f64::new(3.0, 3.0), Point2f64::new(4.0, 3.0)], // crosses the right wall at (4,3)
        ];
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(
            storey.walls.regions.len(),
            6,
            "top + right walls each split"
        );
    }

    /// A window on a wall that gets split must be RE-HOMED onto a sub-wall, not
    /// orphaned and pruned — the prune runs inside the same resolve, so the
    /// re-home has to happen before it.
    #[test]
    fn wall_split_rehomes_openings_instead_of_orphaning_them() {
        use ber_model::ber::fenestration::{OpeningPlacement, Window};
        let doc = unit_square_doc();
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.storeys[0].ceiling_height_m = Some(2.4);
        // Resolve once to learn the bottom wall's id, then place a window on it.
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let bottom = storey
            .walls
            .regions
            .iter()
            .find(|w| {
                w.base_midpoint()
                    .is_some_and(|m| m.y.abs() < 1e-6 && (m.x - 2.0).abs() < 1e-6)
            })
            .expect("bottom wall (base (0,0)->(4,0))");
        let parent_id = bottom.id;
        ber.storeys[0] = storey;
        ber.storeys[0].windows.push(Window {
            id: ItemId::default(),
            vertices: vec![],
            width_m: 1.0,
            height_m: 1.0,
            window_type_id: ItemId::default(),
            placement: Some(OpeningPlacement::Wall {
                wall_id: parent_id,
                along_m: 1.0, // left of the x = 2 cut
                sill_m: 0.8,
            }),
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            arched: false,
            overshading: Default::default(),
        });
        // Split the bottom wall at x = 2.
        ber.storeys[0].wall_splits = vec![[Point2f64::new(2.0, -1.0), Point2f64::new(2.0, 1.0)]];
        let resolved = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(resolved.windows.len(), 1, "the window survives the split");
        match resolved.windows[0].placement.as_ref().unwrap() {
            OpeningPlacement::Wall { wall_id, .. } => {
                assert_ne!(*wall_id, parent_id, "re-homed off the now-gone parent");
                assert!(
                    resolved.walls.regions.iter().any(|w| w.id == *wall_id),
                    "re-homed onto an existing sub-wall"
                );
            }
            _ => panic!("window lost its wall placement"),
        }
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
        let on_step_line = |r: &&ber_model::ber::structural::WallRegion| {
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
        let _riser_id = risers[0].id;
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let _riser2 = storey2
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .expect("riser re-derives");
    }

    /// THE RISER DURABILITY RULE at resolve level: an overridden riser exposure
    /// (recorded in the durable `riser_exposure` table, keyed by base edge)
    /// survives a plain re-resolve AND a re-resolve after a geometry-affecting
    /// edit (the ceiling height lands, re-extruding every wall) — the stamp
    /// re-applies it instead of the derived `Unset` default.
    #[test]
    fn riser_exposure_override_survives_re_resolves() {
        let mut doc = square_doc(&[
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
        let on_step_line = |r: &&ber_model::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 2.0).abs() < 1e-6 && (e[1].x - 2.0).abs() < 1e-6
        };
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        let storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        // The riser defaults Unset — record the user's override in the durable
        // table (keyed by base edge), the source of truth the resolve stamps.
        let riser = storey
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .expect("the floor riser derives");
        assert_eq!(riser.exposure, WallExposure::Unset, "fresh → Unset");
        let (e0, e1) = riser.base_edge().unwrap();
        let key = crate::riser_key(Point2f64::new(e0.x, e0.y), Point2f64::new(e1.x, e1.y));
        doc.riser_exposure.insert(key, WallExposure::SemiExposed);

        // Plain re-resolve: the override is stamped from the table.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let r = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .unwrap();
        assert_eq!(r.exposure, WallExposure::SemiExposed, "override survives");

        // Geometry-affecting edit: the height lands, walls re-extrude — the
        // riser's base edge is unchanged, so the table still keys it.
        ber.storeys[0].ceiling_height_m = Some(2.7);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let r = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .unwrap();
        assert_eq!(
            r.exposure,
            WallExposure::SemiExposed,
            "override survives the re-extrusion re-derive"
        );
    }

    /// The durable `StoreyDoc.riser_exposure` table drives riser exposure
    /// INDEPENDENTLY of the amend: with an EMPTY previous set (so the amend can
    /// carry nothing), a riser still resolves to the table's exposure rather than
    /// the derived `Unset` default. This is the mechanism that lets Stage 2c
    /// retire the amend without losing riser overrides.
    #[test]
    fn riser_exposure_table_drives_exposure_without_amend() {
        let mut doc = square_doc(&[
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
        let on_step_line = |r: &&ber_model::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 2.0).abs() < 1e-6 && (e[1].x - 2.0).abs() < 1e-6
        };
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        // First resolve only to read the riser's base edge for its key.
        let storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let (e0, e1) = storey
            .walls
            .regions
            .iter()
            .find(on_step_line)
            .unwrap()
            .base_edge()
            .unwrap();
        let key = crate::riser_key(Point2f64::new(e0.x, e0.y), Point2f64::new(e1.x, e1.y));
        doc.riser_exposure.insert(key, WallExposure::SemiExposed);

        // Resolve again WITHOUT assigning the prior storey back — `previous` is
        // the still-empty `ber.storeys[0]`, so the amend has nothing to carry.
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);
        let riser = storey2.walls.regions.iter().find(on_step_line).unwrap();
        assert_eq!(
            riser.exposure,
            WallExposure::SemiExposed,
            "riser exposure came from the durable table, not the amend"
        );
    }

    /// A floor/ceiling horizontal-exposure override in the durable
    /// `floor_exposure`/`ceiling_exposure` tables (keyed by region centroid) is
    /// stamped onto the resolved `FloorRegion.exposure`/`CeilingRegion.exposure`
    /// and SURVIVES a re-resolve — the area analogue of
    /// `riser_exposure_override_survives_re_resolves`.
    #[test]
    fn horizontal_exposure_override_survives_re_resolves() {
        use ber_model::ber::structural::{CeilingExposure, FloorExposure};
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.storeys[0].ceiling_height_m = Some(2.4);

        // Resolve once to learn the floor + ceiling region centroids.
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let fkey = crate::centroid_key(storey.floors.regions[0].centroid());
        let ckey = crate::centroid_key(storey.ceilings.regions[0].centroid());
        assert_eq!(
            storey.floors.regions[0].exposure,
            FloorExposure::Unset,
            "fresh → Unset"
        );
        assert_eq!(storey.ceilings.regions[0].exposure, CeilingExposure::Unset);
        doc.floor_exposure.insert(fkey, FloorExposure::Ground);
        doc.ceiling_exposure
            .insert(ckey, CeilingExposure::ExposedRoof);

        // Re-resolve: the overrides are stamped from the tables.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(
            ber.storeys[0].floors.regions[0].exposure,
            FloorExposure::Ground,
            "floor exposure stamped from the durable table"
        );
        assert_eq!(
            ber.storeys[0].ceilings.regions[0].exposure,
            CeilingExposure::ExposedRoof,
            "ceiling exposure stamped from the durable table"
        );

        // Geometry-affecting re-resolve (height change): the region keeps its
        // shape → its centroid still keys the table.
        ber.storeys[0].ceiling_height_m = Some(2.7);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(
            ber.storeys[0].floors.regions[0].exposure,
            FloorExposure::Ground,
            "floor override survives the re-derive"
        );
        assert_eq!(
            ber.storeys[0].ceilings.regions[0].exposure,
            CeilingExposure::ExposedRoof,
            "ceiling override survives the re-derive"
        );
    }

    /// An assigned wall construction (type + make-up) in the durable
    /// `wall_constructions` store, keyed by SurfaceRef, is stamped onto the
    /// re-derived perimeter wall — composed with its derived phase. This is
    /// construction's parametric home (independent of the amend).
    #[test]
    fn assigned_wall_construction_stamps_from_the_store() {
        use ber_model::ber::structural::{ConstructionMakeup, WallConstructionType};
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        // Assign Timber / Default to the first wall by its SurfaceRef.
        let (e0, e1) = ber.storeys[0].walls.regions[0].base_edge().unwrap();
        let mid = Point2f32::new(((e0.x + e1.x) * 0.5) as f32, ((e0.y + e1.y) * 0.5) as f32);
        let sref = doc.outline.floor_plan.surface_ref_at(mid, 0.05).unwrap();
        doc.wall_constructions.insert(
            sref,
            (
                WallConstructionType::TimberConstruction,
                ConstructionMakeup::Default,
                ConstructionPhase::default(),
            ),
        );

        // Re-resolve + stamp (mirroring refresh_storey's tail).
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        crate::stamp_wall_phases(&mut ber, 0);
        crate::stamp_wall_constructions(&mut ber, 0, &doc);

        let wall = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| {
                let (a, b) = r.base_edge().unwrap();
                let m = Point2f32::new(((a.x + b.x) * 0.5) as f32, ((a.y + b.y) * 0.5) as f32);
                doc.outline.floor_plan.surface_ref_at(m, 0.05) == Some(sref)
            })
            .expect("the assigned wall re-derives");
        let wc = ber.wall_construction(wall.construction_id).unwrap();
        assert_eq!(
            wc.construction_type,
            WallConstructionType::TimberConstruction
        );
        assert_eq!(wc.base.construction_makeup, ConstructionMakeup::Default);
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

    /// THE FLOOR-STEP BASE FIX (storey 0, no slab band): a raised floor strip
    /// makes the perimeter walls bordering / spanning it base at the RAISED
    /// level — no flat wall bottom left at the datum under the raised floor
    /// (the phantom-plane bug), and the step RISER face is unchanged.
    #[test]
    fn perimeter_wall_base_follows_a_raised_floor_step() {
        let doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        // Raised strip x ∈ [3, 4] at +0.18 (mirrors the bug dump).
        let steps = vec![crate::StepRegion {
            polygon: vec![
                Point2f32::new(3.0, 0.0),
                Point2f32::new(4.0, 0.0),
                Point2f32::new(4.0, 4.0),
                Point2f32::new(3.0, 4.0),
            ],
            level_m: 0.18,
        }];
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        let storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);

        let is_riser = |r: &ber_model::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 3.0).abs() < 1e-6 && (e[1].x - 3.0).abs() < 1e-6
        };
        // The RIGHT wall (x = 4) entirely borders the raised strip → whole
        // base at 0.18, nothing at the datum.
        let right = storey
            .walls
            .regions
            .iter()
            .find(|r| !is_riser(r) && r.polygon.exterior.iter().all(|v| (v.x - 4.0).abs() < 1e-6))
            .expect("the right wall");
        let rext = &right.polygon.exterior;
        assert!(
            rext.iter()
                .filter(|v| v.z < 1.0)
                .all(|v| (v.z - 0.18).abs() < 1e-9),
            "right wall base entirely at the raised level: {rext:?}"
        );
        // The FRONT wall (y = 0, x: 0 → 4) crosses the step line at x = 3:
        // its base STEPS from 0 to 0.18 — no flat datum bottom under the
        // raised strip.
        let front = storey
            .walls
            .regions
            .iter()
            .find(|r| {
                !is_riser(r) && {
                    let e = &r.polygon.exterior;
                    e.iter().any(|v| v.y.abs() < 1e-9) && e.iter().any(|v| (v.x - 4.0).abs() < 1e-6)
                }
            })
            .expect("the front wall");
        let fext = &front.polygon.exterior;
        assert!(
            fext.iter().any(|v| v.z.abs() < 1e-9) && fext.iter().any(|v| (v.z - 0.18).abs() < 1e-9),
            "front wall base STEPS 0 → 0.18 across the step line: {fext:?}"
        );
        // The step RISER is unchanged: a face on the step line x = 3 from the
        // ground level 0 up to the raised 0.18.
        let riser = storey
            .walls
            .regions
            .iter()
            .find(|r| is_riser(r))
            .expect("the floor-step riser survives");
        let zs: Vec<f64> = riser.polygon.exterior.iter().map(|v| v.z).collect();
        assert!(
            zs.iter().any(|z| z.abs() < 1e-9) && zs.iter().any(|z| (z - 0.18).abs() < 1e-9),
            "riser spans 0 → 0.18, unchanged: {zs:?}"
        );
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
        let is_riser = |r: &ber_model::ber::structural::WallRegion| {
            let e = &r.polygon.exterior;
            (e[0].x - 2.0).abs() < 1e-6 && (e[1].x - 2.0).abs() < 1e-6
        };
        // The slab band COMPOSES with the floor step: a perimeter wall over
        // the step region (x ∈ [0, 2], level −0.3) bases at −0.225 − 0.3 =
        // −0.525; outside it at the bare slab band −0.225. A wall CROSSING
        // the step line (the front y = 0 and back y = 4 spans) steps between
        // the two with a vertical jump, so its base is no longer a flat pair
        // — but `exterior[0]` (a) and the LAST base vertex (b) still bracket
        // the footprint edge, both at a valid base level.
        for r in storey.walls.regions.iter().filter(|r| !is_riser(r)) {
            let e = &r.polygon.exterior;
            let base_levels_ok = |z: f64| (z + 0.225).abs() < 1e-9 || (z + 0.525).abs() < 1e-9;
            // Every base vertex sits at one of the two valid base levels.
            assert!(
                e.iter()
                    .take_while(|v| v.z < -0.1) // base band is below 0; top is +2.4
                    .all(|v| base_levels_ok(v.z)),
                "perimeter base at slab band ± step: {e:?}"
            );
            assert!(
                (e[0].z + 0.225).abs() < 1e-9 || (e[0].z + 0.525).abs() < 1e-9,
                "base origin at a valid level: {e:?}"
            );
            assert!(
                e.iter().any(|v| (v.z - 2.4).abs() < 1e-9),
                "top stays at the ABSOLUTE ceiling height: {e:?}"
            );
        }
        // The wall spanning the step line steps: the front (y = 0) wall has a
        // vertical base jump at x = 2 between −0.525 (over the step) and
        // −0.225 (the bare slab band) — the bug fix, base follows the floor.
        let front = storey
            .walls
            .regions
            .iter()
            .find(|r| {
                let e = &r.polygon.exterior;
                e.iter().any(|v| (v.y).abs() < 1e-9) && !is_riser(r)
            })
            .expect("the front wall");
        let fext = &front.polygon.exterior;
        assert!(
            fext.iter().any(|v| (v.z + 0.525).abs() < 1e-9)
                && fext.iter().any(|v| (v.z + 0.225).abs() < 1e-9),
            "front wall base STEPS over the floor step: {fext:?}"
        );
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
        let len = manip_plan::segment_len(a, b);
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
        // A REAL door on the marked wall — `opes` is derived from placements.
        let marked_wall_id = storey.walls.regions[marked_idx].id;
        let door_id = ber_model::ber::openings::add_wall_door(
            &mut storey,
            marked_wall_id,
            1.0,
            0.9,
            2.0,
            ber_model::ber::fenestration::DoorKind::Solid,
            "door".to_string(),
            0.0,
        )
        .unwrap();
        let (_id0, cid0) = (
            storey.walls.regions[marked_idx].id,
            storey.walls.regions[marked_idx].construction_id,
        );
        let mut ids0: Vec<_> = storey.walls.regions.iter().map(|r| r.id).collect();
        ids0.sort_by_key(|id| id.0);

        // What reload does: rebuild the doc from points (same `doc` here)
        // and re-resolve against the PERSISTED region set.
        ber.storeys[1] = storey;
        let storey2 = resolve_storey(&doc, &ber, 1, &[], &[], &[]);
        let wall = storey2
            .walls
            .regions
            .iter()
            .find(|r| r.opes.doors == vec![door_id])
            .expect("the marked wall keeps its opening across reload");
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
        use manip_plan::{SubareaOutcome, SubareaTuning};
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
        let (doc, mut ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[step]);
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

    /// The exact storey of the LIVE REPORT (2026-06): an L/U-shaped outline
    /// with one ceiling-step rect (+1.0 over h = 2.4) whose south and west
    /// boundary lines CONTINUE perimeter wall planes. The fixture for the
    /// regression test below and the merge-rule resolve tests.
    fn flush_riser_fixture() -> (StoreyDoc, Ber, Vec<crate::CeilingStepRegion>) {
        let doc = square_doc(&[
            Point2f32::new(5.2184, 6.5205),
            Point2f32::new(5.2184, 8.5205),
            Point2f32::new(8.2184, 8.5205),
            Point2f32::new(8.2184, 2.5205),
            Point2f32::new(4.2184, 2.5205),
            Point2f32::new(4.2184, 1.5205),
            Point2f32::new(2.2184, 1.5205),
            Point2f32::new(2.2184, 5.5205),
            Point2f32::new(3.2184, 5.5205),
            Point2f32::new(3.2184, 6.5205),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        let steps = vec![crate::CeilingStepRegion {
            polygon: vec![
                Point2f32::new(3.2184, 6.5205),
                Point2f32::new(3.2184, 2.5205),
                Point2f32::new(5.2184, 2.5205),
                Point2f32::new(5.2184, 6.5205),
            ],
            delta_m: 1.0,
        }];
        (doc, ber, steps)
    }

    /// THE COPLANAR-FLUSH RISER MERGE REGRESSION (live report, 2026-06):
    /// before the merge this storey resolved 13 wall regions — 10 perimeter
    /// + 3 ceiling risers, two of which were coplanar continuations of
    /// perimeter walls, visually splitting one physical wall in two. Now:
    /// 11 regions — the south side covered by ONE region (the wall with an
    /// L-profiled polygon), the west side likewise, and the third riser
    /// (which touches its coplanar wall at a single point only) still a
    /// separate region; `riser_walls` lists ONLY that one, so the dashed
    /// picker never offers a merged riser (it's painted through its host
    /// wall's normal flow). Identity is stable: a re-resolve amends every
    /// region and re-derives the merged polygons byte-identically.
    #[test]
    fn flush_coplanar_risers_merge_into_their_continuation_walls() {
        let (doc, mut ber, steps) = flush_riser_fixture();
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &steps);
        assert_eq!(
            storey.walls.regions.len(),
            11,
            "10 perimeter walls + the single surviving riser"
        );

        // A region by its base edge (the contract: exterior[0]/[1]).
        fn wall_with_base<'a>(
            s: &'a ber_model::ber::structural::Storey,
            a: (f64, f64),
            b: (f64, f64),
        ) -> &'a ber_model::ber::structural::WallRegion {
            s.walls
                .regions
                .iter()
                .find(|r| {
                    let e = &r.polygon.exterior;
                    (e[0].x - a.0).abs() < 1e-4
                        && (e[0].y - a.1).abs() < 1e-4
                        && (e[1].x - b.0).abs() < 1e-4
                        && (e[1].y - b.1).abs() < 1e-4
                })
                .unwrap_or_else(|| panic!("no wall with base {a:?} -> {b:?}"))
        }
        let ring_xyz = |r: &ber_model::ber::structural::WallRegion| -> Vec<(f64, f64, f64)> {
            r.polygon.exterior.iter().map(|v| (v.x, v.y, v.z)).collect()
        };
        let assert_ring = |got: &[(f64, f64, f64)], want: &[(f64, f64, f64)], name: &str| {
            assert_eq!(got.len(), want.len(), "{name}: {got:?}");
            for (g, w) in got.iter().zip(want) {
                assert!(
                    (g.0 - w.0).abs() < 1e-4
                        && (g.1 - w.1).abs() < 1e-4
                        && (g.2 - w.2).abs() < 1e-4,
                    "{name}: got {got:?}, want {want:?}"
                );
            }
        };

        // The SOUTH side: one region — the perimeter wall, its polygon the
        // L-profiled union with the merged riser (base edge first, top
        // tracing b -> a with the step jump at x = 5.2184, the riser ring
        // spliced in along the shared vertical edge at x = 4.2184).
        let south = wall_with_base(&storey, (8.2184, 2.5205), (4.2184, 2.5205));
        assert_ring(
            &ring_xyz(south),
            &[
                (8.2184, 2.5205, 0.0),
                (4.2184, 2.5205, 0.0),
                (4.2184, 2.5205, 2.4),
                (3.2184, 2.5205, 2.4),
                (3.2184, 2.5205, 3.4),
                (5.2184, 2.5205, 3.4),
                (5.2184, 2.5205, 2.4),
                (8.2184, 2.5205, 2.4),
            ],
            "south wall",
        );

        // The WEST side: likewise one L-profiled region.
        let west = wall_with_base(&storey, (3.2184, 5.5205), (3.2184, 6.5205));
        assert_ring(
            &ring_xyz(west),
            &[
                (3.2184, 5.5205, 0.0),
                (3.2184, 6.5205, 0.0),
                (3.2184, 6.5205, 3.4),
                (3.2184, 2.5205, 3.4),
                (3.2184, 2.5205, 2.4),
                (3.2184, 5.5205, 2.4),
            ],
            "west wall",
        );

        // The EAST riser (x = 5.2184) touches its coplanar perimeter wall
        // at the single point (5.2184, 6.5205, 2.4) — NO shared
        // positive-length edge, so it stays separate, and it is the ONLY
        // riser the dashed picker lists.
        let risers = doc.riser_walls(&storey);
        assert_eq!(risers.len(), 1, "merged risers never reach the picker");
        assert_eq!(risers[0].kind, crate::RiserKind::Ceiling);
        assert!(
            (risers[0].a.x - 5.2184).abs() < 1e-4 && (risers[0].b.x - 5.2184).abs() < 1e-4,
            "the surviving riser is the east one: {risers:?}"
        );

        // GEOMETRY FIXPOINT: ids are freshly minted now, so compare the merged
        // POLYGONS — a re-resolve over the merged result re-derives them
        // byte-identically (and the two merged walls still resolve).
        let geom = |w: &WallRegions| {
            w.regions
                .iter()
                .map(|r| format!("{:?}", r.polygon))
                .collect::<Vec<_>>()
        };
        let fingerprint = geom(&storey.walls);
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &[], &steps);
        assert_eq!(
            geom(&storey2.walls),
            fingerprint,
            "re-resolve is a geometry fixpoint over the merged walls"
        );
        let _ = wall_with_base(&storey2, (8.2184, 2.5205), (4.2184, 2.5205));
        let _ = wall_with_base(&storey2, (3.2184, 5.5205), (3.2184, 6.5205));
    }

    /// Exposure painted on a HOST wall survives the merge and the amend: the
    /// merged region carries the painted exposure, recovery
    /// (`sections_from_wall_regions`) still keys off the unchanged base
    /// edge, and a re-resolve keeps it — the painted-section flow is
    /// untouched by the riser merge.
    #[test]
    fn exposure_painting_keys_off_the_merged_hosts_unchanged_base() {
        let (mut doc, mut ber, steps) = flush_riser_fixture();
        // Paint the full south wall (base (8.2184, 2.5205) -> (4.2184,
        // 2.5205)) SemiExposed.
        let (vid, a, b) = doc
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(_, a, b)| {
                (a.y - 2.5205).abs() < 1e-4
                    && (b.y - 2.5205).abs() < 1e-4
                    && (a.x - b.x).abs() > 1.0
            })
            .expect("the south wall exists");
        let len = manip_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::SemiExposed);

        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &steps);
        assert_eq!(storey.walls.regions.len(), 11);
        let south = storey
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::SemiExposed)
            .expect("the painted wall resolves");
        assert_eq!(
            south.polygon.exterior.len(),
            8,
            "the painted wall is the merged L-shaped region: {:?}",
            south.polygon.exterior
        );
        // Recovery (what reload does) finds exactly the painted section on
        // the south wall — the merged region's base edge is unchanged.
        let recovered = doc.sections_from_wall_regions(&storey.walls);
        let secs = recovered.get(&vid).expect("section recovers to the wall");
        assert_eq!(secs.len(), 1);
        assert_eq!(secs[0].exposure, WallExposure::SemiExposed);
        assert!(secs[0].start_m.abs() < 1e-3 && (secs[0].end_m - len).abs() < 1e-3);

        // The amend keeps the painted exposure across a re-resolve.
        let _south_id = south.id;
        ber.storeys[0] = storey;
        let storey2 = resolve_storey(&doc, &ber, 0, &[], &[], &steps);
        let _south2 = storey2
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::SemiExposed)
            .expect("the painted exposure re-applies from wall_sections");
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
        // The default ceiling construction is ensured too (ceilings derivation
        // looks it up read-only).
        assert_eq!(ber.ceiling_constructions.len(), 1);
        assert!(ber
            .ceiling_construction_id(Default::default(), Default::default(), Default::default())
            .is_some());
    }

    /// Ceiling height set → ceilings derive to one flat region per outline ring
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
        let len = manip_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);

        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.7);
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        assert_eq!(storey.ceilings.regions.len(), 1, "one region per outline ring");
        let roof = &storey.ceilings.regions[0];
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

    /// Ceiling height unset → ceilings untouched (whatever the stored storey had)
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
        let len = manip_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);

        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        // Pre-existing roof content on the stored storey survives untouched.
        let marker = ber_model::ber::structural::CeilingRegions::default_for_footprint(
            vec![vec![
                Point2f64::new(0.0, 0.0),
                Point2f64::new(1.0, 0.0),
                Point2f64::new(1.0, 1.0),
            ]],
            9.9,
            Default::default(),
        );
        ber.storeys[0].ceilings = marker.clone();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        assert_eq!(storey.ceilings.regions.len(), 1);
        assert!((storey.ceilings.regions[0].polygon.exterior[0].z - 9.9).abs() < 1e-9);
        assert!(storey.walls.regions.iter().all(|r| r
            .polygon
            .exterior
            .iter()
            .any(|q| (q.z - PROVISIONAL_WALL_EXTRUSION_M).abs() < 1e-9)));
    }

    /// Re-resolving after an outline change re-derives the ceilings to the new
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
        // Regions-as-truth: the outline change RE-FITS the stored roof — the old
        // 4×4 region is kept (clipped) and the newly-exposed 2×4 band fills flat
        // at the default, so the surface now tiles the 6×4 in two flat pieces
        // (no rebuild-from-scratch single region). All at the ceiling height.
        assert!(
            (storey2
                .ceilings
                .regions
                .iter()
                .map(roof_plan_area)
                .sum::<f64>()
                - 24.0)
                .abs()
                < 1e-6,
            "the roof follows the grown footprint"
        );
        assert!(storey2
            .ceilings
            .regions
            .iter()
            .flat_map(|r| r.polygon.exterior.iter())
            .any(|v| (v.x - 6.0).abs() < 1e-6 && (v.y - 4.0).abs() < 1e-6));
        assert!(storey2
            .ceilings
            .regions
            .iter()
            .all(|r| r.flat_z().is_some_and(|z| (z - 2.5).abs() < 1e-9)));
    }

    // ── Ceiling-step derivation ─────────────────────────────────────────────

    /// Unsigned shoelace area of a roof region's plan projection (no geo
    /// dependency in this crate).
    fn roof_plan_area(region: &ber_model::ber::structural::CeilingRegion) -> f64 {
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
            .ceilings
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

    /// Build ceiling geometry the REGIONS-AS-TRUTH way — exactly what the
    /// controller does at EDIT time. Seed the flat default via resolve, apply
    /// the ceiling `steps` (±Δ) and roof `apexes` ONCE onto the stored ceilings,
    /// then re-resolve (re-fit + derive walls). After this `ber.storeys[idx]`
    /// holds the truth ceilings; tests assert against them and any later re-resolve
    /// only re-fits.
    fn build_ceiling(
        doc: &StoreyDoc,
        ber: &mut Ber,
        idx: usize,
        phase: &[PhaseRegion],
        floor_steps: &[StepRegion],
        ceiling_steps: &[CeilingStepRegion],
        apexes: &[CeilingApex],
    ) {
        ber.storeys[idx] = resolve_storey(doc, ber, idx, phase, floor_steps, &[]);
        if !ceiling_steps.is_empty() || !apexes.is_empty() {
            let areas: Vec<(Vec<Point2f64>, f64)> = ceiling_steps
                .iter()
                .map(|r| (r.polygon.iter().copied().map(to_f64).collect(), r.delta_m))
                .collect();
            let mut ceilings = std::mem::take(&mut ber.storeys[idx].ceilings);
            if !areas.is_empty() {
                ceilings = ceilings.with_z_deltas(&areas);
            }
            if !apexes.is_empty() {
                ceilings = ceilings.with_apexes(apexes);
            }
            ber.storeys[idx].ceilings = ceilings;
        }
        ber.storeys[idx] = resolve_storey(doc, ber, idx, phase, floor_steps, &[]);
    }

    /// Legacy-style test shim: apply the storey's declarative ceiling EDITS
    /// (`ceiling_apexes` / `ceiling_splits`) plus the passed `ceiling_steps` (delta-0
    /// dividers included) ONCE onto the freshly-seeded ceilings (composition order —
    /// steps → apexes → face-splits), then resolve. Lets the pre-existing roof
    /// tests keep their declarative setup while exercising the regions-as-truth
    /// pipeline. Use for the FIRST (edit-applying) resolve on a fresh storey;
    /// later refit-only resolves call `resolve_storey` directly.
    fn resolve_storey_t(
        doc: &StoreyDoc,
        ber: &mut Ber,
        idx: usize,
        phase: &[PhaseRegion],
        floor_steps: &[StepRegion],
        ceiling_steps: &[CeilingStepRegion],
    ) -> Storey {
        let apexes = ber.storeys[idx].ceiling_apexes.clone();
        let hints = ber.storeys[idx].ceiling_apex_hints.clone();
        let splits = ber.storeys[idx].ceiling_splits.clone();
        ber.storeys[idx] = resolve_storey(doc, ber, idx, phase, floor_steps, &[]);
        let areas: Vec<(Vec<Point2f64>, f64)> = ceiling_steps
            .iter()
            .map(|r| (r.polygon.iter().copied().map(to_f64).collect(), r.delta_m))
            .collect();
        let mut ceilings = std::mem::take(&mut ber.storeys[idx].ceilings);
        if !areas.is_empty() {
            ceilings = ceilings.with_z_deltas(&areas);
        }
        if !apexes.is_empty() {
            ceilings = ceilings.with_apexes_hinted(&apexes, &hints);
        }
        if !splits.is_empty() {
            ceilings = ceilings.split_faces_coplanar(&splits);
        }
        ber.storeys[idx].ceilings = ceilings;
        ber.storeys[idx] = resolve_storey(doc, ber, idx, phase, floor_steps, &[]);
        ber.storeys[idx].clone()
    }

    /// REGIONS-AS-TRUTH FIXPOINT: once the ceilings hold a stepped + ridged
    /// surface, re-resolving (which re-fits to the unchanged outline) leaves
    /// them bit-stable — the always-refit is idempotent, so there is no drift
    /// across resolves and no reload-fixpoint contract to maintain.
    #[test]
    fn re_resolve_is_a_fixpoint_for_truth_roofs() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(3.0, 1.0),
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(3.0, 3.0),
                z_m: 4.2,
            },
        ];
        build_ceiling(&doc, &mut ber, 0, &[], &[], &[step], &apexes);
        let before = roof_surface_multiset(&ber.storeys[0]);
        let n = ber.storeys[0].ceilings.regions.len();
        assert!(n >= 5, "stepped + ridged surface resolved");
        // Two more pure re-resolves (no edits) — refit only.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(
            ber.storeys[0].ceilings.regions.len(),
            n,
            "region count stable across re-resolve"
        );
        assert_same_surface(&before, &roof_surface_multiset(&ber.storeys[0]));
    }

    /// One ceiling-step region (the line at x = 1, Greater side, +0.3) splits
    /// the derived ceilings into the default piece at h and the stepped piece at
    /// h + Δ — and the overhanging half-rect is clipped to the footprint.
    #[test]
    fn one_ceiling_step_splits_roofs_into_default_and_stepped() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                1.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[step]);

        assert_eq!(storey.ceilings.regions.len(), 2);
        assert!((roof_area_at(&storey, 2.4) - 4.0).abs() < 1e-6, "left 1×4");
        assert!(
            (roof_area_at(&storey, 2.7) - 12.0).abs() < 1e-6,
            "right 3×4"
        );
        // Clipped to the footprint: no vertex from the half-rect's overhang.
        for r in &storey.ceilings.regions {
            for v in &r.polygon.exterior {
                assert!((-1e-6..=4.0 + 1e-6).contains(&v.x));
                assert!((-1e-6..=4.0 + 1e-6).contains(&v.y));
            }
        }
    }

    /// Ceiling steps are deltas against the storey default: while the height
    /// is unset there is no default, so they're IGNORED (ceilings untouched).
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
                manip_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[step]);
        assert!(
            storey.ceilings.regions.is_empty(),
            "no height → ceilings untouched, steps ignored"
        );
    }

    /// Two crossing steps COMPOSE: a vertical then a horizontal line produce
    /// four quadrants at h, h+a, h+b, h+a+b.
    #[test]
    fn two_crossing_ceiling_steps_compose_into_four_heights() {
        let h = 2.4;
        let (a, b) = (0.3, -0.2);
        let (doc, mut ber) = ceiling_test_setup(h);
        let right = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                a,
            )
            .unwrap();
        let top = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::Y,
                2.0,
                crate::LineSide::Greater,
                b,
            )
            .unwrap();
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[right, top]);

        assert_eq!(storey.ceilings.regions.len(), 4, "four quadrants");
        for z in [h, h + a, h + b, h + a + b] {
            assert!(
                (roof_area_at(&storey, z) - 4.0).abs() < 1e-6,
                "one 2×2 quadrant at z = {z}"
            );
        }
    }

    /// Stacked deltas that drive a piece's ceiling to z ≤ 0.1 m are ACCEPTED
    /// (warn-logged, not rejected) — single-delta validation happens at assign.
    #[test]
    fn composed_ceiling_z_at_or_below_floor_is_accepted() {
        let (doc, mut ber) = ceiling_test_setup(2.4);
        let step = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                -2.4,
            )
            .unwrap();
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[step]);
        assert!(
            (roof_area_at(&storey, 0.0) - 8.0).abs() < 1e-6,
            "the z = 0 piece survives (warned, not dropped)"
        );
    }

    // ── Roof apexes (fans) + roof-aware walls ───────────────────────────────

    use ber_model::ber::structural::CeilingApex;

    /// Multiset fingerprint of the roof SURFACE: per region `(plan area,
    /// min z, max z)`, sorted — robust to region order and to collinear
    /// vertices boolean ops insert.
    fn roof_surface_multiset(storey: &Storey) -> Vec<(f64, f64, f64)> {
        let mut v: Vec<(f64, f64, f64)> = storey
            .ceilings
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

    /// While the ceiling height is unset there is no default ceiling to fan:
    /// apexes are IGNORED (ceilings untouched, walls flat-provisional).
    #[test]
    fn apexes_ignored_while_height_unset() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.storeys[0].ceiling_apexes = vec![CeilingApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.0,
        }];
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert!(storey.ceilings.regions.is_empty(), "no height → no ceilings");
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
        ber.storeys[0].ceiling_apexes = vec![CeilingApex {
            point: Point2f64::new(5.0, 2.0), // outside the 4×4 footprint
            z_m: 4.0,
        }];
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(storey.ceilings.regions.len(), 1, "flat — the fan is dormant");
        assert!((roof_area_at(&storey, 2.4) - 16.0).abs() < 1e-6);
        assert_eq!(storey.ceiling_apexes.len(), 1, "the apex is kept");
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
        let len = manip_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);

        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(ber.storeys[0].ceilings.regions.len(), 1);
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
        // Hang a REAL door on the wall — an amend must not orphan it (`opes`
        // is derived from the placement).
        let door_id = ber_model::ber::openings::add_wall_door(
            &mut ber.storeys[0],
            id0,
            2.0,
            0.9,
            2.0,
            ber_model::ber::fenestration::DoorKind::Solid,
            "door".to_string(),
            0.0,
        )
        .unwrap();

        // A step at x = 2 (+0.3) crosses every horizontal wall → re-resolve.
        let step = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey2 = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[step.clone()]);

        // 4 perimeter regions + the step's ONE interior riser face.
        assert_eq!(storey2.walls.regions.len(), 5);
        // The riser: base edge ON the step line (x = 2, interior), spanning
        // the two ceiling heights, Unset (the riser default, like any wall).
        let is_riser = |r: &&ber_model::ber::structural::WallRegion| {
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
        let _riser_id = riser.id;
        let mut ber2 = ber.clone();
        ber2.storeys[0] = storey2.clone();
        let storey3 = resolve_storey(&doc, &ber2, 0, &[], &[], &[step]);
        storey3
            .walls
            .regions
            .iter()
            .find(|r| is_riser(r))
            .expect("riser re-derives");
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
        let len = manip_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);

        // Flat resolve first — capture the marked wall's identity (the full
        // perimeter resolves; select by exposure, not index).
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let _id0 = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::Exposed)
            .unwrap()
            .id;

        // A ridge along x = 2 (apexes (2,1) and (2,3) at 4.2) → re-resolve.
        for y in [1.0, 3.0] {
            ber.add_ceiling_apex(
                0,
                CeilingApex {
                    point: Point2f64::new(2.0, y),
                    z_m: 4.2,
                },
            );
        }
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[]);
        assert_eq!(storey.walls.regions.len(), 4);
        let wall = storey
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::Exposed)
            .unwrap();
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
        use ber_model::ber::structural::WallRegions;
        let wall_cid = ber
            .wall_construction_id(Default::default(), Default::default(), Default::default())
            .unwrap();
        let walls = WallRegions::regenerate(
            vec![(
                Point2f64::new(0.0, 2.0),
                Point2f64::new(4.0, 2.0),
                WallExposure::Exposed,
            )],
            &storey.ceilings,
            &FloorRegions::default(),
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
            let len = manip_plan::segment_len(a, b);
            doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);
        }
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        // Raise the jut: step at x = 4, Greater side, +1.0 → jut ceiling 3.4.
        let step = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                4.0,
                crate::LineSide::Greater,
                1.0,
            )
            .unwrap();
        // Ridge over the raised jut along y = 1: hip end inside, gable end
        // ON the east perimeter wall.
        for apex in [
            CeilingApex {
                point: Point2f64::new(4.5, 1.0),
                z_m: 4.4,
            },
            CeilingApex {
                point: Point2f64::new(6.0, 1.0),
                z_m: 4.4,
            },
        ] {
            ber.add_ceiling_apex(0, apex);
        }
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[step]);
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
        let tops = |r: &ber_model::ber::structural::WallRegion| -> Vec<domain::points::Point3f64> {
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
        let len = manip_plan::segment_len(a, b);
        // Paint the middle SemiExposed → ONE stored section; the rest of the
        // wall is Unset coverage (single representation).
        doc.set_wall_section(vid, 1.0, 3.0, len, WallExposure::SemiExposed);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        // A step crossing the wall AND an apex fanning the stepped piece —
        // every section's top is now non-trivial (slopes and/or jumps).
        ber.storeys[0].ceiling_apexes = vec![CeilingApex {
            point: Point2f64::new(3.0, 2.0),
            z_m: 4.2,
        }];
        let step = doc
            .ceiling_step_region(
                manip_plan::ParameterAxis::X,
                2.0,
                crate::LineSide::Greater,
                0.3,
            )
            .unwrap();
        let storey = resolve_storey_t(&doc, &mut ber, 0, &[], &[], &[step]);

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

    /// REGRESSION (floor-step base, 2026-06): exposure recovery must read the
    /// FULL footprint base edge (`base_frame`), NOT raw `exterior[0]/[1]`. A
    /// perimeter wall painted end-to-end and CROSSED by a floor step gets a
    /// stepped base, so `exterior[1]` is the step breakpoint — reading it raw
    /// recovered the section as `0..step` and silently dropped the paint
    /// beyond the step line on reload. Recovery must return the full painted
    /// length.
    #[test]
    fn sections_recover_full_length_over_a_floor_step_crossed_wall() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        // The SOUTH wall (on y = 0) runs across the step line x = 2, so its
        // base steps; paint it end-to-end → one stored section 0..len.
        let (vid, a, b) = doc
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(_, a, b)| a.y.abs() < 1e-6 && b.y.abs() < 1e-6)
            .expect("south wall on y = 0");
        let len = manip_plan::segment_len(a, b);
        doc.set_wall_section(vid, 0.0, len, len, WallExposure::Exposed);

        // A floor step over the right half (x ∈ [2, 4]) crosses the south wall.
        let steps = vec![crate::StepRegion {
            polygon: vec![
                Point2f32::new(2.0, 0.0),
                Point2f32::new(4.0, 0.0),
                Point2f32::new(4.0, 4.0),
                Point2f32::new(2.0, 4.0),
            ],
            level_m: 0.18,
        }];
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        let storey = resolve_storey(&doc, &ber, 0, &[], &steps, &[]);

        // The painted south wall really did get a stepped base (flat-top,
        // flat-base = 4 vertices; the step adds the vertical jump → more).
        let south = storey
            .walls
            .regions
            .iter()
            .find(|r| r.exposure == WallExposure::Exposed)
            .expect("the painted south wall resolves");
        assert!(
            south.polygon.exterior.len() > 4,
            "south wall base is stepped (jump at x = 2): {:?}",
            south.polygon.exterior
        );
        assert!(
            south
                .polygon
                .exterior
                .iter()
                .any(|v| (v.z - 0.18).abs() < 1e-9),
            "the raised half of the base sits at the step level: {:?}",
            south.polygon.exterior
        );

        // Recovery returns the FULL painted length, not 0..step.
        let recovered = doc.sections_from_wall_regions(&storey.walls);
        let secs = recovered.get(&vid).expect("the painted wall recovers");
        assert_eq!(secs.len(), 1);
        assert!(secs[0].start_m.abs() < 1e-3, "start at 0: {:?}", secs[0]);
        assert!(
            (secs[0].end_m - len).abs() < 1e-3,
            "FULL painted length recovered, not truncated at the step: \
             {:?} (len {len})",
            secs[0]
        );
        assert_eq!(secs[0].exposure, WallExposure::Exposed);
    }

    /// A wall window placed on a resolved storey survives a RE-RESOLVE: its
    /// derived `vertices` are recomputed from the durable placement and the
    /// wall's `opes` reference is preserved by the amend — the openings tail of
    /// `resolve_storey`.
    #[test]
    fn placed_window_derives_vertices_and_survives_re_resolve() {
        use ber_model::ber::fenestration::{OpeningPlacement, Window};
        let doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        // First resolve to mint wall ids.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let wall = &ber.storeys[0].walls.regions[0];
        let wall_id = wall.id;
        let (_, _, len, _) = wall.base_frame().unwrap();
        // Place a 1 m × 1.2 m window centred on that wall, sill 1 m, and link it.
        let win_id = ItemId::default();
        ber.storeys[0].windows.push(Window {
            id: win_id,
            vertices: vec![],
            width_m: 1.0,
            height_m: 1.2,
            window_type_id: ItemId::default(),
            placement: Some(OpeningPlacement::Wall {
                wall_id,
                along_m: (len / 2.0) as f32,
                sill_m: 1.0,
            }),
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            arched: false,
            overshading: Default::default(),
        });
        let widx = ber.storeys[0]
            .walls
            .regions
            .iter()
            .position(|r| r.id == wall_id)
            .unwrap();
        ber.storeys[0].walls.regions[widx].opes.windows.push(win_id);

        // Re-resolve: the tail derives vertices; the amend keeps the opes ref.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let win = &ber.storeys[0].windows[0];
        assert_eq!(win.vertices.len(), 4, "vertices derived");
        assert!(
            win.vertices.iter().any(|v| (v.2 - 1.0).abs() < 1e-3),
            "sill at z = 1.0: {:?}",
            win.vertices
        );
        assert!(
            win.vertices.iter().any(|v| (v.2 - 2.2).abs() < 1e-3),
            "top at z = 2.2 (under the 2.4 ceiling): {:?}",
            win.vertices
        );
        let host_id = match ber.storeys[0].windows[0].placement {
            Some(OpeningPlacement::Wall { wall_id, .. }) => wall_id,
            _ => panic!("still a wall placement"),
        };
        let host = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| r.id == host_id)
            .expect("the window re-homed onto a live wall");
        assert!(
            host.opes.windows.contains(&win_id),
            "the opes index links the (re-derived) host wall"
        );
    }

    /// A wall window survives an outline RESHAPE: the move breaks the base-edge
    /// amend (the wall gets a fresh id), but the durable parametric `wall_anchor`
    /// — seeded on the prior resolve — re-points the opening onto the
    /// regenerated wall instead of letting the orphan-prune drop it.
    #[test]
    fn placed_window_survives_outline_reshape_via_anchor() {
        use ber_model::ber::fenestration::{OpeningPlacement, Window};
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        let wall = &ber.storeys[0].walls.regions[0];
        let wall_id = wall.id;
        let (_, _, len, _) = wall.base_frame().unwrap();
        let win_id = ItemId::default();
        ber.storeys[0].windows.push(Window {
            id: win_id,
            vertices: vec![],
            width_m: 1.0,
            height_m: 1.2,
            window_type_id: ItemId::default(),
            placement: Some(OpeningPlacement::Wall {
                wall_id,
                along_m: (len / 2.0) as f32,
                sill_m: 1.0,
            }),
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            arched: false,
            overshading: Default::default(),
        });

        // Resolve once to SEED the parametric anchor from the (still live) wall.
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert!(
            ber.storeys[0].windows[0].wall_anchor.is_some(),
            "anchor seeded from the live host wall"
        );

        // RESHAPE: translate the whole outline far past the amend tolerance, so
        // every wall is regenerated with a FRESH id (the window's stored wall_id
        // goes stale). VertexIds are invariant under translation, so the anchor
        // still names the same perimeter wall.
        doc.outline.floor_plan.translate_all(5.0, 5.0);
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        assert_eq!(
            ber.storeys[0].windows.len(),
            1,
            "the window survived the reshape (not orphan-pruned)"
        );
        let win = &ber.storeys[0].windows[0];
        let Some(OpeningPlacement::Wall {
            wall_id: new_id, ..
        }) = win.placement
        else {
            panic!("still a wall placement");
        };
        assert!(
            ber.storeys[0].walls.regions.iter().any(|r| r.id == new_id),
            "re-pointed onto a live regenerated wall"
        );
        assert_eq!(win.vertices.len(), 4, "vertices re-derived on the new wall");
        let host = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|r| r.id == new_id)
            .unwrap();
        assert!(
            host.opes.windows.contains(&win_id),
            "the opes index links the re-pointed wall"
        );
    }

    /// The signed planar area of a (possibly non-axis-aligned) vertical wall
    /// polygon: project to its own (along, z) frame using the first base edge
    /// direction and shoelace. Enough for "the two halves sum to the whole".
    fn wall_face_area(poly: &Polygon3D) -> f64 {
        let e = &poly.exterior;
        if e.len() < 3 {
            return 0.0;
        }
        // Along-axis from the first edge that has plan extent.
        let a = e[0];
        let (mut dx, mut dy) = (0.0, 0.0);
        for v in &e[1..] {
            let (cx, cy) = (v.x - a.x, v.y - a.y);
            if cx.hypot(cy) > 1e-9 {
                let l = cx.hypot(cy);
                dx = cx / l;
                dy = cy / l;
                break;
            }
        }
        let pts: Vec<(f64, f64)> = e
            .iter()
            .map(|v| ((v.x - a.x) * dx + (v.y - a.y) * dy, v.z))
            .collect();
        let mut acc = 0.0;
        for i in 0..pts.len() {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[(i + 1) % pts.len()];
            acc += x0 * y1 - x1 * y0;
        }
        (acc * 0.5).abs()
    }

    /// A face-partitioned perimeter wall (one diagonal cut, two region samples)
    /// resolves to TWO wall regions carrying the painted exposures; the other
    /// three walls stay one Unset region each, and the two sub-faces' planar
    /// areas sum to the original full-face area.
    #[test]
    fn face_partition_diagonal_splits_one_wall_into_two_exposures() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_plan::segment_len(a, b);
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        let height = 2.4_f32;
        let full_area = (len * height) as f64;

        // A corner-to-corner diagonal splits the elevation into two triangles.
        doc.commit_partition_cut(
            vid,
            crate::PartitionCut {
                a: Point2f32::new(0.0, 0.0),
                b: Point2f32::new(len, height),
            },
        );
        // Below the diagonal (large along, small height) → Exposed;
        // above (small along, large height) → SemiExposed.
        doc.set_partition_region_exposure(
            vid,
            Point2f32::new(len * 0.75, height * 0.15),
            WallExposure::Exposed,
            len,
            height,
        );
        doc.set_partition_region_exposure(
            vid,
            Point2f32::new(len * 0.25, height * 0.85),
            WallExposure::SemiExposed,
            len,
            height,
        );

        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        // 3 untouched walls (one region each) + the partitioned wall as 2.
        assert_eq!(storey.walls.regions.len(), 5, "3 + 2 = 5 wall regions");
        let by = |e: WallExposure| {
            storey
                .walls
                .regions
                .iter()
                .filter(move |r| r.exposure == e)
                .count()
        };
        assert_eq!(by(WallExposure::Exposed), 1);
        assert_eq!(by(WallExposure::SemiExposed), 1);
        assert_eq!(by(WallExposure::Unset), 3, "the other three walls");

        // The two sub-faces of the partitioned wall sum to its full face.
        let sub_area: f64 = storey
            .walls
            .regions
            .iter()
            .filter(|r| {
                matches!(
                    r.exposure,
                    WallExposure::Exposed | WallExposure::SemiExposed
                )
            })
            .map(|r| wall_face_area(&r.polygon))
            .sum();
        assert!(
            (sub_area - full_area).abs() < 1e-3,
            "two halves ({sub_area}) sum to the full face ({full_area})"
        );
        let _ = (a, b);
    }

    /// LAYER-1 ROUND-TRIP: a face partition survives a snapshot-less reload.
    /// Resolve stamps the recipe onto `Storey` (Layer 1); serialize ONLY the
    /// `Ber` (no `Document` snapshot), deserialize, rebuild the editor with
    /// `from_storey`, and reconstruct the working map — the partition returns on
    /// the matching wall with its cuts + exposures intact. The old snapshot-only
    /// design could NOT pass this: the recipe was lost on a Layer-1-only load.
    #[test]
    fn face_partition_survives_layer1_only_roundtrip() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, a, b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_plan::segment_len(a, b);
        let height = 2.4_f32;
        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);

        doc.commit_partition_cut(
            vid,
            crate::PartitionCut {
                a: Point2f32::new(0.0, 0.0),
                b: Point2f32::new(len, height),
            },
        );
        doc.set_partition_region_exposure(
            vid,
            Point2f32::new(len * 0.75, height * 0.15),
            WallExposure::Exposed,
            len,
            height,
        );

        // Resolve → the recipe is stamped onto Layer 1 (the durable Storey).
        ber.storeys[0] = resolve_storey(&doc, &ber, 0, &[], &[], &[]);
        assert_eq!(
            ber.storeys[0].wall_face_partitions.len(),
            1,
            "resolve stamps the durable recipe onto Storey"
        );

        // Serialize ONLY the Ber (no Document snapshot), then deserialize.
        let json = serde_json::to_string(&ber).unwrap();
        let ber2: Ber = serde_json::from_str(&json).unwrap();

        // Rebuild the editor from Layer 1 and reconstruct the working map.
        let new_doc = StoreyDoc::from_storey(&ber2.storeys[0]);
        let reconstructed = new_doc.wall_face_partitions_from_storey(&ber2.storeys[0]);

        let new_vid = new_doc.outline.floor_plan.perimeter_walls()[0].0;
        assert_eq!(reconstructed.len(), 1, "one partition reconstructed");
        assert_eq!(
            reconstructed.get(&new_vid),
            doc.wall_face_partitions.get(&vid),
            "cuts + exposures intact on the matching wall after a Layer-1-only reload"
        );
        let _ = (a, b);
    }

    /// A partitioned wall with NO cuts + a single Ground sample resolves to ONE
    /// region carrying Ground (no split, just exposure attribution).
    #[test]
    fn face_partition_no_cuts_one_sample_one_ground_region() {
        let mut doc = square_doc(&[
            Point2f32::new(0.0, 0.0),
            Point2f32::new(4.0, 0.0),
            Point2f32::new(4.0, 4.0),
            Point2f32::new(0.0, 4.0),
        ]);
        let (vid, _a, _b) = doc.outline.floor_plan.perimeter_walls()[0];
        let len = manip_plan::segment_len(vid_edge(&doc, vid).0, vid_edge(&doc, vid).1);
        let height = 2.4_f32;
        // Create the partition entry with no cuts, then attribute one sample.
        doc.wall_face_partitions.entry(vid).or_default();
        doc.set_partition_region_exposure(
            vid,
            Point2f32::new(len * 0.5, height * 0.5),
            WallExposure::Ground,
            len,
            height,
        );
        assert!(doc.is_wall_partitioned(vid));
        assert!(doc.wall_face_partitions[&vid].cuts.is_empty());

        let mut ber = Ber::default();
        ensure_storey_constructions(&mut ber, 0, &[]);
        ber.set_default_ceiling_height(Some(0), 2.4);
        let storey = resolve_storey(&doc, &ber, 0, &[], &[], &[]);

        assert_eq!(storey.walls.regions.len(), 4, "no split — still four walls");
        assert_eq!(
            storey
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure == WallExposure::Ground)
                .count(),
            1,
            "the no-cut partitioned wall carries its single Ground sample"
        );
        assert_eq!(
            storey
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure == WallExposure::Unset)
                .count(),
            3
        );
    }

    /// The perimeter edge endpoints for `vid`.
    fn vid_edge(doc: &StoreyDoc, vid: manip_plan::VertexId) -> (Point2f32, Point2f32) {
        let (_, a, b) = doc
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(v, _, _)| *v == vid)
            .unwrap();
        (a, b)
    }
}
