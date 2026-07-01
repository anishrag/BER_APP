//! External per-side ELEVATIONS — the Stage-1 review's view of one building
//! side: every wall (across all storeys) whose outward normal faces the side,
//! stacked by storey height, with its openings, projected onto a vertical view
//! plane. Facing-walls-only: there is NO occlusion, so a concave wing can show a
//! wall a real photo wouldn't — adequate for the convex/rectangular common case.
//!
//! Pure coordinate math over the resolved model (reuses [`WallRegion::base_edge`]
//! / [`base_frame`](WallRegion::base_frame) / [`top_profile`](WallRegion::top_profile)
//! and [`storey_floor_datums`](Ber::storey_floor_datums)); no UI, no I/O.

use crate::ber::ber_struct::Ber;
use crate::ber::common::{ExternalSide, ItemId};
use crate::ber::openings::{wall_openings, OpeningRef};
use domain::points::Point2f64;

impl ExternalSide {
    /// The outward (away-from-building) plan direction this side faces. Plan
    /// convention, verified against the canvas transform (`Transform2d::apply`
    /// has no Y-flip, so "bottom as drawn" is max Y): Front = +Y, Back = −Y,
    /// Right = +X, Left = −X.
    pub fn outward_dir(self) -> (f64, f64) {
        match self {
            ExternalSide::Front => (0.0, 1.0),
            ExternalSide::Back => (0.0, -1.0),
            ExternalSide::Right => (1.0, 0.0),
            ExternalSide::Left => (-1.0, 0.0),
        }
    }

    /// The horizontal view axis `h = (n.y, -n.x)` (unit, ⟂ the outward normal
    /// `n`) onto which [`Ber::side_elevation`] projects plan points to get a
    /// composite elevation x. Its handedness matches a viewer standing OUTSIDE
    /// the side looking IN — the same view a facade PHOTO is taken from, so the
    /// drawn elevation reads the same way round as the photo it's compared to.
    /// (Plan convention: `Transform2d::apply` has no Y-flip, so plan +X is
    /// screen-right, +Y screen-down, +Z up; a viewer outside side `n` looking in
    /// has screen-right `cross(up, -n) = (n.y, -n.x)`.) THE single source of `h`,
    /// shared by the forward projection and the drag inverse
    /// ([`WallRegion::elevation_along_at`]), so the opening drag stays consistent.
    pub fn view_axis(self) -> (f64, f64) {
        let (nx, ny) = self.outward_dir();
        (ny, -nx)
    }
}

/// A window or door in a side elevation, in the composite `(x, z)` elevation
/// frame (metres): `x` is the horizontal position along the side's view plane,
/// `z` is height above the building datum. Carries the durable handle
/// (`storey`, `wall_id`, `opening`) so a drag in the review maps straight back
/// to [`crate::ber::openings::move_opening`].
#[derive(Debug, Clone, Copy)]
pub struct ElevOpening {
    pub storey: usize,
    pub wall_id: ItemId,
    pub opening: OpeningRef,
    pub is_door: bool,
    /// The window is arched (a half-ellipse fanlight) — drawn as an arch, not a
    /// rectangle. Always `false` for doors.
    pub arched: bool,
    /// Composite x of the opening's CENTRE.
    pub x_center: f64,
    pub width_m: f64,
    /// Composite z of the opening's BOTTOM (sill).
    pub sill_z: f64,
    pub height_m: f64,
}

/// One facing wall projected onto the side's view plane: its horizontal span
/// `[x0, x1]`, its base z, and its top edge as an `(x, z)` polyline (supports
/// stepped / sloped tops), all in the composite frame.
#[derive(Debug, Clone)]
pub struct ElevPanel {
    pub x0: f64,
    pub x1: f64,
    pub base_z: f64,
    pub top: Vec<(f64, f64)>,
}

/// The external elevation of one building side — facing walls (stacked by
/// storey) plus their openings, in a common `(x, z)` frame, with the overall
/// bounds for fitting. Empty `panels` means nothing faces that side.
#[derive(Debug, Clone)]
pub struct SideElevation {
    pub side: ExternalSide,
    pub panels: Vec<ElevPanel>,
    pub openings: Vec<ElevOpening>,
    pub min_x: f64,
    pub max_x: f64,
    pub min_z: f64,
    pub max_z: f64,
}

impl SideElevation {
    fn empty(side: ExternalSide) -> Self {
        SideElevation {
            side,
            panels: Vec::new(),
            openings: Vec::new(),
            min_x: 0.0,
            max_x: 0.0,
            min_z: 0.0,
            max_z: 0.0,
        }
    }
}

impl Ber {
    /// Build the external elevation of `side`: every wall whose outward normal
    /// faces the side (dot with the side's normal > [`FACING_MIN`]), across all
    /// storeys stacked by [`storey_floor_datums`](Ber::storey_floor_datums),
    /// projected onto the side's horizontal view axis `h = (-n.y, n.x)` (whose
    /// handedness matches a viewer standing OUTSIDE the side looking in). A
    /// wall's outward normal is the base-edge perpendicular flipped to point
    /// away from the storey's centroid. Facing-walls-only — no occlusion.
    ///
    /// [`FACING_MIN`]: a wall counts as facing the side when its outward normal
    /// is within ~60° of the side's normal.
    pub fn side_elevation(&self, side: ExternalSide) -> SideElevation {
        /// Min dot(outward_normal, side_normal) for a wall to count as facing.
        const FACING_MIN: f64 = 0.5;
        let n = side.outward_dir();
        let h = side.view_axis(); // unit horizontal view axis, ⟂ n
        let datums = self.storey_floor_datums();

        let mut panels: Vec<ElevPanel> = Vec::new();
        let mut openings: Vec<ElevOpening> = Vec::new();
        let (mut min_x, mut max_x) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut min_z, mut max_z) = (f64::INFINITY, f64::NEG_INFINITY);

        for (si, storey) in self.storeys.iter().enumerate() {
            let offset = datums.get(si).copied().unwrap_or(0.0);

            // Storey centroid (mean wall base midpoint) — the side a wall's
            // outward normal must point AWAY from.
            let mids: Vec<Point2f64> = storey
                .walls
                .regions
                .iter()
                .filter_map(|w| w.base_midpoint())
                .collect();
            if mids.is_empty() {
                continue;
            }
            let c = {
                let (sx, sy) = mids
                    .iter()
                    .fold((0.0, 0.0), |(ax, ay), m| (ax + m.x, ay + m.y));
                Point2f64::new(sx / mids.len() as f64, sy / mids.len() as f64)
            };

            for w in &storey.walls.regions {
                let Some((a, b)) = w.base_edge() else {
                    continue;
                };
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let len = (dx * dx + dy * dy).sqrt();
                if len < 1e-9 {
                    continue;
                }
                let (ux, uy) = (dx / len, dy / len);
                // Outward normal: base-edge perpendicular, flipped to point away
                // from the storey centroid.
                let mut o = (-uy, ux);
                let (mx, my) = ((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
                if (mx - c.x) * o.0 + (my - c.y) * o.1 < 0.0 {
                    o = (-o.0, -o.1);
                }
                if o.0 * n.0 + o.1 * n.1 <= FACING_MIN {
                    continue;
                }

                let Some((origin, (fux, fuy), _l, base_z)) = w.base_frame() else {
                    continue;
                };
                let bz = base_z + offset;
                let xa = h.0 * a.x + h.1 * a.y;
                let xb = h.0 * b.x + h.1 * b.y;

                // Top edge, mapped to composite (x, z) via along → plan point.
                let top: Vec<(f64, f64)> = w
                    .top_profile()
                    .iter()
                    .map(|&(along, z)| {
                        let px = origin.x + along as f64 * fux;
                        let py = origin.y + along as f64 * fuy;
                        (h.0 * px + h.1 * py, z as f64 + offset)
                    })
                    .collect();

                min_x = min_x.min(xa.min(xb));
                max_x = max_x.max(xa.max(xb));
                // Fold the base z into BOTH bounds so a panel with an empty top
                // (a degenerate ≤2-vertex wall) still contributes finite bounds.
                min_z = min_z.min(bz);
                max_z = max_z.max(bz);
                for &(tx, tz) in &top {
                    min_x = min_x.min(tx);
                    max_x = max_x.max(tx);
                    min_z = min_z.min(tz);
                    max_z = max_z.max(tz);
                }

                for op in wall_openings(storey, w.id) {
                    let px = origin.x + op.along_m as f64 * fux;
                    let py = origin.y + op.along_m as f64 * fuy;
                    openings.push(ElevOpening {
                        storey: si,
                        wall_id: w.id,
                        opening: op.opening,
                        is_door: op.is_door,
                        arched: op.arched,
                        x_center: h.0 * px + h.1 * py,
                        width_m: op.width_m as f64,
                        sill_z: bz + op.sill_m as f64,
                        height_m: op.height_m as f64,
                    });
                }

                panels.push(ElevPanel {
                    x0: xa.min(xb),
                    x1: xa.max(xb),
                    base_z: bz,
                    top,
                });
            }
        }

        if panels.is_empty() {
            return SideElevation::empty(side);
        }
        SideElevation {
            side,
            panels,
            openings,
            min_x,
            max_x,
            min_z,
            max_z,
        }
    }
}
