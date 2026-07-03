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

impl FloorConstructionType {
    /// Short display label — the single source for the type's name.
    pub fn label(self) -> &'static str {
        match self {
            FloorConstructionType::SolidFloor => "Solid",
            FloorConstructionType::SuspendedFloor => "Suspended",
            FloorConstructionType::ExposedSemiExposedFloor => "Exposed",
            FloorConstructionType::NonHeatLossFloor => "Non-heat-loss",
            FloorConstructionType::Other => "Other",
            FloorConstructionType::Uninitialised => "Unset",
        }
    }
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

impl CeilingConstructionType {
    /// Short display label — the single source for the type's name.
    pub fn label(self) -> &'static str {
        match self {
            CeilingConstructionType::FlatRoof => "Flat",
            CeilingConstructionType::SlopedInsOnRafter => "Ins. on rafter",
            CeilingConstructionType::SlopedInsOnCeiling => "Ins. on ceiling",
            CeilingConstructionType::SlopedInsOnCeilingAndRafter => "Ins. both",
            CeilingConstructionType::NonHeatLossCeiling => "Non-heat-loss",
            CeilingConstructionType::Other => "Other",
            CeilingConstructionType::Uninitialised => "Unset",
        }
    }
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

impl WallConstructionType {
    /// Short display label — the single source for the type's name (the Structure
    /// editor dropdown and the Structure review both use this).
    pub fn label(self) -> &'static str {
        match self {
            WallConstructionType::CavityWall => "Cavity",
            WallConstructionType::FilledCavityWall => "Filled cavity",
            WallConstructionType::ConcreteHollowBlocks => "Hollow block",
            WallConstructionType::TimberConstruction => "Timber",
            WallConstructionType::ICF => "ICF",
            WallConstructionType::Floor => "Floor",
            WallConstructionType::Uninitialised => "Unset",
        }
    }
}

impl ConstructionMakeup {
    /// Whether the assessor has MADE A CHOICE — anything but the `Tbd` UNSET
    /// placeholder. Note `Unknown` counts as set: declaring a build-up unknown is
    /// a real decision (done), unlike `Tbd` which is "not looked at yet". Half of
    /// the per-surface "done" test (the other half is the construction TYPE).
    pub fn is_set(&self) -> bool {
        !matches!(self, ConstructionMakeup::Tbd)
    }

    /// Whether this build-up needs a backing PHOTO to count as done. Only a
    /// SPECIFIED custom build-up (`Layers`) does — the assessor declared an
    /// actual fabric stack, so a photo is the evidence for it. `Default` (a
    /// standard assumed construction), `Unknown` (declared unknown) and
    /// `NonHeatLoss` are not specified fabric and need no photo.
    pub fn requires_photo(&self) -> bool {
        matches!(self, ConstructionMakeup::Layers(_))
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
/// covers walls, floors and ceilings/ceilings. Each variant wraps that kind's own
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
    /// session can span walls, floors and ceilings at once).
    pub fn kind(&self) -> SurfaceKind {
        match self {
            ConstructionType::Wall(_) => SurfaceKind::Walls,
            ConstructionType::Floor(_) => SurfaceKind::Floors,
            ConstructionType::Ceiling(_) => SurfaceKind::Ceilings,
        }
    }
}

/// An exposure VALUE tagged by its surface kind — the unified value the
/// Structure-phase "Edit attributes" editor carries for the exposure dropdown,
/// so one group/event type covers walls, floors and ceilings/ceilings (mirrors
/// [`ConstructionType`] for the type dropdown). Each variant wraps that kind's
/// own exposure enum. Transient editor/UI state, not persisted (the durable
/// record is the region's `exposure` field / the `StoreyDoc` exposure tables).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceExposure {
    Wall(WallExposure),
    Floor(FloorExposure),
    Ceiling(CeilingExposure),
}

impl SurfaceExposure {
    /// The exposure values offered for `kind`, in the dropdown's display order —
    /// the most common boundary first, `Unset` last.
    pub fn variants_for(kind: SurfaceKind) -> &'static [SurfaceExposure] {
        match kind {
            SurfaceKind::Walls => &[
                SurfaceExposure::Wall(WallExposure::Exposed),
                SurfaceExposure::Wall(WallExposure::SemiExposed),
                SurfaceExposure::Wall(WallExposure::NonHeatLoss),
                SurfaceExposure::Wall(WallExposure::Ground),
                SurfaceExposure::Wall(WallExposure::Unset),
            ],
            SurfaceKind::Floors => &[
                SurfaceExposure::Floor(FloorExposure::Ground),
                SurfaceExposure::Floor(FloorExposure::Exposed),
                SurfaceExposure::Floor(FloorExposure::SemiExposed),
                SurfaceExposure::Floor(FloorExposure::NonHeatLoss),
                SurfaceExposure::Floor(FloorExposure::Unset),
            ],
            SurfaceKind::Ceilings => &[
                SurfaceExposure::Ceiling(CeilingExposure::ExposedRoof),
                SurfaceExposure::Ceiling(CeilingExposure::Party),
                SurfaceExposure::Ceiling(CeilingExposure::Unheated),
                SurfaceExposure::Ceiling(CeilingExposure::Unset),
            ],
        }
    }

    /// The surface kind this exposure value belongs to.
    pub fn kind(&self) -> SurfaceKind {
        match self {
            SurfaceExposure::Wall(_) => SurfaceKind::Walls,
            SurfaceExposure::Floor(_) => SurfaceKind::Floors,
            SurfaceExposure::Ceiling(_) => SurfaceKind::Ceilings,
        }
    }

    /// Short label for the dropdown.
    pub fn label(&self) -> &'static str {
        match self {
            SurfaceExposure::Wall(WallExposure::Exposed) => "Exposed",
            SurfaceExposure::Wall(WallExposure::SemiExposed) => "Semi-exposed",
            SurfaceExposure::Wall(WallExposure::NonHeatLoss) => "Non heat loss",
            SurfaceExposure::Wall(WallExposure::Ground) => "Ground",
            SurfaceExposure::Wall(WallExposure::InterStorey) => "Inter-storey",
            SurfaceExposure::Wall(WallExposure::Unset) => "Unset",
            SurfaceExposure::Floor(FloorExposure::Ground) => "Ground",
            SurfaceExposure::Floor(FloorExposure::Exposed) => "Exposed",
            SurfaceExposure::Floor(FloorExposure::SemiExposed) => "Semi-exposed",
            SurfaceExposure::Floor(FloorExposure::NonHeatLoss) => "Non heat loss",
            SurfaceExposure::Floor(FloorExposure::InterStorey) => "Inter-storey",
            SurfaceExposure::Floor(FloorExposure::Unset) => "Unset",
            SurfaceExposure::Ceiling(CeilingExposure::ExposedRoof) => "Exposed roof",
            SurfaceExposure::Ceiling(CeilingExposure::Party) => "Party",
            SurfaceExposure::Ceiling(CeilingExposure::Unheated) => "Unheated",
            SurfaceExposure::Ceiling(CeilingExposure::InterStorey) => "Inter-storey",
            SurfaceExposure::Ceiling(CeilingExposure::Unset) => "Unset",
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
    /// DERIVED: this surface is backed by another storey of the SAME dwelling
    /// (a storey junction). Never assessor-set — `derive_inter_storey` stamps
    /// and clears it on every re-derive; setters targeting it are no-ops.
    /// Excluded from heat-loss area at DEAP export.
    InterStorey,
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
    /// DERIVED: this surface is backed by another storey of the SAME dwelling
    /// (a storey junction). Never assessor-set — `derive_inter_storey` stamps
    /// and clears it on every re-derive; setters targeting it are no-ops.
    /// Excluded from heat-loss area at DEAP export.
    InterStorey,
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
    /// Party ceiling to another dwelling (assessor-paintable: "another
    /// dwelling above" — NOT the derived same-dwelling junction, which is
    /// [`CeilingExposure::InterStorey`]).
    Party,
    /// Under an unheated space (ceiling below an unheated attic).
    Unheated,
    /// DERIVED: this surface is backed by another storey of the SAME dwelling
    /// (a storey junction). Never assessor-set — `derive_inter_storey` stamps
    /// and clears it on every re-derive; setters targeting it are no-ops.
    /// Excluded from heat-loss area at DEAP export.
    InterStorey,
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

/// The assessor's INSULATION spec for a ceiling/roof construction — the
/// Structure "Edit attributes" dialog's "Insulation thickness (mm)" box plus
/// its "unknown" checkbox. `None` on the construction means "not yet
/// specified" (old records load as that via `#[serde(default)]`), which keeps
/// the ceiling incomplete until one of the two is given.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum CeilingInsulation {
    /// The assessor DECLARED the thickness unknown — a real choice; counts as
    /// done without a photo (there's nothing measurable to evidence).
    Unknown,
    /// A specified insulation thickness in mm. Needs a backing photo to count
    /// as done — the assessor asserted a measurable fabric property.
    ThicknessMm(f32),
}

impl CeilingInsulation {
    /// Whether an insulation SPEC counts as DONE given photo presence:
    /// unspecified (`None`) never; `Unknown` outright; a specified thickness
    /// only when a photo backs it. The single predicate shared by
    /// [`CeilingConstruction::is_complete`] and the Structure editor's
    /// session-completion gate.
    pub fn spec_done(spec: Option<CeilingInsulation>, has_photo: bool) -> bool {
        match spec {
            None => false,
            Some(CeilingInsulation::Unknown) => true,
            Some(CeilingInsulation::ThicknessMm(_)) => has_photo,
        }
    }

    /// Normalize a spec at the model boundary: a non-positive or non-finite
    /// thickness is no measurement at all, so it collapses to `None`
    /// (unspecified). The dialog only sends valid parses — belt-and-braces.
    pub fn normalized(spec: Option<CeilingInsulation>) -> Option<CeilingInsulation> {
        match spec {
            Some(CeilingInsulation::ThicknessMm(t)) if !(t.is_finite() && t > 0.0) => None,
            other => other,
        }
    }
}

/// A ceiling/roof construction shared by one or more `CeilingRegion`s.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct CeilingConstruction {
    pub base: ConstructionBase,
    pub construction_type: CeilingConstructionType,
    /// Ceiling insulation as specified in the Structure attribute dialog.
    /// `None` = not yet specified (and the default for records saved before
    /// the field existed). Ceilings only — walls/floors carry no such spec.
    #[serde(default)]
    pub insulation: Option<CeilingInsulation>,
}

impl ConstructionBase {
    /// Whether the build-up half of a construction is DONE: the make-up is set
    /// (not the `Tbd` placeholder) AND — when that make-up is a specified custom
    /// build-up ([`ConstructionMakeup::requires_photo`]) — at least one backing
    /// photo is attached. `Default`/`Unknown`/`NonHeatLoss` need no photo.
    pub fn build_up_done(&self) -> bool {
        self.construction_makeup.is_set()
            && (!self.construction_makeup.requires_photo() || !self.photos.is_empty())
    }
}

impl WallConstruction {
    /// A wall construction is DONE once its type is set (no `Uninitialised`) and
    /// its build-up is done ([`ConstructionBase::build_up_done`] — make-up set,
    /// plus a photo when the build-up is a specified custom stack). The
    /// per-surface "done" test the Structure-phase `Remaining` tool filters on.
    pub fn is_complete(&self) -> bool {
        self.construction_type.is_set() && self.base.build_up_done()
    }
}

impl FloorConstruction {
    /// A floor construction is DONE once its type is set and its build-up is done.
    /// See [`WallConstruction::is_complete`].
    pub fn is_complete(&self) -> bool {
        self.construction_type.is_set() && self.base.build_up_done()
    }
}

impl CeilingConstruction {
    /// A ceiling/roof construction is DONE once its type is set, its build-up
    /// is done (see [`WallConstruction::is_complete`]), AND its insulation is
    /// specified ([`CeilingInsulation::spec_done`] — a thickness needs a
    /// backing photo; declared-unknown doesn't; unspecified never counts).
    pub fn is_complete(&self) -> bool {
        self.construction_type.is_set()
            && self.base.build_up_done()
            && CeilingInsulation::spec_done(self.insulation, !self.base.photos.is_empty())
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
            insulation: None,
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
            SurfaceKind::Ceilings => self
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
            SurfaceKind::Ceilings => self
                .ceiling_constructions
                .iter_mut()
                .find(|c| c.base.id == id)
                .map(|c| &mut c.base),
        };
        if let Some(base) = base {
            base.photos.push(photo);
        }
    }

    /// Set (or clear, with `None`) the INSULATION spec of CEILING construction
    /// `id` — the durable write behind the ceiling attribute dialog's
    /// thickness box / "unknown" checkbox. Ceilings only (walls/floors carry
    /// no insulation spec); no-op when the id isn't in the registry. A
    /// non-positive or non-finite thickness normalizes to `None` (the dialog
    /// only sends valid parses — belt-and-braces at the model boundary).
    pub fn set_ceiling_insulation(&mut self, id: ItemId, insulation: Option<CeilingInsulation>) {
        let insulation = CeilingInsulation::normalized(insulation);
        if let Some(c) = self
            .ceiling_constructions
            .iter_mut()
            .find(|c| c.base.id == id)
        {
            c.insulation = insulation;
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
            SurfaceKind::Ceilings => storey
                .ceilings
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

    /// Whether EVERY surface of `kind` on storey `idx` is fully attributed — the
    /// Structure-phase checklist's per-storey/kind "done" test. True only when
    /// the storey has at least one such surface and none are
    /// [incomplete](Self::incomplete_surface_ids) (so an empty kind is NOT
    /// vacuously done). `false` for an out-of-range storey.
    pub fn structure_surfaces_complete(&self, idx: usize, kind: SurfaceKind) -> bool {
        let Some(storey) = self.storeys.get(idx) else {
            return false;
        };
        let counts = storey.surface_counts();
        let count = match kind {
            SurfaceKind::Walls => counts.walls,
            SurfaceKind::Floors => counts.floors,
            SurfaceKind::Ceilings => counts.ceilings,
        };
        count > 0 && self.incomplete_surface_ids(idx, kind).is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ceiling completeness now includes the INSULATION spec: unspecified
    /// blocks; declared-unknown completes without a photo; a specified
    /// thickness completes only once a photo backs it.
    #[test]
    fn ceiling_completeness_requires_insulation_spec() {
        let mut c = CeilingConstruction {
            construction_type: CeilingConstructionType::FlatRoof,
            ..Default::default()
        };
        c.base.construction_makeup = ConstructionMakeup::Default;
        assert!(
            !c.is_complete(),
            "type + build-up alone no longer complete a ceiling"
        );
        c.insulation = Some(CeilingInsulation::Unknown);
        assert!(c.is_complete(), "declared-unknown needs no photo");
        c.insulation = Some(CeilingInsulation::ThicknessMm(150.0));
        assert!(!c.is_complete(), "a thickness needs a backing photo");
        c.base.photos.push(Photo::default());
        assert!(c.is_complete(), "thickness + photo completes");
    }

    /// Old records predate the `insulation` field → they load as `None`
    /// (unspecified); the two spec values round-trip through serde.
    #[test]
    fn ceiling_insulation_serde_default_and_roundtrip() {
        for spec in [
            Some(CeilingInsulation::Unknown),
            Some(CeilingInsulation::ThicknessMm(112.5)),
            None,
        ] {
            let c = CeilingConstruction {
                construction_type: CeilingConstructionType::FlatRoof,
                insulation: spec,
                ..Default::default()
            };
            let back: CeilingConstruction =
                serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
            assert_eq!(back.insulation, spec, "round-trip of {spec:?}");
        }

        // An OLD record: serialize without the field at all, then load.
        let mut v = serde_json::to_value(CeilingConstruction::default()).unwrap();
        v.as_object_mut().unwrap().remove("insulation");
        let old: CeilingConstruction = serde_json::from_value(v).unwrap();
        assert_eq!(old.insulation, None, "pre-field records load unspecified");
    }

    /// The `Ber` setter stores Unknown vs ThicknessMm on the addressed ceiling
    /// construction, normalizes invalid thicknesses to unspecified, and
    /// no-ops on an unknown id.
    #[test]
    fn set_ceiling_insulation_stores_and_normalizes() {
        let mut ber = Ber::default();
        let id = ber.ensure_ceiling_construction(
            CeilingConstructionType::FlatRoof,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        ber.set_ceiling_insulation(id, Some(CeilingInsulation::ThicknessMm(150.0)));
        assert_eq!(
            ber.ceiling_construction(id).unwrap().insulation,
            Some(CeilingInsulation::ThicknessMm(150.0))
        );
        ber.set_ceiling_insulation(id, Some(CeilingInsulation::Unknown));
        assert_eq!(
            ber.ceiling_construction(id).unwrap().insulation,
            Some(CeilingInsulation::Unknown)
        );
        // Invalid thickness → normalized to unspecified.
        ber.set_ceiling_insulation(id, Some(CeilingInsulation::ThicknessMm(0.0)));
        assert_eq!(ber.ceiling_construction(id).unwrap().insulation, None);
        // Unknown id → no-op (no panic, nothing changed elsewhere).
        ber.set_ceiling_insulation(ItemId::default(), Some(CeilingInsulation::Unknown));
        assert_eq!(ber.ceiling_construction(id).unwrap().insulation, None);
    }
}
