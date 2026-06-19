// src/data/structural.rs
//! Building-structure data model: constructions, floor/ceiling/wall
//! regions, 3D polygon primitives, the roof-loft geometry kernel, and
//! the Storey container. Split from a single file for navigability;
//! the public surface is unchanged via the re-exports below.

mod ceiling_regions;
mod constructions;
mod floor_regions;
mod poly3d;
mod roof_geom;
mod storey;
mod wall_regions;

pub use ceiling_regions::*;
pub use constructions::*;
pub use floor_regions::*;
pub use poly3d::*;
pub use roof_geom::*;
pub use storey::*;
pub use wall_regions::*;

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
        }
    }

    /// A 10×4 ring with a deep SLOT cut from the right (y ∈ [1, 3], reaching
    /// in to x = 3) — a C-shape (the roof_point notched ring).
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

    /// FLAT PARITY with the old `rebuild_preserving`: empty roofs → a fresh
    /// segment becomes the classic 4-vertex vertical quad at the fallback
    /// top, with the supplied default construction.
    #[test]
    fn regenerate_with_empty_roofs_makes_flat_vertical_planes() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(4.0, 0.0);
        let default_cid = ItemId::default();
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::SemiExposed)],
            &WallRegions::default(),
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

    #[test]
    fn regenerate_covered_split_inherits_construction_id_only() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        // A whole-wall region pointing at some shared construction, carrying
        // an opening.
        let orig_cid = ItemId::default();
        let mut previous = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            orig_cid,
        );
        previous.regions[0].opes.windows.push(ItemId::default());
        let prev_id = previous.regions[0].id;
        assert_eq!(previous.regions[0].construction_id, orig_cid);

        // Split it into 3 pieces; each inherits the covered construction
        // (NOT the fresh default) but NOT the id or the opes — a split is
        // positional, ambiguous to carry openings through.
        let fresh_cid = ItemId::default();
        let split = WallRegions::regenerate(
            vec![
                (a, Point2f64::new(2.0, 0.0), WallExposure::Exposed),
                (
                    Point2f64::new(2.0, 0.0),
                    Point2f64::new(5.0, 0.0),
                    WallExposure::SemiExposed,
                ),
                (Point2f64::new(5.0, 0.0), b, WallExposure::Exposed),
            ],
            &previous,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            fresh_cid,
        );
        assert_eq!(split.regions.len(), 3);
        for r in &split.regions {
            assert_eq!(r.construction_id, orig_cid);
            assert_ne!(r.id, prev_id, "split pieces get fresh ids");
            assert!(r.opes.windows.is_empty(), "opes are not split");
        }
        // Exposure still set per piece.
        assert_eq!(split.regions[1].exposure, WallExposure::SemiExposed);
    }

    /// AMEND: a segment whose base edge matches a previous region's (within
    /// 1e-2 m, either orientation) keeps that region's id, construction AND
    /// openings — a roof change re-shapes the wall without orphaning them.
    #[test]
    fn regenerate_amend_keeps_id_cid_and_opes() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(6.0, 0.0);
        let cid = ItemId::default();
        let mut previous = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
        let ope = ItemId::default();
        previous.regions[0].opes.doors.push(ope);
        let prev_id = previous.regions[0].id;

        // Same wall, nudged within tolerance AND flipped in orientation; the
        // top now reaches a different height (the roof changed).
        let walls = WallRegions::regenerate(
            vec![(
                Point2f64::new(6.004, 0.0),
                Point2f64::new(0.0, 0.003),
                WallExposure::SemiExposed,
            )],
            &previous,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            3.1,
            ItemId::default(),
        );
        assert_eq!(walls.regions.len(), 1);
        let w = &walls.regions[0];
        assert_eq!(w.id, prev_id, "same wall keeps its id");
        assert_eq!(w.construction_id, cid);
        assert_eq!(w.opes.doors, vec![ope], "openings survive the amend");
        assert_eq!(w.exposure, WallExposure::SemiExposed, "exposure is new");
        assert!(w.polygon.exterior.iter().any(|v| (v.z - 3.1).abs() < 1e-9));
    }

    /// INJECTIVE matching: when two new segments both fall within tolerance
    /// of ONE previous region, only the best match (smallest endpoint error)
    /// inherits its id — ids are never duplicated.
    #[test]
    fn regenerate_amend_matching_is_injective_best_match() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(6.0, 0.0);
        let cid = ItemId::default();
        let previous = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
        let prev_id = previous.regions[0].id;

        // Worse-but-qualifying candidate FIRST, exact match second — the
        // exact one must still win the id.
        let walls = WallRegions::regenerate(
            vec![
                (
                    Point2f64::new(0.008, 0.0),
                    Point2f64::new(6.008, 0.0),
                    WallExposure::Exposed,
                ),
                (a, b, WallExposure::Exposed),
            ],
            &previous,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            ItemId::default(),
        );
        assert_eq!(walls.regions.len(), 2);
        assert_ne!(
            walls.regions[0].id, prev_id,
            "near-match loses to the exact match"
        );
        assert_eq!(walls.regions[1].id, prev_id, "best match keeps the id");
        assert_ne!(walls.regions[0].id, walls.regions[1].id, "ids unique");
        // The loser still sits on the old base, so it inherits the
        // construction via the covered-split rule.
        assert_eq!(walls.regions[0].construction_id, cid);
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
        let roofs =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid)
                .with_z_deltas(&[(square(2.0, 0.0, 4.0, 4.0), 0.3)]);
        assert_eq!(roofs.regions.len(), 2);
        let faces = roofs.riser_faces();
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
            .with_apexes(&[RoofApex {
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

    /// `with_risers` amend identity: a re-derived riser keeps its previous
    /// id, construction and opes; a riser face landing on an id the
    /// perimeter pass already claimed can never duplicate it (exclusion);
    /// an unmatched-but-covered face inherits the construction only.
    #[test]
    fn with_risers_amend_keeps_identity_and_never_duplicates_ids() {
        let cid = ItemId::default();
        let segs = || {
            vec![(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                WallExposure::Exposed,
            )]
        };
        let riser = || {
            Polygon3D::new(vec![
                Point3f64::new(0.0, 2.0, -0.3),
                Point3f64::new(4.0, 2.0, -0.3),
                Point3f64::new(4.0, 2.0, 0.0),
                Point3f64::new(0.0, 2.0, 0.0),
            ])
        };
        // First derivation: one perimeter wall + one interior riser.
        let mut walls1 = WallRegions::regenerate(
            segs(),
            &WallRegions::default(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        )
        .with_risers(
            vec![(riser(), WallExposure::Ground)],
            &WallRegions::default(),
            cid,
        );
        assert_eq!(walls1.regions.len(), 2);
        let wall_id = walls1.regions[0].id;
        let riser_id = walls1.regions[1].id;
        assert_ne!(wall_id, riser_id, "ids unique on first derivation");
        assert_eq!(walls1.regions[1].exposure, WallExposure::Ground);
        // Decorate the riser as later edits would: its own cid + an ope + a
        // user-overridden exposure.
        let riser_cid = ItemId::default();
        let ope = ItemId::default();
        walls1.regions[1].construction_id = riser_cid;
        walls1.regions[1].opes.doors.push(ope);
        walls1.regions[1].exposure = WallExposure::SemiExposed;

        // Re-derive from walls1: the perimeter amends, the riser re-derives
        // deterministically — and keeps its identity.
        let walls2 = WallRegions::regenerate(
            segs(),
            &walls1,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            ItemId::default(),
        )
        .with_risers(
            vec![(riser(), WallExposure::Ground)],
            &walls1,
            ItemId::default(),
        );
        assert_eq!(walls2.regions.len(), 2);
        assert_eq!(walls2.regions[0].id, wall_id, "perimeter amend keeps id");
        let r = &walls2.regions[1];
        assert_eq!(r.id, riser_id, "riser amend keeps id across re-derivation");
        assert_eq!(r.construction_id, riser_cid);
        assert_eq!(r.opes.doors, vec![ope], "openings survive the amend");
        assert_eq!(
            r.exposure,
            WallExposure::SemiExposed,
            "the overridden exposure survives the amend (durability rule) — \
             the caller's freshly-supplied default never re-stamps it"
        );

        // EXCLUSION: a riser face whose base coincides with the perimeter
        // wall's base can't steal its already-claimed id — it gets a fresh
        // id and inherits the construction via the covered-split rule.
        let on_wall = Polygon3D::new(vec![
            Point3f64::new(0.0, 0.0, -0.3),
            Point3f64::new(4.0, 0.0, -0.3),
            Point3f64::new(4.0, 0.0, 0.0),
            Point3f64::new(0.0, 0.0, 0.0),
        ]);
        let walls3 = WallRegions::regenerate(
            segs(),
            &walls1,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            ItemId::default(),
        )
        .with_risers(
            vec![(on_wall, WallExposure::Ground)],
            &walls1,
            ItemId::default(),
        );
        assert_eq!(walls3.regions.len(), 2);
        assert_eq!(walls3.regions[0].id, wall_id);
        assert_ne!(
            walls3.regions[1].id, wall_id,
            "a claimed id is never duplicated"
        );
        assert_ne!(walls3.regions[1].id, riser_id);
        assert_eq!(
            walls3.regions[1].construction_id, cid,
            "covered base inherits the construction only"
        );
        assert!(walls3.regions[1].opes.doors.is_empty(), "no opes inherited");
        assert_eq!(
            walls3.regions[1].exposure,
            WallExposure::Ground,
            "a FRESH (unmatched) riser takes the derived default"
        );
    }

    /// A floor riser and a ceiling riser SHARE the plan base edge under a
    /// step line — the z-aware ranking keeps each matched to its OWN
    /// previous region. In particular a floor face appearing for the first
    /// time next to an existing ceiling riser must NOT steal its identity
    /// (id/opes/exposure): the ceiling face out-ranks it on z, the floor
    /// face derives fresh with the caller's default.
    #[test]
    fn with_risers_never_swaps_colocated_floor_and_ceiling_risers() {
        let cid = ItemId::default();
        let face_at = |z_lo: f64, z_hi: f64| {
            Polygon3D::new(vec![
                Point3f64::new(0.0, 2.0, z_lo),
                Point3f64::new(4.0, 2.0, z_lo),
                Point3f64::new(4.0, 2.0, z_hi),
                Point3f64::new(0.0, 2.0, z_hi),
            ])
        };
        // Previous state: only the CEILING riser exists (2.4 → 2.7).
        let prev = WallRegions::default().with_risers(
            vec![(face_at(2.4, 2.7), WallExposure::Exposed)],
            &WallRegions::default(),
            cid,
        );
        let ceiling_id = prev.regions[0].id;
        // A floor step now lands on the SAME plan line: the new derive has
        // both faces. The ceiling face keeps its id; the floor face is
        // FRESH and takes the caller's supplied default (Ground here —
        // arbitrary for this test; the app supplies Unset).
        let walls = WallRegions::default().with_risers(
            vec![
                (face_at(-0.3, 0.0), WallExposure::Ground),
                (face_at(2.4, 2.7), WallExposure::Exposed),
            ],
            &prev,
            cid,
        );
        assert_eq!(walls.regions.len(), 2);
        let floor = &walls.regions[0];
        let ceiling = &walls.regions[1];
        assert_eq!(ceiling.id, ceiling_id, "ceiling riser keeps its identity");
        assert_ne!(floor.id, ceiling_id, "floor riser never steals it");
        assert_eq!(floor.exposure, WallExposure::Ground, "fresh → default");
        assert_eq!(ceiling.exposure, WallExposure::Exposed);
    }

    /// Durability across a GEOMETRY edit: the perimeter changes (so the
    /// perimeter pass re-derives different segments) but the riser's base
    /// edge is unchanged — the amend still base-matches and the overridden
    /// exposure survives the fresh derivation.
    #[test]
    fn with_risers_override_survives_a_geometry_edit_that_still_base_matches() {
        let cid = ItemId::default();
        let riser = || {
            Polygon3D::new(vec![
                Point3f64::new(0.0, 2.0, -0.3),
                Point3f64::new(4.0, 2.0, -0.3),
                Point3f64::new(4.0, 2.0, 0.0),
                Point3f64::new(0.0, 2.0, 0.0),
            ])
        };
        let segs_before = vec![(
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            WallExposure::Exposed,
        )];
        // The wall moved to y = -1 (an outline edit); the step line did not.
        let segs_after = vec![(
            Point2f64::new(0.0, -1.0),
            Point2f64::new(4.0, -1.0),
            WallExposure::Exposed,
        )];
        let mut walls1 = WallRegions::regenerate(
            segs_before,
            &WallRegions::default(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        )
        .with_risers(
            vec![(riser(), WallExposure::Ground)],
            &WallRegions::default(),
            cid,
        );
        let riser_id = walls1.regions[1].id;
        walls1.regions[1].exposure = WallExposure::Exposed; // user override

        let walls2 = WallRegions::regenerate(
            segs_after,
            &walls1,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        )
        .with_risers(vec![(riser(), WallExposure::Ground)], &walls1, cid);
        let r = walls2
            .regions
            .iter()
            .find(|r| r.id == riser_id)
            .expect("riser amends across the geometry edit");
        assert_eq!(
            r.exposure,
            WallExposure::Exposed,
            "override survives the fresh derive after the edit"
        );
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
            &WallRegions::default(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        )
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
            &WallRegions::default(),
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
            &WallRegions::default(),
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
        let walls = flat_wall(cid).with_risers(
            vec![(riser, WallExposure::Unset)],
            &WallRegions::default(),
            cid,
        );
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
            &WallRegions::default(),
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
            &WallRegions::default(),
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
            &WallRegions::default(),
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

    /// THE DELIBERATE BEHAVIOUR CHANGE: a riser the user previously painted
    /// (separate region, overridden exposure, own opes) that NOW comes out
    /// flush with a wall merges into it — the HOST's exposure wins and the
    /// riser's old identity/opes/exposure are discarded.
    #[test]
    fn painted_riser_that_now_merges_loses_to_the_host_exposure() {
        let cid = ItemId::default();
        // Previous state, as persisted by an older derivation: the wall and
        // a SEPARATE riser region the user painted SemiExposed.
        let mut previous = flat_wall(cid);
        let wall_id = previous.regions[0].id;
        let old_riser = WallRegion {
            id: ItemId::default(),
            polygon: coplanar_face(4.0, 6.0, 1.0, 2.4),
            exposure: WallExposure::SemiExposed, // the user's old override
            opes: Opes::default(),
            construction_id: cid,
        };
        let old_riser_id = old_riser.id;
        previous.regions.push(old_riser);

        // Re-derive: the riser face amends its old region (picking up the
        // override) and THEN merges — the host's attribution wins.
        let walls = WallRegions::regenerate(
            vec![(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                WallExposure::Exposed,
            )],
            &previous,
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        )
        .with_risers(
            vec![(coplanar_face(4.0, 6.0, 1.0, 2.4), WallExposure::Unset)],
            &previous,
            cid,
        );
        assert_eq!(walls.regions.len(), 1, "merged despite the old override");
        assert_eq!(walls.regions[0].id, wall_id);
        assert_eq!(
            walls.regions[0].exposure,
            WallExposure::Exposed,
            "the host wall's exposure wins over the riser's old override"
        );
        assert!(
            walls.regions.iter().all(|r| r.id != old_riser_id),
            "the merged riser's identity is gone"
        );
    }

    /// Merging is IDEMPOTENT across re-derivations: feeding the merged
    /// result back as `previous` re-derives the identical merged region
    /// (id stable, polygon byte-identical).
    #[test]
    fn merged_wall_rederives_identically() {
        let cid = ItemId::default();
        let segs = || {
            vec![(
                Point2f64::new(0.0, 0.0),
                Point2f64::new(4.0, 0.0),
                WallExposure::Exposed,
            )]
        };
        let derive = |previous: &WallRegions| {
            WallRegions::regenerate(
                segs(),
                previous,
                &CeilingRegions::default(),
                &FloorRegions::default(),
                0.0,
                2.4,
                cid,
            )
            .with_risers(
                vec![(coplanar_face(4.0, 6.0, 1.0, 2.4), WallExposure::Unset)],
                previous,
                cid,
            )
        };
        let walls1 = derive(&WallRegions::default());
        let walls2 = derive(&walls1);
        assert_eq!(walls1.regions.len(), 1);
        assert_eq!(walls2.regions.len(), 1);
        assert_eq!(walls2.regions[0].id, walls1.regions[0].id, "id stable");
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
        let roofs =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 10.0, 4.0)], 2.4, cid)
                .with_z_deltas(&[(square(5.0, 0.0, 10.0, 4.0), 0.3)]);
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            &roofs,
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
            &WallRegions::default(),
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
            &WallRegions::default(),
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
            &WallRegions::default(),
            &CeilingRegions::default(),
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
        // A SINGLE region at level 0, plus a SAME-LEVEL partition seam at x=2
        // (a phase split, not a step): the base must still collapse to [a, b].
        let floors = stepped_floor(square(0.0, 0.0, 2.0, 4.0), square(2.0, 0.0, 4.0, 4.0), 0.0);
        let stepped = WallRegions::regenerate(
            segs(),
            &WallRegions::default(),
            &CeilingRegions::default(),
            &floors,
            0.0,
            2.4,
            cid,
        );
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
        let roofs = CeilingRegions {
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
            }],
        };
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            &roofs,
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

    /// Spans of the base the roofs DON'T cover top out at `fallback_top_z`,
    /// with a jump where coverage ends.
    #[test]
    fn regenerate_uncovered_span_uses_fallback_top() {
        let cid = ItemId::default();
        // Roof over the left half of the base only.
        let roofs =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 5.0, 4.0)], 2.6, cid);
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        let walls = WallRegions::regenerate(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            &roofs,
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
        let apex = RoofApex {
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
        let apex = RoofApex {
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
        let apex = RoofApex {
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
        let out = ceilings.with_apexes(&[RoofApex {
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
            RoofApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.0,
            },
            RoofApex {
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
        };
        assert_eq!(sloped.fan_base_z(&[]), None);

        // The same triangle WITH its apex declared: base recovered.
        let apex = RoofApex {
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
        };
        assert_eq!(
            all_apex.fan_base_z(&[RoofApex {
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
    fn apex_vertex_count(r: &CeilingRegion, apexes: &[RoofApex]) -> usize {
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
    fn gable_apexes() -> (RoofApex, RoofApex) {
        (
            RoofApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.2,
            },
            RoofApex {
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
        let at = |v: &Point3f64, apex: &RoofApex| {
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
        let a = RoofApex {
            point: Point2f64::new(2.0, 2.0),
            z_m: 4.0,
        };
        let b = RoofApex {
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
        let a = RoofApex {
            point: Point2f64::new(0.0, 0.0), // ON the footprint corner
            z_m: 4.0,
        };
        let b = RoofApex {
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
        let c = RoofApex {
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
            RoofApex {
                point: Point2f64::new(2.0, 2.0),
                z_m: 4.0,
            },
            RoofApex {
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
            .apex_groups(&[RoofApex {
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
        let apex = RoofApex {
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
            RoofApex {
                point: Point2f64::new(2.0, 0.0), // ON the bottom wall
                z_m: 4.2,
            },
            RoofApex {
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
        let mut roofs =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid);
        roofs.regions.extend(
            CeilingRegions::default_for_footprint(vec![square(4.0, 0.0, 8.0, 4.0)], 2.4, cid)
                .regions,
        );
        // A ridge along the shared line x = 4 at z = 4.0.
        let apexes = vec![
            RoofApex {
                point: Point2f64::new(4.0, 0.0),
                z_m: 4.0,
            },
            RoofApex {
                point: Point2f64::new(4.0, 4.0),
                z_m: 4.0,
            },
        ];
        // Does any output face on side `right_of_4` reach the ridge height?
        let side_slopes = |roofs: &CeilingRegions, right_of_4: bool| {
            roofs.regions.iter().any(|r| {
                let cx = r.polygon.exterior.iter().map(|v| v.x).sum::<f64>()
                    / r.polygon.exterior.len().max(1) as f64;
                let on_side = if right_of_4 { cx > 4.0 } else { cx < 4.0 };
                on_side && r.polygon.exterior.iter().any(|v| (v.z - 4.0).abs() < 1e-6)
            })
        };
        // Hint into the RIGHT area → it slopes, the left stays flat.
        let hints = vec![
            RoofApexHint {
                apex: (4.0, 0.0),
                region: (6.0, 2.0),
            },
            RoofApexHint {
                apex: (4.0, 4.0),
                region: (6.0, 2.0),
            },
        ];
        let hinted = roofs.clone().with_apexes_hinted(&apexes, &hints);
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
        let unhinted = roofs.with_apexes_hinted(&apexes, &[]);
        assert!(
            side_slopes(&unhinted, false),
            "without a hint the left slopes"
        );
    }

    // ── rect (pavilion) ridges ──────────────────────────────────────────────

    /// The four corner apexes of the canonical pavilion, over the rect
    /// `(1,1)..(5,3)` at z = 4 — equal 1 m eaves offsets inside the 6×4
    /// footprint.
    fn pavilion_apexes() -> Vec<RoofApex> {
        [(1.0, 1.0), (5.0, 1.0), (5.0, 3.0), (1.0, 3.0)]
            .into_iter()
            .map(|(x, y)| RoofApex {
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
        let mut refs: Vec<&RoofApex> = apexes.iter().collect();
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
        let line: Vec<RoofApex> = [(1.0, 1.0), (2.0, 1.0), (3.0, 1.0), (4.0, 1.0)]
            .into_iter()
            .map(|(x, y)| RoofApex {
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
        let apexes: Vec<RoofApex> = [
            (1.0978, 1.1137),
            (1.4478, 1.1137),
            (1.4478, 1.3637),
            (1.0978, 1.3637),
        ]
        .into_iter()
        .map(|(x, y)| RoofApex {
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
        let apexes: Vec<RoofApex> = [(1.0, 1.0), (3.0, 1.0), (3.0, 3.0), (1.0, 3.0)]
            .into_iter()
            .map(|(x, y)| RoofApex {
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
        let apexes: Vec<RoofApex> = [(1.0, 0.0), (5.0, 0.0), (5.0, 2.0), (1.0, 2.0)]
            .into_iter()
            .map(|(x, y)| RoofApex {
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
        // the wall under them gets the gable profile from roof_profile_top.
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
        let apexes: Vec<RoofApex> = [(0.5, 0.5), (9.0, 0.5), (9.0, 3.5), (0.5, 3.5)]
            .into_iter()
            .map(|(x, y)| RoofApex {
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
            RoofApex {
                point: Point2f64::new(2.0, 0.0),
                z_m: 4.2,
            },
            RoofApex {
                point: Point2f64::new(2.0, 4.0),
                z_m: 4.2,
            },
        ];
        let roofs =
            CeilingRegions::default_for_footprint(vec![square(0.0, 0.0, 4.0, 4.0)], 2.4, cid)
                .with_apexes(&apexes);
        let ring = square(0.0, 0.0, 4.0, 4.0);
        let segments: Vec<(Point2f64, Point2f64, WallExposure)> = (0..4)
            .map(|i| (ring[i], ring[(i + 1) % 4], WallExposure::Unset))
            .collect();
        let walls = WallRegions::regenerate(
            segments,
            &WallRegions::default(),
            &roofs,
            &FloorRegions::default(),
            0.0,
            2.4,
            cid,
        );
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
            RoofApex::quantize_point(apex_p),
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
        let shaped = noisy.with_apexes(&[RoofApex {
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
        let apex = RoofApex {
            point: Point2f64::new(1.0, 1.0),
            z_m: 4.2,
        };
        ber.add_roof_apex(1, apex.clone());
        assert!(
            ber.storeys[0].roof_apexes.is_empty(),
            "other storey untouched"
        );
        assert_eq!(ber.storeys[1].roof_apexes, vec![apex.clone()]);
        // Out-of-range indices are no-ops.
        ber.add_roof_apex(7, apex.clone());
        ber.remove_roof_apex(1, 5);
        ber.remove_roof_apex(7, 0);
        assert_eq!(ber.storeys[1].roof_apexes.len(), 1);
        ber.remove_roof_apex(1, 0);
        assert!(ber.storeys[1].roof_apexes.is_empty());
    }

    #[test]
    fn storey_json_without_roof_apexes_deserializes_to_empty() {
        // JSON produced by the OLD shape (no `roof_apexes` field): serialize
        // the current shape and strip the field.
        let storey = Storey::default();
        let mut json = serde_json::to_value(&storey).expect("serialize");
        assert!(json
            .as_object_mut()
            .unwrap()
            .remove("roof_apexes")
            .is_some());
        let old: Storey = serde_json::from_value(json).expect("old JSON still loads");
        assert!(old.roof_apexes.is_empty());
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
        let roofs = stepped_l_roofs();
        // Footprint covering both arms → the LOWER level anchors.
        assert_approx(roofs.lowest_flat_z_over(&square(0.0, 0.0, 10.0, 10.0)), 2.4);
        // Footprint strictly inside the raised bottom arm only.
        assert_approx(roofs.lowest_flat_z_over(&square(1.0, 1.0, 9.0, 3.0)), 2.7);
        // Off to the side → no coverage.
        assert_eq!(
            roofs.lowest_flat_z_over(&square(20.0, 20.0, 30.0, 30.0)),
            None
        );
        // A boundary graze (sub-sliver overlap) is not coverage.
        assert_eq!(
            roofs.lowest_flat_z_over(&square(10.0, 0.0, 20.0, 4.0)),
            None
        );
        // Degenerate footprint.
        assert_eq!(roofs.lowest_flat_z_over(&[]), None);
    }

    #[test]
    fn inherited_floor_levels_pieces_at_z_minus_anchor() {
        use geo::Area;
        let roofs = stepped_l_roofs();
        // Footprint over both arms: anchor 2.4, ONE inherited level — the
        // bottom arm at 2.7 − 2.4 = 0.3 (the slab thickness cancels).
        let levels = roofs.inherited_floor_levels(&square(0.0, 0.0, 10.0, 10.0));
        assert_eq!(levels.len(), 1, "{levels:?}");
        let (ring, level) = &levels[0];
        assert!((level - 0.3).abs() < 1e-9);
        let area = ring_to_poly(ring).unsigned_area();
        assert!(
            (area - 40.0).abs() < 1e-6,
            "the whole bottom arm, got {area}"
        );
        // f32-quantized at the boundary (the add_roof_apex precedent).
        for p in ring {
            assert_eq!(p.x, p.x as f32 as f64);
            assert_eq!(p.y, p.y as f32 as f64);
        }
        // A footprint clipped to part of both arms inherits only the covered
        // piece of the raised arm: (2..10, 0..4) = 32 m².
        let clipped = roofs.inherited_floor_levels(&square(2.0, 0.0, 10.0, 10.0));
        assert_eq!(clipped.len(), 1, "{clipped:?}");
        let clipped_area = ring_to_poly(&clipped[0].0).unsigned_area();
        assert!(
            (clipped_area - 32.0).abs() < 1e-6,
            "8 × 4 piece, got {clipped_area}"
        );
    }

    #[test]
    fn inherited_floor_levels_anchor_level_and_no_anchor_yield_empty() {
        let roofs = stepped_l_roofs();
        // Footprint on the raised arm only: it IS the anchor — no step.
        assert!(roofs
            .inherited_floor_levels(&square(1.0, 1.0, 9.0, 3.0))
            .is_empty());
        // No coverage at all → no anchor → empty.
        assert!(roofs
            .inherited_floor_levels(&square(20.0, 20.0, 30.0, 30.0))
            .is_empty());
    }

    #[test]
    fn inherited_floor_levels_sloped_regions_contribute_nothing() {
        // Flat anchor on the left half, a sloped region on the right half.
        let mut roofs = CeilingRegions::default_for_footprint(
            vec![square(0.0, 0.0, 5.0, 10.0)],
            2.4,
            ItemId::default(),
        );
        roofs.regions.push(CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(5.0, 0.0, 2.4),
                Point3f64::new(10.0, 0.0, 3.4),
                Point3f64::new(10.0, 10.0, 3.4),
                Point3f64::new(5.0, 10.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
        });
        // The sloped half is covered but warn-logged out — no levels.
        assert!(roofs
            .inherited_floor_levels(&square(0.0, 0.0, 10.0, 10.0))
            .is_empty());
        // And it never anchors: only the flat half does.
        assert_eq!(
            roofs.lowest_flat_z_over(&square(0.0, 0.0, 10.0, 10.0)),
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
            roofs: stepped_l_roofs(),
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
        let out = walls.apply_splits(&[cut], &WallRegions { regions: vec![] });
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
        let out = walls.apply_splits(&[cut], &WallRegions { regions: vec![] });
        assert_eq!(out.regions.len(), 1, "untouched wall survives the miss");
    }

    #[test]
    fn apply_splits_recovers_sub_wall_ids_on_re_resolve() {
        let cid = ItemId::default();
        let cut = [Point2f64::new(1.0, -1.0), Point2f64::new(1.0, 1.0)];
        // First split mints two fresh sub-wall ids.
        let first = WallRegions {
            regions: vec![four_m_wall(cid, WallExposure::Exposed)],
        }
        .apply_splits(&[cut], &WallRegions { regions: vec![] });
        let ids_first: std::collections::HashSet<_> = first.regions.iter().map(|r| r.id).collect();
        // Re-resolve: `regenerate` re-makes the FULL wall (fresh id), then
        // apply_splits re-splits against `first` → must recover the SAME ids.
        let second = WallRegions {
            regions: vec![four_m_wall(cid, WallExposure::Exposed)],
        }
        .apply_splits(&[cut], &first);
        let ids_second: std::collections::HashSet<_> =
            second.regions.iter().map(|r| r.id).collect();
        assert_eq!(
            ids_first, ids_second,
            "sub-wall ids stable across re-resolve"
        );
    }

    #[test]
    fn apply_splits_empty_is_a_noop() {
        let cid = ItemId::default();
        let wall = four_m_wall(cid, WallExposure::Exposed);
        let id = wall.id;
        let out = WallRegions {
            regions: vec![wall],
        }
        .apply_splits(&[], &WallRegions { regions: vec![] });
        assert_eq!(out.regions.len(), 1);
        assert_eq!(out.regions[0].id, id, "empty cuts → untouched");
    }
}
