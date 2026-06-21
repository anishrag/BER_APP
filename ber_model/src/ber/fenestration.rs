// src/data/fenestration.rs

use manip_plan::SurfaceRef;
use serde::{Deserialize, Serialize};

use super::common::{ItemId, Photo, Year};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum Glazing {
    Single,
    #[default]
    Double,
    Triple,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum Frame {
    #[default]
    WoodUPVC,
    Metal,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WindowNameSpecifics {
    Standard,
    Custom(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowType {
    pub id: ItemId,
    pub year: Year,
    pub glazing: Glazing,
    pub gap: Vec<u32>, // gap in mm for double/triple glazing
    pub frame: Frame,
    pub specifics: WindowNameSpecifics,
    pub photos: Vec<Photo>,
    pub notes: Option<String>,
}

/// Durable PARAMETRIC anchor for a wall opening: the perimeter wall it sits on
/// (by stable `VertexId`, via [`SurfaceRef`]) plus the opening centre's
/// along-position as a fraction (0..1) of that perimeter wall. The fraction is
/// PARENT-relative, so a structure split is handled for free — the fraction
/// picks which sub-wall the centre falls in. The `wall_id`/`along_m` inside the
/// opening's `placement` are a per-resolve CACHE re-derived from this anchor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct WallAnchor {
    pub wall: SurfaceRef,
    pub along_fraction: f32,
}

/// Where an opening sits, parametrically — the DURABLE record of its location.
/// The 3D `vertices` on `Window`/`Door` are a CACHE derived from this at resolve
/// (`super::openings::derive_opening_vertices`); `placement: None` marks a legacy
/// record whose stored `vertices` stay authoritative.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OpeningPlacement {
    /// On a vertical wall. `along_m` is the CENTRE distance along the wall's
    /// base edge from `exterior[0]` toward `exterior[1]`; `sill_m` is the
    /// opening's bottom height above the wall's `base_z`.
    Wall {
        wall_id: ItemId,
        along_m: f32,
        sill_m: f32,
    },
    /// On a (possibly sloped) roof/ceiling region — free placement at a plan
    /// point inside the region; the z's derive from the region plane.
    Roof {
        roof_region_id: ItemId,
        plan: (f32, f32),
    },
}

/// The make-up of a door panel. `AssemblyPanel` is the door element of a
/// door+sidelights assembly (the glazed sidelights/transom are separate
/// `Window`s sharing the door's `assembly_id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DoorKind {
    Solid,
    HalfGlazed,
    FullyGlazed,
    AssemblyPanel,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: ItemId,
    /// DERIVED cache (recomputed at resolve from `placement`); authoritative
    /// only for legacy records with `placement: None`.
    pub vertices: Vec<(f32, f32, f32)>,
    pub width_m: f32,
    pub height_m: f32,
    pub window_type_id: ItemId,
    #[serde(default)]
    pub placement: Option<OpeningPlacement>,
    /// Durable PARAMETRIC anchor for a wall opening: the perimeter wall it sits
    /// on, by stable [`SurfaceRef`]. The host `wall_id` inside `placement` is a
    /// resolved-region id that the wall amend-match recovers across derivation
    /// edits and `rehome` rebases across splits — but an outline RESHAPE mints a
    /// fresh wall id it can't recover. The resolve tail re-points a stale
    /// `wall_id` from this anchor (and seeds the anchor for new/legacy openings).
    /// `None` for roof openings and not-yet-seeded records.
    #[serde(default)]
    pub wall_anchor: Option<WallAnchor>,
    /// Groups the members of a door+sidelights assembly (this window is a
    /// sidelight/transom of the door with the same id). `None` for standalone.
    #[serde(default)]
    pub assembly_id: Option<ItemId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Door {
    pub id: ItemId,
    pub type_name: String,
    /// DERIVED cache (see `Window::vertices`).
    pub vertices: Vec<(f32, f32, f32)>,
    pub photos: Vec<Photo>,
    #[serde(default)]
    pub width_m: f32,
    #[serde(default)]
    pub height_m: f32,
    #[serde(default)]
    pub kind: Option<DoorKind>,
    #[serde(default)]
    pub placement: Option<OpeningPlacement>,
    /// Durable parametric anchor for a wall opening — see [`Window::wall_anchor`].
    #[serde(default)]
    pub wall_anchor: Option<WallAnchor>,
    /// Set on the door panel of a door+sidelights assembly; its sidelight/
    /// transom `Window`s carry the same id.
    #[serde(default)]
    pub assembly_id: Option<ItemId>,
}
