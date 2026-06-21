//! Durable, geometry-independent references to derived surfaces.
//!
//! Walls / floors / roofs are *derived* every resolve (their region ids are
//! freshly minted), so they can't be durable handles for per-surface data
//! (openings, construction). A [`SurfaceRef`] instead names a surface by the
//! **parametric** entities that generate it — chiefly a wall's stable start
//! [`VertexId`], which survives reflows, drags, orthogonalization and splices.
//! Attribute side-tables key off `SurfaceRef`; the projection resolves a
//! `SurfaceRef` back to a concrete region each rebuild.
//!
//! Only the `Wall` variant exists today (it's all the openings migration
//! needs). `Floor`/`Roof` variants are introduced with their own stages.

use crate::VertexId;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A durable reference to a derived surface, expressed against the parametric
/// model rather than against transient resolved geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum SurfaceRef {
    /// A perimeter wall: the ring-0 edge whose START vertex is `start`. A
    /// structure-phase split of this wall produces sub-walls that still share
    /// this `start`; an attached item's along-fraction selects which sub-span it
    /// falls in at resolve time.
    Wall { start: VertexId },
}

impl SurfaceRef {
    /// Convenience constructor for a perimeter wall reference.
    pub fn wall(start: VertexId) -> Self {
        SurfaceRef::Wall { start }
    }

    /// The start [`VertexId`] when this is a wall reference.
    pub fn wall_start(self) -> Option<VertexId> {
        match self {
            SurfaceRef::Wall { start } => Some(start),
        }
    }
}
