//! The dwelling ROOF — a structure SEPARATE from the habitable-area ceiling.
//!
//! Distinct from the ceiling (`Storey::ceilings` / the roof-point apexes, which
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
use crate::ber::structural::{CeilingApex, CeilingRegion, CeilingRegions};
use domain::points::{Point2f64, Point3f64};

/// Default eave overhang in metres — the roof oversails the walls by this much
/// (the sloped faces continue past the wall, so the overhang eave dips BELOW the
/// wall-top). Drawing-only; ceiling insulation is still assumed full-coverage.
pub const DEFAULT_OVERHANG_M: f64 = 0.3;

/// Offset a plan ring OUTWARD by `d` metres (a miter offset along each vertex's
/// outward edge-normal bisector). Approximate — for the drawing only. `d <= 0` or
/// a degenerate ring returns the ring unchanged.
fn offset_ring_outward(ring: &[Point2f64], d: f64) -> Vec<Point2f64> {
    let n = ring.len();
    if n < 3 || d <= 0.0 {
        return ring.to_vec();
    }
    // Signed area → orientation (outward normal side depends on it).
    let area2: f64 = (0..n)
        .map(|i| {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            a.x * b.y - b.x * a.y
        })
        .sum();
    let ccw = area2 > 0.0;
    let unit = |dx: f64, dy: f64| {
        let l = (dx * dx + dy * dy).sqrt();
        if l < 1e-12 {
            (0.0, 0.0)
        } else {
            (dx / l, dy / l)
        }
    };
    // Outward normal of edge direction (dx,dy): right of travel for CCW rings.
    let outward = |dx: f64, dy: f64| {
        let (ux, uy) = unit(dx, dy);
        if ccw {
            (uy, -ux)
        } else {
            (-uy, ux)
        }
    };
    (0..n)
        .map(|i| {
            let prev = ring[(i + n - 1) % n];
            let cur = ring[i];
            let next = ring[(i + 1) % n];
            let n_in = outward(cur.x - prev.x, cur.y - prev.y);
            let n_out = outward(next.x - cur.x, next.y - cur.y);
            let (mx, my) = unit(n_in.0 + n_out.0, n_in.1 + n_out.1);
            // Miter length = 1/cos(half-angle); clamp so sharp corners don't spike.
            let cos = (mx * n_out.0 + my * n_out.1).max(0.25);
            let f = (1.0 / cos).min(3.0);
            Point2f64::new(cur.x + d * f * mx, cur.y + d * f * my)
        })
        .collect()
}

/// The shape of one roof area's planes. `Flat` keeps the roof level at the eave
/// (no ridge); `Gable` rises to a ridge that reaches the end walls (vertical
/// gable ends); `Hip` rises to a ridge inset from the ends (the ends slope down
/// too); `FollowCeiling` adopts the covered storey's already-sloped CEILING (a
/// converted attic / room-in-roof: the rafters ARE the ceiling, one surface —
/// so the roof reuses the ceiling's apexes instead of a freshly-drawn pitch).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RoofShape {
    Flat,
    Gable,
    Hip,
    FollowCeiling,
}

/// One roof area over a group of top-storey ceiling areas: the durable INPUT the
/// assessor drew (shape + the covered footprint + the apexes that shape it), from
/// which the 3-D roof planes are derived. The eave sits at `eave_z` (the covered
/// storey's ceiling height, absolute above its floor datum). The `apexes` are the
/// geometry driver — the same apex/ridge fan the ceiling uses — populated at
/// commit from the drawn ridge + pitch (`Gable`/`Hip`), the covered storey's
/// ceiling apexes (`FollowCeiling`), or empty (`Flat`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RoofArea {
    pub id: ItemId,
    /// The BASE storey this roof's eave sits on — `eave_z` and the derived
    /// planes' z are local to ITS floor datum. For a roof spanning several
    /// storeys (an attic conversion plus the surrounding ceilings of the
    /// storey below) this is the lowest covered storey; the full covered set
    /// is [`covered_storeys`](Self::covered_storeys).
    pub storey_idx: usize,
    /// Every storey whose ceiling regions this roof covers (an attic-spanning
    /// roof covers the attic AND the storey below). Empty in records written
    /// before multi-storey areas existed — read via
    /// [`covers_storey`](Self::covers_storey), which treats empty as
    /// `[storey_idx]`.
    #[serde(default)]
    pub covered_storeys: Vec<usize>,
    pub shape: RoofShape,
    /// The drawn pitch in degrees (`Gable`/`Hip`); 0 for `Flat`/`FollowCeiling`.
    /// Descriptive — the geometry is driven by `apexes`.
    pub pitch_deg: f32,
    /// Absolute eave height in metres above the covered storey's floor datum
    /// (= that storey's ceiling height).
    pub eave_z: f64,
    /// The ridge endpoints (plan, world metres); `None` for flat / follow-ceiling.
    /// Descriptive — the geometry is driven by `apexes`.
    pub ridge: Option<(Point2f64, Point2f64)>,
    /// The grouped ceiling-area rings this roof covers (plan, world metres, no
    /// closing duplicate). The derived planes span their union (plus overhang).
    pub footprint: Vec<Vec<Point2f64>>,
    /// The apexes that shape the roof over `footprint` — the SAME apex/ridge fan
    /// the ceiling uses. Empty = flat at the eave.
    #[serde(default)]
    pub apexes: Vec<CeilingApex>,
}

impl RoofArea {
    /// True when this roof covers ceiling regions on `storey_idx`. Legacy
    /// records (empty `covered_storeys`) cover exactly their base storey.
    pub fn covers_storey(&self, storey_idx: usize) -> bool {
        if self.covered_storeys.is_empty() {
            self.storey_idx == storey_idx
        } else {
            self.covered_storeys.contains(&storey_idx)
        }
    }

    /// The bounding box (plan, world metres) of the given footprint rings, or
    /// `None` if degenerate.
    fn footprint_bbox(footprint: &[Vec<Point2f64>]) -> Option<(f64, f64, f64, f64)> {
        let mut bb: Option<(f64, f64, f64, f64)> = None;
        for ring in footprint {
            for p in ring {
                bb = Some(match bb {
                    None => (p.x, p.y, p.x, p.y),
                    Some((x0, y0, x1, y1)) => (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y)),
                });
            }
        }
        bb.filter(|(x0, y0, x1, y1)| x1 - x0 > 1e-6 && y1 - y0 > 1e-6)
    }

    /// The two apexes a drawn ridge + pitch imply — the ridge endpoints raised
    /// to `eave_z + run · tan(pitch)`. The run is LOCAL: the larger of the two
    /// [`ridge_perp_reach`] distances from the ridge to the footprint boundary
    /// along its perpendicular — so a centred ridge in a rectangle keeps the
    /// old half-span behaviour, a ridge ON an edge (mono-pitch) rises over the
    /// FULL span to the opposite eave, and a ridge centred in one wing of an
    /// L reads that wing's width, not the whole bounding box. Falls back to
    /// the bbox half-span when the perpendicular finds no boundary (degenerate
    /// footprint). Feeds the ceiling's [`with_apexes`] fan. Empty if the
    /// footprint is degenerate. Used at commit to populate a Gable/Hip roof
    /// area's [`apexes`](Self::apexes) — once per drawn ridge (a multi-ridge
    /// hip concatenates each ridge's pair).
    pub fn ridge_apexes(
        ridge: (Point2f64, Point2f64),
        pitch_deg: f32,
        eave_z: f64,
        footprint: &[Vec<Point2f64>],
    ) -> Vec<CeilingApex> {
        let (a, b) = ridge;
        let Some((x0, y0, x1, y1)) = Self::footprint_bbox(footprint) else {
            return Vec::new();
        };
        let run = match ridge_perp_reach(ridge, footprint) {
            Some((d_neg, d_pos)) if d_neg.max(d_pos) > 1e-6 => d_neg.max(d_pos),
            _ => {
                let ridge_along_y = (a.x - b.x).abs() <= (a.y - b.y).abs();
                if ridge_along_y {
                    (x1 - x0) / 2.0
                } else {
                    (y1 - y0) / 2.0
                }
            }
        };
        let z = eave_z + run * (pitch_deg as f64).to_radians().tan();
        vec![
            CeilingApex { point: a, z_m: z },
            CeilingApex { point: b, z_m: z },
        ]
    }

    /// The effective slope (rise/run) of the roof faces — the apex rise over half
    /// the footprint's shorter span (the cross-slope run). 0 when flat / no apexes.
    fn effective_slope(&self) -> f64 {
        let apex_max = self
            .apexes
            .iter()
            .map(|a| a.z_m)
            .fold(self.eave_z, f64::max);
        match Self::footprint_bbox(&self.footprint) {
            Some((x0, y0, x1, y1)) => {
                let perp_half = ((x1 - x0).min(y1 - y0)) / 2.0;
                if perp_half > 1e-6 {
                    (apex_max - self.eave_z) / perp_half
                } else {
                    0.0
                }
            }
            None => 0.0,
        }
    }

    /// The roof as `CeilingRegions` — built with the SAME machinery as the ceiling
    /// ([`CeilingRegions::default_for_footprint`] over the grouped footprint, then
    /// [`with_apexes`](CeilingRegions::with_apexes) with this area's apexes). The
    /// footprint rings ARE the covered ceiling regions' rings, UNIONED first
    /// ([`merge_footprint_rings`]) so a roof grouped over several adjacent
    /// ceiling areas is ONE surface — its ridge fans across the whole grouped
    /// area, not per-piece (each apex would otherwise only shape the piece it
    /// lands in). A `FollowCeiling` roof reproduces the sloped ceiling exactly
    /// (rafters = ceiling, one surface). The z is LOCAL to the covered
    /// storey's floor datum.
    ///
    /// `overhang_m` oversails the walls: the footprint is offset outward by that
    /// much and the fan's base is LOWERED by `overhang · slope`, so each sloped
    /// face passes through the wall-top at the wall and CONTINUES down past it —
    /// the overhang eave sits below the wall-top (it may fall within the storey
    /// for a thick-insulation attic; that's fine, it's the drawing only). A FLAT
    /// area (no apexes) gets NO overhang at all — a flat roof IS its ceiling's
    /// upper face, and a level lip past the walls would draw structure that
    /// isn't there.
    pub fn regions(&self, overhang_m: f64, construction_id: ItemId) -> CeilingRegions {
        let merged = merge_footprint_rings(&self.footprint);
        if overhang_m <= 0.0 || self.apexes.is_empty() {
            return CeilingRegions::default_for_footprint(merged, self.eave_z, construction_id)
                .with_apexes(&self.apexes);
        }
        let footprint: Vec<Vec<Point2f64>> = merged
            .iter()
            .map(|ring| offset_ring_outward(ring, overhang_m))
            .collect();
        // Lower the offset base so the plane continues the slope through the wall.
        let base_z = self.eave_z - overhang_m * self.effective_slope();
        CeilingRegions::default_for_footprint(footprint, base_z, construction_id)
            .with_apexes(&self.apexes)
    }

    /// True when this roof area COINCIDES with the ceiling it covers — the roof
    /// surface adds no geometry over the ceiling already there (a flat roof on
    /// a flat ceiling at the eave, or a `FollowCeiling` roof, where the rafters
    /// ARE the ceiling). The 3D view uses this to suppress the roof wireframe:
    /// roof lines only carry information where the roof DEPARTS from the
    /// ceiling (a pitched roof over a flat ceiling).
    ///
    /// `FollowCeiling` is true by definition; other shapes get a geometric
    /// two-way check between the UN-OVERHUNG roof surface (`regions(0.0, …)` —
    /// the overhang lip is drawing-only) and the covered ceiling regions (those
    /// whose plan centroid falls inside the footprint): every vertex of each
    /// surface must lie ON the other surface (within [`COINCIDE_EPS_M`]). Both
    /// directions are needed — a gable's eave corners lie on a flat ceiling
    /// (ridge vertices catch it), and a flat roof's corners lie on a raised
    /// ceiling's plan (the ceiling's raised vertices catch it). No covered
    /// ceiling (unresolved storey / mismatched grouping) is `false` — when in
    /// doubt, draw the roof.
    pub fn follows_ceiling(&self, ceilings: &CeilingRegions) -> bool {
        if self.shape == RoofShape::FollowCeiling {
            return true;
        }
        use geo::Contains;
        let footprints: Vec<geo::Polygon<f64>> =
            self.footprint.iter().map(|r| ring_polygon(r)).collect();
        let covered: Vec<&CeilingRegion> = ceilings
            .regions
            .iter()
            .filter(|r| {
                let c = r.centroid();
                footprints
                    .iter()
                    .any(|fp| fp.contains(&geo::Point::new(c.x, c.y)))
            })
            .collect();
        let roof = self.regions(0.0, ItemId::default());
        if covered.is_empty() || roof.regions.is_empty() {
            return false;
        }
        let roof_refs: Vec<&CeilingRegion> = roof.regions.iter().collect();
        covered
            .iter()
            .all(|r| r.polygon.exterior.iter().all(|v| on_surface(&roof_refs, v)))
            && roof
                .regions
                .iter()
                .all(|r| r.polygon.exterior.iter().all(|v| on_surface(&covered, v)))
    }
}

/// UNION the grouped footprint rings into contiguous plan pieces: adjacent
/// rings (a roof grouped over several ceiling areas) merge into ONE simple
/// exterior ring; disjoint rings stay separate. Holes a union could
/// theoretically produce are decomposed away (`split_to_simple` — `Polygon3D`
/// is exterior-only and would fill them). Rings come back OPEN (no closing
/// duplicate), matching the footprint convention. Degenerate rings (< 3
/// vertices) are dropped.
///
/// This is what makes a grouped roof area ONE surface: the ridge fan runs
/// over the merged ring, so an apex shapes the whole grouped area rather than
/// just the piece it happens to land in.
pub fn merge_footprint_rings(rings: &[Vec<Point2f64>]) -> Vec<Vec<Point2f64>> {
    use geo::bool_ops::BooleanOps;
    let mut acc: Option<geo::MultiPolygon<f64>> = None;
    for ring in rings {
        if ring.len() < 3 {
            continue;
        }
        let mp = geo::MultiPolygon(vec![ring_polygon(ring)]);
        acc = Some(match acc {
            None => mp,
            Some(so_far) => so_far.union(&mp),
        });
    }
    let Some(merged) = acc else {
        return Vec::new();
    };
    merged
        .0
        .iter()
        .flat_map(crate::ber::structural::split_to_simple)
        .map(|p| {
            let mut ring: Vec<Point2f64> = p
                .exterior()
                .coords()
                .map(|c| Point2f64::new(c.x, c.y))
                .collect();
            if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
                ring.pop();
            }
            ring
        })
        .filter(|ring| ring.len() >= 3)
        .collect()
}

/// The perpendicular reach of a ridge within its footprint: cast the line
/// PERPENDICULAR to the ridge through its midpoint and return the distance to
/// the nearest footprint-boundary crossing on each side, `(d_neg, d_pos)` in
/// metres (0.0 for a side with no crossing — the midpoint sits on/outside the
/// boundary there). `None` when the ridge is degenerate (< 1 mm long) or NO
/// side crosses. This is the LOCAL span the ridge serves — for a ridge centred
/// in one wing of an L it reads that wing's width, where the bounding box
/// would read the whole plan.
pub fn ridge_perp_reach(
    ridge: (Point2f64, Point2f64),
    footprint: &[Vec<Point2f64>],
) -> Option<(f64, f64)> {
    let (a, b) = ridge;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return None;
    }
    // Unit perpendicular; the midpoint is the sample point.
    let (nx, ny) = (-dy / len, dx / len);
    let (mx, my) = ((a.x + b.x) / 2.0, (a.y + b.y) / 2.0);
    // Crossings at |t| ≤ 1e-6 are the ridge itself sitting on the boundary
    // (an edge-snapped ridge) — not a reach on either side.
    let mut d_neg: Option<f64> = None; // nearest crossing with t < 0, as |t|
    let mut d_pos: Option<f64> = None; // nearest crossing with t > 0
    for ring in footprint {
        let n = ring.len();
        if n < 3 {
            continue;
        }
        for i in 0..n {
            let p = ring[i];
            let q = ring[(i + 1) % n];
            let (ex, ey) = (q.x - p.x, q.y - p.y);
            // mid + t·n = p + s·e  →  cross both sides with e and n.
            let denom = nx * ey - ny * ex;
            if denom.abs() < 1e-12 {
                continue; // edge parallel to the perpendicular
            }
            let (wx, wy) = (p.x - mx, p.y - my);
            let t = (wx * ey - wy * ex) / denom;
            let s = (wx * ny - wy * nx) / denom;
            if !(-1e-9..=1.0 + 1e-9).contains(&s) {
                continue;
            }
            if t > 1e-6 {
                d_pos = Some(d_pos.map_or(t, |d: f64| d.min(t)));
            } else if t < -1e-6 {
                d_neg = Some(d_neg.map_or(-t, |d: f64| d.min(-t)));
            }
        }
    }
    if d_neg.is_none() && d_pos.is_none() {
        return None;
    }
    Some((d_neg.unwrap_or(0.0), d_pos.unwrap_or(0.0)))
}

/// Snap a drawn ridge ONTO the footprint edge it nearly sits on: when the
/// nearest boundary crossing along the ridge's perpendicular is within
/// `tol_m` on one side, shift the whole ridge (both endpoints) perpendicular
/// by that distance so it lies exactly on the boundary — a roughly-drawn
/// eave-line becomes a true mono-pitch. The NEARER side wins when both are in
/// tolerance. Unchanged when nothing is within `tol_m` or the ridge is
/// degenerate.
pub fn snap_ridge_to_edge(
    ridge: (Point2f64, Point2f64),
    footprint: &[Vec<Point2f64>],
    tol_m: f64,
) -> (Point2f64, Point2f64) {
    let Some((d_neg, d_pos)) = ridge_perp_reach(ridge, footprint) else {
        return ridge;
    };
    let near_neg = d_neg > 0.0 && d_neg <= tol_m;
    let near_pos = d_pos > 0.0 && d_pos <= tol_m;
    let shift = if near_neg && (!near_pos || d_neg <= d_pos) {
        -d_neg
    } else if near_pos {
        d_pos
    } else {
        return ridge;
    };
    shift_ridge_perp(ridge, shift)
}

/// Re-centre a drawn ridge between the footprint boundaries it spans: shift it
/// along its perpendicular so the reach is equal on both sides (the "centre on
/// the roof" option after a rough gable ridge). Unchanged when degenerate.
pub fn centre_ridge(
    ridge: (Point2f64, Point2f64),
    footprint: &[Vec<Point2f64>],
) -> (Point2f64, Point2f64) {
    let Some((d_neg, d_pos)) = ridge_perp_reach(ridge, footprint) else {
        return ridge;
    };
    shift_ridge_perp(ridge, (d_pos - d_neg) / 2.0)
}

/// How far (degrees) a drawn hip ridge may lean off an axis and still be
/// treated as MEANT orthogonal. Beyond this it's a deliberate diagonal (a
/// hip's corner ridge) and keeps its drawn direction.
pub const RIDGE_ORTHO_SNAP_DEG: f64 = 15.0;

/// Tidy a drawn HIP ridge:
///
/// - Drawn within [`RIDGE_ORTHO_SNAP_DEG`] of an axis ⇒ meant orthogonal —
///   straighten it (both endpoints onto the mean coordinate) and snap it onto
///   a footprint edge it nearly sits on ([`snap_ridge_to_edge`], `tol_m`).
/// - Drawn clearly DIAGONAL ⇒ a hip corner ridge — keep the direction as
///   drawn and instead snap each ENDPOINT (in plan) onto the nearest footprint
///   corner or `extra_targets` point (an already-drawn ridge's endpoint, so
///   junctions meet exactly) within `tol_m`.
pub fn snap_hip_ridge(
    ridge: (Point2f64, Point2f64),
    footprint: &[Vec<Point2f64>],
    extra_targets: &[Point2f64],
    tol_m: f64,
) -> (Point2f64, Point2f64) {
    let (a, b) = ridge;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 {
        return ridge;
    }
    // Lean off the NEAREST axis: 0° = exactly orthogonal, 45° = pure diagonal.
    let lean = {
        let ang = dy.atan2(dx).abs().to_degrees(); // 0..180 vs +x
        let from_h = ang.min(180.0 - ang);
        let from_v = (90.0 - ang).abs();
        from_h.min(from_v)
    };
    if lean <= RIDGE_ORTHO_SNAP_DEG {
        let straight = if dx.abs() >= dy.abs() {
            let y = (a.y + b.y) / 2.0;
            (Point2f64::new(a.x, y), Point2f64::new(b.x, y))
        } else {
            let x = (a.x + b.x) / 2.0;
            (Point2f64::new(x, a.y), Point2f64::new(x, b.y))
        };
        return snap_ridge_to_edge(straight, footprint, tol_m);
    }
    let targets: Vec<Point2f64> = footprint
        .iter()
        .flatten()
        .chain(extra_targets)
        .copied()
        .collect();
    let snap = |p: Point2f64| {
        targets
            .iter()
            .map(|t| {
                let (ex, ey) = (t.x - p.x, t.y - p.y);
                ((ex * ex + ey * ey).sqrt(), t)
            })
            .filter(|(d, _)| *d <= tol_m)
            .min_by(|(d1, _), (d2, _)| d1.total_cmp(d2))
            .map(|(_, t)| *t)
            .unwrap_or(p)
    };
    (snap(a), snap(b))
}

/// Translate both ridge endpoints by `shift` metres along the ridge's unit
/// perpendicular (the same perpendicular [`ridge_perp_reach`] measures along).
fn shift_ridge_perp(ridge: (Point2f64, Point2f64), shift: f64) -> (Point2f64, Point2f64) {
    let (a, b) = ridge;
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    if len < 1e-3 || shift.abs() < 1e-9 {
        return ridge;
    }
    let (nx, ny) = (-dy / len, dx / len);
    (
        Point2f64::new(a.x + shift * nx, a.y + shift * ny),
        Point2f64::new(b.x + shift * nx, b.y + shift * ny),
    )
}

/// z tolerance (m) for [`RoofArea::follows_ceiling`]'s vertex-on-surface test —
/// well above boolean-op noise (~1e-7 m), well below any step an assessor
/// would draw.
const COINCIDE_EPS_M: f64 = 1e-3;

/// `v` lies on the piecewise-planar surface `regions` describes: some region
/// plan-contains its (x, y) (boundary included — ring vertices sit exactly on
/// region edges) and that region's plane z there matches `v.z` within
/// [`COINCIDE_EPS_M`]. A vertex over no region (or over a vertical/degenerate
/// one) is NOT on the surface.
fn on_surface(regions: &[&CeilingRegion], v: &Point3f64) -> bool {
    let p = Point2f64::new(v.x, v.y);
    regions
        .iter()
        .filter(|r| r.plan_contains_within(p, 1e-4))
        .any(|r| {
            r.polygon
                .plane_z(v.x, v.y)
                .is_some_and(|z| (z - v.z).abs() <= COINCIDE_EPS_M)
        })
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

    /// True when plan point `p` lies inside some roof area on `storey_idx` — i.e.
    /// a roof is assigned over that spot. Deliberately LENIENT (plan containment
    /// of the point): whether the roof's eave drops short or overhangs doesn't
    /// matter — only that something covers the ceiling area there.
    pub fn covers_on(&self, storey_idx: usize, p: Point2f64) -> bool {
        use geo::Contains;
        let pt = geo::Point::new(p.x, p.y);
        self.areas
            .iter()
            .filter(|a| a.covers_storey(storey_idx))
            .flat_map(|a| a.footprint.iter())
            .any(|ring| ring_polygon(ring).contains(&pt))
    }
}

/// A plan ring → a `geo` polygon (no holes) for point-in-polygon tests.
fn ring_polygon(ring: &[Point2f64]) -> geo::Polygon<f64> {
    let coords: Vec<geo::Coord<f64>> = ring
        .iter()
        .map(|p| geo::coord! { x: p.x, y: p.y })
        .collect();
    geo::Polygon::new(geo::LineString::new(coords), vec![])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn square_10() -> Vec<Vec<Point2f64>> {
        vec![vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ]]
    }

    /// A multi-storey area (attic + the storey below) covers ceiling regions on
    /// EVERY storey in `covered_storeys`; a legacy area (empty set) covers
    /// exactly its base storey.
    #[test]
    fn covers_on_credits_every_covered_storey() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: vec![0, 1],
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        let roof = Roof { areas: vec![area] };
        let p = Point2f64::new(5.0, 5.0);
        assert!(roof.covers_on(0, p), "covers its base storey");
        assert!(roof.covers_on(1, p), "covers the attic storey too");
        assert!(!roof.covers_on(2, p), "not a storey outside the set");
    }

    /// A RoofArea JSON written before `covered_storeys` existed deserializes
    /// with the empty set — and `covers_storey` then reads it as covering
    /// exactly its base storey.
    #[test]
    fn roof_area_json_without_covered_storeys_deserializes() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 1,
            covered_storeys: vec![0, 1],
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        // JSON produced by the OLD shape: serialize and strip the field.
        let mut json = serde_json::to_value(&area).expect("serialize");
        assert!(
            json.as_object_mut()
                .expect("object")
                .remove("covered_storeys")
                .is_some(),
            "field present in the current shape"
        );
        let legacy: RoofArea = serde_json::from_value(json).expect("legacy JSON loads");
        assert!(legacy.covered_storeys.is_empty());
        assert!(
            legacy.covers_storey(1),
            "legacy area covers its base storey"
        );
        assert!(!legacy.covers_storey(0));
    }

    #[test]
    fn covers_on_matches_storey_and_plan_containment() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 1,
            covered_storeys: Vec::new(),
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        let roof = Roof { areas: vec![area] };
        assert!(
            roof.covers_on(1, Point2f64::new(5.0, 5.0)),
            "inside the footprint on the owning storey"
        );
        assert!(
            !roof.covers_on(1, Point2f64::new(15.0, 5.0)),
            "outside the footprint in plan"
        );
        assert!(
            !roof.covers_on(0, Point2f64::new(5.0, 5.0)),
            "a roof on storey 1 doesn't cover storey 0"
        );
    }

    fn z_extent(regions: &CeilingRegions) -> (f64, f64) {
        let zs: Vec<f64> = regions
            .regions
            .iter()
            .flat_map(|r| r.polygon.exterior.iter().map(|p| p.z))
            .collect();
        (
            zs.iter().cloned().fold(f64::MAX, f64::min),
            zs.iter().cloned().fold(f64::MIN, f64::max),
        )
    }

    #[test]
    fn gable_regions_rise_from_eave_to_ridge_via_the_ceiling_fan() {
        let ridge = (Point2f64::new(5.0, 0.0), Point2f64::new(5.0, 10.0));
        // Apex height: eave 2.4 + half-span 5 · tan(45°) = 7.4.
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &square_10());
        assert_eq!(apexes.len(), 2, "a ridge is two apexes");
        assert!(apexes.iter().all(|a| (a.z_m - 7.4).abs() < 1e-6));
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint: square_10(),
            apexes,
        };
        // Regions are built by the SHARED ceiling fan; z runs eave→ridge.
        let regions = area.regions(0.0, ItemId::default());
        assert!(!regions.regions.is_empty(), "the fan produced roof faces");
        let (zmin, zmax) = z_extent(&regions);
        assert!((zmin - 2.4).abs() < 1e-6, "eave at 2.4, got {zmin}");
        assert!((zmax - 7.4).abs() < 1e-6, "ridge at 7.4, got {zmax}");
    }

    /// A FollowCeiling roof adopts the covered storey's ceiling apexes (converted
    /// attic): its regions REPRODUCE the sloped ceiling — same eave→ridge z range,
    /// same footprint vertices — so the roof coincides with the ceiling.
    #[test]
    fn follow_ceiling_reproduces_the_sloped_ceiling() {
        // The ceiling's own apexes (a ridge at x = 5, height 6.9).
        let ceiling_apexes = vec![
            CeilingApex {
                point: Point2f64::new(5.0, 0.0),
                z_m: 6.9,
            },
            CeilingApex {
                point: Point2f64::new(5.0, 10.0),
                z_m: 6.9,
            },
        ];
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::FollowCeiling,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: ceiling_apexes.clone(),
        };
        let roof = area.regions(0.0, ItemId::default());
        // Same as the ceiling built the same way.
        let ceiling = CeilingRegions::default_for_footprint(square_10(), 2.4, ItemId::default())
            .with_apexes(&ceiling_apexes);
        assert_eq!(
            z_extent(&roof),
            z_extent(&ceiling),
            "roof coincides with ceiling"
        );
        let (zmin, zmax) = z_extent(&roof);
        assert!((zmin - 2.4).abs() < 1e-6 && (zmax - 6.9).abs() < 1e-6);
    }

    #[test]
    fn flat_regions_are_level_at_the_eave_and_share_the_footprint() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.6,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        let regions = area.regions(0.0, ItemId::default());
        // Flat = the footprint regions at the eave (shares the ceiling's vertices).
        assert_eq!(regions.regions.len(), 1);
        let (zmin, zmax) = z_extent(&regions);
        assert!(
            (zmin - 2.6).abs() < 1e-9 && (zmax - 2.6).abs() < 1e-9,
            "level"
        );
        // The plan ring matches the footprint exactly (shared vertices).
        let xy: Vec<(f64, f64)> = regions.regions[0]
            .polygon
            .exterior
            .iter()
            .map(|p| (p.x, p.y))
            .collect();
        for v in &square_10()[0] {
            assert!(
                xy.iter()
                    .any(|(x, y)| (x - v.x).abs() < 1e-9 && (y - v.y).abs() < 1e-9),
                "footprint vertex {v:?} is shared by the flat roof"
            );
        }
    }

    fn min_x(regions: &CeilingRegions) -> f64 {
        regions
            .regions
            .iter()
            .flat_map(|r| r.polygon.exterior.iter().map(|p| p.x))
            .fold(f64::MAX, f64::min)
    }

    /// A 0.3 m overhang on a 45° gable oversails the wall (footprint offset out)
    /// and CONTINUES the slope past it — the overhang eave dips below the wall-top
    /// eave (2.4 − 0.3·tan45 ≈ 2.1), while the ridge height is unchanged.
    #[test]
    fn overhang_continues_the_slope_below_the_wall_top() {
        let ridge = (Point2f64::new(5.0, 0.0), Point2f64::new(5.0, 10.0));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &square_10());
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint: square_10(),
            apexes,
        };
        let over = area.regions(0.3, ItemId::default());
        assert!(
            min_x(&over) < -0.2,
            "footprint offset outward past x=0: {}",
            min_x(&over)
        );
        let (zmin, zmax) = z_extent(&over);
        assert!(
            (zmin - 2.1).abs() < 0.05,
            "overhang eave dips below the 2.4 wall-top to ~2.1: {zmin}"
        );
        assert!((zmax - 7.4).abs() < 0.05, "ridge height unchanged: {zmax}");
    }

    /// The perpendicular reach is LOCAL and split per side: a ridge at x = 3
    /// in the 10-square reads 3 to the left boundary and 7 to the right.
    #[test]
    fn ridge_perp_reach_reads_local_side_distances() {
        let ridge = (Point2f64::new(3.0, 2.0), Point2f64::new(3.0, 8.0));
        let (d_neg, d_pos) = ridge_perp_reach(ridge, &square_10()).unwrap();
        let (lo, hi) = (d_neg.min(d_pos), d_neg.max(d_pos));
        assert!((lo - 3.0).abs() < 1e-9 && (hi - 7.0).abs() < 1e-9);
    }

    /// A ridge ON the footprint edge (mono-pitch) rises over the FULL span to
    /// the opposite eave: z = eave + 10 · tan(45°), not the old bbox half.
    #[test]
    fn edge_ridge_apexes_rise_over_the_full_span() {
        let ridge = (Point2f64::new(0.0, 0.0), Point2f64::new(0.0, 10.0));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &square_10());
        assert_eq!(apexes.len(), 2);
        assert!(
            apexes.iter().all(|a| (a.z_m - 12.4).abs() < 1e-6),
            "full-span run: eave 2.4 + 10·tan45 = 12.4, got {:?}",
            apexes.iter().map(|a| a.z_m).collect::<Vec<_>>()
        );
    }

    /// A ridge centred in ONE WING of an L reads that wing's local width (its
    /// half = 2), not the whole plan's bounding box (which would read 8).
    #[test]
    fn wing_ridge_apexes_use_the_local_wing_width() {
        // An L: a 4-wide vertical wing (x 0..4, y 0..10) plus a horizontal
        // wing (x 4..10, y 0..4).
        let l_shape = vec![vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(4.0, 4.0),
            Point2f64::new(4.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ]];
        // Ridge centred in the vertical wing, in its upper half (clear of the
        // other wing): perpendicular reach is 2 each side.
        let ridge = (Point2f64::new(2.0, 5.0), Point2f64::new(2.0, 9.0));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &l_shape);
        assert!(
            apexes.iter().all(|a| (a.z_m - 4.4).abs() < 1e-6),
            "local run 2: eave 2.4 + 2·tan45 = 4.4, got {:?}",
            apexes.iter().map(|a| a.z_m).collect::<Vec<_>>()
        );
    }

    /// A ridge drawn within tolerance of an edge snaps ONTO it; one drawn
    /// clear of every edge stays put.
    #[test]
    fn ridge_snaps_to_a_near_edge_and_only_a_near_edge() {
        let near = (Point2f64::new(9.7, 2.0), Point2f64::new(9.7, 8.0));
        let (a, b) = snap_ridge_to_edge(near, &square_10(), 0.5);
        assert!(
            (a.x - 10.0).abs() < 1e-9 && (b.x - 10.0).abs() < 1e-9,
            "0.3 m off the x=10 edge snaps onto it: {a:?} {b:?}"
        );
        assert!((a.y - 2.0).abs() < 1e-9, "along-ridge coords untouched");
        let clear = (Point2f64::new(4.0, 2.0), Point2f64::new(4.0, 8.0));
        assert_eq!(
            snap_ridge_to_edge(clear, &square_10(), 0.5),
            clear,
            "4 m from either edge — no snap"
        );
    }

    /// Centring shifts the ridge to equal reach on both sides, moving only
    /// perpendicular to itself.
    #[test]
    fn centre_ridge_balances_the_perpendicular_reach() {
        let off = (Point2f64::new(3.0, 0.0), Point2f64::new(3.0, 10.0));
        let (a, b) = centre_ridge(off, &square_10());
        assert!(
            (a.x - 5.0).abs() < 1e-9 && (b.x - 5.0).abs() < 1e-9,
            "recentred to x = 5: {a:?} {b:?}"
        );
        assert!(
            (a.y - 0.0).abs() < 1e-9 && (b.y - 10.0).abs() < 1e-9,
            "endpoints keep their along-ridge positions"
        );
    }

    /// Two adjacent grouped rings UNION into one footprint ring; a disjoint
    /// ring stays its own piece.
    #[test]
    fn adjacent_footprint_rings_merge_into_one() {
        let left = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0),
            Point2f64::new(5.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let right = vec![
            Point2f64::new(5.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(5.0, 4.0),
        ];
        let apart = vec![
            Point2f64::new(20.0, 0.0),
            Point2f64::new(24.0, 0.0),
            Point2f64::new(24.0, 4.0),
            Point2f64::new(20.0, 4.0),
        ];
        let merged = merge_footprint_rings(&[left.clone(), right.clone()]);
        assert_eq!(merged.len(), 1, "adjacent rings become one: {merged:?}");
        let merged_with_island = merge_footprint_rings(&[left, right, apart]);
        assert_eq!(merged_with_island.len(), 2, "the disjoint ring stays");
    }

    /// A roof grouped over TWO adjacent ceiling areas is ONE surface: a ridge
    /// along the merged footprint's top edge (a mono-pitch against the house)
    /// fans the WHOLE grouped area down to the far eave — z = eave + y·tan45
    /// everywhere — instead of each piece coning to the one apex inside it.
    #[test]
    fn grouped_footprint_fans_as_one_surface() {
        let left = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0),
            Point2f64::new(5.0, 4.0),
            Point2f64::new(0.0, 4.0),
        ];
        let right = vec![
            Point2f64::new(5.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 4.0),
            Point2f64::new(5.0, 4.0),
        ];
        let footprint = vec![left, right];
        // Ridge along the full top edge (y = 4), one endpoint over EACH piece.
        let ridge = (Point2f64::new(0.0, 4.0), Point2f64::new(10.0, 4.0));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &footprint);
        assert!(
            apexes.iter().all(|a| (a.z_m - 6.4).abs() < 1e-6),
            "edge ridge rises over the merged 4 m run: {apexes:?}"
        );
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint,
            apexes,
        };
        let regions = area.regions(0.0, ItemId::default());
        assert!(!regions.regions.is_empty());
        // Every vertex of every face lies on the single mono-pitch PLANE
        // z = 2.4 + y — the far eave (y = 0) at 2.4, the ridge (y = 4) at 6.4,
        // on BOTH grouped pieces.
        for r in &regions.regions {
            for v in &r.polygon.exterior {
                assert!(
                    (v.z - (2.4 + v.y)).abs() < 1e-6,
                    "vertex off the mono-pitch plane: {v:?}"
                );
            }
        }
    }

    /// A hip ridge drawn a few degrees off an axis is MEANT orthogonal: it
    /// straightens (and edge-snaps like a gable's).
    #[test]
    fn near_orthogonal_hip_ridge_straightens() {
        // ~4° off vertical, well inside RIDGE_ORTHO_SNAP_DEG.
        let drawn = (Point2f64::new(4.9, 2.0), Point2f64::new(5.1, 8.0));
        let (a, b) = snap_hip_ridge(drawn, &square_10(), &[], 0.5);
        assert!(
            (a.x - 5.0).abs() < 1e-9 && (b.x - 5.0).abs() < 1e-9,
            "straightened onto x = 5: {a:?} {b:?}"
        );
        assert!((a.y - 2.0).abs() < 1e-9 && (b.y - 8.0).abs() < 1e-9);
    }

    /// A hip ridge drawn clearly DIAGONAL keeps its direction — no
    /// orthogonalizing — and its endpoint near a footprint corner snaps ONTO
    /// that corner; the far endpoint snaps to an already-drawn ridge end.
    #[test]
    fn diagonal_hip_ridge_keeps_direction_and_snaps_to_corner() {
        let prior_end = Point2f64::new(5.0, 5.0);
        // Roughly 45°, from near the (10,10) corner down towards the prior
        // ridge's end at (5,5).
        let drawn = (Point2f64::new(9.8, 9.7), Point2f64::new(5.2, 5.3));
        let (a, b) = snap_hip_ridge(drawn, &square_10(), &[prior_end], 0.5);
        assert!(
            (a.x - 10.0).abs() < 1e-9 && (a.y - 10.0).abs() < 1e-9,
            "corner end snaps onto (10,10): {a:?}"
        );
        assert!(
            (b.x - 5.0).abs() < 1e-9 && (b.y - 5.0).abs() < 1e-9,
            "the other end meets the prior ridge exactly: {b:?}"
        );
    }

    /// A diagonal hip ridge clear of every corner stays exactly as drawn.
    #[test]
    fn diagonal_hip_ridge_clear_of_corners_stays_as_drawn() {
        let drawn = (Point2f64::new(3.0, 3.0), Point2f64::new(7.0, 7.0));
        assert_eq!(snap_hip_ridge(drawn, &square_10(), &[], 0.5), drawn);
    }

    /// A flat roof at the eave over the flat default ceiling COINCIDES with it —
    /// its wireframe would only double the ceiling's lines.
    #[test]
    fn flat_roof_over_a_flat_ceiling_follows_it() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        let ceilings = CeilingRegions::default_for_footprint(square_10(), 2.4, ItemId::default());
        assert!(area.follows_ceiling(&ceilings));
    }

    /// A flat roof over a ceiling with a RAISED STEP departs from it (the roof
    /// plane sits below the raised piece) — the wireframe carries information.
    #[test]
    fn flat_roof_over_a_raised_ceiling_step_departs() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        let half = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(5.0, 0.0),
            Point2f64::new(5.0, 10.0),
            Point2f64::new(0.0, 10.0),
        ];
        let ceilings = CeilingRegions::default_for_footprint(square_10(), 2.4, ItemId::default())
            .with_z_deltas(&[(half, 0.5)]);
        assert!(!area.follows_ceiling(&ceilings));
    }

    /// A gable over a flat ceiling departs — the eave corners lie on the
    /// ceiling, but the ridge vertices don't (the both-directions check).
    #[test]
    fn gable_roof_over_a_flat_ceiling_departs() {
        let ridge = (Point2f64::new(5.0, 0.0), Point2f64::new(5.0, 10.0));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &square_10());
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint: square_10(),
            apexes,
        };
        let ceilings = CeilingRegions::default_for_footprint(square_10(), 2.4, ItemId::default());
        assert!(!area.follows_ceiling(&ceilings));
    }

    /// FollowCeiling follows BY DEFINITION (rafters are the ceiling, one
    /// surface) — no geometry check, even against an empty resolve.
    #[test]
    fn follow_ceiling_roof_follows_by_definition() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::FollowCeiling,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        assert!(area.follows_ceiling(&CeilingRegions::default()));
    }

    /// No covered ceiling (unresolved storey) → NOT following: when in doubt,
    /// draw the roof.
    #[test]
    fn flat_roof_with_no_resolved_ceiling_departs() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.4,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        assert!(!area.follows_ceiling(&CeilingRegions::default()));
    }

    /// A FLAT area ignores the overhang entirely: no plan offset, no z change —
    /// a flat roof coincides with its ceiling's plan and level, so an overhang
    /// lip would draw structure that isn't there.
    #[test]
    fn flat_area_ignores_the_overhang() {
        let area = RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: Vec::new(),
            shape: RoofShape::Flat,
            pitch_deg: 0.0,
            eave_z: 2.6,
            ridge: None,
            footprint: square_10(),
            apexes: Vec::new(),
        };
        let over = area.regions(0.3, ItemId::default());
        assert!(
            (min_x(&over) - 0.0).abs() < 1e-9,
            "footprint NOT offset outward: min_x = {}",
            min_x(&over)
        );
        let (zmin, zmax) = z_extent(&over);
        assert!(
            (zmin - 2.6).abs() < 1e-9 && (zmax - 2.6).abs() < 1e-9,
            "flat stays level at the eave"
        );
        let plain = area.regions(0.0, ItemId::default());
        let rings = |r: &CeilingRegions| -> Vec<Vec<(f64, f64, f64)>> {
            r.regions
                .iter()
                .map(|r| r.polygon.exterior.iter().map(|v| (v.x, v.y, v.z)).collect())
                .collect()
        };
        assert_eq!(
            rings(&over),
            rings(&plain),
            "overhung and un-overhung flat areas are identical"
        );
    }
}
