// src/data/structural.rs
use geo::{Contains, Coord, LineString, MultiPolygon, Point as GeoPoint, Polygon};
use serde::{Deserialize, Serialize};

use super::ber_struct::Ber;
use super::common::{ConstructionPhase, Item, ItemId, Photo};
use super::fenestration::{Door, Window};

use domain::points::{point_to_segment_distance, Point2f64, Point3f64};
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
/// referenced by `construction_id`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FloorRegion {
    pub id: ItemId,
    pub polygon: Polygon<f64>, // 2D polygon boundary
    pub height: f64,           // height per vertex
    pub construction_id: ItemId,
}

/// A planar region of the floor
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FloorRegions {
    pub regions: Vec<FloorRegion>, // height per vertex
}

impl FloorRegions {
    /// Create a `FloorRegions` with a single region referencing `construction_id`.
    pub fn new(vertices: Vec<Point2f64>, height: f64, construction_id: ItemId) -> Self {
        let polygon = Polygon::new(
            LineString::from(vertices.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
            vec![],
        );

        let region = FloorRegion {
            id: ItemId::default(),
            polygon,
            height,
            construction_id,
        };

        Self {
            regions: vec![region],
        }
    }

    /// Create a FloorRegions containing one region per polygon. Used when a
    /// storey has multiple disjoint floor areas (e.g. extension sections on
    /// upper floors). All regions share the same height and `construction_id`.
    /// (Distinct from `from_polygons` — that one takes `&Polygon` slices for
    /// gen_3d_vol's 3D-derived polygons; this one takes raw vertex lists
    /// straight from the parametric layer.)
    pub fn from_vertex_lists(
        polygons: Vec<Vec<Point2f64>>,
        height: f64,
        construction_id: ItemId,
    ) -> Self {
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
                    height,
                    construction_id,
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
        height: f64,
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
            height,
            construction_id: cid,
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
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Polygon3D {
    pub exterior: Vec<Point3f64>,
}

impl Polygon3D {
    pub fn new(exterior: Vec<Point3f64>) -> Self {
        Self { exterior }
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

impl Default for FloorRegion {
    fn default() -> Self {
        Self {
            id: ItemId::default(),
            polygon: Polygon::new(LineString::new(vec![]), vec![]),
            height: f64::default(),
            construction_id: ItemId::default(),
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
    /// `(a, b)` with its thermal `exposure`), PRESERVING the construction link
    /// from the `previous` regions. Each segment becomes a vertical rectangular
    /// plane rising from `base_z` to `base_z + height` (4 vertices: a-bottom,
    /// b-bottom, b-top, a-top). Its `construction_id` is inherited from whichever
    /// previous region covered the segment's midpoint — so when a wall is split
    /// into pieces, each piece keeps the parent's (shared) construction and only
    /// the `exposure` differs. A fresh segment (no covering region) gets
    /// `default_construction_id` (the caller supplies the id of an
    /// "uninitialised" construction it has ensured exists in the BER).
    ///
    /// NB: per-region `id` and `opes` (openings) are NOT carried over (they're
    /// positional — ambiguous to split); they reset until there's an openings
    /// model that knows which piece each belongs to.
    pub fn rebuild_preserving(
        segments: Vec<(Point2f64, Point2f64, WallExposure)>,
        previous: &WallRegions,
        base_z: f64,
        height: f64,
        default_construction_id: ItemId,
    ) -> Self {
        let top = base_z + height;
        let regions = segments
            .into_iter()
            .map(|(a, b, exposure)| {
                let polygon = Polygon3D::new(vec![
                    Point3f64::new(a.x, a.y, base_z),
                    Point3f64::new(b.x, b.y, base_z),
                    Point3f64::new(b.x, b.y, top),
                    Point3f64::new(a.x, a.y, top),
                ]);
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
        let fr = FloorRegions::partition(&outline, &[], rem, 2.4);
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
            2.4,
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
            2.4,
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
    fn rebuild_makes_vertical_planes_with_exposure() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(4.0, 0.0);
        let default_cid = ItemId::default();
        let walls = WallRegions::rebuild_preserving(
            vec![(a, b, WallExposure::SemiExposed)],
            &WallRegions::default(),
            0.0,
            2.4,
            default_cid,
        );
        assert_eq!(walls.regions.len(), 1);
        let w = &walls.regions[0];
        assert_eq!(w.exposure, WallExposure::SemiExposed);
        // Fresh segment (no covering region) takes the supplied default.
        assert_eq!(w.construction_id, default_cid);
        // 4 vertices: base edge then top edge, vertical plane 0 -> 2.4.
        let ext = &w.polygon.exterior;
        assert_eq!(ext.len(), 4);
        assert_eq!((ext[0].x, ext[0].y, ext[0].z), (0.0, 0.0, 0.0));
        assert_eq!((ext[1].x, ext[1].y, ext[1].z), (4.0, 0.0, 0.0));
        assert_eq!((ext[2].x, ext[2].y, ext[2].z), (4.0, 0.0, 2.4));
        assert_eq!((ext[3].x, ext[3].y, ext[3].z), (0.0, 0.0, 2.4));
    }

    #[test]
    fn rebuild_inherits_construction_id_from_covering_region_when_split() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        // A whole-wall region pointing at some shared construction.
        let orig_cid = ItemId::default();
        let previous = WallRegions::rebuild_preserving(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            0.0,
            2.4,
            orig_cid,
        );
        assert_eq!(previous.regions[0].construction_id, orig_cid);

        // Split it into 3 pieces; each should inherit the covered construction,
        // NOT the fresh default passed for uncovered segments.
        let fresh_cid = ItemId::default();
        let split = WallRegions::rebuild_preserving(
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
            0.0,
            2.4,
            fresh_cid,
        );
        assert_eq!(split.regions.len(), 3);
        for r in &split.regions {
            assert_eq!(r.construction_id, orig_cid);
        }
        // Exposure still set per piece.
        assert_eq!(split.regions[1].exposure, WallExposure::SemiExposed);
    }
}
