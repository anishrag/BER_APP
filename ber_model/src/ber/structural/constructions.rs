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
    /// The DEFAULT construction PLUS extra measured insulation the assessor
    /// found added on top (an insulation upgrade over the era-default fabric).
    /// `extra_insulation_mm` is the measured added thickness. ADDITIVE serde
    /// variant (2026-07): records saved before it existed never contain the
    /// tag, so old JSON keeps loading unchanged.
    ///
    /// TODO(deap-export): the DEAP U-value story — the default construction's
    /// age-band U-value adjusted for the added insulation — is a derivation
    /// for the future deap mapping layer (no U-value derivation exists in
    /// this crate yet); the model just stores the measurement faithfully.
    DefaultPlus {
        extra_insulation_mm: u32,
    },
    Layers(Vec<ConstructionLayer>), // custom stack of layers
}

/// DEAP's standard insulation THICKNESS options, in mm — the preset list the
/// quick pickers offer (the [`ConstructionMakeup::DefaultPlus`]
/// extra-insulation entry and the quick plasterboard+insulation build-up).
/// A shortcut table, not a restriction: free-typed thicknesses stay possible
/// via the layer editor / the free-entry box.
pub const DEAP_INSULATION_THICKNESS_MM: &[u32] = &[10, 20, 25, 30, 40, 50, 60, 70, 80];

/// The CEILING attribute editor's extra-insulation THICKNESS options, in mm —
/// the option list of the combined "Default" dropdown (era-default
/// construction + this much measured insulation, in 25 mm steps; the
/// dropdown's other entry, "unknown", is the TRUE default: era construction,
/// extra insulation unknown). Distinct from the walls' DEAP dry-lining table
/// above — loft insulation runs much thicker.
pub const CEILING_INSULATION_THICKNESS_MM: &[u32] =
    &[25, 50, 75, 100, 125, 150, 175, 200, 225, 250, 275, 300];

/// Nominal plasterboard slab thickness for the quick plasterboard+insulation
/// build-up, in whole mm (the standard 12.5 mm slab, rounded up —
/// [`ConstructionLayer::thickness_mm`] is integral mm).
pub const PLASTERBOARD_THICKNESS_MM: u32 = 13;

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

    /// Whether this build-up needs a backing PHOTO to count as done. A
    /// SPECIFIED custom build-up (`Layers`) does — the assessor declared an
    /// actual fabric stack, so a photo is the evidence for it — and so does
    /// `DefaultPlus`: the assessor asserted a MEASURED added-insulation
    /// thickness, and (exactly like a ceiling's specified
    /// [`CeilingInsulation::ThicknessMm`]) a measurable fabric claim needs
    /// photographic evidence. `Default` (a standard assumed construction),
    /// `Unknown` (declared unknown) and `NonHeatLoss` are not specified
    /// fabric and need no photo.
    pub fn requires_photo(&self) -> bool {
        matches!(
            self,
            ConstructionMakeup::Layers(_) | ConstructionMakeup::DefaultPlus { .. }
        )
    }

    /// The quick "plasterboard + insulation" build-up: a plasterboard slab
    /// (nominal [`PLASTERBOARD_THICKNESS_MM`]) over `insulation_mm` of EPS
    /// insulation, inside → outside. The one-tap shortcut the Structure
    /// editor offers beside Default/Unknown/Specify — it produces an ordinary
    /// `Layers` build-up, so it edits, photographs and completes exactly like
    /// a stack specified by hand in the layer editor.
    pub fn plasterboard_insulation(insulation_mm: u32) -> Self {
        ConstructionMakeup::Layers(vec![
            ConstructionLayer {
                thickness_mm: PLASTERBOARD_THICKNESS_MM,
                material: ConstructionMaterial::Plasterboard,
                bridged: false,
            },
            ConstructionLayer {
                thickness_mm: insulation_mm,
                material: ConstructionMaterial::InsulationEPS,
                bridged: false,
            },
        ])
    }

    /// The insulation thickness (mm) when this make-up IS a quick
    /// plasterboard+insulation build-up
    /// ([`plasterboard_insulation`](Self::plasterboard_insulation)) — exactly
    /// a plasterboard layer over an EPS insulation layer. `None` for anything
    /// else, including hand-specified stacks that merely contain those
    /// materials among others. Lets the quick picker show its selected state.
    pub fn plasterboard_insulation_mm(&self) -> Option<u32> {
        match self {
            ConstructionMakeup::Layers(v) => match v.as_slice() {
                [pb, ins]
                    if pb.material == ConstructionMaterial::Plasterboard
                        && ins.material == ConstructionMaterial::InsulationEPS =>
                {
                    Some(ins.thickness_mm)
                }
                _ => None,
            },
            _ => None,
        }
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

impl WallExposure {
    /// Whether a wall with this boundary condition PARTICIPATES in the
    /// Structure phase — selectable, groupable, counted by completeness, and
    /// shown in the Structure review. `NonHeatLoss` (internal / party walls —
    /// no heat loss, so no construction attribution is needed) and the derived
    /// `InterStorey` junctions are OUT of scope: they still draw everywhere,
    /// but every Structure-phase wall query skips them (see the Walls arms of
    /// [`Storey::surface_at`](super::Storey::surface_at) and friends).
    /// `Unset` stays IN scope — an unclassified wall must still be reachable.
    pub fn in_structure_scope(self) -> bool {
        !matches!(self, WallExposure::NonHeatLoss | WallExposure::InterStorey)
    }
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

impl FloorExposure {
    /// Whether a floor with this boundary condition PARTICIPATES in the
    /// Structure phase — the horizontal mirror of
    /// [`WallExposure::in_structure_scope`]. `NonHeatLoss` (a party floor to
    /// another dwelling — no heat loss, so no construction attribution is
    /// needed) and the derived `InterStorey` junction are OUT of scope: they
    /// still draw, and the Mapping-phase exposure tools still reach them (to
    /// repaint the classification), but the Structure phase neither selects,
    /// counts nor reviews them. `Unset` stays IN scope.
    pub fn in_structure_scope(self) -> bool {
        !matches!(
            self,
            FloorExposure::NonHeatLoss | FloorExposure::InterStorey
        )
    }
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

impl CeilingExposure {
    /// Whether a ceiling with this boundary condition PARTICIPATES in the
    /// Structure phase — the horizontal mirror of
    /// [`WallExposure::in_structure_scope`]. `Party` (another dwelling above
    /// — no heat loss) and the derived `InterStorey` junction (this
    /// dwelling's own storey above — e.g. the ground-floor ceiling under an
    /// upper storey) are OUT of scope: they still draw, and the
    /// Mapping-phase exposure tools still reach a `Party` ceiling (to
    /// repaint the classification), but the Structure phase neither selects,
    /// counts, suggests nor reviews them. `Unset` stays IN scope.
    pub fn in_structure_scope(self) -> bool {
        !matches!(self, CeilingExposure::Party | CeilingExposure::InterStorey)
    }
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
/// Structure "Edit attributes" dialog's combined "Default" dropdown (extra
/// insulation over the era-default construction: a measured thickness, or
/// declared unknown). `None` on the construction means "not yet specified"
/// (old records load as that via `#[serde(default)]`), which keeps a
/// plain-`Default` ceiling incomplete until one of the two is given.
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

    /// Whether a ceiling's insulation question is ANSWERED for the given
    /// build-up. Only the plain `Default` make-up asks it — the era-default
    /// construction is exactly the state where the extra loft insulation is a
    /// separate open question (the combined "Default" dropdown's unknown /
    /// thickness choice, stored in [`CeilingConstruction::insulation`]). Every
    /// other make-up already carries its own answer: `DefaultPlus` holds the
    /// measured extra thickness, `Layers` specifies the stack,
    /// `Unknown`/`NonHeatLoss` declare there is nothing further to specify —
    /// and the unset `Tbd` is caught by the build-up gate, not this one. The
    /// per-make-up successor to bare [`spec_done`](Self::spec_done) in
    /// [`CeilingConstruction::is_complete`] and the Structure editor's group
    /// tick.
    pub fn spec_done_for(
        makeup: &ConstructionMakeup,
        spec: Option<CeilingInsulation>,
        has_photo: bool,
    ) -> bool {
        !matches!(makeup, ConstructionMakeup::Default) || Self::spec_done(spec, has_photo)
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
    /// Extra insulation as specified in the Structure attribute dialog's
    /// combined "Default" dropdown. `None` = not yet specified (and the
    /// default for records saved before the field existed). Only consulted
    /// while the make-up is the plain `Default`
    /// ([`CeilingInsulation::spec_done_for`]); other make-ups carry their own
    /// answer. Ceilings only — walls/floors carry no such spec.
    #[serde(default)]
    pub insulation: Option<CeilingInsulation>,
}

/// Normalize a construction NAME for identity/collision checks: trimmed;
/// blank/absent reads as "unnamed" (`None`). The single equality the
/// name-aware `ensure_*_named` find and the name-integrity rule share.
fn normalized_name(name: Option<&str>) -> Option<&str> {
    name.map(str::trim).filter(|s| !s.is_empty())
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
    /// is done (see [`WallConstruction::is_complete`]), AND — for the plain
    /// `Default` make-up only — its extra insulation is specified
    /// ([`CeilingInsulation::spec_done_for`] — a thickness needs a backing
    /// photo; declared-unknown doesn't; unspecified never counts). Any other
    /// make-up (`Unknown`, `Layers`, `DefaultPlus`, `NonHeatLoss`) already
    /// answers the insulation question itself, so only the build-up rules
    /// (incl. its photo gate) apply.
    pub fn is_complete(&self) -> bool {
        self.construction_type.is_set()
            && self.base.build_up_done()
            && CeilingInsulation::spec_done_for(
                &self.base.construction_makeup,
                self.insulation,
                !self.base.photos.is_empty(),
            )
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
        self.ensure_wall_construction_named(construction_type, makeup, phase, None)
    }

    /// [`ensure_wall_construction`](Self::ensure_wall_construction) with the
    /// NAME as part of the find-or-create identity: two groups with the SAME
    /// fabric `(type, make-up, phase)` but DIFFERENT names are DIFFERENT
    /// groups (e.g. "Porch roof" vs "Main house roof", both sloped/default/
    /// original) — without the name axis they'd collapse into one shared
    /// construction and editing either would edit both. `None`/blank matches
    /// only an unnamed construction.
    pub fn ensure_wall_construction_named(
        &mut self,
        construction_type: WallConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
        name: Option<&str>,
    ) -> ItemId {
        let name = normalized_name(name);
        if let Some(c) = self.wall_constructions.iter().find(|c| {
            c.construction_type == construction_type
                && c.base.construction_makeup == makeup
                && c.base.construction_phase == phase
                && normalized_name(c.base.name.as_deref()) == name
        }) {
            return c.base.id;
        }
        let c = WallConstruction {
            base: ConstructionBase {
                construction_makeup: makeup,
                construction_phase: phase,
                name: name.map(str::to_string),
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
        self.ensure_floor_construction_named(construction_type, makeup, phase, None)
    }

    /// Name-aware find-or-create — see
    /// [`ensure_wall_construction_named`](Self::ensure_wall_construction_named).
    pub fn ensure_floor_construction_named(
        &mut self,
        construction_type: FloorConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
        name: Option<&str>,
    ) -> ItemId {
        let name = normalized_name(name);
        if let Some(c) = self.floor_constructions.iter().find(|c| {
            c.construction_type == construction_type
                && c.base.construction_makeup == makeup
                && c.base.construction_phase == phase
                && normalized_name(c.base.name.as_deref()) == name
        }) {
            return c.base.id;
        }
        let c = FloorConstruction {
            base: ConstructionBase {
                construction_makeup: makeup,
                construction_phase: phase,
                name: name.map(str::to_string),
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
        self.ensure_ceiling_construction_named(construction_type, makeup, phase, None)
    }

    /// Name-aware find-or-create — see
    /// [`ensure_wall_construction_named`](Self::ensure_wall_construction_named).
    pub fn ensure_ceiling_construction_named(
        &mut self,
        construction_type: CeilingConstructionType,
        makeup: ConstructionMakeup,
        phase: ConstructionPhase,
        name: Option<&str>,
    ) -> ItemId {
        let name = normalized_name(name);
        if let Some(c) = self.ceiling_constructions.iter().find(|c| {
            c.construction_type == construction_type
                && c.base.construction_makeup == makeup
                && c.base.construction_phase == phase
                && normalized_name(c.base.name.as_deref()) == name
        }) {
            return c.base.id;
        }
        let c = CeilingConstruction {
            base: ConstructionBase {
                construction_makeup: makeup,
                construction_phase: phase,
                name: name.map(str::to_string),
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
    /// the matching registry; a missing id is a successful no-op.
    ///
    /// NAME-INTEGRITY rule: a construction name IMPLIES its attributes, so a
    /// non-empty name already held by ANOTHER construction of the same kind
    /// is REJECTED with a user-facing hint. There is never a
    /// same-name-same-attributes duplicate to merge with instead: the
    /// registry find-or-creates constructions BY their `(type, make-up,
    /// phase)` key (`ensure_*_construction`), so identical attributes always
    /// resolve to one shared entry — a colliding name necessarily belongs to
    /// a construction with DIFFERENT attributes. (Rejection is also the only
    /// safe reaction: renames stream per keystroke, so "become that group on
    /// a name match" would silently re-attribute a group the moment a longer
    /// name passes through a colliding prefix.) `Err` leaves every name
    /// untouched; clearing (`None`/blank) always succeeds.
    pub fn set_construction_name(
        &mut self,
        kind: SurfaceKind,
        id: ItemId,
        name: Option<String>,
    ) -> Result<(), String> {
        if let Some(n) = normalized_name(name.as_deref()) {
            let holds = |base: &ConstructionBase| {
                base.id != id && normalized_name(base.name.as_deref()) == Some(n)
            };
            // Only a construction some region still REFERENCES holds its name
            // against a rename — an orphaned registry entry (all members
            // re-pointed elsewhere) doesn't hold the name hostage.
            let taken = match kind {
                SurfaceKind::Walls => self.wall_constructions.iter().any(|c| {
                    holds(&c.base) && !self.construction_member_ids(kind, c.base.id).is_empty()
                }),
                SurfaceKind::Floors => self.floor_constructions.iter().any(|c| {
                    holds(&c.base) && !self.construction_member_ids(kind, c.base.id).is_empty()
                }),
                SurfaceKind::Ceilings => self.ceiling_constructions.iter().any(|c| {
                    holds(&c.base) && !self.construction_member_ids(kind, c.base.id).is_empty()
                }),
            };
            if taken {
                let kind_word = match kind {
                    SurfaceKind::Walls => "wall",
                    SurfaceKind::Floors => "floor",
                    SurfaceKind::Ceilings => "roof",
                };
                return Err(format!(
                    "\"{n}\" is already another {kind_word} group's name — a name means \
                     exactly one set of attributes, so pick a different name (matching \
                     attributes would already be one group)"
                ));
            }
        }
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
        Ok(())
    }

    /// The ids of every RESOLVED region (across all storeys) of `kind` that
    /// references construction `cid` — the group's current MEMBERS. Drives
    /// the shell's full-group-vs-subset decision (a subset edit FORKS instead
    /// of mutating the shared construction) and the referenced-only name
    /// collision check (an orphaned construction doesn't hold a name
    /// hostage).
    pub fn construction_member_ids(&self, kind: SurfaceKind, cid: ItemId) -> Vec<ItemId> {
        let mut out = Vec::new();
        for s in &self.storeys {
            match kind {
                SurfaceKind::Walls => out.extend(
                    s.walls
                        .regions
                        .iter()
                        .filter(|r| r.construction_id == cid)
                        .map(|r| r.id),
                ),
                SurfaceKind::Floors => out.extend(
                    s.floors
                        .regions
                        .iter()
                        .filter(|r| r.construction_id == cid)
                        .map(|r| r.id),
                ),
                SurfaceKind::Ceilings => out.extend(
                    s.ceilings
                        .regions
                        .iter()
                        .filter(|r| r.construction_id == cid)
                        .map(|r| r.id),
                ),
            }
        }
        out
    }

    /// The `(kind-tagged type, make-up, phase)` attributes of the `kind`
    /// construction named `name` (trimmed exact match), if any — lets the
    /// shell tell whether a rename that collided
    /// ([`set_construction_name`](Self::set_construction_name)) actually
    /// targets an IDENTICAL-attribute group: same name + same attributes IS
    /// that group (the write is redundant, not conflicting), while different
    /// attributes is the real violation.
    pub fn named_construction_attrs(
        &self,
        kind: SurfaceKind,
        name: &str,
    ) -> Option<(ConstructionType, ConstructionMakeup, ConstructionPhase)> {
        let n = normalized_name(Some(name))?;
        // REFERENCED holders only — an orphan's stale name shouldn't steer
        // the shell's merge decision.
        let named = |base: &ConstructionBase| {
            normalized_name(base.name.as_deref()) == Some(n)
                && !self.construction_member_ids(kind, base.id).is_empty()
        };
        match kind {
            SurfaceKind::Walls => {
                self.wall_constructions
                    .iter()
                    .find(|c| named(&c.base))
                    .map(|c| {
                        (
                            ConstructionType::Wall(c.construction_type),
                            c.base.construction_makeup.clone(),
                            c.base.construction_phase.clone(),
                        )
                    })
            }
            SurfaceKind::Floors => self
                .floor_constructions
                .iter()
                .find(|c| named(&c.base))
                .map(|c| {
                    (
                        ConstructionType::Floor(c.construction_type),
                        c.base.construction_makeup.clone(),
                        c.base.construction_phase.clone(),
                    )
                }),
            SurfaceKind::Ceilings => self
                .ceiling_constructions
                .iter()
                .find(|c| named(&c.base))
                .map(|c| {
                    (
                        ConstructionType::Ceiling(c.construction_type),
                        c.base.construction_makeup.clone(),
                        c.base.construction_phase.clone(),
                    )
                }),
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
    /// tool. Surfaces OUT of structure scope (see
    /// [`WallExposure::in_structure_scope`] and its floor/ceiling mirrors —
    /// non-heat-loss / party / derived inter-storey) are not-applicable, so
    /// they never count as remaining. Empty when `idx` is out of range.
    pub fn incomplete_surface_ids(&self, idx: usize, kind: SurfaceKind) -> Vec<ItemId> {
        let Some(storey) = self.storeys.get(idx) else {
            return Vec::new();
        };
        match kind {
            SurfaceKind::Walls => storey
                .walls
                .regions
                .iter()
                .filter(|r| r.exposure.in_structure_scope())
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
                .filter(|r| r.exposure.in_structure_scope())
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
                .filter(|r| r.exposure.in_structure_scope())
                .filter(|r| {
                    self.ceiling_construction(r.construction_id)
                        .map_or(true, |c| !c.is_complete())
                })
                .map(|r| r.id)
                .collect(),
        }
    }

    /// `(attributed, total)` surfaces of `kind` on storey `idx`, both counted
    /// over the surfaces IN STRUCTURE SCOPE — the Structure-phase header's
    /// "N/M" read-out. `total` is [`Storey::surface_ids`]'s count (which
    /// already skips out-of-scope walls — non-heat-loss / inter-storey);
    /// `attributed` is the complement of
    /// [`incomplete_surface_ids`](Self::incomplete_surface_ids) within it, so
    /// both numbers share the checklist's completeness predicate rather than
    /// re-deriving it. `(0, 0)` for an out-of-range storey.
    pub fn structure_surface_progress(&self, idx: usize, kind: SurfaceKind) -> (usize, usize) {
        let Some(storey) = self.storeys.get(idx) else {
            return (0, 0);
        };
        let total = storey.surface_ids(kind).len();
        let remaining = self.incomplete_surface_ids(idx, kind).len();
        (total.saturating_sub(remaining), total)
    }

    /// Whether EVERY surface of `kind` on storey `idx` is fully attributed — the
    /// Structure-phase checklist's per-storey/kind "done" test. True only when
    /// the storey has at least one such surface and none are
    /// [incomplete](Self::incomplete_surface_ids) (so an empty kind is NOT
    /// vacuously done). Out-of-scope walls (non-heat-loss / inter-storey —
    /// [`WallExposure::in_structure_scope`]) are not-applicable: they don't
    /// block going green, though they do still count toward "the storey has
    /// walls" (the guard is about geometry being derived at all). `false` for
    /// an out-of-range storey.
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

    /// Ceiling completeness demands the INSULATION spec exactly when the
    /// make-up is the plain `Default` (the combined "Default" dropdown's open
    /// question): unspecified blocks; declared-unknown completes without a
    /// photo; a specified thickness completes only once a photo backs it.
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

    /// Every NON-`Default` make-up answers the ceiling insulation question
    /// itself, so completeness falls back to the ordinary build-up rules
    /// (photo-gated for the fabric-asserting ones) with `insulation` left
    /// unspecified — the state the compacted ceiling editor produces, and the
    /// state legacy records with any old makeup/insulation combination load
    /// into.
    #[test]
    fn ceiling_completeness_non_default_makeups_skip_insulation() {
        let ceiling = |makeup: ConstructionMakeup, photo: bool| {
            let mut c = CeilingConstruction {
                construction_type: CeilingConstructionType::FlatRoof,
                ..Default::default()
            };
            c.base.construction_makeup = makeup;
            if photo {
                c.base.photos.push(Photo::default());
            }
            c
        };
        assert!(
            ceiling(ConstructionMakeup::Unknown, false).is_complete(),
            "declared-unknown build-up: insulation not demanded"
        );
        assert!(
            !ceiling(ConstructionMakeup::Layers(vec![]), false).is_complete(),
            "a specified stack still needs its backing photo"
        );
        assert!(
            ceiling(ConstructionMakeup::Layers(vec![]), true).is_complete(),
            "layers + photo complete without a separate insulation spec"
        );
        assert!(
            !ceiling(
                ConstructionMakeup::DefaultPlus {
                    extra_insulation_mm: 150
                },
                false
            )
            .is_complete(),
            "a measured added thickness still needs its backing photo"
        );
        assert!(
            ceiling(
                ConstructionMakeup::DefaultPlus {
                    extra_insulation_mm: 150
                },
                true
            )
            .is_complete(),
            "legacy DefaultPlus + photo completes without the insulation field"
        );
        assert!(
            !ceiling(ConstructionMakeup::Tbd, false).is_complete(),
            "the unset placeholder stays incomplete via the build-up gate"
        );
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

    /// `DefaultPlus` — the default construction plus measured extra
    /// insulation — is a real choice (set), and like `Layers` it asserts
    /// measurable fabric, so its build-up completes only with a backing photo.
    #[test]
    fn default_plus_is_set_and_requires_photo() {
        let dp = ConstructionMakeup::DefaultPlus {
            extra_insulation_mm: 50,
        };
        assert!(dp.is_set(), "DefaultPlus is a made choice");
        assert!(
            dp.requires_photo(),
            "a measured added thickness needs photographic evidence"
        );

        let mut base = ConstructionBase {
            construction_makeup: dp,
            ..Default::default()
        };
        assert!(
            !base.build_up_done(),
            "DefaultPlus without a photo is not done"
        );
        base.photos.push(Photo::default());
        assert!(base.build_up_done(), "a backing photo completes it");
    }

    /// The new `DefaultPlus` variant round-trips through serde, and —
    /// additive evolution — JSON produced by the OLD enum shape (which never
    /// contains the new tag) still deserializes to the same values.
    #[test]
    fn construction_makeup_serde_roundtrip_and_old_shape() {
        // Every variant round-trips, including the new one.
        for mk in [
            ConstructionMakeup::NonHeatLoss,
            ConstructionMakeup::Default,
            ConstructionMakeup::Tbd,
            ConstructionMakeup::Unknown,
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 40,
            },
            ConstructionMakeup::plasterboard_insulation(60),
        ] {
            let back: ConstructionMakeup =
                serde_json::from_str(&serde_json::to_string(&mk).unwrap()).unwrap();
            assert_eq!(back, mk, "round-trip of {mk:?}");
        }

        // The new variant's wire shape (externally tagged struct variant).
        assert_eq!(
            serde_json::to_string(&ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 25
            })
            .unwrap(),
            r#"{"DefaultPlus":{"extra_insulation_mm":25}}"#
        );

        // JSON literals exactly as the OLD shape serialized them (unit
        // variants as strings; Layers externally tagged) keep loading.
        for (old_json, expect) in [
            (r#""NonHeatLoss""#, ConstructionMakeup::NonHeatLoss),
            (r#""Default""#, ConstructionMakeup::Default),
            (r#""Tbd""#, ConstructionMakeup::Tbd),
            (r#""Unknown""#, ConstructionMakeup::Unknown),
            (
                r#"{"Layers":[{"thickness_mm":100,"material":"InsulationEPS","bridged":false}]}"#,
                ConstructionMakeup::Layers(vec![ConstructionLayer {
                    thickness_mm: 100,
                    material: ConstructionMaterial::InsulationEPS,
                    bridged: false,
                }]),
            ),
        ] {
            let loaded: ConstructionMakeup = serde_json::from_str(old_json)
                .unwrap_or_else(|e| panic!("old-shape {old_json} failed to load: {e}"));
            assert_eq!(loaded, expect, "old-shape {old_json}");
        }
    }

    /// The quick plasterboard+insulation shortcut builds the two-layer stack
    /// (plasterboard inside, EPS insulation outside), reads back through
    /// `plasterboard_insulation_mm`, and nothing else masquerades as it.
    #[test]
    fn plasterboard_insulation_quick_buildup() {
        let mk = ConstructionMakeup::plasterboard_insulation(50);
        let ConstructionMakeup::Layers(layers) = &mk else {
            panic!("expected a Layers build-up");
        };
        assert_eq!(layers.len(), 2);
        assert_eq!(layers[0].material, ConstructionMaterial::Plasterboard);
        assert_eq!(layers[0].thickness_mm, PLASTERBOARD_THICKNESS_MM);
        assert_eq!(layers[1].material, ConstructionMaterial::InsulationEPS);
        assert_eq!(layers[1].thickness_mm, 50);
        assert_eq!(mk.plasterboard_insulation_mm(), Some(50));
        assert!(
            mk.requires_photo(),
            "an ordinary specified build-up — photo-gated like any Layers"
        );

        // Non-quick make-ups never read as one: wrong order, extra layers,
        // or a non-Layers make-up.
        let reversed = ConstructionMakeup::Layers(vec![
            ConstructionLayer {
                thickness_mm: 50,
                material: ConstructionMaterial::InsulationEPS,
                bridged: false,
            },
            ConstructionLayer {
                thickness_mm: PLASTERBOARD_THICKNESS_MM,
                material: ConstructionMaterial::Plasterboard,
                bridged: false,
            },
        ]);
        assert_eq!(reversed.plasterboard_insulation_mm(), None);
        assert_eq!(
            ConstructionMakeup::Default.plasterboard_insulation_mm(),
            None
        );
        assert_eq!(
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 50
            }
            .plasterboard_insulation_mm(),
            None
        );
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
