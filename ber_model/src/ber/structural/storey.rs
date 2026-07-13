use geo::{Contains, Coord, LineString, Point as GeoPoint, Polygon};
use serde::{Deserialize, Serialize};

use crate::ber::common::{Item, ItemId};
use crate::ber::fenestration::{Door, Window};
use domain::points::{
    point_on_ring_boundary, point_to_segment_distance, ring_centroid, Point2f64, Point3f64,
};

#[allow(unused_imports)]
use super::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    pub id: ItemId,
    pub name: String,
    pub vertices: Vec<Point3f64>,
    pub thermostats: Vec<Item>,
    pub vents: Vec<Item>,
    pub fans: Vec<Item>,
    pub lights: Vec<Item>,
    pub chimneys: Vec<Item>,
    pub trvs: Vec<Item>,
    pub living_room_candidate: bool,
    pub underfloor_heating: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Storey {
    pub id: ItemId,
    pub name: String, // e.g., "Ground Floor", "First Floor"
    pub storey_number: i32,
    pub floors: FloorRegions,
    pub walls: WallRegions,
    /// The heat-loss CEILING regions (the top of the habitable storey) — NOT the
    /// dwelling roof (that's `Ber::roof`).
    pub ceilings: CeilingRegions,
    pub windows: Vec<Window>,
    pub doors: Vec<Door>,
    pub rooms: Vec<Room>,
    /// Candidate living-space sub-areas inside the storey (the largest is chosen
    /// as THE living area later). Stored as resolved floor polygons — they're
    /// non-editable on reload (redraw to change), so unlike the outline they
    /// need no parametric snapshot. `#[serde(default)]` so older records load.
    #[serde(default)]
    pub living_areas: FloorRegions,
    /// Default ceiling height in metres above this storey's floor datum.
    /// `None` until the assessor enters it — gates 3D viewing; when `Some`,
    /// resolve derives the default ceiling regions and extrudes walls to it.
    #[serde(default)]
    pub ceiling_height_m: Option<f64>,
    /// Declared roof high points: each fans the ceiling region containing it
    /// up (or down) to its absolute `z_m`. Durable INPUT like
    /// `ceiling_height_m` — a fan apex is not cleanly invertible from
    /// triangulated geometry, so resolve re-applies the list on every
    /// materialize, AFTER the ceiling-step areas (composition order is law:
    /// default ceiling → steps → apexes). `#[serde(default)]` so older
    /// records load.
    #[serde(default, alias = "ceiling_apexes")]
    pub ceiling_apexes: Vec<CeilingApex>,
    /// Side hints for apexes on an internal shared boundary (a mono-pitch ridge
    /// along the house↔extension junction) — which ceiling area each slopes.
    /// Keyed by apex point (see [`CeilingApexHint`]); empty for normal apexes.
    /// `#[serde(default)]` so older records load.
    #[serde(default, alias = "ceiling_apex_hints")]
    pub ceiling_apex_hints: Vec<CeilingApexHint>,
    /// UNFANNED ceiling ring vertices (tap-to-unfan): plan points (f32 grid,
    /// the `CeilingApex::quantize_point` convention) of ceiling-area corners
    /// that must NOT anchor a fan/ridge plane of their own — the resolve lifts
    /// each onto its neighbours' chord plane instead
    /// (`CeilingRegions::with_apexes_hinted_unfanned`). Durable INPUT like the
    /// apexes, re-applied on every recompose. `#[serde(default)]` so older
    /// records load (empty = the fan-to-every-vertex behaviour of before).
    #[serde(default)]
    pub ceiling_unfanned: Vec<(f32, f32)>,
    /// User-selected GABLE pitch-anchor OVERRIDES (plan points, f32 grid —
    /// the `CeilingApex::quantize_point` convention): a point matching an
    /// ELIGIBLE eave vertex of a true-gable ridge side makes THAT vertex the
    /// side's plane anchor instead of the auto-nearest corner
    /// (`gable_plan_classify`); stale points are ignored. Durable INPUT like
    /// `ceiling_unfanned`, re-applied on every recompose; a CEILING-solve
    /// concern only — roof areas keep auto-nearest. `#[serde(default)]` so
    /// older records load (empty = auto-nearest everywhere).
    #[serde(default)]
    pub ceiling_plane_anchors: Vec<(f32, f32)>,
    /// Flat-topped DORMER patches carved into this storey's sloped attic
    /// ceiling: each raises its `footprint` to a single flat `top_z_m` while the
    /// surrounding roof KEEPS its slope (the dormer does NOT reslope its
    /// neighbours; its vertical walls fall out of `riser_faces`). Durable INPUT
    /// like the apexes, replayed by `CeilingRegions::with_flat_patches` at
    /// resolve. `#[serde(default)]` so older records load.
    #[serde(default)]
    pub dormers: Vec<Dormer>,
    /// Floor-slab thickness in metres between this storey's floor and the
    /// ceiling of the storey below it bears on. Durable INPUT like
    /// `ceiling_height_m` — asked first when the storey is added (never on
    /// storey 0) and re-editable in Heights. `None` on storey 0 and on
    /// records from before the new-storey flow: `Ber::storey_floor_datums`
    /// then falls back to the legacy below-ceiling-height stacking.
    /// `#[serde(default)]` so older records load.
    #[serde(default)]
    pub floor_thickness_m: Option<f64>,
    /// Durable CANONICAL ceiling-step list (`(world-metre plan ring, z delta)`)
    /// — the explicit record of the storey's ceiling steps, so the editing list
    /// is RESTORED on reload instead of re-inferred from the roof geometry. The
    /// controller keeps it in step with the session list at each edit; the
    /// regions-as-truth ceilings are the geometric truth, this is the editable
    /// INPUT behind them. Empty on records saved before this field existed:
    /// reload RESTORES the durable list only — there is NO geometry-inference
    /// migration (the step-inference subsystem was removed), so such a record
    /// simply has no editable steps to restore until the assessor re-draws them.
    /// `#[serde(default)]` so older records load.
    #[serde(default)]
    pub ceiling_steps: Vec<(Vec<Point2f64>, f64)>,
    /// Structure-phase FLOOR splits: world-metre plan rings, each a finalized
    /// (dimensioned) cut whose two sides become separate floor regions with
    /// IDENTICAL properties (`construction_id` + `level_m` copied to both). Like
    /// a ceiling divider the split leaves no recoverable trace (both sides share
    /// every property), so the ring is the only durable record; resolve replays
    /// it as a property-preserving split (`FloorRegions::with_splits`), AFTER the
    /// phase/step partition so it composes on top. `#[serde(default)]` so older
    /// records load.
    #[serde(default)]
    pub floor_splits: Vec<Vec<Point2f64>>,
    /// Structure-phase ROOF/CEILING splits — same contract as `floor_splits`,
    /// over the ceiling regions. Replayed by `CeilingRegions::split_faces_coplanar`
    /// AFTER the apexes: it cuts the already-resolved faces in place (z read
    /// from each face's plane), so a split piece keeps the exact surface — a
    /// carve across a ridge yields coplanar sub-faces, never re-pitched halves.
    /// `#[serde(default)]` so older records load.
    #[serde(default, alias = "ceiling_splits")]
    pub ceiling_splits: Vec<Vec<Point2f64>>,
    /// Structure-phase WALL splits: each the plan SEGMENT of a finalized cut
    /// that crosses a wall's base edge. A wall split is a vertical plane (not an
    /// area), so the segment — not a closed ring — is stored: resolve lifts it to
    /// the crossing point + along-distance to split the wall and re-home its
    /// openings (`WallRegions::apply_splits`). `#[serde(default)]` so older
    /// records load.
    #[serde(default)]
    pub wall_splits: Vec<[Point2f64; 2]>,
    /// Face-PARTITIONED perimeter walls: the durable partition RECIPE (cut lines
    /// + per-sub-face exposure samples, wall-local) keyed by the wall's base-edge
    /// endpoints ([`WallFaceKey`], orientation-preserving). Category-(b): the
    /// cuts and samples are consumed by `apply_face_partitions` (resolve) and are
    /// NOT invertible from the split `WallRegion`s, so unlike the exposures they
    /// must be STORED, not reconstructed. The editing layer keeps a
    /// `VertexId`-keyed working copy, rebuilt from this on load via
    /// `surface_ref_at`. `#[serde(default)]` so older records load.
    #[serde(default)]
    pub wall_face_partitions: Vec<(WallFaceKey, WallFacePartition)>,
    /// Phase-3 (Objects) survey: the fixed building-services objects the assessor
    /// recorded on this storey, grouped by the approximate room location they
    /// were captured at. Durable INPUT (a pin position + counted, photographed
    /// objects) — not derived from anything. `#[serde(default)]` so older records
    /// load with no objects.
    #[serde(default)]
    pub object_rooms: Vec<crate::ber::objects::ObjectRoom>,
    // No editable sketch snapshot is stored: the resolved geometry above
    // (`floors`/`walls`/`living_areas`) is the only persisted representation,
    // and the editor rebuilds its working state from it on load. The DB only
    // ever holds finalised (complete, metre-scale) geometry.
}

impl Storey {
    /// Record a dimensioned Structure-phase split onto the durable split field
    /// for `kind` (replayed by `resolve_storey`). Floors/ceilings store the whole
    /// carved `ring`; walls store the fixed interior cut SEGMENT(s) in
    /// `wall_cuts` (each crossing at most one wall) — the ring is unused for
    /// walls. This owns which field a split lands in; the caller supplies the
    /// already-derived ring and cut edges.
    pub fn push_structure_split(
        &mut self,
        kind: SurfaceKind,
        ring: &[Point2f64],
        wall_cuts: Vec<[Point2f64; 2]>,
    ) {
        match kind {
            SurfaceKind::Floors => self.floor_splits.push(ring.to_vec()),
            SurfaceKind::Ceilings => self.ceiling_splits.push(ring.to_vec()),
            SurfaceKind::Walls => self.wall_splits.extend(wall_cuts),
        }
    }

    /// The floor-region ring (world metres, closing duplicate dropped) whose
    /// boundary passes through BOTH `wall_a` and `wall_b` — the local room a
    /// wall belongs to, e.g. for placing a dimension arrow in its interior.
    /// `None` if no region's outline contains both endpoints.
    pub fn floor_ring_containing_wall(
        &self,
        wall_a: (f64, f64),
        wall_b: (f64, f64),
    ) -> Option<Vec<(f64, f64)>> {
        let eps = 1e-3_f64;
        for region in &self.floors.regions {
            let mut coords: Vec<(f64, f64)> = region
                .polygon
                .exterior()
                .coords()
                .map(|c| (c.x, c.y))
                .collect();
            // Drop the closing duplicate that geo adds.
            if coords.len() >= 2 && {
                let first = coords[0];
                let last = coords[coords.len() - 1];
                (first.0 - last.0).abs() < eps && (first.1 - last.1).abs() < eps
            } {
                coords.pop();
            }
            let has = |p: (f64, f64)| {
                coords
                    .iter()
                    .any(|q| (q.0 - p.0).abs() < eps && (q.1 - p.1).abs() < eps)
            };
            if has(wall_a) && has(wall_b) {
                return Some(coords);
            }
        }
        None
    }

    /// Drop all DERIVED geometry — floors, walls, living areas, ceilings, and the
    /// The storey's UNFANNED ceiling vertices as plan points — the form
    /// the `CeilingRegions` shaping/grouping calls consume.
    pub fn ceiling_unfanned_points(&self) -> Vec<Point2f64> {
        self.ceiling_unfanned
            .iter()
            .map(|&(x, y)| Point2f64::new(x as f64, y as f64))
            .collect()
    }

    /// The storey's gable plane-anchor OVERRIDES as plan points — the form
    /// the `CeilingRegions` shaping/grouping calls consume.
    pub fn ceiling_plane_anchor_points(&self) -> Vec<Point2f64> {
        self.ceiling_plane_anchors
            .iter()
            .map(|&(x, y)| Point2f64::new(x as f64, y as f64))
            .collect()
    }

    /// The storey's ceiling apex groups WITH its own unfanned + plane-anchor
    /// sets supplied — the call every consumer with a `Storey` in hand
    /// should make (the raw `CeilingRegions::apex_groups` would
    /// warn-exclude lifted/embedded faces from the merged rings on a storey
    /// carrying either set).
    pub fn ceiling_apex_groups(&self) -> Vec<CeilingApexGroup> {
        self.ceilings.apex_groups_shaped(
            &self.ceiling_apexes,
            &self.ceiling_unfanned_points(),
            &self.ceiling_plane_anchor_points(),
        )
    }

    /// Drop all DERIVED geometry — floors, walls, living areas, ceilings, and the
    /// fan apexes over them — back to empty. The persist path calls this when
    /// the outline isn't finalised yet: that geometry is still pixel/incomplete
    /// (ceilings derived over a pixel footprint are pixel units too, so the apexes
    /// that fan them go with it), and the DB only ever holds finalised geometry.
    /// The durable INPUTS (`ceiling_height_m`, `ceiling_apex_hints`,
    /// `floor_thickness_m`, `ceiling_steps`) are left untouched.
    pub fn clear_derived_geometry(&mut self) {
        self.floors = FloorRegions::default();
        self.walls = WallRegions::default();
        self.living_areas = FloorRegions::default();
        self.ceilings = CeilingRegions::default();
        self.ceiling_apexes = Vec::new();
    }

    /// How many of each structural surface the storey holds — the
    /// Structure-phase tool read-out (how many walls / ceilings / floors the
    /// diagram shows). Counts the derived regions, so they're meaningful only
    /// after the outline is finalised (walls/ceilings/floors are re-derived by
    /// `document::resolve_storey`).
    pub fn surface_counts(&self) -> SurfaceCounts {
        SurfaceCounts {
            walls: self.walls.regions.len(),
            ceilings: self.ceilings.regions.len(),
            floors: self.floors.regions.len(),
        }
    }

    /// The id of the surface of `kind` under (or, for walls, nearest within
    /// `tol` of) the plan point `(x, y)`. Floors/ceilings: point-in-polygon
    /// over the region footprint; walls: nearest base edge within tolerance
    /// (a wall collapses to its base edge in plan).
    ///
    /// SCOPE: the WALLS arm is structure-scope filtered in place (an
    /// out-of-scope wall never shadows a selectable one within `tol`; this
    /// arm has no other callers). The FLOORS/CEILINGS arms are UNFILTERED —
    /// they're shared with the Mapping-phase exposure taps, which must reach
    /// a Party/NonHeatLoss region to repaint its classification. The
    /// Structure-phase tap goes through
    /// [`structure_surface_at`](Self::structure_surface_at) instead.
    pub fn surface_at(&self, kind: SurfaceKind, x: f64, y: f64, tol: f64) -> Option<ItemId> {
        match kind {
            SurfaceKind::Floors => self
                .floors
                .regions
                .iter()
                .find(|r| r.polygon.contains(&GeoPoint::new(x, y)))
                .map(|r| r.id),
            SurfaceKind::Ceilings => self
                .ceilings
                .regions
                .iter()
                .find(|r| polygon3d_contains_plan(&r.polygon, x, y))
                .map(|r| r.id),
            // Walls collapse to their base edge in plan — pick the nearest one
            // within tolerance (its first two exterior vertices are the ground
            // line `WallRegions::regenerate` lays down).
            SurfaceKind::Walls => self
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
                .filter_map(|r| {
                    let e = &r.polygon.exterior;
                    if e.len() < 2 {
                        return None;
                    }
                    let d = point_to_segment_distance(x, y, e[0].x, e[0].y, e[1].x, e[1].y);
                    (d <= tol).then_some((d, r.id))
                })
                .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
                .map(|(_, id)| id),
        }
    }

    /// Every surface id of `kind` whose footprint lies inside the carved
    /// `region_ring` (world metres) — the Structure-phase carve pick. Floors and
    /// ceilings match when their footprint centre is inside; a wall (a BOUNDARY
    /// edge, never strictly inside) matches when its base-edge midpoint sits on
    /// the ring's perimeter OR strictly inside (an interior riser between
    /// stepped sections), the spanning wall claimed by whichever region holds
    /// its midpoint. Surfaces out of structure scope (`in_structure_scope`
    /// on the kind's exposure enum) are never picked.
    pub fn surfaces_in_ring(&self, kind: SurfaceKind, region_ring: &[Point2f64]) -> Vec<ItemId> {
        let ring_poly = ring_to_geo(region_ring);
        let inside = |x: f64, y: f64| ring_poly.contains(&GeoPoint::new(x, y));
        match kind {
            SurfaceKind::Floors => self
                .floors
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
                .filter(|r| {
                    let c = ring_centroid(&floor_ring(&r.polygon));
                    inside(c.x, c.y)
                })
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Ceilings => self
                .ceilings
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
                .filter(|r| {
                    let c = ring_centroid(&ceiling_ring(&r.polygon));
                    inside(c.x, c.y)
                })
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Walls => self
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
                .filter(|r| match r.base_midpoint() {
                    Some(m) => {
                        point_on_ring_boundary(region_ring, m.x, m.y, WALL_ON_BOUNDARY_TOL_M)
                            || inside(m.x, m.y)
                    }
                    None => false,
                })
                .map(|r| r.id)
                .collect(),
        }
    }

    /// [`surface_at`](Self::surface_at) restricted to STRUCTURE scope — the
    /// Structure-phase tap hit-test. A hit on an out-of-scope floor/ceiling
    /// (party / derived inter-storey — e.g. the ground-floor ceiling under an
    /// upper storey) reads as NO hit, so the tap can fall through to another
    /// active kind instead of selecting a surface the phase doesn't attribute.
    pub fn structure_surface_at(
        &self,
        kind: SurfaceKind,
        x: f64,
        y: f64,
        tol: f64,
    ) -> Option<ItemId> {
        let id = self.surface_at(kind, x, y, tol)?;
        self.surface_in_structure_scope(kind, id).then_some(id)
    }

    /// Whether region `id` of `kind` participates in the Structure phase —
    /// the per-kind `in_structure_scope` of its exposure. `false` for an id
    /// that isn't a live region of that kind.
    pub fn surface_in_structure_scope(&self, kind: SurfaceKind, id: ItemId) -> bool {
        match kind {
            SurfaceKind::Walls => self
                .walls
                .regions
                .iter()
                .find(|r| r.id == id)
                .is_some_and(|r| r.exposure.in_structure_scope()),
            SurfaceKind::Floors => self
                .floors
                .regions
                .iter()
                .find(|r| r.id == id)
                .is_some_and(|r| r.exposure.in_structure_scope()),
            SurfaceKind::Ceilings => self
                .ceilings
                .regions
                .iter()
                .find(|r| r.id == id)
                .is_some_and(|r| r.exposure.in_structure_scope()),
        }
    }

    /// The wall regions a single carve edge MAPS TO: every wall whose base-edge
    /// midpoint lies on the segment `a`→`b` (within the same tolerance as
    /// [`surfaces_in_ring`](Self::surfaces_in_ring)'s boundary test, so an
    /// exclusion here removes exactly what that inclusion test added). The
    /// shell's per-edge include/exclude decision for a staged structure carve
    /// resolves to wall ids through this. Walls out of structure scope
    /// ([`WallExposure::in_structure_scope`]) don't map: an edge whose only
    /// walls are non-heat-loss / inter-storey has nothing selectable on it, so
    /// the carve's wall-inclusion ask never asks about it.
    pub fn wall_ids_on_segment(&self, a: Point2f64, b: Point2f64) -> Vec<ItemId> {
        self.walls
            .regions
            .iter()
            .filter(|r| r.exposure.in_structure_scope())
            .filter(|r| {
                r.base_midpoint().is_some_and(|m| {
                    point_to_segment_distance(m.x, m.y, a.x, a.y, b.x, b.y)
                        <= WALL_ON_BOUNDARY_TOL_M
                })
            })
            .map(|r| r.id)
            .collect()
    }

    /// Does `ring` (world coords) genuinely SUBDIVIDE a labellable
    /// (non-inter-storey) FLOOR region — i.e. cross it so a split would yield
    /// more than one piece?
    /// True iff some labellable region's polygon both overlaps the ring (positive
    /// intersection area) AND isn't fully covered by it (positive difference
    /// area). The carve/cut split-overlap test: an interior carve and an
    /// edge-to-edge cut both pass, while a ring that misses a storey (or coincides
    /// with its whole footprint) doesn't.
    pub fn ring_splits_labellable_floor(&self, ring: &[Point2f64]) -> bool {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        const SLIVER_AREA_M2: f64 = 1e-6;
        if ring.len() < 3 {
            return false;
        }
        let cut = ring_to_geo(ring);
        self.floors
            .regions
            .iter()
            // InterStorey is the derived hidden class — everything else
            // (NonHeatLoss/party floors included) is assessor-paintable and
            // therefore splittable.
            .filter(|r| r.exposure != FloorExposure::InterStorey)
            .any(|r| {
                let inside = r.polygon.intersection(&cut).unsigned_area();
                let outside = r.polygon.difference(&cut).unsigned_area();
                inside >= SLIVER_AREA_M2 && outside >= SLIVER_AREA_M2
            })
    }

    /// Mirror of [`ring_splits_labellable_floor`](Self::ring_splits_labellable_floor)
    /// over the storey's labellable (non-`InterStorey`) ceiling/roof regions.
    pub fn ring_splits_labellable_ceiling(&self, ring: &[Point2f64]) -> bool {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        const SLIVER_AREA_M2: f64 = 1e-6;
        if ring.len() < 3 {
            return false;
        }
        let cut = ring_to_geo(ring);
        self.ceilings
            .regions
            .iter()
            // InterStorey is the derived hidden class — everything else
            // (Party ceilings included) is assessor-paintable and therefore
            // splittable.
            .filter(|r| r.exposure != CeilingExposure::InterStorey)
            .any(|r| {
                let ceiling_ring = ceiling_ring(&r.polygon);
                let poly = ring_to_geo(&ceiling_ring);
                let inside = poly.intersection(&cut).unsigned_area();
                let outside = poly.difference(&cut).unsigned_area();
                inside >= SLIVER_AREA_M2 && outside >= SLIVER_AREA_M2
            })
    }

    /// Clip a cross-storey LASSO ring (world coords, THIS storey's frame)
    /// against the storey's footprint: `ring ∩ footprint` as one or more
    /// simple rings, each paired with its INTERIOR edges — the clipped-ring
    /// edges that do NOT lie on the footprint boundary (they were drawn by
    /// the lasso through the middle of the storey, so house area continues
    /// beyond them and wall inclusion there is a judgement call, exactly like
    /// the single-storey carve's interior edges). Edges ON the footprint
    /// boundary are the storey's true exterior there — never ambiguous. The
    /// lasso may miss this storey entirely (it spans the all-storeys view,
    /// gap included): that's simply an empty result.
    pub fn lasso_regions(&self, ring: &[Point2f64]) -> Vec<(Vec<Point2f64>, Vec<[Point2f64; 2]>)> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        const SLIVER_AREA_M2: f64 = 1e-4;
        if ring.len() < 3 || self.floors.regions.is_empty() {
            return Vec::new();
        }
        // The storey footprint: the union of its floor regions.
        let mut footprint = geo::MultiPolygon::new(vec![self.floors.regions[0].polygon.clone()]);
        for r in self.floors.regions.iter().skip(1) {
            footprint = footprint.union(&geo::MultiPolygon::new(vec![r.polygon.clone()]));
        }
        let clipped = footprint.intersection(&geo::MultiPolygon::new(vec![ring_to_geo(ring)]));
        // Every footprint boundary ring (exteriors + any holes), for the
        // edge classification below.
        let boundary_rings: Vec<Vec<Point2f64>> = footprint
            .iter()
            .flat_map(|p| {
                std::iter::once(p.exterior())
                    .chain(p.interiors().iter())
                    .map(|ls| {
                        let mut r: Vec<Point2f64> =
                            ls.coords().map(|c| Point2f64::new(c.x, c.y)).collect();
                        // Drop the closing duplicate geo appends.
                        if r.len() >= 2 {
                            r.pop();
                        }
                        r
                    })
            })
            .collect();
        let on_boundary = |x: f64, y: f64| {
            boundary_rings
                .iter()
                .any(|r| point_on_ring_boundary(r, x, y, WALL_ON_BOUNDARY_TOL_M))
        };
        clipped
            .iter()
            .filter(|p| p.unsigned_area() >= SLIVER_AREA_M2)
            .map(|p| {
                let mut out: Vec<Point2f64> = p
                    .exterior()
                    .coords()
                    .map(|c| Point2f64::new(c.x, c.y))
                    .collect();
                if out.len() >= 2 {
                    out.pop(); // closing duplicate
                }
                let n = out.len();
                let interior: Vec<[Point2f64; 2]> = (0..n)
                    .map(|k| [out[k], out[(k + 1) % n]])
                    .filter(|e| {
                        let (mx, my) = ((e[0].x + e[1].x) * 0.5, (e[0].y + e[1].y) * 0.5);
                        !on_boundary(mx, my)
                    })
                    .collect();
                (out, interior)
            })
            .collect()
    }

    /// Region ids of `kind` that lie MOSTLY inside `ring` (world coords): the
    /// region∩ring area is at least `min_frac` of the region's OWN area. Unlike
    /// the centroid test in [`surfaces_in_ring`](Self::surfaces_in_ring), this
    /// doesn't mis-select a concave region whose area-weighted centroid happens
    /// to fall inside the ring — e.g. an L-shaped cut REMAINDER, whose centroid
    /// sits in the notch (inside the chosen half) but which is almost entirely
    /// OUTSIDE it. The carve/cut SELECT uses this so only the piece actually
    /// carved out is picked, never the rest of the floor/ceiling.
    pub fn surfaces_mostly_in_ring(
        &self,
        kind: SurfaceKind,
        ring: &[Point2f64],
        min_frac: f64,
    ) -> Vec<ItemId> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        if ring.len() < 3 {
            return Vec::new();
        }
        let cut = ring_to_geo(ring);
        let mostly = |poly: &geo::Polygon<f64>| {
            let area = poly.unsigned_area();
            area > 0.0 && poly.intersection(&cut).unsigned_area() >= min_frac * area
        };
        match kind {
            SurfaceKind::Floors => self
                .floors
                .regions
                .iter()
                .filter(|r| mostly(&r.polygon))
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Ceilings => self
                .ceilings
                .regions
                .iter()
                .filter(|r| mostly(&ring_to_geo(&ceiling_ring(&r.polygon))))
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Walls => Vec::new(),
        }
    }

    /// Which kind of surface region `id` is, if any — lets a mixed-kind
    /// selection (walls + floors + ceilings) recover each id's kind. `None` when no
    /// region of any kind has that id.
    pub fn surface_kind_of(&self, id: ItemId) -> Option<SurfaceKind> {
        if self.walls.regions.iter().any(|r| r.id == id) {
            Some(SurfaceKind::Walls)
        } else if self.ceilings.regions.iter().any(|r| r.id == id) {
            Some(SurfaceKind::Ceilings)
        } else if self.floors.regions.iter().any(|r| r.id == id) {
            Some(SurfaceKind::Floors)
        } else {
            None
        }
    }

    /// Every surface id of `kind` on the storey (the `Select all` seed).
    /// Surfaces out of structure scope (`in_structure_scope` on the kind's
    /// exposure enum — non-heat-loss / party / derived inter-storey) are
    /// skipped — `Select all` never seeds them.
    pub fn surface_ids(&self, kind: SurfaceKind) -> Vec<ItemId> {
        match kind {
            SurfaceKind::Walls => self
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Ceilings => self
                .ceilings
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Floors => self
                .floors
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
                .map(|r| r.id)
                .collect(),
        }
    }

    /// The KNOWN ceiling-context height at plan point `(x, y)` within `tol`
    /// (world metres), or `None` when the surface hit has no height the
    /// assessor could read (a sloped face / mid-slope point / sloped ridge).
    /// Tried in order, first hit wins: (1) a LEVEL ridge / single apex, (2) a
    /// FLAT-TOPPED wall, (3) a FLAT ceiling area. Reads RESOLVED regions — the
    /// caller materializes first.
    pub fn copy_height_ceiling_at(&self, x: f64, y: f64, tol: f64) -> Option<f64> {
        // 1. LEVEL ridge segment / single apex point.
        for g in self.ceiling_apex_groups() {
            match g.apex_idxs.as_slice() {
                [ia, ib] => {
                    let (a, b) = (&self.ceiling_apexes[*ia], &self.ceiling_apexes[*ib]);
                    if (a.z_m - b.z_m).abs() > 1e-6 {
                        continue; // sloped/mono-pitch ridge — height not readable
                    }
                    if point_to_segment_distance(x, y, a.point.x, a.point.y, b.point.x, b.point.y)
                        <= tol
                    {
                        return Some(a.z_m);
                    }
                }
                [ia] => {
                    let a = &self.ceiling_apexes[*ia];
                    if (x - a.point.x).hypot(y - a.point.y) <= tol {
                        return Some(a.z_m);
                    }
                }
                _ => {}
            }
        }
        // 2. FLAT-TOPPED wall.
        if let Some(id) = self.surface_at(SurfaceKind::Walls, x, y, tol) {
            if let Some(z) = self
                .walls
                .regions
                .iter()
                .find(|r| r.id == id)
                .and_then(|w| w.flat_top_z())
            {
                return Some(z);
            }
        }
        // 3. FLAT ceiling area (flat_z Some = known; None = sloped → None).
        if let Some(id) = self.surface_at(SurfaceKind::Ceilings, x, y, tol) {
            return self
                .ceilings
                .regions
                .iter()
                .find(|r| r.id == id)
                .and_then(|r| r.flat_z());
        }
        None
    }

    /// The floor level at plan point `(x, y)` within `tol` — floors are always
    /// flat, so any floor area's `level_m` is a KNOWN height. `None` off any
    /// floor region. Reads RESOLVED regions — the caller materializes first.
    pub fn copy_height_floor_at(&self, x: f64, y: f64, tol: f64) -> Option<f64> {
        let id = self.surface_at(SurfaceKind::Floors, x, y, tol)?;
        self.floors
            .regions
            .iter()
            .find(|r| r.id == id)
            .map(|r| r.level_m)
    }
}

/// Per-storey tally of structural surfaces, one field per kind. Drives the
/// Structure-phase tool counts (and a future structure checklist).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SurfaceCounts {
    pub walls: usize,
    pub ceilings: usize,
    pub floors: usize,
}

/// A structural surface kind — the Structure-phase headings (Walls / Ceilings /
/// Floors) the surface queries and the assessor's Select/Set subtools hang
/// under. The data-model taxonomy the queries above (`surface_at`,
/// `surfaces_in_ring`, `surface_ids`) dispatch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SurfaceKind {
    Walls,
    Ceilings,
    Floors,
}

/// How close (world metres) a wall's base-edge midpoint must be to a carved
/// region's boundary to count as that region's wall. Walls trace the outline
/// exactly (same source coords), so this only absorbs f64 round-trip error.
const WALL_ON_BOUNDARY_TOL_M: f64 = 0.05;

/// Whether the `Polygon3D`'s plan projection (its `(x, y)` footprint) contains
/// `(x, y)` — point-in-polygon for a ceiling/roof region.
fn polygon3d_contains_plan(polygon: &Polygon3D, x: f64, y: f64) -> bool {
    ring_to_geo(&ceiling_ring(polygon)).contains(&GeoPoint::new(x, y))
}

/// A floor region's exterior ring as plan `Point2f64`s.
fn floor_ring(polygon: &Polygon<f64>) -> Vec<Point2f64> {
    polygon
        .exterior()
        .coords()
        .map(|c| Point2f64::new(c.x, c.y))
        .collect()
}

/// A ceiling/wall region's exterior ring projected to plan `Point2f64`s.
fn ceiling_ring(polygon: &Polygon3D) -> Vec<Point2f64> {
    polygon
        .exterior
        .iter()
        .map(|v| Point2f64::new(v.x, v.y))
        .collect()
}

/// Build a closed `geo` polygon from a plan ring for point-in-polygon tests.
fn ring_to_geo(ring: &[Point2f64]) -> Polygon<f64> {
    let coords: Vec<Coord<f64>> = ring.iter().map(|p| Coord { x: p.x, y: p.y }).collect();
    Polygon::new(LineString::from(coords), vec![])
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub enum ThermalMassCategoryEnum {
    Low,
    Medium,
    High,
    #[default]
    Invalid,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ThermalMassInfo {
    pub id: ItemId,
    pub floors: ThermalMassCategoryEnum,
    pub partition_walls: ThermalMassCategoryEnum,
    pub external_walls: ThermalMassCategoryEnum,
    pub separating_walls: ThermalMassCategoryEnum,
}
