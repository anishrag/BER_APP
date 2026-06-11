//! Dormant 3D-volume crate (nothing in the app calls it yet).
//!
//! Floor/roof/wall REGION derivation is owned by `datatypes` + `document` now:
//! `FloorRegions::{partition, with_levels}`, `CeilingRegions::{
//! default_for_footprint, with_z_deltas, with_apexes}`, and
//! `WallRegions::regenerate`, composed by `document::resolve_storey`. The
//! editing wrapper this crate used to carry (region splitting, construction
//! re-pointing, wall generation, region merging, its own construction
//! registry) duplicated those ops and has been deleted.
//!
//! What remains is only what the canonical ops do NOT do, kept for a future
//! solid-3D feature:
//! - [`Gen3D`] — per-storey volume assembly from a resolved `Ber` (the
//!   floors + walls + ceilings a 3D viewer would render).
//! - [`CeilingMesh`] / [`CeilingRegionsManip::insert_vertex`] — earcut
//!   triangulation of ceiling regions and ridge-vertex insertion (a gable
//!   ridge needs per-vertex heights; `with_apexes` only fans).
//! - [`WallRegionsManip::split_wall_region`] — splitting one wall face by a
//!   length/height line (the deferred "diagonal split" of the wall-exposure
//!   design).

mod ceiling;
mod geometry;
mod walls;

pub use crate::ceiling::{
    CeilingMesh, CeilingRegionConversion, CeilingRegionError, CeilingRegionsManip, CeilingTriangle,
    Plane, VertexInsertionError,
};
pub use crate::geometry::{Geometry, SplitLine3D};
pub use crate::walls::WallRegionsManip;

use datatypes::ber::ber_struct::Ber;
use datatypes::ber::structural::{CeilingRegions, FloorRegions, WallRegions};

/// One storey's resolved 3D surfaces, assembled for rendering. Kept as the
/// entry point a future 3D viewer would build its scene from; regions
/// reference constructions by id, resolved against the parent `Ber`'s
/// house-level tables.
pub struct Gen3D {
    pub storey: u32,
    pub floors: FloorRegions,
    pub walls: WallRegions,
    pub ceilings: CeilingRegions,
}

impl Gen3D {
    /// Snapshot storey `storey_idx` of `ber` as a renderable volume. Pure
    /// clone of the resolved geometry — derivation already happened in
    /// `document::resolve_storey`.
    pub fn from_ber_storey(ber: &Ber, storey_idx: usize) -> Self {
        let storey = &ber.storeys[storey_idx];
        Self {
            storey: storey.storey_number as u32,
            floors: storey.floors.clone(),
            walls: storey.walls.clone(),
            ceilings: storey.roofs.clone(),
        }
    }
}
