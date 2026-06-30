//! The dwelling ROOF — a structure SEPARATE from the habitable-area ceiling.
//!
//! Distinct from the ceiling (`Storey::roofs` / the roof-point apexes, which
//! shape the heat-loss ceiling at the top of a storey): the roof here is the
//! external structure that sits ABOVE the top storey, on its own plane(s). It is
//! only modelled when it matters for the assessment:
//!
//! - an attic insulated at BOTH the ceiling/attic-floor AND the rafters (so the
//!   roof is a second thermal surface, not the same one as the ceiling), and
//! - solar panels / solar water heaters mounted on the roof (the plane's area,
//!   pitch and orientation drive their contribution).
//!
//! The roof does NOT change the ceiling geometry. It is built area-by-area: the
//! assessor groups top-storey ceiling areas, picks a shape, draws a ridge and
//! gives a pitch; each becomes a [`RoofArea`] here, from which the 3-D roof
//! planes are derived (resolve), eaves at the covered storey's ceiling height.
use serde::{Deserialize, Serialize};

use crate::ber::common::ItemId;
use domain::points::Point2f64;

/// The shape of one roof area's planes. `Flat` keeps the roof level at the eave
/// (no ridge); `Gable` rises to a ridge that reaches the end walls (vertical
/// gable ends); `Hip` rises to a ridge inset from the ends (the ends slope down
/// too).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoofShape {
    Flat,
    Gable,
    Hip,
}

/// One roof area over a group of top-storey ceiling areas: the durable INPUT the
/// assessor drew (shape + pitch + ridge + the covered footprint), from which the
/// 3-D roof planes are derived on resolve. The eave sits at `eave_z` (the covered
/// storey's ceiling height, absolute above its floor datum); the ridge rises from
/// there per `pitch_deg`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoofArea {
    pub id: ItemId,
    /// The storey this roof sits on (its top ceiling) — for the eave height and
    /// the dual-insulation pairing with that storey's ceiling.
    pub storey_idx: usize,
    pub shape: RoofShape,
    pub pitch_deg: f32,
    /// Absolute eave height in metres above the covered storey's floor datum
    /// (= that storey's ceiling height).
    pub eave_z: f64,
    /// The ridge endpoints (plan, world metres); `None` for a flat roof.
    pub ridge: Option<(Point2f64, Point2f64)>,
    /// The grouped ceiling-area rings this roof covers (plan, world metres, no
    /// closing duplicate). The derived planes span their union (plus overhang).
    pub footprint: Vec<Vec<Point2f64>>,
}

/// The dwelling roof: the set of roof areas the assessor drew. Lives on
/// [`Ber`](crate::ber::ber_struct::Ber) — one roof per dwelling, separate from
/// every storey's ceiling.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Roof {
    pub areas: Vec<RoofArea>,
}

impl Roof {
    /// True when no roof area has been drawn yet.
    pub fn is_empty(&self) -> bool {
        self.areas.is_empty()
    }
}
