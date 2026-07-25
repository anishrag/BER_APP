use geo::{LineString, MultiPolygon, Polygon};
use serde::{Deserialize, Serialize};

use crate::ber::common::{ConstructionPhase, ItemId};
use domain::points::{ring_centroid, Point2f64};

#[allow(unused_imports)]
use super::*;

/// A planar region of the floor — pure geometry. Its fabric (construction
/// type/make-up/phase, photos, description) lives on the `FloorConstruction`
/// referenced by `construction_id`. Floors carry no vertical extent: `level_m`
/// is their datum; ceilings own heights (`CeilingRegion` per-vertex z).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FloorRegion {
    pub id: ItemId,
    pub polygon: Polygon<f64>, // 2D polygon boundary
    pub construction_id: ItemId,
    /// Floor level in metres relative to the storey's floor datum: 0.0 = the
    /// unstepped storey floor, positive = raised, negative = lowered (steps).
    #[serde(default)]
    pub level_m: f64,
    /// Thermal exposure (boundary condition) set in the Mapping phase.
    #[serde(default)]
    pub exposure: FloorExposure,
    /// Ceiling height in metres above the storey floor datum for a LIVING-AREA
    /// region — the snapped floor-to-ceiling height of the room, taken from the
    /// ceiling above it (see [`document::living_area_ceiling_height`]).
    /// Populated ONLY on [`Storey::living_areas`](crate::ber::structural::Storey)
    /// regions; `None` on ordinary floor partition regions, which carry no
    /// vertical extent (`level_m` is their datum). Re-derived on every resolve
    /// so it always tracks the ceiling above — hence `skip_serializing_if` keeps
    /// records with no living-area height byte-identical to the pre-field layout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height_m: Option<f64>,
}

impl FloorRegion {
    /// The region's plan area in square metres — used to disambiguate a tap
    /// that lands on overlapping floors from different storeys (the smallest /
    /// most-specific sub-region wins).
    pub fn area(&self) -> f64 {
        use geo::Area;
        self.polygon.unsigned_area()
    }

    /// The region's plan centroid — the durable geometric key for an assigned
    /// construction (floors have no parametric `SurfaceRef`, so the centroid is
    /// their stable identity across the re-derive; mirrors a riser's base edge).
    pub fn centroid(&self) -> Point2f64 {
        let ring: Vec<Point2f64> = self
            .polygon
            .exterior()
            .coords()
            .map(|c| Point2f64::new(c.x, c.y))
            .collect();
        ring_centroid(&ring)
    }

    /// Per-segment overlap length of this region's boundary with the given
    /// plan segments (e.g. exposed-wall base edges) — `out[k]` is how much of
    /// `segs[k]` lies ALONG the region's exterior ring. The DEAP
    /// exposed-perimeter share of one floor element is the sum over the
    /// dwelling's exposed/semi-exposed walls.
    ///
    /// Tolerant of derivation noise: a segment counts against a ring edge when
    /// both its endpoints are within [`BOUNDARY_TOL_M`] of the edge's infinite
    /// line (regions and walls derive from the same outline, so genuine
    /// matches are far inside this and genuine offsets — e.g. an internal wall
    /// a room away — are far outside it). Overlap is measured in 1D along the
    /// edge, so a wall merely COLLINEAR with an edge but elsewhere on the plan
    /// contributes nothing, and partial overlaps count partially. Each
    /// segment's total is capped at its own length, so consecutive collinear
    /// ring edges can't double-count a wall.
    pub fn boundary_overlaps(&self, segs: &[(Point2f64, Point2f64)]) -> Vec<f64> {
        /// Plan-distance tolerance for "this wall lies on this boundary".
        const BOUNDARY_TOL_M: f64 = 0.01;

        let ring: Vec<Point2f64> = self
            .polygon
            .exterior()
            .coords()
            .map(|c| Point2f64::new(c.x, c.y))
            .collect();

        segs.iter()
            .map(|(a, b)| {
                let seg_len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
                if seg_len < BOUNDARY_TOL_M {
                    return 0.0;
                }
                let mut total = 0.0;
                for edge in ring.windows(2) {
                    let (p, q) = (edge[0], edge[1]);
                    let (ex, ey) = (q.x - p.x, q.y - p.y);
                    let edge_len = (ex * ex + ey * ey).sqrt();
                    if edge_len < BOUNDARY_TOL_M {
                        continue;
                    }
                    let (ux, uy) = (ex / edge_len, ey / edge_len);
                    // Perpendicular distance of both segment endpoints to the
                    // edge's infinite line — collinearity within tolerance.
                    let d_a = ((a.x - p.x) * uy - (a.y - p.y) * ux).abs();
                    let d_b = ((b.x - p.x) * uy - (b.y - p.y) * ux).abs();
                    if d_a > BOUNDARY_TOL_M || d_b > BOUNDARY_TOL_M {
                        continue;
                    }
                    // 1D overlap of the segment's projection with the edge.
                    let t_a = (a.x - p.x) * ux + (a.y - p.y) * uy;
                    let t_b = (b.x - p.x) * ux + (b.y - p.y) * uy;
                    let (lo, hi) = (t_a.min(t_b), t_a.max(t_b));
                    total += (hi.min(edge_len) - lo.max(0.0)).max(0.0);
                }
                total.min(seg_len)
            })
            .collect()
    }
}

/// A planar region of the floor
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FloorRegions {
    pub regions: Vec<FloorRegion>,
}

impl FloorRegions {
    /// Create a `FloorRegions` with a single region referencing `construction_id`.
    pub fn new(vertices: Vec<Point2f64>, construction_id: ItemId) -> Self {
        let polygon = Polygon::new(
            LineString::from(vertices.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
            vec![],
        );

        let region = FloorRegion {
            id: ItemId::default(),
            polygon,
            construction_id,
            level_m: 0.0,
            exposure: FloorExposure::default(),
            height_m: None,
        };

        Self {
            regions: vec![region],
        }
    }

    /// Create a FloorRegions containing one region per polygon. Used when a
    /// storey has multiple disjoint floor areas (e.g. extension sections on
    /// upper floors). All regions share the same `construction_id`. Takes raw
    /// vertex lists straight from the parametric layer.
    pub fn from_vertex_lists(polygons: Vec<Vec<Point2f64>>, construction_id: ItemId) -> Self {
        let regions = polygons
            .into_iter()
            .filter(|p| p.len() >= 3)
            .map(|vertices| {
                let polygon = Polygon::new(
                    LineString::from(vertices.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                    vec![],
                );
                FloorRegion {
                    id: ItemId::default(),
                    polygon,
                    construction_id,
                    level_m: 0.0,
                    exposure: FloorExposure::default(),
                    height_m: None,
                }
            })
            .collect();

        Self { regions }
    }

    /// Exterior vertices of each region, one `Vec` per region. Drops the
    /// closing coordinate that `geo` polygons repeat (last == first). Inverse
    /// of `from_vertex_lists`; used by the editor to rebuild a parametric model
    /// from resolved geometry when no sketch snapshot is available.
    pub fn to_vertex_lists(&self) -> Vec<Vec<Point2f64>> {
        self.regions
            .iter()
            .map(|r| {
                let mut pts: Vec<Point2f64> = r
                    .polygon
                    .exterior()
                    .coords()
                    .map(|c| Point2f64::new(c.x, c.y))
                    .collect();
                if pts.len() > 1 && pts.first() == pts.last() {
                    pts.pop();
                }
                pts
            })
            .collect()
    }

    /// The storey footprint as a single ring — the UNION of all floor regions'
    /// polygons (the inverse of `partition`: merge the phase zones back into one
    /// outline). Collinear vertices left where two zones' shared edge dissolved
    /// are stripped, so the rebuilt parametric outline doesn't gain phantom
    /// walls. Returns the largest resulting ring (a storey is normally one
    /// contiguous footprint); empty when there are no regions.
    pub fn merged_outline(&self) -> Vec<Point2f64> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        let mut acc: Option<MultiPolygon<f64>> = None;
        for r in &self.regions {
            if r.polygon.exterior().0.len() < 4 {
                continue;
            }
            acc = Some(match acc {
                None => MultiPolygon::new(vec![r.polygon.clone()]),
                Some(mp) => mp.union(&r.polygon),
            });
        }
        let Some(mp) = acc else {
            return Vec::new();
        };
        let Some(best) = mp.0.iter().max_by(|a, b| {
            a.unsigned_area()
                .partial_cmp(&b.unsigned_area())
                .unwrap_or(std::cmp::Ordering::Equal)
        }) else {
            return Vec::new();
        };
        let mut ring: Vec<Point2f64> = best
            .exterior()
            .coords()
            .map(|c| Point2f64::new(c.x, c.y))
            .collect();
        if ring.len() > 1 && ring.first() == ring.last() {
            ring.pop();
        }
        strip_collinear(&ring)
    }

    /// Group the floor regions by the construction PHASE their `construction_id`
    /// resolves to (via `phase_of`) and union each group's polygons, returning
    /// one ring per connected component per phase. A step (`level_m`) or a
    /// differing make-up splits the FLOOR into several regions, but they're
    /// still ONE phase — so a phase ZONE is the union of all same-phase floor
    /// area, not one zone per floor region. Deriving it per-region instead lets
    /// a stepped storey seed spurious phase boundaries onto the storey above
    /// (which has no step), splitting its floor where the phase is actually
    /// uniform. Rings are open (no repeated closing vertex) with collinear
    /// vertices stripped, matching `merged_outline`; disjoint same-phase area
    /// yields several rings (one per connected component). First-seen phase
    /// order is preserved for determinism.
    pub fn phase_zone_rings(
        &self,
        phase_of: impl Fn(ItemId) -> ConstructionPhase,
    ) -> Vec<(ConstructionPhase, Vec<Point2f64>)> {
        use geo::bool_ops::BooleanOps;
        let mut groups: Vec<(ConstructionPhase, MultiPolygon<f64>)> = Vec::new();
        for r in &self.regions {
            if r.polygon.exterior().0.len() < 4 {
                continue;
            }
            let phase = phase_of(r.construction_id);
            match groups.iter_mut().find(|(p, _)| *p == phase) {
                Some((_, mp)) => *mp = mp.union(&r.polygon),
                None => groups.push((phase, MultiPolygon::new(vec![r.polygon.clone()]))),
            }
        }
        let mut out = Vec::new();
        for (phase, mp) in groups {
            for poly in &mp.0 {
                let mut ring: Vec<Point2f64> = poly
                    .exterior()
                    .coords()
                    .map(|c| Point2f64::new(c.x, c.y))
                    .collect();
                if ring.len() > 1 && ring.first() == ring.last() {
                    ring.pop();
                }
                let ring = strip_collinear(&ring);
                if ring.len() >= 3 {
                    out.push((phase.clone(), ring));
                }
            }
        }
        out
    }

    /// Like [`phase_zone_rings`](Self::phase_zone_rings) but keeps the FULL
    /// `geo::Polygon` of each connected same-phase component — INCLUDING its
    /// interior holes. When a newer overlapping phase carves the middle out of an
    /// older one (the latest-wins partition → a donut), `phase_zone_rings` returns
    /// only the exterior; THIS keeps the hole so a display projection can tint /
    /// stroke the ring without filling the hole and place the label clear of it.
    /// One polygon per connected component per phase; first-seen phase order is
    /// preserved for determinism. A rebuilt outline isn't re-normalised (no
    /// collinear strip) — the extra edge vertices are invisible to a stroke.
    pub fn phase_zone_polygons(
        &self,
        phase_of: impl Fn(ItemId) -> ConstructionPhase,
    ) -> Vec<(ConstructionPhase, Polygon<f64>)> {
        use geo::bool_ops::BooleanOps;
        let mut groups: Vec<(ConstructionPhase, MultiPolygon<f64>)> = Vec::new();
        for r in &self.regions {
            if r.polygon.exterior().0.len() < 4 {
                continue;
            }
            let phase = phase_of(r.construction_id);
            match groups.iter_mut().find(|(p, _)| *p == phase) {
                Some((_, mp)) => *mp = mp.union(&r.polygon),
                None => groups.push((phase, MultiPolygon::new(vec![r.polygon.clone()]))),
            }
        }
        let mut out = Vec::new();
        for (phase, mp) in groups {
            for poly in mp.0 {
                if poly.exterior().0.len() >= 4 {
                    out.push((phase.clone(), poly));
                }
            }
        }
        out
    }

    /// EVERY boundary ring of the floor's true outline: the union of all region
    /// polygons, then each resulting component's exterior AND its interior-hole
    /// rings. Unlike [`merged_outline`](Self::merged_outline) (largest exterior
    /// only), this keeps DISJOINT footprints and light-wells — so the renderer's
    /// wall-vs-seam classifier sees every real perimeter edge, not just the main
    /// footprint's. Rings are open (no repeated closing vertex); collinear
    /// vertices are stripped so each ring edge is a maximal straight run (a
    /// region sub-edge lying along it then tests as contained). Empty when there
    /// are no ≥3-vertex regions.
    /// The PURELY GEOMETRIC division of the storey's floor for the height
    /// calculation: regions grouped by level (1 mm tolerance), each group
    /// unioned, and every connected piece of a union is one zone. Largest
    /// zone first — its plane is the natural 0 point for the storey-height
    /// working. Empty when there are no ≥3-vertex regions.
    pub fn level_zones(&self) -> Vec<FloorLevelZone> {
        use geo::bool_ops::BooleanOps;
        use geo::{Area, Centroid, InteriorPoint};
        let mut groups: Vec<(f64, MultiPolygon<f64>)> = Vec::new();
        for r in &self.regions {
            if r.polygon.exterior().0.len() < 4 {
                continue;
            }
            match groups
                .iter_mut()
                .find(|(l, _)| (*l - r.level_m).abs() < 1e-3)
            {
                Some((_, mp)) => *mp = mp.union(&r.polygon),
                None => groups.push((r.level_m, MultiPolygon::new(vec![r.polygon.clone()]))),
            }
        }
        let mut zones: Vec<FloorLevelZone> = groups
            .into_iter()
            .flat_map(|(level_m, mp)| {
                mp.0.into_iter().filter_map(move |polygon| {
                    let area_m2 = polygon.unsigned_area();
                    if area_m2 < 1e-4 {
                        return None; // boolean-op sliver
                    }
                    let a = polygon.interior_point().or_else(|| polygon.centroid())?;
                    Some(FloorLevelZone {
                        anchor: Point2f64::new(a.x(), a.y()),
                        polygon,
                        level_m,
                        area_m2,
                    })
                })
            })
            .collect();
        zones.sort_by(|a, b| b.area_m2.total_cmp(&a.area_m2));
        zones
    }

    pub fn boundary_rings(&self) -> Vec<Vec<Point2f64>> {
        use geo::bool_ops::BooleanOps;
        let mut acc: Option<MultiPolygon<f64>> = None;
        for r in &self.regions {
            if r.polygon.exterior().0.len() < 4 {
                continue;
            }
            acc = Some(match acc {
                None => MultiPolygon::new(vec![r.polygon.clone()]),
                Some(mp) => mp.union(&r.polygon),
            });
        }
        let Some(mp) = acc else {
            return Vec::new();
        };
        let collect = |ls: &LineString<f64>| -> Vec<Point2f64> {
            let mut pts: Vec<Point2f64> = ls.coords().map(|c| Point2f64::new(c.x, c.y)).collect();
            if pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            strip_collinear(&pts)
        };
        let mut rings = Vec::new();
        for poly in &mp.0 {
            let ext = collect(poly.exterior());
            if ext.len() >= 3 {
                rings.push(ext);
            }
            for hole in poly.interiors() {
                let h = collect(hole);
                if h.len() >= 3 {
                    rings.push(h);
                }
            }
        }
        rings
    }

    /// The storey footprint as ONE usable ring: when the floor is split into
    /// several phase zones, merge them back (`merged_outline`); otherwise the
    /// single region's exact stored points (no geo ops, so they round-trip).
    /// `None` when no region yields a ≥3-vertex ring.
    pub fn footprint_ring(&self) -> Option<Vec<Point2f64>> {
        let ring = if self.regions.len() > 1 {
            self.merged_outline()
        } else {
            self.to_vertex_lists()
                .into_iter()
                .find(|r| r.len() >= 3)
                .unwrap_or_default()
        };
        (ring.len() >= 3).then_some(ring)
    }

    /// Partition `outline` into floor regions by a set of construction-phase
    /// `areas` (each a ring + the construction id to stamp on it). Every area is
    /// clipped to the outline and to the parts NOT already claimed by an earlier
    /// area (so a later selection abuts — "collapses to" — an existing one rather
    /// than overlapping it); the leftover gets `remainder_construction_id`. A
    /// boolean op can split one area into several pieces (MultiPolygon), so this
    /// yields one `FloorRegion` per resulting piece. Empty `areas` → a single
    /// region for the whole outline (the common, untagged case). Reusable for
    /// ceilings later (same partition over the same rings).
    pub fn partition(
        outline: &[Point2f64],
        areas: &[(Vec<Point2f64>, ItemId)],
        remainder_construction_id: ItemId,
    ) -> Self {
        use geo::bool_ops::BooleanOps;
        if outline.len() < 3 {
            return Self::default();
        }
        let to_poly = |ring: &[Point2f64]| {
            Polygon::new(
                LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            )
        };
        let mk = |poly: &Polygon<f64>, cid: ItemId| FloorRegion {
            id: ItemId::default(),
            polygon: poly.clone(),
            construction_id: cid,
            level_m: 0.0,
            exposure: FloorExposure::default(),
            height_m: None,
        };
        let outline_poly = to_poly(outline);
        let mut regions: Vec<FloorRegion> = Vec::new();
        let mut claimed: MultiPolygon<f64> = MultiPolygon::new(vec![]);
        for (ring, cid) in areas {
            if ring.len() < 3 {
                continue;
            }
            let inside = outline_poly.intersection(&to_poly(ring));
            let piece = inside.difference(&claimed);
            for poly in &piece.0 {
                // Thickness-aware guard: a phase ring a hair off the outline
                // edge would otherwise shave a long-thin strip that clears
                // the area floor (field report 797a7ff5). Root fix keeps the
                // rings f64-coincident; this is defence in depth.
                if plan_piece_is_degenerate(poly) {
                    continue;
                }
                regions.push(mk(poly, *cid));
            }
            claimed = claimed.union(&piece);
        }
        let remainder = outline_poly.difference(&claimed);
        for poly in &remainder.0 {
            if plan_piece_is_degenerate(poly) {
                continue;
            }
            regions.push(mk(poly, remainder_construction_id));
        }
        Self { regions }
    }

    /// RE-UNION partition output: merge regions that reference the SAME
    /// construction (and share `level_m`/`exposure`) back into single regions.
    /// [`partition`](Self::partition) emits one region per input area piece with
    /// no re-union, so two ADJACENT areas that resolve to the same construction
    /// id — the "assign the rest to Original" flow pushing a leftover strip
    /// beside an already-Original area (bug report c78ebed1) — leave a redundant
    /// interior boundary between two regions that are one physical floor. A
    /// boundary only exists to separate DIFFERENT fabric; same construction on
    /// both sides means no boundary.
    ///
    /// Strictly conservative about geometry: a group is re-unioned ONLY when the
    /// union actually reduces its piece count (i.e. some pieces were adjacent /
    /// overlapping). Disjoint same-construction pieces — and every singleton
    /// group — pass through VERBATIM (no geo ops, stored points round-trip
    /// exactly). Merged rings get collinear vertices stripped (the dissolved
    /// seam's endpoints) on the exterior AND any interior hole rings; merged
    /// regions take a fresh id and the group's first-seen `height_m`. First-seen
    /// region order is preserved (merged pieces land at the group's first slot).
    pub fn merge_same_construction(self) -> Self {
        use geo::bool_ops::BooleanOps;
        // Group by full property identity — merging across a level or exposure
        // difference would erase a real division (a step, a mapped exposure).
        type Key = (ItemId, u64, FloorExposure);
        let key_of =
            |r: &FloorRegion| -> Key { (r.construction_id, r.level_m.to_bits(), r.exposure) };
        let mut groups: Vec<(Key, Vec<usize>)> = Vec::new();
        for (i, r) in self.regions.iter().enumerate() {
            let k = key_of(r);
            match groups.iter_mut().find(|(gk, _)| *gk == k) {
                Some((_, idxs)) => idxs.push(i),
                None => groups.push((k, vec![i])),
            }
        }
        // Merged pieces per group-leader index; None = keep members verbatim.
        let mut merged_at: Vec<Option<Vec<FloorRegion>>> = vec![None; self.regions.len()];
        let mut absorbed: Vec<bool> = vec![false; self.regions.len()];
        for (_, idxs) in &groups {
            if idxs.len() < 2 {
                continue;
            }
            let mut acc: Option<MultiPolygon<f64>> = None;
            for &i in idxs {
                let p = &self.regions[i].polygon;
                if p.exterior().0.len() < 4 {
                    continue;
                }
                acc = Some(match acc {
                    None => MultiPolygon::new(vec![p.clone()]),
                    Some(mp) => mp.union(p),
                });
            }
            let Some(mp) = acc else { continue };
            if mp.0.len() >= idxs.len() {
                continue; // all disjoint — keep every stored piece verbatim
            }
            let first = &self.regions[idxs[0]];
            let pieces: Vec<FloorRegion> =
                mp.0.iter()
                    .map(|poly| FloorRegion {
                        id: ItemId::default(),
                        polygon: strip_polygon_collinear(poly),
                        construction_id: first.construction_id,
                        level_m: first.level_m,
                        exposure: first.exposure,
                        height_m: first.height_m,
                    })
                    .collect();
            merged_at[idxs[0]] = Some(pieces);
            for &i in &idxs[1..] {
                absorbed[i] = true;
            }
        }
        let mut out: Vec<FloorRegion> = Vec::with_capacity(self.regions.len());
        for (i, r) in self.regions.into_iter().enumerate() {
            if absorbed[i] {
                continue;
            }
            match merged_at[i].take() {
                Some(pieces) => out.extend(pieces),
                None => out.push(r),
            }
        }
        Self { regions: out }
    }

    /// Re-partition the regions by step `areas` (ring + floor level in metres
    /// from the storey datum; + raised, − lowered): each region is split along
    /// the area boundaries, intersected pieces take the area's level, the
    /// remainders keep their existing level. Earlier areas claim first where
    /// areas overlap (same rule as `partition`). Empty `areas` → `self`
    /// unchanged (NO geo ops, so stored points round-trip exactly).
    ///
    /// Sub-mm² slivers are dropped: a step ring reconstructed on reload has
    /// round-tripped through `f32`, so it no longer coincides exactly with a
    /// phase boundary it once abutted — the boolean ops would otherwise shave a
    /// degenerate piece and persist it as a junk region.
    pub fn with_levels(self, areas: &[(Vec<Point2f64>, f64)]) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        /// Pieces smaller than this (m²) are boolean-op debris, not floors.
        const SLIVER_AREA_M2: f64 = 1e-6;
        if areas.is_empty() {
            return self;
        }
        let to_poly = |ring: &[Point2f64]| {
            Polygon::new(
                LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            )
        };
        let mut regions: Vec<FloorRegion> = Vec::new();
        for region in self.regions {
            let mk = |poly: &Polygon<f64>, level_m: f64| FloorRegion {
                id: ItemId::default(),
                polygon: poly.clone(),
                construction_id: region.construction_id,
                level_m,
                exposure: region.exposure,
                height_m: region.height_m,
            };
            let mut claimed: MultiPolygon<f64> = MultiPolygon::new(vec![]);
            for (ring, level) in areas {
                if ring.len() < 3 {
                    continue;
                }
                let inside = region.polygon.intersection(&to_poly(ring));
                let piece = inside.difference(&claimed);
                for poly in &piece.0 {
                    if poly.unsigned_area() < SLIVER_AREA_M2 {
                        continue;
                    }
                    regions.push(mk(poly, *level));
                }
                claimed = claimed.union(&piece);
            }
            let remainder = region.polygon.difference(&claimed);
            for poly in &remainder.0 {
                if poly.unsigned_area() < SLIVER_AREA_M2 {
                    continue;
                }
                regions.push(mk(poly, region.level_m));
            }
        }
        Self { regions }
    }

    /// Divide the regions by structure-phase `cuts` (each a closed plan ring):
    /// every region the cut crosses splits into an inside piece and an outside
    /// piece, and — unlike [`partition`](Self::partition) / [`with_levels`](Self::with_levels),
    /// which stamp a value on the intersected side — BOTH pieces keep ALL of the
    /// source region's properties (`construction_id` AND `level_m`); only the id
    /// is fresh. Cuts apply in order, each further subdividing the pieces left by
    /// the earlier ones. Empty `cuts` → `self` unchanged (NO geo ops, so stored
    /// points round-trip exactly). Sub-mm² slivers are dropped (boolean-op debris
    /// from an `f32`-round-tripped ring no longer coinciding exactly).
    pub fn with_splits(self, cuts: &[Vec<Point2f64>]) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        /// Pieces smaller than this (m²) are boolean-op debris, not floors.
        const SLIVER_AREA_M2: f64 = 1e-6;
        if cuts.is_empty() {
            return self;
        }
        let to_poly = |ring: &[Point2f64]| {
            Polygon::new(
                LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                vec![],
            )
        };
        let mut regions = self.regions;
        for cut in cuts {
            if cut.len() < 3 {
                continue;
            }
            let cut_poly = to_poly(cut);
            let mut next: Vec<FloorRegion> = Vec::new();
            for region in regions {
                let mk = |poly: &Polygon<f64>| FloorRegion {
                    id: ItemId::default(),
                    polygon: poly.clone(),
                    construction_id: region.construction_id,
                    level_m: region.level_m,
                    exposure: region.exposure,
                    height_m: region.height_m,
                };
                // inside ∪ outside == the whole region, so a region that the cut
                // misses (or fully covers) emits its whole self re-id'd — never
                // lost. A genuine crossing emits both sides with copied props.
                let inside = region.polygon.intersection(&cut_poly);
                for poly in &inside.0 {
                    if poly.unsigned_area() >= SLIVER_AREA_M2 {
                        next.push(mk(poly));
                    }
                }
                let outside = region.polygon.difference(&cut_poly);
                for poly in &outside.0 {
                    if poly.unsigned_area() >= SLIVER_AREA_M2 {
                        next.push(mk(poly));
                    }
                }
            }
            regions = next;
        }
        Self { regions }
    }

    /// Interior FLOOR-RISER faces: for every plan edge piece shared by two
    /// regions whose `level_m` differ (> [`RISER_Z_EPS_M`]), the vertical
    /// quad from the lower to the higher level. `exterior[0]`/`[1]` are the
    /// base edge at the LOWER level (the vertex-order contract `WallRegion`
    /// readers rely on). Outline edges never emit: a piece must be shared by
    /// TWO regions, and the regions tile the footprint with disjoint
    /// interiors. Shared pieces are recovered by METRIC collinearity
    /// ([`shared_plan_pieces`]) — the rings are boolean-op output, so a
    /// conceptually shared edge is never an exact vertex match. Interior
    /// rings (a step strictly inside a region leaves the remainder with a
    /// hole) participate like exteriors. A face that comes out a coplanar
    /// FLUSH continuation of a wall region does not survive as its own
    /// region: `WallRegions::with_risers` merges it into that wall
    /// (`merge_coplanar_flush`).
    pub fn riser_faces(&self) -> Vec<Polygon3D> {
        let to_pts = |ls: &LineString<f64>| {
            let mut pts: Vec<Point2f64> = ls.coords().map(|c| Point2f64::new(c.x, c.y)).collect();
            if pts.len() > 1 && pts.first() == pts.last() {
                pts.pop();
            }
            pts
        };
        let rings_of = |r: &FloorRegion| -> Vec<Vec<Point2f64>> {
            std::iter::once(to_pts(r.polygon.exterior()))
                .chain(r.polygon.interiors().iter().map(to_pts))
                .collect()
        };
        let rings: Vec<Vec<Vec<Point2f64>>> = self.regions.iter().map(rings_of).collect();
        let mut out = Vec::new();
        for i in 0..self.regions.len() {
            for j in (i + 1)..self.regions.len() {
                let (zi, zj) = (self.regions[i].level_m, self.regions[j].level_m);
                if (zi - zj).abs() <= RISER_Z_EPS_M {
                    continue; // same level — a phase split, not a step
                }
                let (lo, hi) = (zi.min(zj), zi.max(zj));
                for ring_i in &rings[i] {
                    for ring_j in &rings[j] {
                        for (p, q) in shared_plan_pieces(ring_i, ring_j) {
                            out.extend(riser_face(p, q, lo, hi, lo, hi));
                        }
                    }
                }
            }
        }
        out
    }
}

/// Strip collinear vertices from every ring of a boolean-op output polygon —
/// the exterior AND any interior holes — so a dissolved seam's endpoints don't
/// survive as phantom vertices. Ring order/orientation is preserved.
fn strip_polygon_collinear(poly: &Polygon<f64>) -> Polygon<f64> {
    let clean = |ls: &LineString<f64>| -> LineString<f64> {
        let mut pts: Vec<Point2f64> = ls.coords().map(|c| Point2f64::new(c.x, c.y)).collect();
        if pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        let pts = strip_collinear(&pts);
        LineString::from(pts.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>())
    };
    Polygon::new(
        clean(poly.exterior()),
        poly.interiors().iter().map(clean).collect(),
    )
}

/// Union same-phase session rings into canonical per-phase rings — the input
/// the CUT consumers (the resolve's ceiling phase-carve and phase wall-splits)
/// should see. Two session areas of ONE phase drawn adjacent (the drawn area +
/// the "assign the rest" leftover, bug report c78ebed1) carry their shared
/// boundary as an edge of each ring; fed raw to a cut pass, that boundary
/// splits walls/ceilings between two SAME-phase zones — a division that
/// separates nothing. The union dissolves exactly those internal edges while
/// keeping every real boundary (against another phase, the untagged remainder,
/// or the outline).
///
/// CUT semantics only — deliberately NOT for the floor `partition` feed: the
/// partition's latest-tag-wins overlap ordering (`.rev()` at the call site)
/// depends on per-tag entries, and collapsing same-phase tags around a
/// different-phase tag can flip an overlap's owner. Cut edges have no such
/// ordering (a cut either exists or not), and an edge internal to a same-phase
/// union can never be a final zone boundary — zones are per-phase unions.
///
/// Conservative like [`FloorRegions::merge_same_construction`]: a phase's rings
/// are replaced by union output ONLY when the union reduces the ring count;
/// singleton phases and fully-disjoint groups pass through VERBATIM (no geo
/// ops, bit-identical rings — the reconstructed-on-load zones are already one
/// ring per phase, so reload paths are untouched). Union output emits the
/// exterior AND any interior hole rings (a hole boundary is a real cut against
/// whatever the hole contains), open (no closing duplicate) with collinear
/// vertices stripped, matching [`FloorRegions::phase_zone_rings`].
pub fn merge_same_phase_rings(
    rings: &[(Vec<Point2f64>, ConstructionPhase)],
) -> Vec<(Vec<Point2f64>, ConstructionPhase)> {
    use geo::bool_ops::BooleanOps;
    let mut groups: Vec<(ConstructionPhase, Vec<usize>)> = Vec::new();
    for (i, (_, phase)) in rings.iter().enumerate() {
        match groups.iter_mut().find(|(p, _)| p == phase) {
            Some((_, idxs)) => idxs.push(i),
            None => groups.push((phase.clone(), vec![i])),
        }
    }
    let to_poly = |ring: &[Point2f64]| {
        Polygon::new(
            LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
            vec![],
        )
    };
    let open_ring = |ls: &LineString<f64>| -> Vec<Point2f64> {
        let mut pts: Vec<Point2f64> = ls.coords().map(|c| Point2f64::new(c.x, c.y)).collect();
        if pts.len() > 1 && pts.first() == pts.last() {
            pts.pop();
        }
        strip_collinear(&pts)
    };
    let mut out: Vec<(Vec<Point2f64>, ConstructionPhase)> = Vec::new();
    for (phase, idxs) in groups {
        let usable: Vec<usize> = idxs
            .iter()
            .copied()
            .filter(|&i| rings[i].0.len() >= 3)
            .collect();
        if usable.len() < 2 {
            for &i in &idxs {
                out.push(rings[i].clone()); // verbatim — nothing to union
            }
            continue;
        }
        let mut acc: Option<MultiPolygon<f64>> = None;
        for &i in &usable {
            let p = to_poly(&rings[i].0);
            acc = Some(match acc {
                None => MultiPolygon::new(vec![p]),
                Some(mp) => mp.union(&p),
            });
        }
        let mp = acc.expect("usable.len() >= 2");
        if mp.0.len() >= usable.len() {
            // Fully disjoint — keep the stored rings verbatim (bit-identical).
            for &i in &idxs {
                out.push(rings[i].clone());
            }
            continue;
        }
        for poly in &mp.0 {
            let ext = open_ring(poly.exterior());
            if ext.len() >= 3 {
                out.push((ext, phase.clone()));
            }
            for hole in poly.interiors() {
                let h = open_ring(hole);
                if h.len() >= 3 {
                    out.push((h, phase.clone()));
                }
            }
        }
        // Degenerate (< 3-vertex) members of a merged group are dropped with it.
    }
    out
}

impl Default for FloorRegion {
    fn default() -> Self {
        Self {
            id: ItemId::default(),
            polygon: Polygon::new(LineString::new(vec![]), vec![]),
            construction_id: ItemId::default(),
            level_m: 0.0,
            exposure: FloorExposure::default(),
            height_m: None,
        }
    }
}

#[cfg(test)]
mod boundary_overlap_tests {
    use super::*;
    use geo::{Coord, LineString};

    fn region(ring: &[(f64, f64)]) -> FloorRegion {
        let coords: Vec<Coord<f64>> = ring.iter().map(|&(x, y)| Coord { x, y }).collect();
        FloorRegion {
            polygon: Polygon::new(LineString::from(coords), vec![]),
            ..Default::default()
        }
    }
    fn seg(a: (f64, f64), b: (f64, f64)) -> (Point2f64, Point2f64) {
        (Point2f64::new(a.0, a.1), Point2f64::new(b.0, b.1))
    }

    /// A 10×6 rectangle: full-edge, partial, noisy-but-on, offset-off,
    /// collinear-but-elsewhere, and spanning-two-edges cases.
    #[test]
    fn overlaps_are_tolerant_and_bounded() {
        let r = region(&[(0., 0.), (10., 0.), (10., 6.), (0., 6.)]);
        let out = r.boundary_overlaps(&[
            seg((0., 0.), (10., 0.)),       // exactly the bottom edge
            seg((2., 0.), (7., 0.)),        // part of the bottom edge
            seg((0., 0.004), (10., 0.004)), // 4 mm derivation noise: counts
            seg((0., 0.5), (10., 0.5)),     // half a metre inside: doesn't
            seg((12., 0.), (15., 0.)),      // collinear but past the corner
            seg((10., -2.), (10., 3.)),     // spans past the corner: clipped
        ]);
        let exp = [10.0, 5.0, 10.0, 0.0, 0.0, 3.0];
        for (i, (got, want)) in out.iter().zip(exp).enumerate() {
            assert!(
                (got - want).abs() < 1e-6,
                "case {i}: got {got}, want {want}"
            );
        }
    }

    /// Consecutive collinear ring vertices must not double-count a wall.
    #[test]
    fn collinear_ring_edges_do_not_double_count() {
        let r = region(&[(0., 0.), (5., 0.), (10., 0.), (10., 6.), (0., 6.)]);
        let out = r.boundary_overlaps(&[seg((0., 0.), (10., 0.))]);
        assert!((out[0] - 10.0).abs() < 1e-6, "got {}", out[0]);
    }
}

#[cfg(test)]
mod same_construction_merge_tests {
    use super::*;

    fn rect(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<Point2f64> {
        vec![
            Point2f64::new(x0, y0),
            Point2f64::new(x1, y0),
            Point2f64::new(x1, y1),
            Point2f64::new(x0, y1),
        ]
    }

    /// Two ADJACENT partition pieces with the SAME construction id (the
    /// "assign the rest to Original" leftover beside an already-Original area,
    /// bug c78ebed1) merge into ONE region with the dissolved seam's collinear
    /// vertices stripped.
    #[test]
    fn adjacent_same_construction_pieces_merge_into_one() {
        let cid = ItemId::default();
        let mut floors = FloorRegions::from_vertex_lists(vec![rect(0.0, 0.0, 5.0, 4.0)], cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![rect(5.0, 0.0, 8.0, 4.0)], cid).regions);
        let merged = floors.merge_same_construction();
        assert_eq!(merged.regions.len(), 1, "one physical floor, one region");
        assert_eq!(
            merged.to_vertex_lists()[0].len(),
            4,
            "the x=5 seam dissolved and its collinear endpoints were stripped"
        );
        assert_eq!(merged.regions[0].construction_id, cid);
    }

    /// DIFFERENT construction ids never merge — and their stored points pass
    /// through verbatim (no geo ops on the untouched groups).
    #[test]
    fn different_constructions_stay_split_verbatim() {
        let (cid_a, cid_b) = (ItemId::default(), ItemId::default());
        let mut floors = FloorRegions::from_vertex_lists(vec![rect(0.0, 0.0, 5.0, 4.0)], cid_a);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![rect(5.0, 0.0, 8.0, 4.0)], cid_b).regions);
        let before = floors.to_vertex_lists();
        let merged = floors.merge_same_construction();
        assert_eq!(merged.regions.len(), 2, "a real phase boundary stays");
        assert_eq!(merged.to_vertex_lists(), before, "points bit-identical");
    }

    /// DISJOINT same-construction pieces (a genuinely split footprint) stay two
    /// regions with bit-identical points — the union is only accepted when it
    /// actually reduces the piece count.
    #[test]
    fn disjoint_same_construction_pieces_stay_verbatim() {
        let cid = ItemId::default();
        let mut floors = FloorRegions::from_vertex_lists(vec![rect(0.0, 0.0, 3.0, 4.0)], cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![rect(5.0, 0.0, 8.0, 4.0)], cid).regions);
        let before = floors.to_vertex_lists();
        let merged = floors.merge_same_construction();
        assert_eq!(merged.regions.len(), 2);
        assert_eq!(merged.to_vertex_lists(), before, "points bit-identical");
    }

    /// A `level_m` difference is a REAL division (a step) even under one
    /// construction id — never merged.
    #[test]
    fn level_difference_blocks_the_merge() {
        let cid = ItemId::default();
        let mut floors = FloorRegions::from_vertex_lists(vec![rect(0.0, 0.0, 5.0, 4.0)], cid);
        floors
            .regions
            .extend(FloorRegions::from_vertex_lists(vec![rect(5.0, 0.0, 8.0, 4.0)], cid).regions);
        floors.regions[1].level_m = -0.3;
        assert_eq!(floors.merge_same_construction().regions.len(), 2);
    }

    /// `merge_same_phase_rings`: two ADJACENT same-phase rings union into one
    /// canonical ring (seam edges gone); disjoint same-phase rings and
    /// different-phase singletons pass through verbatim.
    #[test]
    fn same_phase_rings_union_only_when_adjacent() {
        // Adjacent Original pair → one 4-corner ring.
        let merged = merge_same_phase_rings(&[
            (rect(0.0, 0.0, 5.0, 4.0), ConstructionPhase::Original),
            (rect(5.0, 0.0, 8.0, 4.0), ConstructionPhase::Original),
        ]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].1, ConstructionPhase::Original);
        assert_eq!(merged[0].0.len(), 4, "seam dissolved, collinear stripped");

        // Disjoint Original pair → verbatim (bit-identical rings).
        let input = vec![
            (rect(0.0, 0.0, 3.0, 4.0), ConstructionPhase::Original),
            (rect(5.0, 0.0, 8.0, 4.0), ConstructionPhase::Original),
        ];
        assert_eq!(merge_same_phase_rings(&input), input, "disjoint verbatim");

        // Mixed phases, one ring each → verbatim in first-seen order.
        let input = vec![
            (rect(0.0, 0.0, 4.0, 4.0), ConstructionPhase::Original),
            (rect(4.0, 0.0, 8.0, 4.0), ConstructionPhase::Extension1),
        ];
        assert_eq!(merge_same_phase_rings(&input), input, "singletons verbatim");
    }
}

/// One contiguous same-level floor zone — see [`FloorRegions::level_zones`].
#[derive(Clone, Debug)]
pub struct FloorLevelZone {
    pub polygon: Polygon<f64>,
    /// Level relative to the storey's floor datum (m).
    pub level_m: f64,
    pub area_m2: f64,
    /// A point inside the zone — the label / datum-marker anchor.
    pub anchor: Point2f64,
}
