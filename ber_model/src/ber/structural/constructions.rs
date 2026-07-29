use serde::{Deserialize, Serialize};

use crate::ber::ber_struct::Ber;
use crate::ber::common::{ConstructionPhase, Evidence, ItemId};

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
    /// The DEFAULT construction PLUS an upgrade the assessor found added on
    /// top: `extra_insulation_mm` of measured insulation, and — walls'
    /// insulated dry-lining — optionally a plasterboard slab over it.
    /// ADDITIVE serde variant (2026-07): records saved before it existed
    /// never contain the tag, so old JSON keeps loading unchanged.
    ///
    /// TODO(deap-export): the DEAP U-value story — the default construction's
    /// age-band U-value adjusted for the added insulation — is a derivation
    /// for the future deap mapping layer (no U-value derivation exists in
    /// this crate yet); the model just stores the measurement faithfully.
    DefaultPlus {
        extra_insulation_mm: u32,
        /// Whether the upgrade is an insulated DRY-LINING: a plasterboard
        /// slab over the extra insulation (the wall editor's "+ plasterboard"
        /// / "+ pb + X mm" picks — dry-lining always includes the slab, so
        /// plasterboard-only is `plasterboard: true` with 0 mm insulation).
        /// ADDITIVE serde field (2026-07, days after the variant itself):
        /// records saved by the first `DefaultPlus` shape — bare added
        /// insulation, no slab (the retired ✓+ picker) — never contain it
        /// and load as `false`.
        #[serde(default)]
        plasterboard: bool,
    },
    /// The DEFAULT construction PLUS insulation whose EVIDENCE exists but
    /// whose thickness is NOT measurable — the "some insulation" state. Sits
    /// between `Default` (nothing found) and `DefaultPlus` (measured): the
    /// DEAP side grants partial ("half") credit rather than a measured
    /// adjustment. Like every asserted-fabric claim it needs a backing PHOTO
    /// — the photo IS the evidence ([`requires_photo`](Self::requires_photo)).
    /// ADDITIVE serde variant (2026-07): older records never contain the tag,
    /// so old JSON keeps loading unchanged.
    SomeInsulation,
    Layers(Vec<ConstructionLayer>), // custom stack of layers
}

/// The WALL attribute editor's insulated dry-lining THICKNESS options, in mm
/// — the option list of the combined "✓ Default" dropdown's "+ pb + X mm"
/// entries (10 mm steps; the plasterboard slab is always included —
/// [`ConstructionMakeup::DefaultPlus`] with `plasterboard: true`). A shortcut
/// table, not a restriction: free-form stacks stay possible via the layer
/// editor.
pub const WALL_DRY_LINING_THICKNESS_MM: &[u32] = &[10, 20, 30, 40, 50, 60, 70, 80, 90, 100];

/// The CEILING attribute editor's extra-insulation THICKNESS options, in mm —
/// the option list of the combined "Default" dropdown (era-default
/// construction + this much measured insulation, in 25 mm steps; the
/// dropdown's other entry, "unknown", is the TRUE default: era construction,
/// extra insulation unknown). Distinct from the walls' dry-lining table
/// above — loft insulation runs much thicker.
pub const CEILING_INSULATION_THICKNESS_MM: &[u32] =
    &[25, 50, 75, 100, 125, 150, 175, 200, 225, 250, 275, 300];

/// Nominal plasterboard slab thickness, in whole mm (the standard 12.5 mm
/// slab, rounded up — [`ConstructionLayer::thickness_mm`] is integral mm).
/// The thickness the retired ▤ quick picker stamped on its plasterboard
/// layer (see [`ConstructionMakeup::plasterboard_insulation_mm`]).
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
    /// Aerated (aircrete) concrete block — additive 2026-07; serde is
    /// name-based, so older records (which never stored it) are unaffected.
    AeratedBlock,
    /// The assessor LOOKED and could not determine the construction — a made
    /// choice, so `is_set()` and the surface counts as done on the type axis
    /// (unlike [`Uninitialised`](Self::Uninitialised), which is "not looked at
    /// yet").
    ///
    /// SEPARATE from [`ConstructionMakeup::Unknown`], which declares the
    /// BUILD-UP unknown for a KNOWN type ("it's a cavity wall, I don't know
    /// what's in it"). This one is unknown on the TYPE axis ("I don't know what
    /// this wall is built of"), and the two combine freely. Additive 2026-07;
    /// serde is name-based, so older records are unaffected.
    Unknown,
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
            WallConstructionType::AeratedBlock => "Aerated block",
            WallConstructionType::Unknown => "Unknown",
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
    /// `DefaultPlus` WITH measured extra insulation: that's a concrete,
    /// measurable fabric claim on top of the default and (exactly like a
    /// ceiling's specified [`CeilingInsulation::ThicknessMm`]) it needs
    /// photographic evidence. `SomeInsulation` does too — the claim is
    /// unmeasurable but it's still an asserted upgrade, and the photo of the
    /// evidence is what backs the partial credit. `Default` (a standard
    /// assumed construction), `Unknown` (declared unknown) and `NonHeatLoss`
    /// are not specified fabric and need no photo.
    ///
    /// PLASTERBOARD-ONLY dry-lining — `DefaultPlus { extra_insulation_mm: 0,
    /// plasterboard: true }` — needs NO photo either (field call, 2026-07-29).
    /// A bare slab claims no INSULATION: there's no thermal upgrade being
    /// asserted, just the lining the assessor can see, so demanding evidence
    /// for it only blocked the box from completing. Add any thickness and the
    /// photo requirement comes straight back.
    pub fn requires_photo(&self) -> bool {
        match self {
            ConstructionMakeup::Layers(_) | ConstructionMakeup::SomeInsulation => true,
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm,
                ..
            } => *extra_insulation_mm > 0,
            _ => false,
        }
    }

    /// The insulation thickness (mm) when this make-up IS a quick
    /// plasterboard+insulation build-up — exactly a plasterboard layer over
    /// an EPS insulation layer, the two-layer `Layers` stack the RETIRED ▤
    /// quick picker produced. `None` for anything else, including
    /// hand-specified stacks that merely contain those materials among
    /// others. Kept for legacy DISPLAY: the wall editor's combined "✓
    /// Default" button shows such a record as its nearest dry-lining label
    /// without rewriting the stored `Layers` data.
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
            SurfaceExposure::Wall(WallExposure::Irrelevant) => "Irrelevant",
            SurfaceExposure::Wall(WallExposure::Unset) => "Unset",
            SurfaceExposure::Floor(FloorExposure::Ground) => "Ground",
            SurfaceExposure::Floor(FloorExposure::Exposed) => "Exposed",
            SurfaceExposure::Floor(FloorExposure::SemiExposed) => "Semi-exposed",
            SurfaceExposure::Floor(FloorExposure::NonHeatLoss) => "Non heat loss",
            SurfaceExposure::Floor(FloorExposure::InterStorey) => "Inter-storey",
            SurfaceExposure::Floor(FloorExposure::Irrelevant) => "Irrelevant",
            SurfaceExposure::Floor(FloorExposure::Unset) => "Unset",
            SurfaceExposure::Ceiling(CeilingExposure::ExposedRoof) => "Exposed roof",
            SurfaceExposure::Ceiling(CeilingExposure::Party) => "Party",
            SurfaceExposure::Ceiling(CeilingExposure::Unheated) => "Unheated",
            SurfaceExposure::Ceiling(CeilingExposure::InterStorey) => "Inter-storey",
            SurfaceExposure::Ceiling(CeilingExposure::Irrelevant) => "Irrelevant",
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
    /// DERIVED: this surface belongs to an EXCLUDED unheated space (a porch
    /// ruled out of the assessment — DEAP Table 2.2) and sits OUTSIDE the
    /// dwelling's assessment boundary entirely. Distinct from `NonHeatLoss`
    /// (a party/internal element whose area still belongs to the dwelling):
    /// an `Irrelevant` surface contributes to NO heat-loss, area or volume
    /// aggregate. Never assessor-set — `derive_porch_exclusions` stamps and
    /// clears it on every re-derive.
    Irrelevant,
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
        !matches!(
            self,
            WallExposure::NonHeatLoss | WallExposure::InterStorey | WallExposure::Irrelevant
        )
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
    /// DERIVED: this surface belongs to an EXCLUDED unheated space (see
    /// [`WallExposure::Irrelevant`]) — e.g. the floor slab of an excluded
    /// porch. Outside the assessment boundary: no heat loss, no floor area,
    /// no volume. Never assessor-set — `derive_porch_exclusions` stamps and
    /// clears it on every re-derive.
    Irrelevant,
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
            FloorExposure::NonHeatLoss | FloorExposure::InterStorey | FloorExposure::Irrelevant
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
    /// DERIVED: this surface belongs to an EXCLUDED unheated space (see
    /// [`WallExposure::Irrelevant`]) — e.g. the roof/ceiling over an excluded
    /// porch. Outside the assessment boundary: no heat loss, no area. Never
    /// assessor-set — `derive_porch_exclusions` stamps and clears it on
    /// every re-derive.
    Irrelevant,
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
        !matches!(
            self,
            CeilingExposure::Party | CeilingExposure::InterStorey | CeilingExposure::Irrelevant
        )
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
    pub photos: Vec<Evidence>,
    /// The office-RESOLVED U-value (W/m²K), entered in BERresolve from a
    /// submitted U-value calculation — the calculation document itself is
    /// attached to `photos`. Meaningful for a `Layers` (custom) build-up
    /// whose U the office computed; DEAP-default build-ups leave it unset.
    /// `None` = not resolved yet — BERreport then asks its `u-value:{id}`
    /// question; `Some` pre-answers it (the value + `photos` feed the
    /// report's appendix). `#[serde(default)]` so older records load.
    #[serde(default)]
    pub assessed_u_value: Option<f32>,
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
    /// measured extra thickness, `SomeInsulation` declares unmeasurable
    /// insulation (photo-backed via the build-up gate), `Layers` specifies
    /// the stack, `Unknown`/`NonHeatLoss` declare there is nothing further
    /// to specify —
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
    /// make-up (`Unknown`, `Layers`, `DefaultPlus`, `SomeInsulation`,
    /// `NonHeatLoss`) already
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
    /// Rename construction group `id` (any kind — wall, floor or ceiling):
    /// set its custom display name, or CLEAR it back to the type label when
    /// `name` is blank. `false` when no group carries that id. The rename is
    /// display-only — regions reference the group by id, so nothing re-links;
    /// the Structure review, letters and pickers all read the new name.
    pub fn rename_construction_group(&mut self, id: ItemId, name: &str) -> bool {
        let trimmed = name.trim();
        let new = (!trimmed.is_empty()).then(|| trimmed.to_string());
        let base = self
            .wall_constructions
            .iter_mut()
            .map(|c| &mut c.base)
            .chain(self.floor_constructions.iter_mut().map(|c| &mut c.base))
            .chain(self.ceiling_constructions.iter_mut().map(|c| &mut c.base))
            .find(|b| b.id == id);
        match base {
            Some(b) => {
                b.name = new;
                true
            }
            None => false,
        }
    }

    /// MERGE construction phase `from` into `to`: everything that points at
    /// `from` re-points to `to` — every wall/floor/ceiling construction
    /// group's phase tag, the follow-up extension ticks, and the phase
    /// records themselves ([`ConstructionPhases::merge`]: year / nickname /
    /// proof / evidence fold, target wins on conflicts). `from` ends up
    /// undefined. `false` (no change) when the phases are equal or either is
    /// undefined. Construction groups that become fabric-identical after the
    /// re-tag stay SEPARATE groups (their surfaces, photos and names are
    /// untouched) — only the phase pointer moves.
    pub fn merge_construction_phases(
        &mut self,
        from: &ConstructionPhase,
        to: &ConstructionPhase,
    ) -> bool {
        if !self.construction_phases.merge(from, to) {
            return false;
        }
        for base in self
            .wall_constructions
            .iter_mut()
            .map(|c| &mut c.base)
            .chain(self.floor_constructions.iter_mut().map(|c| &mut c.base))
            .chain(self.ceiling_constructions.iter_mut().map(|c| &mut c.base))
        {
            if base.construction_phase == *from {
                base.construction_phase = to.clone();
            }
        }
        // Follow-up extension ticks: a tick on `from` becomes a tick on `to`
        // when `to` is itself a dated extension; merged into Original the
        // tick simply retires (Original is asked via the house-build-date
        // point, not the extension list).
        let ext = &mut self.followup.extensions;
        if ext.contains(from) {
            ext.retain(|p| p != from);
            let to_is_extension = matches!(
                to,
                ConstructionPhase::Extension1
                    | ConstructionPhase::Extension2
                    | ConstructionPhase::Extension3
                    | ConstructionPhase::Extension4
            );
            if to_is_extension && !ext.contains(to) {
                ext.push(to.clone());
            }
        }
        true
    }

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

    /// Drop wall/floor/ceiling constructions that NO region references — the dead
    /// records left behind as walls are re-phased/reclassified/redrawn (find-or-
    /// create mints a new construction and repoints the region, orphaning the old).
    /// Returns the number dropped. CONSERVATIVE: the referenced set is a SUPERSET
    /// gathered from every region collection that carries a construction_id, so a
    /// still-referenced construction is never dropped. CALLER MUST ensure every
    /// storey is fully resolved (measurements_complete) before calling — a storey
    /// mid-dimensioning holds stale/empty regions, and a construction only
    /// transiently unreferenced (with its classification + evidence photos) would
    /// be lost. See the call site in the controller's cross-storey tail.
    ///
    /// The fully-default BASE SEED of each kind — the `(default type, default
    /// make-up, default phase, unnamed)` find-or-create key that
    /// `resolve_storey`/`recompose_ceilings` LOOK UP read-only — is ALWAYS kept,
    /// even when momentarily unreferenced. `recompose_ceilings` reads the ceiling
    /// seed WITHOUT re-ensuring it, so dropping it while a dimensioned storey
    /// transiently holds zero ceiling regions (e.g. ceiling height not entered
    /// yet) would strand the recompose (early return → no ceiling). Keeping the
    /// three seeds is a harmless false-keep (the real accumulation is TYPED/PHASED
    /// records, never the bare seed, which any unclassified surface references
    /// anyway); dropping one breaks the derive pipeline.
    pub fn prune_orphaned_constructions(&mut self) -> usize {
        let mut referenced = self.referenced_construction_ids();
        // Never drop the base seed the resolve pipeline looks up read-only.
        for id in [
            self.wall_construction_id(Default::default(), Default::default(), Default::default()),
            self.floor_construction_id(Default::default(), Default::default(), Default::default()),
            self.ceiling_construction_id(
                Default::default(),
                Default::default(),
                Default::default(),
            ),
        ]
        .into_iter()
        .flatten()
        {
            referenced.insert(id);
        }

        let walls_before = self.wall_constructions.len();
        let floors_before = self.floor_constructions.len();
        let ceilings_before = self.ceiling_constructions.len();

        self.wall_constructions
            .retain(|c| referenced.contains(&c.base.id));
        self.floor_constructions
            .retain(|c| referenced.contains(&c.base.id));
        self.ceiling_constructions
            .retain(|c| referenced.contains(&c.base.id));

        let walls_dropped = walls_before - self.wall_constructions.len();
        let floors_dropped = floors_before - self.floor_constructions.len();
        let ceilings_dropped = ceilings_before - self.ceiling_constructions.len();
        let total = walls_dropped + floors_dropped + ceilings_dropped;
        if total > 0 {
            log::info!(
                "prune_orphaned_constructions: dropped {total} orphaned constructions \
                 ({walls_dropped} wall, {floors_dropped} floor, {ceilings_dropped} ceiling)"
            );
        }
        total
    }

    /// Gather EVERY construction_id any region references, across all storeys
    /// and all region collections that carry one. ItemIds are UUID-unique, so
    /// a single combined set can't cross-keep between the three registries.
    /// Collections walked (the complete set of region carriers):
    ///   * placeable_wall_regions() — perimeter/riser `walls` AND porch inner
    ///     `porch_walls`, the full opening-placeable wall set.
    ///   * floors.regions, living_areas.regions, porches.regions — every
    ///     FloorRegion carrier (each FloorRegion has a construction_id).
    ///   * ceilings.regions — the CeilingRegion carrier.
    fn referenced_construction_ids(&self) -> std::collections::HashSet<ItemId> {
        let mut referenced: std::collections::HashSet<ItemId> = std::collections::HashSet::new();
        for s in &self.storeys {
            for w in s.placeable_wall_regions() {
                referenced.insert(w.construction_id);
            }
            for f in s
                .floors
                .regions
                .iter()
                .chain(s.living_areas.regions.iter())
                .chain(s.porches.regions.iter())
            {
                referenced.insert(f.construction_id);
            }
            for c in &s.ceilings.regions {
                referenced.insert(c.construction_id);
            }
        }
        referenced
    }

    /// The base of construction `id`, whichever registry holds it.
    fn construction_base_mut(&mut self, id: ItemId) -> Option<&mut ConstructionBase> {
        if let Some(c) = self.wall_constructions.iter_mut().find(|c| c.base.id == id) {
            return Some(&mut c.base);
        }
        if let Some(c) = self
            .floor_constructions
            .iter_mut()
            .find(|c| c.base.id == id)
        {
            return Some(&mut c.base);
        }
        if let Some(c) = self
            .ceiling_constructions
            .iter_mut()
            .find(|c| c.base.id == id)
        {
            return Some(&mut c.base);
        }
        None
    }

    /// Carry a construction's EVIDENCE across a find-or-create re-point: when a
    /// stamp re-points regions from construction `old` to `new` (a spec, phase
    /// or name change re-`ensure`s a fresh construction) and `old` ends up
    /// referenced by NO region, its build-up photos — and a ceiling's
    /// insulation spec, when `new` has none — move onto `new`. The group
    /// EVOLVED; evidence recorded on it must ride along rather than die with
    /// the orphan at the next [`prune_orphaned_constructions`]. (Field flow:
    /// photograph the build-up FIRST, then set the spec to match — without the
    /// carry, that exact order strands the photo on the doomed orphan.)
    ///
    /// A still-referenced `old` keeps its evidence: a SUBSET of members forked
    /// away, and the evidence belongs to the stay-behind group. Pairs are
    /// processed in re-point order, so a chain (a→b, b→c) within one pass
    /// ferries the evidence to the terminal construction. Photos merge with
    /// dedup by `Evidence` id; `assessed_u_value` and the name deliberately do
    /// NOT carry (both describe the OLD identity — the office resolved the old
    /// build-up's U, and the name axis is handled by the rename flow). A
    /// re-point onto the unclassified base seed (= a group RESET) never
    /// carries — see the seed guard in the body.
    /// Called by the `document` stamp functions after their re-point loops.
    /// Whether `id` names a construction nothing has been decided about — its
    /// TYPE and its BUILD-UP are both unset. Such a construction is not a group
    /// the assessor built; it is the shared bucket every un-answered surface of
    /// its phase falls into, so nothing group-specific (evidence above all) may
    /// be attached to it. `false` for an unknown id.
    pub fn is_unclassified_construction(&self, id: ItemId) -> bool {
        if let Some(c) = self.wall_constructions.iter().find(|c| c.base.id == id) {
            return !c.construction_type.is_set() && !c.base.construction_makeup.is_set();
        }
        if let Some(c) = self.floor_constructions.iter().find(|c| c.base.id == id) {
            return !c.construction_type.is_set() && !c.base.construction_makeup.is_set();
        }
        if let Some(c) = self.ceiling_constructions.iter().find(|c| c.base.id == id) {
            return !c.construction_type.is_set() && !c.base.construction_makeup.is_set();
        }
        false
    }

    pub fn carry_evidence_from_orphans(&mut self, repoints: &[(ItemId, ItemId)]) {
        if repoints.is_empty() {
            return;
        }
        let referenced = self.referenced_construction_ids();
        // Never carry ONTO an UNCLASSIFIED construction: that re-point is a
        // RESET ("forget this group's attribution"), and an unclassified
        // construction is the shared bucket every un-answered surface of its
        // phase sits in — evidence landing there surfaces on all of them.
        //
        // Tested by NATURE (type unset AND make-up unset), not by identity
        // against the phase-default seed. `stamp_wall_phases` gives each
        // unclassified surface a PHASE-stamped bucket, so the buckets that
        // actually hold the un-answered walls are `Uninitialised/Tbd/Original`,
        // `…/Extension1`, and so on — never the `…/Unassigned` default the old
        // seed lookup asked for. Field report, 56 Elmwood Road: resetting a
        // group carried its photos onto the `Original` bucket that 32 walls
        // shared, so the pictures appeared on walls that were never
        // photographed — and vanished again the moment one of those walls was
        // attributed and forked onto a construction of its own.
        let unclassified: std::collections::HashSet<ItemId> = repoints
            .iter()
            .map(|&(_, new)| new)
            .filter(|&id| self.is_unclassified_construction(id))
            .collect();
        for &(old, new) in repoints {
            if old == new || referenced.contains(&old) || unclassified.contains(&new) {
                continue;
            }
            let photos = match self.construction_base_mut(old) {
                Some(from) if !from.photos.is_empty() => std::mem::take(&mut from.photos),
                _ => Vec::new(),
            };
            let insulation = self
                .ceiling_constructions
                .iter_mut()
                .find(|c| c.base.id == old)
                .and_then(|c| c.insulation.take());
            if photos.is_empty() && insulation.is_none() {
                continue;
            }
            let Some(to) = self.construction_base_mut(new) else {
                // No successor registered (shouldn't happen) — restore the
                // orphan's state rather than dropping evidence.
                if let Some(from) = self.construction_base_mut(old) {
                    from.photos = photos;
                }
                if insulation.is_some() {
                    if let Some(c) = self
                        .ceiling_constructions
                        .iter_mut()
                        .find(|c| c.base.id == old)
                    {
                        c.insulation = insulation;
                    }
                }
                continue;
            };
            for p in photos {
                if !to.photos.iter().any(|q| q.id == p.id) {
                    to.photos.push(p);
                }
            }
            if insulation.is_some() {
                if let Some(c) = self
                    .ceiling_constructions
                    .iter_mut()
                    .find(|c| c.base.id == new)
                {
                    if c.insulation.is_none() {
                        c.insulation = insulation;
                    }
                }
            }
        }
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
    pub fn add_construction_photo(&mut self, kind: SurfaceKind, id: ItemId, photo: Evidence) {
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
                .chain(storey.structure_porch_walls())
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

    /// WHY [`structure_surfaces_complete`](Self::structure_surfaces_complete)
    /// isn't green for (`idx`, `kind`) — the SAME branches, decomposed into
    /// user-facing reasons for the tappable checklist row. Guaranteed empty
    /// exactly when the predicate is true: both read the same storey guard,
    /// the same [`Storey::surface_counts`] gate, and the same
    /// [`incomplete_surface_ids`](Self::incomplete_surface_ids) set, so colour
    /// and reasons can never disagree. Each incomplete surface is attributed
    /// to the FIRST failing clause of its construction's completeness test
    /// ([`WallConstruction::is_complete`] and its floor/ceiling mirrors):
    /// unresolvable construction → type unset → build-up unset → photo-backed
    /// build-up missing its photo → (ceilings only) insulation spec
    /// unresolved. `m` in the "n of m" messages is the same
    /// [`Storey::surface_ids`] total the Structure header's "N/M" read-out
    /// uses.
    pub fn structure_incomplete_reasons(&self, idx: usize, kind: SurfaceKind) -> Vec<String> {
        let Some(storey) = self.storeys.get(idx) else {
            return vec!["Storey not resolved yet".to_string()];
        };
        let counts = storey.surface_counts();
        let (count, noun) = match kind {
            SurfaceKind::Walls => (counts.walls, "wall"),
            SurfaceKind::Floors => (counts.floors, "floor"),
            SurfaceKind::Ceilings => (counts.ceilings, "ceiling"),
        };
        if count == 0 {
            return vec![format!("No {noun} surfaces resolved yet")];
        }
        let total = storey.surface_ids(kind).len();
        // Bucket counters, in the order `is_complete` checks its clauses. The
        // `other` catch-all keeps the emptiness guarantee airtight even if a
        // future `is_complete` clause isn't decomposed here yet.
        let (mut unassigned, mut no_type, mut no_build_up, mut no_photo, mut insulation, mut other) =
            (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
        for id in self.incomplete_surface_ids(idx, kind) {
            // The same region → construction lookups the incomplete filter
            // used: (type set, base build-up state, insulation answered).
            let attrs: Option<(bool, &ConstructionBase, bool)> = match kind {
                SurfaceKind::Walls => storey
                    // `placeable_wall` spans perimeter AND porch inner walls —
                    // an incomplete id can be either.
                    .placeable_wall(id)
                    .and_then(|r| self.wall_construction(r.construction_id))
                    .map(|c| (c.construction_type.is_set(), &c.base, true)),
                SurfaceKind::Floors => storey
                    .floors
                    .regions
                    .iter()
                    .find(|r| r.id == id)
                    .and_then(|r| self.floor_construction(r.construction_id))
                    .map(|c| (c.construction_type.is_set(), &c.base, true)),
                SurfaceKind::Ceilings => storey
                    .ceilings
                    .regions
                    .iter()
                    .find(|r| r.id == id)
                    .and_then(|r| self.ceiling_construction(r.construction_id))
                    .map(|c| {
                        (
                            c.construction_type.is_set(),
                            &c.base,
                            CeilingInsulation::spec_done_for(
                                &c.base.construction_makeup,
                                c.insulation,
                                !c.base.photos.is_empty(),
                            ),
                        )
                    }),
            };
            match attrs {
                None => unassigned += 1,
                Some((type_set, base, insulation_done)) => {
                    if !type_set {
                        no_type += 1;
                    } else if !base.construction_makeup.is_set() {
                        no_build_up += 1;
                    } else if base.construction_makeup.requires_photo() && base.photos.is_empty() {
                        no_photo += 1;
                    } else if !insulation_done {
                        insulation += 1;
                    } else {
                        other += 1; // unreachable while is_complete has exactly these clauses
                    }
                }
            }
        }
        let mut reasons = Vec::new();
        for (n, cause) in [
            (unassigned, "no construction assigned"),
            (no_type, "no construction type set"),
            (no_build_up, "no build-up set"),
            (no_photo, "a build-up awaiting its backing photo"),
            (
                insulation,
                "extra insulation unspecified or awaiting its photo",
            ),
            (other, "an incomplete construction"),
        ] {
            if n > 0 {
                reasons.push(format!("{n} of {total} {noun} surface(s) have {cause}"));
            }
        }
        reasons
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `structure_incomplete_reasons` is the detailed core behind
    /// `structure_surfaces_complete`: the reason list is empty exactly when
    /// the predicate is green (swept at every stage of the fixture's life,
    /// over every kind), and the bucket messages carry the same counts the
    /// predicate's branches see.
    #[test]
    fn structure_incomplete_reasons_track_the_completeness_predicate() {
        use crate::ber::structural::{Storey, SurfaceKind, WallExposure, WallRegion, WallRegions};
        use domain::points::Point3f64;

        let equivalent = |ber: &Ber, idx: usize| {
            for kind in [
                SurfaceKind::Walls,
                SurfaceKind::Floors,
                SurfaceKind::Ceilings,
            ] {
                assert_eq!(
                    ber.structure_incomplete_reasons(idx, kind).is_empty(),
                    ber.structure_surfaces_complete(idx, kind),
                    "reasons/colour must agree for {kind:?} on storey {idx}"
                );
            }
        };

        // Out-of-range storey.
        let mut ber = Ber::default();
        assert_eq!(
            ber.structure_incomplete_reasons(0, SurfaceKind::Walls),
            vec!["Storey not resolved yet".to_string()]
        );
        equivalent(&ber, 0);

        // Storey with no surfaces of the kind resolved yet.
        ber.storeys.push(Storey::default());
        assert_eq!(
            ber.structure_incomplete_reasons(0, SurfaceKind::Walls),
            vec!["No wall surfaces resolved yet".to_string()]
        );
        assert_eq!(
            ber.structure_incomplete_reasons(0, SurfaceKind::Ceilings),
            vec!["No ceiling surfaces resolved yet".to_string()]
        );
        equivalent(&ber, 0);

        // One wall on a SPECIFIED (Layers) build-up with no photo yet.
        let cid = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Layers(vec![]),
            ConstructionPhase::default(),
        );
        let wall = |construction_id| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id,
        };
        let mut walls = WallRegions::default();
        walls.regions = vec![wall(cid)];
        ber.storeys[0].walls = walls;
        assert_eq!(
            ber.structure_incomplete_reasons(0, SurfaceKind::Walls),
            vec!["1 of 1 wall surface(s) have a build-up awaiting its backing photo".to_string()]
        );
        equivalent(&ber, 0);

        // A second wall whose construction id resolves to nothing → the two
        // causes report side by side, over the same total.
        ber.storeys[0].walls.regions.push(wall(ItemId::default()));
        assert_eq!(
            ber.structure_incomplete_reasons(0, SurfaceKind::Walls),
            vec![
                "1 of 2 wall surface(s) have no construction assigned".to_string(),
                "1 of 2 wall surface(s) have a build-up awaiting its backing photo".to_string(),
            ]
        );
        equivalent(&ber, 0);

        // Back the build-up with a photo and assign the orphan → green, and
        // the reason list agrees by construction.
        ber.wall_constructions
            .iter_mut()
            .find(|c| c.base.id == cid)
            .unwrap()
            .base
            .photos
            .push(Evidence::default());
        ber.storeys[0].walls.regions[1].construction_id = cid;
        assert!(ber
            .structure_incomplete_reasons(0, SurfaceKind::Walls)
            .is_empty());
        assert!(ber.structure_surfaces_complete(0, SurfaceKind::Walls));
        equivalent(&ber, 0);
    }

    /// Every `WallConstructionType` (incl. the additive `AeratedBlock`)
    /// round-trips through serde, and — additive evolution — JSON produced by
    /// the OLD enum shape (which had no `AeratedBlock` variant) still
    /// deserializes to the same values. Labels stay real for every pickable
    /// type.
    #[test]
    fn wall_construction_type_serde_roundtrip_and_old_shape() {
        for ty in [
            WallConstructionType::CavityWall,
            WallConstructionType::FilledCavityWall,
            WallConstructionType::ConcreteHollowBlocks,
            WallConstructionType::TimberConstruction,
            WallConstructionType::ICF,
            WallConstructionType::AeratedBlock,
            WallConstructionType::Floor,
            WallConstructionType::Uninitialised,
        ] {
            let back: WallConstructionType =
                serde_json::from_str(&serde_json::to_string(&ty).unwrap()).unwrap();
            assert_eq!(back, ty, "round-trip of {ty:?}");
        }
        // Unit variants serialize as plain name strings.
        assert_eq!(
            serde_json::to_string(&WallConstructionType::AeratedBlock).unwrap(),
            r#""AeratedBlock""#
        );
        // JSON literals exactly as the OLD shape serialized them keep loading.
        for (old_json, expect) in [
            (r#""CavityWall""#, WallConstructionType::CavityWall),
            (
                r#""FilledCavityWall""#,
                WallConstructionType::FilledCavityWall,
            ),
            (
                r#""ConcreteHollowBlocks""#,
                WallConstructionType::ConcreteHollowBlocks,
            ),
            (
                r#""TimberConstruction""#,
                WallConstructionType::TimberConstruction,
            ),
            (r#""ICF""#, WallConstructionType::ICF),
            (r#""Floor""#, WallConstructionType::Floor),
            (r#""Uninitialised""#, WallConstructionType::Uninitialised),
        ] {
            let got: WallConstructionType = serde_json::from_str(old_json).unwrap();
            assert_eq!(got, expect, "old-shape {old_json} loads unchanged");
        }
        assert_eq!(WallConstructionType::AeratedBlock.label(), "Aerated block");
        assert!(WallConstructionType::AeratedBlock.is_set());
    }

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
        c.base.photos.push(Evidence::default());
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
                c.base.photos.push(Evidence::default());
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
                    extra_insulation_mm: 150,
                    plasterboard: false
                },
                false
            )
            .is_complete(),
            "a measured added thickness still needs its backing photo"
        );
        assert!(
            ceiling(
                ConstructionMakeup::DefaultPlus {
                    extra_insulation_mm: 150,
                    plasterboard: false
                },
                true
            )
            .is_complete(),
            "legacy DefaultPlus + photo completes without the insulation field"
        );
        assert!(
            !ceiling(ConstructionMakeup::SomeInsulation, false).is_complete(),
            "some-insulation still needs its evidence photo"
        );
        assert!(
            ceiling(ConstructionMakeup::SomeInsulation, true).is_complete(),
            "some-insulation + photo completes without a separate insulation spec"
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

    /// `DefaultPlus` — the default construction plus a found upgrade — is a
    /// real choice (set), and where it claims MEASURED extra insulation it
    /// asserts fabric like `Layers` does, so its build-up completes only with
    /// a backing photo.
    #[test]
    fn default_plus_is_set_and_requires_photo() {
        for dp in [
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 50,
                plasterboard: false,
            },
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 50,
                plasterboard: true,
            },
        ] {
            assert!(dp.is_set(), "{dp:?} is a made choice");
            assert!(
                dp.requires_photo(),
                "{dp:?}: an asserted fabric upgrade needs photographic evidence"
            );

            let mut base = ConstructionBase {
                construction_makeup: dp.clone(),
                ..Default::default()
            };
            assert!(!base.build_up_done(), "{dp:?} without a photo is not done");
            base.photos.push(Evidence::default());
            assert!(base.build_up_done(), "a backing photo completes {dp:?}");
        }
    }

    /// FIELD ADDITION (2026-07-29): the TYPE axis gets its own `Unknown` —
    /// "I looked and can't tell what this wall is built of" — alongside cavity
    /// / filled cavity / hollow block / timber. It is a MADE CHOICE (`is_set`),
    /// so unlike `Uninitialised` it completes the type half of the surface.
    ///
    /// It is SEPARATE from `ConstructionMakeup::Unknown`, which declares the
    /// BUILD-UP of a KNOWN type unknown. The two axes are orthogonal and combine
    /// freely — including "unknown type, unknown build-up", which is complete
    /// (both are answers) and needs no photo.
    #[test]
    fn wall_type_unknown_is_a_made_choice_distinct_from_makeup_unknown() {
        assert!(
            WallConstructionType::Unknown.is_set(),
            "an unknown TYPE is an answer, not the unset placeholder"
        );
        assert!(!WallConstructionType::Uninitialised.is_set());
        assert_eq!(WallConstructionType::Unknown.label(), "Unknown");

        // Unknown TYPE + a known build-up: complete.
        let known_makeup = WallConstruction {
            construction_type: WallConstructionType::Unknown,
            base: ConstructionBase {
                construction_makeup: ConstructionMakeup::Default,
                ..Default::default()
            },
        };
        assert!(
            known_makeup.is_complete(),
            "unknown type + default build-up is a fully answered wall"
        );

        // A known TYPE with an unknown build-up: also complete — the other axis.
        let known_type = WallConstruction {
            construction_type: WallConstructionType::CavityWall,
            base: ConstructionBase {
                construction_makeup: ConstructionMakeup::Unknown,
                ..Default::default()
            },
        };
        assert!(known_type.is_complete());

        // Both unknown: still complete, still no photo demanded.
        let both = WallConstruction {
            construction_type: WallConstructionType::Unknown,
            base: ConstructionBase {
                construction_makeup: ConstructionMakeup::Unknown,
                ..Default::default()
            },
        };
        assert!(!ConstructionMakeup::Unknown.requires_photo());
        assert!(both.is_complete(), "two answers are still two answers");

        // But an unknown TYPE does not excuse the UNSET build-up.
        let makeup_tbd = WallConstruction {
            construction_type: WallConstructionType::Unknown,
            base: ConstructionBase::default(),
        };
        assert!(
            !makeup_tbd.is_complete(),
            "the build-up axis is still unanswered"
        );
    }

    /// FIELD CALL (2026-07-29): PLASTERBOARD-ONLY dry-lining — "default plus
    /// plasterboard", 0 mm extra insulation — is a made choice that needs NO
    /// photo. A bare slab asserts no INSULATION, so there is no measurable
    /// thermal claim to evidence; requiring one only left the box incomplete.
    /// Adding any thickness restores the requirement.
    #[test]
    fn plasterboard_only_dry_lining_needs_no_photo() {
        let pb_only = ConstructionMakeup::DefaultPlus {
            extra_insulation_mm: 0,
            plasterboard: true,
        };
        assert!(pb_only.is_set(), "plasterboard-only is a made choice");
        assert!(
            !pb_only.requires_photo(),
            "a bare slab claims no insulation — nothing to evidence"
        );
        let base = ConstructionBase {
            construction_makeup: pb_only,
            ..Default::default()
        };
        assert!(
            base.build_up_done(),
            "…so the build-up completes with no photo attached"
        );

        // One millimetre of claimed insulation and the evidence is due again.
        let with_insulation = ConstructionMakeup::DefaultPlus {
            extra_insulation_mm: 10,
            plasterboard: true,
        };
        assert!(
            with_insulation.requires_photo(),
            "insulated dry-lining is a measurable claim — photo required"
        );
    }

    /// `SomeInsulation` — evidence of insulation exists but the thickness is
    /// unmeasurable — is a real choice (set) and, like every asserted-fabric
    /// claim, completes only with a backing photo (the photo IS the
    /// evidence). Its wire shape is the additive plain-string tag.
    #[test]
    fn some_insulation_is_set_and_requires_photo() {
        let mk = ConstructionMakeup::SomeInsulation;
        assert!(mk.is_set(), "some-insulation is a made choice");
        assert!(mk.requires_photo(), "the evidence photo backs the claim");

        let mut base = ConstructionBase {
            construction_makeup: mk,
            ..Default::default()
        };
        assert!(!base.build_up_done(), "no photo → not done");
        base.photos.push(Evidence::default());
        assert!(base.build_up_done(), "a backing photo completes it");

        assert_eq!(
            serde_json::to_string(&ConstructionMakeup::SomeInsulation).unwrap(),
            r#""SomeInsulation""#
        );
    }

    /// The `DefaultPlus` variant (incl. the additive `plasterboard` field)
    /// round-trips through serde, and — additive evolution — JSON produced by
    /// the OLD enum shapes (no `DefaultPlus` tag at all; a `DefaultPlus`
    /// without the `plasterboard` field) still deserializes to the same
    /// values.
    #[test]
    fn construction_makeup_serde_roundtrip_and_old_shape() {
        // Every variant round-trips, including the new dry-lining shapes.
        for mk in [
            ConstructionMakeup::NonHeatLoss,
            ConstructionMakeup::Default,
            ConstructionMakeup::Tbd,
            ConstructionMakeup::Unknown,
            ConstructionMakeup::SomeInsulation,
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 40,
                plasterboard: false,
            },
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 0,
                plasterboard: true,
            },
            ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 50,
                plasterboard: true,
            },
        ] {
            let back: ConstructionMakeup =
                serde_json::from_str(&serde_json::to_string(&mk).unwrap()).unwrap();
            assert_eq!(back, mk, "round-trip of {mk:?}");
        }

        // The variant's wire shape (externally tagged struct variant).
        assert_eq!(
            serde_json::to_string(&ConstructionMakeup::DefaultPlus {
                extra_insulation_mm: 25,
                plasterboard: true
            })
            .unwrap(),
            r#"{"DefaultPlus":{"extra_insulation_mm":25,"plasterboard":true}}"#
        );

        // JSON literals exactly as the OLD shapes serialized them (unit
        // variants as strings; Layers externally tagged; DefaultPlus before
        // the `plasterboard` field, which loads as `false`) keep loading.
        for (old_json, expect) in [
            (r#""NonHeatLoss""#, ConstructionMakeup::NonHeatLoss),
            (r#""Default""#, ConstructionMakeup::Default),
            (r#""Tbd""#, ConstructionMakeup::Tbd),
            (r#""Unknown""#, ConstructionMakeup::Unknown),
            (
                r#"{"DefaultPlus":{"extra_insulation_mm":25}}"#,
                ConstructionMakeup::DefaultPlus {
                    extra_insulation_mm: 25,
                    plasterboard: false,
                },
            ),
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

    /// `plasterboard_insulation_mm` recognizes exactly the two-layer stack
    /// the RETIRED ▤ quick picker saved (plasterboard inside, EPS insulation
    /// outside) — the legacy-display hook — and nothing else masquerades as
    /// it.
    #[test]
    fn plasterboard_insulation_quick_buildup() {
        let mk = ConstructionMakeup::Layers(vec![
            ConstructionLayer {
                thickness_mm: PLASTERBOARD_THICKNESS_MM,
                material: ConstructionMaterial::Plasterboard,
                bridged: false,
            },
            ConstructionLayer {
                thickness_mm: 50,
                material: ConstructionMaterial::InsulationEPS,
                bridged: false,
            },
        ]);
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
                extra_insulation_mm: 50,
                plasterboard: true
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

    /// `prune_orphaned_constructions` drops exactly the wall/floor/ceiling
    /// constructions no region references — including Uninitialised ones AND
    /// typed constructions carrying evidence photos — while keeping every
    /// referenced one, including a construction referenced ONLY by the second
    /// storey (guards a per-storey scan bug). Idempotent on a second run.
    #[test]
    fn prune_orphaned_constructions_drops_only_unreferenced() {
        use crate::ber::structural::{
            CeilingRegions, FloorRegions, Storey, SurfaceKind, WallExposure, WallRegion,
            WallRegions,
        };
        use domain::points::{Point2f64, Point3f64};

        let mut ber = Ber::default();

        // Referenced constructions (survivors).
        let wall_cid_a = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        // Referenced ONLY by the second storey — guards a per-storey scan bug.
        let wall_cid_b = ber.ensure_wall_construction(
            WallConstructionType::FilledCavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let floor_cid = ber.ensure_floor_construction(
            FloorConstructionType::SolidFloor,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let ceiling_cid = ber.ensure_ceiling_construction(
            CeilingConstructionType::FlatRoof,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );

        // Orphans, never referenced by any region: some barely-classified (Tbd
        // make-up on a real type), some typed WITH evidence photos — to prove
        // even a photographed classification is dropped once nothing references
        // it. Deliberately NON-default types, so none is the preserved base seed.
        let orphan_wall_bare = ber.ensure_wall_construction(
            WallConstructionType::TimberConstruction,
            ConstructionMakeup::Tbd,
            ConstructionPhase::default(),
        );
        let orphan_wall_photo = ber.ensure_wall_construction(
            WallConstructionType::ICF,
            ConstructionMakeup::Layers(vec![]),
            ConstructionPhase::default(),
        );
        ber.add_construction_photo(SurfaceKind::Walls, orphan_wall_photo, Evidence::default());
        let orphan_floor = ber.ensure_floor_construction(
            FloorConstructionType::SuspendedFloor,
            ConstructionMakeup::Layers(vec![]),
            ConstructionPhase::default(),
        );
        ber.add_construction_photo(SurfaceKind::Floors, orphan_floor, Evidence::default());
        let orphan_ceiling = ber.ensure_ceiling_construction(
            CeilingConstructionType::SlopedInsOnRafter,
            ConstructionMakeup::Tbd,
            ConstructionPhase::default(),
        );

        assert_eq!(ber.wall_constructions.len(), 4);
        assert_eq!(ber.floor_constructions.len(), 2);
        assert_eq!(ber.ceiling_constructions.len(), 2);

        let wall = |construction_id| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id,
        };
        let quad = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 2.0),
            Point2f64::new(0.0, 2.0),
        ];

        // Storey 0: a wall (wall_cid_a) + a floor (floor_cid).
        let s0 = Storey {
            walls: WallRegions {
                regions: vec![wall(wall_cid_a)],
            },
            floors: FloorRegions::new(quad.clone(), floor_cid),
            ..Storey::default()
        };
        // Storey 1: a wall referencing ONLY wall_cid_b + a ceiling (ceiling_cid).
        let s1 = Storey {
            walls: WallRegions {
                regions: vec![wall(wall_cid_b)],
            },
            ceilings: CeilingRegions::new(
                vec![
                    Point3f64::new(0.0, 0.0, 2.4),
                    Point3f64::new(2.0, 0.0, 2.4),
                    Point3f64::new(2.0, 2.0, 2.4),
                    Point3f64::new(0.0, 2.0, 2.4),
                ],
                ceiling_cid,
            ),
            ..Storey::default()
        };
        ber.storeys = vec![s0, s1];

        let dropped = ber.prune_orphaned_constructions();
        assert_eq!(dropped, 4, "two wall + one floor + one ceiling orphan");

        let wall_ids: Vec<ItemId> = ber.wall_constructions.iter().map(|c| c.base.id).collect();
        assert_eq!(wall_ids.len(), 2);
        assert!(wall_ids.contains(&wall_cid_a));
        assert!(
            wall_ids.contains(&wall_cid_b),
            "kept even though only storey 1 references it"
        );
        assert!(!wall_ids.contains(&orphan_wall_bare));
        assert!(!wall_ids.contains(&orphan_wall_photo));

        let floor_ids: Vec<ItemId> = ber.floor_constructions.iter().map(|c| c.base.id).collect();
        assert_eq!(floor_ids, vec![floor_cid]);
        let ceiling_ids: Vec<ItemId> = ber
            .ceiling_constructions
            .iter()
            .map(|c| c.base.id)
            .collect();
        assert_eq!(ceiling_ids, vec![ceiling_cid]);
        let _ = orphan_ceiling;

        // Idempotent: a second prune drops nothing.
        assert_eq!(ber.prune_orphaned_constructions(), 0);
    }

    /// `carry_evidence_from_orphans`: a re-point that leaves the old
    /// construction UNREFERENCED moves its photos (and a ceiling's insulation
    /// spec) onto the successor — including across a chain (a→b, b→c) in one
    /// pass — while a still-referenced old (a subset forked away) keeps its
    /// evidence with the stay-behind group.
    #[test]
    fn carry_evidence_from_orphans_moves_photos_only_off_unreferenced() {
        use crate::ber::structural::{Storey, SurfaceKind, WallExposure, WallRegion, WallRegions};
        use domain::points::Point3f64;

        let mut ber = Ber::default();
        // Wall chain: a (photo, orphaned) → b (orphaned) → c (referenced).
        let a = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let b = ber.ensure_wall_construction(
            WallConstructionType::FilledCavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let c = ber.ensure_wall_construction(
            WallConstructionType::TimberConstruction,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let photo_a = Evidence::default();
        ber.add_construction_photo(SurfaceKind::Walls, a, photo_a.clone());
        // A subset fork: `kept` stays referenced, so its photo must NOT move.
        let kept = ber.ensure_wall_construction(
            WallConstructionType::ICF,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let fork = ber.ensure_wall_construction(
            WallConstructionType::ConcreteHollowBlocks,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let photo_kept = Evidence::default();
        ber.add_construction_photo(SurfaceKind::Walls, kept, photo_kept.clone());
        // Ceilings: insulation + photo carried onto the successor.
        let ceil_old = ber.ensure_ceiling_construction(
            CeilingConstructionType::FlatRoof,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let ceil_new = ber.ensure_ceiling_construction(
            CeilingConstructionType::SlopedInsOnRafter,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let photo_ceil = Evidence::default();
        ber.add_construction_photo(SurfaceKind::Ceilings, ceil_old, photo_ceil.clone());
        ber.set_ceiling_insulation(ceil_old, Some(CeilingInsulation::ThicknessMm(100.0)));

        let wall = |construction_id| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 0.0),
                Point3f64::new(1.0, 0.0, 2.4),
                Point3f64::new(0.0, 0.0, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id,
        };
        ber.storeys = vec![Storey {
            walls: WallRegions {
                regions: vec![wall(c), wall(kept), wall(fork)],
            },
            ceilings: CeilingRegions::new(
                vec![
                    Point3f64::new(0.0, 0.0, 2.4),
                    Point3f64::new(2.0, 0.0, 2.4),
                    Point3f64::new(2.0, 2.0, 2.4),
                    Point3f64::new(0.0, 2.0, 2.4),
                ],
                ceil_new,
            ),
            ..Storey::default()
        }];

        ber.carry_evidence_from_orphans(&[(a, b), (b, c), (kept, fork), (ceil_old, ceil_new)]);

        let photos_of = |ber: &Ber, id| ber.wall_construction(id).unwrap().base.photos.clone();
        assert!(photos_of(&ber, a).is_empty(), "a drained");
        assert!(photos_of(&ber, b).is_empty(), "b only ferried");
        assert_eq!(
            photos_of(&ber, c).iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![photo_a.id],
            "the chain lands a's photo on the terminal construction"
        );
        assert_eq!(
            photos_of(&ber, kept)
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            vec![photo_kept.id],
            "a still-referenced old keeps its evidence (subset fork)"
        );
        assert!(photos_of(&ber, fork).is_empty());
        let cn = ber.ceiling_construction(ceil_new).unwrap();
        assert_eq!(
            cn.base.photos.iter().map(|p| p.id).collect::<Vec<_>>(),
            vec![photo_ceil.id]
        );
        assert_eq!(
            cn.insulation,
            Some(CeilingInsulation::ThicknessMm(100.0)),
            "the insulation spec rides along too"
        );
        assert!(ber
            .ceiling_construction(ceil_old)
            .unwrap()
            .base
            .photos
            .is_empty());

        // Re-running with the same pairs is a no-op (evidence already moved).
        ber.carry_evidence_from_orphans(&[(a, b), (b, c)]);
        assert_eq!(photos_of(&ber, c).len(), 1);
    }

    /// A construction referenced ONLY via the extra region collections
    /// (living areas, porches, porch inner walls) is KEPT — those carriers are
    /// part of the referenced-set sweep, so nothing living there is falsely
    /// orphaned.
    #[test]
    fn prune_keeps_constructions_referenced_via_extra_collections() {
        use crate::ber::structural::{FloorRegions, Storey, WallExposure, WallRegion, WallRegions};
        use domain::points::{Point2f64, Point3f64};

        let mut ber = Ber::default();
        let living_cid = ber.ensure_floor_construction(
            FloorConstructionType::SolidFloor,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let porch_floor_cid = ber.ensure_floor_construction(
            FloorConstructionType::SuspendedFloor,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );
        let porch_wall_cid = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );

        let quad = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(2.0, 0.0),
            Point2f64::new(2.0, 2.0),
            Point2f64::new(0.0, 2.0),
        ];

        // NONE of these live in floors/walls/ceilings — only the extra carriers.
        let s = Storey {
            living_areas: FloorRegions::new(quad.clone(), living_cid),
            porches: FloorRegions::new(quad.clone(), porch_floor_cid),
            porch_walls: WallRegions {
                regions: vec![WallRegion {
                    id: ItemId::default(),
                    polygon: Polygon3D::new(vec![
                        Point3f64::new(0.0, 0.0, 0.0),
                        Point3f64::new(1.0, 0.0, 0.0),
                        Point3f64::new(1.0, 0.0, 2.4),
                        Point3f64::new(0.0, 0.0, 2.4),
                    ]),
                    exposure: WallExposure::Unset,
                    opes: Opes::default(),
                    construction_id: porch_wall_cid,
                }],
            },
            ..Storey::default()
        };
        ber.storeys = vec![s];

        let dropped = ber.prune_orphaned_constructions();
        assert_eq!(
            dropped, 0,
            "living-area / porch / porch-wall references all keep their constructions"
        );
        assert_eq!(ber.floor_constructions.len(), 2);
        assert_eq!(ber.wall_constructions.len(), 1);
    }

    /// The fully-default BASE SEED of each kind — the `(default type, default
    /// make-up, default phase)` find-or-create key `resolve_storey` /
    /// `recompose_ceilings` look up read-only — is KEPT even with no region
    /// referencing it, so a dimensioned storey that momentarily holds zero
    /// regions of a kind doesn't strand the derive pipeline. A TYPED orphan
    /// alongside it is still dropped.
    #[test]
    fn prune_preserves_the_base_seed_constructions() {
        let mut ber = Ber::default();
        let wall_seed = ber.ensure_wall_construction(
            WallConstructionType::default(),
            ConstructionMakeup::default(),
            ConstructionPhase::default(),
        );
        let floor_seed = ber.ensure_floor_construction(
            FloorConstructionType::default(),
            ConstructionMakeup::default(),
            ConstructionPhase::default(),
        );
        let ceiling_seed = ber.ensure_ceiling_construction(
            CeilingConstructionType::default(),
            ConstructionMakeup::default(),
            ConstructionPhase::default(),
        );
        // A typed orphan that IS droppable (not a seed).
        let typed_orphan = ber.ensure_wall_construction(
            WallConstructionType::CavityWall,
            ConstructionMakeup::Default,
            ConstructionPhase::default(),
        );

        // No storeys / regions at all — everything is unreferenced.
        let dropped = ber.prune_orphaned_constructions();
        assert_eq!(dropped, 1, "only the typed orphan drops");
        assert!(ber.wall_construction(wall_seed).is_some(), "wall seed kept");
        assert!(
            ber.floor_construction(floor_seed).is_some(),
            "floor seed kept"
        );
        assert!(
            ber.ceiling_construction(ceiling_seed).is_some(),
            "ceiling seed kept"
        );
        assert!(
            ber.wall_construction(typed_orphan).is_none(),
            "the typed orphan is gone"
        );
    }
}
