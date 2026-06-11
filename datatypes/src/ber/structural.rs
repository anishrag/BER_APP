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
    /// `split_to_simple`). Sub-mm² slivers dropped (same rationale as
    /// `FloorRegions::with_levels`). Pieces inherit `construction_id`, get
    /// fresh ids and default `opes` (openings are dropped on split — region
    /// identity doesn't survive a re-split). Assumes FLAT input regions (all
    /// current content is — see `flat_z`); sloped regions are the later
    /// roofs feature.
    pub fn with_z_deltas(self, areas: &[(Vec<Point2f64>, f64)]) -> Self {
        use geo::bool_ops::BooleanOps;
        use geo::Area;
        /// Pieces smaller than this (m²) are boolean-op debris, not ceilings.
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

    /// FAN the regions by roof `apexes`, applied IN ORDER (the last step of
    /// the composition: default ceiling → step areas → apexes). Each apex
    /// fans every region whose plan projection contains it — containment is
    /// boundary-INCLUSIVE (`geo` `Intersects`, not `Contains`): a re-applied
    /// apex is a CORNER of its own fan triangles and must still find them so
    /// re-fanning is the identity (assign-time validation deliberately uses
    /// the strict predicate instead — [`region_strictly_containing`]; two
    /// different predicates by design). A fanned region becomes one triangle
    /// per boundary edge: the edge keeps its per-vertex z (no flat-input
    /// stamp like `with_z_deltas`, so fans compose over stepped — and,
    /// structurally, already-fanned — pieces), the apex sits at `z_m`.
    /// Sub-mm² (plan) sliver triangles are dropped — exactly the degenerate
    /// pair incident to an apex that is already a corner, which is what makes
    /// re-fan-to-self the identity. Fan triangles are SIMPLE by construction
    /// (three exterior vertices, no interior rings), so unlike `with_z_deltas`
    /// no hole decomposition (`split_to_simple`) is needed here. An apex
    /// contained by NO region is warn-logged and skipped (never rejected —
    /// the assessor may be mid-edit). Pieces inherit `construction_id`, get
    /// fresh ids and default `opes`. Empty `apexes` → `self` unchanged.
    ///
    /// [`region_strictly_containing`]: CeilingRegions::region_strictly_containing
    pub fn with_apexes(self, apexes: &[RoofApex]) -> Self {
        use geo::Intersects;
        /// Triangles with plan area below this (m²) are fan debris, not
        /// ceilings (same threshold as `with_z_deltas`).
        const SLIVER_AREA_M2: f64 = 1e-6;
        if apexes.is_empty() {
            return self;
        }
        let mut regions = self.regions;
        for apex in apexes {
            let p = GeoPoint::new(apex.point.x, apex.point.y);
            let mut next: Vec<CeilingRegion> = Vec::new();
            let mut hit = false;
            for region in regions {
                let plan: Polygon<f64> = region.polygon.clone().into();
                if region.polygon.exterior.len() < 3 || !plan.intersects(&p) {
                    next.push(region);
                    continue;
                }
                hit = true;
                let ext = &region.polygon.exterior;
                let n = ext.len();
                for i in 0..n {
                    let a = ext[i];
                    let b = ext[(i + 1) % n];
                    // Plan area via the cross product — degenerate triangles
                    // (the two edges INCIDENT to an apex that is already a
                    // corner) fall below the sliver threshold and drop.
                    let area = ((b.x - a.x) * (apex.point.y - a.y)
                        - (b.y - a.y) * (apex.point.x - a.x))
                        .abs()
                        / 2.0;
                    if area < SLIVER_AREA_M2 {
                        continue;
                    }
                    next.push(CeilingRegion {
                        id: ItemId::default(),
                        polygon: Polygon3D::new(vec![
                            a,
                            b,
                            Point3f64::new(apex.point.x, apex.point.y, apex.z_m),
                        ]),
                        opes: Opes::default(),
                        construction_id: region.construction_id,
                    });
                }
            }
            if !hit {
                log::warn!(
                    "roof apex at ({:.3}, {:.3}) lies in no ceiling region — skipped",
                    apex.point.x,
                    apex.point.y
                );
            }
            regions = next;
        }
        Self { regions }
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
    /// vertex matches an apex. `None` when fewer than two base vertices
    /// remain or the remainder isn't level (a genuinely irregular slope).
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
        if rest.is_empty() {
            return None;
        }
        rest.iter().all(|z| (z - z0).abs() <= EPS_M).then_some(z0)
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
        /// Both endpoints of a base edge must sit within this (m) of a
        /// previous region's for the segment to be the SAME wall.
        const MATCH_EPS_M: f64 = 1e-2;
        let dist =
            |u: Point2f64, vx: f64, vy: f64| ((u.x - vx).powi(2) + (u.y - vy).powi(2)).sqrt();
        // Every qualifying (error, segment, previous) pair, then greedy
        // assignment best-first — injective by construction.
        let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
        for (si, (a, b, _)) in segments.iter().enumerate() {
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
        let mut amend_from: Vec<Option<usize>> = vec![None; segments.len()];
        let mut prev_used = vec![false; previous.regions.len()];
        for (_, si, pi) in candidates {
            if amend_from[si].is_none() && !prev_used[pi] {
                amend_from[si] = Some(pi);
                prev_used[pi] = true;
            }
        }

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

// ── Roof-profile sampling (private helpers for `WallRegions::regenerate`) ──

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

/// The plane of the first roof region whose plan projection touches `p`.
/// Containment is boundary-INCLUSIVE (`Intersects`): a wall base lies ON the
/// outline edge that bounds its roof region, so the strict predicate would
/// never match. When two regions share the boundary `p` sits on (a ceiling
/// step running ALONG the wall), the first in region order wins — callers
/// sample interval midpoints, which are never at a crossing breakpoint.
fn covering_roof_plane(roofs: &CeilingRegions, p: Point2f64) -> Option<(f64, f64, f64)> {
    use geo::Intersects;
    let gp = GeoPoint::new(p.x, p.y);
    roofs.regions.iter().find_map(|r| {
        Polygon::<f64>::from(r.polygon.clone())
            .intersects(&gp)
            .then(|| region_plane(r))
            .flatten()
    })
}

/// Parameters t ∈ [0, 1] along the base `a`→`b` where the covering roof
/// region can change: every crossing of a region boundary edge with the base
/// (collinear overlapping edges contribute their projected endpoints), plus
/// the ends 0 and 1. Sorted ascending; the caller treats near-equal values
/// (Δt ≤ 1e-9) as one breakpoint.
fn profile_breakpoints(roofs: &CeilingRegions, a: Point2f64, b: Point2f64) -> Vec<f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let mut ts = vec![0.0, 1.0];
    if len2 <= 1e-18 {
        return ts;
    }
    for r in &roofs.regions {
        let ext = &r.polygon.exterior;
        let n = ext.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let p = ext[i];
            let q = ext[(i + 1) % n];
            let (ex, ey) = (q.x - p.x, q.y - p.y);
            let (wx, wy) = (p.x - a.x, p.y - a.y);
            let denom = dx * ey - dy * ex;
            if denom.abs() > 1e-12 {
                // Proper crossing: base(t) == edge(s); keep t when both
                // parameters land on their segments.
                let t = (wx * ey - wy * ex) / denom;
                let s = (wx * dy - wy * dx) / denom;
                if (-1e-9..=1.0 + 1e-9).contains(&s) && (0.0..=1.0).contains(&t) {
                    ts.push(t);
                }
            } else if (wx * dy - wy * dx).abs() <= 1e-9 * len2.sqrt() {
                // Collinear overlap (e.g. the roof edge along the wall base
                // itself): the region can start/stop at the edge's endpoints.
                for c in [p, q] {
                    let t = ((c.x - a.x) * dx + (c.y - a.y) * dy) / len2;
                    if (0.0..=1.0).contains(&t) {
                        ts.push(t);
                    }
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
    let at = |t: f64| Point2f64::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t);
    // (t0, t1, z at t0, z at t1) per interval, ascending t.
    let mut spans: Vec<(f64, f64, f64, f64)> = Vec::new();
    for w in profile_breakpoints(roofs, a, b).windows(2) {
        let (t0, t1) = (w[0], w[1]);
        if t1 - t0 <= 1e-9 {
            continue;
        }
        let span = match covering_roof_plane(roofs, at((t0 + t1) / 2.0)) {
            Some((c0, cx, cy)) => {
                let (p0, p1) = (at(t0), at(t1));
                (
                    t0,
                    t1,
                    c0 + cx * p0.x + cy * p0.y,
                    c0 + cx * p1.x + cy * p1.y,
                )
            }
            None => (t0, t1, fallback_top_z, fallback_top_z),
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
    fn with_apexes_applies_in_order_across_regions() {
        // Two disjoint rings; one apex each — both fan independently.
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

        // Degenerate: fewer than two non-apex vertices remain.
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
        assert_eq!(needle.fan_base_z(&[apex]), None);
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
