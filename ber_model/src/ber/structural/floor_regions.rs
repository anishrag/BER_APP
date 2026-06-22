use geo::{LineString, MultiPolygon, Polygon};
use serde::{Deserialize, Serialize};

use crate::ber::common::ItemId;
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
}

impl FloorRegion {
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
                regions.push(mk(poly, *cid));
            }
            claimed = claimed.union(&piece);
        }
        let remainder = outline_poly.difference(&claimed);
        for poly in &remainder.0 {
            regions.push(mk(poly, remainder_construction_id));
        }
        Self { regions }
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

impl Default for FloorRegion {
    fn default() -> Self {
        Self {
            id: ItemId::default(),
            polygon: Polygon::new(LineString::new(vec![]), vec![]),
            construction_id: ItemId::default(),
            level_m: 0.0,
        }
    }
}
