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
    /// Side hints for apexes on an internal shared boundary (a mono-pitch ridge
    /// along the house↔extension junction) — which ceiling area each slopes.
    /// Keyed by apex point (see [`RoofApexHint`]); empty for normal apexes.
    /// `#[serde(default)]` so older records load.
    #[serde(default)]
    pub roof_apex_hints: Vec<RoofApexHint>,
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
    /// regions-as-truth roofs are the geometric truth, this is the editable
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
    #[serde(default)]
    pub roof_splits: Vec<Vec<Point2f64>>,
    /// Structure-phase WALL splits: each the plan SEGMENT of a finalized cut
    /// that crosses a wall's base edge. A wall split is a vertical plane (not an
    /// area), so the segment — not a closed ring — is stored: resolve lifts it to
    /// the crossing point + along-distance to split the wall and re-home its
    /// openings (`WallRegions::apply_splits`). `#[serde(default)]` so older
    /// records load.
    #[serde(default)]
    pub wall_splits: Vec<[Point2f64; 2]>,
    // No editable sketch snapshot is stored: the resolved geometry above
    // (`floors`/`walls`/`living_areas`) is the only persisted representation,
    // and the editor rebuilds its working state from it on load. The DB only
    // ever holds finalised (complete, metre-scale) geometry.
}

impl Storey {
    /// Record a dimensioned Structure-phase split onto the durable split field
    /// for `kind` (replayed by `resolve_storey`). Floors/roofs store the whole
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
            SurfaceKind::Roofs => self.roof_splits.push(ring.to_vec()),
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

    /// Drop all DERIVED geometry — floors, walls, living areas, roofs, and the
    /// fan apexes over them — back to empty. The persist path calls this when
    /// the outline isn't finalised yet: that geometry is still pixel/incomplete
    /// (roofs derived over a pixel footprint are pixel units too, so the apexes
    /// that fan them go with it), and the DB only ever holds finalised geometry.
    /// The durable INPUTS (`ceiling_height_m`, `roof_apex_hints`,
    /// `floor_thickness_m`, `ceiling_steps`) are left untouched.
    pub fn clear_derived_geometry(&mut self) {
        self.floors = FloorRegions::default();
        self.walls = WallRegions::default();
        self.living_areas = FloorRegions::default();
        self.roofs = CeilingRegions::default();
        self.roof_apexes = Vec::new();
    }

    /// How many of each structural surface the storey holds — the
    /// Structure-phase tool read-out (how many walls / roofs / floors the
    /// diagram shows). Counts the derived regions, so they're meaningful only
    /// after the outline is finalised (walls/roofs/floors are re-derived by
    /// `document::resolve_storey`).
    pub fn surface_counts(&self) -> SurfaceCounts {
        SurfaceCounts {
            walls: self.walls.regions.len(),
            roofs: self.roofs.regions.len(),
            floors: self.floors.regions.len(),
        }
    }

    /// The id of the surface of `kind` under (or, for walls, nearest within
    /// `tol` of) the plan point `(x, y)` — the Structure-phase tap hit-test.
    /// Floors/roofs: point-in-polygon over the region footprint; walls: nearest
    /// base edge within tolerance (a wall collapses to its base edge in plan).
    pub fn surface_at(&self, kind: SurfaceKind, x: f64, y: f64, tol: f64) -> Option<ItemId> {
        match kind {
            SurfaceKind::Floors => self
                .floors
                .regions
                .iter()
                .find(|r| r.polygon.contains(&GeoPoint::new(x, y)))
                .map(|r| r.id),
            SurfaceKind::Roofs => self
                .roofs
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
    /// roofs match when their footprint centre is inside; a wall (a BOUNDARY
    /// edge, never strictly inside) matches when its base-edge midpoint sits on
    /// the ring's perimeter OR strictly inside (an interior riser between
    /// stepped sections), the spanning wall claimed by whichever region holds
    /// its midpoint.
    pub fn surfaces_in_ring(&self, kind: SurfaceKind, region_ring: &[Point2f64]) -> Vec<ItemId> {
        let ring_poly = ring_to_geo(region_ring);
        let inside = |x: f64, y: f64| ring_poly.contains(&GeoPoint::new(x, y));
        match kind {
            SurfaceKind::Floors => self
                .floors
                .regions
                .iter()
                .filter(|r| {
                    let c = ring_centroid(&floor_ring(&r.polygon));
                    inside(c.x, c.y)
                })
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Roofs => self
                .roofs
                .regions
                .iter()
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

    /// Which kind of surface region `id` is, if any — lets a mixed-kind
    /// selection (walls + floors + roofs) recover each id's kind. `None` when no
    /// region of any kind has that id.
    pub fn surface_kind_of(&self, id: ItemId) -> Option<SurfaceKind> {
        if self.walls.regions.iter().any(|r| r.id == id) {
            Some(SurfaceKind::Walls)
        } else if self.roofs.regions.iter().any(|r| r.id == id) {
            Some(SurfaceKind::Roofs)
        } else if self.floors.regions.iter().any(|r| r.id == id) {
            Some(SurfaceKind::Floors)
        } else {
            None
        }
    }

    /// Every surface id of `kind` on the storey (the `Select all` seed).
    pub fn surface_ids(&self, kind: SurfaceKind) -> Vec<ItemId> {
        match kind {
            SurfaceKind::Walls => self.walls.regions.iter().map(|r| r.id).collect(),
            SurfaceKind::Roofs => self.roofs.regions.iter().map(|r| r.id).collect(),
            SurfaceKind::Floors => self.floors.regions.iter().map(|r| r.id).collect(),
        }
    }
}

/// Per-storey tally of structural surfaces, one field per kind. Drives the
/// Structure-phase tool counts (and a future structure checklist).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SurfaceCounts {
    pub walls: usize,
    pub roofs: usize,
    pub floors: usize,
}

/// A structural surface kind — the Structure-phase headings (Walls / Roofs /
/// Floors) the surface queries and the assessor's Select/Set subtools hang
/// under. The data-model taxonomy the queries above (`surface_at`,
/// `surfaces_in_ring`, `surface_ids`) dispatch on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SurfaceKind {
    Walls,
    Roofs,
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
