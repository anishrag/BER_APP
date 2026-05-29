// src/data/structural.rs
use geo::{Coord, LineString, Polygon};
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CeilingConstructionType {
    FlatRoof,
    SlopedInsOnRafter,
    SlopedInsOnCeiling,
    SlopedInsOnCeilingAndRafter,
    NonHeatLossCeiling,
    Uninitialised,
    Other,
}

/// Wall construction types
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum WallConstructionType {
    CavityWall,
    FilledCavityWall,
    ConcreteHollowBlocks,
    TimberConstruction,
    ICF,
    Floor,
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

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct RegionBase {
    pub id: ItemId,
    pub construction_makeup: ConstructionMakeup,
    pub construction_phase: ConstructionPhase,
    pub opes: Opes,
    pub photos: Vec<Photo>,
    pub description: Option<String>,
}

impl RegionBase {
    pub fn clone_with_new_id(&self) -> Self {
        Self {
            id: ItemId::default(),
            ..self.clone() // use regular clone for the rest
        }
    }
}
/// A planar region of the floor
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FloorRegion {
    pub polygon: Polygon<f64>, // 2D polygon boundary
    pub height: f64,           // height per vertex
    pub construction_type: FloorConstructionType,
    pub base: RegionBase,
}

/// A planar region of the floor
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct FloorRegions {
    pub regions: Vec<FloorRegion>, // height per vertex
}

impl FloorRegions {
    /// Create a new FloorRegions
    pub fn new(
        vertices: Vec<Point2f64>,
        height: f64,
        construction_type: Option<FloorConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
    ) -> Self {
        let polygon = Polygon::new(
            LineString::from(vertices.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
            vec![],
        );

        let region = FloorRegion {
            polygon,
            height,
            construction_type: construction_type.unwrap_or_default(),
            base: RegionBase {
                construction_makeup: construction_makeup.unwrap_or_default(),
                construction_phase: construction_phase.unwrap_or_default(),
                ..Default::default()
            },
        };

        Self {
            regions: vec![region],
        }
    }

    /// Create a FloorRegions containing one region per polygon. Used when a
    /// storey has multiple disjoint floor areas (e.g. extension sections on
    /// upper floors). All regions get the same height + construction defaults.
    /// (Distinct from `from_polygons` — that one takes `&Polygon` slices for
    /// gen_3d_vol's 3D-derived polygons; this one takes raw vertex lists
    /// straight from the parametric layer.)
    pub fn from_vertex_lists(
        polygons: Vec<Vec<Point2f64>>,
        height: f64,
        construction_type: Option<FloorConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
    ) -> Self {
        let ct = construction_type.unwrap_or_default();
        let cm = construction_makeup.unwrap_or_default();
        let cp = construction_phase.unwrap_or_default();

        let regions = polygons
            .into_iter()
            .filter(|p| p.len() >= 3)
            .map(|vertices| {
                let polygon = Polygon::new(
                    LineString::from(vertices.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
                    vec![],
                );
                FloorRegion {
                    polygon,
                    height,
                    construction_type: ct,
                    base: RegionBase {
                        construction_makeup: cm.clone(),
                        construction_phase: cp.clone(),
                        ..Default::default()
                    },
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
}
/// A planar region of the floor
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CeilingRegions {
    pub regions: Vec<CeilingRegion>, // height per vertex
}
impl CeilingRegions {
    /// Create a new CeilingRegions
    pub fn new(
        vertices: Vec<Point3f64>,
        construction_type: Option<CeilingConstructionType>,
        construction_makeup: Option<ConstructionMakeup>,
        construction_phase: Option<ConstructionPhase>,
    ) -> Self {
        Self {
            regions: vec![CeilingRegion {
                polygon: Polygon3D::new(vertices),
                construction_type: construction_type
                    .unwrap_or(CeilingConstructionType::Uninitialised),
                base: RegionBase {
                    id: ItemId::default(),
                    construction_makeup: construction_makeup.unwrap_or_default(),
                    construction_phase: construction_phase.unwrap_or(ConstructionPhase::Original),
                    opes: Opes::default(),
                    photos: Vec::new(),
                    description: None,
                },
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
/// A planar region of the ceiling
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CeilingRegion {
    pub base: RegionBase,
    pub polygon: Polygon3D, // 2D polygon boundary
    pub construction_type: CeilingConstructionType,
}

impl Default for FloorRegion {
    fn default() -> Self {
        Self {
            polygon: Polygon::new(LineString::new(vec![]), vec![]),
            height: f64::default(),
            construction_type: FloorConstructionType::default(),
            base: RegionBase::default(),
        }
    }
}

/// A vertical planar wall region
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WallRegion {
    pub base: RegionBase,
    pub polygon: Polygon3D, // 3D polygon (vertical plane, typically 4 vertices)
    pub construction_type: WallConstructionType,
    pub exposure: WallExposure,
}

/// Collection of wall regions
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WallRegions {
    pub regions: Vec<WallRegion>,
}

impl WallRegions {
    /// Re-derive the storey's walls from `segments` (each a footprint edge
    /// `(a, b)` with its thermal `exposure`), PRESERVING fabric properties from
    /// the `previous` regions. Each segment becomes a vertical rectangular plane
    /// rising from `base_z` to `base_z + height` (4 vertices: a-bottom, b-bottom,
    /// b-top, a-top). Its construction (`construction_type`, `construction_makeup`,
    /// `construction_phase`, `description`) is inherited from whichever previous
    /// region covered the segment's midpoint — so when a wall is split into
    /// pieces, each piece inherits the parent wall's properties and only the
    /// `exposure` differs. A fresh segment (no covering region) gets defaults.
    ///
    /// NB: per-region `id`, `opes` (openings) and `photos` are NOT carried over
    /// (they're positional — ambiguous to split); they reset until there's an
    /// openings model that knows which piece each belongs to.
    pub fn rebuild_preserving(
        segments: Vec<(Point2f64, Point2f64, WallExposure)>,
        previous: &WallRegions,
        base_z: f64,
        height: f64,
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
                match previous.region_covering(mid) {
                    Some(src) => WallRegion {
                        base: RegionBase {
                            construction_makeup: src.base.construction_makeup.clone(),
                            construction_phase: src.base.construction_phase.clone(),
                            description: src.base.description.clone(),
                            ..RegionBase::default()
                        },
                        polygon,
                        construction_type: src.construction_type,
                        exposure,
                    },
                    None => WallRegion {
                        base: RegionBase::default(),
                        polygon,
                        construction_type: WallConstructionType::Uninitialised,
                        exposure,
                    },
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
    /// Opaque serialized snapshot of the parametric sketch model + dimensioning
    /// state for this storey, written by the editor so it can resume editing
    /// with all collinearity equivalences and entered dimensions intact. The
    /// `floors`/`walls` regions above are the *resolved* geometry derived from
    /// it (used by non-editing consumers); this is the editable source. Held as
    /// an opaque string so `datatypes` stays independent of the geometry
    /// library. `None`/unparseable → the editor rebuilds from `floors`.
    #[serde(default)]
    pub sketch_state: Option<String>,
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

    #[test]
    fn rebuild_makes_vertical_planes_with_exposure() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(4.0, 0.0);
        let walls = WallRegions::rebuild_preserving(
            vec![(a, b, WallExposure::SemiExposed)],
            &WallRegions::default(),
            0.0,
            2.4,
        );
        assert_eq!(walls.regions.len(), 1);
        let w = &walls.regions[0];
        assert_eq!(w.exposure, WallExposure::SemiExposed);
        assert_eq!(w.construction_type, WallConstructionType::Uninitialised);
        // 4 vertices: base edge then top edge, vertical plane 0 -> 2.4.
        let ext = &w.polygon.exterior;
        assert_eq!(ext.len(), 4);
        assert_eq!((ext[0].x, ext[0].y, ext[0].z), (0.0, 0.0, 0.0));
        assert_eq!((ext[1].x, ext[1].y, ext[1].z), (4.0, 0.0, 0.0));
        assert_eq!((ext[2].x, ext[2].y, ext[2].z), (4.0, 0.0, 2.4));
        assert_eq!((ext[3].x, ext[3].y, ext[3].z), (0.0, 0.0, 2.4));
    }

    #[test]
    fn rebuild_inherits_construction_from_covering_region_when_split() {
        let a = Point2f64::new(0.0, 0.0);
        let b = Point2f64::new(10.0, 0.0);
        // A whole-wall region with a non-default construction.
        let mut previous = WallRegions::rebuild_preserving(
            vec![(a, b, WallExposure::Exposed)],
            &WallRegions::default(),
            0.0,
            2.4,
        );
        previous.regions[0].construction_type = WallConstructionType::CavityWall;
        previous.regions[0].base.description = Some("party wall".into());

        // Split it into 3 pieces; each should inherit the construction.
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
        );
        assert_eq!(split.regions.len(), 3);
        for r in &split.regions {
            assert_eq!(r.construction_type, WallConstructionType::CavityWall);
            assert_eq!(r.base.description.as_deref(), Some("party wall"));
        }
        // Exposure still set per piece.
        assert_eq!(split.regions[1].exposure, WallExposure::SemiExposed);
    }
}
