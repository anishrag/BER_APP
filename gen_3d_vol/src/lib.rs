mod ceiling;
mod floor;
mod geometry;
mod types;
mod walls;

use domain::points::{Point2f64, Point3f64};
use geo::{BooleanOps, LineString, Polygon};

use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::ItemId;

pub use datatypes::ber::structural::FloorConstructionType;
use datatypes::ber::structural::{
    CeilingConstruction, CeilingConstructionType, CeilingRegion, CeilingRegions, ConstructionBase,
    ConstructionMakeup, FloorConstruction, FloorRegion, FloorRegions, WallConstruction,
    WallConstructionType, WallRegion, WallRegions,
};

use crate::ceiling::CeilingRegionsManip;
use crate::floor::FloorRegionsManip;
use crate::geometry::{Geometry, MergeableGeometry, MergeableRegion};
use crate::types::{MergeWarning, RegionContainer, RegionType, EPSILON};
use crate::walls::WallRegionsManip;

pub use datatypes::ber::common::ConstructionPhase;

pub enum Gen3DError {
    PointNotOnEdge,
    HeightConflict,
    CeilingError,
}

/// Build a 2D `geo` polygon from an ordered vertex ring.
fn vertices_to_polygon(vertices: &[Point2f64]) -> Polygon<f64> {
    Polygon::new(
        LineString::from(
            vertices
                .iter()
                .map(|p| (p.x, p.y))
                .collect::<Vec<(f64, f64)>>(),
        ),
        vec![],
    )
}

pub struct Gen3D {
    pub storey: u32,
    pub floors: FloorRegions,
    pub walls: WallRegions,
    pub ceilings: CeilingRegions,
    /// Self-contained construction registry. Regions reference a construction by
    /// `construction_id`; these tables hold the actual fabric (type/make-up/phase
    /// /photos/description). `Gen3D` keeps its OWN copy of the `Ber`'s house-level
    /// tables (cloned in by `from_ber_storey`) so its `construction_id`s resolve.
    pub wall_constructions: Vec<WallConstruction>,
    pub floor_constructions: Vec<FloorConstruction>,
    pub ceiling_constructions: Vec<CeilingConstruction>,
}

/// Find-or-create construction registry + resolver accessors. Mirrors the
/// `ensure_*` pattern on `Ber`: many regions share one construction entry rather
/// than each duplicating fabric. The `*_construction` accessors map a region's
/// `construction_id` back to its construction (or `None` if dangling).
impl Gen3D {
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

    /// Resolve a wall region's `construction_id` to its construction.
    pub fn wall_construction(&self, id: ItemId) -> Option<&WallConstruction> {
        self.wall_constructions.iter().find(|c| c.base.id == id)
    }

    /// Resolve a floor region's `construction_id` to its construction.
    pub fn floor_construction(&self, id: ItemId) -> Option<&FloorConstruction> {
        self.floor_constructions.iter().find(|c| c.base.id == id)
    }

    /// Resolve a ceiling region's `construction_id` to its construction.
    pub fn ceiling_construction(&self, id: ItemId) -> Option<&CeilingConstruction> {
        self.ceiling_constructions.iter().find(|c| c.base.id == id)
    }
}

//TODO: ddecide where storey lives
impl Gen3D {
    // costructors
    pub fn new_floor(
        vertices: Vec<Point2f64>,
        height: f64,
        construction_type: FloorConstructionType,
        floor_plan_below: Option<&Gen3D>,
    ) -> Self {
        // Every region of this storey's floor shares one construction; create it
        // up-front so we have an id to attach to each region.
        let mut floor_constructions = Vec::new();
        let construction = FloorConstruction {
            base: ConstructionBase::default(),
            construction_type,
        };
        let construction_id = construction.base.id;
        floor_constructions.push(construction);

        let floor_regions;
        let mut storey = 0;
        match floor_plan_below {
            Some(floor_plan) => {
                let floor_polygon_2d = Polygon::new(
                    LineString::from(
                        vertices
                            .iter()
                            .map(|p| (p.x, p.y))
                            .collect::<Vec<(f64, f64)>>(),
                    ),
                    vec![],
                );
                let ceiling_regions_below = &floor_plan.ceilings;
                let floor_polygons: Vec<(Polygon<f64>, f64)> = ceiling_regions_below
                    .regions
                    .iter()
                    .flat_map(|region| region.polygon.clone().intersection(&floor_polygon_2d))
                    .map(|poly3d| {
                        let (poly2d, heights) = poly3d.to_2d_polygon_and_heights();
                        // Assert all heights are the same
                        assert!(heights.iter().all(|&h| (h - heights[0]).abs() < EPSILON));
                        (poly2d, heights[0])
                    })
                    .collect();
                let (polygons, heights): (Vec<&Polygon>, Vec<f64>) =
                    floor_polygons.iter().map(|(p, h)| (p, *h)).unzip();
                floor_regions = FloorRegions::from_polygons(polygons, heights, construction_id);
                storey = floor_plan.storey + 1;
            }
            None => {
                floor_regions = FloorRegions::new(vertices, height, construction_id);
            }
        }

        Self {
            storey,
            floors: floor_regions,
            walls: WallRegions { regions: vec![] },
            ceilings: CeilingRegions { regions: vec![] },
            wall_constructions: vec![],
            floor_constructions,
            ceiling_constructions: vec![],
        }
    }

    // ----------------------------------------------------------------------------------------------
    // --- Floor modification methods ---
    // ----------------------------------------------------------------------------------------------
    pub fn raise_lower_floor_split(&mut self, vertices: Vec<Point2f64>, split_height_change: f64) {
        let polygon = Polygon::new(
            LineString::from(
                vertices
                    .iter()
                    .map(|p| (p.x, p.y))
                    .collect::<Vec<(f64, f64)>>(),
            ),
            vec![],
        );
        self.floors
            .raise_lower_polygon(polygon, split_height_change);
    }

    /// Read the (type, make-up, phase) of the floor construction currently
    /// covering `polygon`, so a single fabric field can be changed while the
    /// others are preserved. Picks the first floor region overlapping `polygon`;
    /// falls back to defaults if none overlap or the id is dangling.
    fn current_floor_construction_values(
        &self,
        polygon: &Polygon<f64>,
    ) -> (FloorConstructionType, ConstructionMakeup, ConstructionPhase) {
        let cid = self
            .floors
            .regions
            .iter()
            .find(|r| !r.polygon.intersection(polygon).0.is_empty())
            .map(|r| r.construction_id);
        match cid.and_then(|id| self.floor_construction(id)) {
            Some(c) => (
                c.construction_type,
                c.base.construction_makeup.clone(),
                c.base.construction_phase.clone(),
            ),
            None => (
                FloorConstructionType::default(),
                ConstructionMakeup::default(),
                ConstructionPhase::Original,
            ),
        }
    }

    pub fn change_floor_construction_phase_split(
        &mut self,
        vertices: Vec<Point2f64>,
        phase: ConstructionPhase,
    ) {
        let polygon = vertices_to_polygon(&vertices);
        let (ct, makeup, _) = self.current_floor_construction_values(&polygon);
        let construction_id = self.ensure_floor_construction(ct, makeup, phase);
        self.floors
            .set_polygon_construction(polygon, construction_id);
    }

    pub fn change_floor_construction_type_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_type: FloorConstructionType,
    ) {
        let polygon = vertices_to_polygon(&vertices);
        let (_, makeup, phase) = self.current_floor_construction_values(&polygon);
        let construction_id = self.ensure_floor_construction(construction_type, makeup, phase);
        self.floors
            .set_polygon_construction(polygon, construction_id);
    }

    pub fn change_floor_construction_makeup_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_makeup: ConstructionMakeup,
    ) {
        let polygon = vertices_to_polygon(&vertices);
        let (ct, _, phase) = self.current_floor_construction_values(&polygon);
        let construction_id = self.ensure_floor_construction(ct, construction_makeup, phase);
        self.floors
            .set_polygon_construction(polygon, construction_id);
    }

    // ----------------------------------------------------------------------------------------------
    // --- Ceiling modification methods ---
    // ----------------------------------------------------------------------------------------------
    pub fn add_ceiling_with_height(&mut self, height: f64) {
        let first_floor_height = self.floors.regions[0].height;
        let height = first_floor_height + height;
        let exterior_polygon = self
            .floors
            .get_clockwise_3d_polygon_starting_closest_to_origin();
        let vertices_2d: Vec<Point2f64> = exterior_polygon.into();
        let vertices_3d: Vec<Point3f64> = vertices_2d
            .into_iter()
            .map(|p| Point3f64::new(p.x, p.y, height))
            .collect();
        // The whole ceiling shares one (uninitialised) construction.
        let construction_id = self.ensure_ceiling_construction(
            CeilingConstructionType::default(),
            ConstructionMakeup::default(),
            ConstructionPhase::Original,
        );
        self.ceilings = CeilingRegions::new(vertices_3d, construction_id);
    }

    pub fn raise_lower_ceiling_split(
        &mut self,
        vertices: Vec<Point2f64>,
        split_height_change: f64,
    ) {
        let polygon = vertices_to_polygon(&vertices);
        self.ceilings
            .modify_polygon_increase_height(polygon, split_height_change);
    }

    pub fn set_point_height_ceiling(
        &mut self,
        point: Point2f64,
        new_height: f64,
        from_highest_region: bool,
    ) -> Result<(), Gen3DError> {
        let floor_heights = self.floors.get_floor_height_at_point(&point);
        let floor_z = if from_highest_region {
            floor_heights
                .iter()
                .copied()
                .max_by(|a, b| a.partial_cmp(b).unwrap())
                .unwrap()
        } else {
            floor_heights
                .iter()
                .copied()
                .min_by(|a, b| a.partial_cmp(b).unwrap())
                .unwrap()
        };

        self.ceilings.insert_vertex(point, new_height + floor_z);

        Ok(())
    }

    /// Read the (type, make-up, phase) of the ceiling construction currently
    /// covering `polygon`, preserving the unchanged fields when one is edited.
    /// Falls back to defaults if no region overlaps or the id is dangling.
    fn current_ceiling_construction_values(
        &self,
        polygon: &Polygon<f64>,
    ) -> (
        CeilingConstructionType,
        ConstructionMakeup,
        ConstructionPhase,
    ) {
        let cid = self
            .ceilings
            .regions
            .iter()
            .find(|r| !r.polygon.intersection(polygon).is_empty())
            .map(|r| r.construction_id);
        match cid.and_then(|id| self.ceiling_construction(id)) {
            Some(c) => (
                c.construction_type,
                c.base.construction_makeup.clone(),
                c.base.construction_phase.clone(),
            ),
            None => (
                CeilingConstructionType::default(),
                ConstructionMakeup::default(),
                ConstructionPhase::Original,
            ),
        }
    }

    pub fn change_ceiling_construction_makeup_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_makeup: ConstructionMakeup,
    ) {
        let polygon = vertices_to_polygon(&vertices);
        let (ct, _, phase) = self.current_ceiling_construction_values(&polygon);
        let construction_id = self.ensure_ceiling_construction(ct, construction_makeup, phase);
        self.ceilings
            .set_polygon_construction(polygon, construction_id);
    }

    pub fn change_ceiling_construction_type_split(
        &mut self,
        vertices: Vec<Point2f64>,
        construction_type: CeilingConstructionType,
    ) {
        let polygon = vertices_to_polygon(&vertices);
        let (_, makeup, phase) = self.current_ceiling_construction_values(&polygon);
        let construction_id = self.ensure_ceiling_construction(construction_type, makeup, phase);
        self.ceilings
            .set_polygon_construction(polygon, construction_id);
    }

    pub fn change_ceiling_construction_phase_split(
        &mut self,
        vertices: Vec<Point2f64>,
        phase: ConstructionPhase,
    ) {
        let polygon = vertices_to_polygon(&vertices);
        let (ct, makeup, _) = self.current_ceiling_construction_values(&polygon);
        let construction_id = self.ensure_ceiling_construction(ct, makeup, phase);
        self.ceilings
            .set_polygon_construction(polygon, construction_id);
    }

    // ----------------------------------------------------------------------------------------------
    // --- Wall modification methods ---
    // ----------------------------------------------------------------------------------------------
    //
    /// Id of an "uninitialised" wall construction (find-or-create). Derived walls
    /// (between floor/ceiling edges, exterior perimeter) reference it until the
    /// user assigns a real construction.
    fn default_wall_construction_id(&mut self) -> ItemId {
        self.ensure_wall_construction(
            WallConstructionType::default(),
            ConstructionMakeup::default(),
            ConstructionPhase::Original,
        )
    }

    pub fn create_walls_betweeen_floor_edges(&mut self) {
        let construction_id = self.default_wall_construction_id();
        self.walls
            .add_from_floor_discontinuities(&self.floors, construction_id, None);
    }

    pub fn create_walls_between_ceiling_edges(&mut self) {
        let construction_id = self.default_wall_construction_id();
        self.walls
            .add_from_ceiling_discontinuities(&self.ceilings, construction_id, None);
    }

    pub fn create_exterior_walls(&mut self) {
        let construction_id = self.default_wall_construction_id();
        self.walls
            .add_from_exterior_perimeter(&self.floors, &self.ceilings, construction_id, None);
    }

    /// Read the (type, make-up, phase) of the construction on wall `wall_id`,
    /// so a single fabric field can be changed while the others are preserved.
    /// Falls back to defaults if the wall or its construction is missing.
    fn wall_construction_values(
        &self,
        wall_id: ItemId,
    ) -> (WallConstructionType, ConstructionMakeup, ConstructionPhase) {
        let cid = self
            .walls
            .regions
            .iter()
            .find(|r| r.id == wall_id)
            .map(|r| r.construction_id);
        match cid.and_then(|id| self.wall_construction(id)) {
            Some(c) => (
                c.construction_type,
                c.base.construction_makeup.clone(),
                c.base.construction_phase.clone(),
            ),
            None => (
                WallConstructionType::default(),
                ConstructionMakeup::default(),
                ConstructionPhase::Original,
            ),
        }
    }

    pub fn change_wall_construction_makeup_split(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        construction_makeup: ConstructionMakeup,
    ) {
        let (ct, _, phase) = self.wall_construction_values(wall_id);
        let construction_id = self.ensure_wall_construction(ct, construction_makeup, phase);
        self.walls
            .split_wall_region(wall_id, split_along_xy, split_along_z, |region| {
                region.construction_id = construction_id;
            });
    }

    pub fn change_wall_construction_phase_split(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        phase: ConstructionPhase,
    ) {
        let (ct, makeup, _) = self.wall_construction_values(wall_id);
        let construction_id = self.ensure_wall_construction(ct, makeup, phase);
        self.walls
            .split_wall_region(wall_id, split_along_xy, split_along_z, |region| {
                region.construction_id = construction_id;
            });
    }

    pub fn change_wall_construction_type_split(
        &mut self,
        wall_id: ItemId,
        split_along_xy: f64,
        split_along_z: f64,
        construction_type: WallConstructionType,
    ) {
        let (_, makeup, phase) = self.wall_construction_values(wall_id);
        let construction_id = self.ensure_wall_construction(construction_type, makeup, phase);
        self.walls
            .split_wall_region(wall_id, split_along_xy, split_along_z, |region| {
                region.construction_id = construction_id;
            });
    }

    // ----------------------------------------------------------------------------------------------
    // --- General modification methods ---
    // ----------------------------------------------------------------------------------------------

    pub fn combine_regions(
        &mut self,
        region_ids: &mut Vec<ItemId>,
        region_type: RegionType,
    ) -> Vec<MergeWarning> {
        match region_type {
            RegionType::Floor => self.combine_regions_generic::<FloorRegion>(region_ids),
            RegionType::Wall => self.combine_regions_generic::<WallRegion>(region_ids),
            RegionType::Ceiling => self.combine_regions_generic::<CeilingRegion>(region_ids),
        }
    }

    fn combine_regions_generic<T>(&mut self, region_ids: &mut Vec<ItemId>) -> Vec<MergeWarning>
    where
        T: MergeableRegion,
        Gen3D: RegionContainer<T>,
    {
        let mut warnings = Vec::new();
        // Select only the regions we want to merge
        let regions = self.regions();
        let mut selected: Vec<&T> = regions
            .iter()
            .filter(|r: &&T| region_ids.contains(&r.id()))
            .collect();

        // Warn for invalid IDs
        let invalid_ids: Vec<ItemId> = region_ids
            .iter()
            .filter(|id| !regions.iter().any(|r| &r.id() == *id))
            .copied()
            .collect();

        if !invalid_ids.is_empty() {
            warnings.push(MergeWarning::InvalidIds(invalid_ids));
        }

        // Process each selected region
        while let Some(region) = selected.pop() {
            let mut group = vec![region.clone()];

            // Find all merge-compatible regions
            selected.retain(|other| {
                if region.is_merge_compatible(other) {
                    group.push((*other).clone());
                    false
                } else {
                    true
                }
            });

            // --- Iterative / convergence merge ---
            // Start with all polygons in the group as separate geometries
            let mut geometries: Vec<T::Geometry> =
                group.iter().map(|r| r.geometry().clone()).collect();

            let mut changed = true;
            while changed {
                changed = false;
                let mut new_geometries = Vec::new();
                let mut skip = vec![false; geometries.len()];

                for i in 0..geometries.len() {
                    if skip[i] {
                        continue;
                    }
                    let mut merged_geom = geometries[i].clone();
                    for j in (i + 1)..geometries.len() {
                        if skip[j] {
                            continue;
                        }
                        // Attempt merge
                        let union_result = merged_geom.merge(&geometries[j]);
                        if union_result.len() == 1 {
                            // Successfully merged
                            merged_geom = union_result.into_iter().next().unwrap();
                            skip[j] = true;
                            changed = true;
                        }
                    }
                    new_geometries.push(merged_geom);
                }
                geometries = new_geometries;
            }
            // --- end convergence merge ---

            let source_ids: Vec<ItemId> = group.iter().map(|r| r.id()).collect();

            // Build new merged regions
            for geom in geometries.iter().cloned() {
                region.with_geometry(geom);
                region_ids.retain(|id| id != &region.id());
            }

            if geometries.len() > 1 {
                warnings.push(MergeWarning::MultipleResultingRegions {
                    source_ids,
                    resulting_regions: geometries.len(),
                });
            }
        }

        warnings
    }
}

impl RegionContainer<FloorRegion> for Gen3D {
    fn regions(&self) -> &Vec<FloorRegion> {
        &self.floors.regions
    }
}
impl RegionContainer<CeilingRegion> for Gen3D {
    fn regions(&self) -> &Vec<CeilingRegion> {
        &self.ceilings.regions
    }
}
impl RegionContainer<WallRegion> for Gen3D {
    fn regions(&self) -> &Vec<WallRegion> {
        &self.walls.regions
    }
}

impl Gen3D {
    /// Build a `Gen3D` for storey `storey_idx`, carrying the `Ber`'s house-level
    /// construction tables across so each region's `construction_id` resolves.
    ///
    /// Replaces the old `From<Storey>`, which received only the storey and so
    /// couldn't see the constructions (they live on the parent `Ber`) — it left
    /// the tables empty and every `construction_id` dangled. All constructions are
    /// cloned: there are only a few per house, so filtering to the ones this
    /// storey references isn't worth it.
    ///
    /// NB: the inverse — writing `Gen3D`'s edited/added constructions back into
    /// the `Ber` — is still TODO, needed when `gen_3d_vol` is wired into the
    /// controller (alongside the per-region writeback that also doesn't exist yet).
    pub fn from_ber_storey(ber: &Ber, storey_idx: usize) -> Self {
        let storey = &ber.storeys[storey_idx];
        Self {
            storey: storey.storey_number as u32,
            floors: storey.floors.clone(),
            walls: storey.walls.clone(),
            ceilings: storey.roofs.clone(),
            wall_constructions: ber.wall_constructions.clone(),
            floor_constructions: ber.floor_constructions.clone(),
            ceiling_constructions: ber.ceiling_constructions.clone(),
        }
    }
}
