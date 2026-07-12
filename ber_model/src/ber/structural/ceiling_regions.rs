use geo::{Contains, LineString, MultiPolygon, Point as GeoPoint, Polygon};
use serde::{Deserialize, Serialize};

use crate::ber::common::ItemId;
use domain::points::{point_to_segment_distance, ring_centroid, Point2f64, Point3f64};

#[allow(unused_imports)]
use super::*;

/// A declared roof high point: fan the ceiling region containing `point` up
/// (or down) to `z_m`. Durable INPUT, like `Storey::ceiling_height_m` — a fan
/// apex is not cleanly invertible from triangulated geometry, so the
/// declarative form is stored and resolve re-applies it on every materialize
/// (composition order is law: default ceiling → step areas → apexes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CeilingApex {
    /// Plan position of the apex, in storey/world metres. Must be
    /// f32-representable (the reload reconstruction round-trips rings through
    /// f32, and the fan's exact vertex matching depends on it) —
    /// `Ber::add_ceiling_apex` quantizes, so go through it.
    pub point: Point2f64,
    /// ABSOLUTE height of the apex in metres above the storey's floor datum
    /// ("ridge at 4.2 m"), NOT a delta against the region it lands in.
    pub z_m: f64,
}

/// Disambiguates a ceiling apex that sits on an INTERNAL boundary shared by two
/// ceiling areas (a mono-pitch ridge along the house↔extension junction): which
/// area should it slope? `apex` is the apex plan point it applies to; `region`
/// is a point INSIDE the intended area (the drawn pre-snap location). Durable,
/// stored on the `Storey` parallel to (but keyed by point, not index) the
/// apexes, so `with_apexes` resolves the side deterministically every resolve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CeilingApexHint {
    pub apex: (f32, f32),
    pub region: (f32, f32),
}

impl CeilingApex {
    /// Quantize a plan position to the f32 grid — exactly the rounding
    /// [`Ber::add_ceiling_apex`] applies on store. Callers that must reason
    /// about an apex BEFORE storing it (e.g. "are these two endpoints still
    /// distinct?") use this so their answer matches the stored value.
    ///
    /// [`Ber::add_ceiling_apex`]: super::ber_struct::Ber::add_ceiling_apex
    pub fn quantize_point(p: Point2f64) -> Point2f64 {
        Point2f64::new(p.x as f32 as f64, p.y as f32 as f64)
    }
}

/// A flat-topped DORMER patch carved into an already-sloped attic ceiling: the
/// `footprint` region is set to the single flat height `top_z_m` while the
/// SURROUNDING roof keeps its slope — the dormer does NOT reslope its
/// neighbours. Its vertical walls fall out for free from
/// [`CeilingRegions::riser_faces`] once the flat patch and the sloped remainder
/// meet at different heights along the footprint boundary. Durable INPUT like
/// [`CeilingApex`]; resolve replays the list via
/// [`CeilingRegions::with_flat_patches`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Dormer {
    /// The dormer's plan footprint: a CLOSED ring in storey/world metres (no
    /// closing duplicate required). Quantized to the f32 grid by
    /// [`Ber::add_dormer`], mirroring [`CeilingApex::point`], so the derived
    /// seams line up after the reload round-trip.
    ///
    /// [`Ber::add_dormer`]: super::ber_struct::Ber::add_dormer
    pub footprint: Vec<Point2f64>,
    /// ABSOLUTE flat height of the dormer top in metres above the storey's floor
    /// datum ("dormer ceiling at 2.6 m"), NOT a delta against the roof it carves
    /// into.
    pub top_z_m: f64,
}

/// Plan tolerance (m) for claiming an apex that sits ON a region boundary.
/// Same rationale as `PROFILE_EPS_M`: derived rings are boolean-op output that
/// can sit a few 1e-7 m off the line they conceptually coincide with — a
/// gable apex stored exactly on the outline must still claim its region when
/// the derived ring is noise-displaced, or the apex warn-skips and the roof
/// flattens on re-resolve (a fixpoint break).
pub const APEX_BOUNDARY_EPS_M: f64 = 1e-4;

/// A connected GROUP of ceiling apexes and the ceiling regions they shape — the
/// session-derived view (never persisted; no serde) that is THE grouping
/// authority: ridge validation ("does this region already have a high
/// point?") and the group-aware ceiling-step reconstruction both consume it
/// rather than re-deriving their own pairing.
///
/// Built by [`CeilingRegions::apex_groups`]: an apex and a region are
/// incident when the region carries a vertex plan-matching the apex point
/// (within 1e-6 m — the [`fan_base_z`] predicate; apex coordinates are
/// copied verbatim into fan/ridge triangles, no boolean ops touch them), and
/// the incidence is closed TRANSITIVELY — a ridge's transition triangles
/// carry BOTH endpoints, linking apex A to apex B through them.
///
/// [`fan_base_z`]: CeilingRegion::fan_base_z
#[derive(Clone, Debug, PartialEq)]
pub struct CeilingApexGroup {
    /// Indices into the `apexes` slice passed to `apex_groups`, ascending.
    pub apex_idxs: Vec<usize>,
    /// Indices into [`CeilingRegions::regions`] of every member region
    /// (one carrying a vertex plan-matching one of the group's apexes),
    /// ascending. Includes base-z DISAGREEING members (membership is
    /// topological); only the merge excludes them.
    pub region_idxs: Vec<usize>,
    /// The flat base the group's roof rises from: the first member region's
    /// [`fan_base_z`] (in region order). Members whose own base disagrees
    /// (or is `None`) are warn-logged and EXCLUDED from `merged_rings`;
    /// `0.0` (warn-logged) in the degenerate case where no member has a
    /// level base.
    ///
    /// [`fan_base_z`]: CeilingRegion::fan_base_z
    pub base_z: f64,
    /// Plan union of the agreeing member regions' polygons, as SIMPLE
    /// exterior rings (open — no closing duplicate; holes decomposed away
    /// via `split_to_simple`; sub-sliver pieces dropped). For an intact
    /// fan/ridge this is one ring equal to the footprint piece the roof
    /// was raised over.
    pub merged_rings: Vec<Vec<Point2f64>>,
}

/// A planar region of the floor
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CeilingRegions {
    pub regions: Vec<CeilingRegion>, // height per vertex
}

impl CeilingRegions {
    /// Create a `CeilingRegions` with a single region referencing `construction_id`.
    pub fn new(vertices: Vec<Point3f64>, construction_id: ItemId) -> Self {
        Self {
            regions: vec![CeilingRegion {
                id: ItemId::default(),
                polygon: Polygon3D::new(vertices),
                opes: Opes::default(),
                construction_id,
                exposure: CeilingExposure::default(),
            }],
        }
    }

    /// One FLAT ceiling region per footprint ring: every vertex at z = `height_m`
    /// (metres above the storey's floor datum), all sharing `construction_id`.
    /// Rings with < 3 vertices are skipped. The default layout the assessor's
    /// typed height creates; later roof work re-splits/refines these regions.
    pub fn default_for_footprint(
        rings: Vec<Vec<Point2f64>>,
        height_m: f64,
        construction_id: ItemId,
    ) -> Self {
        let regions = rings
            .into_iter()
            .filter(|ring| ring.len() >= 3)
            .map(|ring| {
                let vertices = ring
                    .into_iter()
                    .map(|p| Point3f64::new(p.x, p.y, height_m))
                    .collect();
                CeilingRegion {
                    id: ItemId::default(),
                    polygon: Polygon3D::new(vertices),
                    opes: Opes::default(),
                    construction_id,
                    exposure: CeilingExposure::default(),
                }
            })
            .collect();
        Self { regions }
    }

    /// Re-split the regions by ceiling-step `areas` (world ring + z DELTA in
    /// metres), applied IN ORDER: each area splits every region produced so
    /// far (2D boolean ops on the plan projection); intersected pieces get
    /// every vertex's z shifted by the delta, remainders keep theirs — so
    /// overlapping areas COMPOSE (deltas sum). Empty `areas` → `self`
    /// unchanged (no geo ops). Boolean results carrying interior rings (a
    /// step strictly inside a region) are decomposed into simple pieces
    /// first — `Polygon3D` is exterior-only and would fill the hole (see
    /// `split_to_simple`). Sub-mm² slivers dropped ([`SLIVER_AREA_M2`], same
    /// rationale as `FloorRegions::with_levels`). Pieces inherit
    /// `construction_id`, get fresh ids and default `opes` (openings are
    /// dropped on split — region identity doesn't survive a re-split).
    /// Assumes FLAT input regions (all current content is — see `flat_z`);
    /// sloped regions are the later ceilings feature.
    pub fn with_z_deltas(self, areas: &[(Vec<Point2f64>, f64)]) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        if areas.is_empty() {
            return self;
        }
        let to_poly = |ring: &[Point2f64]| {
            Polygon::new(
                LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            )
        };
        let mut regions = self.regions;
        for (ring, delta) in areas {
            if ring.len() < 3 {
                continue;
            }
            let area_poly = to_poly(ring);
            let mut next: Vec<CeilingRegion> = Vec::new();
            for region in regions {
                // FLAT-region assumption: the boolean ops produce fresh 2D
                // vertices, so per-vertex z can't be carried through — stamp
                // every output vertex at the source region's single height.
                let base_z = region.polygon.exterior.first().map(|v| v.z).unwrap_or(0.0);
                let region_poly: Polygon<f64> = region.polygon.clone().into();
                let mk = |poly: &Polygon<f64>, z: f64| {
                    let mut pts: Vec<Point3f64> = poly
                        .exterior()
                        .coords()
                        .map(|c| Point3f64::new(c.x, c.y, z))
                        .collect();
                    if pts.len() > 1 && pts.first() == pts.last() {
                        pts.pop();
                    }
                    CeilingRegion {
                        id: ItemId::default(),
                        polygon: Polygon3D::new(pts),
                        opes: Opes::default(),
                        construction_id: region.construction_id,
                        exposure: region.exposure,
                    }
                };
                let piece = region_poly.intersection(&area_poly);
                for poly in piece.0.iter().flat_map(split_to_simple) {
                    if poly.unsigned_area() < SLIVER_AREA_M2 {
                        continue;
                    }
                    next.push(mk(&poly, base_z + delta));
                }
                let remainder = region_poly.difference(&area_poly);
                for poly in remainder.0.iter().flat_map(split_to_simple) {
                    if poly.unsigned_area() < SLIVER_AREA_M2 {
                        continue;
                    }
                    next.push(mk(&poly, base_z));
                }
            }
            regions = next;
        }
        Self { regions }
    }

    /// Split the regions by each of `rings` (world plan rings) WITHOUT changing
    /// the surface: every output vertex's z is read from the source region's
    /// own plane ([`region_plane`]), so a sloped fan/ridge face splits into
    /// COPLANAR sub-faces — same geometry, just more pieces to attribute. This
    /// is the Structure-phase ROOF carve, and the reason it runs AFTER
    /// [`with_apexes`](Self::with_apexes) (unlike the flat-stamping
    /// [`with_z_deltas`](Self::with_z_deltas) steps/dividers, which run BEFORE):
    /// the assessor carves an already-shaped roof into attributable pieces and
    /// the shape must NOT be re-derived — carving across a ridge into the two
    /// halves before the apexes would re-pitch each half as its own roof.
    ///
    /// A face the carve straddles splits into its inside/outside pieces; a face
    /// the carve misses (or fully covers) is kept INTACT — same id, same opes —
    /// so unrelated faces and their assigned constructions are untouched. Split
    /// pieces inherit `construction_id`, get fresh ids and default `opes`
    /// (re-homed at the resolve tail by plan point). Boolean results carrying
    /// interior rings are decomposed (`split_to_simple`) and sub-[`SLIVER_AREA_M2`]
    /// slivers dropped; rings with < 3 vertices are skipped; empty `rings` →
    /// `self`. A region degenerate in plan (no plane) passes through unsplit.
    ///
    /// [`region_plane`]: super::region_plane
    pub fn split_faces_coplanar(self, rings: &[Vec<Point2f64>]) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        if rings.is_empty() {
            return self;
        }
        let to_poly = |ring: &[Point2f64]| {
            Polygon::new(
                LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            )
        };
        let mut regions = self.regions;
        for ring in rings {
            if ring.len() < 3 {
                continue;
            }
            let area_poly = to_poly(ring);
            let mut next: Vec<CeilingRegion> = Vec::new();
            for region in regions {
                let Some((c0, cx, cy)) = region_plane(&region) else {
                    next.push(region); // plan-degenerate — nothing to split
                    continue;
                };
                let region_poly: Polygon<f64> = region.polygon.clone().into();
                let inside = region_poly.intersection(&area_poly);
                let inside_area: f64 = inside.0.iter().map(|p| p.unsigned_area()).sum();
                if inside_area < SLIVER_AREA_M2 {
                    next.push(region); // carve misses this face — keep it intact
                    continue;
                }
                let outside = region_poly.difference(&area_poly);
                let outside_area: f64 = outside.0.iter().map(|p| p.unsigned_area()).sum();
                if outside_area < SLIVER_AREA_M2 {
                    next.push(region); // carve fully covers this face — keep it intact
                    continue;
                }
                // Straddled: cut it in two, each piece coplanar with the source
                // face (z from its plane, not a flat stamp), so the surface is
                // bit-identical and only the region count grows.
                let cid = region.construction_id;
                let exposure = region.exposure;
                let mk = |poly: &Polygon<f64>| {
                    let mut pts: Vec<Point3f64> = poly
                        .exterior()
                        .coords()
                        .map(|c| Point3f64::new(c.x, c.y, c0 + cx * c.x + cy * c.y))
                        .collect();
                    if pts.len() > 1 && pts.first() == pts.last() {
                        pts.pop();
                    }
                    CeilingRegion {
                        id: ItemId::default(),
                        polygon: Polygon3D::new(pts),
                        opes: Opes::default(),
                        construction_id: cid,
                        exposure,
                    }
                };
                for poly in inside.0.iter().flat_map(split_to_simple) {
                    if poly.unsigned_area() < SLIVER_AREA_M2 {
                        continue;
                    }
                    next.push(mk(&poly));
                }
                for poly in outside.0.iter().flat_map(split_to_simple) {
                    if poly.unsigned_area() < SLIVER_AREA_M2 {
                        continue;
                    }
                    next.push(mk(&poly));
                }
            }
            regions = next;
        }
        Self { regions }
    }

    /// Carve flat-topped DORMER patches into the (possibly SLOPED) regions
    /// WITHOUT resloping their neighbours. For each `(ring, z_abs)`: 2D-boolean
    /// split every region by `ring` — the same [`geo`] `BooleanOps` +
    /// [`SLIVER_AREA_M2`] pattern as [`with_z_deltas`](Self::with_z_deltas) and
    /// [`split_faces_coplanar`](Self::split_faces_coplanar). The INSIDE piece(s)
    /// (region ∩ ring) become NEW FLAT regions with every vertex at the absolute
    /// `z_abs` (a fresh id, so [`riser_faces`](Self::riser_faces) pairs the patch
    /// against its neighbours); the OUTSIDE piece(s) (region − ring) keep the
    /// SOURCE face's plane ([`region_plane`]) — so the surrounding slope stays
    /// bit-identical to the original. A region the ring never intersects (inside
    /// area < [`SLIVER_AREA_M2`]) is returned UNTOUCHED — same id, same opes; a
    /// region the ring fully covers flattens to one `z_abs` piece (no remainder).
    ///
    /// Split pieces inherit `construction_id`/`exposure`, get fresh ids and
    /// default `opes` (mirroring `split_faces_coplanar` — region identity does
    /// not survive a split). Boolean results carrying interior rings are
    /// decomposed (`split_to_simple`) and sub-[`SLIVER_AREA_M2`] slivers dropped;
    /// rings with < 3 vertices are skipped; a plan-degenerate region (no plane)
    /// passes through unsplit; multiple patches compose in SEQUENCE; empty
    /// `patches` → `self` unchanged.
    ///
    /// Unlike the flat-input, delta-stamping `with_z_deltas`, this runs over the
    /// resolved SLOPED attic: the flat patch and the sloped remainder meet at
    /// different heights along the footprint boundary, so `riser_faces` derives
    /// the dormer's vertical walls for free — and emits NOTHING along a boundary
    /// edge where the flat top is flush with the roof.
    ///
    /// [`region_plane`]: super::region_plane
    pub fn with_flat_patches(&self, patches: &[(Vec<Point2f64>, f64)]) -> CeilingRegions {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        if patches.is_empty() {
            return self.clone();
        }
        let to_poly = |ring: &[Point2f64]| {
            Polygon::new(
                LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            )
        };
        let mut regions = self.regions.clone();
        for (ring, z_abs) in patches {
            if ring.len() < 3 {
                continue;
            }
            let area_poly = to_poly(ring);
            let mut next: Vec<CeilingRegion> = Vec::new();
            // The dormer-top (INSIDE) pieces from EVERY source region are all
            // stamped at `z_abs` — the same height — so a footprint spanning a
            // roof ridge, a ceiling step, or an area boundary is ONE flat surface,
            // not split by the seam underneath. Union them into a single top; only
            // the OUTSIDE remainders (each on its own source plane) stay separate.
            let mut inside_union: Option<geo::MultiPolygon<f64>> = None;
            let mut top: Option<(ItemId, CeilingExposure, f64)> = None; // dominant (cid, exposure, area)
            for region in regions {
                let Some((c0, cx, cy)) = region_plane(&region) else {
                    next.push(region); // plan-degenerate — nothing to split
                    continue;
                };
                let region_poly: Polygon<f64> = region.polygon.clone().into();
                let inside = region_poly.intersection(&area_poly);
                let inside_area: f64 = inside.0.iter().map(|p| p.unsigned_area()).sum();
                if inside_area < SLIVER_AREA_M2 {
                    next.push(region); // patch misses this face — keep it byte-identical
                    continue;
                }
                let cid = region.construction_id;
                let exposure = region.exposure;
                // The merged top inherits the DOMINANT source's construction /
                // exposure (largest overlap) — the dormer roof is one surface the
                // assessor assigns; dominant is the sensible default.
                if top.is_none_or(|(_, _, a)| inside_area > a) {
                    top = Some((cid, exposure, inside_area));
                }
                inside_union = Some(match inside_union {
                    Some(acc) => acc.union(&inside),
                    None => inside,
                });
                // OUTSIDE: the surrounding roof kept on the SOURCE plane
                // (z read from the plane, not a flat stamp) — bit-identical slope.
                let mk_sloped = |poly: &Polygon<f64>| {
                    let mut pts: Vec<Point3f64> = poly
                        .exterior()
                        .coords()
                        .map(|c| Point3f64::new(c.x, c.y, c0 + cx * c.x + cy * c.y))
                        .collect();
                    if pts.len() > 1 && pts.first() == pts.last() {
                        pts.pop();
                    }
                    CeilingRegion {
                        id: ItemId::default(),
                        polygon: Polygon3D::new(pts),
                        opes: Opes::default(),
                        construction_id: cid,
                        exposure,
                    }
                };
                let outside = region_poly.difference(&area_poly);
                for poly in outside.0.iter().flat_map(split_to_simple) {
                    if poly.unsigned_area() < SLIVER_AREA_M2 {
                        continue;
                    }
                    next.push(mk_sloped(&poly));
                }
            }
            // Emit the merged flat top: one region per connected component of the
            // union, every vertex stamped at `z_abs`.
            if let (Some(union), Some((cid, exposure, _))) = (inside_union, top) {
                for poly in union.0.iter().flat_map(split_to_simple) {
                    if poly.unsigned_area() < SLIVER_AREA_M2 {
                        continue;
                    }
                    let mut pts: Vec<Point3f64> = poly
                        .exterior()
                        .coords()
                        .map(|c| Point3f64::new(c.x, c.y, *z_abs))
                        .collect();
                    if pts.len() > 1 && pts.first() == pts.last() {
                        pts.pop();
                    }
                    next.push(CeilingRegion {
                        id: ItemId::default(),
                        polygon: Polygon3D::new(pts),
                        opes: Opes::default(),
                        construction_id: cid,
                        exposure,
                    });
                }
            }
            regions = next;
        }
        CeilingRegions { regions }
    }

    /// Shape the regions by roof `apexes` in ONE pass (the last step of the
    /// composition: default ceiling → step areas → apexes), assigning each
    /// apex to its FIRST claiming region ([`claim_regions`]: strict-inside,
    /// else the [`APEX_BOUNDARY_EPS_M`] boundary band — boundary-INCLUSIVE,
    /// so a re-applied apex that is a CORNER of its own fan/ridge triangles,
    /// or a GABLE apex stored exactly on the outline, still finds a region
    /// and re-application stays the geometric identity; assign-time
    /// validation deliberately uses the strict predicate instead —
    /// [`region_strictly_containing`]; two different predicates by design).
    /// Per region:
    ///
    /// [`claim_regions`]: CeilingRegions::claim_regions
    ///
    /// - **0** contained apexes → kept untouched;
    /// - **1** → a FAN: one triangle per boundary edge — the edge keeps its
    ///   per-vertex z (no flat-input stamp like `with_z_deltas`, so fans
    ///   compose over stepped — and, structurally, already-shaped — pieces),
    ///   the apex sits at its `z_m`;
    /// - **2** → a RIDGE: boundary edges are first SPLIT where they cross
    ///   the ridge MIDLINE (`t = 0.5`), then each (sub-)edge rises to its
    ///   nearer apex (HUB triangles; nearness by ridge-axis midpoint
    ///   projection) plus one TRANSITION triangle `(v, A, B)` per hub
    ///   switch — see `ridge_triangles`;
    /// - **4 forming an axis-aligned RECT** (one apex per corner, shared z
    ///   — [`apexes_form_rect`]) → the flat-topped HIP (pavilion): the
    ///   pavilion loft [`rect_ridge_faces`] — each same-side run of
    ///   boundary edges lofts to the rect side it faces as ONE face (no
    ///   inserted vertices), plus the flat TOP. A ring whose sector walk
    ///   isn't the clean 4 switches ([`rect_hip_switches`]) warns and
    ///   proceeds;
    /// - **≥3** (and a 4 that is no rect) → warn + ridge from the FIRST
    ///   TWO in apex-list order (deterministic, never a panic — resolve
    ///   must always produce geometry; assign-time validation rejects this
    ///   up front).
    ///
    /// Both branches PRE-LIFT first (D2 step 0): ring vertices plan-matching
    /// an applied apex (1e-6 m) take that apex's z — load-bearing for the
    /// re-apply-to-self identity. Sub-mm² (plan) sliver triangles are
    /// dropped ([`SLIVER_AREA_M2`]) — exactly the degenerate pieces incident
    /// to an apex that is already a corner. Triangles are SIMPLE by
    /// construction (three exterior vertices, no interior rings), so unlike
    /// `with_z_deltas` no hole decomposition (`split_to_simple`) is needed
    /// here. An apex contained by NO region is warn-logged and skipped
    /// (never rejected — the assessor may be mid-edit). Pieces inherit
    /// `construction_id`, get fresh ids and default `opes`. Empty `apexes`
    /// → `self` unchanged.
    ///
    /// Each shaped region's output finally runs through the COPLANAR MERGE
    /// (`merge_coplanar_faces`): coplanar triangle neighbours collapse into
    /// single planar polygons, so a centred ridge over a rectangle comes out
    /// as the classic 4 faces — two trapezoids and two end triangles — not
    /// 8 triangles. Re-applying the apexes to the merged faces reproduces
    /// them (the re-apply identity now holds on the MERGED output: the
    /// triangulation re-fragments each face and the merge folds it back).
    ///
    /// [`region_strictly_containing`]: CeilingRegions::region_strictly_containing
    pub fn with_apexes(self, apexes: &[CeilingApex]) -> Self {
        self.with_apexes_hinted(apexes, &[])
    }

    /// Like [`with_apexes`](Self::with_apexes), but `hints` resolve a MULTI-claim
    /// apex (one on an internal boundary shared by two areas — a mono-pitch ridge
    /// along the junction) to the area whose interior contains the hint's
    /// `region` point, instead of the first claimant. An apex with no matching
    /// hint, or whose hint lands in no claimed area, keeps first-claim (so normal
    /// apexes and old records are byte-identical).
    pub fn with_apexes_hinted(self, apexes: &[CeilingApex], hints: &[CeilingApexHint]) -> Self {
        self.with_apexes_hinted_unfanned(apexes, hints, &[])
    }

    /// Like [`with_apexes_hinted`](Self::with_apexes_hinted), plus the unfan
    /// machinery: `unfanned` (the storey's durable `ceiling_unfanned` set,
    /// as plan points) holds the assessor's MANUAL fan toggles.
    ///
    /// - A TRUE-GABLE ridge (both endpoints on its region's boundary — the
    ///   assign-time gable-end snap) solves each side as ONE PLANE through
    ///   the ridge and the side's nearest eave corner; every other vertex
    ///   (bay/notch corners, gable-wall extras) EMBEDS on that plane, so the
    ///   roof is two planes with zero taps (`gable_plan_classify` +
    ///   `embed_gable_sides` in `ceiling_geom` — the normative docs). The
    ///   stored set TOGGLES against that auto baseline: re-fan an embedded
    ///   corner (pin it at its own z) or unfan an anchor (pitch to the next
    ///   wall).
    /// - Any other fan/ridge: only stored-set vertices stop being fan
    ///   targets, each embedded on the plane of the EFFECTIVE solve's face
    ///   across it (`lift_unfanned_fan` / `lift_unfanned_hip_ridge`), so the
    ///   coplanar merge folds its facets into the neighbours' plane.
    ///
    /// Empty `unfanned` + non-gable ridges (every old record before the
    /// gable-end snap existed) are byte-identical to before. The 4-corner
    /// RECT (pavilion) branch has no per-vertex fan and ignores the set.
    pub fn with_apexes_hinted_unfanned(
        self,
        apexes: &[CeilingApex],
        hints: &[CeilingApexHint],
        unfanned: &[Point2f64],
    ) -> Self {
        if apexes.is_empty() {
            return self;
        }
        // One claim pass per apex ([`claim_regions`]): the FIRST claimant in
        // region order wins — deterministic per resolve, since boolean ops
        // emit regions in a stable order. Strict-inside apexes claim exactly
        // the region the old boundary-inclusive collect gave them; a
        // boundary apex (a GABLE end, or a re-applied apex that is a corner
        // of its own triangles) claims its first incident region — re-fanning
        // / re-ridging a planar face to its own corners is the identity after
        // the coplanar merge, so first-claim preserves the re-apply identity.
        // Multi-claim (an apex on an INTERNAL shared boundary — legacy or
        // hand-edited input; assign-time validation rejects creating one) is
        // warn-and-proceed, the ≥3-apex posture.
        let mut per_region: Vec<Vec<usize>> = vec![Vec::new(); self.regions.len()];
        for (i, apex) in apexes.iter().enumerate() {
            let claims = self.claim_regions(apex.point);
            match claims.split_first() {
                None => log::warn!(
                    "ceiling apex at ({:.3}, {:.3}) lies in no ceiling region — skipped",
                    apex.point.x,
                    apex.point.y
                ),
                Some((&first, rest)) => {
                    let chosen = if rest.is_empty() {
                        first
                    } else {
                        // Multi-claim (internal shared boundary): a side hint for
                        // this apex picks the area whose interior it names; else
                        // first-claim (warn), as before.
                        let hinted = hints
                            .iter()
                            .find(|h| {
                                (h.apex.0 as f64 - apex.point.x).abs() < 1e-3
                                    && (h.apex.1 as f64 - apex.point.y).abs() < 1e-3
                            })
                            .and_then(|h| {
                                let rp = GeoPoint::new(h.region.0 as f64, h.region.1 as f64);
                                claims.iter().copied().find(|&ri| {
                                    Polygon::<f64>::from(self.regions[ri].polygon.clone())
                                        .contains(&rp)
                                })
                            });
                        match hinted {
                            Some(ri) => ri,
                            None => {
                                log::warn!(
                                    "ceiling apex at ({:.3}, {:.3}) sits on a boundary shared by {} \
                                     regions — claimed by the first (#{first})",
                                    apex.point.x,
                                    apex.point.y,
                                    claims.len()
                                );
                                first
                            }
                        }
                    };
                    per_region[chosen].push(i);
                }
            }
        }
        let mut next: Vec<CeilingRegion> = Vec::new();
        for (ri, region) in self.regions.into_iter().enumerate() {
            if region.polygon.exterior.len() < 3 {
                next.push(region);
                continue;
            }
            let contained = &per_region[ri];
            match contained.len() {
                0 => next.push(region),
                1 => next.extend(merge_coplanar_faces(fan_triangles(
                    region,
                    &apexes[contained[0]],
                    unfanned,
                ))),
                n => {
                    // 4 contained apexes forming an axis-aligned RECT with
                    // one shared z → the flat-topped hip (pavilion),
                    // [`rect_ridge_faces`]. A ring whose sector walk isn't
                    // the clean 4 switches (assign-time validation rejects
                    // creating one) warns and proceeds: the loft may fold
                    // but resolve always produces geometry.
                    if n == 4 {
                        let quad = [
                            &apexes[contained[0]],
                            &apexes[contained[1]],
                            &apexes[contained[2]],
                            &apexes[contained[3]],
                        ];
                        if let Some((lo, hi)) = apexes_form_rect(&quad) {
                            let ring: Vec<Point2f64> = region
                                .polygon
                                .exterior
                                .iter()
                                .map(|v| Point2f64::new(v.x, v.y))
                                .collect();
                            let switches = rect_hip_switches(&ring, lo, hi);
                            if switches != 4 {
                                log::warn!(
                                    "rect ridge over a ring with an unclean sector walk \
                                     ({switches} switches ≠ 4) — the loft may fold; \
                                     proceeding"
                                );
                            }
                            next.extend(merge_coplanar_faces(rect_ridge_faces(region, quad)));
                            continue;
                        }
                    }
                    if n >= 3 {
                        log::warn!(
                            "ceiling region contains {n} ceiling apexes — \
                             ridging from the first two in apex-list order"
                        );
                    }
                    next.extend(merge_coplanar_faces(ridge_triangles(
                        region,
                        &apexes[contained[0]],
                        &apexes[contained[1]],
                        unfanned,
                    )));
                }
            }
        }
        Self { regions: next }
    }

    /// Group `apexes` with the regions they shape — THE grouping authority
    /// (see [`CeilingApexGroup`]). Incidence is quantized plan-corner identity
    /// (a region vertex within 1e-6 m of the apex point — the
    /// [`fan_base_z`] predicate), closed transitively over shared regions
    /// (a ridge's transition triangles carry both endpoints, linking A to
    /// B). Apexes incident to no region join no group (e.g. mid-edit, or a
    /// dormant apex outside every ring). Groups come out ordered by their
    /// smallest apex index; `base_z` and the warn-excluded merge are per
    /// the [`CeilingApexGroup`] field docs.
    ///
    /// [`fan_base_z`]: CeilingRegion::fan_base_z
    pub fn apex_groups(&self, apexes: &[CeilingApex]) -> Vec<CeilingApexGroup> {
        self.apex_groups_unfanned(apexes, &[])
    }

    /// Like [`apex_groups`](Self::apex_groups), plus the storey's UNFANNED
    /// vertex set: a face carrying a LIFTED (unfanned) vertex is not level
    /// off its apex corners, so the plain [`fan_base_z`] would warn-exclude
    /// it from `base_z`/`merged_rings` on every call (log spam, and the
    /// merged ring — the marker/validation footprint — would lose exactly
    /// the unfanned vertex the assessor needs to tap again to RE-fan).
    /// Passing the set makes the base check drop those vertices too
    /// ([`fan_base_z_excluding`]). Callers with a `Storey` in hand should
    /// prefer `Storey::ceiling_apex_groups`, which supplies the set.
    ///
    /// [`fan_base_z`]: CeilingRegion::fan_base_z
    /// [`fan_base_z_excluding`]: CeilingRegion::fan_base_z_excluding
    pub fn apex_groups_unfanned(
        &self,
        apexes: &[CeilingApex],
        unfanned: &[Point2f64],
    ) -> Vec<CeilingApexGroup> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        const EPS_M: f64 = 1e-6;
        if apexes.is_empty() {
            return Vec::new();
        }
        // Whether a region is actually SHAPED by the apexes — matching what
        // [`with_apexes_hinted`] produced — rather than merely sharing a plan
        // CORNER with one. A region is shaped if it is sloped (a real fan/ridge
        // face — `flat_z` is None) OR it is an all-apex flat TOP (every vertex
        // is an apex corner: a hipped/rect roof's level top). A region that is
        // flat AND keeps non-apex corners only TOUCHES an apex at a shared
        // boundary corner (the flat neighbour of a junction mono-pitch ridge) —
        // it is NOT shaped, so it must not be claimed/merged by the group. (All
        // callers run on RESOLVED ceilings, so `flat_z` reflects the real slope.)
        let is_shaped = |r: &CeilingRegion| -> bool {
            if r.flat_z().is_none() {
                return true;
            }
            r.polygon.exterior.len() >= 3
                && r.polygon.exterior.iter().all(|v| {
                    apexes.iter().any(|a| {
                        (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M
                    })
                })
        };
        // Region → incident apex indices, by quantized plan-corner identity —
        // but only for regions the apexes actually shape (above).
        let matched: Vec<Vec<usize>> = self
            .regions
            .iter()
            .map(|r| {
                if !is_shaped(r) {
                    return Vec::new();
                }
                apexes
                    .iter()
                    .enumerate()
                    .filter(|(_, a)| {
                        r.polygon.exterior.iter().any(|v| {
                            (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M
                        })
                    })
                    .map(|(i, _)| i)
                    .collect()
            })
            .collect();
        // Transitive closure: union-find over apex indices, linked whenever
        // one region is incident to both.
        let mut parent: Vec<usize> = (0..apexes.len()).collect();
        fn find(parent: &mut [usize], mut i: usize) -> usize {
            while parent[i] != i {
                parent[i] = parent[parent[i]]; // path halving
                i = parent[i];
            }
            i
        }
        for m in &matched {
            for w in m.windows(2) {
                let (ra, rb) = (find(&mut parent, w[0]), find(&mut parent, w[1]));
                parent[ra.max(rb)] = ra.min(rb);
            }
        }
        // Collect components: apex members (only incident apexes), then
        // region members in region order.
        let mut groups: Vec<(usize, Vec<usize>, Vec<usize>)> = Vec::new();
        let group_of = |root: usize, groups: &mut Vec<(usize, Vec<usize>, Vec<usize>)>| {
            if let Some(g) = groups.iter().position(|(r, _, _)| *r == root) {
                g
            } else {
                groups.push((root, Vec::new(), Vec::new()));
                groups.len() - 1
            }
        };
        for ai in 0..apexes.len() {
            if matched.iter().any(|m| m.contains(&ai)) {
                let root = find(&mut parent, ai);
                let g = group_of(root, &mut groups);
                groups[g].1.push(ai);
            }
        }
        for (ri, m) in matched.iter().enumerate() {
            if let Some(&a0) = m.first() {
                let root = find(&mut parent, a0);
                let g = group_of(root, &mut groups);
                groups[g].2.push(ri);
            }
        }
        groups.sort_by_key(|(_, apex_idxs, _)| apex_idxs[0]);
        groups
            .into_iter()
            .map(|(_, apex_idxs, region_idxs)| {
                // base_z from the members, warn-excluding disagreement.
                let mut base_z: Option<f64> = None;
                let mut merge: Vec<&CeilingRegion> = Vec::new();
                // For a 2-apex RIDGE group, each member face may be a TRUE
                // GABLE slope (both apexes on its own ring) carrying
                // AUTO-EMBEDDED eave vertices (`embed_gable_sides`) — the
                // base recovery must skip those exactly like the stored set,
                // or a gable with bays on BOTH sides has no voting member at
                // all (base_z would collapse to 0 with a warn on every call).
                let gable_pair = (apex_idxs.len() == 2)
                    .then(|| (apexes[apex_idxs[0]].point, apexes[apex_idxs[1]].point));
                for &ri in &region_idxs {
                    let r = &self.regions[ri];
                    let mut excluded: Vec<Point2f64> = unfanned.to_vec();
                    if let Some((pa, pb)) = gable_pair {
                        let plan: Vec<Point2f64> = r
                            .polygon
                            .exterior
                            .iter()
                            .map(|v| Point2f64::new(v.x, v.y))
                            .collect();
                        if let Some(cls) = gable_plan_classify(&plan, pa, pb, unfanned) {
                            excluded.extend(
                                plan.iter()
                                    .zip(&cls.fanned)
                                    .filter(|(_, fanned)| !**fanned)
                                    .map(|(p, _)| *p),
                            );
                        }
                    }
                    // A member whose EVERY vertex is apex-matching (the
                    // rect ridge's flat TOP) has no base of its own: it
                    // joins the merge WITHOUT voting on base_z — excluding
                    // it would punch a hole in the reconstructed step area
                    // and revert the under-top base on re-resolve. Regions
                    // with a DISAGREEING or genuinely irregular base stay
                    // warn-excluded below.
                    let all_apex = r.polygon.exterior.len() >= 3
                        && r.polygon.exterior.iter().all(|v| {
                            apexes.iter().any(|a| {
                                (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M
                            })
                        });
                    if all_apex {
                        merge.push(r);
                        continue;
                    }
                    let planar = face_plane(&r.polygon.exterior)
                        .map(|(nrm, d)| {
                            r.polygon.exterior.iter().all(|v| {
                                (nrm.x * v.x + nrm.y * v.y + nrm.z * v.z - d).abs() <= EPS_M
                            })
                        })
                        .unwrap_or(false);
                    match (r.fan_base_z_excluding(apexes, &excluded), base_z) {
                        (Some(z), None) => {
                            base_z = Some(z);
                            merge.push(r);
                        }
                        (Some(z), Some(b)) if (z - b).abs() <= EPS_M => merge.push(r),
                        // A PLANAR slope face with no recoverable base: a
                        // gable side whose eave vertices are EMBEDDED
                        // off-base (auto-unfanned bay/notch corners —
                        // `embed_gable_sides`). A genuine roof member (it
                        // carries an apex corner, or it wouldn't be in this
                        // group): keep it in the merged ring — the embedded
                        // corners must stay tappable as markers — while the
                        // level-based members vote base_z.
                        (None, _) if planar => merge.push(r),
                        (got, _) => log::warn!(
                            "ceiling apex group: region base z {got:?} disagrees with the \
                             group's base {base_z:?} — excluded from the merged ring"
                        ),
                    }
                }
                if base_z.is_none() {
                    log::warn!("ceiling apex group has no level-based member region — base_z = 0");
                }
                // Plan union of the agreeing members → simple exterior rings.
                let mut acc = MultiPolygon::<f64>(Vec::new());
                for r in &merge {
                    acc = acc.union(&MultiPolygon(vec![r.polygon.clone().into()]));
                }
                let merged_rings = acc
                    .0
                    .iter()
                    .flat_map(split_to_simple)
                    .filter(|p| p.unsigned_area() >= SLIVER_AREA_M2)
                    .map(|p| {
                        let mut pts: Vec<Point2f64> = p
                            .exterior()
                            .coords()
                            .map(|c| Point2f64::new(c.x, c.y))
                            .collect();
                        if pts.len() > 1 && pts.first() == pts.last() {
                            pts.pop();
                        }
                        pts
                    })
                    .filter(|ring| ring.len() >= 3)
                    .collect();
                CeilingApexGroup {
                    apex_idxs,
                    region_idxs,
                    base_z: base_z.unwrap_or(0.0),
                    merged_rings,
                }
            })
            .collect()
    }

    /// Re-fit the regions to a changed outline `footprint` (the regions-as-truth
    /// answer to an outline edit, kept deliberately simple since the outline is
    /// frozen before ceiling work in practice). Every stored region is
    /// COPLANAR-clipped to the footprint (its plan ∩ footprint, z read from the
    /// region's own plane — a sloped face stays sloped where it survives), and
    /// any newly-EXPOSED area (the outline grew, or a region clipped away) is
    /// filled with flat region(s) at `default_height` referencing `fill_cid`. A
    /// step or ridge near a moved boundary may end short / clip away — the
    /// assessor touches it up (the accepted lossy trade). Sub-[`SLIVER_AREA_M2`]
    /// pieces dropped; an empty footprint → empty regions.
    pub fn refit_to_footprint(
        self,
        footprint: &[Vec<Point2f64>],
        default_height: f64,
        fill_cid: ItemId,
    ) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let fp_polys: Vec<Polygon<f64>> = footprint
            .iter()
            .filter(|r| r.len() >= 3)
            .map(|r| ring_to_poly(r))
            .collect();
        if fp_polys.is_empty() {
            return Self::default();
        }
        let fp = MultiPolygon(fp_polys);
        let mk = |poly: &Polygon<f64>,
                  z: Box<dyn Fn(f64, f64) -> f64>,
                  cid: ItemId,
                  exposure: CeilingExposure| {
            let mut pts: Vec<Point3f64> = poly
                .exterior()
                .coords()
                .map(|c| Point3f64::new(c.x, c.y, z(c.x, c.y)))
                .collect();
            if pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            CeilingRegion {
                id: ItemId::default(),
                polygon: Polygon3D::new(pts),
                opes: Opes::default(),
                construction_id: cid,
                exposure,
            }
        };
        let mut next: Vec<CeilingRegion> = Vec::new();
        let mut covered = MultiPolygon::<f64>(Vec::new());
        for region in &self.regions {
            let Some((c0, cx, cy)) = region_plane(region) else {
                continue;
            };
            let region_poly: Polygon<f64> = region.polygon.clone().into();
            let clipped = region_poly.intersection(&fp);
            for poly in clipped.0.iter().flat_map(split_to_simple) {
                if poly.unsigned_area() < SLIVER_AREA_M2 {
                    continue;
                }
                next.push(mk(
                    &poly,
                    Box::new(move |x, y| c0 + cx * x + cy * y),
                    region.construction_id,
                    region.exposure,
                ));
                covered = covered.union(&MultiPolygon(vec![poly]));
            }
        }
        // Newly-exposed footprint (grew, or a region clipped away) → flat default.
        for poly in fp.difference(&covered).0.iter().flat_map(split_to_simple) {
            if poly.unsigned_area() < SLIVER_AREA_M2 {
                continue;
            }
            next.push(mk(
                &poly,
                Box::new(move |_, _| default_height),
                fill_cid,
                CeilingExposure::default(),
            ));
        }
        Self { regions: next }
    }

    /// The LOWEST flat ceiling height over `footprint`: the minimum
    /// [`flat_z`] among regions whose plan polygon overlaps `footprint` by
    /// more than [`SLIVER_AREA_M2`] (a boundary graze is not coverage).
    /// Heights are metres above this storey's floor datum. This is the
    /// ANCHOR the storey above's floor slab sits on
    /// (`Ber::storey_floor_datums`) and the zero its inherited floor steps
    /// are measured from ([`inherited_floor_levels`]). `None` when no flat
    /// region overlaps — sloped-only coverage, empty ceilings, or a degenerate
    /// footprint (< 3 vertices).
    ///
    /// [`flat_z`]: CeilingRegion::flat_z
    /// [`inherited_floor_levels`]: CeilingRegions::inherited_floor_levels
    pub(crate) fn lowest_flat_z_over(&self, footprint: &[Point2f64]) -> Option<f64> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        if footprint.len() < 3 {
            return None;
        }
        let fp = ring_to_poly(footprint);
        self.regions
            .iter()
            .filter_map(|r| {
                let z = r.flat_z()?;
                let plan: Polygon<f64> = r.polygon.clone().into();
                (plan.intersection(&fp).unsigned_area() > SLIVER_AREA_M2).then_some(z)
            })
            .min_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// Absolute-z of the ceiling SURFACE over plan point `p` (plane-eval on the
    /// containing region — fan/ridge faces evaluate their sloped plane, flat
    /// pieces their constant height), in the storey-LOCAL frame — the caller
    /// adds the storey's floor datum for world z. Containment is
    /// boundary-inclusive (`Intersects`), first region in region order wins on
    /// a shared boundary — callers sample points just INSIDE the footprint,
    /// never on a step line. `None` when no region covers `p` (or the covering
    /// region is plan-degenerate). The point-query cousin of
    /// [`lowest_flat_z_over`](Self::lowest_flat_z_over), which is flat-only and
    /// area-based.
    pub fn top_z_at(&self, p: Point2f64) -> Option<f64> {
        use geo::Intersects;
        let gp = GeoPoint::new(p.x, p.y);
        self.regions.iter().find_map(|r| {
            Polygon::<f64>::from(r.polygon.clone())
                .intersects(&gp)
                .then(|| region_plane(r).map(|(c0, cx, cy)| c0 + cx * p.x + cy * p.y))
                .flatten()
        })
    }

    /// How much of `footprint` the roof covers, and the roof's MAXIMUM height
    /// over that coverage — the two facts the dormer validator needs, computed
    /// so the roof height at a region-boundary seam is captured from BOTH sides.
    ///
    /// Returns `(covered_area, roof_max_z)`: `covered_area` (m²) is the summed
    /// plan area of `footprint ∩` each region (region interiors are disjoint, so
    /// the sum is the covered fraction — a dormer may legitimately SPAN several
    /// ceiling areas); `roof_max_z` is the largest ceiling-surface height over
    /// the covered area, or `None` when the footprint intersects nothing.
    ///
    /// The height is taken from EACH region's OWN affine plane
    /// ([`region_plane`]: `z = c0 + cx·x + cy·y`) evaluated at every vertex of
    /// THAT region's intersection piece — the footprint corners inside the
    /// region, the region corners inside the footprint (a ridge/apex passing
    /// through the interior), and the boundary crossings. Because each region
    /// contributes the max of its own plane over its own piece, a DISCONTINUOUS
    /// roof step at a shared seam is captured from both sides: the higher region
    /// wins there regardless of region order — unlike [`top_z_at`], whose
    /// first-region-wins tie-break is order-dependent ON a seam.
    ///
    /// The intersection / `split_to_simple` / MultiPolygon handling mirrors
    /// [`with_flat_patches`](Self::with_flat_patches): MultiPolygon results are
    /// decomposed to simple pieces and sub-[`SLIVER_AREA_M2`] slivers are
    /// dropped. A plan-degenerate region (no plane) and a footprint of < 3
    /// vertices contribute nothing.
    ///
    /// [`region_plane`]: super::region_plane
    /// [`top_z_at`]: Self::top_z_at
    pub fn footprint_coverage_and_roof_max(&self, footprint: &[Point2f64]) -> (f64, Option<f64>) {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        if footprint.len() < 3 {
            return (0.0, None);
        }
        let fp_poly = ring_to_poly(footprint);
        let mut covered_area = 0.0_f64;
        let mut roof_max: Option<f64> = None;
        for region in &self.regions {
            let Some((c0, cx, cy)) = region_plane(region) else {
                continue; // plan-degenerate — no plane to sample
            };
            let region_poly: Polygon<f64> = region.polygon.clone().into();
            let inter = region_poly.intersection(&fp_poly);
            for poly in inter.0.iter().flat_map(split_to_simple) {
                let a = poly.unsigned_area();
                if a < SLIVER_AREA_M2 {
                    continue; // graze / no real overlap
                }
                covered_area += a;
                for c in poly.exterior().coords() {
                    let z = c0 + cx * c.x + cy * c.y;
                    roof_max = Some(roof_max.map_or(z, |m: f64| m.max(z)));
                }
            }
        }
        (covered_area, roof_max)
    }

    /// The floor levels a storey ABOVE inherits from this (below-storey)
    /// ceiling: for every FLAT region overlapping `footprint` (by more than
    /// [`SLIVER_AREA_M2`]) whose height sits more than [`RISER_Z_EPS_M`]
    /// above the anchor ([`lowest_flat_z_over`]), the plan pieces of
    /// region ∩ footprint at level `z − anchor` — the anchor (and with it
    /// the floor-slab thickness) cancels, so levels are
    /// thickness-independent. Boolean results carrying interior rings are
    /// decomposed via `split_to_simple` (`Polygon3D`-style exterior-only
    /// consumers would fill the hole) and sub-[`SLIVER_AREA_M2`] pieces are
    /// dropped. Piece rings are OPEN (no closing duplicate) and
    /// f32-QUANTIZED (the `Ber::add_ceiling_apex` precedent: the document
    /// layer round-trips step rings through `Point2f32`, and the
    /// seed → reload fixpoint needs the stored ring to survive that
    /// round-trip). Sloped regions overlapping the footprint are
    /// warn-logged and contribute nothing (sloped-under-floor is later
    /// ceilings work). Empty when there is no anchor.
    ///
    /// [`lowest_flat_z_over`]: CeilingRegions::lowest_flat_z_over
    pub fn inherited_floor_levels(&self, footprint: &[Point2f64]) -> Vec<(Vec<Point2f64>, f64)> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let Some(anchor) = self.lowest_flat_z_over(footprint) else {
            return Vec::new();
        };
        let fp = ring_to_poly(footprint);
        let mut out = Vec::new();
        for r in &self.regions {
            let plan: Polygon<f64> = r.polygon.clone().into();
            let covered = plan.intersection(&fp);
            if covered.unsigned_area() <= SLIVER_AREA_M2 {
                continue; // not under the new storey
            }
            let Some(z) = r.flat_z() else {
                log::warn!(
                    "sloped ceiling region under the new storey's footprint — \
                     it contributes no inherited floor level"
                );
                continue;
            };
            if z - anchor <= RISER_Z_EPS_M {
                continue; // the anchor level itself — not a step
            }
            for poly in covered.0.iter().flat_map(split_to_simple) {
                if poly.unsigned_area() < SLIVER_AREA_M2 {
                    continue;
                }
                let mut ring: Vec<Point2f64> = poly
                    .exterior()
                    .coords()
                    .map(|c| Point2f64::new(c.x as f32 as f64, c.y as f32 as f64))
                    .collect();
                if ring.len() > 1 && ring.first() == ring.last() {
                    ring.pop();
                }
                ring.dedup(); // quantization can collapse near-identical vertices
                if ring.len() > 1 && ring.first() == ring.last() {
                    ring.pop();
                }
                if ring.len() >= 3 {
                    out.push((ring, z - anchor));
                }
            }
        }
        out
    }

    /// The region whose plan projection STRICTLY contains `p` (boundary
    /// EXCLUDED — `geo` `Contains`). The assign-time validation predicate for
    /// a new apex: an apex on a region boundary (or the outline) is rejected
    /// up front, while [`with_apexes`](CeilingRegions::with_apexes) uses the
    /// boundary-INCLUSIVE predicate so a persisted apex still finds its own
    /// fan pieces on re-application — two deliberately different predicates.
    /// `None` if `p` is strictly inside no region.
    pub fn region_strictly_containing(&self, p: Point2f64) -> Option<&CeilingRegion> {
        let gp = GeoPoint::new(p.x, p.y);
        self.regions
            .iter()
            .find(|r| Polygon::<f64>::from(r.polygon.clone()).contains(&gp))
    }

    /// Indices of the regions that CLAIM plan point `p`: the region strictly
    /// containing it (at most one — interiors are disjoint), else every
    /// region whose plan boundary passes within [`APEX_BOUNDARY_EPS_M`] of
    /// it (ascending index order — deterministic, since resolve emits the
    /// regions in a stable order). Empty when `p` is inside nothing and near
    /// no boundary. THE apex-claim predicate:
    /// [`with_apexes`](CeilingRegions::with_apexes) takes the FIRST claimant
    /// (warn when > 1); assign-time validation REQUIRES exactly one (an apex
    /// on an internal shared boundary is rejected).
    pub fn claim_regions(&self, p: Point2f64) -> Vec<usize> {
        let gp = GeoPoint::new(p.x, p.y);
        if let Some(i) = self
            .regions
            .iter()
            .position(|r| Polygon::<f64>::from(r.polygon.clone()).contains(&gp))
        {
            return vec![i];
        }
        self.regions
            .iter()
            .enumerate()
            .filter(|(_, r)| r.plan_contains_within(p, APEX_BOUNDARY_EPS_M))
            .map(|(i, _)| i)
            .collect()
    }

    /// Interior CEILING-RISER faces: for every plan edge piece shared by two
    /// regions ([`shared_plan_pieces`] — metric collinearity over boolean-op
    /// output, never exact vertex equality) whose (linear) z profiles differ
    /// by more than [`RISER_Z_EPS_M`] somewhere along it, the vertical face
    /// between them: a quad with `exterior[0]`/`[1]` the base edge along the
    /// LOWER profile (the `WallRegion` vertex-order contract), split into
    /// two triangles where the profiles CROSS inside the piece (sub-sliver
    /// halves dropped). The predicate is Z-DISCONTINUITY, not adjacency:
    /// continuous fan/ridge seams — equal profiles along the shared edge —
    /// emit NOTHING. Outline edges never emit (they border one region). A
    /// face that comes out a coplanar FLUSH continuation of a wall region
    /// does not survive as its own region: `WallRegions::with_risers`
    /// merges it into that wall (`merge_coplanar_flush`).
    pub fn riser_faces(&self) -> Vec<Polygon3D> {
        let plan_ring = |r: &CeilingRegion| -> Vec<Point2f64> {
            r.polygon
                .exterior
                .iter()
                .map(|v| Point2f64::new(v.x, v.y))
                .collect()
        };
        let rings: Vec<Vec<Point2f64>> = self.regions.iter().map(plan_ring).collect();
        let mut out = Vec::new();
        for i in 0..self.regions.len() {
            for j in (i + 1)..self.regions.len() {
                let (Some((ci, cxi, cyi)), Some((cj, cxj, cyj))) = (
                    region_plane(&self.regions[i]),
                    region_plane(&self.regions[j]),
                ) else {
                    continue; // plan-degenerate region — no profile to compare
                };
                let z_i = |p: Point2f64| ci + cxi * p.x + cyi * p.y;
                let z_j = |p: Point2f64| cj + cxj * p.x + cyj * p.y;
                for (p, q) in shared_plan_pieces(&rings[i], &rings[j]) {
                    let (dp, dq) = (z_i(p) - z_j(p), z_i(q) - z_j(q));
                    if dp.abs() <= RISER_Z_EPS_M && dq.abs() <= RISER_Z_EPS_M {
                        continue; // continuous seam (fan/ridge) — no face
                    }
                    if dp * dq < 0.0 && dp.abs() > RISER_Z_EPS_M && dq.abs() > RISER_Z_EPS_M {
                        // Profiles cross inside the piece: one triangle per
                        // side, each closing to zero gap at the crossing.
                        let t = dp / (dp - dq);
                        let x = Point2f64::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t);
                        let zx = z_i(x);
                        out.extend(riser_face(
                            p,
                            x,
                            z_i(p).min(z_j(p)),
                            z_i(p).max(z_j(p)),
                            zx,
                            zx,
                        ));
                        out.extend(riser_face(
                            x,
                            q,
                            zx,
                            zx,
                            z_i(q).min(z_j(q)),
                            z_i(q).max(z_j(q)),
                        ));
                    } else {
                        out.extend(riser_face(
                            p,
                            q,
                            z_i(p).min(z_j(p)),
                            z_i(p).max(z_j(p)),
                            z_i(q).min(z_j(q)),
                            z_i(q).max(z_j(q)),
                        ));
                    }
                }
            }
        }
        out
    }
}

/// A planar region of the ceiling — pure geometry + openings (roof lights /
/// attic hatch). Fabric lives on the `CeilingConstruction` it references.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CeilingRegion {
    pub id: ItemId,
    pub polygon: Polygon3D, // 2D polygon boundary
    pub opes: Opes,
    pub construction_id: ItemId,
    /// Thermal exposure (boundary condition) set in the Mapping phase.
    #[serde(default)]
    pub exposure: CeilingExposure,
}

impl CeilingRegion {
    /// The region's plan centroid (exterior projected to xy) — the durable
    /// geometric key for an assigned ceiling/roof construction, mirroring
    /// [`FloorRegion::centroid`](super::FloorRegion::centroid).
    pub fn centroid(&self) -> Point2f64 {
        let ring: Vec<Point2f64> = self
            .polygon
            .exterior
            .iter()
            .map(|v| Point2f64::new(v.x, v.y))
            .collect();
        ring_centroid(&ring)
    }

    /// The region's plan area (m²) — its `Polygon3D` exterior projected to xy.
    /// Mirrors [`FloorRegion::area`](super::FloorRegion::area); used by the
    /// ceiling-exposure tap pick to choose the smallest region under a point.
    pub fn area(&self) -> f64 {
        use geo::Area;
        let ring: Vec<(f64, f64)> = self.polygon.exterior.iter().map(|v| (v.x, v.y)).collect();
        Polygon::new(LineString::from(ring), vec![]).unsigned_area()
    }

    /// The region's single height when it is FLAT (every vertex z within
    /// 1e-6 m), `None` for sloped/degenerate regions. Used to reconstruct
    /// session step areas and label heights in 2D.
    pub fn flat_z(&self) -> Option<f64> {
        if self.polygon.exterior.len() < 3 {
            return None;
        }
        let z0 = self.polygon.exterior[0].z;
        self.polygon
            .exterior
            .iter()
            .all(|v| (v.z - z0).abs() <= 1e-6)
            .then_some(z0)
    }

    /// The region's single BASE height once apex vertices are dropped:
    /// vertices whose plan position matches an `apexes` point (within
    /// 1e-6 m — apex coordinates are copied verbatim into fan triangles, no
    /// boolean ops touch them) are removed, and if every remaining vertex
    /// shares one z (within 1e-6 m, like `flat_z`) that z is returned. This
    /// is what makes ceiling-step reconstruction APEX-AWARE: a stepped piece
    /// fanned under an apex still recovers its step delta on reload — the
    /// load-bearing reload-fixpoint invariant. Equals [`flat_z`] when no
    /// vertex matches an apex. `None` when NO base vertex remains or the
    /// remainder isn't level (a genuinely irregular slope). RELAXED (ridge
    /// work) to accept a SINGLE remaining base vertex: a ridge TRANSITION
    /// triangle `(v, A, B)` keeps exactly one non-apex vertex and must still
    /// recover its base on reload; the plan-degenerate needles the old
    /// two-vertex floor guarded against still die downstream at the
    /// reconstruction's `ring_area2` filter.
    ///
    /// [`flat_z`]: CeilingRegion::flat_z
    pub fn fan_base_z(&self, apexes: &[CeilingApex]) -> Option<f64> {
        self.fan_base_z_excluding(apexes, &[])
    }

    /// [`fan_base_z`](Self::fan_base_z) with the storey's UNFANNED vertex set
    /// also dropped (within [`UNFAN_MATCH_EPS_M`] — those vertices were
    /// LIFTED off the base by `lift_unfanned`, so they must not disqualify
    /// the remaining level base). Empty `unfanned` is exactly `fan_base_z`.
    pub fn fan_base_z_excluding(
        &self,
        apexes: &[CeilingApex],
        unfanned: &[Point2f64],
    ) -> Option<f64> {
        const EPS_M: f64 = 1e-6;
        if self.polygon.exterior.len() < 3 {
            return None;
        }
        let base: Vec<f64> =
            self.polygon
                .exterior
                .iter()
                .filter(|v| {
                    !apexes.iter().any(|a| {
                        (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M
                    }) && !unfanned.iter().any(|u| {
                        (v.x - u.x).abs() <= UNFAN_MATCH_EPS_M
                            && (v.y - u.y).abs() <= UNFAN_MATCH_EPS_M
                    })
                })
                .map(|v| v.z)
                .collect();
        let (&z0, rest) = base.split_first()?;
        rest.iter().all(|z| (z - z0).abs() <= EPS_M).then_some(z0)
    }

    /// `p` is strictly inside this region's plan polygon OR within `eps_m`
    /// of its boundary (point-to-ring-edge distance). The per-region half of
    /// [`CeilingRegions::claim_regions`] — the boundary band is what keeps a
    /// gable apex claimed when its derived ring is boolean-noise-displaced.
    /// `false` for plan-degenerate regions (< 3 vertices — they can never
    /// host an apex, so they must never claim one).
    pub(crate) fn plan_contains_within(&self, p: Point2f64, eps_m: f64) -> bool {
        let ext = &self.polygon.exterior;
        if ext.len() < 3 {
            return false;
        }
        let gp = GeoPoint::new(p.x, p.y);
        if Polygon::<f64>::from(self.polygon.clone()).contains(&gp) {
            return true;
        }
        let n = ext.len();
        (0..n).any(|i| {
            let (a, b) = (&ext[i], &ext[(i + 1) % n]);
            point_to_segment_distance(p.x, p.y, a.x, a.y, b.x, b.y) <= eps_m
        })
    }

    /// The contiguous run of the plan line `p + t·dir` (`t` in METRES, `dir`
    /// unit-length) that stays inside this region's plan polygon and contains
    /// t = 0, as `(t_min, t_max)` with `t_min ≤ 0 ≤ t_max`. Even-odd over the
    /// ring's crossings: every proper crossing of a ring edge with the line
    /// contributes its `t`, and an edge lying ALONG the line contributes its
    /// endpoints' `t` (the [`profile_breakpoints`] convention — tolerances are
    /// METRIC, [`PROFILE_EPS_M`], because the derived rings are boolean-op
    /// output that can sit a few 1e-7 m off the line they conceptually
    /// coincide with). `None` when `p` is not strictly inside the plan
    /// polygon (boundary EXCLUDED — the [`region_strictly_containing`]
    /// predicate) or the region/direction is degenerate.
    ///
    /// [`region_strictly_containing`]:
    /// CeilingRegions::region_strictly_containing
    pub fn plan_span_through(&self, p: Point2f64, dir: Point2f64) -> Option<(f64, f64)> {
        let ext = &self.polygon.exterior;
        if ext.len() < 3 {
            return None;
        }
        let dlen = (dir.x * dir.x + dir.y * dir.y).sqrt();
        if dlen <= 1e-12 {
            return None;
        }
        // Normalize so `t` is metres even if `dir` is only near-unit.
        let (dx, dy) = (dir.x / dlen, dir.y / dlen);
        let gp = GeoPoint::new(p.x, p.y);
        if !Polygon::<f64>::from(self.polygon.clone()).contains(&gp) {
            return None;
        }
        // Every t where the line can switch between inside and outside.
        let mut ts: Vec<f64> = Vec::new();
        let n = ext.len();
        for i in 0..n {
            let a = ext[i];
            let b = ext[(i + 1) % n];
            // Perpendicular distance (m) of a ring vertex to the LINE.
            let dist_to_line = |c: &Point3f64| ((c.x - p.x) * dy - (c.y - p.y) * dx).abs();
            if dist_to_line(&a) <= PROFILE_EPS_M && dist_to_line(&b) <= PROFILE_EPS_M {
                // (Near-)collinear overlap: the run can start/stop at the
                // edge's endpoints — both contribute their projected t.
                for c in [a, b] {
                    ts.push((c.x - p.x) * dx + (c.y - p.y) * dy);
                }
                continue;
            }
            let (ex, ey) = (b.x - a.x, b.y - a.y);
            let denom = dx * ey - dy * ex;
            if denom.abs() <= 1e-12 {
                continue; // parallel but not collinear — no crossing
            }
            // Proper crossing: line(t) == edge(s). The edge may fall short
            // of / overshoot the line by PROFILE_EPS_M (scaled to its own
            // parameter), like profile_breakpoints' crossing test.
            let (wx, wy) = (a.x - p.x, a.y - p.y);
            let t = (wx * ey - wy * ex) / denom;
            let s = (wx * dy - wy * dx) / denom;
            let edge_len = (ex * ex + ey * ey).sqrt();
            let eps_s = if edge_len > 0.0 {
                PROFILE_EPS_M / edge_len
            } else {
                0.0
            };
            if (-eps_s..=1.0 + eps_s).contains(&s) {
                ts.push(t);
            }
        }
        // The contiguous run containing t = 0 is bounded by the nearest
        // crossing on each side (NOT the union of runs — a notch past the
        // first boundary doesn't extend the span). `p` strictly inside a
        // closed ring guarantees a crossing on both sides; a degenerate ring
        // that yields none on either side returns None.
        let t_min = ts
            .iter()
            .copied()
            .filter(|t| *t <= 0.0)
            .fold(f64::NAN, f64::max);
        let t_max = ts
            .iter()
            .copied()
            .filter(|t| *t >= 0.0)
            .fold(f64::NAN, f64::min);
        (t_min.is_finite() && t_max.is_finite()).then_some((t_min, t_max))
    }

    /// Like [`plan_span_through`](Self::plan_span_through), but tolerant of `p`
    /// lying ON the region's plan boundary — the mono-pitch case where a ridge
    /// runs ALONG a shared edge, so its midpoint is on the edge, not strictly
    /// inside. When the strict span is empty we nudge `p` a hair PERPENDICULAR
    /// to `dir` (which leaves the along-`dir` origin unchanged, so the returned
    /// `(t_min, t_max)` stay valid for the original `p`) toward the region's
    /// interior and re-measure. Returns `None` only when `p` is on no edge of
    /// this region or the region/direction is degenerate.
    pub fn plan_span_through_inclusive(&self, p: Point2f64, dir: Point2f64) -> Option<(f64, f64)> {
        if let Some(s) = self.plan_span_through(p, dir) {
            return Some(s);
        }
        let ext = &self.polygon.exterior;
        if ext.len() < 3 {
            return None;
        }
        // `p` must actually sit on this region's boundary — otherwise the nudge
        // would invent a span for a point that isn't on the edge at all.
        if !self.plan_contains_within(p, APEX_BOUNDARY_EPS_M) {
            return None;
        }
        let dlen = (dir.x * dir.x + dir.y * dir.y).sqrt();
        if dlen <= 1e-12 {
            return None;
        }
        let (dx, dy) = (dir.x / dlen, dir.y / dlen);
        // Unit perpendicular to `dir`; point it toward the region centroid (the
        // vertex average — the interior side for the rectangular ceiling areas
        // a junction ridge bounds).
        let (mut nx, mut ny) = (-dy, dx);
        let n = ext.len() as f64;
        let cx = ext.iter().map(|v| v.x).sum::<f64>() / n;
        let cy = ext.iter().map(|v| v.y).sum::<f64>() / n;
        if (cx - p.x) * nx + (cy - p.y) * ny < 0.0 {
            nx = -nx;
            ny = -ny;
        }
        let eps = 1e-3;
        let pin = Point2f64::new(p.x + nx * eps, p.y + ny * eps);
        self.plan_span_through(pin, dir)
    }
}
