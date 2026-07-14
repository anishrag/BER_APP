// src/data/structural.rs
//! Building-structure data model: constructions, floor/ceiling/wall
//! regions, 3D polygon primitives, the roof-loft geometry kernel, and
//! the Storey container. Split from a single file for navigability;
//! the public surface is unchanged via the re-exports below.

mod ceiling_geom;
mod ceiling_regions;
mod constructions;
mod elevation;
mod floor_regions;
mod inter_storey;
mod roof_voids;
mod storey;
mod wall_partition;
mod wall_regions;

pub use ceiling_geom::*;
pub use ceiling_regions::*;
pub use constructions::*;
pub use elevation::*;
pub use floor_regions::*;
pub use inter_storey::*;
pub use roof_voids::*;
pub use storey::*;
pub use wall_partition::*;
pub use wall_regions::*;

// The 3D polygon primitive lives in `manip_plan` (the geometry crate) and is
// re-exported here so serialized region storage and the existing
// `ber_model::ber::structural::Polygon3D` paths are unchanged.
pub use manip_plan::{Polygon3D, SplitLine3D};

// The test module below reaches external names (domain points, BER common ids,
// geo + serde types) through `use super::*`; in the pre-split single file these
// were top-level private imports. Re-provide them at the root, test-only.
#[cfg(test)]
#[allow(unused_imports)]
use {
    crate::ber::ber_struct::Ber,
    crate::ber::common::{ConstructionPhase, Item, ItemId, Photo},
    crate::ber::fenestration::{Door, Window},
    domain::points::{
        point_on_ring_boundary, point_to_segment_distance, ring_centroid, segment_cross_param,
        Point2f64, Point3f64, Vector3f64,
    },
    geo::{Contains, Coord, LineString, MultiPolygon, Point as GeoPoint, Polygon},
    serde::{Deserialize, Serialize},
};

#[cfg(test)]
mod tests {
    use super::*;

    fn square(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(x0, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0, y1),
        ]
    }

    // ── CeilingRegion::plan_span_through ────────────────────────────────────

    /// A flat region over `ring` at z = 2.4 (the span is plan-only; z is
    /// irrelevant to it).
    fn region_over(ring: Vec<Point2f64>) -> CeilingRegion {
        CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(
                ring.into_iter()
                    .map(|p| Point3f64::new(p.x, p.y, 2.4))
                    .collect(),
            ),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        }
    }

    /// A 10×4 ring with a deep SLOT cut from the right (y ∈ [1, 3], reaching
    /// in to x = 3) — a C-shape (the ceiling_point notched ring).
    fn notched_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 1.0),
            Point2f64::new(3.0, 1.0),
            Point2f64::new(3.0, 3.0),
            Point2f64::new(10.0, 3.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ]
    }

    /// A rectangle yields the full chord through the point, on both axes.
    #[test]
    fn plan_span_through_rectangle_is_the_full_chord() {
        let r = region_over(square(0.0, 0.0, 10.0, 4.0));
        let (lo, hi) = r
            .plan_span_through(Point2f64::new(3.0, 2.0), Point2f64::new(1.0, 0.0))
            .expect("strictly inside");
        assert!(
            (lo + 3.0).abs() < 1e-9 && (hi - 7.0).abs() < 1e-9,
            "{lo} {hi}"
        );
        let (lo, hi) = r
            .plan_span_through(Point2f64::new(3.0, 2.0), Point2f64::new(0.0, 1.0))
            .expect("strictly inside");
        assert!(
            (lo + 2.0).abs() < 1e-9 && (hi - 2.0).abs() < 1e-9,
            "{lo} {hi}"
        );
    }

    /// A notched ring limits the span to the contiguous run CONTAINING t = 0,
    /// not the even-odd union of runs along the whole line.
    #[test]
    fn plan_span_through_notch_is_the_interval_containing_zero() {
        let r = region_over(notched_ring());
        // From (1, 2) along +X the left arm ends at the slot's near edge
        // (x = 3) — NOT at the ring's x = 10 extent.
        let (lo, hi) = r
            .plan_span_through(Point2f64::new(1.0, 2.0), Point2f64::new(1.0, 0.0))
            .expect("strictly inside the left arm");
        assert!(
            (lo + 1.0).abs() < 1e-9 && (hi - 2.0).abs() < 1e-9,
            "{lo} {hi}"
        );
        // From (5, 0.5) along +Y the bottom arm ends at the slot (y = 1).
        let (lo, hi) = r
            .plan_span_through(Point2f64::new(5.0, 0.5), Point2f64::new(0.0, 1.0))
            .expect("strictly inside the bottom arm");
        assert!(
            (lo + 0.5).abs() < 1e-9 && (hi - 0.5).abs() < 1e-9,
            "{lo} {hi}"
        );
    }

    /// Containment is STRICT: a boundary or outside point yields None.
    #[test]
    fn plan_span_through_boundary_or_outside_is_none() {
        let r = region_over(square(0.0, 0.0, 10.0, 4.0));
        for p in [
            Point2f64::new(0.0, 2.0),  // on the left edge
            Point2f64::new(10.0, 4.0), // a corner
            Point2f64::new(11.0, 2.0), // outside
        ] {
            assert!(
                r.plan_span_through(p, Point2f64::new(1.0, 0.0)).is_none(),
                "{p:?}"
            );
        }
    }

    /// The boundary-tolerant variant DOES yield a span for a point ON an edge
    /// when the line runs along that edge (a mono-pitch ridge on a wall): the
    /// perpendicular nudge into the interior recovers the chord, measured from
    /// the same along-edge origin. A point genuinely outside still yields None.
    #[test]
    fn plan_span_through_inclusive_recovers_an_on_edge_span() {
        let r = region_over(square(0.0, 0.0, 10.0, 4.0));
        // On the left edge x = 0, running UP the edge (dir +y): the chord is the
        // edge itself, y in [0, 4], so from y = 2 the span is [-2, 2].
        let (lo, hi) = r
            .plan_span_through_inclusive(Point2f64::new(0.0, 2.0), Point2f64::new(0.0, 1.0))
            .expect("a span along the edge");
        assert!(
            (lo - -2.0).abs() < 0.01 && (hi - 2.0).abs() < 0.01,
            "{lo} {hi}"
        );
        // Outside is still None.
        assert!(r
            .plan_span_through_inclusive(Point2f64::new(11.0, 2.0), Point2f64::new(0.0, 1.0))
            .is_none());
    }

    /// Degenerate input — a sub-triangle ring or a zero direction — is None.
    #[test]
    fn plan_span_through_degenerate_is_none() {
        let two = region_over(vec![Point2f64::new(0.0, 0.0), Point2f64::new(4.0, 0.0)]);
        assert!(two
            .plan_span_through(Point2f64::new(1.0, 0.0), Point2f64::new(1.0, 0.0))
            .is_none());
        let r = region_over(square(0.0, 0.0, 10.0, 4.0));
        assert!(r
            .plan_span_through(Point2f64::new(3.0, 2.0), Point2f64::new(0.0, 0.0))
            .is_none());
    }

    #[test]
    fn partition_with_no_areas_is_one_region() {
        let outline = square(0.0, 0.0, 10.0, 10.0);
        let rem = ItemId::default();
        let fr = FloorRegions::partition(&outline, &[], rem);
        assert_eq!(fr.regions.len(), 1);
        assert_eq!(fr.regions[0].construction_id, rem);
    }

    #[test]
    fn partition_splits_outline_into_area_and_remainder() {
        use geo::Area;
        let outline = square(0.0, 0.0, 10.0, 10.0);
        let area_cid = ItemId::default();
        let rem_cid = ItemId::default();
        // Right half [5,10] tagged.
        let fr = FloorRegions::partition(
            &outline,
            &[(square(5.0, 0.0, 10.0, 10.0), area_cid)],
            rem_cid,
        );
        assert_eq!(fr.regions.len(), 2, "area piece + remainder");
        let area_region = fr
            .regions
            .iter()
            .find(|r| r.construction_id == area_cid)
            .expect("tagged piece exists");
        let rem_region = fr
            .regions
            .iter()
            .find(|r| r.construction_id == rem_cid)
            .expect("remainder exists");
        assert!((area_region.polygon.unsigned_area() - 50.0).abs() < 1e-6);
        assert!((rem_region.polygon.unsigned_area() - 50.0).abs() < 1e-6);
    }

    #[test]
    fn partition_second_area_abuts_the_first() {
        let outline = square(0.0, 0.0, 10.0, 10.0);
        let a = ItemId::default();
        let b = ItemId::default();
        let rem = ItemId::default();
        // Two OVERLAPPING areas; the second yields to the first in the overlap.
        let fr = FloorRegions::partition(
            &outline,
            &[
                (square(0.0, 0.0, 6.0, 10.0), a),  // left 0..6
                (square(4.0, 0.0, 10.0, 10.0), b), // right 4..10 (overlaps 4..6)
            ],
            rem,
        );
        use geo::Area;
        let area_a: f64 = fr
            .regions
            .iter()
            .filter(|r| r.construction_id == a)
            .map(|r| r.polygon.unsigned_area())
            .sum();
        let area_b: f64 = fr
            .regions
            .iter()
            .filter(|r| r.construction_id == b)
            .map(|r| r.polygon.unsigned_area())
            .sum();
        assert!(
            (area_a - 60.0).abs() < 1e-6,
            "first area keeps its full 0..6"
        );
        assert!(
            (area_b - 40.0).abs() < 1e-6,
            "second yields the 4..6 overlap → 6..10 only"
        );
    }

    #[test]
    fn with_levels_splits_region_into_stepped_piece_and_remainder() {
        use geo::Area;
        let cid = ItemId::default();
        let fr = FloorRegions::new(square(0.0, 0.0, 10.0, 10.0), cid);
        // Step the right half down 0.3 m.
        let stepped = fr.with_levels(&[(square(5.0, 0.0, 10.0, 10.0), -0.3)]);
        assert_eq!(stepped.regions.len(), 2, "stepped piece + remainder");
        let step = stepped
            .regions
            .iter()
            .find(|r| r.level_m == -0.3)
            .expect("stepped piece exists");
        let rem = stepped
            .regions
            .iter()
            .find(|r| r.level_m == 0.0)
            .expect("remainder exists");
        assert!((step.polygon.unsigned_area() - 50.0).abs() < 1e-6);
        assert!((rem.polygon.unsigned_area() - 50.0).abs() < 1e-6);
        // Pieces inherit the region's fabric link.
        for r in &stepped.regions {
            assert_eq!(r.construction_id, cid);
        }
    }

    #[test]
    fn with_splits_divides_region_copying_all_properties_with_fresh_ids() {
        use geo::Area;
        let cid = ItemId::default();
        let mut fr = FloorRegions::new(square(0.0, 0.0, 10.0, 8.0), cid);
        fr.regions[0].level_m = 0.3; // a raised floor — both halves must keep it
        let parent_id = fr.regions[0].id;
        // A structure split cutting the right half off [5,10].
        let split = fr.with_splits(&[square(5.0, 0.0, 10.0, 8.0)]);
        assert_eq!(split.regions.len(), 2, "one cut → two pieces");
        for r in &split.regions {
            assert_eq!(r.construction_id, cid, "construction copied to both sides");
            assert!(
                (r.level_m - 0.3).abs() < 1e-9,
                "level_m copied to both sides"
            );
            assert_ne!(r.id, parent_id, "each piece gets a fresh id");
            assert!(
                (r.polygon.unsigned_area() - 40.0).abs() < 1e-6,
                "two 5×8 halves"
            );
        }
        let ids: std::collections::HashSet<_> = split.regions.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2, "the two pieces have distinct ids");
    }

    #[test]
    fn with_splits_empty_is_a_noop_preserving_identity() {
        let cid = ItemId::default();
        let fr = FloorRegions::new(square(0.0, 0.0, 10.0, 8.0), cid);
        let id = fr.regions[0].id;
        let out = fr.with_splits(&[]);
        assert_eq!(out.regions.len(), 1);
        assert_eq!(
            out.regions[0].id, id,
            "empty cuts → untouched, id preserved"
        );
    }

    #[test]
    fn with_splits_missing_region_emits_it_whole() {
        // A cut that doesn't reach the region leaves it intact (re-id'd), never lost.
        let cid = ItemId::default();
        let fr = FloorRegions::new(square(0.0, 0.0, 4.0, 4.0), cid);
        let out = fr.with_splits(&[square(10.0, 10.0, 12.0, 12.0)]);
        assert_eq!(out.regions.len(), 1, "untouched region survives the miss");
    }

    #[test]
    fn with_levels_empty_areas_is_a_noop_with_exact_points() {
        let cid = ItemId::default();
        // Deliberately awkward ring (collinear vertex) — geo ops would normalise
        // it; an exact round-trip proves none ran.
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0), // collinear with neighbours
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ];
        let fr = FloorRegions::new(ring.clone(), cid);
        let before: Vec<Vec<Point2f64>> = fr.to_vertex_lists();
        let before_id = fr.regions[0].id;
        let out = fr.with_levels(&[]);
        assert_eq!(out.to_vertex_lists(), before, "points round-trip exactly");
        assert_eq!(out.regions[0].id, before_id, "region untouched");
    }

    #[test]
    fn floor_region_without_level_m_deserializes_to_zero() {
        // JSON produced by the OLD shape (no `level_m` field): serialize the
        // current shape and strip the field.
        let region = FloorRegions::new(square(0.0, 0.0, 10.0, 10.0), ItemId::default())
            .regions
            .remove(0);
        let mut json = serde_json::to_value(&region).expect("serialize");
        assert!(json.as_object_mut().unwrap().remove("level_m").is_some());
        let old: FloorRegion = serde_json::from_value(json).expect("old JSON still loads");
        assert_eq!(old.level_m, 0.0);
        assert_eq!(old.construction_id, region.construction_id);
    }

    #[test]
    fn with_levels_earlier_area_claims_overlap() {
        use geo::Area;
        let fr = FloorRegions::new(square(0.0, 0.0, 10.0, 10.0), ItemId::default());
        // Two OVERLAPPING step areas; the second yields to the first in 4..6.
        let stepped = fr.with_levels(&[
            (square(0.0, 0.0, 6.0, 10.0), 0.5),   // left 0..6, raised
            (square(4.0, 0.0, 10.0, 10.0), -0.2), // right 4..10 (overlaps 4..6)
        ]);
        let area_at = |level: f64| -> f64 {
            stepped
                .regions
                .iter()
                .filter(|r| r.level_m == level)
                .map(|r| r.polygon.unsigned_area())
                .sum()
        };
        assert!(
            (area_at(0.5) - 60.0).abs() < 1e-6,
            "first area keeps its full 0..6"
        );
        assert!(
            (area_at(-0.2) - 40.0).abs() < 1e-6,
            "second yields the 4..6 overlap → 6..10 only"
        );
        assert!(area_at(0.0).abs() < 1e-6, "no unstepped remainder left");
    }

    /// FLAT PARITY with the old `rebuild_preserving`: empty ceilings → a fresh
    /// segment becomes the classic 4-vertex vertical quad at the fallback
    /// top, with the supplied default construction.
    #[test]
    fn regenerate_with_empty_roofs_makes_flat_vertical_planes() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(4.0, 0.0);
        let default_cid = ItemId::default();
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::SemiExposed)],
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            default_cid,
        );
        assert_eq!(walls.regions.len(), 1);
        let w = &walls.regions[0];
        assert_eq!(w.exposure, WallExposure::SemiExposed);
        // Fresh segment (no covering region) takes the supplied default.
        assert_eq!(w.construction_id, default_cid);
        // 4 vertices: base edge then top edge, vertical plane 0 -> 2.4 —
        // exactly the old rebuild_preserving output.
        let ext = &w.polygon.exterior;
        assert_eq!(ext.len(), 4);
        assert_eq!((ext[0].x, ext[0].y, ext[0].z), (0.0, 0.0, 0.0));
        assert_eq!((ext[1].x, ext[1].y, ext[1].z), (4.0, 0.0, 0.0));
        assert_eq!((ext[2].x, ext[2].y, ext[2].z), (4.0, 0.0, 2.4));
        assert_eq!((ext[3].x, ext[3].y, ext[3].z), (0.0, 0.0, 2.4));
    }

    // ── Riser faces + with_risers ───────────────────────────────────────────

    /// One floor step → ONE riser quad on the interior boundary, base edge
    /// at the LOWER level (the vertex-order contract), nothing on the
    /// outline.
    #[test]
    fn floor_riser_faces_emit_on_the_interior_step_boundary_only() {
        let cid = ItemId::default();
        let floors = FloorRegions::new(square(0.0, 0.0, 4.0, 4.0), cid)
            .with_levels(&[(square(0.0, 0.0, 2.0, 4.0), -0.3)]);
        assert_eq!(floors.regions.len(), 2);
        let faces = floors.riser_faces();
        assert_eq!(faces.len(), 1, "one shared boundary piece steps");
        let ext = &faces[0].exterior;
        assert_eq!(ext.len(), 4, "a full quad");
        // Base edge at the LOWER level −0.3, top at 0.0, the whole face on
        // the interior line x = 2 (never the outline).
        assert!((ext[0].z + 0.3).abs() < 1e-9 && (ext[1].z + 0.3).abs() < 1e-9);
        assert!(ext[2].z.abs() < 1e-9 && ext[3].z.abs() < 1e-9);
        assert!(ext.iter().all(|v| (v.x - 2.0).abs() < 1e-6));
        let ys: Vec<f64> = ext.iter().map(|v| v.y).collect();
        assert!(ys.iter().any(|y| y.abs() < 1e-6));
        assert!(ys.iter().any(|y| (y - 4.0).abs() < 1e-6));
    }

    /// Two floor regions at the SAME level (a phase partition) emit nothing:
    /// the riser predicate is z-discontinuity, not adjacency.
    #[test]
    fn floor_riser_faces_skip_same_level_boundaries() {
        let cid = ItemId::default();
        let floors = FloorRegions::partition(
            &square(0.0, 0.0, 4.0, 4.0),
            &[(square(0.0, 0.0, 2.0, 4.0), ItemId::default())],
            cid,
        );
        assert_eq!(floors.regions.len(), 2);
        assert!(floors.riser_faces().is_empty());
    }

    /// A ceiling step → one face from the lower to the higher flat piece,
    /// base edge along the LOWER profile.
    #[test]
    fn ceiling_riser_faces_emit_one_face_per_step_boundary() {
        let cid = ItemId::default();
        let ceilings =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid)
                .with_z_deltas(&[(square(2.0, 0.0, 4.0, 4.0), 0.3)]);
        assert_eq!(ceilings.regions.len(), 2);
        let faces = ceilings.riser_faces();
        assert_eq!(faces.len(), 1);
        let ext = &faces[0].exterior;
        assert_eq!(ext.len(), 4);
        assert!(
            (ext[0].z - 2.4).abs() < 1e-9 && (ext[1].z - 2.4).abs() < 1e-9,
            "base edge along the lower profile"
        );
        assert!((ext[2].z - 2.7).abs() < 1e-9 && (ext[3].z - 2.7).abs() < 1e-9);
        assert!(ext.iter().all(|v| (v.x - 2.0).abs() < 1e-4));
    }

    /// Continuous fan and ridge seams (equal profiles along the shared edge)
    /// emit NOTHING — the surfaces meet; there is no vertical face.
    #[test]
    fn ceiling_riser_faces_skip_fan_and_ridge_seams() {
        let cid = ItemId::default();
        let fan = CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid)
            .with_apexes(&[CeilingApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.0,
            }]);
        assert!(fan.regions.len() >= 4, "one triangle per edge");
        assert!(fan.riser_faces().is_empty(), "fan seams are continuous");

        let (a, b) = gable_apexes();
        let gable = gable_footprint(cid).with_apexes(&[a, b]);
        assert!(gable.regions.len() >= 2);
        assert!(gable.riser_faces().is_empty(), "ridge seams are continuous");
    }

    /// CROSSING profiles split the face into two triangles at the crossing
    /// point, each with its base edge along the locally LOWER profile.
    #[test]
    fn ceiling_riser_faces_split_crossing_profiles_into_triangles() {
        let cid = ItemId::default();
        let mk = |pts: Vec<Point3f64>| CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(pts),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        // Shared edge (0,0)→(2,0): A flat at 0.5 below it, B sloped z = x/2
        // above it — the profiles cross at (1, 0, 0.5).
        let a = mk(vec![
            Point3f64::new(0.0, 0.0, 0.5),
            Point3f64::new(2.0, 0.0, 0.5),
            Point3f64::new(2.0, -1.0, 0.5),
            Point3f64::new(0.0, -1.0, 0.5),
        ]);
        let b = mk(vec![
            Point3f64::new(0.0, 0.0, 0.0),
            Point3f64::new(2.0, 0.0, 1.0),
            Point3f64::new(2.0, 1.0, 1.0),
            Point3f64::new(0.0, 1.0, 0.0),
        ]);
        let faces = CeilingRegions {
            regions: vec![a, b],
        }
        .riser_faces();
        assert_eq!(faces.len(), 2, "one triangle per side of the crossing");
        for f in &faces {
            assert_eq!(f.exterior.len(), 3);
            assert!(
                f.exterior
                    .iter()
                    .any(|v| (v.x - 1.0).abs() < 1e-9 && (v.z - 0.5).abs() < 1e-9),
                "each triangle closes at the crossing point: {:?}",
                f.exterior
            );
        }
        // Left side spans the gap 0 → 0.5 at x = 0; right side 0.5 → 1 at
        // x = 2 — and each base edge sits on the lower of the two profiles.
        assert!(faces.iter().any(|f| f
            .exterior
            .iter()
            .any(|v| v.x.abs() < 1e-9 && v.z.abs() < 1e-9)));
        assert!(faces.iter().any(|f| f
            .exterior
            .iter()
            .any(|v| (v.x - 2.0).abs() < 1e-9 && (v.z - 1.0).abs() < 1e-9)));
        for f in &faces {
            assert!(
                f.exterior[2].z >= f.exterior[0].z.max(f.exterior[1].z) - 1e-9,
                "base first, apex above: {:?}",
                f.exterior
            );
        }
    }

    // ── Coplanar-flush riser merging ────────────────────────────────────────

    /// A ring's (x, z) profile (y is constant in these fixtures), for terse
    /// merged-shape assertions.
    fn xz(p: &Polygon3D) -> Vec<(f64, f64)> {
        p.exterior.iter().map(|v| (v.x, v.z)).collect()
    }

    /// One wall region with base `(0,0)→(4,0)`, flat top at 2.4 (a quad).
    fn flat_wall(cid: ItemId) -> WallRegions {
        WallRegions::regenerate(
            vec![(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                WallExposure::Exposed,
            )],
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        )
    }

    /// Two cut lines that should COINCIDE but drift ~0.8 mm apart (a construction
    /// -phase boundary vs an exposure/partition boundary reconstructed by geo
    /// booleans on reload) must NOT spawn a sub-mm "sliver" wall between them: the
    /// near-coincident second cut is treated as grazing the first, so the wall
    /// splits ONCE (2 sub-walls), not twice (3, with a 0.8 mm sliver).
    #[test]
    fn apply_splits_drops_near_coincident_sliver_cut() {
        let cid = ItemId::default();
        // A 13 m-long wall (base (0,0)->(0,13)) split at y = 8.9270 and 8.9278.
        let wall = WallRegions::regenerate(
            vec![(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(0.0, 13.0),
                WallExposure::Exposed,
            )],
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
        let cuts = [
            [Point2f64::new(-1.0, 8.9270), Point2f64::new(1.0, 8.9270)],
            [Point2f64::new(-1.0, 8.9278), Point2f64::new(1.0, 8.9278)],
        ];
        let split = wall.apply_splits(&cuts);
        assert_eq!(
            split.regions.len(),
            2,
            "near-coincident cuts split the wall once, not twice (no sliver)"
        );
        for r in &split.regions {
            let (a, b) = r.base_edge().unwrap();
            let len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
            assert!(len > 0.01, "no sub-mm sliver sub-wall (got {len:.6} m)");
        }
    }

    /// A cut that genuinely divides the wall well away from its ends still splits
    /// it — the sliver guard only rejects sub-[`MIN_SUBWALL_M`] pieces.
    #[test]
    fn apply_splits_still_splits_a_real_cut() {
        let cid = ItemId::default();
        let wall = flat_wall(cid); // base (0,0)->(4,0)
        let split = wall.apply_splits(&[[Point2f64::new(2.0, -1.0), Point2f64::new(2.0, 1.0)]]);
        assert_eq!(split.regions.len(), 2, "a mid-wall cut still divides it");
    }

    /// A vertical quad on the wall plane y = 0 over plan x ∈ [x0, x1],
    /// z ∈ [z0, z1] — a riser face continuing the wall's plane.
    fn coplanar_face(x0: f64, x1: f64, z0: f64, z1: f64) -> Polygon3D {
        Polygon3D::new(vec![
            Point3f64::new(x0, 0.0, z0),
            Point3f64::new(x1, 0.0, z0),
            Point3f64::new(x1, 0.0, z1),
            Point3f64::new(x0, 0.0, z1),
        ])
    }

    /// THE COPLANAR-FLUSH MERGE: a riser continuing the wall's plane past
    /// its end, sharing the wall's side edge over z ∈ [1, 2.4] (a strict
    /// SUB-SEGMENT — the partial-overlap case), merges INTO the wall: one
    /// region, host id/exposure/opes kept, polygon the L-shaped union with
    /// the base edge still at indices 0/1.
    #[test]
    fn flush_coplanar_riser_merges_into_its_continuation_wall() {
        let cid = ItemId::default();
        let mut walls = flat_wall(cid);
        let wall_id = walls.regions[0].id;
        let ope = ItemId::default();
        walls.regions[0].opes.doors.push(ope);
        let walls = walls.with_risers(
            vec![(coplanar_face(4.0, 6.0, 1.0, 2.4), WallExposure::Unset)],
            cid,
        );
        assert_eq!(walls.regions.len(), 1, "the flush riser merged");
        let r = &walls.regions[0];
        assert_eq!(r.id, wall_id, "host id kept");
        assert_eq!(r.exposure, WallExposure::Exposed, "host exposure kept");
        assert_eq!(r.opes.doors, vec![ope], "host openings kept");
        assert_eq!(r.construction_id, cid, "host construction kept");
        assert_eq!(
            xz(&r.polygon),
            vec![
                (0.0, 0.0),
                (4.0, 0.0), // base edge first — the vertex-order contract
                (4.0, 1.0), // the riser's low end splits the wall's side edge
                (6.0, 1.0),
                (6.0, 2.4),
                (0.0, 2.4), // the collinear junction at (4, 2.4) dropped
            ],
            "the union ring, stitched along the shared side edge"
        );
    }

    /// A coplanar riser that touches the wall at a SINGLE POINT (no
    /// positive-length shared segment — the corner-only case) stays a
    /// separate region.
    #[test]
    fn coplanar_riser_touching_at_a_point_stays_separate() {
        let cid = ItemId::default();
        let walls = flat_wall(cid).with_risers(
            // Beside the wall and ABOVE its top: only the corner (4, 0, 2.4)
            // is shared.
            vec![(coplanar_face(4.0, 6.0, 2.4, 3.4), WallExposure::Unset)],
            cid,
        );
        assert_eq!(walls.regions.len(), 2, "a point touch never merges");
    }

    /// A NON-coplanar riser (parallel plane, 0.5 m away) never merges,
    /// whatever its z band.
    #[test]
    fn off_plane_riser_never_merges() {
        let cid = ItemId::default();
        let riser = Polygon3D::new(vec![
            Point3f64::new(4.0, 0.5, 1.0),
            Point3f64::new(6.0, 0.5, 1.0),
            Point3f64::new(6.0, 0.5, 2.4),
            Point3f64::new(4.0, 0.5, 2.4),
        ]);
        let walls = flat_wall(cid).with_risers(vec![(riser, WallExposure::Unset)], cid);
        assert_eq!(walls.regions.len(), 2, "different plane — no merge");
    }

    /// CHAINS merge transitively: riser A is flush with the wall, riser B
    /// only with A. A merges first (non-riser host preferred), the host
    /// grows, then B is flush with the grown host and merges too — one
    /// region, the seams cleaned up.
    #[test]
    fn flush_riser_chain_merges_transitively_into_the_wall() {
        let cid = ItemId::default();
        let walls = flat_wall(cid);
        let wall_id = walls.regions[0].id;
        let walls = walls.with_risers(
            vec![
                (coplanar_face(4.0, 6.0, 1.0, 2.4), WallExposure::Unset),
                (coplanar_face(6.0, 8.0, 1.0, 2.4), WallExposure::Unset),
            ],
            cid,
        );
        assert_eq!(walls.regions.len(), 1, "the whole chain merged");
        assert_eq!(walls.regions[0].id, wall_id);
        assert_eq!(
            xz(&walls.regions[0].polygon),
            vec![
                (0.0, 0.0),
                (4.0, 0.0),
                (4.0, 1.0),
                (8.0, 1.0),
                (8.0, 2.4),
                (0.0, 2.4),
            ],
            "both seam junctions dropped as collinear"
        );
    }

    /// A riser BRIDGING two coplanar walls merges into exactly ONE of them,
    /// deterministically: the lexicographically smallest base edge wins.
    /// The walls themselves never merge into each other (exposure sections
    /// stay separate regions).
    #[test]
    fn bridging_riser_prefers_the_lexicographically_smallest_wall() {
        let cid = ItemId::default();
        let walls = WallRegions::regenerate(
            vec![
                (
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    WallExposure::Exposed,
                ),
                (
                    Point2f64::new(6.0, 0.0),
                    Point2f64::new(10.0, 0.0),
                    WallExposure::SemiExposed,
                ),
            ],
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
        let (id_a, id_b) = (walls.regions[0].id, walls.regions[1].id);
        let walls = walls.with_risers(
            // Flush with wall A's right edge AND wall B's left edge.
            vec![(coplanar_face(4.0, 6.0, 1.0, 2.4), WallExposure::Unset)],
            cid,
        );
        assert_eq!(walls.regions.len(), 2, "walls never merge into each other");
        let a = walls.regions.iter().find(|r| r.id == id_a).unwrap();
        let b = walls.regions.iter().find(|r| r.id == id_b).unwrap();
        assert_eq!(
            a.polygon.exterior.len(),
            6,
            "the riser merged into the smaller base: {:?}",
            xz(&a.polygon)
        );
        assert_eq!(b.polygon.exterior.len(), 4, "the other wall is untouched");
    }

    /// Merging is IDEMPOTENT across re-derivations: re-deriving produces the
    /// identical merged region (polygon byte-identical). Ids are freshly minted
    /// each derive now (the amend is retired), so identity isn't asserted here.
    #[test]
    fn merged_wall_rederives_identically() {
        let cid = ItemId::default();
        let derive = || {
            WallRegions::regenerate(
                vec![(
                    Point2f64::new(0.0, 0.0),
                    Point2f64::new(4.0, 0.0),
                    WallExposure::Exposed,
                )],
                &CeilingRegions::default(),
                &FloorRegions::default(),
                0.0,
                2.4,
                cid,
            )
            .with_risers(
                vec![(coplanar_face(4.0, 6.0, 1.0, 2.4), WallExposure::Unset)],
                cid,
            )
        };
        let walls1 = derive();
        let walls2 = derive();
        assert_eq!(walls1.regions.len(), 1);
        assert_eq!(walls2.regions.len(), 1);
        assert_eq!(
            walls2.regions[0].polygon, walls1.regions[0].polygon,
            "the merged polygon re-derives identically"
        );
    }

    /// A ceiling step crossing the wall base makes a VERTICAL JUMP in the
    /// top: 6 vertices, with the base-edge contract at indices 0 and 1.
    #[test]
    fn regenerate_step_crossing_makes_six_vertex_jump() {
        let cid = ItemId::default();
        // Two flat roof pieces over a 10×4 footprint: z = 2.4 left of x = 5,
        // z = 2.7 right of it.
        let ceilings =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 10.0, 4.0)], 2.4, cid)
                .with_z_deltas(&[(square(5.0, 0.0, 10.0, 4.0), 0.3)]);
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &ceilings,
            &FloorRegions::default(),
            0.0,
            9.9, // fallback must NOT be used — the roof covers the base
            cid,
        );
        let ext = &walls.regions[0].polygon.exterior;
        assert_eq!(ext.len(), 6, "base 2 + tops 4 (jump at the step)");
        // CONTRACT: ext[0]/ext[1] are the base edge, a then b.
        assert_eq!((ext[0].x, ext[0].y, ext[0].z), (0.0, 0.0, 0.0));
        assert_eq!((ext[1].x, ext[1].y, ext[1].z), (10.0, 0.0, 0.0));
        // Tops trace b → a: up at 2.7 to the step, jump down to 2.4, on to a.
        let near = |v: &Point3f64, x: f64, z: f64| {
            (v.x - x).abs() < 1e-9 && v.y.abs() < 1e-9 && (v.z - z).abs() < 1e-9
        };
        assert!(near(&ext[2], 10.0, 2.7));
        assert!(near(&ext[3], 5.0, 2.7));
        assert!(near(&ext[4], 5.0, 2.4));
        assert!(near(&ext[5], 0.0, 2.4));
    }

    /// Two flat floor regions tiling a footprint at different levels — a
    /// ground level at 0 and a raised strip at 0.18. Helper for the floor-step
    /// base tests.
    fn stepped_floor(ground: Vec<Point2f64>, raised: Vec<Point2f64>, raise: f64) -> FloorRegions {
        let cid = ItemId::default();
        let mut f = FloorRegions::from_vertex_lists(vec![ground], cid);
        f.regions
            .extend(FloorRegions::from_vertex_lists(vec![raised], cid).regions);
        // Stamp the raised level on the second region.
        f.regions[1].level_m = raise;
        f
    }

    /// (b) A wall entirely over a RAISED floor region bases at that level —
    /// no vertices at the storey datum (the phantom-plane bug fixed).
    #[test]
    fn regenerate_base_follows_a_fully_raised_floor() {
        let cid = ItemId::default();
        // Raised strip x∈[3,4] at 0.18; ground x∈[0,3] at 0. The wall runs
        // ALONG the raised strip's far edge (x = 4), entirely over the raise.
        let floors = stepped_floor(square(0.0, 0.0, 3.0, 4.0), square(3.0, 0.0, 4.0, 4.0), 0.18);
        let a = Point2f64::new(4.0, 0.0);
        let b = Point2f64::new(4.0, 4.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &CeilingRegions::default(),
            &floors,
            0.0,
            2.4,
            cid,
        );
        let ext = &walls.regions[0].polygon.exterior;
        assert_eq!(ext.len(), 4, "single level → flat 4-vertex quad");
        assert!((ext[0].z - 0.18).abs() < 1e-9, "base a at the raised level");
        assert!((ext[1].z - 0.18).abs() < 1e-9, "base b at the raised level");
        assert!(
            ext.iter()
                .all(|v| (v.z - 0.18).abs() < 1e-9 || (v.z - 2.4).abs() < 1e-9),
            "base raised, top at ceiling, nothing at the datum: {ext:?}"
        );
    }

    /// (a) A wall SPANNING the step gets a stepped base with a vertical jump
    /// at the step line; the top is unchanged (flat at the fallback).
    #[test]
    fn regenerate_base_steps_across_a_floor_step() {
        let cid = ItemId::default();
        // Ground x∈[0,3] at 0; raised x∈[3,4] at 0.18. Front wall y = 0 runs
        // x: 0 → 4, crossing the step line at x = 3.
        let floors = stepped_floor(square(0.0, 0.0, 3.0, 4.0), square(3.0, 0.0, 4.0, 4.0), 0.18);
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(4.0, 0.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &CeilingRegions::default(),
            &floors,
            0.0,
            2.4,
            cid,
        );
        let ext = &walls.regions[0].polygon.exterior;
        let near = |v: &Point3f64, x: f64, z: f64| (v.x - x).abs() < 1e-9 && (v.z - z).abs() < 1e-9;
        // Base a→b: 0 at ground, jump at x=3 to 0.18, on to b.
        assert!(near(&ext[0], 0.0, 0.0), "base a at ground: {ext:?}");
        assert!(near(&ext[1], 3.0, 0.0), "ground span ends at the step line");
        assert!(near(&ext[2], 3.0, 0.18), "VERTICAL JUMP at the step line");
        assert!(near(&ext[3], 4.0, 0.18), "raised span to b");
        // Top unchanged: flat at the 2.4 fallback, b→a.
        assert!(near(&ext[4], 4.0, 2.4), "top at b: {ext:?}");
        assert!(near(&ext[5], 0.0, 2.4), "top at a");
        assert_eq!(ext.len(), 6, "4 base (with jump) + 2 flat top: {ext:?}");
        // The base edge contract: exterior[0] = a, footprint b recovered.
        let (ba, bb) = walls.regions[0]
            .base_frame()
            .map(|(o, (ux, uy), l, _)| (o, Point2f64::new(o.x + ux * l, o.y + uy * l)))
            .unwrap();
        assert!(
            (ba.x).abs() < 1e-9 && (bb.x - 4.0).abs() < 1e-9,
            "base frame spans the full footprint a→b"
        );
    }

    /// (d) BYTE-IDENTITY: with no floor step (a single-level region, or a
    /// same-level partition seam), the base collapses to the exact 2-vertex
    /// `[a, b]` — the floor-step feature is invisible to the common case.
    #[test]
    fn regenerate_single_level_base_is_byte_identical_to_flat() {
        let cid = ItemId::default();
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(4.0, 0.0);
        let segs = || vec![(a, b, WallExposure::Exposed)];
        // Reference: no floors at all (the legacy flat base).
        let flat = WallRegions::regenerate(
            segs(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
        // A SINGLE region at level 0, plus a SAME-LEVEL partition seam at x=2
        // (a phase split, not a step): the base must still collapse to [a, b].
        let floors = stepped_floor(square(0.0, 0.0, 2.0, 4.0), square(2.0, 0.0, 4.0, 4.0), 0.0);
        let stepped =
            WallRegions::regenerate(segs(), &CeilingRegions::default(), &floors, 0.0, 2.4, cid);
        assert_eq!(
            stepped.regions[0].polygon, flat.regions[0].polygon,
            "a same-level floor seam leaves the base byte-identical to flat"
        );
        assert_eq!(flat.regions[0].polygon.exterior.len(), 4, "flat quad");
    }

    /// A wall under a FAN (sloped planar roof) gets a linear sloped top —
    /// still 4 vertices, the top endpoints on the fan's plane.
    #[test]
    fn regenerate_fan_slope_makes_linear_top() {
        let cid = ItemId::default();
        // A planar sloped rectangle: z = 2.0 + 0.1·x over [0,10] × [0,4].
        let ceilings = CeilingRegions {
            regions: vec![CeilingRegion {
                id: ItemId::default(),
                polygon: Polygon3D::new(vec![
                    Point3f64::new(0.0, 0.0, 2.0),
                    Point3f64::new(10.0, 0.0, 3.0),
                    Point3f64::new(10.0, 4.0, 3.0),
                    Point3f64::new(0.0, 4.0, 2.0),
                ]),
                opes: Opes::default(),
                construction_id: cid,
                exposure: CeilingExposure::default(),
            }],
        };
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &ceilings,
            &FloorRegions::default(),
            0.0,
            9.9,
            cid,
        );
        let ext = &walls.regions[0].polygon.exterior;
        assert_eq!(ext.len(), 4, "one linear span — no spurious breakpoints");
        assert_eq!((ext[0].x, ext[0].z), (0.0, 0.0));
        assert_eq!((ext[1].x, ext[1].z), (10.0, 0.0));
        assert!((ext[2].x - 10.0).abs() < 1e-9 && (ext[2].z - 3.0).abs() < 1e-9);
        assert!(ext[3].x.abs() < 1e-9 && (ext[3].z - 2.0).abs() < 1e-9);
    }

    /// Spans of the base the ceilings DON'T cover top out at `fallback_top_z`,
    /// with a jump where coverage ends.
    #[test]
    fn regenerate_uncovered_span_uses_fallback_top() {
        let cid = ItemId::default();
        // Roof over the left half of the base only.
        let ceilings =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 5.0, 4.0)], 2.6, cid);
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &ceilings,
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
        let ext = &walls.regions[0].polygon.exterior;
        assert_eq!(ext.len(), 6, "covered span + fallback span + jump");
        let near = |v: &Point3f64, x: f64, z: f64| (v.x - x).abs() < 1e-9 && (v.z - z).abs() < 1e-9;
        assert!(near(&ext[2], 10.0, 2.4), "uncovered end at the fallback");
        assert!(near(&ext[3], 5.0, 2.4));
        assert!(near(&ext[4], 5.0, 2.6), "covered span on the roof");
        assert!(near(&ext[5], 0.0, 2.6));
    }

    #[test]
    fn default_for_footprint_stamps_height_on_every_vertex() {
        let cid = ItemId::default();
        let rings = vec![square(0.0, 0.0, 4.0, 4.0), square(6.0, 0.0, 9.0, 3.0)];
        let ceilings = CeilingRegions::default_for_footprint(rings, 2.7, cid);
        assert_eq!(ceilings.regions.len(), 2, "one region per ring");
        for r in &ceilings.regions {
            assert_eq!(r.construction_id, cid);
            assert_eq!(r.polygon.exterior.len(), 4);
            assert!(r.polygon.exterior.iter().all(|v| (v.z - 2.7).abs() < 1e-9));
        }
    }

    #[test]
    fn default_for_footprint_skips_degenerate_rings() {
        let rings = vec![
            vec![Point2f64::new(0.0, 0.0), Point2f64::new(1.0, 0.0)], // < 3 vertices
            square(0.0, 0.0, 4.0, 4.0),
        ];
        let ceilings = CeilingRegions::default_for_footprint(rings, 2.4, ItemId::default());
        assert_eq!(ceilings.regions.len(), 1, "degenerate ring skipped");
    }

    #[test]
    fn with_z_deltas_crossing_steps_compose_into_four_levels() {
        use geo::Area;
        let h = 2.4;
        let (a, b) = (0.3, -0.2);
        let cid = ItemId::default();
        // ONE call, areas in order: the vertical step (right half, `a`) runs
        // first, then the horizontal step (top half, `b`) re-splits BOTH
        // pieces the first one produced — deltas sum in the overlap.
        let ceilings =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 10.0, 10.0)], h, cid)
                .with_z_deltas(&[
                    (square(5.0, 0.0, 10.0, 10.0), a), // vertical line, right half
                    (square(0.0, 5.0, 10.0, 10.0), b), // horizontal line, top half
                ]);
        assert_eq!(
            ceilings.regions.len(),
            4,
            "two crossing steps → 4 quadrants"
        );
        let area_at = |z: f64| -> f64 {
            ceilings
                .regions
                .iter()
                .filter(|r| r.flat_z().is_some_and(|rz| (rz - z).abs() < 1e-9))
                .map(|r| Polygon::<f64>::from(r.polygon.clone()).unsigned_area())
                .sum()
        };
        for z in [h, h + a, h + b, h + a + b] {
            assert!(
                (area_at(z) - 25.0).abs() < 1e-6,
                "one 25 m² quadrant at z = {z}"
            );
        }
        // Pieces inherit the fabric link.
        for r in &ceilings.regions {
            assert_eq!(r.construction_id, cid);
        }
    }

    #[test]
    fn with_z_deltas_empty_areas_is_a_noop_with_exact_points() {
        // Deliberately awkward ring (collinear vertex) — geo ops would normalise
        // it; an exact round-trip proves none ran.
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0), // collinear with neighbours
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ];
        let ceilings = CeilingRegions::default_for_footprint(vec![ring], 2.7, ItemId::default());
        let before = ceilings.regions[0].polygon.clone();
        let before_id = ceilings.regions[0].id;
        let out = ceilings.with_z_deltas(&[]);
        assert_eq!(out.regions[0].polygon, before, "points round-trip exactly");
        assert_eq!(out.regions[0].id, before_id, "region untouched");
    }

    #[test]
    fn with_z_deltas_drops_sliver_pieces() {
        let h = 2.5;
        let ceilings = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 10.0, 10.0)],
            h,
            ItemId::default(),
        )
        // Area only grazes the footprint: the intersected strip is
        // 1e-8 × 10 m = 1e-7 m² — boolean-op debris, not a ceiling.
        .with_z_deltas(&[(square(-5.0, 0.0, 1e-8, 10.0), 0.3)]);
        assert_eq!(ceilings.regions.len(), 1, "sliver piece dropped");
        let z = ceilings.regions[0].flat_z().expect("remainder is flat");
        assert!((z - h).abs() < 1e-9, "remainder keeps its height");
    }

    // ── with_flat_patches (dormers) ─────────────────────────────────────────

    /// A ceiling region sloping z = 2.0 + 0.1·x over `ring`, one construction.
    fn sloped_region(ring: Vec<Point2f64>, cid: ItemId) -> CeilingRegion {
        CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(
                ring.into_iter()
                    .map(|p| Point3f64::new(p.x, p.y, 2.0 + 0.1 * p.x))
                    .collect(),
            ),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        }
    }

    /// A ceiling region over `ring` on the plane z = c0 + cx·x + cy·y (cx = cy =
    /// 0 gives a flat region at c0), one construction.
    fn region_on_plane(
        ring: Vec<Point2f64>,
        c0: f64,
        cx: f64,
        cy: f64,
        cid: ItemId,
    ) -> CeilingRegion {
        CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(
                ring.into_iter()
                    .map(|p| Point3f64::new(p.x, p.y, c0 + cx * p.x + cy * p.y))
                    .collect(),
            ),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        }
    }

    /// The DISCONTINUOUS-STEP span the dormer review flagged: region A slopes UP
    /// to 3.0 at the shared seam x = 5, region B is flat at 2.6 there — a
    /// discontinuous roof step. A footprint spanning both must report the roof's
    /// max as A's 3.0 (the higher side of the seam), NOT B's 2.6. Because each
    /// region contributes the max of its OWN plane over its OWN piece, the answer
    /// is the same in either region order — `top_z_at`'s first-region-wins
    /// tie-break ON the seam is exactly the ambiguity this avoids.
    #[test]
    fn footprint_coverage_roof_max_captures_the_higher_side_of_a_step() {
        let cid = ItemId::default();
        // A: x∈[0,5]×[0,6], plane z = 1.0 + 0.4·x → 3.0 at the seam x = 5.
        let mk_a = || region_on_plane(square(0.0, 0.0, 5.0, 6.0), 1.0, 0.4, 0.0, cid);
        // B: x∈[5,10]×[0,6], flat at 2.6.
        let mk_b = || region_on_plane(square(5.0, 0.0, 10.0, 6.0), 2.6, 0.0, 0.0, cid);
        // Footprint x∈[3,7]×[2,4] spans both: 2×2 over A + 2×2 over B = 8 m².
        let fp = square(3.0, 2.0, 7.0, 4.0);
        for (label, regions) in [
            ("A before B", vec![mk_a(), mk_b()]),
            ("B before A", vec![mk_b(), mk_a()]),
        ] {
            let ceilings = CeilingRegions { regions };
            let (covered, roof_max) = ceilings.footprint_coverage_and_roof_max(&fp);
            assert!(
                (covered - 8.0).abs() < 1e-6,
                "{label}: full coverage, got {covered}"
            );
            let roof_max = roof_max.expect("footprint intersects both regions");
            assert!(
                (roof_max - 3.0).abs() < 1e-9,
                "{label}: the step's higher side (3.0) binds, not B's 2.6 — got {roof_max}"
            );
        }
        // A footprint over NOTHING → no coverage, no roof.
        let (covered, roof_max) = CeilingRegions {
            regions: vec![mk_a()],
        }
        .footprint_coverage_and_roof_max(&square(20.0, 20.0, 24.0, 24.0));
        assert_eq!(covered, 0.0);
        assert_eq!(roof_max, None);
    }

    /// A flat dormer patch carved into a SLOPED region: the inside piece is flat
    /// at the absolute top, the outside remainder stays on the SOURCE plane, and
    /// a region the ring never touches comes out byte-identical.
    #[test]
    fn with_flat_patches_flattens_inside_and_keeps_the_slope_outside() {
        let cid = ItemId::default();
        // Sloped attic z = 2.0 + 0.1·x over [0,10]² (plane c0=2.0, cx=0.1, cy=0).
        let sloped = sloped_region(square(0.0, 0.0, 10.0, 10.0), cid);
        // A flat region far away the dormer never intersects.
        let far = CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(20.0, 20.0, 2.4),
                Point3f64::new(24.0, 20.0, 2.4),
                Point3f64::new(24.0, 24.0, 2.4),
                Point3f64::new(20.0, 24.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        let far_id = far.id;
        let far_poly = far.polygon.clone();
        let ceilings = CeilingRegions {
            regions: vec![sloped, far],
        };

        let out = ceilings.with_flat_patches(&[(square(3.0, 3.0, 6.0, 6.0), 3.5)]);

        // INSIDE: exactly one flat patch at z = 3.5, area 3×3 = 9 m².
        let patches: Vec<_> = out
            .regions
            .iter()
            .filter(|r| r.flat_z().is_some_and(|z| (z - 3.5).abs() < 1e-9))
            .collect();
        assert_eq!(patches.len(), 1, "one flat dormer patch");
        assert!(
            (patches[0].area() - 9.0).abs() < 1e-6,
            "3×3 dormer footprint"
        );
        assert!(
            patches[0]
                .polygon
                .exterior
                .iter()
                .all(|v| (v.z - 3.5).abs() < 1e-9),
            "every dormer-top vertex sits at z_abs"
        );
        assert_eq!(
            patches[0].construction_id, cid,
            "patch inherits construction"
        );

        // UNTOUCHED neighbour: byte-identical, same id (never re-split).
        let far_out = out
            .regions
            .iter()
            .find(|r| r.id == far_id)
            .expect("the untouched region survives");
        assert_eq!(
            far_out.polygon, far_poly,
            "a region the ring misses is byte-identical"
        );

        // OUTSIDE: every sloped-remainder piece samples the ORIGINAL plane.
        for r in &out.regions {
            if r.id == far_id || r.flat_z().is_some_and(|z| (z - 3.5).abs() < 1e-9) {
                continue;
            }
            let (c0, cx, cy) = region_plane(r).expect("planar remainder");
            assert!(
                (c0 - 2.0).abs() < 1e-9 && (cx - 0.1).abs() < 1e-9 && cy.abs() < 1e-9,
                "remainder stays on the source plane, got ({c0}, {cx}, {cy})"
            );
        }
    }

    /// After `with_flat_patches`, `riser_faces` derives the dormer's vertical
    /// walls around the boundary — but emits NOTHING along an edge where the
    /// flat top is flush with the sloped roof.
    #[test]
    fn with_flat_patches_riser_faces_wall_the_dormer_and_skip_the_flush_edge() {
        let cid = ItemId::default();
        // Slope z = 2.0 + 0.1·x: roof is 2.3 at x = 3 and 2.6 at x = 6.
        let sloped = sloped_region(square(0.0, 0.0, 10.0, 10.0), cid);
        let ceilings = CeilingRegions {
            regions: vec![sloped],
        };
        // Dormer top at 2.6 = the roof height along the x = 6 edge → FLUSH there,
        // a 0.3 m step along the x = 3 edge.
        let out = ceilings.with_flat_patches(&[(square(3.0, 3.0, 6.0, 6.0), 2.6)]);
        let faces = out.riser_faces();
        assert!(!faces.is_empty(), "the dormer boundary walls up");
        assert!(faces.len() >= 3, "faces on the three non-flush edges");

        // The x = 3 edge: a full-height 0.3 m riser (2.3 → 2.6) on the line x = 3.
        let x3 = faces.iter().find(|f| {
            f.exterior.iter().all(|v| (v.x - 3.0).abs() < 1e-6)
                && f.exterior.iter().any(|v| (v.z - 2.3).abs() < 1e-4)
                && f.exterior.iter().any(|v| (v.z - 2.6).abs() < 1e-4)
        });
        assert!(x3.is_some(), "a 0.3 m riser on the x = 3 dormer edge");

        // The x = 6 edge is FLUSH (patch 2.6 = roof 2.6): no face lies wholly on it.
        assert!(
            !faces
                .iter()
                .any(|f| f.exterior.iter().all(|v| (v.x - 6.0).abs() < 1e-6)),
            "no riser along the flush x = 6 edge"
        );
    }

    /// A dormer footprint SPANNING a SLOPED region and an adjacent FLAT
    /// protrusion (the reported workflow) composes to a single flat top at
    /// `z_abs`: both source regions' inside pieces flatten to `z_abs` — coplanar,
    /// so together they read as ONE continuous dormer top with NO riser along
    /// their shared seam — while the surrounding roof keeps its own planes and
    /// `riser_faces` walls the whole spanning footprint.
    #[test]
    fn with_flat_patches_span_a_sloped_and_a_flat_region() {
        let cid = ItemId::default();
        // Sloped A over x∈[0,5]: z = 2.0 + 0.1·x (2.0 → 2.5). Flat protrusion B
        // over x∈[5,10] at 2.5 — continuous with A at the shared boundary x = 5.
        let sloped = sloped_region(square(0.0, 0.0, 5.0, 10.0), cid);
        let flat = region_on_plane(square(5.0, 0.0, 10.0, 10.0), 2.5, 0.0, 0.0, cid);
        let ceilings = CeilingRegions {
            regions: vec![sloped, flat],
        };

        // Dormer top at 2.8, footprint square(3,3,7,7) — spans the x = 5 boundary.
        let out = ceilings.with_flat_patches(&[(square(3.0, 3.0, 7.0, 7.0), 2.8)]);
        assert_flat_dormer_top_spans(&out, 2.8, 16.0, 5.0);

        // Surrounding roof unchanged: the sloped remainder on its source plane,
        // the flat remainder still at 2.5.
        for r in &out.regions {
            if r.flat_z().is_some_and(|z| (z - 2.8).abs() < 1e-9) {
                continue;
            }
            let (c0, cx, cy) = region_plane(r).expect("planar remainder");
            let sloped_ok = (c0 - 2.0).abs() < 1e-9 && (cx - 0.1).abs() < 1e-9 && cy.abs() < 1e-9;
            let flat_ok = (c0 - 2.5).abs() < 1e-9 && cx.abs() < 1e-9 && cy.abs() < 1e-9;
            assert!(
                sloped_ok || flat_ok,
                "remainder keeps its own plane, got ({c0}, {cx}, {cy})"
            );
        }
    }

    /// A CORNER/RIDGE dormer spanning TWO SLOPED faces that meet at a ridge
    /// composes the same way — both inside pieces flatten to one coplanar top at
    /// `z_abs`, the two slopes stay unchanged, and `riser_faces` walls the whole
    /// footprint with NO riser along the coplanar seam over the ridge.
    #[test]
    fn with_flat_patches_span_two_slopes_across_a_ridge() {
        let cid = ItemId::default();
        // Gable: south face rises z = 2.0 + 0.4·y over y∈[0,5] to the ridge at
        // y = 5 (z = 4.0); north face falls z = 6.0 - 0.4·y over y∈[5,10].
        let south = region_on_plane(square(0.0, 0.0, 10.0, 5.0), 2.0, 0.0, 0.4, cid);
        let north = region_on_plane(square(0.0, 5.0, 10.0, 10.0), 6.0, 0.0, -0.4, cid);
        let ceilings = CeilingRegions {
            regions: vec![south, north],
        };

        // Dormer top at 4.5 (above the 4.0 ridge), footprint square(3,4,7,6) —
        // straddles the ridge y = 5.
        let out = ceilings.with_flat_patches(&[(square(3.0, 4.0, 7.0, 6.0), 4.5)]);
        assert_flat_dormer_top_spans(&out, 4.5, 8.0, 5.0);

        // Both slopes survive on their source planes.
        for r in &out.regions {
            if r.flat_z().is_some_and(|z| (z - 4.5).abs() < 1e-9) {
                continue;
            }
            let (c0, cx, cy) = region_plane(r).expect("planar remainder");
            let south_ok = (c0 - 2.0).abs() < 1e-9 && cx.abs() < 1e-9 && (cy - 0.4).abs() < 1e-9;
            let north_ok = (c0 - 6.0).abs() < 1e-9 && cx.abs() < 1e-9 && (cy + 0.4).abs() < 1e-9;
            assert!(
                south_ok || north_ok,
                "remainder keeps its own slope, got ({c0}, {cx}, {cy})"
            );
        }
    }

    /// Shared assertions for a SPANNING dormer top: the inside pieces are all
    /// flat at `z_abs` (coplanar — one continuous top), together cover `area`
    /// m², and `riser_faces` walls the footprint while emitting NO riser along
    /// the coplanar seam at the source-region boundary `seam_coord` (an x=const
    /// or y=const line inside the flat top).
    fn assert_flat_dormer_top_spans(out: &CeilingRegions, z_abs: f64, area: f64, seam_coord: f64) {
        let top: Vec<_> = out
            .regions
            .iter()
            .filter(|r| r.flat_z().is_some_and(|z| (z - z_abs).abs() < 1e-9))
            .collect();
        assert!(!top.is_empty(), "the spanning dormer top exists");
        assert_eq!(
            top.len(),
            1,
            "the dormer top is ONE merged region, not split by the seam underneath"
        );
        let top_area: f64 = top.iter().map(|r| r.area()).sum();
        assert!(
            (top_area - area).abs() < 1e-6,
            "the flat top covers the whole footprint ({area} m²), got {top_area}"
        );
        for r in &top {
            assert!(
                r.polygon
                    .exterior
                    .iter()
                    .all(|v| (v.z - z_abs).abs() < 1e-9),
                "every dormer-top vertex sits at z_abs"
            );
        }
        let faces = out.riser_faces();
        assert!(!faces.is_empty(), "the dormer boundary walls up");
        // The coplanar seam between the two inside pieces (both at z_abs) emits
        // nothing — no riser face lies wholly on the source-region boundary line.
        assert!(
            !faces.iter().any(|f| {
                f.exterior.iter().all(|v| (v.x - seam_coord).abs() < 1e-6)
                    || f.exterior.iter().all(|v| (v.y - seam_coord).abs() < 1e-6)
            }),
            "no riser along the coplanar seam inside the flat top"
        );
    }

    /// `#[serde(default)]`: a Storey JSON produced by the OLD shape (no
    /// `dormers` field) still deserializes, with an empty dormer vec.
    #[test]
    fn storey_without_dormers_deserializes_to_empty_vec() {
        let mut storey = Storey::default();
        storey.dormers.push(Dormer {
            footprint: square(3.0, 3.0, 6.0, 6.0),
            top_z_m: 2.6,
        });
        // Round-trips as-is with the field present.
        let json = serde_json::to_value(&storey).expect("serialize");
        let back: Storey = serde_json::from_value(json.clone()).expect("round-trip");
        assert_eq!(back.dormers.len(), 1);
        assert!((back.dormers[0].top_z_m - 2.6).abs() < 1e-9);
        // OLD shape: strip the field entirely — it defaults to empty.
        let mut old = json;
        assert!(old.as_object_mut().unwrap().remove("dormers").is_some());
        let loaded: Storey = serde_json::from_value(old).expect("old JSON still loads");
        assert!(loaded.dormers.is_empty(), "missing dormers → empty vec");
    }

    /// HIGH-severity regression: a step area strictly INTERIOR to a region
    /// makes `difference` return the region with a HOLE; `Polygon3D` is
    /// exterior-only, so without decomposition the hole is silently filled —
    /// a phantom full-area region overlapping the stepped piece. Four
    /// composing half-plane steps create a strictly-interior [2,8]² centre
    /// cell at +0.4; canonicalize (rebuild areas from the derived regions,
    /// as `ceiling_step_regions_from_roofs` does) then re-derive must be a
    /// FIXPOINT — pre-fix it inflated 100 m² of ceiling to 136 m².
    #[test]
    fn with_z_deltas_interior_step_keeps_holes_and_rederive_is_a_fixpoint() {
        use geo::Area;
        use std::collections::BTreeMap;
        let h = 2.4;
        let cid = ItemId::default();
        let footprint =
            || CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 10.0, 10.0)], h, cid);
        // z → total area, keyed at 1e-6 m height resolution.
        let area_by_z = |c: &CeilingRegions| -> BTreeMap<i64, f64> {
            let mut map: BTreeMap<i64, f64> = BTreeMap::new();
            for r in &c.regions {
                let z = r.flat_z().expect("every step piece is flat");
                *map.entry((z * 1e6).round() as i64).or_insert(0.0) +=
                    Polygon::<f64>::from(r.polygon.clone()).unsigned_area();
            }
            map
        };

        // First pass: four half-plane steps composing to +0.4 in the centre.
        let first = footprint().with_z_deltas(&[
            (square(2.0, 0.0, 10.0, 10.0), 0.1), // x > 2
            (square(0.0, 0.0, 8.0, 10.0), 0.1),  // x < 8
            (square(0.0, 2.0, 10.0, 10.0), 0.1), // y > 2
            (square(0.0, 0.0, 10.0, 8.0), 0.1),  // y < 8
        ]);
        let first_map = area_by_z(&first);
        let total: f64 = first_map.values().sum();
        assert!(
            (total - 100.0).abs() < 1e-6,
            "total area 100 m², got {total}"
        );
        // The strictly-interior centre cell: [2,8]² at h + 0.4 (36 m²).
        let key = |z: f64| (z * 1e6).round() as i64;
        assert!(
            (first_map[&key(h + 0.4)] - 36.0).abs() < 1e-6,
            "36 m² centre cell at h + 0.4"
        );
        let centre: Vec<_> = first
            .regions
            .iter()
            .filter(|r| r.flat_z().is_some_and(|z| (z - (h + 0.4)).abs() < 1e-9))
            .collect();
        assert_eq!(centre.len(), 1, "one centre piece");
        for v in &centre[0].polygon.exterior {
            assert!(
                (2.0..=8.0).contains(&v.x) && (2.0..=8.0).contains(&v.y),
                "centre piece is [2,8]², got vertex ({}, {})",
                v.x,
                v.y
            );
        }

        // Simulate canonicalize: rebuild (ring, delta) areas from the derived
        // regions and re-apply to a FRESH default ceiling. The interior
        // centre cell now arrives as an area of its own, so its remainder is
        // the footprint with a hole — the case `split_to_simple` exists for.
        let rebuilt: Vec<(Vec<Point2f64>, f64)> = first
            .regions
            .iter()
            .map(|r| {
                let ring = r
                    .polygon
                    .exterior
                    .iter()
                    .map(|v| Point2f64::new(v.x, v.y))
                    .collect();
                (ring, r.flat_z().unwrap() - h)
            })
            .collect();
        let second = footprint().with_z_deltas(&rebuilt);
        let second_map = area_by_z(&second);
        assert_eq!(
            first_map.keys().collect::<Vec<_>>(),
            second_map.keys().collect::<Vec<_>>(),
            "re-derive keeps the same set of heights"
        );
        for (z, a) in &first_map {
            assert!(
                (a - second_map[z]).abs() < 1e-6,
                "area at z key {z} changed on re-derive: {a} → {}",
                second_map[z]
            );
        }
    }

    #[test]
    fn flat_z_some_for_flat_none_for_sloped_or_degenerate() {
        let cid = ItemId::default();
        let flat =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.6, cid)
                .regions
                .remove(0);
        assert_eq!(flat.flat_z(), Some(2.6));

        let sloped = CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 2.4),
                Point3f64::new(4.0, 0.0, 2.4),
                Point3f64::new(4.0, 4.0, 3.1), // ridge — not within 1e-6
                Point3f64::new(0.0, 4.0, 3.1),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        assert_eq!(sloped.flat_z(), None);

        let degenerate = CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 2.4),
                Point3f64::new(4.0, 0.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        assert_eq!(degenerate.flat_z(), None, "< 3 vertices is degenerate");
    }

    // ── Roof apexes (fans) ──────────────────────────────────────────────

    /// Plan area of a region's exterior ring (shoelace).
    fn plan_area(r: &CeilingRegion) -> f64 {
        use geo::Area;
        Polygon::<f64>::from(r.polygon.clone()).unsigned_area()
    }

    #[test]
    fn with_apexes_fans_containing_region_into_edge_triangles() {
        let cid = ItemId::default();
        let apex = CeilingApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.2,
        };
        let fanned =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid)
                .with_apexes(&[apex.clone()]);
        assert_eq!(fanned.regions.len(), 4, "one triangle per boundary edge");
        let total: f64 = fanned.regions.iter().map(plan_area).sum();
        assert!((total - 16.0).abs() < 1e-6, "fan tiles the region");
        for r in &fanned.regions {
            assert_eq!(r.construction_id, cid, "pieces inherit the fabric link");
            let ext = &r.polygon.exterior;
            assert_eq!(ext.len(), 3);
            // Boundary keeps its z; the apex sits at z_m.
            assert!((ext[0].z - 2.4).abs() < 1e-9);
            assert!((ext[1].z - 2.4).abs() < 1e-9);
            assert_eq!((ext[2].x, ext[2].y, ext[2].z), (2.0, 2.0, 4.2));
        }
    }

    /// RELOAD FIXPOINT: re-applying an apex to its own fan triangles — where
    /// it is a boundary CORNER — is the identity (boundary-inclusive
    /// containment finds the triangles; the two degenerate triangles per
    /// piece drop as slivers; the third is the piece itself).
    #[test]
    fn with_apexes_apex_as_corner_refan_is_identity() {
        let apex = CeilingApex {
            point: Point2f64::new(1.5, 2.5),
            z_m: 3.8,
        };
        let fanned = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 4.0, 4.0)],
            2.4,
            ItemId::default(),
        )
        .with_apexes(&[apex.clone()]);
        let before: Vec<Polygon3D> = fanned.regions.iter().map(|r| r.polygon.clone()).collect();
        let refanned = fanned.with_apexes(&[apex]);
        let after: Vec<Polygon3D> = refanned.regions.iter().map(|r| r.polygon.clone()).collect();
        assert_eq!(after, before, "re-fan is the geometric identity");
    }

    /// COMPOSITION ORDER: an apex over a `with_z_deltas` piece fans from the
    /// STEPPED base (boundary keeps the stepped z), and `fan_base_z` recovers
    /// that base on the way back — the apex-aware step-reconstruction chain.
    #[test]
    fn with_apexes_over_stepped_piece_keeps_stepped_base_and_fan_base_z_recovers_it() {
        let h = 2.4;
        let delta = 0.3;
        let apex = CeilingApex {
            point: Point2f64::new(7.5, 2.0), // inside the stepped right half
            z_m: 4.0,
        };
        let regions = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 10.0, 4.0)],
            h,
            ItemId::default(),
        )
        .with_z_deltas(&[(square(5.0, 0.0, 10.0, 4.0), delta)])
        .with_apexes(&[apex.clone()]);

        // The unstepped left half is untouched and still flat at h.
        let flat: Vec<_> = regions
            .regions
            .iter()
            .filter(|r| r.flat_z().is_some())
            .collect();
        assert_eq!(flat.len(), 1);
        assert!((flat[0].flat_z().unwrap() - h).abs() < 1e-9);

        // The fan triangles rise from the STEPPED base, not the default.
        let fans: Vec<_> = regions
            .regions
            .iter()
            .filter(|r| r.flat_z().is_none())
            .collect();
        assert_eq!(fans.len(), 4, "stepped rectangle fans to 4 triangles");
        for r in &fans {
            assert_eq!(
                r.fan_base_z(&[apex.clone()]),
                Some(h + delta),
                "fan_base_z recovers the stepped base (apex-aware reload)"
            );
            assert_eq!(r.flat_z(), None, "sloped piece has no flat z");
        }
    }

    #[test]
    fn with_apexes_apex_in_no_region_is_skipped_and_regions_untouched() {
        let ceilings = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 4.0, 4.0)],
            2.4,
            ItemId::default(),
        );
        let before_id = ceilings.regions[0].id;
        let before_poly = ceilings.regions[0].polygon.clone();
        let out = ceilings.with_apexes(&[CeilingApex {
            point: Point2f64::new(50.0, 50.0),
            z_m: 4.0,
        }]);
        assert_eq!(out.regions.len(), 1);
        assert_eq!(out.regions[0].id, before_id, "region untouched");
        assert_eq!(out.regions[0].polygon, before_poly);
    }

    #[test]
    fn with_apexes_one_pass_fans_disjoint_rings_independently() {
        // Two disjoint rings; one apex each — both fan independently (each
        // region groups its own contained apexes; here one each → FAN).
        let regions = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 4.0, 4.0), square(6.0, 0.0, 10.0, 4.0)],
            2.4,
            ItemId::default(),
        )
        .with_apexes(&[
            CeilingApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.0,
            },
            CeilingApex {
                point: Point2f64::new(8.0, 2.0),
                z_m: 3.5,
            },
        ]);
        assert_eq!(regions.regions.len(), 8, "both rings fanned");
        let total: f64 = regions.regions.iter().map(plan_area).sum();
        assert!((total - 32.0).abs() < 1e-6);
    }

    /// The TWO predicates: assign-time validation is strict (boundary
    /// excluded), while `with_apexes` containment is boundary-inclusive.
    #[test]
    fn region_strictly_containing_excludes_the_boundary() {
        let ceilings = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 4.0, 4.0)],
            2.4,
            ItemId::default(),
        );
        assert!(ceilings
            .region_strictly_containing(Point2f64::new(2.0, 2.0))
            .is_some());
        assert!(
            ceilings
                .region_strictly_containing(Point2f64::new(4.0, 2.0))
                .is_none(),
            "edge point excluded"
        );
        assert!(
            ceilings
                .region_strictly_containing(Point2f64::new(0.0, 0.0))
                .is_none(),
            "corner excluded"
        );
        assert!(ceilings
            .region_strictly_containing(Point2f64::new(9.0, 9.0))
            .is_none());
    }

    #[test]
    fn fan_base_z_flat_sloped_and_degenerate_cases() {
        let cid = ItemId::default();
        // Flat region, no apex vertices: equals flat_z.
        let flat =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.6, cid)
                .regions
                .remove(0);
        assert_eq!(flat.fan_base_z(&[]), Some(2.6));

        // Sloped WITHOUT a matching apex: not level → None.
        let sloped = CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 2.4),
                Point3f64::new(4.0, 0.0, 2.4),
                Point3f64::new(2.0, 2.0, 4.0),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        assert_eq!(sloped.fan_base_z(&[]), None);

        // The same triangle WITH its apex declared: base recovered.
        let apex = CeilingApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.0,
        };
        assert_eq!(sloped.fan_base_z(&[apex.clone()]), Some(2.4));

        // A needle with a SINGLE non-apex vertex. DELIBERATE CHANGE (ridge
        // work): this used to be None under a two-base-vertex floor, but a
        // ridge TRANSITION triangle (v, A, B) keeps exactly one non-apex
        // vertex and must still recover its base on reload, so fan_base_z is
        // RELAXED to >= 1 remaining vertex. Plan-degenerate needles like this
        // one still die downstream at the reconstruction's ring_area2 filter.
        let needle = CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(2.0, 2.0, 4.0),
                Point3f64::new(2.0, 2.0, 4.0),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        assert_eq!(needle.fan_base_z(&[apex]), Some(2.4));

        // All vertices are apexes: nothing left to base on → still None.
        let all_apex = CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(2.0, 2.0, 4.0),
                Point3f64::new(2.0, 2.0, 4.0),
                Point3f64::new(2.0, 2.0, 4.0),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure: CeilingExposure::default(),
        };
        assert_eq!(
            all_apex.fan_base_z(&[CeilingApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.0,
            }]),
            None
        );
    }

    // ── Roof ridges (two apexes in one region) ──────────────────────────

    /// Signed plan area of a region's exterior ring (shoelace; positive CCW).
    fn signed_plan_area(r: &CeilingRegion) -> f64 {
        let e = &r.polygon.exterior;
        let n = e.len();
        (0..n)
            .map(|i| {
                let p = e[i];
                let q = e[(i + 1) % n];
                p.x * q.y - q.x * p.y
            })
            .sum::<f64>()
            / 2.0
    }

    /// How many of the region's vertices plan-match one of `apexes`
    /// (1e-6 m) — 1 for a hub triangle, 2 for a transition triangle.
    fn apex_vertex_count(r: &CeilingRegion, apexes: &[CeilingApex]) -> usize {
        r.polygon
            .exterior
            .iter()
            .filter(|v| {
                apexes
                    .iter()
                    .any(|a| (v.x - a.point.x).abs() <= 1e-6 && (v.y - a.point.y).abs() <= 1e-6)
            })
            .count()
    }

    /// Cyclic ring equality: same vertices in the same cyclic order, any
    /// starting vertex (triangulation may re-emit a triangle rotated).
    fn cyclically_equal(a: &Polygon3D, b: &Polygon3D) -> bool {
        let (ea, eb) = (&a.exterior, &b.exterior);
        let n = ea.len();
        n == eb.len() && (0..n).any(|s| (0..n).all(|i| ea[i] == eb[(i + s) % n]))
    }

    /// A level gable over the 10×4 footprint: apexes at (2,2) and (8,2).
    fn gable_apexes() -> (CeilingApex, CeilingApex) {
        (
            CeilingApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(8.0, 2.0),
                z_m: 4.2,
            },
        )
    }

    fn gable_footprint(cid: ItemId) -> CeilingRegions {
        CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 10.0, 4.0)], 2.4, cid)
    }

    /// Every vertex of the region within 1e-6 m of its own (Newell) plane.
    fn is_planar(r: &CeilingRegion) -> bool {
        let ext = &r.polygon.exterior;
        let Some((n, d)) = face_plane(ext) else {
            return false;
        };
        ext.iter()
            .all(|v| (n.x * v.x + n.y * v.y + n.z * v.z - d).abs() <= 1e-6)
    }

    /// THE rectangle-ridge test: the classic hipped result. The midline edge
    /// split shares the long edges between the two hubs (no more "B connects
    /// to all 4 corners" on the tie), and the coplanar merge collapses each
    /// slope's triangles into ONE planar trapezoid: exactly 4 faces —
    /// 2 trapezoids (4 vertices, holding BOTH apexes) + 2 end triangles
    /// (3 vertices, one apex each), all planar, tiling the footprint.
    #[test]
    fn with_apexes_two_apexes_make_a_rectangle_gable() {
        let cid = ItemId::default();
        let (a, b) = gable_apexes();
        let apexes = vec![a.clone(), b.clone()];
        let out = gable_footprint(cid).with_apexes(&apexes);
        assert_eq!(out.regions.len(), 4, "2 trapezoids + 2 end triangles");
        let mut vertex_counts: Vec<usize> = out
            .regions
            .iter()
            .map(|r| r.polygon.exterior.len())
            .collect();
        vertex_counts.sort();
        assert_eq!(vertex_counts, vec![3, 3, 4, 4], "trapezoid/triangle split");
        let trapezoids: Vec<&CeilingRegion> = out
            .regions
            .iter()
            .filter(|r| r.polygon.exterior.len() == 4)
            .collect();
        let at = |v: &Point3f64, apex: &CeilingApex| {
            (v.x - apex.point.x).abs() <= 1e-9
                && (v.y - apex.point.y).abs() <= 1e-9
                && (v.z - apex.z_m).abs() <= 1e-9
        };
        for t in &trapezoids {
            assert!(
                t.polygon.exterior.iter().any(|v| at(v, &a))
                    && t.polygon.exterior.iter().any(|v| at(v, &b)),
                "each trapezoid holds BOTH apex vertices: {:?}",
                t.polygon
            );
            // The merged trapezoid still recovers its base for the
            // ceiling-step reconstruction — the reload-fixpoint invariant.
            assert_eq!(t.fan_base_z(&apexes), Some(2.4), "trapezoid base z");
        }
        let triangles: Vec<&CeilingRegion> = out
            .regions
            .iter()
            .filter(|r| r.polygon.exterior.len() == 3)
            .collect();
        for t in &triangles {
            assert_eq!(apex_vertex_count(t, &apexes), 1, "end triangles: one apex");
            assert_eq!(t.fan_base_z(&apexes), Some(2.4), "end triangle base z");
        }
        // The faces tile the region exactly, planar, CCW, fabric inherited.
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 40.0).abs() < 1e-6, "areas sum to the region");
        for r in &out.regions {
            assert!(is_planar(r), "non-planar face: {:?}", r.polygon);
            assert_eq!(r.construction_id, cid, "pieces inherit the fabric link");
            assert!(signed_plan_area(r) > 0.0, "flipped face: {:?}", r.polygon);
        }
    }

    /// THE NEW IDENTITY: re-applying the apexes to with_apexes' own MERGED
    /// output reproduces the same merged faces — the triangulation
    /// re-fragments each face (the trapezoids contain both apexes as
    /// corners; pre-lift holds their z) and the coplanar merge folds it
    /// back. Load-bearing for the reload → re-derive fixpoint.
    #[test]
    fn with_apexes_on_its_own_merged_output_is_identity() {
        let (a, b) = gable_apexes();
        let apexes = vec![a, b];
        let gable = gable_footprint(ItemId::default()).with_apexes(&apexes);
        assert_eq!(gable.regions.len(), 4);
        let again = gable.clone().with_apexes(&apexes);
        assert_eq!(again.regions.len(), gable.regions.len(), "same face count");
        for (before, after) in gable.regions.iter().zip(&again.regions) {
            assert!(
                cyclically_equal(&after.polygon, &before.polygon),
                "re-apply identity on the merged gable: {:?} vs {:?}",
                before.polygon,
                after.polygon
            );
        }
        // And each face individually re-applies to itself too (the end
        // triangles via the fan branch, the trapezoids via the ridge one).
        for face in &gable.regions {
            let out = CeilingRegions {
                regions: vec![face.clone()],
            }
            .with_apexes(&apexes);
            assert_eq!(out.regions.len(), 1, "one face survives");
            assert!(
                cyclically_equal(&out.regions[0].polygon, &face.polygon),
                "per-face re-apply identity: {:?} vs {:?}",
                face.polygon,
                out.regions[0].polygon
            );
        }
    }

    /// THE TRUE GABLE (field report 2026-07-12): ridge endpoints ON two
    /// opposite boundary edges — even on a rectangle-ISH ring carrying an
    /// extra collinear vertex on a gable edge — yield exactly TWO sloped
    /// planes reaching only the 4 corners: the gable-edge hub triangles are
    /// vertical (zero plan area) and drop as slivers, the hip transitions
    /// merge into the slopes, and NO fan plane attaches the end-edge
    /// vertices to the ridge. The vertical gable ends themselves are walls
    /// (the roof-void wall split), not ceiling faces.
    #[test]
    fn ridge_endpoints_on_opposite_edges_make_two_plane_gable() {
        let cid = ItemId::default();
        // 10×4 ring with an extra collinear vertex at (0, 1) on the left
        // (gable) edge — the "rectangle-ish" drawing.
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(0.0, 4.0),
            Point2f64::new(0.0, 1.0),
        ];
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(0.0, 2.0), // ON the left edge
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(10.0, 2.0), // ON the right edge
                z_m: 4.2,
            },
        ];
        let out = CeilingRegions::default_for_footprint(vec![ring], 2.4, cid).with_apexes(&apexes);
        assert_eq!(
            out.regions.len(),
            2,
            "a true gable: two slopes, no hip/fan faces: {:?}",
            out.regions.iter().map(|r| &r.polygon).collect::<Vec<_>>()
        );
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 40.0).abs() < 1e-6, "the two slopes tile the area");
        for r in &out.regions {
            assert!(is_planar(r), "non-planar slope: {:?}", r.polygon);
            assert!(r.flat_z().is_none(), "each face genuinely slopes");
            assert_eq!(
                apex_vertex_count(r, &apexes),
                2,
                "each slope runs the full ridge"
            );
            // Only the 4 footprint corners join the ridge corners on each
            // face — the collinear left-edge vertex vanished with its
            // vertical slivers.
            assert_eq!(r.polygon.exterior.len(), 4, "eave edge + ridge edge");
        }
    }

    /// The tap-to-unfan LIFT on a single-apex fan: unfanning one vertex of a
    /// hexagonal fan removes its fan plane — the two hub triangles beside it
    /// become coplanar with the chord plane through its fanned neighbours
    /// and the apex, and merge into ONE face: 6 planes → 5, same coverage,
    /// with the unfanned vertex LIFTED onto that plane (here BELOW the base:
    /// the corner pokes past the chord, so the plane keeps falling past the
    /// eave line — the "roof continues over the bay" reading).
    #[test]
    fn unfan_one_vertex_of_a_hexagon_fan_gives_five_planes() {
        let cid = ItemId::default();
        let hex = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(6.0, 2.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
            Point2f64::new(-2.0, 2.0),
        ];
        let apexes = vec![CeilingApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.5,
        }];
        let base = CeilingRegions::default_for_footprint(vec![hex], 2.4, cid);
        // Baseline: the fan reaches ALL six vertices (the conservatory case).
        let fanned = base.clone().with_apexes(&apexes);
        assert_eq!(fanned.regions.len(), 6, "fan to every vertex by default");

        let unfanned = vec![Point2f64::new(6.0, 2.0)];
        let out = base.with_apexes_hinted_unfanned(&apexes, &[], &unfanned);
        assert_eq!(out.regions.len(), 5, "one fan plane folded away");
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 24.0).abs() < 1e-6, "coverage unchanged");
        // The merged face holds the lifted vertex on the chord plane through
        // (4,0,2.4)/(4,4,2.4) and the apex (2,2,4.5): z = 2.4 + (4−x)·1.05 →
        // 0.3 at x = 6.
        let lifted = out
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 6.0).abs() <= 1e-9 && (v.y - 2.0).abs() <= 1e-9)
            })
            .expect("the unfanned vertex still bounds a face");
        assert_eq!(lifted.polygon.exterior.len(), 4, "two triangles merged");
        let v = lifted
            .polygon
            .exterior
            .iter()
            .find(|v| (v.x - 6.0).abs() <= 1e-9)
            .unwrap();
        assert!(
            (v.z - 0.3).abs() <= 1e-9,
            "lifted onto the chord plane, got z = {}",
            v.z
        );
        assert!(is_planar(lifted), "the merged face is one plane");
        // And the group machinery stays intact: the unfanned-aware base
        // check keeps every face in the merged ring (the plain one would
        // warn-exclude the lifted face).
        let groups = out.apex_groups_unfanned(&apexes, &unfanned);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].base_z, 2.4);
        let ring_area: f64 = groups[0]
            .merged_rings
            .iter()
            .map(|ring| {
                let n = ring.len();
                (0..n)
                    .map(|i| {
                        let (p, q) = (ring[i], ring[(i + 1) % n]);
                        p.x * q.y - q.x * p.y
                    })
                    .sum::<f64>()
                    .abs()
                    / 2.0
            })
            .sum();
        assert!(
            (ring_area - 24.0).abs() < 1e-6,
            "the merged ring keeps the lifted face's area (got {ring_area})"
        );
    }

    /// Re-applying the SAME apexes + unfanned set to the unfanned solve's own
    /// output reproduces it — the re-apply identity the reload/recompose
    /// fixpoint rides on (the lift re-computes the same chord plane, so the
    /// lifted z round-trips).
    #[test]
    fn unfanned_solve_reapplies_to_itself() {
        let cid = ItemId::default();
        let hex = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(6.0, 2.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
            Point2f64::new(-2.0, 2.0),
        ];
        let apexes = vec![CeilingApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.5,
        }];
        let unfanned = vec![Point2f64::new(6.0, 2.0)];
        let out = CeilingRegions::default_for_footprint(vec![hex], 2.4, cid)
            .with_apexes_hinted_unfanned(&apexes, &[], &unfanned);
        let again = out
            .clone()
            .with_apexes_hinted_unfanned(&apexes, &[], &unfanned);
        assert_eq!(again.regions.len(), out.regions.len(), "same face count");
        for before in &out.regions {
            assert!(
                again
                    .regions
                    .iter()
                    .any(|after| cyclically_equal(&after.polygon, &before.polygon)),
                "face lost by re-apply: {:?}",
                before.polygon
            );
        }
    }

    /// The field-report drawing (bug ee77622f, 2026-07-12): rectangle
    /// x[1.52, 6.52] × y[2.31, 7.31] plus a south-east BAY x[5.52, 6.52] ×
    /// y[7.31, 8.31], gable ridge (1.52, 4.66)–(6.52, 4.66) at 3.0 over base
    /// 2.0.
    fn bay_gable_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(1.52, 2.31),
            Point2f64::new(6.52, 2.31),
            Point2f64::new(6.52, 8.31),
            Point2f64::new(5.52, 8.31),
            Point2f64::new(5.52, 7.31),
            Point2f64::new(1.52, 7.31),
        ]
    }

    fn bay_gable_apexes() -> Vec<CeilingApex> {
        vec![
            CeilingApex {
                point: Point2f64::new(1.52, 4.66),
                z_m: 3.0,
            },
            CeilingApex {
                point: Point2f64::new(6.52, 4.66),
                z_m: 3.0,
            },
        ]
    }

    /// THE ee77622f REGRESSION (the assessor's voice note: "it should just
    /// be two planes, one of them extends a bit more over the bay"): the
    /// gable over the bay footprint solves to EXACTLY 2 planes with ZERO
    /// taps — the south plane is pitched to the MAIN south wall (base z at
    /// y = 7.31) and CONTINUES over the bay, whose corners embed BELOW base
    /// on the same plane; no midline-split break, no bay fan facets.
    #[test]
    fn gable_over_bay_is_two_planes_with_bay_embedded_below_base() {
        let cid = ItemId::default();
        let apexes = bay_gable_apexes();
        let out = CeilingRegions::default_for_footprint(vec![bay_gable_ring()], 2.0, cid)
            .with_apexes(&apexes); // EMPTY stored unfanned set — all automatic
        assert_eq!(
            out.regions.len(),
            2,
            "two planes, nothing else: {:?}",
            out.regions.iter().map(|r| &r.polygon).collect::<Vec<_>>()
        );
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 26.0).abs() < 1e-6, "rect + bay fully covered");
        for r in &out.regions {
            assert!(is_planar(r), "non-planar face: {:?}", r.polygon);
            assert!(r.flat_z().is_none(), "each face genuinely slopes");
        }
        // The south plane: z = 3.0 − (y − 4.66)/2.65 → 1.6226… at the bay's
        // far wall (y = 8.31), BELOW the 2.0 base.
        let z_bay = 3.0 - 3.65 / 2.65;
        let south = out
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            })
            .expect("the south plane reaches the bay");
        let at = |x: f64, y: f64| {
            south
                .polygon
                .exterior
                .iter()
                .find(|v| (v.x - x).abs() <= 1e-9 && (v.y - y).abs() <= 1e-9)
                .unwrap_or_else(|| panic!("south plane misses ({x}, {y}): {:?}", south.polygon))
        };
        for (x, y) in [(5.52, 8.31), (6.52, 8.31)] {
            let v = at(x, y);
            assert!(
                (v.z - z_bay).abs() <= 1e-9 && v.z < 2.0 - 0.1,
                "bay corner ({x}, {y}) embeds below base on the plane, got z = {}",
                v.z
            );
        }
        assert!(
            (at(1.52, 7.31).z - 2.0).abs() <= 1e-9 && (at(5.52, 7.31).z - 2.0).abs() <= 1e-9,
            "the main south wall stays the eave (base z)"
        );
        // No midline-split artifact survives: nothing at x = 4.02 off-plane
        // (the merge's collinear pass folds the on-plane split point away).
        assert!(
            south
                .polygon
                .exterior
                .iter()
                .all(|v| (v.x - 4.02).abs() > 1e-6),
            "the midline split point must merge away: {:?}",
            south.polygon
        );

        // RE-APPLY identity: the two planes reproduce themselves.
        let again = out.clone().with_apexes(&apexes);
        assert_eq!(again.regions.len(), 2, "re-apply keeps the two planes");
        let total: f64 = again.regions.iter().map(plan_area).sum();
        assert!((total - 26.0).abs() < 1e-6);

        // The dump's ACTUAL stored state — the assessor had already tapped
        // (5.52, 7.31): the toggle re-pins a vertex that sits ON the south
        // plane anyway, so the roof stays exactly the SAME two planes.
        let stored = vec![Point2f64::new(5.52, 7.31)];
        let out = CeilingRegions::default_for_footprint(vec![bay_gable_ring()], 2.0, cid)
            .with_apexes_hinted_unfanned(&apexes, &[], &stored);
        assert_eq!(
            out.regions.len(),
            2,
            "the field dump's stored set still solves to 2 planes"
        );
        let south = out
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            })
            .expect("south plane present under the stored set");
        for v in &south.polygon.exterior {
            let expect = if (v.y - 4.66).abs() <= 1e-9 {
                3.0 // the ridge
            } else if (v.y - 7.31).abs() <= 1e-9 {
                2.0 // the main-wall eave (incl. the re-pinned (5.52, 7.31))
            } else {
                z_bay // the bay's far wall, below base on the plane
            };
            assert!(
                (v.z - expect).abs() <= 1e-9,
                "stored-set south plane vertex {v:?} expected z {expect}"
            );
        }
    }

    /// Fix for the reviewer's base_z collapse: a gable with bays on BOTH
    /// sides auto-embeds eave corners on BOTH member faces, so neither has a
    /// level base off its apex corners alone — the group base recovery must
    /// skip the AUTO-embedded vertices (not just the stored set) so the
    /// anchors still vote and `base_z` stays sane (no warn-collapse to 0).
    #[test]
    fn double_bay_gable_group_recovers_base_z() {
        let cid = ItemId::default();
        // 10×5 rectangle with a 2×1 bay off BOTH long walls.
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, -1.0),
            Point2f64::new(6.0, -1.0),
            Point2f64::new(6.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 5.0),
            Point2f64::new(6.0, 5.0),
            Point2f64::new(6.0, 6.0),
            Point2f64::new(4.0, 6.0),
            Point2f64::new(4.0, 5.0),
            Point2f64::new(0.0, 5.0),
        ];
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(0.0, 2.5),
                z_m: 4.0,
            },
            CeilingApex {
                point: Point2f64::new(10.0, 2.5),
                z_m: 4.0,
            },
        ];
        let out = CeilingRegions::default_for_footprint(vec![ring], 2.0, cid).with_apexes(&apexes);
        assert_eq!(out.regions.len(), 2, "two planes, both bays embedded");
        let groups = out.apex_groups(&apexes);
        assert_eq!(groups.len(), 1);
        assert!(
            (groups[0].base_z - 2.0).abs() <= 1e-9,
            "the eave anchors still vote the base: got {}",
            groups[0].base_z
        );
        // Both faces stay in the merged ring (rect 50 + 2 bays of 2 m²).
        let ring_area: f64 = groups[0]
            .merged_rings
            .iter()
            .map(|ring| {
                let n = ring.len();
                (0..n)
                    .map(|i| {
                        let (p, q) = (ring[i], ring[(i + 1) % n]);
                        p.x * q.y - q.x * p.y
                    })
                    .sum::<f64>()
                    .abs()
                    / 2.0
            })
            .sum();
        assert!(
            (ring_area - 54.0).abs() < 1e-6,
            "merged ring keeps both embedded faces (got {ring_area})"
        );
    }

    /// The test2 field report's T-shaped plan: a 6×5 rect x[1.3, 7.3] ×
    /// y[2.45, 7.45] with a 2×2 stem x[3.3, 5.3] × y[7.45, 9.45] centred on
    /// the south side. Base 2.0.
    fn t_plan_ring() -> Vec<Point2f64> {
        vec![
            Point2f64::new(1.3, 2.45),
            Point2f64::new(1.3, 7.45),
            Point2f64::new(3.3, 7.45),
            Point2f64::new(3.3, 9.45),
            Point2f64::new(5.3, 9.45),
            Point2f64::new(5.3, 7.45),
            Point2f64::new(7.3, 7.45),
            Point2f64::new(7.3, 2.45),
        ]
    }

    /// TEST2 REGRESSION, horizontal mid-ridge: the gable across the T's main
    /// rect solves to exactly 2 planes anchored at the OUTER y = 7.45
    /// corners — the reflex armpits (3.3/5.3, 7.45) are at the same 2.5 m
    /// ridge distance but are INELIGIBLE (concave), and the dominant-eave
    /// band (the two outer wall runs, 4 m total, vs the stem's 2 m) keeps
    /// the pitch on the main walls. The stem embeds below base on the south
    /// plane.
    #[test]
    fn t_plan_horizontal_ridge_is_two_planes_with_the_stem_embedded() {
        let cid = ItemId::default();
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(1.3, 4.95),
                z_m: 3.0,
            },
            CeilingApex {
                point: Point2f64::new(7.3, 4.95),
                z_m: 3.0,
            },
        ];
        let out = CeilingRegions::default_for_footprint(vec![t_plan_ring()], 2.0, cid)
            .with_apexes(&apexes);
        assert_eq!(
            out.regions.len(),
            2,
            "two planes over the T: {:?}",
            out.regions.iter().map(|r| &r.polygon).collect::<Vec<_>>()
        );
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 34.0).abs() < 1e-6, "rect (30) + stem (4) covered");
        // South plane: eave at y = 7.45 (z 2.0, incl. the reflex armpits ON
        // the plane), stem south wall embedded at 3.0 − 4.5/2.5 = 1.2.
        let south = out
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.y - 9.45).abs() <= 1e-9)
            })
            .expect("the south plane reaches the stem");
        for v in &south.polygon.exterior {
            let expect = if (v.y - 4.95).abs() <= 1e-9 {
                3.0
            } else if (v.y - 7.45).abs() <= 1e-9 {
                2.0
            } else {
                1.2
            };
            assert!(
                (v.z - expect).abs() <= 1e-9,
                "south plane vertex {v:?} expected z {expect}"
            );
        }
        // The markers agree: anchors on OUTER (convex) corners only.
        let roles = gable_ring_roles(
            &t_plan_ring(),
            Point2f64::new(1.3, 4.95),
            Point2f64::new(7.3, 4.95),
            &[],
        )
        .expect("a true gable");
        let ring = t_plan_ring();
        for (i, v) in ring.iter().enumerate() {
            let armpit = ((v.x - 3.3).abs() <= 1e-9 || (v.x - 5.3).abs() <= 1e-9)
                && (v.y - 7.45).abs() <= 1e-9;
            if armpit {
                assert!(
                    !roles.eligible[i] && !roles.anchor[i],
                    "reflex armpit {v:?} must never anchor"
                );
            }
        }
        assert!(
            roles.anchor.iter().filter(|a| **a).count() == 2,
            "one anchor per side"
        );
    }

    /// TEST2 REGRESSION, vertical stem-axis ridge (endpoints on the north
    /// edge and the stem's south edge): the DOMINANT-EAVE rule pitches each
    /// side on the 5 m MAIN wall (band length 5 beats the stem wall's 2,
    /// despite the stem wall being nearer at 1 m — and the reflex armpit at
    /// 1 m is ineligible anyway). Two planes; the stem's side corners embed
    /// ABOVE base (the stem tucks under the big roof, its walls rising into
    /// it via the roof-void split).
    #[test]
    fn t_plan_stem_axis_ridge_pitches_on_the_dominant_main_walls() {
        let cid = ItemId::default();
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(4.3, 2.45),
                z_m: 3.0,
            },
            CeilingApex {
                point: Point2f64::new(4.3, 9.45),
                z_m: 3.0,
            },
        ];
        let out = CeilingRegions::default_for_footprint(vec![t_plan_ring()], 2.0, cid)
            .with_apexes(&apexes);
        assert_eq!(
            out.regions.len(),
            2,
            "two planes along the stem axis: {:?}",
            out.regions.iter().map(|r| &r.polygon).collect::<Vec<_>>()
        );
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 34.0).abs() < 1e-6);
        // West plane: pitched to the main wall x = 1.3 (dist 3.0) → slope
        // 1/3 per metre; the stem's west wall (x = 3.3, dist 1.0) embeds at
        // 3.0 − 1/3 ≈ 2.667, ABOVE the 2.0 base; the main wall stays the
        // 2.0 eave. Every face vertex z is on its plane.
        let z_stem = 3.0 - 1.0 / 3.0;
        let west = out
            .regions
            .iter()
            .find(|r| r.polygon.exterior.iter().any(|v| (v.x - 1.3).abs() <= 1e-9))
            .expect("the west plane");
        for v in &west.polygon.exterior {
            let expect = if (v.x - 4.3).abs() <= 1e-9 {
                3.0 // the ridge
            } else if (v.x - 1.3).abs() <= 1e-9 {
                2.0 // the dominant main-wall eave
            } else {
                z_stem // the stem wall, tucked under the plane
            };
            assert!(
                (v.z - expect).abs() <= 1e-9,
                "west plane vertex {v:?} expected z {expect}"
            );
        }
        assert!(
            (z_stem - 2.0) > 0.5,
            "the stem corners sit clearly above base — the roof passes over the stem"
        );
    }

    /// THE test2 ON-DEVICE REGRESSION: a stale delta-0 step DIVIDER (it
    /// survives canonicalize and is filtered out of the delete feed, so the
    /// assessor can neither see nor remove it) had split the T into
    /// rect + stem BEFORE the apexes — the ridge then claimed only the rect
    /// and the stem silently dropped out of the roof (the field dump's
    /// 5-region / rect-only merged ring). The same-height ABSORPTION now
    /// fuses the pieces back before shaping: two planes over the whole T.
    #[test]
    fn stale_divider_split_t_still_solves_the_full_gable() {
        let cid = ItemId::default();
        // The device's exact plan (f32-offset coordinates from the dump).
        let ring = vec![
            Point2f64::new(1.2961, 2.4527),
            Point2f64::new(1.2961, 7.4527),
            Point2f64::new(3.2961, 7.4527),
            Point2f64::new(3.2961, 9.4527),
            Point2f64::new(5.2961, 9.4527),
            Point2f64::new(5.2961, 7.4527),
            Point2f64::new(7.2961, 7.4527),
            Point2f64::new(7.2961, 2.4527),
        ];
        let stem = vec![
            Point2f64::new(3.2961, 7.4527),
            Point2f64::new(3.2961, 9.4527),
            Point2f64::new(5.2961, 9.4527),
            Point2f64::new(5.2961, 7.4527),
        ];
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(1.2961, 4.9527),
                z_m: 3.0,
            },
            CeilingApex {
                point: Point2f64::new(7.2961, 4.9527),
                z_m: 3.0,
            },
        ];
        let split = CeilingRegions::default_for_footprint(vec![ring], 2.0, cid)
            .with_z_deltas(&[(stem, 0.0)]); // the invisible divider
        assert_eq!(split.regions.len(), 2, "the divider split the T");
        let out = split.with_apexes(&apexes);
        assert_eq!(
            out.regions.len(),
            2,
            "the absorption re-fuses the pieces: two planes over the whole T"
        );
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 34.0).abs() < 1e-6, "rect + stem all covered");
        let groups = out.apex_groups(&apexes);
        assert_eq!(groups.len(), 1);
        let ring_area: f64 = groups[0]
            .merged_rings
            .iter()
            .map(|ring| {
                let n = ring.len();
                (0..n)
                    .map(|i| {
                        let (p, q) = (ring[i], ring[(i + 1) % n]);
                        p.x * q.y - q.x * p.y
                    })
                    .sum::<f64>()
                    .abs()
                    / 2.0
            })
            .sum();
        assert!(
            (ring_area - 34.0).abs() < 1e-6,
            "the group's merged ring keeps the stem (got {ring_area})"
        );
        // The stem embeds below base on the south plane.
        let south = out
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.y - 9.4527).abs() <= 1e-9)
            })
            .expect("the south plane spans the stem");
        for v in south
            .polygon
            .exterior
            .iter()
            .filter(|v| (v.y - 9.4527).abs() <= 1e-9)
        {
            assert!(
                v.z < 2.0 - 0.5,
                "stem wall embeds well below base, got {}",
                v.z
            );
        }
    }

    /// A ridge with INTERIOR endpoints keeps the hip solve — no gable
    /// auto-unfan: every boundary corner stays a fan target at base z.
    #[test]
    fn interior_ridge_keeps_the_hip_fan_to_all_corners() {
        let cid = ItemId::default();
        let hex = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(6.0, 2.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(0.0, 4.0),
            Point2f64::new(-2.0, 2.0),
        ];
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(1.0, 2.0),
                z_m: 4.5,
            },
            CeilingApex {
                point: Point2f64::new(3.0, 2.0),
                z_m: 4.5,
            },
        ];
        let out =
            CeilingRegions::default_for_footprint(vec![hex.clone()], 2.4, cid).with_apexes(&apexes);
        assert!(
            out.regions.len() > 2,
            "an interior ridge is a hip, not a gable ({} faces)",
            out.regions.len()
        );
        // Every hexagon corner keeps its base z somewhere in the output —
        // nothing was auto-embedded.
        for p in &hex {
            assert!(
                out.regions.iter().any(|r| {
                    r.polygon.exterior.iter().any(|v| {
                        (v.x - p.x).abs() <= 1e-9
                            && (v.y - p.y).abs() <= 1e-9
                            && (v.z - 2.4).abs() <= 1e-9
                    })
                }),
                "corner {p:?} must stay a base-z fan target"
            );
        }
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 24.0).abs() < 1e-6);
    }

    /// PRESENTATION CONSISTENCY for a legacy in-scope/out-of-scope
    /// construction SHARE (the "final house" record): with an InterStorey
    /// region and an exposed nib both pointing at one COMPLETE construction,
    /// every counter reads the IN-SCOPE membership — progress (1, 1), the
    /// kind complete, nothing remaining. A 4/5-with-all-ticks state can't
    /// arise from the share itself.
    #[test]
    fn interstorey_construction_share_counts_only_the_in_scope_member() {
        let mut ber = Ber::default();
        let cid = ber.ensure_ceiling_construction(
            CeilingConstructionType::FlatRoof,
            ConstructionMakeup::Unknown,
            ConstructionPhase::default(),
        );
        let region = |x0: f64, x1: f64, exposure: CeilingExposure| CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(x0, 0.0, 2.4),
                Point3f64::new(x1, 0.0, 2.4),
                Point3f64::new(x1, 2.0, 2.4),
                Point3f64::new(x0, 2.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: cid,
            exposure,
        };
        ber.storeys.push(Storey {
            ceilings: CeilingRegions {
                regions: vec![
                    region(0.0, 4.0, CeilingExposure::InterStorey),
                    region(4.0, 6.0, CeilingExposure::ExposedRoof),
                ],
            },
            ..Storey::default()
        });
        assert_eq!(
            ber.structure_surface_progress(0, SurfaceKind::Ceilings),
            (1, 1),
            "the buried sharer is in neither counter"
        );
        assert!(ber
            .incomplete_surface_ids(0, SurfaceKind::Ceilings)
            .is_empty());
        assert!(ber.structure_surfaces_complete(0, SurfaceKind::Ceilings));
    }

    /// `Storey::ceiling_unfanned` and `Storey::ceiling_plane_anchors` are
    /// serde-ADDITIVE: an old record without either field loads with empty
    /// sets (fan-everything, auto-nearest anchors), and populated sets
    /// round-trip.
    #[test]
    fn storey_ceiling_unfanned_serde_roundtrip_and_default() {
        let mut storey = Storey::default();
        storey.ceiling_unfanned = vec![(6.0, 2.0), (-2.0, 2.0)];
        storey.ceiling_plane_anchors = vec![(5.52, 8.31)];
        storey.ceiling_continuations = vec![CeilingContinuation {
            target: (1.0, 1.0),
            source: (3.0, 1.0),
            source_storey_offset: -1,
        }];
        let json = serde_json::to_value(&storey).unwrap();
        let back: Storey = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.ceiling_unfanned, vec![(6.0, 2.0), (-2.0, 2.0)]);
        assert_eq!(back.ceiling_plane_anchors, vec![(5.52, 8.31)]);
        assert_eq!(back.ceiling_continuations, storey.ceiling_continuations);
        // A continuation entry saved before `source_storey_offset` existed
        // defaults to SAME-storey.
        let mut entry = serde_json::to_value(&storey.ceiling_continuations[0]).unwrap();
        entry
            .as_object_mut()
            .unwrap()
            .remove("source_storey_offset");
        let legacy_entry: CeilingContinuation = serde_json::from_value(entry).unwrap();
        assert_eq!(legacy_entry.source_storey_offset, 0);
        // An "old" record: the same JSON with the fields removed.
        let mut old = json;
        old.as_object_mut().unwrap().remove("ceiling_unfanned");
        old.as_object_mut().unwrap().remove("ceiling_plane_anchors");
        old.as_object_mut().unwrap().remove("ceiling_continuations");
        let legacy: Storey = serde_json::from_value(old).unwrap();
        assert!(
            legacy.ceiling_unfanned.is_empty(),
            "records saved before the field default to fan-everything"
        );
        assert!(
            legacy.ceiling_plane_anchors.is_empty(),
            "records saved before the field default to auto-nearest anchors"
        );
        assert!(
            legacy.ceiling_continuations.is_empty(),
            "records saved before the field default to no continuations"
        );
    }

    /// A gable-solve regression for the OVERRIDE path at the model level:
    /// with the bay corner stored as the plane anchor, the ee77622f south
    /// plane pitches to the BAY wall (main-wall corners embed ABOVE base)
    /// and stays one plane; hips ignore the set entirely.
    #[test]
    fn plane_anchor_override_repitches_the_gable_and_hips_ignore_it() {
        let cid = ItemId::default();
        let apexes = bay_gable_apexes();
        let overrides = vec![Point2f64::new(5.52, 8.31)];
        let out = CeilingRegions::default_for_footprint(vec![bay_gable_ring()], 2.0, cid)
            .with_apexes_shaped(&apexes, &[], &[], &overrides);
        assert_eq!(out.regions.len(), 2, "still two planes under the override");
        let south = out
            .regions
            .iter()
            .find(|r| {
                r.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - 5.52).abs() <= 1e-9 && (v.y - 8.31).abs() <= 1e-9)
            })
            .expect("south plane");
        let z_main = 3.0 - 2.65 / 3.65; // ≈ 2.274, ABOVE the 2.0 base
        for v in &south.polygon.exterior {
            let expect = if (v.y - 4.66).abs() <= 1e-9 {
                3.0
            } else if (v.y - 8.31).abs() <= 1e-9 {
                2.0 // the overridden anchor wall — the new eave
            } else {
                z_main
            };
            assert!(
                (v.z - expect).abs() <= 1e-9,
                "override south plane vertex {v:?} expected z {expect}"
            );
        }
        // The base recovery still works with the override supplied.
        let groups = out.apex_groups_shaped(&apexes, &[], &overrides);
        assert_eq!(groups.len(), 1);
        assert!(
            (groups[0].base_z - 2.0).abs() <= 1e-9,
            "the north anchors still vote the base: got {}",
            groups[0].base_z
        );
        // A HIP (interior endpoints) ignores the anchor set entirely.
        let hip_apexes = vec![
            CeilingApex {
                point: Point2f64::new(3.0, 4.66),
                z_m: 3.0,
            },
            CeilingApex {
                point: Point2f64::new(5.0, 4.66),
                z_m: 3.0,
            },
        ];
        let plain = CeilingRegions::default_for_footprint(vec![bay_gable_ring()], 2.0, cid)
            .with_apexes(&hip_apexes);
        let with_set = CeilingRegions::default_for_footprint(vec![bay_gable_ring()], 2.0, cid)
            .with_apexes_shaped(&hip_apexes, &[], &[], &overrides);
        assert_eq!(plain.regions.len(), with_set.regions.len());
        for before in &plain.regions {
            assert!(
                with_set
                    .regions
                    .iter()
                    .any(|after| cyclically_equal(&after.polygon, &before.polygon)),
                "hip face changed by the anchor set: {:?}",
                before.polygon
            );
        }
    }

    /// RE-APPLY-TO-SELF IDENTITY for a TRANSITION triangle `(w, A, B)`, for
    /// all three positions of w's ridge parameter: t(w) interior to [0, 1]
    /// (it comes back as a transition triangle), t(w) < 0 and t(w) > 1 (it
    /// comes back as a HUB triangle, possibly rotated — hence cyclic
    /// equality). PRE-LIFT is what makes the corners hold their apex z.
    #[test]
    fn ridge_transition_triangle_reapply_is_identity_for_all_t_cases() {
        let (a, b) = gable_apexes();
        let apexes = vec![a, b];
        for w in [
            Point3f64::new(5.0, 4.0, 2.4),  // t(w) = 0.5: interior
            Point3f64::new(0.0, 4.0, 2.4),  // t(w) < 0: behind A
            Point3f64::new(10.0, 4.0, 2.4), // t(w) > 1: beyond B
        ] {
            let tri = CeilingRegions {
                regions: vec![CeilingRegion {
                    id: ItemId::default(),
                    polygon: Polygon3D::new(vec![
                        w,
                        Point3f64::new(2.0, 2.0, 4.2),
                        Point3f64::new(8.0, 2.0, 4.2),
                    ]),
                    opes: Opes::default(),
                    construction_id: ItemId::default(),
                    exposure: CeilingExposure::default(),
                }],
            };
            let before = tri.regions[0].polygon.clone();
            let out = tri.with_apexes(&apexes);
            assert_eq!(out.regions.len(), 1, "one triangle survives for w={w:?}");
            assert!(
                cyclically_equal(&out.regions[0].polygon, &before),
                "re-apply identity for w={w:?}: got {:?}",
                out.regions[0].polygon
            );
        }
    }

    /// RE-APPLY-TO-SELF IDENTITY for END (hub) triangles: an end triangle
    /// contains only its own apex (as a corner), so it re-applies through
    /// the FAN branch and comes back unchanged.
    #[test]
    fn ridge_hub_triangle_reapply_is_identity() {
        let (a, b) = gable_apexes();
        let apexes = vec![a, b];
        let gable = gable_footprint(ItemId::default()).with_apexes(&apexes);
        let hubs: Vec<&CeilingRegion> = gable
            .regions
            .iter()
            .filter(|r| apex_vertex_count(r, &apexes) == 1)
            .collect();
        assert_eq!(hubs.len(), 2, "the two end triangles");
        for r in hubs {
            let before = r.polygon.clone();
            let out = CeilingRegions {
                regions: vec![r.clone()],
            }
            .with_apexes(&apexes);
            assert_eq!(out.regions.len(), 1);
            assert!(
                cyclically_equal(&out.regions[0].polygon, &before),
                "hub re-fan identity: got {:?}",
                out.regions[0].polygon
            );
        }
    }

    /// SLOPING ridge (zA ≠ zB — the two-point flow with different heights):
    /// every triangle vertex at apex A's plan position carries zA, at B's
    /// carries zB, and the boundary keeps the base.
    #[test]
    fn with_apexes_sloping_ridge_carries_each_apexes_z() {
        let a = CeilingApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.0,
        };
        let b = CeilingApex {
            point: Point2f64::new(8.0, 2.0),
            z_m: 4.5,
        };
        let out = gable_footprint(ItemId::default()).with_apexes(&[a.clone(), b.clone()]);
        let at =
            |v: &Point3f64, p: Point2f64| (v.x - p.x).abs() <= 1e-9 && (v.y - p.y).abs() <= 1e-9;
        let (mut saw_a, mut saw_b) = (false, false);
        for r in &out.regions {
            for v in &r.polygon.exterior {
                if at(v, a.point) {
                    assert!((v.z - 4.0).abs() < 1e-9, "A end at zA");
                    saw_a = true;
                } else if at(v, b.point) {
                    assert!((v.z - 4.5).abs() < 1e-9, "B end at zB");
                    saw_b = true;
                } else {
                    assert!((v.z - 2.4).abs() < 1e-9, "boundary keeps the base z");
                }
            }
        }
        assert!(saw_a && saw_b, "both ridge ends appear in the roof");
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 40.0).abs() < 1e-6);
    }

    /// SLIVER DROP: an apex sitting ON a ring corner makes its incident hub
    /// triangle and one transition zero-area — both drop, and the remaining
    /// triangles still tile the footprint with nothing under the threshold.
    #[test]
    fn ridge_triangles_drop_corner_apex_slivers() {
        let a = CeilingApex {
            point: Point2f64::new(0.0, 0.0), // ON the footprint corner
            z_m: 4.0,
        };
        let b = CeilingApex {
            point: Point2f64::new(8.0, 2.0),
            z_m: 4.0,
        };
        let out = gable_footprint(ItemId::default()).with_apexes(&[a, b]);
        // The midline split inserts two boundary vertices; the two zero-area
        // slivers at the corner apex (its hub triangle and the (0,0)
        // transition) drop; and on the bottom edge — whose corner end was
        // pre-lifted to the apex, so the split vertex's z interpolates DOWN
        // THE SLOPE — the hub triangle and its transition are coplanar and
        // merge back into one clean triangle ((10,0), B, A): 5 faces.
        assert_eq!(out.regions.len(), 5, "slivers dropped, south slope merged");
        for r in &out.regions {
            assert!(plan_area(r) >= 1e-6, "no sliver emitted: {:?}", r.polygon);
        }
        let total: f64 = out.regions.iter().map(plan_area).sum();
        assert!((total - 40.0).abs() < 1e-6, "tiling survives the drops");
    }

    /// DEGENERATE ridge: two apexes at one plan point fall back to a single
    /// fan at the first (warn + proceed — resolve never panics).
    #[test]
    fn with_apexes_coincident_apexes_fall_back_to_a_fan() {
        let (a, _) = gable_apexes();
        let dup = a.clone();
        let fanned = gable_footprint(ItemId::default()).with_apexes(&[a.clone()]);
        let ridged = gable_footprint(ItemId::default()).with_apexes(&[a, dup]);
        let polys = |r: &CeilingRegions| -> Vec<Polygon3D> {
            r.regions.iter().map(|x| x.polygon.clone()).collect()
        };
        assert_eq!(polys(&ridged), polys(&fanned), "fallback equals the fan");
    }

    /// ≥3 apexes in one region: warn + ridge from the FIRST TWO in apex-list
    /// order — deterministic, never a panic.
    #[test]
    fn with_apexes_three_apexes_use_the_first_two() {
        let (a, b) = gable_apexes();
        let c = CeilingApex {
            point: Point2f64::new(5.0, 1.0),
            z_m: 5.0,
        };
        let two = gable_footprint(ItemId::default()).with_apexes(&[a.clone(), b.clone()]);
        let three = gable_footprint(ItemId::default()).with_apexes(&[a, b, c]);
        let polys = |r: &CeilingRegions| -> Vec<Polygon3D> {
            r.regions.iter().map(|x| x.polygon.clone()).collect()
        };
        assert_eq!(polys(&three), polys(&two), "third apex ignored");
    }

    /// GROUPING: a ridge's end triangles carry only their own apex, but the
    /// merged trapezoids carry BOTH — transitive closure links A and B
    /// into ONE group, whose merged ring is the original footprint.
    #[test]
    fn apex_groups_link_a_ridges_apexes_transitively_and_merge_to_the_footprint() {
        use geo::Area;
        let (a, b) = gable_apexes();
        let apexes = vec![a, b];
        let gable = gable_footprint(ItemId::default()).with_apexes(&apexes);
        let groups = gable.apex_groups(&apexes);
        assert_eq!(groups.len(), 1, "the trapezoids link A and B");
        let g = &groups[0];
        assert_eq!(g.apex_idxs, vec![0, 1]);
        assert_eq!(g.region_idxs.len(), 4, "every face is a member");
        assert!((g.base_z - 2.4).abs() < 1e-9, "group base from the members");
        assert_eq!(g.merged_rings.len(), 1, "union is one simple ring");
        let ring = &g.merged_rings[0];
        let poly = Polygon::new(
            LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
            vec![],
        );
        assert!(
            (poly.unsigned_area() - 40.0).abs() < 1e-6,
            "merged ring = the original footprint"
        );
        for corner in [(0.0, 0.0), (10.0, 0.0), (10.0, 4.0), (0.0, 4.0)] {
            assert!(
                ring.iter()
                    .any(|p| (p.x - corner.0).abs() < 1e-9 && (p.y - corner.1).abs() < 1e-9),
                "footprint corner {corner:?} on the merged ring"
            );
        }
    }

    /// Two fans over DISJOINT rings share no region → two separate groups,
    /// each merging back to its own footprint square.
    #[test]
    fn apex_groups_keep_unlinked_fans_separate() {
        use geo::Area;
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.0,
            },
            CeilingApex {
                point: Point2f64::new(8.0, 2.0),
                z_m: 3.5,
            },
        ];
        let regions = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 4.0, 4.0), square(6.0, 0.0, 10.0, 4.0)],
            2.4,
            ItemId::default(),
        )
        .with_apexes(&apexes);
        let groups = regions.apex_groups(&apexes);
        assert_eq!(groups.len(), 2, "no shared region, no link");
        for (i, g) in groups.iter().enumerate() {
            assert_eq!(g.apex_idxs, vec![i], "groups ordered by apex index");
            assert_eq!(g.region_idxs.len(), 4, "each fan's 4 triangles");
            assert!((g.base_z - 2.4).abs() < 1e-9);
            assert_eq!(g.merged_rings.len(), 1);
            let ring = &g.merged_rings[0];
            let poly = Polygon::new(
                LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            );
            assert!((poly.unsigned_area() - 16.0).abs() < 1e-6);
        }
        // An apex matched by no region joins no group.
        assert!(regions
            .apex_groups(&[CeilingApex {
                point: Point2f64::new(50.0, 50.0),
                z_m: 4.0,
            }])
            .is_empty());
    }

    /// `ridge_hub_switches`: a convex ring with the ridge inside switches
    /// hubs exactly twice; a ring with a deep notch crossing the ridge axis
    /// switches more (the shape assign-time validation rejects); degenerate
    /// inputs report 0.
    #[test]
    fn ridge_hub_switches_convex_is_two_and_deep_notch_more() {
        let a = Point2f64::new(2.0, 2.0);
        let b = Point2f64::new(8.0, 2.0);
        assert_eq!(ridge_hub_switches(&square(0.0, 0.0, 10.0, 4.0), a, b), 2);
        // A deep slot cut from the right past the ridge midline: the slot's
        // far edge hubs to A between B-hubbed runs → 4 switches.
        let notched = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 1.0),
            Point2f64::new(3.0, 1.0),
            Point2f64::new(3.0, 3.0),
            Point2f64::new(10.0, 3.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        assert_eq!(ridge_hub_switches(&notched, a, b), 4);
        // Degenerate: coincident endpoints / too-small rings.
        assert_eq!(ridge_hub_switches(&square(0.0, 0.0, 10.0, 4.0), a, a), 0);
        assert_eq!(ridge_hub_switches(&[], a, b), 0);
        // A BOUNDARY-ended ridge (both ends on opposite walls — a gable)
        // still switches exactly 2 over a rectangle: the midline crosses the
        // two long edges only, so the existing ≤ 2 guard keeps working.
        assert_eq!(
            ridge_hub_switches(
                &square(0.0, 0.0, 10.0, 4.0),
                Point2f64::new(0.0, 2.0),
                Point2f64::new(10.0, 2.0)
            ),
            2
        );
    }

    /// `claim_regions` / `plan_contains_within`: strictly-inside points claim
    /// exactly their region; a point ON a region boundary (or within the
    /// [`APEX_BOUNDARY_EPS_M`] band of it) claims via the boundary arm; a
    /// point inside nothing and near no boundary claims nothing.
    #[test]
    fn claim_regions_strict_inside_then_boundary_band() {
        let regions = gable_footprint(ItemId::default()); // one 10×4 region
        assert_eq!(regions.claim_regions(Point2f64::new(5.0, 2.0)), vec![0]);
        // Exactly on the outline edge / a corner: the boundary band claims.
        assert_eq!(regions.claim_regions(Point2f64::new(0.0, 2.0)), vec![0]);
        assert_eq!(regions.claim_regions(Point2f64::new(10.0, 4.0)), vec![0]);
        // Displaced off the edge by less than the band: still claimed.
        assert_eq!(regions.claim_regions(Point2f64::new(-5e-5, 2.0)), vec![0]);
        // Beyond the band: nothing.
        assert!(regions.claim_regions(Point2f64::new(-1.0, 2.0)).is_empty());
        assert!(regions.claim_regions(Point2f64::new(5.0, 9.0)).is_empty());
    }

    /// THE SYMMETRIC BAND (field report cd36f510): a point within the
    /// [`APEX_BOUNDARY_EPS_M`] band of a shared boundary claims EVERY region
    /// touching it — even when float noise puts it strictly INSIDE one of
    /// them. The trigger was a junction ridge end recomputed as `lo + len`
    /// in f32, landing ~5e-7 m past the corner where two raised strips meet
    /// a third region: strictly inside the third, yet a rightful claim of
    /// all three.
    #[test]
    fn claim_band_is_symmetric_across_a_junction_corner() {
        let cid = ItemId::default();
        let mut r =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 1.0)], 2.9, cid);
        for (ring, z) in [
            (square(0.0, -1.0, 4.0, 0.0), 2.5),
            (square(4.0, -1.0, 8.0, 1.0), 2.4),
        ] {
            r.regions
                .extend(CeilingRegions::default_for_footprint(vec![ring], z, cid).regions);
        }
        // ε past the strips' shared corner (4, 0), strictly inside region 2.
        let p = Point2f64::new(4.0 + 5e-7, 0.0);
        assert_eq!(r.claim_regions(p), vec![0, 1, 2], "all three claim");
        // ε inside region 0 near the internal junction y = 0: both strips.
        let p = Point2f64::new(2.0, 5e-7);
        assert_eq!(r.claim_regions(p), vec![0, 1], "both strips claim");
        // Strictly inside and beyond the band: that region alone.
        assert_eq!(r.claim_regions(Point2f64::new(2.0, 0.5)), vec![0]);
    }

    /// MULTI-CLAIM DETERMINISM (the legacy / hand-edited shared-boundary
    /// case validation now prevents creating): an apex on the boundary
    /// SHARED by two regions claims both, in ascending region order, and
    /// `with_apexes` hands it to the FIRST — identically on every resolve.
    #[test]
    fn shared_boundary_apex_claims_are_deterministic_and_first_claim_wins() {
        let cid = ItemId::default();
        let two = || {
            let mut r =
                CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid);
            r.regions.extend(
                CeilingRegions::default_for_footprint(vec![square(4.0, 0.0, 8.0, 4.0)], 2.4, cid)
                    .regions,
            );
            r
        };
        let apex = CeilingApex {
            point: Point2f64::new(4.0, 2.0), // ON the shared boundary x = 4
            z_m: 4.2,
        };
        assert_eq!(
            two().claim_regions(apex.point),
            vec![0, 1],
            "both claim, ascending region order"
        );
        let fingerprint = |r: &CeilingRegions| {
            let mut vs: Vec<(f64, f64, f64)> = r
                .regions
                .iter()
                .flat_map(|reg| reg.polygon.exterior.iter().map(|v| (v.x, v.y, v.z)))
                .collect();
            vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            vs
        };
        let once = two().with_apexes(std::slice::from_ref(&apex));
        let again = two().with_apexes(std::slice::from_ref(&apex));
        assert_eq!(
            fingerprint(&once),
            fingerprint(&again),
            "first-claim is deterministic across resolves"
        );
        // The FIRST region (index 0) was fanned; the second is untouched.
        assert!(
            once.regions.iter().any(|r| r
                .polygon
                .exterior
                .iter()
                .any(|v| (v.z - 4.2).abs() < 1e-9)),
            "the claiming region rises to the apex"
        );
        let untouched: Vec<_> = once
            .regions
            .iter()
            .filter(|r| r.polygon.exterior.iter().all(|v| v.x >= 4.0 - 1e-9))
            .collect();
        assert!(
            untouched.iter().any(|r| r.polygon.exterior.len() == 4
                && r.polygon.exterior.iter().all(|v| (v.z - 2.4).abs() < 1e-9)),
            "the second region passes through flat"
        );
    }

    /// A wall-to-wall (BOUNDARY) ridge — both ends ON opposite outline walls
    /// — makes a GABLE: the wall-edge hub triangles are plan-degenerate and
    /// sliver-drop, the rest merge into exactly TWO planar slope faces, and
    /// re-applying the apexes to the output is the identity (the reload
    /// fixpoint's resolve half).
    #[test]
    fn with_apexes_boundary_ridge_makes_two_gable_slopes_and_reapplies_identically() {
        let cid = ItemId::default();
        let apexes = [
            CeilingApex {
                point: Point2f64::new(2.0, 0.0), // ON the bottom wall
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(2.0, 4.0), // ON the top wall
                z_m: 4.2,
            },
        ];
        let base =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid);
        let out = base.with_apexes(&apexes);
        assert_eq!(out.regions.len(), 2, "two slope faces, nothing else");
        for r in &out.regions {
            assert!(is_planar(r), "each slope is one plane");
            let zs: Vec<f64> = r.polygon.exterior.iter().map(|v| v.z).collect();
            assert!(zs.iter().any(|z| (z - 4.2).abs() < 1e-9), "ridge edge");
            assert!(zs.iter().any(|z| (z - 2.4).abs() < 1e-9), "eaves edge");
        }
        // Plan coverage: the two slopes tile the footprint (8 m² each).
        use geo::Area;
        let total: f64 = out
            .regions
            .iter()
            .map(|r| Polygon::<f64>::from(r.polygon.clone()).unsigned_area())
            .sum();
        assert!((total - 16.0).abs() < 1e-9, "slopes tile the footprint");

        // Re-apply identity: the gable apexes are claimed (via the boundary
        // band — they are corners of the slope faces) and reproduce the
        // same two faces.
        let fingerprint = |r: &CeilingRegions| {
            let mut vs: Vec<(f64, f64, f64)> = r
                .regions
                .iter()
                .flat_map(|reg| reg.polygon.exterior.iter().map(|v| (v.x, v.y, v.z)))
                .collect();
            vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
            vs
        };
        let again = out.clone().with_apexes(&apexes);
        assert_eq!(fingerprint(&out), fingerprint(&again), "re-apply identity");
        // …and the grouping authority still links both apexes to the slopes.
        let groups = again.apex_groups(&apexes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].apex_idxs, vec![0, 1]);
    }

    /// A mono-pitch ridge on the boundary SHARED by two areas: the side HINT
    /// picks which one slopes. Hinting the RIGHT area slopes it (and leaves the
    /// LEFT flat); WITHOUT a hint, first-claim slopes the LEFT (region 0).
    #[test]
    fn with_apexes_hint_picks_the_sloped_side_of_a_shared_boundary() {
        let cid = ItemId::default();
        // Left area x0..4, right area x4..8, both flat at 2.4.
        let mut ceilings =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid);
        ceilings.regions.extend(
            CeilingRegions::default_for_footprint(vec![square(4.0, 0.0, 8.0, 4.0)], 2.4, cid)
                .regions,
        );
        // A ridge along the shared line x = 4 at z = 4.0.
        let apexes = vec![
            CeilingApex {
                point: Point2f64::new(4.0, 0.0),
                z_m: 4.0,
            },
            CeilingApex {
                point: Point2f64::new(4.0, 4.0),
                z_m: 4.0,
            },
        ];
        // Does any output face on side `right_of_4` reach the ridge height?
        let side_slopes = |ceilings: &CeilingRegions, right_of_4: bool| {
            ceilings.regions.iter().any(|r| {
                let cx = r.polygon.exterior.iter().map(|v| v.x).sum::<f64>()
                    / r.polygon.exterior.len().max(1) as f64;
                let on_side = if right_of_4 { cx > 4.0 } else { cx < 4.0 };
                on_side && r.polygon.exterior.iter().any(|v| (v.z - 4.0).abs() < 1e-6)
            })
        };
        // Hint into the RIGHT area → it slopes, the left stays flat.
        let hints = vec![
            CeilingApexHint {
                apex: (4.0, 0.0),
                region: (6.0, 2.0),
            },
            CeilingApexHint {
                apex: (4.0, 4.0),
                region: (6.0, 2.0),
            },
        ];
        let hinted = ceilings.clone().with_apexes_hinted(&apexes, &hints);
        assert!(side_slopes(&hinted, true), "the hinted (right) area slopes");
        assert!(!side_slopes(&hinted, false), "the left area stays flat");
        // The apex group must claim ONLY the sloped side — even though the
        // apexes (4,0)/(4,4) are also CORNERS of the flat left area, that area
        // is merely touched, not shaped, so it stays free for its own ridge/step
        // (the bug: a junction ridge marking the flat neighbour "occupied").
        let groups = hinted.apex_groups(&apexes);
        assert_eq!(groups.len(), 1, "one group");
        let flat_left = hinted
            .regions
            .iter()
            .position(|r| {
                let cx = r.polygon.exterior.iter().map(|v| v.x).sum::<f64>()
                    / r.polygon.exterior.len().max(1) as f64;
                cx < 4.0
            })
            .expect("a left-side region");
        assert!(
            !groups[0].region_idxs.contains(&flat_left),
            "the flat left neighbour is NOT claimed by the group (got {:?})",
            groups[0].region_idxs
        );
        // No hint → first claim (region 0, the LEFT) slopes instead.
        let unhinted = ceilings.with_apexes_hinted(&apexes, &[]);
        assert!(
            side_slopes(&unhinted, false),
            "without a hint the left slopes"
        );
    }

    // ── rect (pavilion) ridges ──────────────────────────────────────────────

    /// The four corner apexes of the canonical pavilion, over the rect
    /// `(1,1)..(5,3)` at z = 4 — equal 1 m eaves offsets inside the 6×4
    /// footprint.
    fn pavilion_apexes() -> Vec<CeilingApex> {
        [(1.0, 1.0), (5.0, 1.0), (5.0, 3.0), (1.0, 3.0)]
            .into_iter()
            .map(|(x, y)| CeilingApex {
                point: Point2f64::new(x, y),
                z_m: 4.0,
            })
            .collect()
    }

    fn fingerprint(r: &CeilingRegions) -> Vec<(f64, f64, f64)> {
        let mut vs: Vec<(f64, f64, f64)> = r
            .regions
            .iter()
            .flat_map(|reg| reg.polygon.exterior.iter().map(|v| (v.x, v.y, v.z)))
            .collect();
        vs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        vs
    }

    /// `apexes_form_rect`: one apex per corner of {x0,x1}×{y0,y1} with a
    /// shared z is a rect, in any order; unequal z, a duplicated corner, a
    /// degenerate side, and a wrong count are not.
    #[test]
    fn apexes_form_rect_recognizes_corner_sets() {
        let apexes = pavilion_apexes();
        let mut refs: Vec<&CeilingApex> = apexes.iter().collect();
        let (lo, hi) = apexes_form_rect(&refs).expect("a rect");
        assert_eq!((lo.x, lo.y, hi.x, hi.y), (1.0, 1.0, 5.0, 3.0));
        refs.swap(0, 2); // order-free
        assert!(apexes_form_rect(&refs).is_some());

        // Unequal z.
        let mut bad = apexes.clone();
        bad[3].z_m = 4.5;
        assert!(apexes_form_rect(&bad.iter().collect::<Vec<_>>()).is_none());
        // A duplicated corner (three distinct points).
        let mut bad = apexes.clone();
        bad[3].point = bad[0].point;
        assert!(apexes_form_rect(&bad.iter().collect::<Vec<_>>()).is_none());
        // A degenerate side (all four on one line).
        let line: Vec<CeilingApex> = [(1.0, 1.0), (2.0, 1.0), (3.0, 1.0), (4.0, 1.0)]
            .into_iter()
            .map(|(x, y)| CeilingApex {
                point: Point2f64::new(x, y),
                z_m: 4.0,
            })
            .collect();
        assert!(apexes_form_rect(&line.iter().collect::<Vec<_>>()).is_none());
        // A non-rect quad (one corner pulled inward).
        let mut bad = apexes.clone();
        bad[2].point = Point2f64::new(4.5, 3.0);
        assert!(apexes_form_rect(&bad.iter().collect::<Vec<_>>()).is_none());
        // Wrong count.
        assert!(apexes_form_rect(&apexes.iter().take(3).collect::<Vec<_>>()).is_none());
    }

    /// `rect_hip_switches`: a ring whose raw edges (midpoint sector) visit
    /// the four sectors once each switches exactly 4 — equal or unequal
    /// offsets, and a rect side ON the ring (the mono-gable case) included;
    /// a ring wrapping into a foreign sector (the notched C with the rect
    /// spanning its slot) switches more; degenerate input is 0.
    #[test]
    fn rect_hip_switches_clean_is_four_notch_wrap_is_more() {
        let ring = square(0.0, 0.0, 6.0, 4.0);
        let (lo, hi) = (Point2f64::new(1.0, 1.0), Point2f64::new(5.0, 3.0));
        assert_eq!(rect_hip_switches(&ring, lo, hi), 4, "equal offsets");
        let (lo, hi) = (Point2f64::new(2.0, 1.0), Point2f64::new(4.0, 3.0));
        assert_eq!(rect_hip_switches(&ring, lo, hi), 4, "unequal offsets");
        let (lo, hi) = (Point2f64::new(1.0, 0.0), Point2f64::new(5.0, 2.0));
        assert_eq!(rect_hip_switches(&ring, lo, hi), 4, "side on the ring");
        // The notched C with the rect spanning the slot's reach: the ring
        // wraps past the hip lines.
        let (lo, hi) = (Point2f64::new(0.5, 0.5), Point2f64::new(9.0, 3.5));
        assert!(
            rect_hip_switches(&notched_ring(), lo, hi) > 4,
            "wrap past a hip line exceeds the fence (got {})",
            rect_hip_switches(&notched_ring(), lo, hi)
        );
        // Degenerate: a sub-triangle ring or a zero-side rect.
        assert_eq!(rect_hip_switches(&ring[..2], lo, hi), 0);
        assert_eq!(
            rect_hip_switches(&ring, Point2f64::new(1.0, 1.0), Point2f64::new(1.0, 3.0)),
            0
        );
    }

    /// THE PAVILION: four corner apexes with equal eaves offsets → the
    /// classic flat-topped hip — 4 planar trapezoids (one per same-side
    /// run, straight out of the loft) + the flat TOP, tiling the footprint,
    /// every boundary vertex at the eaves and every rect corner verbatim at
    /// its apex; the grouping authority links all four corners into ONE
    /// group at the eaves base whose merged ring (the TOP joins WITHOUT
    /// voting — the apex_groups amendment) is the whole footprint.
    #[test]
    fn with_apexes_four_corners_make_a_pavilion() {
        use geo::Area;
        let cid = ItemId::default();
        let apexes = pavilion_apexes();
        let out = CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 6.0, 4.0)], 2.4, cid)
            .with_apexes(&apexes);
        assert_eq!(out.regions.len(), 5, "4 slopes + the flat top");
        for r in &out.regions {
            assert!(
                is_planar(r),
                "every pavilion face is planar: {:?}",
                r.polygon
            );
            assert_eq!(r.polygon.exterior.len(), 4, "trapezoids + the top");
        }
        let tops: Vec<&CeilingRegion> = out
            .regions
            .iter()
            .filter(|r| r.flat_z().is_some_and(|z| (z - 4.0).abs() < 1e-9))
            .collect();
        assert_eq!(tops.len(), 1, "one flat top at the apex height");
        let top_plan: Polygon<f64> = tops[0].polygon.clone().into();
        assert!(
            (top_plan.unsigned_area() - 8.0).abs() < 1e-9,
            "the 4×2 rect"
        );
        let total: f64 = out
            .regions
            .iter()
            .map(|r| Polygon::<f64>::from(r.polygon.clone()).unsigned_area())
            .sum();
        assert!((total - 24.0).abs() < 1e-9, "faces tile the footprint");

        // The grouping authority: one group, all four corners, eaves base,
        // merged ring = the footprint (the all-apex TOP joined the merge).
        let groups = out.apex_groups(&apexes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].apex_idxs, vec![0, 1, 2, 3]);
        assert!((groups[0].base_z - 2.4).abs() < 1e-9);
        assert_eq!(groups[0].merged_rings.len(), 1, "one merged ring");
        let ring = ring_to_poly(&groups[0].merged_rings[0]);
        assert!(
            (ring.unsigned_area() - 24.0).abs() < 1e-6,
            "the merged ring covers the WHOLE footprint — no hole under the top"
        );
    }

    /// THE RECT RE-APPLY IDENTITY (the fixpoint core): feeding the merged
    /// pavilion output back through `with_apexes` with the same four apexes
    /// reproduces it vertex for vertex — each claimed face re-triangulates
    /// in its own plane and folds back; unclaimed faces pass through.
    #[test]
    fn rect_reapply_is_identity() {
        let cid = ItemId::default();
        let apexes = pavilion_apexes();
        let out = CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 6.0, 4.0)], 2.4, cid)
            .with_apexes(&apexes);
        let again = out.clone().with_apexes(&apexes);
        assert_eq!(fingerprint(&out), fingerprint(&again), "re-apply identity");
        // And once more — settled for good.
        let third = again.clone().with_apexes(&apexes);
        assert_eq!(fingerprint(&again), fingerprint(&third));
    }

    /// THE LIVE-REPRO REGRESSION — UNEQUAL per-axis eaves offsets (x-sides
    /// 0.325 m, y-sides 0.375 m), so every footprint corner sits OFF the
    /// 45° hip diagonals: the loft inserts NO boundary vertices and emits
    /// EXACTLY 4 side quads + the flat top (no corner wedges — each
    /// boundary corner connects straight to its nearest rect corner), every
    /// face PLANAR (each side quad's z is linear in the coordinate
    /// perpendicular to its side), tiling the footprint.
    #[test]
    fn rect_unequal_offsets_loft_four_planar_quads_no_inserted_vertices() {
        use geo::Area;
        let cid = ItemId::default();
        let apexes: Vec<CeilingApex> = [
            (1.0978, 1.1137),
            (1.4478, 1.1137),
            (1.4478, 1.3637),
            (1.0978, 1.3637),
        ]
        .into_iter()
        .map(|(x, y)| CeilingApex {
            point: Point2f64::new(x, y),
            z_m: 4.0,
        })
        .collect();
        let footprint = square(0.7728, 0.7387, 1.7728, 1.7387);
        let out = CeilingRegions::default_for_footprint(vec![footprint.clone()], 3.1, cid)
            .with_apexes(&apexes);
        assert_eq!(out.regions.len(), 5, "4 side quads + the flat top");
        let allowed: Vec<Point2f64> = footprint
            .into_iter()
            .chain(apexes.iter().map(|a| a.point))
            .collect();
        for r in &out.regions {
            assert_eq!(r.polygon.exterior.len(), 4, "quads only: {:?}", r.polygon);
            assert!(
                is_planar(r),
                "planar despite unequal offsets: {:?}",
                r.polygon
            );
            for v in &r.polygon.exterior {
                assert!(
                    allowed
                        .iter()
                        .any(|p| (p.x - v.x).abs() < 1e-9 && (p.y - v.y).abs() < 1e-9),
                    "no inserted boundary vertices (got ({}, {}))",
                    v.x,
                    v.y
                );
            }
        }
        let total: f64 = out
            .regions
            .iter()
            .map(|r| Polygon::<f64>::from(r.polygon.clone()).unsigned_area())
            .sum();
        assert!((total - 1.0).abs() < 1e-9, "faces tile the 1 m² footprint");
        // Re-apply identity holds on the unequal-offsets output too.
        let again = out.clone().with_apexes(&apexes);
        assert_eq!(fingerprint(&out), fingerprint(&again), "re-apply identity");
    }

    /// An L-SHAPED region: the two boundary edges facing one rect side form
    /// a single multi-edge RUN lofting to that side as ONE polygon face (5
    /// vertices) — still no inserted vertices and no corner wedges — the
    /// faces tile the L, and the grouping authority's merged ring is the
    /// original L boundary (no extra vertices).
    #[test]
    fn rect_over_l_region_lofts_multi_edge_runs_as_single_faces() {
        use geo::Area;
        let cid = ItemId::default();
        let l_ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(6.0, 0.0),
            Point2f64::new(6.0, 4.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(4.0, 6.0),
            Point2f64::new(0.0, 6.0),
        ];
        let apexes: Vec<CeilingApex> = [(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]
            .into_iter()
            .map(|(x, y)| CeilingApex {
                point: Point2f64::new(x, y),
                z_m: 4.0,
            })
            .collect();
        let ring2: Vec<Point2f64> = l_ring.clone();
        assert_eq!(
            rect_hip_switches(&ring2, apexes[0].point, apexes[2].point),
            4,
            "the L's sector walk is clean"
        );
        let out = CeilingRegions::default_for_footprint(vec![l_ring.clone()], 2.4, cid)
            .with_apexes(&apexes);
        assert_eq!(out.regions.len(), 5, "4 side faces + the flat top");
        let mut vertex_counts: Vec<usize> = out
            .regions
            .iter()
            .map(|r| r.polygon.exterior.len())
            .collect();
        vertex_counts.sort();
        assert_eq!(
            vertex_counts,
            vec![4, 4, 4, 5, 5],
            "two single-edge runs + the top are quads; the two-edge runs are 5-gons"
        );
        let allowed: Vec<Point2f64> = l_ring
            .iter()
            .copied()
            .chain(apexes.iter().map(|a| a.point))
            .collect();
        for r in &out.regions {
            for v in &r.polygon.exterior {
                assert!(
                    allowed
                        .iter()
                        .any(|p| (p.x - v.x).abs() < 1e-9 && (p.y - v.y).abs() < 1e-9),
                    "no inserted boundary vertices (got ({}, {}))",
                    v.x,
                    v.y
                );
            }
        }
        let total: f64 = out
            .regions
            .iter()
            .map(|r| Polygon::<f64>::from(r.polygon.clone()).unsigned_area())
            .sum();
        assert!((total - 32.0).abs() < 1e-9, "faces tile the L footprint");
        // The grouping authority: one group whose merged ring is the L
        // itself — every merged-ring vertex is an original L corner.
        let groups = out.apex_groups(&apexes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].merged_rings.len(), 1, "one merged ring");
        let merged = &groups[0].merged_rings[0];
        assert!(
            (ring_to_poly(merged).unsigned_area() - 32.0).abs() < 1e-6,
            "the merged ring covers the whole L"
        );
        for v in merged {
            assert!(
                l_ring
                    .iter()
                    .any(|p| (p.x - v.x).abs() < 1e-6 && (p.y - v.y).abs() < 1e-6),
                "merged ring keeps only original L corners (got ({}, {}))",
                v.x,
                v.y
            );
        }
    }

    /// Risk #4 — the rect TOUCHING ONE WALL (the mono-gable side): the
    /// ZERO-WIDTH run along the wall emits NOTHING (the boundary edge IS
    /// the rect edge — plan-degenerate, dropped), leaving exactly 3 side
    /// quads + the flat top; the switch count stays at the clean 4, the
    /// surviving faces still tile the footprint, the wall-side boundary
    /// rises to the apexes (the mono-gable profile), and the re-apply
    /// identity holds.
    #[test]
    fn rect_touching_one_wall_drops_the_degenerate_side_and_reapplies() {
        use geo::Area;
        let cid = ItemId::default();
        // Rect (1,0)..(5,2): its bottom side lies ON the footprint edge y=0.
        let apexes: Vec<CeilingApex> = [(1.0, 0.0), (5.0, 0.0), (5.0, 2.0), (1.0, 2.0)]
            .into_iter()
            .map(|(x, y)| CeilingApex {
                point: Point2f64::new(x, y),
                z_m: 4.0,
            })
            .collect();
        let out = CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 6.0, 4.0)], 2.4, cid)
            .with_apexes(&apexes);
        assert_eq!(
            out.regions.len(),
            4,
            "the on-wall side lofts nothing: 3 side quads + the top"
        );
        // Tiles the footprint with no face below the wall side.
        let total: f64 = out
            .regions
            .iter()
            .map(|r| Polygon::<f64>::from(r.polygon.clone()).unsigned_area())
            .sum();
        assert!((total - 24.0).abs() < 1e-9, "faces tile the footprint");
        assert!(
            out.regions
                .iter()
                .all(|r| r.polygon.exterior.iter().all(|v| v.y >= -1e-9)),
            "nothing lofts past the wall-side boundary"
        );
        // The flat top carries the two on-wall corners at the apex height —
        // the wall under them gets the gable profile from ceiling_profile_top.
        let top = out
            .regions
            .iter()
            .find(|r| r.flat_z().is_some_and(|z| (z - 4.0).abs() < 1e-9))
            .expect("the flat top");
        for x in [1.0, 5.0] {
            assert!(
                top.polygon
                    .exterior
                    .iter()
                    .any(|v| (v.x - x).abs() < 1e-9 && v.y.abs() < 1e-9),
                "top corner ON the wall at x = {x}"
            );
        }
        // One group of four; re-apply identity.
        let groups = out.apex_groups(&apexes);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].apex_idxs, vec![0, 1, 2, 3]);
        let again = out.clone().with_apexes(&apexes);
        assert_eq!(fingerprint(&out), fingerprint(&again), "re-apply identity");
    }

    /// Risk #4 — a ring wrapping past a hip line (the L/C shape with the
    /// rect spanning its slot, > 4 switches): assign-time validation fences
    /// this, but legacy input must WARN-AND-PROCEED — with_apexes still
    /// produces faces (the flat top included) and never panics, and
    /// re-resolving is deterministic.
    #[test]
    fn rect_over_a_wrapping_ring_warns_and_proceeds() {
        let cid = ItemId::default();
        let apexes: Vec<CeilingApex> = [(0.5, 0.5), (9.0, 0.5), (9.0, 3.5), (0.5, 3.5)]
            .into_iter()
            .map(|(x, y)| CeilingApex {
                point: Point2f64::new(x, y),
                z_m: 4.0,
            })
            .collect();
        let base = || CeilingRegions::default_for_footprint(vec![notched_ring()], 2.4, cid);
        let out = base().with_apexes(&apexes);
        assert!(!out.regions.is_empty(), "faces produced, no panic");
        assert!(
            out.regions
                .iter()
                .any(|r| r.flat_z().is_some_and(|z| (z - 4.0).abs() < 1e-9)),
            "the flat top is emitted"
        );
        let again = base().with_apexes(&apexes);
        assert_eq!(
            fingerprint(&out),
            fingerprint(&again),
            "deterministic across resolves"
        );
    }

    /// Four apexes that do NOT form a rect (unequal z, or a non-corner
    /// point) fall through to the existing ≥3 warn path: a ridge from the
    /// first two — never a panic.
    #[test]
    fn four_non_rect_apexes_fall_through_to_the_ridge_warn_path() {
        let cid = ItemId::default();
        let mut apexes = pavilion_apexes();
        apexes[2].z_m = 4.5; // breaks the shared-z rule
        let out = CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 6.0, 4.0)], 2.4, cid)
            .with_apexes(&apexes);
        assert!(!out.regions.is_empty());
        // The ridge-from-first-two fallback: the surface rises to 4.0 (the
        // first two apexes' z), and no flat top at 4.0 exists.
        let zs: Vec<f64> = out
            .regions
            .iter()
            .flat_map(|r| r.polygon.exterior.iter().map(|v| v.z))
            .collect();
        assert!(zs.iter().any(|z| (z - 4.0).abs() < 1e-9));
        assert!(
            !out.regions
                .iter()
                .any(|r| r.flat_z().is_some_and(|z| (z - 4.0).abs() < 1e-9)
                    && r.polygon.exterior.len() == 4),
            "no pavilion top — the rect branch did not fire"
        );
    }

    /// THE GABLE WALL-PROFILE TEST: the wall UNDER a boundary ridge end gets
    /// the triangular gable top straight out of `WallRegions::regenerate` —
    /// `profile_breakpoints` picks up the slope edges along the wall,
    /// `covering_roof_plane` evaluates the two sloping planes, and the top
    /// traces base → apex → base. No wall-side change was needed.
    #[test]
    fn regenerate_boundary_ridge_makes_a_triangular_gable_wall_top() {
        let cid = ItemId::default();
        let apexes = [
            CeilingApex {
                point: Point2f64::new(2.0, 0.0),
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(2.0, 4.0),
                z_m: 4.2,
            },
        ];
        let ceilings =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid)
                .with_apexes(&apexes);
        let ring = square(0.0, 0.0, 4.0, 4.0);
        let segments: Vec<(Point2f64, Point2f64, WallExposure)> = (0..4)
            .map(|i| (ring[i], ring[(i + 1) % 4], WallExposure::Unset))
            .collect();
        let walls =
            WallRegions::regenerate(segments, &ceilings, &FloorRegions::default(), 0.0, 2.4, cid);
        assert_eq!(walls.regions.len(), 4);
        // The GABLE wall y = 0 (under the ridge end at (2, 0)): base edge,
        // then the top traced b → a as eaves → apex → eaves.
        let gable = &walls.regions[0].polygon.exterior;
        assert_eq!(gable.len(), 5, "quad + the apex vertex");
        assert!(
            (gable[0].z).abs() < 1e-9 && (gable[1].z).abs() < 1e-9,
            "base"
        );
        assert!(
            (gable[2].x - 4.0).abs() < 1e-9 && (gable[2].z - 2.4).abs() < 1e-9,
            "eaves at the b corner"
        );
        assert!(
            (gable[3].x - 2.0).abs() < 1e-9
                && (gable[3].y).abs() < 1e-9
                && (gable[3].z - 4.2).abs() < 1e-9,
            "the GABLE APEX on the wall top: {gable:?}"
        );
        assert!(
            (gable[4].x).abs() < 1e-9 && (gable[4].z - 2.4).abs() < 1e-9,
            "eaves at the a corner"
        );
        // The two EAVES walls (x = 4 and x = 0, ALONG the ridge slopes' low
        // edges) stay flat quads at the eaves height.
        for i in [1, 3] {
            let ext = &walls.regions[i].polygon.exterior;
            assert_eq!(ext.len(), 4, "eaves wall {i} is a flat quad: {ext:?}");
            assert!(ext.iter().skip(2).all(|v| (v.z - 2.4).abs() < 1e-9));
        }
        // The opposite gable (y = 4) mirrors the first.
        let gable2 = &walls.regions[2].polygon.exterior;
        assert_eq!(gable2.len(), 5);
        assert!(gable2
            .iter()
            .any(|v| (v.x - 2.0).abs() < 1e-9 && (v.z - 4.2).abs() < 1e-9));
    }

    /// f32 QUANTIZATION ROUND-TRIP of a boundary apex: a snapped end's
    /// coordinate IS an outline wall's — an f32 widened to f64 — so
    /// `quantize_point` is the identity on it and the stored apex still
    /// claims its region exactly; when the derived ring is boolean-noise
    /// displaced (~1e-7 m) the [`APEX_BOUNDARY_EPS_M`] band keeps the claim.
    #[test]
    fn boundary_apex_f32_round_trip_reclaims_its_region() {
        let cid = ItemId::default();
        // Outline coordinates as the pipeline stores them: f32 widened.
        let (x0, x1) = (0.1f32 as f64, 4.3f32 as f64);
        let (y0, y1) = (0.2f32 as f64, 3.7f32 as f64);
        let ring = vec![
            Point2f64::new(x0, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0, y1),
        ];
        let regions = CeilingRegions::default_for_footprint(vec![ring], 2.4, cid);
        // The apex sits ON the left wall (a gable end at x = x0).
        let apex_p = Point2f64::new(x0, 2.0);
        assert_eq!(
            CeilingApex::quantize_point(apex_p),
            apex_p,
            "quantization is the identity on an outline coordinate"
        );
        assert_eq!(regions.claim_regions(apex_p), vec![0], "exact claim");

        // Boolean-noise displacement: the ring sits 1e-7 m INSIDE the stored
        // apex — strict containment fails, the EPS band claims.
        let noisy_ring = vec![
            Point2f64::new(x0 + 1e-7, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0 + 1e-7, y1),
        ];
        let noisy = CeilingRegions::default_for_footprint(vec![noisy_ring], 2.4, cid);
        assert_eq!(
            noisy.claim_regions(apex_p),
            vec![0],
            "the boundary band claims through boolean noise"
        );
        // …and with_apexes still shapes the noisy region (no warn-skip).
        let shaped = noisy.with_apexes(&[CeilingApex {
            point: apex_p,
            z_m: 4.2,
        }]);
        assert!(
            shaped.regions.iter().any(|r| r
                .polygon
                .exterior
                .iter()
                .any(|v| (v.z - 4.2).abs() < 1e-9)),
            "the noise-displaced region still rises to the apex"
        );
    }

    #[test]
    fn add_and_remove_roof_apex_on_a_storey() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        ber.storeys.push(Storey::default());
        let apex = CeilingApex {
            point: Point2f64::new(1.0, 1.0),
            z_m: 4.2,
        };
        ber.add_ceiling_apex(1, apex.clone());
        assert!(
            ber.storeys[0].ceiling_apexes.is_empty(),
            "other storey untouched"
        );
        assert_eq!(ber.storeys[1].ceiling_apexes, vec![apex.clone()]);
        // Out-of-range indices are no-ops.
        ber.add_ceiling_apex(7, apex.clone());
        ber.remove_roof_apex(1, 5);
        ber.remove_roof_apex(7, 0);
        assert_eq!(ber.storeys[1].ceiling_apexes.len(), 1);
        ber.remove_roof_apex(1, 0);
        assert!(ber.storeys[1].ceiling_apexes.is_empty());
    }

    #[test]
    fn construction_is_complete_requires_type_and_build_up() {
        // Wall: default type is `Uninitialised` — incomplete even once a build-up
        // is chosen, until a real type is set.
        let mut w = WallConstruction::default();
        assert!(!w.is_complete());
        w.base.construction_makeup = ConstructionMakeup::Default;
        assert!(!w.is_complete(), "type still Uninitialised");
        w.construction_type = WallConstructionType::CavityWall;
        assert!(w.is_complete());

        // Floor: like walls, the default type is `Uninitialised` — nothing reads
        // as set until the assessor picks a type AND a build-up.
        let mut f = FloorConstruction::default();
        assert!(!f.construction_type.is_set(), "default floor type is unset");
        f.base.construction_makeup = ConstructionMakeup::NonHeatLoss;
        assert!(!f.is_complete(), "type still Uninitialised");
        f.construction_type = FloorConstructionType::SolidFloor;
        assert!(f.is_complete());

        // Ceiling: default type is `Uninitialised`; ceilings additionally need
        // their INSULATION specified (a photo-backed thickness, or declared
        // unknown) before they read as complete.
        let mut c = CeilingConstruction::default();
        c.base.construction_makeup = ConstructionMakeup::Default;
        assert!(!c.is_complete());
        c.construction_type = CeilingConstructionType::FlatRoof;
        assert!(!c.is_complete(), "insulation still unspecified");
        c.insulation = Some(CeilingInsulation::Unknown);
        assert!(c.is_complete());

        // UNSET vs UNKNOWN: `Tbd` (the pre-edit default) is NOT set, but a
        // declared `Unknown` build-up IS — so a typed wall marked Unknown is done.
        assert!(!ConstructionMakeup::Tbd.is_set(), "Tbd is unset");
        assert!(
            ConstructionMakeup::Unknown.is_set(),
            "Unknown is a real choice"
        );
        let mut w = WallConstruction {
            construction_type: WallConstructionType::CavityWall,
            ..Default::default()
        };
        assert!(!w.is_complete(), "default build-up Tbd is unset");
        w.base.construction_makeup = ConstructionMakeup::Unknown;
        assert!(w.is_complete(), "Unknown build-up counts as done");
    }

    #[test]
    fn incomplete_surface_ids_lists_only_unfinished_walls() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());

        // One DONE wall construction (type + build-up set) and one whose build-up
        // is still TBD.
        let done = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let tbd = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Tbd,
            ConstructionPhase::default(),
        );

        let mk = |cid: ItemId| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: cid,
        };

        let done_region = mk(done);
        let tbd_region = mk(tbd);
        // A region whose construction can't be resolved reads as incomplete.
        let orphan_region = mk(ItemId::default());
        let (done_id, tbd_id, orphan_id) = (done_region.id, tbd_region.id, orphan_region.id);

        let mut walls = WallRegions::default();
        walls.regions = vec![done_region, tbd_region, orphan_region];
        ber.storeys[0].walls = walls;

        let incomplete = ber.incomplete_surface_ids(0, SurfaceKind::Walls);
        assert!(!incomplete.contains(&done_id), "finished wall excluded");
        assert!(incomplete.contains(&tbd_id), "TBD build-up still remaining");
        assert!(
            incomplete.contains(&orphan_id),
            "unresolved construction is remaining"
        );
        assert_eq!(incomplete.len(), 2);

        // Out-of-range storey is empty, not a panic.
        assert!(ber.incomplete_surface_ids(9, SurfaceKind::Walls).is_empty());
    }

    /// Non-heat-loss and derived inter-storey walls are OUT of structure
    /// scope ([`WallExposure::in_structure_scope`]): the Storey's structure
    /// queries (tap hit-test, carve pick, edge→wall map, `Select all` seed)
    /// skip them, and completeness treats them as not-applicable — while an
    /// ordinary (here Exposed) wall on the same storey stays fully in scope.
    #[test]
    fn out_of_scope_walls_skip_structure_queries_and_completeness() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());

        // Three unit walls along y=0: in-scope (Exposed), NonHeatLoss, InterStorey.
        let mk = |x0: f64, exposure: WallExposure| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(x0, 0.0, 0.0),
                Point3f64::new(x0 + 1.0, 0.0, 0.0),
                Point3f64::new(x0 + 1.0, 0.0, 2.4),
                Point3f64::new(x0, 0.0, 2.4),
            ]),
            exposure,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        };
        let exposed = mk(0.0, WallExposure::Exposed);
        let nhl = mk(1.0, WallExposure::NonHeatLoss);
        let inter = mk(2.0, WallExposure::InterStorey);
        let (exposed_id, nhl_id, inter_id) = (exposed.id, nhl.id, inter.id);
        ber.storeys[0].walls.regions = vec![exposed, nhl, inter];
        let storey = &ber.storeys[0];

        // Select-all seed: only the in-scope wall.
        assert_eq!(storey.surface_ids(SurfaceKind::Walls), vec![exposed_id]);
        // Tap hit-test: the out-of-scope walls are not targets.
        assert_eq!(
            storey.surface_at(SurfaceKind::Walls, 0.5, 0.0, 0.1),
            Some(exposed_id)
        );
        assert_eq!(storey.surface_at(SurfaceKind::Walls, 1.5, 0.0, 0.1), None);
        assert_eq!(storey.surface_at(SurfaceKind::Walls, 2.5, 0.0, 0.1), None);
        // Edge→wall map (the carve ask's filter): out-of-scope edges map to
        // nothing, so the ask has nothing to ask about there.
        let seg = |x0: f64| {
            storey.wall_ids_on_segment(Point2f64::new(x0, 0.0), Point2f64::new(x0 + 1.0, 0.0))
        };
        assert_eq!(seg(0.0), vec![exposed_id]);
        assert!(seg(1.0).is_empty(), "NHL wall never maps to a carve edge");
        assert!(seg(2.0).is_empty(), "InterStorey wall never maps either");
        // Carve pick over a ring enclosing all three: only the in-scope wall.
        let ring = [
            Point2f64::new(-1.0, -1.0),
            Point2f64::new(4.0, -1.0),
            Point2f64::new(4.0, 1.0),
            Point2f64::new(-1.0, 1.0),
        ];
        assert_eq!(
            storey.surfaces_in_ring(SurfaceKind::Walls, &ring),
            vec![exposed_id]
        );

        // Completeness: only the in-scope wall is remaining; attributing IT
        // alone turns the kind complete — the others are not-applicable.
        let remaining = ber.incomplete_surface_ids(0, SurfaceKind::Walls);
        assert_eq!(remaining, vec![exposed_id]);
        assert!(!remaining.contains(&nhl_id) && !remaining.contains(&inter_id));
        assert!(!ber.structure_surfaces_complete(0, SurfaceKind::Walls));
        let done = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        ber.storeys[0].walls.regions[0].construction_id = done;
        assert!(
            ber.structure_surfaces_complete(0, SurfaceKind::Walls),
            "green with the NHL/InterStorey walls unattributed"
        );

        // PROGRESS counter ("N/M complete"): both numbers count only the
        // in-scope walls. With the Exposed wall attributed: 1/1. Adding a
        // second in-scope wall that stays unattributed: 1/2 — the NHL and
        // InterStorey walls appear in neither number.
        assert_eq!(
            ber.structure_surface_progress(0, SurfaceKind::Walls),
            (1, 1)
        );
        ber.storeys[0]
            .walls
            .regions
            .push(mk(3.0, WallExposure::Exposed));
        assert_eq!(
            ber.structure_surface_progress(0, SurfaceKind::Walls),
            (1, 2),
            "1 attributed + 1 unattributed in-scope + NHL/InterStorey → 1/2"
        );
        assert_eq!(
            ber.structure_surface_progress(9, SurfaceKind::Walls),
            (0, 0),
            "out-of-range storey reads 0/0"
        );
    }

    /// The horizontal mirror of the walls scope test: PARTY ceilings /
    /// NON-HEAT-LOSS floors (another dwelling — no heat loss) and the derived
    /// `InterStorey` junctions are OUT of structure scope — skipped by the
    /// Select-all seed, the carve pick, the structure tap and completeness —
    /// while the plain exposure tap (`surface_at`, shared with the
    /// Mapping-phase exposure painting) still reaches them to repaint.
    #[test]
    fn out_of_scope_floors_and_ceilings_skip_structure_queries() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        let floor_sq = |x0: f64| {
            geo::Polygon::new(
                geo::LineString::from(vec![(x0, 0.0), (x0 + 1.0, 0.0), (x0 + 1.0, 1.0), (x0, 1.0)]),
                vec![],
            )
        };
        let mk_floor = |x0: f64, exposure: FloorExposure| FloorRegion {
            id: ItemId::default(),
            polygon: floor_sq(x0),
            construction_id: ItemId::default(),
            level_m: 0.0,
            exposure,
        };
        let ceil_sq = |x0: f64| {
            Polygon3D::new(vec![
                Point3f64::new(x0, 0.0, 2.4),
                Point3f64::new(x0 + 1.0, 0.0, 2.4),
                Point3f64::new(x0 + 1.0, 1.0, 2.4),
                Point3f64::new(x0, 1.0, 2.4),
            ])
        };
        let mk_ceiling = |x0: f64, exposure: CeilingExposure| CeilingRegion {
            id: ItemId::default(),
            polygon: ceil_sq(x0),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure,
        };
        let in_floor = mk_floor(0.0, FloorExposure::Ground);
        let nhl_floor = mk_floor(1.0, FloorExposure::NonHeatLoss);
        let inter_floor = mk_floor(2.0, FloorExposure::InterStorey);
        let in_ceiling = mk_ceiling(0.0, CeilingExposure::ExposedRoof);
        let party_ceiling = mk_ceiling(1.0, CeilingExposure::Party);
        let inter_ceiling = mk_ceiling(2.0, CeilingExposure::InterStorey);
        let (in_floor_id, in_ceiling_id) = (in_floor.id, in_ceiling.id);
        let party_ceiling_id = party_ceiling.id;
        ber.storeys[0].floors.regions = vec![in_floor, nhl_floor, inter_floor];
        ber.storeys[0].ceilings.regions = vec![in_ceiling, party_ceiling, inter_ceiling];
        let storey = &ber.storeys[0];

        // Select-all seed + carve pick: in-scope only.
        assert_eq!(storey.surface_ids(SurfaceKind::Floors), vec![in_floor_id]);
        assert_eq!(
            storey.surface_ids(SurfaceKind::Ceilings),
            vec![in_ceiling_id]
        );
        let ring = [
            Point2f64::new(-1.0, -1.0),
            Point2f64::new(4.0, -1.0),
            Point2f64::new(4.0, 2.0),
            Point2f64::new(-1.0, 2.0),
        ];
        assert_eq!(
            storey.surfaces_in_ring(SurfaceKind::Floors, &ring),
            vec![in_floor_id]
        );
        assert_eq!(
            storey.surfaces_in_ring(SurfaceKind::Ceilings, &ring),
            vec![in_ceiling_id]
        );
        // The STRUCTURE tap misses an out-of-scope surface; the plain
        // exposure tap still reaches it (to repaint the classification).
        assert_eq!(
            storey.structure_surface_at(SurfaceKind::Ceilings, 1.5, 0.5, 0.0),
            None,
            "the structure tap doesn\'t offer the party ceiling"
        );
        assert_eq!(
            storey.surface_at(SurfaceKind::Ceilings, 1.5, 0.5, 0.0),
            Some(party_ceiling_id),
            "the exposure tap still reaches it"
        );
        assert_eq!(
            storey.structure_surface_at(SurfaceKind::Ceilings, 0.5, 0.5, 0.0),
            Some(in_ceiling_id)
        );

        // Completeness: only the in-scope surfaces are applicable.
        assert_eq!(
            ber.incomplete_surface_ids(0, SurfaceKind::Floors),
            vec![in_floor_id]
        );
        assert_eq!(
            ber.incomplete_surface_ids(0, SurfaceKind::Ceilings),
            vec![in_ceiling_id]
        );
        assert_eq!(
            ber.structure_surface_progress(0, SurfaceKind::Floors),
            (0, 1)
        );
        assert_eq!(
            ber.structure_surface_progress(0, SurfaceKind::Ceilings),
            (0, 1)
        );
        let done_floor = ber.ensure_floor_construction(
            FloorConstructionType::SolidFloor,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        ber.storeys[0].floors.regions[0].construction_id = done_floor;
        assert!(
            ber.structure_surfaces_complete(0, SurfaceKind::Floors),
            "green with the NHL/InterStorey floors unattributed"
        );
    }

    /// NAME-INTEGRITY: a construction name implies its attributes. Renaming a
    /// same-kind construction to a name another one holds is rejected with a
    /// hint (a colliding name always means different attributes — identical
    /// attributes find-or-create into ONE shared entry, so no duplicate ever
    /// exists to merge). Self-renames, other-kind reuse and clearing are fine.
    #[test]
    fn construction_rename_enforces_name_integrity() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        let brick = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let block = ber.ensure_wall_construction(
            WallConstructionType::ConcreteHollowBlocks,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        assert_ne!(brick, block, "different attributes → distinct entries");
        // Identical UNNAMED attributes never duplicate (same name + same
        // attributes IS the same group)…
        assert_eq!(
            brick,
            ber.ensure_wall_construction(
                WallConstructionType::CavityWall,
                ConstructionMakeup::Default,
                ConstructionPhase::default(),
            )
        );
        // …but the NAME is part of the identity: the same fabric under a
        // different name is a DIFFERENT group (e.g. "Porch roof" vs "Main
        // house roof", both sloped/default/original).
        let named_twin = ber.ensure_wall_construction_named(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
            Some("Twin"),
        );
        assert_ne!(brick, named_twin, "same fabric, different name → distinct");
        assert_eq!(
            named_twin,
            ber.ensure_wall_construction_named(
                WallConstructionType::CavityWall,
                ConstructionMakeup::Default,
                ConstructionPhase::default(),
                Some("  Twin "),
            ),
            "trimmed name variants find the same group"
        );

        // A name is only HELD by a construction some region still references —
        // give brick and block members (the rename rule is member-scoped).
        let mk = |x0: f64, cid: ItemId| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(x0, 0.0, 0.0),
                Point3f64::new(x0 + 1.0, 0.0, 0.0),
                Point3f64::new(x0 + 1.0, 0.0, 2.4),
                Point3f64::new(x0, 0.0, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: cid,
        };
        ber.storeys[0].walls.regions = vec![mk(0.0, brick), mk(1.0, block)];

        ber.set_construction_name(SurfaceKind::Walls, brick, Some("Brick".into()))
            .expect("first use of the name");
        let err = ber
            .set_construction_name(SurfaceKind::Walls, block, Some("Brick".into()))
            .expect_err("another wall group already holds \"Brick\"");
        assert!(err.contains("Brick"), "the hint names the collision: {err}");
        assert!(
            err.contains("different name"),
            "…and says what to do: {err}"
        );
        assert_eq!(
            ber.wall_construction(block).unwrap().base.name,
            None,
            "the rejected rename left the name untouched"
        );
        // Whitespace variants are the same name.
        assert!(ber
            .set_construction_name(SurfaceKind::Walls, block, Some("  Brick ".into()))
            .is_err());
        // Re-asserting a group's OWN name is a no-op success.
        ber.set_construction_name(SurfaceKind::Walls, brick, Some("Brick".into()))
            .expect("self-rename is not a collision");
        // Another KIND may reuse the name; clearing always succeeds.
        let floor = ber.ensure_floor_construction(
            FloorConstructionType::SolidFloor,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        ber.set_construction_name(SurfaceKind::Floors, floor, Some("Brick".into()))
            .expect("names are per kind");
        ber.set_construction_name(SurfaceKind::Walls, block, None)
            .expect("clearing is always fine");
        // The holder lookup the shell's same-group acceptance uses.
        assert_eq!(
            ber.named_construction_attrs(SurfaceKind::Walls, "Brick"),
            Some((
                ConstructionType::Wall(WallConstructionType::CavityWall),
                ConstructionMakeup::Default,
                ConstructionPhase::default(),
            ))
        );
        assert_eq!(
            ber.named_construction_attrs(SurfaceKind::Walls, "nope"),
            None
        );
    }

    #[test]
    fn specified_build_up_needs_a_photo_but_default_and_unknown_do_not() {
        // Only a SPECIFIED custom (Layers) build-up requires a backing photo.
        assert!(!ConstructionMakeup::Default.requires_photo());
        assert!(!ConstructionMakeup::Unknown.requires_photo());
        assert!(!ConstructionMakeup::NonHeatLoss.requires_photo());
        assert!(ConstructionMakeup::Layers(vec![]).requires_photo());

        // A typed wall with a Layers build-up is NOT complete until a photo
        // backs it; a Default build-up is complete with no photo.
        let mut specified = WallConstruction {
            construction_type: WallConstructionType::CavityWall,
            ..Default::default()
        };
        specified.base.construction_makeup = ConstructionMakeup::Layers(vec![]);
        assert!(
            !specified.is_complete(),
            "specified build-up without a photo is not done"
        );
        specified.base.photos.push(Photo::default());
        assert!(specified.is_complete(), "a backing photo completes it");

        let mut default = WallConstruction {
            construction_type: WallConstructionType::CavityWall,
            ..Default::default()
        };
        default.base.construction_makeup = ConstructionMakeup::Default;
        assert!(default.is_complete(), "Default build-up needs no photo");
    }

    #[test]
    fn structure_surfaces_complete_needs_surfaces_and_photo_backed_build_ups() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());

        // An empty kind is NOT vacuously complete.
        assert!(!ber.structure_surfaces_complete(0, SurfaceKind::Walls));

        // One wall whose construction is a SPECIFIED (Layers) build-up with no
        // photo yet.
        let cid = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Layers(vec![]),
            ConstructionPhase::default(),
        );
        let region = WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: cid,
        };
        let mut walls = WallRegions::default();
        walls.regions = vec![region];
        ber.storeys[0].walls = walls;

        assert!(
            !ber.structure_surfaces_complete(0, SurfaceKind::Walls),
            "a specified build-up without a photo leaves the kind incomplete"
        );

        // Back the construction with a photo → the kind is now complete.
        ber.wall_constructions
            .iter_mut()
            .find(|c| c.base.id == cid)
            .unwrap()
            .base
            .photos
            .push(Photo::default());
        assert!(
            ber.structure_surfaces_complete(0, SurfaceKind::Walls),
            "a backing photo completes the kind"
        );
    }

    // ── External per-side elevation ──────────────────────────────────────────

    /// A flat 10×10 storey's four walls at z `[z0, z0+h]`. Returns the walls and
    /// the id of the FRONT wall (the y=10 edge, whose outward normal is +Y).
    fn square_storey_walls(z0: f64, h: f64) -> (WallRegions, ItemId) {
        let wall = |a: (f64, f64), b: (f64, f64)| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(a.0, a.1, z0),
                Point3f64::new(b.0, b.1, z0),
                Point3f64::new(b.0, b.1, z0 + h),
                Point3f64::new(a.0, a.1, z0 + h),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        };
        let front = wall((0.0, 10.0), (10.0, 10.0)); // y=10 edge → outward +Y
        let front_id = front.id;
        let mut regions = WallRegions::default();
        regions.regions = vec![
            wall((0.0, 0.0), (10.0, 0.0)),   // y=0  → Back (−Y)
            wall((10.0, 0.0), (10.0, 10.0)), // x=10 → Right (+X)
            wall((0.0, 0.0), (0.0, 10.0)),   // x=0  → Left (−X)
            front,
        ];
        (regions, front_id)
    }

    /// A ceiling RISER whose base sits at a step (z=2.0) and whose top slopes
    /// DOWN to `a` — like the riser between a sloped ceiling and its lower
    /// neighbour. Its `top_profile` must include the closing slope edge back to
    /// `a` (along 0, base z), so the sloped face is drawn in the elevation and an
    /// opening under it clamps to the slope. (Regression: `top_profile` dropped
    /// that closing edge, so the elevation showed only the flat step part.)
    #[test]
    fn sloped_ceiling_riser_top_profile_includes_the_closing_slope() {
        // x=0 riser: base y 0→4.5 at z=2.0; flat top 3.0 over y 2.0→4.5; slope
        // from (y2.0,z3.0) back down to a=(y0,z2.0).
        let riser = WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 2.0), // a — low end of the slope
                Point3f64::new(0.0, 2.0, 2.0), // base mid
                Point3f64::new(0.0, 4.5, 2.0), // b — base end
                Point3f64::new(0.0, 4.5, 3.0), // top up
                Point3f64::new(0.0, 2.0, 3.0), // top, then slopes back to a
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        };
        let prof = riser.top_profile();
        // Ends at (along 0, base z) — the slope reaches `a`.
        let last = *prof.last().expect("non-empty");
        assert!(
            last.0.abs() < 1e-3 && (last.1 - 2.0).abs() < 1e-3,
            "top profile closes down to a at (0, 2.0), got {last:?}: {prof:?}"
        );
        // The slope actually descends over the near-`a` span (z at along 1 is
        // strictly between the base 2.0 and the flat top 3.0).
        let z1 = riser.top_z_at_along(1.0);
        assert!(
            z1 > 2.0 + 1e-6 && z1 < 3.0 - 1e-6,
            "sloped top at along 1 is between 2.0 and 3.0, got {z1}"
        );
    }

    #[test]
    fn side_elevation_picks_the_facing_wall_and_projects_an_opening() {
        use crate::ber::common::ExternalSide;
        use crate::ber::openings::{add_wall_window, OpeningRef};

        let mut ber = Ber::default();
        let mut storey = Storey::default();
        let (walls, front_id) = square_storey_walls(0.0, 2.4);
        storey.walls = walls;
        ber.storeys.push(storey);
        // A window centred 2 m along the front wall (from its start vertex).
        let wid = add_wall_window(
            &mut ber.storeys[0],
            front_id,
            2.0,
            1.2,
            1.0,
            0.9,
            ItemId::default(),
        )
        .expect("window placed");

        // Front picks exactly the one wall facing +Y, and carries the window.
        let front = ber.side_elevation(ExternalSide::Front);
        assert_eq!(front.panels.len(), 1, "only the +Y wall faces Front");
        assert_eq!(front.openings.len(), 1);
        let o = front.openings[0];
        assert_eq!(o.opening, OpeningRef::Window(wid));
        // The opening centre projects to an x within the panel's span.
        let p = &front.panels[0];
        assert!(
            o.x_center >= p.x0 - 1e-6 && o.x_center <= p.x1 + 1e-6,
            "opening x {} within panel [{}, {}]",
            o.x_center,
            p.x0,
            p.x1
        );
        assert!((o.sill_z - 0.9).abs() < 1e-6, "sill above the datum");

        // Forward/inverse round-trip: the projected x_center inverts back to the
        // opening's along-distance (2 m) via the wall's drag inverse.
        let front_wall = ber.storeys[0]
            .walls
            .regions
            .iter()
            .find(|w| w.id == front_id)
            .unwrap();
        let recovered = front_wall
            .elevation_along_at(ExternalSide::Front.view_axis(), o.x_center)
            .unwrap();
        assert!(
            (recovered - 2.0).abs() < 1e-6,
            "x_center {} inverts back to along 2.0 (got {})",
            o.x_center,
            recovered
        );

        // Each of the four sides selects exactly one wall of the square.
        for side in ExternalSide::ALL {
            assert_eq!(
                ber.side_elevation(side).panels.len(),
                1,
                "{} faces exactly one wall",
                side.label()
            );
        }
    }

    #[test]
    fn side_elevation_stacks_storeys_by_floor_datum() {
        use crate::ber::common::ExternalSide;

        let mut ber = Ber::default();
        for _ in 0..2 {
            let mut storey = Storey::default();
            storey.walls = square_storey_walls(0.0, 2.4).0; // walls are storey-LOCAL
            storey.ceiling_height_m = Some(2.4);
            ber.storeys.push(storey);
        }
        let front = ber.side_elevation(ExternalSide::Front);
        assert_eq!(front.panels.len(), 2, "one front wall per storey");
        // The upper storey's wall sits a full ground-storey height higher.
        let mut bases: Vec<f64> = front.panels.iter().map(|p| p.base_z).collect();
        bases.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((bases[0] - 0.0).abs() < 1e-6, "ground storey at datum 0");
        assert!(
            (bases[1] - 2.4).abs() < 1e-6,
            "upper storey offset by storey-0 height"
        );
    }

    #[test]
    fn storey_json_without_roof_apexes_deserializes_to_empty() {
        // JSON produced by the OLD shape (no `ceiling_apexes` field): serialize
        // the current shape and strip the field.
        let storey = Storey::default();
        let mut json = serde_json::to_value(&storey).expect("serialize");
        assert!(json
            .as_object_mut()
            .unwrap()
            .remove("ceiling_apexes")
            .is_some());
        let old: Storey = serde_json::from_value(json).expect("old JSON still loads");
        assert!(old.ceiling_apexes.is_empty());
    }

    #[test]
    fn set_default_ceiling_height_on_one_storey() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        ber.storeys.push(Storey::default());
        ber.set_default_ceiling_height(Some(1), 2.6);
        assert_eq!(
            ber.storeys[0].ceiling_height_m, None,
            "other storey untouched"
        );
        assert_eq!(ber.storeys[1].ceiling_height_m, Some(2.6));
        // Re-stamping the SAME storey overwrites (per-storey scope is explicit).
        ber.set_default_ceiling_height(Some(1), 2.9);
        assert_eq!(ber.storeys[1].ceiling_height_m, Some(2.9));
    }

    #[test]
    fn set_default_ceiling_height_all_storeys_fills_only_missing() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey {
            ceiling_height_m: Some(3.1),
            ..Storey::default()
        });
        ber.storeys.push(Storey::default());
        ber.set_default_ceiling_height(None, 2.4);
        assert_eq!(
            ber.storeys[0].ceiling_height_m,
            Some(3.1),
            "set storey never overwritten"
        );
        assert_eq!(ber.storeys[1].ceiling_height_m, Some(2.4), "gap filled");
    }

    #[test]
    fn floor_region_json_with_legacy_height_field_still_deserializes() {
        // JSON produced by the OLD shape carried a `height` field (always the
        // 2.4 placeholder). Removal is load-compatible: serialize the current
        // shape, inject the legacy field, deserialize.
        let region = FloorRegions::new(square(0.0, 0.0, 10.0, 10.0), ItemId::default())
            .regions
            .remove(0);
        let mut json = serde_json::to_value(&region).expect("serialize");
        json.as_object_mut()
            .unwrap()
            .insert("height".to_string(), serde_json::json!(2.4));
        let old: FloorRegion = serde_json::from_value(json).expect("old JSON still loads");
        assert_eq!(old.construction_id, region.construction_id);
        assert_eq!(old.level_m, 0.0);
    }

    #[test]
    fn storey_json_without_ceiling_height_deserializes_to_none() {
        let storey = Storey::default();
        let mut json = serde_json::to_value(&storey).expect("serialize");
        assert!(json
            .as_object_mut()
            .unwrap()
            .remove("ceiling_height_m")
            .is_some());
        let old: Storey = serde_json::from_value(json).expect("old JSON still loads");
        assert_eq!(old.ceiling_height_m, None);
    }

    /* ---------------------------------------------------------------------
     * Floor thickness, storey floor datums, ceiling anchor + inheritance
     * ------------------------------------------------------------------ */

    /// L-shape ceiling stepped via `with_z_deltas`: bottom arm (x∈[0,10],
    /// y∈[0,4], 40 m²) raised to 2.7, upper arm (x∈[0,4], y∈[4,10], 24 m²)
    /// left at 2.4 — flats at two z's.
    fn stepped_l_roofs() -> CeilingRegions {
        let l_ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(4.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ];
        CeilingRegions::default_for_footprint(vec![l_ring], 2.4, ItemId::default())
            .with_z_deltas(&[(square(0.0, 0.0, 10.0, 4.0), 0.3)])
    }

    /// `Some(z)` within float noise of `want` (2.4 + 0.3 lands at
    /// 2.6999999999999997 — `with_z_deltas` sums deltas in f64).
    fn assert_approx(got: Option<f64>, want: f64) {
        assert!(
            got.is_some_and(|z| (z - want).abs() < 1e-9),
            "want ~{want}, got {got:?}"
        );
    }

    #[test]
    fn lowest_flat_z_over_takes_min_of_overlapping_flats() {
        let ceilings = stepped_l_roofs();
        // Footprint covering both arms → the LOWER level anchors.
        assert_approx(
            ceilings.lowest_flat_z_over(&square(0.0, 0.0, 10.0, 10.0)),
            2.4,
        );
        // Footprint strictly inside the raised bottom arm only.
        assert_approx(
            ceilings.lowest_flat_z_over(&square(1.0, 1.0, 9.0, 3.0)),
            2.7,
        );
        // Off to the side → no coverage.
        assert_eq!(
            ceilings.lowest_flat_z_over(&square(20.0, 20.0, 30.0, 30.0)),
            None
        );
        // A boundary graze (sub-sliver overlap) is not coverage.
        assert_eq!(
            ceilings.lowest_flat_z_over(&square(10.0, 0.0, 20.0, 4.0)),
            None
        );
        // Degenerate footprint.
        assert_eq!(ceilings.lowest_flat_z_over(&[]), None);
    }

    #[test]
    fn inherited_floor_levels_pieces_at_z_minus_anchor() {
        use geo::Area;
        let ceilings = stepped_l_roofs();
        // Footprint over both arms: anchor 2.4, ONE inherited level — the
        // bottom arm at 2.7 − 2.4 = 0.3 (the slab thickness cancels).
        let levels = ceilings.inherited_floor_levels(&square(0.0, 0.0, 10.0, 10.0));
        assert_eq!(levels.len(), 1, "{levels:?}");
        let (ring, level) = &levels[0];
        assert!((level - 0.3).abs() < 1e-9);
        let area = ring_to_poly(ring).unsigned_area();
        assert!(
            (area - 40.0).abs() < 1e-6,
            "the whole bottom arm, got {area}"
        );
        // f32-quantized at the boundary (the add_ceiling_apex precedent).
        for p in ring {
            assert_eq!(p.x, p.x as f32 as f64);
            assert_eq!(p.y, p.y as f32 as f64);
        }
        // A footprint clipped to part of both arms inherits only the covered
        // piece of the raised arm: (2..10, 0..4) = 32 m².
        let clipped = ceilings.inherited_floor_levels(&square(2.0, 0.0, 10.0, 10.0));
        assert_eq!(clipped.len(), 1, "{clipped:?}");
        let clipped_area = ring_to_poly(&clipped[0].0).unsigned_area();
        assert!(
            (clipped_area - 32.0).abs() < 1e-6,
            "8 × 4 piece, got {clipped_area}"
        );
    }

    #[test]
    fn inherited_floor_levels_anchor_level_and_no_anchor_yield_empty() {
        let ceilings = stepped_l_roofs();
        // Footprint on the raised arm only: it IS the anchor — no step.
        assert!(ceilings
            .inherited_floor_levels(&square(1.0, 1.0, 9.0, 3.0))
            .is_empty());
        // No coverage at all → no anchor → empty.
        assert!(ceilings
            .inherited_floor_levels(&square(20.0, 20.0, 30.0, 30.0))
            .is_empty());
    }

    #[test]
    fn inherited_floor_levels_sloped_regions_contribute_nothing() {
        // Flat anchor on the left half, a sloped region on the right half.
        let mut ceilings = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 5.0, 10.0)],
            2.4,
            ItemId::default(),
        );
        ceilings.regions.push(CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(5.0, 0.0, 2.4),
                Point3f64::new(10.0, 0.0, 3.4),
                Point3f64::new(10.0, 10.0, 3.4),
                Point3f64::new(5.0, 10.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        });
        // The sloped half is covered but warn-logged out — no levels.
        assert!(ceilings
            .inherited_floor_levels(&square(0.0, 0.0, 10.0, 10.0))
            .is_empty());
        // And it never anchors: only the flat half does.
        assert_eq!(
            ceilings.lowest_flat_z_over(&square(0.0, 0.0, 10.0, 10.0)),
            Some(2.4)
        );
    }

    #[test]
    fn set_floor_thickness_stamps_existing_storey_only() {
        let mut ber = Ber::default();
        ber.storeys.push(Storey::default());
        ber.storeys.push(Storey::default());
        ber.set_floor_thickness(1, 0.3);
        assert_eq!(
            ber.storeys[0].floor_thickness_m, None,
            "other storey untouched"
        );
        assert_eq!(ber.storeys[1].floor_thickness_m, Some(0.3));
        // Re-stamping overwrites (the Heights re-edit).
        ber.set_floor_thickness(1, 0.25);
        assert_eq!(ber.storeys[1].floor_thickness_m, Some(0.25));
        // Out-of-range is a no-op.
        ber.set_floor_thickness(7, 0.3);
    }

    #[test]
    fn storey_floor_datums_legacy_stacking_is_ceiling_height_sum() {
        let mut ber = Ber::default();
        for h in [Some(2.4), Some(2.6), Some(3.0)] {
            ber.storeys.push(Storey {
                ceiling_height_m: h,
                ..Storey::default()
            });
        }
        // No thicknesses anywhere → byte-identical to the old stacking:
        // z += below.ceiling_height_m.unwrap_or_default().
        assert_eq!(ber.storey_floor_datums(), vec![0.0, 2.4, 5.0]);
        // An unset height counts 0 — callers gate before trusting the stack.
        ber.storeys[1].ceiling_height_m = None;
        assert_eq!(ber.storey_floor_datums(), vec![0.0, 2.4, 2.4]);
    }

    #[test]
    fn storey_floor_datums_anchors_thickness_on_lowest_covered_flat() {
        let cid = ItemId::default();
        let below = Storey {
            floors: FloorRegions::new(square(0.0, 0.0, 10.0, 10.0), cid),
            ceilings: stepped_l_roofs(),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        };
        // Upper footprint strictly inside the RAISED arm → anchor 2.7.
        let upper_on_raised = Storey {
            floors: FloorRegions::new(square(1.0, 1.0, 9.0, 3.0), cid),
            floor_thickness_m: Some(0.3),
            ..Storey::default()
        };
        let mut ber = Ber::default();
        ber.storeys = vec![below.clone(), upper_on_raised];
        let datums_approx = |ber: &Ber, want: [f64; 2], why: &str| {
            let got = ber.storey_floor_datums();
            assert!(
                got.len() == 2 && got.iter().zip(want).all(|(g, w)| (g - w).abs() < 1e-9),
                "{why}: want ~{want:?}, got {got:?}"
            );
        };
        datums_approx(&ber, [0.0, 3.0], "2.7 + 0.3");
        // Upper footprint spanning BOTH arms → the LOW flat anchors.
        ber.storeys[1].floors = FloorRegions::new(square(0.0, 0.0, 10.0, 10.0), cid);
        datums_approx(&ber, [0.0, 2.7], "2.4 + 0.3");
        // No flat coverage (footprint off to the side) → fall back to the
        // below ceiling height.
        ber.storeys[1].floors = FloorRegions::new(square(20.0, 20.0, 26.0, 26.0), cid);
        datums_approx(&ber, [0.0, 2.7], "2.4 + 0.3 fallback");
        // No footprint at all (fresh storey) → same fallback.
        ber.storeys[1].floors = FloorRegions::default();
        datums_approx(&ber, [0.0, 2.7], "fresh storey fallback");
    }

    #[test]
    fn storey_json_without_floor_thickness_deserializes_to_none() {
        // JSON produced by the OLD shape (no `floor_thickness_m` field).
        let storey = Storey::default();
        let mut json = serde_json::to_value(&storey).expect("serialize");
        assert!(json
            .as_object_mut()
            .unwrap()
            .remove("floor_thickness_m")
            .is_some());
        let old: Storey = serde_json::from_value(json).expect("old JSON still loads");
        assert_eq!(old.floor_thickness_m, None);
    }

    /* ---------------------------------------------------------------------
     * Polygon3D::split_by_line (moved from the dissolved gen_3d_vol crate)
     * ------------------------------------------------------------------ */

    /// A 4 m × 3 m vertical wall face in the y = 0 plane.
    fn wall_face() -> Polygon3D {
        Polygon3D::new(vec![
            Point3f64::new(0.0, 0.0, 0.0),
            Point3f64::new(4.0, 0.0, 0.0),
            Point3f64::new(4.0, 0.0, 3.0),
            Point3f64::new(0.0, 0.0, 3.0),
        ])
    }

    #[test]
    fn polygon3d_plane_normal_is_unit_and_perpendicular() {
        let (n, _) = wall_face().get_normal_and_distance_to_origin().unwrap();
        assert!((n.norm() - 1.0).abs() < 1e-9);
        // perpendicular to both spanning edges of the face
        assert!(n.dot(&Vector3f64::new(1.0, 0.0, 0.0)).abs() < 1e-9);
        assert!(n.dot(&Vector3f64::new(0.0, 0.0, 1.0)).abs() < 1e-9);
    }

    #[test]
    fn polygon3d_degenerate_has_no_plane_normal() {
        let degenerate = Polygon3D::new(vec![
            Point3f64::new(0.0, 0.0, 0.0),
            Point3f64::new(1.0, 0.0, 0.0),
        ]);
        assert!(degenerate.get_normal_and_distance_to_origin().is_none());
    }

    #[test]
    fn split_by_line_divides_wall_face_at_intercept() {
        let line = SplitLine3D {
            point: Point3f64::new(1.0, 0.0, 0.0),
            direction: Vector3f64::new(0.0, 0.0, 1.0), // vertical, in the face plane
            normal: Vector3f64::new(1.0, 0.0, 0.0),
        };
        let halves = wall_face().split_by_line(&line);
        assert_eq!(halves.len(), 2);
        for half in &halves {
            assert_eq!(half.exterior.len(), 4);
        }
        let xs = |p: &Polygon3D| {
            let min = p.exterior.iter().map(|v| v.x).fold(f64::MAX, f64::min);
            let max = p.exterior.iter().map(|v| v.x).fold(f64::MIN, f64::max);
            (min, max)
        };
        let (mut a, mut b) = (xs(&halves[0]), xs(&halves[1]));
        if a.0 > b.0 {
            std::mem::swap(&mut a, &mut b);
        }
        assert_eq!(a, (0.0, 1.0));
        assert_eq!(b, (1.0, 4.0));
    }

    #[test]
    fn split_by_line_outside_face_leaves_one_half_empty() {
        let line = SplitLine3D {
            point: Point3f64::new(-1.0, 0.0, 0.0),
            direction: Vector3f64::new(0.0, 0.0, 1.0),
            normal: Vector3f64::new(1.0, 0.0, 0.0),
        };
        let halves = wall_face().split_by_line(&line);
        assert_eq!(halves.len(), 2);
        let mut sizes: Vec<usize> = halves.iter().map(|h| h.exterior.len()).collect();
        sizes.sort();
        assert_eq!(sizes, vec![0, 4]);
    }

    /* ---------------------------------------------------------------------
     * WallRegions::apply_splits — structure-phase wall split
     * ------------------------------------------------------------------ */

    /// A flat 4 m × 2.5 m wall from (0,0) to (4,0) in the y = 0 plane.
    fn four_m_wall(cid: ItemId, exposure: WallExposure) -> WallRegion {
        WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 0.0),
                Point3f64::new(4.0, 0.0, 2.5),
                Point3f64::new(0.0, 0.0, 2.5),
            ]),
            exposure,
            opes: Opes::default(),
            construction_id: cid,
        }
    }

    #[test]
    fn apply_splits_divides_flat_wall_copying_props_with_fresh_ids() {
        let cid = ItemId::default();
        let wall = four_m_wall(cid, WallExposure::Exposed);
        let parent_id = wall.id;
        let walls = WallRegions {
            regions: vec![wall],
        };
        // Vertical cut line at x = 1 crossing the base edge.
        let cut = [Point2f64::new(1.0, -1.0), Point2f64::new(1.0, 1.0)];
        let out = walls.apply_splits(&[cut]);
        assert_eq!(out.regions.len(), 2, "one cut → two sub-walls");
        for r in &out.regions {
            assert_eq!(r.exposure, WallExposure::Exposed, "exposure copied to both");
            assert_eq!(r.construction_id, cid, "construction copied to both");
            assert_ne!(r.id, parent_id, "fresh id per piece (nothing to amend)");
        }
        let ids: std::collections::HashSet<_> = out.regions.iter().map(|r| r.id).collect();
        assert_eq!(ids.len(), 2, "distinct ids");
        // Base edges abut at the cut: far endpoints are x = 1 and x = 4.
        let mut far_x: Vec<f64> = out
            .regions
            .iter()
            .map(|r| r.base_edge().unwrap().1.x)
            .collect();
        far_x.sort_by(|a, b| a.partial_cmp(b).unwrap());
        assert!((far_x[0] - 1.0).abs() < 1e-9 && (far_x[1] - 4.0).abs() < 1e-9);
    }

    #[test]
    fn apply_splits_missing_wall_is_left_whole() {
        let cid = ItemId::default();
        let walls = WallRegions {
            regions: vec![four_m_wall(cid, WallExposure::Exposed)],
        };
        let cut = [Point2f64::new(10.0, -1.0), Point2f64::new(10.0, 1.0)]; // misses the wall
        let out = walls.apply_splits(&[cut]);
        assert_eq!(out.regions.len(), 1, "untouched wall survives the miss");
    }

    #[test]
    fn apply_splits_empty_is_a_noop() {
        let cid = ItemId::default();
        let wall = four_m_wall(cid, WallExposure::Exposed);
        let id = wall.id;
        let out = WallRegions {
            regions: vec![wall],
        }
        .apply_splits(&[]);
        assert_eq!(out.regions.len(), 1);
        assert_eq!(out.regions[0].id, id, "empty cuts → untouched");
    }

    // ── Copy-height helpers ─────────────────────────────────────────────────

    /// A flat-topped 4-vertex quad wall reports its single top z; a gabled top
    /// (a peak vertex breaking the flat run) reports `None`.
    #[test]
    fn flat_top_z_some_for_flat_top_none_for_gabled() {
        let flat = WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0), // a base
                Point3f64::new(4.0, 0.0, 0.0), // b base
                Point3f64::new(4.0, 0.0, 2.5), // b top
                Point3f64::new(0.0, 0.0, 2.5), // a top
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        };
        assert_eq!(flat.flat_top_z(), Some(2.5));

        let gabled = WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),  // a base
                Point3f64::new(10.0, 0.0, 0.0), // b base
                Point3f64::new(10.0, 0.0, 2.5), // b top
                Point3f64::new(5.0, 0.0, 3.5),  // ridge peak — top varies
                Point3f64::new(0.0, 0.0, 2.5),  // a top
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        };
        assert_eq!(gabled.flat_top_z(), None, "a gabled top has no single z");
    }

    /// `copy_height_ceiling_at` reads a KNOWN height and only a known one:
    /// (a) flat ceiling area, (b) sloped area → None, (c) level ridge,
    /// (d) sloped ridge → None, (e) flat-topped wall.
    #[test]
    fn copy_height_ceiling_at_reads_known_heights() {
        let cid = ItemId::default();

        // (a) FLAT ceiling area at z = 2.6 → its z inside.
        let flat_storey = Storey {
            ceilings: CeilingRegions::default_for_footprint(
                vec![square(0.0, 0.0, 4.0, 4.0)],
                2.6,
                cid,
            ),
            ..Default::default()
        };
        assert_eq!(flat_storey.copy_height_ceiling_at(2.0, 2.0, 0.1), Some(2.6));

        // (b) SLOPED ceiling area → None at a mid-slope point.
        let sloped_storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![CeilingRegion {
                    id: ItemId::default(),
                    polygon: Polygon3D::new(vec![
                        Point3f64::new(0.0, 0.0, 2.4),
                        Point3f64::new(4.0, 0.0, 2.4),
                        Point3f64::new(4.0, 4.0, 3.1),
                        Point3f64::new(0.0, 4.0, 3.1),
                    ]),
                    opes: Opes::default(),
                    construction_id: cid,
                    exposure: CeilingExposure::default(),
                }],
            },
            ..Default::default()
        };
        assert_eq!(sloped_storey.copy_height_ceiling_at(2.0, 2.0, 0.1), None);

        // (c) LEVEL ridge (both apexes at z = 4.2) → z near the ridge segment.
        let (a, b) = gable_apexes();
        let level_apexes = vec![a, b];
        let level_storey = Storey {
            ceilings: gable_footprint(cid).with_apexes(&level_apexes),
            ceiling_apexes: level_apexes,
            ..Default::default()
        };
        assert_eq!(
            level_storey.copy_height_ceiling_at(5.0, 2.0, 0.1),
            Some(4.2)
        );

        // (d) SLOPED ridge (apexes at differing z) → None at a mid-ridge point
        // far from either apex (no other known surface there).
        let sloped_apexes = vec![
            CeilingApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.2,
            },
            CeilingApex {
                point: Point2f64::new(8.0, 2.0),
                z_m: 3.0,
            },
        ];
        let sloped_ridge_storey = Storey {
            ceilings: gable_footprint(cid).with_apexes(&sloped_apexes),
            ceiling_apexes: sloped_apexes,
            ..Default::default()
        };
        assert_eq!(
            sloped_ridge_storey.copy_height_ceiling_at(5.0, 2.0, 0.1),
            None
        );

        // (e) FLAT-TOPPED wall → its top z near the base edge.
        let wall_storey = Storey {
            walls: WallRegions {
                regions: vec![WallRegion {
                    id: ItemId::default(),
                    polygon: Polygon3D::new(vec![
                        Point3f64::new(0.0, 0.0, 0.0),
                        Point3f64::new(10.0, 0.0, 0.0),
                        Point3f64::new(10.0, 0.0, 2.5),
                        Point3f64::new(0.0, 0.0, 2.5),
                    ]),
                    exposure: WallExposure::Exposed,
                    opes: Opes::default(),
                    construction_id: cid,
                }],
            },
            ..Default::default()
        };
        assert_eq!(wall_storey.copy_height_ceiling_at(5.0, 0.0, 0.1), Some(2.5));
    }

    /// `copy_height_floor_at` returns a floor area's `level_m` inside it, and
    /// `None` off any floor region.
    #[test]
    fn copy_height_floor_at_reads_level_or_none() {
        let cid = ItemId::default();
        let mut floors = FloorRegions::new(square(0.0, 0.0, 10.0, 10.0), cid);
        floors.regions[0].level_m = 0.3;
        let storey = Storey {
            floors,
            ..Default::default()
        };
        assert_eq!(storey.copy_height_floor_at(5.0, 5.0, 0.1), Some(0.3));
        assert_eq!(storey.copy_height_floor_at(50.0, 50.0, 0.1), None);
    }
}
