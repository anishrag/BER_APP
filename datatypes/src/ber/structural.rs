// src/data/structural.rs
use geo::{Contains, Coord, LineString, MultiPolygon, Point as GeoPoint, Polygon};
use serde::{Deserialize, Serialize};

use super::ber_struct::Ber;
use super::common::{ConstructionPhase, Item, ItemId, Photo};
use super::fenestration::{Door, Window};

use domain::points::{point_to_segment_distance, Point2f64, Point3f64, Vector3f64};
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Opes {
    pub doors: Vec<ItemId>,   // references to Door IDs
    pub windows: Vec<ItemId>, // references to Window IDs
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConstructionMaterial {
    Concrete,
    InsulationEPS,
    Plasterboard,
    Timber,
    Custom {
        lambda: f32, // W/m·K
        name: String,
    },
}

impl ConstructionMaterial {
    pub fn lambda(&self) -> f32 {
        match self {
            ConstructionMaterial::Concrete => 1.7,
            ConstructionMaterial::InsulationEPS => 0.035,
            ConstructionMaterial::Plasterboard => 0.25,
            ConstructionMaterial::Timber => 0.13,
            ConstructionMaterial::Custom { lambda, .. } => *lambda,
        }
    }

    pub fn name(&self) -> String {
        match self {
            ConstructionMaterial::Concrete => "Concrete".into(),
            ConstructionMaterial::InsulationEPS => "EPS Insulation".into(),
            ConstructionMaterial::Plasterboard => "Plasterboard".into(),
            ConstructionMaterial::Timber => "Timber".into(),
            ConstructionMaterial::Custom { name, .. } => name.clone(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConstructionLayer {
    pub thickness_mm: u32,              // thickness in meters
    pub material: ConstructionMaterial, // material name or ID
    pub bridged: bool,                  // true if thermal bridging exists
}
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConstructionMakeup {
    NonHeatLoss, // no heat loss
    Default,     // use default construction
    #[default]
    Tbd, // to be determined
    Layers(Vec<ConstructionLayer>), // custom stack of layers
}

/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FloorConstructionType {
    #[default]
    SolidFloor,
    SuspendedFloor,
    ExposedSemiExposedFloor,
    NonHeatLossFloor,
    Uninitialised,
    Other,
}

/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CeilingConstructionType {
    FlatRoof,
    SlopedInsOnRafter,
    SlopedInsOnCeiling,
    SlopedInsOnCeilingAndRafter,
    NonHeatLossCeiling,
    #[default]
    Uninitialised,
    Other,
}

/// Wall construction types
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WallConstructionType {
    CavityWall,
    FilledCavityWall,
    ConcreteHollowBlocks,
    TimberConstruction,
    ICF,
    Floor,
    #[default]
    Uninitialised,
}

/// Thermal exposure classification
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WallExposure {
    Exposed,     // Exterior wall exposed to outside
    SemiExposed, // Wall to unheated space
    NonHeatLoss, // Internal wall, no heat loss
    /// No exposure marked yet. Walls exist (and draw) Unset; DEAP export will
    /// refuse a BER containing Unset regions — that restriction lives at the
    /// future export boundary (the deap mapping layer), not here.
    Unset,
    /// In contact with the ground (floor-step risers; DEAP treats
    /// ground-contact walls separately from Exposed).
    Ground,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WallData {
    pub construction_type: WallConstructionType,
    pub exposure: WallExposure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoofData {
    pub construction_type: CeilingConstructionType,
}

/// Shared construction identity + fabric properties. A handful of these exist
/// per dwelling (e.g. "300mm filled cavity, original"); many regions reference
/// one by `construction_id`. Geometry (polygon, exposure, openings) lives on the
/// *region*, not here — so splitting a wall into exposure sections, or reusing
/// the same make-up on several storeys, keeps a single shared construction.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct ConstructionBase {
    pub id: ItemId,
    pub construction_makeup: ConstructionMakeup,
    pub construction_phase: ConstructionPhase,
    pub photos: Vec<Photo>,
    pub description: Option<String>,
}

impl ConstructionBase {
    pub fn clone_with_new_id(&self) -> Self {
        Self {
            id: ItemId::default(),
            ..self.clone() // use regular clone for the rest
        }
    }
}

/// A wall construction shared by one or more `WallRegion`s.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct WallConstruction {
    pub base: ConstructionBase,
    pub construction_type: WallConstructionType,
}

/// A floor construction shared by one or more `FloorRegion`s.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FloorConstruction {
    pub base: ConstructionBase,
    pub construction_type: FloorConstructionType,
}

/// A ceiling/roof construction shared by one or more `CeilingRegion`s.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CeilingConstruction {
    pub base: ConstructionBase,
    pub construction_type: CeilingConstructionType,
}

/// House-level construction registry. Regions reference a construction by id;
/// these find-or-create it so many regions share one entry instead of
/// duplicating fabric (the inverse — grouping for DEAP export — then just reads
/// back the shared constructions).
impl Ber {
    /// Read-only lookup of an existing wall construction by `(type, makeup,
    /// phase)` — the side-effect-free half of [`ensure_wall_construction`]. For
    /// the geometry projection, which must not mutate the construction registry.
    pub fn wall_construction_id(
        &self,
        construction_type: WallConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) -> Option<ItemId> {
        self.wall_constructions
            .iter()
            .find(|c| {
                c.construction_type == construction_type
                    && c.base.construction_makeup == makeup
                    && c.base.construction_phase == phase
            })
            .map(|c| c.base.id)
    }

    /// Read-only lookup of an existing floor construction by `(type, makeup,
    /// phase)` — the side-effect-free half of [`ensure_floor_construction`].
    pub fn floor_construction_id(
        &self,
        construction_type: FloorConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) -> Option<ItemId> {
        self.floor_constructions
            .iter()
            .find(|c| {
                c.construction_type == construction_type
                    && c.base.construction_makeup == makeup
                    && c.base.construction_phase == phase
            })
            .map(|c| c.base.id)
    }

    /// Read-only lookup of an existing ceiling construction by `(type, makeup,
    /// phase)` — the side-effect-free half of [`ensure_ceiling_construction`].
    pub fn ceiling_construction_id(
        &self,
        construction_type: CeilingConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) -> Option<ItemId> {
        self.ceiling_constructions
            .iter()
            .find(|c| {
                c.construction_type == construction_type
                    && c.base.construction_makeup == makeup
                    && c.base.construction_phase == phase
            })
            .map(|c| c.base.id)
    }

    /// Find a wall construction matching `(construction_type, makeup, phase)`,
    /// else create and append one. Returns its id.
    pub fn ensure_wall_construction(
        &mut self,
        construction_type: WallConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) -> ItemId {
        if let Some(c) = self.wall_constructions.iter().find(|c| {
            c.construction_type == construction_type
                && c.base.construction_makeup == makeup
                && c.base.construction_phase == phase
        }) {
            return c.base.id;
        }
        let c = WallConstruction {
            base: ConstructionBase {
                construction_makeup: makeup,
                construction_phase: phase,
                ..Default::default()
            },
            construction_type,
        };
        let id = c.base.id;
        self.wall_constructions.push(c);
        id
    }

    /// Find a floor construction matching `(construction_type, makeup, phase)`,
    /// else create and append one. Returns its id.
    pub fn ensure_floor_construction(
        &mut self,
        construction_type: FloorConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) -> ItemId {
        if let Some(c) = self.floor_constructions.iter().find(|c| {
            c.construction_type == construction_type
                && c.base.construction_makeup == makeup
                && c.base.construction_phase == phase
        }) {
            return c.base.id;
        }
        let c = FloorConstruction {
            base: ConstructionBase {
                construction_makeup: makeup,
                construction_phase: phase,
                ..Default::default()
            },
            construction_type,
        };
        let id = c.base.id;
        self.floor_constructions.push(c);
        id
    }

    /// Find a ceiling construction matching `(construction_type, makeup, phase)`,
    /// else create and append one. Returns its id.
    pub fn ensure_ceiling_construction(
        &mut self,
        construction_type: CeilingConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
    ) -> ItemId {
        if let Some(c) = self.ceiling_constructions.iter().find(|c| {
            c.construction_type == construction_type
                && c.base.construction_makeup == makeup
                && c.base.construction_phase == phase
        }) {
            return c.base.id;
        }
        let c = CeilingConstruction {
            base: ConstructionBase {
                construction_makeup: makeup,
                construction_phase: phase,
                ..Default::default()
            },
            construction_type,
        };
        let id = c.base.id;
        self.ceiling_constructions.push(c);
        id
    }

    /// Resolve a wall construction by id (the inverse of `ensure_*`).
    pub fn wall_construction(&self, id: ItemId) -> Option<&WallConstruction> {
        self.wall_constructions.iter().find(|c| c.base.id == id)
    }

    /// Resolve a floor construction by id.
    pub fn floor_construction(&self, id: ItemId) -> Option<&FloorConstruction> {
        self.floor_constructions.iter().find(|c| c.base.id == id)
    }

    /// Resolve a ceiling construction by id.
    pub fn ceiling_construction(&self, id: ItemId) -> Option<&CeilingConstruction> {
        self.ceiling_constructions.iter().find(|c| c.base.id == id)
    }
}
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

    /// The region whose polygon contains `p`, used to inherit a floor's fabric
    /// (incl. construction phase) when re-deriving geometry. `None` if `p` is in
    /// no region.
    pub fn region_containing(&self, p: Point2f64) -> Option<&FloorRegion> {
        self.regions
            .iter()
            .find(|r| r.polygon.contains(&GeoPoint::new(p.x, p.y)))
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
    /// hole) participate like exteriors.
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

/// Drop vertices that lie on the straight line between their neighbours (within
/// a small cross-product tolerance) — used to clean a merged outline ring.
fn strip_collinear(ring: &[Point2f64]) -> Vec<Point2f64> {
    let n = ring.len();
    if n < 3 {
        return ring.to_vec();
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let cur = ring[i];
        let next = ring[(i + 1) % n];
        let cross = (cur.x - prev.x) * (next.y - prev.y) - (cur.y - prev.y) * (next.x - prev.x);
        if cross.abs() > 1e-9 {
            out.push(cur);
        }
    }
    if out.len() >= 3 {
        out
    } else {
        ring.to_vec()
    }
}

impl FloorRegion {
    /// A representative point of the region — the centroid of its exterior
    /// vertices. Used to hit-test which construction-phase area a floor falls in.
    /// (For a convex/whole-storey floor this lies inside; concave floors may need
    /// splitting before per-part phase assignment — deferred.)
    pub fn representative_point(&self) -> Point2f64 {
        let mut coords: Vec<Coord<f64>> = self.polygon.exterior().coords().copied().collect();
        if coords.len() > 1 && coords.first() == coords.last() {
            coords.pop();
        }
        if coords.is_empty() {
            return Point2f64::new(0.0, 0.0);
        }
        let (sx, sy) = coords
            .iter()
            .fold((0.0, 0.0), |(ax, ay), c| (ax + c.x, ay + c.y));
        let n = coords.len() as f64;
        Point2f64::new(sx / n, sy / n)
    }
}
/// A declared roof high point: fan the ceiling region containing `point` up
/// (or down) to `z_m`. Durable INPUT, like `Storey::ceiling_height_m` — a fan
/// apex is not cleanly invertible from triangulated geometry, so the
/// declarative form is stored and resolve re-applies it on every materialize
/// (composition order is law: default ceiling → step areas → apexes).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoofApex {
    /// Plan position of the apex, in storey/world metres. Must be
    /// f32-representable (the reload reconstruction round-trips rings through
    /// f32, and the fan's exact vertex matching depends on it) —
    /// `Ber::add_roof_apex` quantizes, so go through it.
    pub point: Point2f64,
    /// ABSOLUTE height of the apex in metres above the storey's floor datum
    /// ("ridge at 4.2 m"), NOT a delta against the region it lands in.
    pub z_m: f64,
}

/// Ceiling pieces and boolean debris with plan area below this (m²) are
/// slivers, not ceilings. ONE shared constant for the ceiling re-split
/// ([`CeilingRegions::with_z_deltas`]), the fan/ridge triangulations
/// ([`CeilingRegions::with_apexes`]), and the group merge
/// ([`CeilingRegions::apex_groups`]) — the re-apply identities depend on all
/// three drawing the line in the same place.
const SLIVER_AREA_M2: f64 = 1e-6;

/// A connected GROUP of roof apexes and the ceiling regions they shape — the
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
pub struct RoofApexGroup {
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
    /// sloped regions are the later roofs feature.
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

    /// Shape the regions by roof `apexes` in ONE pass (the last step of the
    /// composition: default ceiling → step areas → apexes), grouping the
    /// apexes PER REGION by boundary-INCLUSIVE containment (`geo`
    /// `Intersects`, not `Contains`): a re-applied apex is a CORNER of its
    /// own fan/ridge triangles and must still find them so re-application is
    /// the geometric identity (assign-time validation deliberately uses the
    /// strict predicate instead — [`region_strictly_containing`]; two
    /// different predicates by design). Per region:
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
    /// - **≥3** → warn + ridge from the FIRST TWO in apex-list order
    ///   (deterministic, never a panic — resolve must always produce
    ///   geometry; assign-time validation rejects this up front).
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
    pub fn with_apexes(self, apexes: &[RoofApex]) -> Self {
        use geo::Intersects;
        if apexes.is_empty() {
            return self;
        }
        let mut used = vec![false; apexes.len()];
        let mut next: Vec<CeilingRegion> = Vec::new();
        for region in self.regions {
            if region.polygon.exterior.len() < 3 {
                next.push(region);
                continue;
            }
            let plan: Polygon<f64> = region.polygon.clone().into();
            let contained: Vec<usize> = apexes
                .iter()
                .enumerate()
                .filter(|(_, a)| plan.intersects(&GeoPoint::new(a.point.x, a.point.y)))
                .map(|(i, _)| i)
                .collect();
            for &i in &contained {
                used[i] = true;
            }
            match contained.len() {
                0 => next.push(region),
                1 => next.extend(merge_coplanar_faces(fan_triangles(
                    region,
                    &apexes[contained[0]],
                ))),
                n => {
                    if n >= 3 {
                        log::warn!(
                            "ceiling region contains {n} roof apexes — \
                             ridging from the first two in apex-list order"
                        );
                    }
                    next.extend(merge_coplanar_faces(ridge_triangles(
                        region,
                        &apexes[contained[0]],
                        &apexes[contained[1]],
                    )));
                }
            }
        }
        for (i, hit) in used.iter().enumerate() {
            if !hit {
                log::warn!(
                    "roof apex at ({:.3}, {:.3}) lies in no ceiling region — skipped",
                    apexes[i].point.x,
                    apexes[i].point.y
                );
            }
        }
        Self { regions: next }
    }

    /// Group `apexes` with the regions they shape — THE grouping authority
    /// (see [`RoofApexGroup`]). Incidence is quantized plan-corner identity
    /// (a region vertex within 1e-6 m of the apex point — the
    /// [`fan_base_z`] predicate), closed transitively over shared regions
    /// (a ridge's transition triangles carry both endpoints, linking A to
    /// B). Apexes incident to no region join no group (e.g. mid-edit, or a
    /// dormant apex outside every ring). Groups come out ordered by their
    /// smallest apex index; `base_z` and the warn-excluded merge are per
    /// the [`RoofApexGroup`] field docs.
    ///
    /// [`fan_base_z`]: CeilingRegion::fan_base_z
    pub fn apex_groups(&self, apexes: &[RoofApex]) -> Vec<RoofApexGroup> {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        const EPS_M: f64 = 1e-6;
        if apexes.is_empty() {
            return Vec::new();
        }
        // Region → incident apex indices, by quantized plan-corner identity.
        let matched: Vec<Vec<usize>> = self
            .regions
            .iter()
            .map(|r| {
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
                for &ri in &region_idxs {
                    let r = &self.regions[ri];
                    match (r.fan_base_z(apexes), base_z) {
                        (Some(z), None) => {
                            base_z = Some(z);
                            merge.push(r);
                        }
                        (Some(z), Some(b)) if (z - b).abs() <= EPS_M => merge.push(r),
                        (got, _) => log::warn!(
                            "roof apex group: region base z {got:?} disagrees with the \
                             group's base {base_z:?} — excluded from the merged ring"
                        ),
                    }
                }
                if base_z.is_none() {
                    log::warn!("roof apex group has no level-based member region — base_z = 0");
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
                RoofApexGroup {
                    apex_idxs,
                    region_idxs,
                    base_z: base_z.unwrap_or(0.0),
                    merged_rings,
                }
            })
            .collect()
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

    /// Interior CEILING-RISER faces: for every plan edge piece shared by two
    /// regions ([`shared_plan_pieces`] — metric collinearity over boolean-op
    /// output, never exact vertex equality) whose (linear) z profiles differ
    /// by more than [`RISER_Z_EPS_M`] somewhere along it, the vertical face
    /// between them: a quad with `exterior[0]`/`[1]` the base edge along the
    /// LOWER profile (the `WallRegion` vertex-order contract), split into
    /// two triangles where the profiles CROSS inside the piece (sub-sliver
    /// halves dropped). The predicate is Z-DISCONTINUITY, not adjacency:
    /// continuous fan/ridge seams — equal profiles along the shared edge —
    /// emit NOTHING. Outline edges never emit (they border one region).
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

/// D2 step 0 — PRE-LIFT: any ring vertex plan-matching one of `apexes`
/// (within 1e-6 m, the `fan_base_z` predicate) takes that apex's z.
/// Load-bearing for the re-apply-to-self identity: a fan/ridge triangle fed
/// back through `with_apexes` carries the apexes as ring CORNERS, and the
/// emitted hub/transition triangles keep the (lifted) boundary z — without
/// the lift a re-emitted corner could revert to a stale height.
fn pre_lift(mut ext: Vec<Point3f64>, apexes: &[&RoofApex]) -> Vec<Point3f64> {
    const EPS_M: f64 = 1e-6;
    for v in &mut ext {
        if let Some(a) = apexes
            .iter()
            .find(|a| (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M)
        {
            v.z = a.z_m;
        }
    }
    ext
}

/// Plan (projected) area of triangle `(p, q, r)` via the cross product —
/// positive CCW, negative CW.
fn tri_signed_plan_area(p: &Point3f64, q: &Point3f64, r: &Point3f64) -> f64 {
    ((q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)) / 2.0
}

/// FAN a region to a single `apex` (the 1-apex branch of
/// [`CeilingRegions::with_apexes`], where the semantics are documented):
/// pre-lift, then one triangle per boundary edge `(vᵢ, vᵢ₊₁, apex)`, dropping
/// sub-[`SLIVER_AREA_M2`] slivers — exactly the degenerate pair incident to
/// an apex that is already a corner. Fresh ids, default `opes`, inherited
/// `construction_id`.
fn fan_triangles(region: CeilingRegion, apex: &RoofApex) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let ext = pre_lift(region.polygon.exterior, &[apex]);
    let apex_v = Point3f64::new(apex.point.x, apex.point.y, apex.z_m);
    let n = ext.len();
    let mut out = Vec::new();
    for i in 0..n {
        let a = ext[i];
        let b = ext[(i + 1) % n];
        if tri_signed_plan_area(&a, &b, &apex_v).abs() < SLIVER_AREA_M2 {
            continue;
        }
        out.push(CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![a, b, apex_v]),
            opes: Opes::default(),
            construction_id: cid,
        });
    }
    out
}

/// Split each edge of `ext` at the point where the ridge parameter
/// `t(P) = (P − a)·d / |d|²` crosses 0.5 STRICTLY INSIDE the edge — the
/// ridge MIDLINE. Run before hub assignment (`ridge_edge_hubs`) so the two
/// hubs split a tied edge between them instead of one hub hogging it: on a
/// centred rectangle ridge both long edges used to midpoint-tie to B, which
/// sent ALL FOUR corners to B and made the roof asymmetric. The inserted
/// vertex's z is linearly interpolated along the edge (boundary edges are
/// typically level, so it's the base z). Edges whose endpoint sits exactly
/// ON the midline don't split (the vertex is already there); a whole edge
/// ALONG the midline has constant `t = 0.5` and never crosses. Degenerate
/// ridge (`|d|² < 1e-12`) → `ext` unchanged (the caller falls back to a
/// fan). The SINGLE source of the split — `ridge_triangles` and
/// [`ridge_hub_switches`] both run it, or the assign-time hub-switch check
/// would validate a different roof than resolve builds.
fn ridge_split_ring(ext: Vec<Point3f64>, a: Point2f64, b: Point2f64) -> Vec<Point3f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let d2 = dx * dx + dy * dy;
    if d2 < 1e-12 {
        return ext;
    }
    let t_of = |v: &Point3f64| ((v.x - a.x) * dx + (v.y - a.y) * dy) / d2;
    let n = ext.len();
    let mut out = Vec::with_capacity(n + 2);
    for i in 0..n {
        let p = ext[i];
        let q = ext[(i + 1) % n];
        out.push(p);
        let (tp, tq) = (t_of(&p), t_of(&q));
        if (tp - 0.5) * (tq - 0.5) < 0.0 {
            let s = (0.5 - tp) / (tq - tp);
            out.push(p + s * (q - p));
        }
    }
    out
}

/// Per-EDGE hub assignment for a ridge from `a` to `b` over a plan ring:
/// with `d = b − a` and the unclamped ridge parameter
/// `t(P) = (P − a)·d / |d|²`, edge `i` (from `ring[i]` to `ring[i+1]`)
/// belongs to hub B iff `t(midpoint) ≥ 0.5` (tie → B — after
/// [`ridge_split_ring`] a tie only remains for an edge lying ALONG the
/// midline, equidistant from both hubs so either choice tiles, or for a
/// zero-length edge, whose triangle is a sliver anyway).
/// Returns one bool per edge (`true` = B), or `None` for a degenerate ridge
/// (`|d|² < 1e-12`). The SINGLE source of the hub rule — `ridge_triangles`
/// and [`ridge_hub_switches`] must agree or the assign-time hub-switch check
/// would validate a different roof than resolve builds.
fn ridge_edge_hubs(ring: &[(f64, f64)], a: Point2f64, b: Point2f64) -> Option<Vec<bool>> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let d2 = dx * dx + dy * dy;
    if d2 < 1e-12 {
        return None;
    }
    let n = ring.len();
    Some(
        (0..n)
            .map(|i| {
                let p = ring[i];
                let q = ring[(i + 1) % n];
                let (mx, my) = ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
                ((mx - a.x) * dx + (my - a.y) * dy) / d2 >= 0.5
            })
            .collect(),
    )
}

/// How many times the per-edge hub assignment SWITCHES between the two ridge
/// endpoints walking `ring` — after the midline split ([`ridge_split_ring`]),
/// so the count equals the number of boundary crossings of the ridge midline
/// and agrees with what `ridge_triangles` builds (counted at vertices whose
/// entering and leaving edges disagree — each switch is one transition
/// triangle). A convex ring with both apexes interior switches exactly 2;
/// more means the ring wraps past the ridge axis (a deep notch) and the
/// transition triangles would overlap — assign-time validation rejects > 2,
/// resolve warns and proceeds. `0` for rings with fewer than 3 vertices or a
/// degenerate ridge (`a` ≈ `b`). Pure plan-space — usable on a group's
/// merged ring before any apex exists.
pub fn ridge_hub_switches(ring: &[Point2f64], a: Point2f64, b: Point2f64) -> usize {
    if ring.len() < 3 {
        return 0;
    }
    let ext: Vec<Point3f64> = ring.iter().map(|p| Point3f64::new(p.x, p.y, 0.0)).collect();
    let plan: Vec<(f64, f64)> = ridge_split_ring(ext, a, b)
        .iter()
        .map(|p| (p.x, p.y))
        .collect();
    let Some(hubs) = ridge_edge_hubs(&plan, a, b) else {
        return 0;
    };
    let n = hubs.len();
    (0..n).filter(|&i| hubs[i] != hubs[(i + n - 1) % n]).count()
}

/// Triangulate a region under a RIDGE between apexes `a` and `b` (the 2-apex
/// branch of [`CeilingRegions::with_apexes`]), per design D2:
///
/// 0. PRE-LIFT ring vertices plan-matching an apex to that apex's z.
/// 1. Degenerate ridge (`|b − a|² < 1e-12` in plan) → warn + single fan
///    to `a`.
/// 2. SPLIT boundary edges where they cross the ridge MIDLINE (`t = 0.5`),
///    z interpolated along the edge ([`ridge_split_ring`]) — so the two
///    hubs share a tied edge symmetrically instead of B hogging it.
/// 3. HUB triangles, in edge order: each boundary (sub-)edge `(vᵢ, vᵢ₊₁)`
///    rises to its hub `H` — `a` if the edge midpoint's ridge parameter
///    `t < 0.5`, else `b` (see [`ridge_edge_hubs`]) — emitting
///    `(vᵢ, vᵢ₊₁, H)` with the boundary keeping its (pre-lifted /
///    interpolated) z and `H` at the hub's `z_m`.
/// 4. TRANSITION triangles, in vertex order: at each vertex `v` whose
///    entering and leaving edges disagree, `(v, hub(leaving),
///    hub(entering))` — this order preserves CCW winding. After the split
///    these sit at the midline crossings, tiling symmetrically.
///
/// Sub-[`SLIVER_AREA_M2`] (plan) triangles drop in both steps. Fresh ids,
/// default `opes`, inherited `construction_id` (fan parity). For a convex
/// ring (≤ 2 hub switches) the triangles tile the region exactly; every
/// triangle is trivially planar. The caller (`with_apexes`) merges coplanar
/// neighbours afterwards ([`merge_coplanar_faces`]).
fn ridge_triangles(region: CeilingRegion, a: &RoofApex, b: &RoofApex) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let ext = pre_lift(region.polygon.exterior, &[a, b]);
    let ext = ridge_split_ring(ext, a.point, b.point);
    let plan: Vec<(f64, f64)> = ext.iter().map(|v| (v.x, v.y)).collect();
    let Some(hubs) = ridge_edge_hubs(&plan, a.point, b.point) else {
        log::warn!(
            "degenerate ridge: apexes coincide in plan at ({:.3}, {:.3}) — fanning to the first",
            a.point.x,
            a.point.y
        );
        return fan_triangles(
            CeilingRegion {
                id: ItemId::default(),
                polygon: Polygon3D::new(ext),
                opes: Opes::default(),
                construction_id: cid,
            },
            a,
        );
    };
    let hub_v = |is_b: bool| {
        let h = if is_b { b } else { a };
        Point3f64::new(h.point.x, h.point.y, h.z_m)
    };
    let mk = |verts: Vec<Point3f64>| CeilingRegion {
        id: ItemId::default(),
        polygon: Polygon3D::new(verts),
        opes: Opes::default(),
        construction_id: cid,
    };
    let n = ext.len();
    let mut out = Vec::new();
    // Hub triangles, in edge order.
    for i in 0..n {
        let (va, vb) = (ext[i], ext[(i + 1) % n]);
        let h = hub_v(hubs[i]);
        if tri_signed_plan_area(&va, &vb, &h).abs() < SLIVER_AREA_M2 {
            continue;
        }
        out.push(mk(vec![va, vb, h]));
    }
    // Transition triangles, in vertex order.
    for i in 0..n {
        let entering = hubs[(i + n - 1) % n];
        let leaving = hubs[i];
        if entering == leaving {
            continue;
        }
        let (hl, he) = (hub_v(leaving), hub_v(entering));
        if tri_signed_plan_area(&ext[i], &hl, &he).abs() < SLIVER_AREA_M2 {
            continue;
        }
        out.push(mk(vec![ext[i], hl, he]));
    }
    out
}

/// Vertex / plane tolerance for the coplanar face merge, in metres.
const MERGE_EPS_M: f64 = 1e-6;

/// Two 3D vertices equal within [`MERGE_EPS_M`] on every coordinate.
fn merge_pts_eq(u: &Point3f64, v: &Point3f64) -> bool {
    (u.x - v.x).abs() <= MERGE_EPS_M
        && (u.y - v.y).abs() <= MERGE_EPS_M
        && (u.z - v.z).abs() <= MERGE_EPS_M
}

/// Unit normal `n` and offset `d` (`n·v = d`) of a face's plane via Newell's
/// method (robust for any planar polygon, not just its first three
/// vertices); `None` when degenerate.
fn face_plane(ext: &[Point3f64]) -> Option<(Vector3f64, f64)> {
    if ext.len() < 3 {
        return None;
    }
    let (mut nx, mut ny, mut nz) = (0.0, 0.0, 0.0);
    for i in 0..ext.len() {
        let p = ext[i];
        let q = ext[(i + 1) % ext.len()];
        nx += (p.y - q.y) * (p.z + q.z);
        ny += (p.z - q.z) * (p.x + q.x);
        nz += (p.x - q.x) * (p.y + q.y);
    }
    let len = (nx * nx + ny * ny + nz * nz).sqrt();
    if len < 1e-12 {
        return None;
    }
    let n = Vector3f64::new(nx / len, ny / len, nz / len);
    let d = ext
        .iter()
        .map(|v| n.x * v.x + n.y * v.y + n.z * v.z)
        .sum::<f64>()
        / ext.len() as f64;
    Some((n, d))
}

/// `w` lies on the straight 3D line through its neighbours `p` → `q` (within
/// [`MERGE_EPS_M`] of it) — a redundant vertex on a merged boundary.
fn collinear3(p: &Point3f64, w: &Point3f64, q: &Point3f64) -> bool {
    let u = *w - *p;
    let v = *q - *p;
    let vl = v.norm();
    if vl <= MERGE_EPS_M {
        // p ≈ q: w sits between duplicate neighbours — treat as redundant
        // only when it coincides with them too.
        return u.norm() <= MERGE_EPS_M;
    }
    u.cross(&v).norm() <= MERGE_EPS_M * vl
}

/// First edge of `r1` whose REVERSE appears in `r2`:
/// `(i1, i2)` with `r2[i2] == r1[i1+1]` and `r2[i2+1] == r1[i1]`
/// ([`merge_pts_eq`], plan + z). Zero-length edges never qualify.
fn shared_edge(r1: &[Point3f64], r2: &[Point3f64]) -> Option<(usize, usize)> {
    let (n1, n2) = (r1.len(), r2.len());
    for i1 in 0..n1 {
        let (u, v) = (r1[i1], r1[(i1 + 1) % n1]);
        if merge_pts_eq(&u, &v) {
            continue;
        }
        for i2 in 0..n2 {
            if merge_pts_eq(&r2[i2], &v) && merge_pts_eq(&r2[(i2 + 1) % n2], &u) {
                return Some((i1, i2));
            }
        }
    }
    None
}

/// Merge two face rings into their union polygon when they (a) share a FULL
/// edge — two consecutive vertices equal within [`MERGE_EPS_M`] (plan + z),
/// either orientation (`r2` is reversed first if the edge matches
/// same-direction) — and (b) are COPLANAR: every vertex of both within
/// [`MERGE_EPS_M`] of the plane of the larger (by plan area, Newell fit).
/// The shared edge is removed, the boundaries spliced, and collinear-in-plane
/// vertices along the merged boundary dropped. `None` when the pair doesn't
/// qualify (or the union would degenerate below 3 vertices).
fn merge_coplanar_pair(r1: &[Point3f64], r2: &[Point3f64]) -> Option<Vec<Point3f64>> {
    if r1.len() < 3 || r2.len() < 3 {
        return None;
    }
    // (a) the shared full edge, normalizing r2 to opposite orientation.
    let (r2v, (i1, i2)) = match shared_edge(r1, r2) {
        Some(hit) => (r2.to_vec(), hit),
        None => {
            let rev: Vec<Point3f64> = r2.iter().rev().copied().collect();
            let hit = shared_edge(r1, &rev)?;
            (rev, hit)
        }
    };
    // (b) coplanarity against the larger face's plane.
    let plan_area = |ext: &[Point3f64]| {
        let n = ext.len();
        (0..n)
            .map(|i| {
                let p = ext[i];
                let q = ext[(i + 1) % n];
                p.x * q.y - q.x * p.y
            })
            .sum::<f64>()
            .abs()
            / 2.0
    };
    let big = if plan_area(r1) >= plan_area(&r2v) {
        r1
    } else {
        &r2v[..]
    };
    let (n, d) = face_plane(big)?;
    let on_plane = |ext: &[Point3f64]| {
        ext.iter()
            .all(|v| (n.x * v.x + n.y * v.y + n.z * v.z - d).abs() <= MERGE_EPS_M)
    };
    if !on_plane(r1) || !on_plane(&r2v) {
        return None;
    }
    // (c) splice: r1 from v all the way round to u, then r2's path from u
    // back round to v with both shared endpoints dropped.
    let (n1, n2) = (r1.len(), r2v.len());
    let mut merged: Vec<Point3f64> = (0..n1).map(|k| r1[(i1 + 1 + k) % n1]).collect();
    merged.extend((2..n2).map(|k| r2v[(i2 + k) % n2]));
    // (d) drop collinear / duplicate vertices along the merged boundary.
    let mut changed = true;
    while changed && merged.len() > 3 {
        changed = false;
        let mut i = 0;
        while i < merged.len() && merged.len() > 3 {
            let m = merged.len();
            let p = merged[(i + m - 1) % m];
            let q = merged[(i + 1) % m];
            if collinear3(&p, &merged[i], &q) {
                merged.remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
    }
    (merged.len() >= 3).then_some(merged)
}

/// The COPLANAR MERGE pass run on each region's fan/ridge output at the end
/// of [`CeilingRegions::with_apexes`]: repeatedly merge the first pair of
/// faces (in stable index order — determinism is part of the reload
/// fixpoint) that share a full edge and are coplanar
/// ([`merge_coplanar_pair`]), until none qualify. Coplanar triangle
/// neighbours thus collapse into single planar polygons (the rectangle
/// ridge's two slopes become trapezoids). Merged faces inherit
/// `construction_id`, get fresh ids and default `opes` (split parity);
/// untouched faces pass through as-is.
fn merge_coplanar_faces(mut faces: Vec<CeilingRegion>) -> Vec<CeilingRegion> {
    'outer: loop {
        for i in 0..faces.len() {
            for j in (i + 1)..faces.len() {
                if let Some(merged) =
                    merge_coplanar_pair(&faces[i].polygon.exterior, &faces[j].polygon.exterior)
                {
                    let construction_id = faces[i].construction_id;
                    faces[i] = CeilingRegion {
                        id: ItemId::default(),
                        polygon: Polygon3D::new(merged),
                        opes: Opes::default(),
                        construction_id,
                    };
                    faces.remove(j);
                    continue 'outer;
                }
            }
        }
        return faces;
    }
}

/// Decompose a boolean-op result polygon into SIMPLE (exterior-only) pieces.
///
/// `Polygon3D` stores only an exterior ring, so emitting a `geo` polygon that
/// carries interior rings would silently FILL its holes — and `difference`
/// produces exactly that when a step area lies strictly INSIDE a region (the
/// remainder is the region with a hole). The filled phantom would overlap the
/// stepped piece and inflate the total ceiling area on the next re-derive,
/// breaking the canonicalize → re-derive fixpoint.
///
/// All ceiling-step geometry is axis-aligned, so a polygon with holes is cut
/// with an axis-aligned line through the first hole's centre (intersection
/// with the two half-plane rectangles over the polygon's bbox) and the pieces
/// are decomposed recursively. The cut crosses that hole, so neither half can
/// still contain it whole — every split strictly reduces the remaining hole
/// extent and the recursion terminates with simple pieces.
fn split_to_simple(poly: &Polygon<f64>) -> Vec<Polygon<f64>> {
    use geo::bool_ops::BooleanOps;
    use geo::BoundingRect;
    if poly.interiors().is_empty() {
        return vec![poly.clone()];
    }
    let (Some(bbox), Some(hole)) = (poly.bounding_rect(), poly.interiors()[0].bounding_rect())
    else {
        // Degenerate ring with no extent — nothing meaningful to split.
        return vec![poly.clone()];
    };
    // Cut perpendicular to the hole's WIDER dimension so the line genuinely
    // crosses it (a vertical cut through a hair-thin vertical hole may not).
    let c = hole.center();
    let (min, max) = (bbox.min(), bbox.max());
    let pad = 1.0; // clip rects overhang the bbox so its edges never graze
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
        Polygon::new(
            LineString::from(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]),
            vec![],
        )
    };
    let halves = if hole.width() >= hole.height() {
        [
            rect(min.x - pad, min.y - pad, c.x, max.y + pad),
            rect(c.x, min.y - pad, max.x + pad, max.y + pad),
        ]
    } else {
        [
            rect(min.x - pad, min.y - pad, max.x + pad, c.y),
            rect(min.x - pad, c.y, max.x + pad, max.y + pad),
        ]
    };
    halves
        .iter()
        .flat_map(|half| poly.intersection(half).0)
        .flat_map(|piece| split_to_simple(&piece))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Polygon3D {
    pub exterior: Vec<Point3f64>,
}

/// A line lying in a planar polygon's plane, with the side-deciding `normal` —
/// the input to [`Polygon3D::split_by_line`]. Moved here from the dissolved
/// `gen_3d_vol` crate (2026-06).
pub struct SplitLine3D {
    pub point: Point3f64,      // point on line
    pub direction: Vector3f64, // unit
    pub normal: Vector3f64,    // points to "positive" side
}

impl Polygon3D {
    pub fn new(exterior: Vec<Point3f64>) -> Self {
        Self { exterior }
    }

    /// Unit normal of the polygon's plane (from its first three vertices) and
    /// the plane's distance-to-origin term; `None` when degenerate. Private
    /// helper for [`Polygon3D::split_by_line`].
    fn get_normal_and_distance_to_origin(&self) -> Option<(Vector3f64, f64)> {
        if self.exterior.len() < 3 {
            return None;
        }
        let p0 = &self.exterior[0];
        let p1 = &self.exterior[1];
        let p2 = &self.exterior[2];
        let u = Vector3f64::new(p1.x - p0.x, p1.y - p0.y, p1.z - p0.z);
        let v = Vector3f64::new(p2.x - p0.x, p2.y - p0.y, p2.z - p0.z);
        let normal = Vector3f64::new(
            u.y * v.z - u.z * v.y,
            u.z * v.x - u.x * v.z,
            u.x * v.y - u.y * v.x,
        );
        let norm_length = (normal.x.powi(2) + normal.y.powi(2) + normal.z.powi(2)).sqrt();
        if norm_length < 1e-10 {
            return None;
        }
        let unit_normal = Vector3f64::new(
            normal.x / norm_length,
            normal.y / norm_length,
            normal.z / norm_length,
        );
        let d = -(unit_normal.x * p0.x + unit_normal.y * p0.y + unit_normal.z * p0.z);
        Some((unit_normal, d / norm_length))
    }

    /// Split this planar polygon in two along `line` (which must lie in its
    /// plane).
    ///
    /// SEAM: currently has no caller. Kept for the deferred wall face/height
    /// DIAGONAL SPLIT of the wall-exposure design — dividing one marked wall's
    /// face along a length/height intercept into separately-attributed
    /// `WallRegion`s for mixed constructions.
    pub fn split_by_line(&self, line: &SplitLine3D) -> Vec<Polygon3D> {
        let (plane_n, _) = self.get_normal_and_distance_to_origin().unwrap();
        let line_n = plane_n.cross(&line.direction).normalize();

        let mut left = Vec::new();
        let mut right = Vec::new();

        let verts = &self.exterior;

        for i in 0..verts.len() {
            let a = verts[i];
            let b = verts[(i + 1) % verts.len()];

            let da = (a.coords - line.point.coords).dot(&line_n);
            let db = (b.coords - line.point.coords).dot(&line_n);

            if da >= 0.0 {
                left.push(a);
            }
            if da <= 0.0 {
                right.push(a);
            }

            // Edge crosses the line → add intersection
            if da * db < 0.0 {
                let t = da / (da - db);
                let p = a + t * (b - a);
                left.push(p);
                right.push(p);
            }
        }

        vec![Polygon3D::new(left), Polygon3D::new(right)]
    }
}
impl From<Polygon3D> for Polygon<f64> {
    fn from(polygon_3d: Polygon3D) -> Self {
        let coords_2d: Vec<Coord<f64>> = polygon_3d
            .exterior
            .iter()
            .map(|v| Coord { x: v.x, y: v.y })
            .collect();
        Polygon::new(LineString::from(coords_2d), vec![])
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
}

impl CeilingRegion {
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
    pub fn fan_base_z(&self, apexes: &[RoofApex]) -> Option<f64> {
        const EPS_M: f64 = 1e-6;
        if self.polygon.exterior.len() < 3 {
            return None;
        }
        let base: Vec<f64> = self
            .polygon
            .exterior
            .iter()
            .filter(|v| {
                !apexes
                    .iter()
                    .any(|a| (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M)
            })
            .map(|v| v.z)
            .collect();
        let (&z0, rest) = base.split_first()?;
        rest.iter().all(|z| (z - z0).abs() <= EPS_M).then_some(z0)
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

/// A vertical planar wall region — pure geometry + exposure + openings. Its
/// fabric (construction type/make-up/phase, photos, description) lives on the
/// `WallConstruction` referenced by `construction_id`; exposure stays here so a
/// single construction can be split into exposed / semi-exposed sections.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WallRegion {
    pub id: ItemId,
    pub polygon: Polygon3D, // 3D polygon (vertical plane, typically 4 vertices)
    pub exposure: WallExposure,
    pub opes: Opes,
    pub construction_id: ItemId,
}

impl WallRegion {
    /// Midpoint of the wall's base edge (footprint), in plan (2D). Used to
    /// hit-test which construction-phase area the wall falls in. `None` for a
    /// degenerate polygon with fewer than two vertices.
    pub fn base_midpoint(&self) -> Option<Point2f64> {
        let e = &self.polygon.exterior;
        (e.len() >= 2).then(|| Point2f64::new((e[0].x + e[1].x) * 0.5, (e[0].y + e[1].y) * 0.5))
    }
}

/// Collection of wall regions
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WallRegions {
    pub regions: Vec<WallRegion>,
}

impl WallRegions {
    /// Re-derive the storey's walls from `segments` (each a footprint edge
    /// `(a, b)` with its thermal `exposure`), tracing each wall's TOP along
    /// the roof profile and carrying identity over from the `previous`
    /// regions (replaces the flat-top `rebuild_preserving`).
    ///
    /// Geometry — vertex-order CONTRACT: `exterior[0]`/`exterior[1]` are
    /// ALWAYS the base edge, `a` then `b` at `base_z` (exposure recovery and
    /// `base_midpoint` read them positionally); the top vertices append after
    /// index 1, tracing from `b` back to `a` so the ring closes. The top
    /// follows the `roofs` profile over the base: piecewise-LINEAR across
    /// each planar roof region (flat pieces and fan triangles alike), with a
    /// VERTICAL JUMP — two vertices at one plan point — where a ceiling step
    /// crosses the wall, keeping the perimeter envelope gap-free. Spans of
    /// the base covered by no roof region — including when `roofs` is empty
    /// (ceiling height unset) — top out flat at `fallback_top_z` (ABSOLUTE
    /// z, not a height above `base_z`). A flat roof (or the fallback) yields
    /// the classic 4-vertex quad: a-bottom, b-bottom, b-top, a-top.
    ///
    /// Identity (amend-vs-create):
    /// - AMEND: a segment whose base edge matches a previous region's (both
    ///   endpoints within 1e-2 m, either orientation) keeps that region's
    ///   `id`, `construction_id` AND `opes` — a roof change re-shapes a
    ///   marked wall without orphaning its openings. Matching is INJECTIVE:
    ///   the best match (smallest endpoint error) wins and a previous region
    ///   amends at most one new segment, so ids are never duplicated.
    /// - COVERED SPLIT: an unmatched segment whose midpoint a previous
    ///   region covers (`region_covering`) inherits `construction_id` only —
    ///   fresh `id`, empty `opes` (positional: ambiguous to split).
    /// - FRESH: anything else gets `default_construction_id` (the caller
    ///   supplies the id of an "uninitialised" construction it has ensured
    ///   exists in the BER).
    ///
    /// Runs on every materialize: O(segments × previous + segments × roof
    /// vertices), no caching.
    pub fn regenerate(
        segments: Vec<(Point2f64, Point2f64, WallExposure)>,
        previous: &WallRegions,
        roofs: &CeilingRegions,
        base_z: f64,
        fallback_top_z: f64,
        default_construction_id: ItemId,
    ) -> Self {
        // ── Injective base-edge matching (amend) ─────────────────────────
        let bases: Vec<(Point2f64, Point2f64)> =
            segments.iter().map(|(a, b, _)| (*a, *b)).collect();
        let mut prev_used = vec![false; previous.regions.len()];
        let amend_from = Self::match_bases_to_previous(&bases, previous, &mut prev_used);

        let regions = segments
            .into_iter()
            .enumerate()
            .map(|(si, (a, b, exposure))| {
                // Base edge first (the contract), then the roof-profile top.
                let mut ext = vec![
                    Point3f64::new(a.x, a.y, base_z),
                    Point3f64::new(b.x, b.y, base_z),
                ];
                ext.extend(roof_profile_top(roofs, a, b, fallback_top_z));
                let polygon = Polygon3D::new(ext);
                match amend_from[si] {
                    Some(pi) => {
                        let src = &previous.regions[pi];
                        WallRegion {
                            id: src.id,
                            polygon,
                            exposure,
                            opes: src.opes.clone(),
                            construction_id: src.construction_id,
                        }
                    }
                    None => {
                        let mid = Point2f64::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
                        let construction_id = match previous.region_covering(mid) {
                            Some(src) => src.construction_id,
                            None => default_construction_id,
                        };
                        WallRegion {
                            id: ItemId::default(),
                            polygon,
                            exposure,
                            opes: Opes::default(),
                            construction_id,
                        }
                    }
                }
            })
            .collect();
        Self { regions }
    }

    /// `regenerate`'s injective amend matcher, shared with [`with_risers`]:
    /// a base edge matches a previous region when both endpoints sit within
    /// [`MATCH_EPS_M`] of its base edge (either orientation). Every
    /// qualifying (error, base, previous) pair is assigned greedily
    /// best-first, so a previous region amends at most one base and ids are
    /// never duplicated. `prev_used` marks previous regions already CLAIMED
    /// on entry (how `with_risers` excludes the ids the perimeter pass kept)
    /// and is updated with the assignments made.
    ///
    /// [`with_risers`]: WallRegions::with_risers
    fn match_bases_to_previous(
        bases: &[(Point2f64, Point2f64)],
        previous: &WallRegions,
        prev_used: &mut [bool],
    ) -> Vec<Option<usize>> {
        let dist =
            |u: Point2f64, vx: f64, vy: f64| ((u.x - vx).powi(2) + (u.y - vy).powi(2)).sqrt();
        let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
        for (si, (a, b)) in bases.iter().enumerate() {
            for (pi, r) in previous.regions.iter().enumerate() {
                let e = &r.polygon.exterior;
                if e.len() < 2 {
                    continue;
                }
                let fwd = dist(*a, e[0].x, e[0].y).max(dist(*b, e[1].x, e[1].y));
                let rev = dist(*a, e[1].x, e[1].y).max(dist(*b, e[0].x, e[0].y));
                let err = fwd.min(rev);
                if err <= MATCH_EPS_M {
                    candidates.push((err, si, pi));
                }
            }
        }
        candidates.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap_or(std::cmp::Ordering::Equal));
        let mut matched: Vec<Option<usize>> = vec![None; bases.len()];
        for (_, si, pi) in candidates {
            if matched[si].is_none() && !prev_used[pi] {
                matched[si] = Some(pi);
                prev_used[pi] = true;
            }
        }
        matched
    }

    /// Append derived RISER faces (`faces` pairs each face with the exposure
    /// the caller assigned) with `regenerate`'s amend identity: the same
    /// injective best-match on the base edge (`exterior[0]`/`[1]`, within
    /// [`MATCH_EPS_M`]) against `previous` — EXCLUDING the previous ids
    /// `self` already holds, so a riser can never duplicate an id the
    /// perimeter pass kept. A matched face keeps the previous region's `id`,
    /// `construction_id` AND `opes`; an unmatched face whose base midpoint a
    /// previous region covers inherits its `construction_id` only; anything
    /// else gets `default_construction_id`. The faces are re-derived
    /// deterministically every materialize, so the match keeps riser
    /// identity stable across re-resolves.
    pub fn with_risers(
        mut self,
        faces: Vec<(Polygon3D, WallExposure)>,
        previous: &WallRegions,
        default_construction_id: ItemId,
    ) -> Self {
        let faces: Vec<(Polygon3D, WallExposure)> = faces
            .into_iter()
            .filter(|(p, _)| p.exterior.len() >= 3)
            .collect();
        // Previous regions whose id survived into `self` are already claimed.
        let mut prev_used: Vec<bool> = previous
            .regions
            .iter()
            .map(|p| self.regions.iter().any(|r| r.id == p.id))
            .collect();
        let bases: Vec<(Point2f64, Point2f64)> = faces
            .iter()
            .map(|(p, _)| {
                (
                    Point2f64::new(p.exterior[0].x, p.exterior[0].y),
                    Point2f64::new(p.exterior[1].x, p.exterior[1].y),
                )
            })
            .collect();
        let matched = Self::match_bases_to_previous(&bases, previous, &mut prev_used);
        for ((polygon, exposure), m) in faces.into_iter().zip(matched) {
            let region = match m {
                Some(pi) => {
                    let src = &previous.regions[pi];
                    WallRegion {
                        id: src.id,
                        polygon,
                        exposure,
                        opes: src.opes.clone(),
                        construction_id: src.construction_id,
                    }
                }
                None => {
                    let mid = Point2f64::new(
                        (polygon.exterior[0].x + polygon.exterior[1].x) * 0.5,
                        (polygon.exterior[0].y + polygon.exterior[1].y) * 0.5,
                    );
                    let construction_id = match previous.region_covering(mid) {
                        Some(src) => src.construction_id,
                        None => default_construction_id,
                    };
                    WallRegion {
                        id: ItemId::default(),
                        polygon,
                        exposure,
                        opes: Opes::default(),
                        construction_id,
                    }
                }
            };
            self.regions.push(region);
        }
        self
    }

    /// The region whose base edge (the wall footprint) passes through `p`, used
    /// to inherit fabric properties when re-deriving. `None` if no region covers
    /// the point (e.g. a freshly drawn wall).
    pub fn region_covering(&self, p: Point2f64) -> Option<&WallRegion> {
        self.regions.iter().find(|r| {
            let e = &r.polygon.exterior;
            e.len() >= 2
                && point_to_segment_distance(p.x, p.y, e[0].x, e[0].y, e[1].x, e[1].y) < 1e-2
        })
    }
}

/// Both endpoints of a base edge must sit within this (m) of a previous
/// region's for it to be the SAME wall — the amend-matching tolerance of
/// [`WallRegions::regenerate`] and [`WallRegions::with_risers`].
const MATCH_EPS_M: f64 = 1e-2;

// ── Shared-boundary riser extraction (private helpers for `riser_faces`) ──

/// Minimum 1D overlap (m) for two near-collinear boundary edges to count as
/// a SHARED plan piece — anything shorter is boolean-op noise, not a real
/// region boundary.
const RISER_MIN_OVERLAP_M: f64 = 1e-3;

/// Z discontinuity (m) above which two surfaces meeting at a plan edge need
/// a riser face between them. At or below it they are the SAME continuous
/// surface — a fan/ridge seam, or floor pieces one phase boundary splits at
/// a single level.
const RISER_Z_EPS_M: f64 = 1e-4;

/// The plan pieces two region boundary rings SHARE: every edge pair that is
/// metrically collinear (each endpoint of one within [`PROFILE_EPS_M`] of
/// the other's line — the rings are boolean-op output, so a conceptually
/// shared edge is never an exact vertex match) whose 1D overlap along the
/// line exceeds [`RISER_MIN_OVERLAP_M`], as the overlap's plan endpoints.
/// Simple rings never retrace themselves, so one pair's pieces are disjoint.
fn shared_plan_pieces(a: &[Point2f64], b: &[Point2f64]) -> Vec<(Point2f64, Point2f64)> {
    let mut out = Vec::new();
    let (na, nb) = (a.len(), b.len());
    if na < 2 || nb < 2 {
        return out;
    }
    for i in 0..na {
        let (p, q) = (a[i], a[(i + 1) % na]);
        let (dx, dy) = (q.x - p.x, q.y - p.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= RISER_MIN_OVERLAP_M {
            continue;
        }
        let (ux, uy) = (dx / len, dy / len);
        // Perpendicular distance (m) to / parameter (m) along edge a's line.
        let dist_to_line = |c: Point2f64| ((c.x - p.x) * uy - (c.y - p.y) * ux).abs();
        let t_of = |c: Point2f64| (c.x - p.x) * ux + (c.y - p.y) * uy;
        for j in 0..nb {
            let (r, s) = (b[j], b[(j + 1) % nb]);
            if dist_to_line(r) > PROFILE_EPS_M || dist_to_line(s) > PROFILE_EPS_M {
                continue; // not (near-)collinear — no shared run
            }
            let (t0, t1) = (t_of(r), t_of(s));
            let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(len));
            if hi - lo <= RISER_MIN_OVERLAP_M {
                continue;
            }
            out.push((
                Point2f64::new(p.x + ux * lo, p.y + uy * lo),
                Point2f64::new(p.x + ux * hi, p.y + uy * hi),
            ));
        }
    }
    out
}

/// One vertical riser face over the plan run `p`→`q`, from a LOWER surface
/// (`lo_*` at each end) up to an upper one (`hi_*`): `exterior[0]`/`[1]` are
/// the base edge along the lower (the `WallRegion` vertex-order contract),
/// then the top traces back `q`→`p`. A top vertex that wouldn't open past
/// [`RISER_Z_EPS_M`] is skipped, so a face whose profiles meet at one end
/// comes out a triangle. `None` when degenerate — a sub-
/// [`RISER_MIN_OVERLAP_M`] plan run or no end opening past the z epsilon
/// (the sub-sliver halves of a crossing split).
fn riser_face(
    p: Point2f64,
    q: Point2f64,
    lo_p: f64,
    hi_p: f64,
    lo_q: f64,
    hi_q: f64,
) -> Option<Polygon3D> {
    let plan_len = ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
    if plan_len <= RISER_MIN_OVERLAP_M {
        return None;
    }
    if hi_p - lo_p <= RISER_Z_EPS_M && hi_q - lo_q <= RISER_Z_EPS_M {
        return None;
    }
    let mut ext = vec![
        Point3f64::new(p.x, p.y, lo_p),
        Point3f64::new(q.x, q.y, lo_q),
    ];
    if hi_q - lo_q > RISER_Z_EPS_M {
        ext.push(Point3f64::new(q.x, q.y, hi_q));
    }
    if hi_p - lo_p > RISER_Z_EPS_M {
        ext.push(Point3f64::new(p.x, p.y, hi_p));
    }
    Some(Polygon3D::new(ext))
}

// ── Roof-profile sampling (private helpers for `WallRegions::regenerate`) ──

/// Plan-coordinate tolerance (m) of the roof-profile sampling. The derived
/// roof regions are boolean-op output (geo/i_overlay snaps coordinates to an
/// adaptive grid) and round-trip through f32 on canonicalize/reload, so a
/// region boundary that conceptually COINCIDES with a wall base line can sit
/// up to a few 1e-7 m off it. 1e-4 m (0.1 mm) is comfortably above that
/// noise and far below any real architectural feature, so a pick/breakpoint
/// within this band is the same geometry, not a different roof.
const PROFILE_EPS_M: f64 = 1e-4;

/// The plane `z = c0 + cx·x + cy·y` through a planar ceiling region, from its
/// first vertex and the first vertex pair with a non-degenerate plan cross
/// product. Exact for flat pieces (`cx = cy = 0`) and fan triangles alike.
/// `None` for regions degenerate in plan (< 3 vertices or collinear).
fn region_plane(region: &CeilingRegion) -> Option<(f64, f64, f64)> {
    let ext = &region.polygon.exterior;
    if ext.len() < 3 {
        return None;
    }
    let o = ext[0];
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
}

/// The (region index, plane) of the first roof region whose plan projection
/// touches `p`. Containment is boundary-INCLUSIVE (`Intersects`): a wall
/// base lies ON the outline edge that bounds its roof region, so the strict
/// predicate would never match. When two regions share the boundary `p` sits
/// on (a ceiling step running ALONG the wall), the first in region order
/// wins — callers sample interval midpoints, which are never at a crossing
/// breakpoint.
///
/// When the EXACT point touches no region, retry NUDGED by [`PROFILE_EPS_M`]
/// perpendicular to the wall (`n` is the caller's unit normal; both sides
/// tried). The derived regions are boolean-op output whose boundary can sit
/// a few 1e-7 m INSIDE the wall line it conceptually coincides with (grid
/// snapping + f32 round-trips — see [`PROFILE_EPS_M`]); without the retry
/// the pick fails along exactly the walls that bound a stepped/ridged roof
/// piece and the caller's fallback silently flattens them to the default
/// height. The exact pick stays first so behaviour on clean geometry — tie
/// order included — is unchanged.
fn covering_roof_plane(
    roofs: &CeilingRegions,
    p: Point2f64,
    n: (f64, f64),
) -> Option<(usize, (f64, f64, f64))> {
    use geo::Intersects;
    let pick = |q: Point2f64| {
        let gq = GeoPoint::new(q.x, q.y);
        roofs.regions.iter().enumerate().find_map(|(i, r)| {
            Polygon::<f64>::from(r.polygon.clone())
                .intersects(&gq)
                .then(|| region_plane(r).map(|plane| (i, plane)))
                .flatten()
        })
    };
    pick(p).or_else(|| {
        [1.0, -1.0].iter().find_map(|s| {
            pick(Point2f64::new(
                p.x + s * n.0 * PROFILE_EPS_M,
                p.y + s * n.1 * PROFILE_EPS_M,
            ))
        })
    })
}

/// Parameters t ∈ [0, 1] along the base `a`→`b` where the covering roof
/// region can change: every crossing of a region boundary edge with the base
/// (near-collinear overlapping edges contribute their projected endpoints),
/// plus the ends 0 and 1. Sorted ascending; the caller skips sub-µm spans.
///
/// Tolerances are METRIC ([`PROFILE_EPS_M`]), not exact: the derived roof
/// rings are boolean-op output that can sit a few 1e-7 m off the wall line
/// they conceptually coincide with. An edge whose BOTH endpoints lie within
/// the band counts as collinear-overlapping (the old exact cross-product
/// test missed displaced eaves edges); a crossing edge may start/stop up to
/// [`PROFILE_EPS_M`] (in metres, scaled to its own parameter) short of the
/// wall line and still contribute (the old `s` tolerance of 1e-9 dropped
/// step-boundary edges that stopped ~1e-7 m shy, losing the step's jump).
fn profile_breakpoints(roofs: &CeilingRegions, a: Point2f64, b: Point2f64) -> Vec<f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let mut ts = vec![0.0, 1.0];
    if len2 <= 1e-18 {
        return ts;
    }
    let len = len2.sqrt();
    for r in &roofs.regions {
        let ext = &r.polygon.exterior;
        let n = ext.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let p = ext[i];
            let q = ext[(i + 1) % n];
            // Perpendicular distance (m) of each edge endpoint to the wall
            // LINE — the metric collinearity measure.
            let dist_to_line = |c: &Point3f64| ((c.x - a.x) * dy - (c.y - a.y) * dx).abs() / len;
            if dist_to_line(&p) <= PROFILE_EPS_M && dist_to_line(&q) <= PROFILE_EPS_M {
                // (Near-)collinear overlap (e.g. the roof's eaves edge along
                // the wall base itself, possibly displaced by boolean-op
                // noise): the region can start/stop at the edge's endpoints.
                for c in [p, q] {
                    let t = ((c.x - a.x) * dx + (c.y - a.y) * dy) / len2;
                    if (0.0..=1.0).contains(&t) {
                        ts.push(t);
                    }
                }
                continue;
            }
            let (ex, ey) = (q.x - p.x, q.y - p.y);
            let (wx, wy) = (p.x - a.x, p.y - a.y);
            let denom = dx * ey - dy * ex;
            if denom.abs() > 1e-12 {
                // Proper crossing: base(t) == edge(s); keep t when both
                // parameters land on their segments, letting the edge fall
                // short of / overshoot the wall line by PROFILE_EPS_M.
                let t = (wx * ey - wy * ex) / denom;
                let s = (wx * dy - wy * dx) / denom;
                let edge_len = (ex * ex + ey * ey).sqrt();
                let eps_s = if edge_len > 0.0 {
                    PROFILE_EPS_M / edge_len
                } else {
                    0.0
                };
                if (-eps_s..=1.0 + eps_s).contains(&s) && (0.0..=1.0).contains(&t) {
                    ts.push(t);
                }
            }
        }
    }
    ts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    ts
}

/// The wall TOP for the base `a`→`b`, traced from `b` BACK to `a` (so the
/// caller's ring closes base → top in one loop): one linear span per
/// breakpoint interval, z evaluated at the interval ends on the covering
/// region's plane (region chosen at the interval MIDPOINT — never a
/// crossing), `fallback_top_z` over uncovered spans. Where adjacent spans
/// disagree at a breakpoint, both vertices are kept — the VERTICAL JUMP of a
/// ceiling step crossing the wall; where they agree, consecutive duplicates
/// collapse, so a flat roof (or empty `roofs`) yields exactly
/// `[b_top, a_top]`.
fn roof_profile_top(
    roofs: &CeilingRegions,
    a: Point2f64,
    b: Point2f64,
    fallback_top_z: f64,
) -> Vec<Point3f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    // Unit normal of the base — the nudge direction for the tolerant pick.
    let n = if len > 0.0 {
        (-dy / len, dx / len)
    } else {
        (0.0, 0.0)
    };
    let at = |t: f64| Point2f64::new(a.x + dx * t, a.y + dy * t);
    // (t0, t1, z at t0, z at t1) per interval, ascending t.
    let mut spans: Vec<(f64, f64, f64, f64)> = Vec::new();
    for w in profile_breakpoints(roofs, a, b).windows(2) {
        let (t0, t1) = (w[0], w[1]);
        // Skip degenerate and sub-µm spans (boolean-op noise can put two
        // breakpoints ~1e-8 m apart; a hairline span samples unreliably and
        // its vertices are meaningless).
        if t1 - t0 <= 1e-9 || (t1 - t0) * len <= 1e-6 {
            continue;
        }
        let mid = at((t0 + t1) / 2.0);
        let span = match covering_roof_plane(roofs, mid, n) {
            Some((ri, (c0, cx, cy))) => {
                let (p0, p1) = (at(t0), at(t1));
                let (z0, z1) = (c0 + cx * p0.x + cy * p0.y, c0 + cx * p1.x + cy * p1.y);
                log::debug!(
                    "wall profile [{:.3},{:.3}]→[{:.3},{:.3}] span t=[{t0:.4},{t1:.4}] \
                     mid=({:.4},{:.4}): roof region #{ri} plane z={c0:.4}{cx:+.4}·x{cy:+.4}·y \
                     → top {z0:.4}..{z1:.4}",
                    a.x,
                    a.y,
                    b.x,
                    b.y,
                    mid.x,
                    mid.y
                );
                (t0, t1, z0, z1)
            }
            None => {
                log::debug!(
                    "wall profile [{:.3},{:.3}]→[{:.3},{:.3}] span t=[{t0:.4},{t1:.4}] \
                     mid=({:.4},{:.4}): NO covering roof region → fallback z={fallback_top_z:.4}",
                    a.x,
                    a.y,
                    b.x,
                    b.y,
                    mid.x,
                    mid.y
                );
                (t0, t1, fallback_top_z, fallback_top_z)
            }
        };
        spans.push(span);
    }
    if spans.is_empty() {
        spans.push((0.0, 1.0, fallback_top_z, fallback_top_z));
    }
    let mut out: Vec<Point3f64> = Vec::new();
    let push = |out: &mut Vec<Point3f64>, p: Point2f64, z: f64| {
        let keep = out.last().is_none_or(|l| {
            (l.x - p.x).abs() > 1e-9 || (l.y - p.y).abs() > 1e-9 || (l.z - z).abs() > 1e-9
        });
        if keep {
            out.push(Point3f64::new(p.x, p.y, z));
        }
    };
    for (t0, t1, z0, z1) in spans.iter().rev() {
        push(&mut out, at(*t1), *z1);
        push(&mut out, at(*t0), *z0);
    }
    if log::log_enabled!(log::Level::Debug) {
        let pl: Vec<String> = out
            .iter()
            .map(|v| format!("({:.4},{:.4},{:.4})", v.x, v.y, v.z))
            .collect();
        log::debug!(
            "wall profile [{:.3},{:.3}]→[{:.3},{:.3}] top polyline (b→a): {}",
            a.x,
            a.y,
            b.x,
            b.y,
            pl.join(" ")
        );
    }
    out
}

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
    pub roofs: CeilingRegions,
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
    #[serde(default)]
    pub roof_apexes: Vec<RoofApex>,
    // No editable sketch snapshot is stored: the resolved geometry above
    // (`floors`/`walls`/`living_areas`) is the only persisted representation,
    // and the editor rebuilds its working state from it on load. The DB only
    // ever holds finalised (complete, metre-scale) geometry.
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShelteredSideInfo {
    pub id: ItemId,
    pub sheltered_sides: i32, // e.g., ["North", "West"]
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
        // Decorate the riser as later edits would: its own cid + an ope.
        let riser_cid = ItemId::default();
        let ope = ItemId::default();
        walls1.regions[1].construction_id = riser_cid;
        walls1.regions[1].opes.doors.push(ope);

        // Re-derive from walls1: the perimeter amends, the riser re-derives
        // deterministically — and keeps its identity.
        let walls2 = WallRegions::regenerate(
            segs(),
            &walls1,
            &CeilingRegions::default(),
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
}
