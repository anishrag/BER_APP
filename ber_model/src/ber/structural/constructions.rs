use serde::{Deserialize, Serialize};

use crate::ber::ber_struct::Ber;
use crate::ber::common::{ConstructionPhase, ItemId, Photo};

#[allow(unused_imports)]
use super::*;

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

impl ConstructionLayer {
    /// Build a layer from the fields the assessor types in the build-up editor
    /// (name + thickness + thermal conductivity λ + bridged). Typed layers carry
    /// a `Custom` material so the name + λ travel with them into the reusable
    /// layer library.
    pub fn typed(name: String, thickness_mm: u32, lambda: f32, bridged: bool) -> Self {
        Self {
            thickness_mm,
            material: ConstructionMaterial::Custom { lambda, name },
            bridged,
        }
    }

    /// The layer's material name (a preset's name, or the custom name).
    pub fn name(&self) -> String {
        self.material.name()
    }

    /// The layer's thermal conductivity λ (W/m·K); `0.0` for a preset that
    /// doesn't carry one.
    pub fn lambda(&self) -> f32 {
        match &self.material {
            ConstructionMaterial::Custom { lambda, .. } => *lambda,
            _ => 0.0,
        }
    }
}

#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConstructionMakeup {
    NonHeatLoss, // no heat loss
    Default,     // use default construction
    #[default]
    Tbd, // UNSET — not yet classified (the pre-edit default); counts as not done
    Unknown,     // the assessor DECLARED it unknown — a real choice, counts as done
    Layers(Vec<ConstructionLayer>), // custom stack of layers
}

/// Construction types (example: 5-6 options)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FloorConstructionType {
    SolidFloor,
    SuspendedFloor,
    ExposedSemiExposedFloor,
    NonHeatLossFloor,
    #[default]
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

impl ConstructionMakeup {
    /// Whether the assessor has MADE A CHOICE — anything but the `Tbd` UNSET
    /// placeholder. Note `Unknown` counts as set: declaring a build-up unknown is
    /// a real decision (done), unlike `Tbd` which is "not looked at yet". Half of
    /// the per-surface "done" test (the other half is the construction TYPE).
    pub fn is_set(&self) -> bool {
        !matches!(self, ConstructionMakeup::Tbd)
    }
}

impl WallConstructionType {
    /// Whether a real wall type has been chosen (not the `Uninitialised`
    /// placeholder). Part of [`WallConstruction::is_complete`].
    pub fn is_set(&self) -> bool {
        !matches!(self, WallConstructionType::Uninitialised)
    }
}

impl FloorConstructionType {
    /// Whether a real floor type has been chosen (not the `Uninitialised`
    /// placeholder). Part of [`FloorConstruction::is_complete`].
    pub fn is_set(&self) -> bool {
        !matches!(self, FloorConstructionType::Uninitialised)
    }
}

impl CeilingConstructionType {
    /// Whether a real ceiling/roof type has been chosen (not the `Uninitialised`
    /// placeholder). Part of [`CeilingConstruction::is_complete`].
    pub fn is_set(&self) -> bool {
        !matches!(self, CeilingConstructionType::Uninitialised)
    }
}

/// A construction TYPE tagged by its surface kind — the unified value the
/// Structure-phase "Edit attributes" editor carries so one group/event type
/// covers walls, floors and ceilings/roofs. Each variant wraps that kind's own
/// type enum. Transient editor/UI state, not persisted (the durable record is a
/// surface's `construction_id`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConstructionType {
    Wall(WallConstructionType),
    Floor(FloorConstructionType),
    Ceiling(CeilingConstructionType),
}

impl ConstructionType {
    /// The surface kind this construction type belongs to — lets the
    /// kind-agnostic "Edit attributes" group carry its own kind (so a single
    /// session can span walls, floors and roofs at once).
    pub fn kind(&self) -> SurfaceKind {
        match self {
            ConstructionType::Wall(_) => SurfaceKind::Walls,
            ConstructionType::Floor(_) => SurfaceKind::Floors,
            ConstructionType::Ceiling(_) => SurfaceKind::Roofs,
        }
    }
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

/// Thermal exposure (boundary condition) of a horizontal FLOOR region — set in
/// the Mapping phase, parallel to [`WallExposure`]. The Structure-phase
/// construction type/make-up stays separate (on the referenced
/// `FloorConstruction`); this is purely the boundary condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FloorExposure {
    /// No exposure marked yet (the default; auto-derivation/manual marking fill it).
    #[default]
    Unset,
    /// Ground-bearing slab / ground floor.
    Ground,
    /// Exposed to outside air below (floor over a passage/overhang).
    Exposed,
    /// Over an unheated space.
    SemiExposed,
    /// Party floor to another dwelling (no heat loss).
    NonHeatLoss,
}

/// Thermal exposure (boundary condition) of a horizontal CEILING region — set in
/// the Mapping phase, parallel to [`WallExposure`]. The Structure-phase
/// construction type/make-up stays separate (on the referenced
/// `CeilingConstruction`); this is purely the boundary condition.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CeilingExposure {
    /// No exposure marked yet (the default; auto-derivation/manual marking fill it).
    #[default]
    Unset,
    /// Exposed roof / ceiling under roof to outside.
    ExposedRoof,
    /// Party ceiling to another dwelling.
    Party,
    /// Under an unheated space (ceiling below an unheated attic).
    Unheated,
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
    /// The assessor's editable label for this construction (shown as the group's
    /// name in the Structure editor). `None` falls back to "Group N". `alias`
    /// keeps records saved under the former field name `description` loadable.
    #[serde(default, alias = "description")]
    pub name: Option<String>,
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

impl WallConstruction {
    /// A wall construction is DONE once both its type and its build-up are set
    /// (no `Uninitialised` type, no `Tbd` make-up) — the per-surface "done" test
    /// the Structure-phase `Remaining` tool filters on.
    pub fn is_complete(&self) -> bool {
        self.construction_type.is_set() && self.base.construction_makeup.is_set()
    }
}

impl FloorConstruction {
    /// A floor construction is DONE once both its type and its build-up are set.
    /// See [`WallConstruction::is_complete`].
    pub fn is_complete(&self) -> bool {
        self.construction_type.is_set() && self.base.construction_makeup.is_set()
    }
}

impl CeilingConstruction {
    /// A ceiling/roof construction is DONE once both its type and its build-up
    /// are set. See [`WallConstruction::is_complete`].
    pub fn is_complete(&self) -> bool {
        self.construction_type.is_set() && self.base.construction_makeup.is_set()
    }
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

    /// Resolve a ceiling/roof construction by id (the inverse of
    /// `ensure_ceiling_construction`).
    pub fn ceiling_construction(&self, id: ItemId) -> Option<&CeilingConstruction> {
        self.ceiling_constructions.iter().find(|c| c.base.id == id)
    }

    /// Set (or clear, with `None`) the editable NAME of the `kind` construction
    /// `id` — the durable home for a Structure group's label. Find-by-id within
    /// the matching registry; no-op if it isn't there.
    pub fn set_construction_name(&mut self, kind: SurfaceKind, id: ItemId, name: Option<String>) {
        let base = match kind {
            SurfaceKind::Walls => self
                .wall_constructions
                .iter_mut()
                .find(|c| c.base.id == id)
                .map(|c| &mut c.base),
            SurfaceKind::Floors => self
                .floor_constructions
                .iter_mut()
                .find(|c| c.base.id == id)
                .map(|c| &mut c.base),
            SurfaceKind::Roofs => self
                .ceiling_constructions
                .iter_mut()
                .find(|c| c.base.id == id)
                .map(|c| &mut c.base),
        };
        if let Some(base) = base {
            base.name = name;
        }
    }

    /// Attach a `photo` to the `kind` construction `id` (its build-up photos).
    /// Find-by-id within the matching registry; no-op if it isn't there.
    pub fn add_construction_photo(&mut self, kind: SurfaceKind, id: ItemId, photo: Photo) {
        let base = match kind {
            SurfaceKind::Walls => self
                .wall_constructions
                .iter_mut()
                .find(|c| c.base.id == id)
                .map(|c| &mut c.base),
            SurfaceKind::Floors => self
                .floor_constructions
                .iter_mut()
                .find(|c| c.base.id == id)
                .map(|c| &mut c.base),
            SurfaceKind::Roofs => self
                .ceiling_constructions
                .iter_mut()
                .find(|c| c.base.id == id)
                .map(|c| &mut c.base),
        };
        if let Some(base) = base {
            base.photos.push(photo);
        }
    }

    /// The ids of every `kind` surface in storey `idx` whose construction is NOT
    /// yet done — its type or build-up is still unset (see
    /// [`WallConstruction::is_complete`]). A region whose construction can't be
    /// resolved counts as incomplete, matching the editor's "missing reads as
    /// default" rule. The complement of the done surfaces within
    /// [`Storey::surface_ids`]; drives the Structure-phase `Remaining` select
    /// tool. Empty when `idx` is out of range.
    pub fn incomplete_surface_ids(&self, idx: usize, kind: SurfaceKind) -> Vec<ItemId> {
        let Some(storey) = self.storeys.get(idx) else {
            return Vec::new();
        };
        match kind {
            SurfaceKind::Walls => storey
                .walls
                .regions
                .iter()
                .filter(|r| {
                    self.wall_construction(r.construction_id)
                        .map_or(true, |c| !c.is_complete())
                })
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Floors => storey
                .floors
                .regions
                .iter()
                .filter(|r| {
                    self.floor_construction(r.construction_id)
                        .map_or(true, |c| !c.is_complete())
                })
                .map(|r| r.id)
                .collect(),
            SurfaceKind::Roofs => storey
                .roofs
                .regions
                .iter()
                .filter(|r| {
                    self.ceiling_construction(r.construction_id)
                        .map_or(true, |c| !c.is_complete())
                })
                .map(|r| r.id)
                .collect(),
        }
    }
}
