//! Derived ROOF-VOID wall exposure: the stateless, post-resolve pass that
//! splits a covered storey's walls where they cross the dwelling roof's
//! surface and stamps the piece UNDER the roof `SemiExposed` — it faces the
//! unheated eaves / roof void, not outside air.
//!
//! The scenario is a multi-storey roof area (an attic conversion whose roof
//! extends over the ceilings of the storey below): a dormer cheek that pokes
//! past the attic's rectangle, or a knee wall inset from the eave, has its
//! lower part inside the roof envelope (semi-exposed to the void) and its
//! upper part in the open air (exposed). The roof plane is exactly the split
//! line, so the split is DERIVED here on every resolve — the durable inputs
//! are the roof area and the walls themselves; no new persistence.
//!
//! Like [`derive_inter_storey`](super::derive_inter_storey), this composes on
//! top of the per-storey resolves. Only the ACTIVE storey is re-resolved
//! between derive tails, so the pass regularly re-runs on its OWN output —
//! it must be (and is) stable there: a re-cut along an existing split
//! collapses to one piece and the stamp re-applies. The stamp is the
//! paintable `SemiExposed`, indistinguishable from assessor paint, so there
//! is NO clear pass here; instead the CONTRACT is that every roof MUTATION
//! (commit / clear in the controller's roof flow) triggers a full
//! re-resolve of the storey stack, which rebuilds each covered storey's
//! walls from durable inputs — stale splits/stamps from a deleted or
//! reshaped roof area cannot survive it. (Retired the v1 known limit.) The
//! pass runs on the storeys a roof area covers ABOVE its base storey only —
//! the base storey's walls sit below their own ceiling, i.e. below the
//! eave, and are never inside the roof void.
//!
//! A wall is considered only when its base midpoint sits strictly INSIDE the
//! roof footprint ([`BOUNDARY_INSET_M`]): a gable-end / eave-line wall ON the
//! footprint boundary carries the roof at its top rather than passing under
//! it, and keeps its painted exposure untouched.

use geo::{Contains, LineString, Point as GeoPoint, Polygon};

use crate::ber::ber_struct::Ber;
use crate::ber::common::ItemId;
use crate::ber::roof::merge_footprint_rings;
use domain::points::{point_to_segment_distance, Point2f64, Point3f64};

use super::constructions::WallExposure;
use super::wall_regions::{split_wall_by_line, WallRegion, MAX_FLOOR_STEP_M};
use super::{CeilingRegions, Polygon3D};

/// How far INSIDE the roof footprint a wall's base midpoint must sit for the
/// roof to pass OVER the wall (metres). A wall on the boundary (gable end,
/// eave line) meets the roof at its top instead and is left alone.
const BOUNDARY_INSET_M: f64 = 0.1;

/// Plan nudge (m) for sampling the roof surface just OFF the wall line — a
/// dormer cheek sits on a roof seam (dormer top one side, slope the other),
/// and the VOID side is the lower of the two samples.
const SIDE_NUDGE_M: f64 = 0.05;

/// z slack (m) for "below the roof surface" — coincident-with-the-roof pieces
/// (a wall top ON the slope) must not read as below it.
const Z_EPS_M: f64 = 1e-3;

/// One covered-storey job, precomputed immutably before the walls are edited:
/// the roof surface (base-storey-local z, dormers composed), the shift onto
/// the covered storey's local frame, and the merged plan footprint.
struct VoidJob {
    storey_idx: usize,
    roof: CeilingRegions,
    /// base-local roof z + `shift` = this-storey-local z.
    shift: f64,
    rings: Vec<Vec<Point2f64>>,
}

/// Split covered storeys' walls by the dwelling roof surface and stamp the
/// piece under the roof `SemiExposed`. Runs across the whole resolved stack —
/// call it with `derive_inter_storey` in the pipeline tail, after every
/// per-storey resolve.
///
/// Only painted-or-plain exposures are overwritten (`Unset` / `Exposed` /
/// `SemiExposed`): `InterStorey` (another derivation's stamp), `NonHeatLoss`
/// and `Ground` are deliberate special labels and are left alone.
pub fn derive_roof_voids(ber: &mut Ber) {
    let datums = ber.storey_floor_datums();
    let mut jobs: Vec<VoidJob> = Vec::new();
    for area in &ber.roof.areas {
        for idx in area.covered_storey_indices() {
            // The base storey's walls end below its own ceiling — the eave —
            // so only storeys the roof spans ONTO can have void-side walls.
            if idx == area.storey_idx || idx >= ber.storeys.len() {
                continue;
            }
            let shift = datums.get(area.storey_idx).copied().unwrap_or(0.0)
                - datums.get(idx).copied().unwrap_or(0.0);
            jobs.push(VoidJob {
                storey_idx: idx,
                roof: area.regions_with_dormers(ber, 0.0, ItemId::default()),
                shift,
                rings: merge_footprint_rings(&area.footprint),
            });
        }
    }
    for job in jobs {
        let storey = &mut ber.storeys[job.storey_idx];
        let walls = std::mem::take(&mut storey.walls.regions);
        let mut out: Vec<WallRegion> = Vec::with_capacity(walls.len());
        for wall in walls {
            split_and_stamp(wall, &job, &mut out);
        }
        storey.walls.regions = out;
    }
}

/// True when `e` may be overwritten by the void stamp.
fn overridable(e: WallExposure) -> bool {
    matches!(
        e,
        WallExposure::Unset | WallExposure::Exposed | WallExposure::SemiExposed
    )
}

/// `p` sits strictly inside one of the merged footprint rings, at least
/// [`BOUNDARY_INSET_M`] off its boundary.
fn strictly_inside(rings: &[Vec<Point2f64>], p: Point2f64) -> bool {
    let gp = GeoPoint::new(p.x, p.y);
    rings.iter().any(|ring| {
        if ring.len() < 3 {
            return false;
        }
        let coords: Vec<(f64, f64)> = ring.iter().map(|v| (v.x, v.y)).collect();
        let poly = Polygon::new(LineString::from(coords), vec![]);
        if !poly.contains(&gp) {
            return false;
        }
        let n = ring.len();
        (0..n).all(|i| {
            let a = ring[i];
            let b = ring[(i + 1) % n];
            point_to_segment_distance(p.x, p.y, a.x, a.y, b.x, b.y) >= BOUNDARY_INSET_M
        })
    })
}

/// The roof surface's z (COVERED-storey-local) governing the VOID side of a
/// wall at plan point `p`: sample just off the wall on both sides
/// (`n` = the wall's plan normal) and take the LOWER — on a roof seam (a
/// dormer cheek: flat top one side, slope the other) the void is under the
/// lower surface. Falls back to the on-line query when both nudges miss.
fn void_side_roof_z(job: &VoidJob, p: Point2f64, n: (f64, f64)) -> Option<f64> {
    let q1 = Point2f64::new(p.x + n.0 * SIDE_NUDGE_M, p.y + n.1 * SIDE_NUDGE_M);
    let q2 = Point2f64::new(p.x - n.0 * SIDE_NUDGE_M, p.y - n.1 * SIDE_NUDGE_M);
    let z = match (job.roof.top_z_at(q1), job.roof.top_z_at(q2)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => job.roof.top_z_at(p),
    };
    z.map(|z| z + job.shift)
}

/// Minimum face area (m²) a split piece must have to be kept: a cut that
/// GRAZES a wall corner or runs along its top edge produces a sliver /
/// zero-area half with duplicated vertices, which would pollute the wall list
/// and crash a LATER pass over it (its plane has no normal). Below this, the
/// cut is treated as not dividing the wall.
const MIN_PIECE_AREA_M2: f64 = 1e-4;

/// The face area (m²) of a planar ring via Newell's method — ~0 for a
/// degenerate ring (duplicated / collinear vertices).
fn face_area(ext: &[Point3f64]) -> f64 {
    if ext.len() < 3 {
        return 0.0;
    }
    let (mut nx, mut ny, mut nz) = (0.0_f64, 0.0_f64, 0.0_f64);
    for i in 0..ext.len() {
        let a = ext[i];
        let b = ext[(i + 1) % ext.len()];
        nx += (a.y - b.y) * (a.z + b.z);
        ny += (a.z - b.z) * (a.x + b.x);
        nz += (a.x - b.x) * (a.y + b.y);
    }
    (nx * nx + ny * ny + nz * nz).sqrt() / 2.0
}

/// Normalise a split piece's ring: a BOTTOM base frame or NO frame, never an
/// inverted one. [`rebase_ring_start`](super::wall_regions::rebase_ring_start)
/// rotates the ring to a valid floor-band frame — right for the horizontal
/// cuts it was built for, but a piece above a DIAGONAL roof cut has no
/// horizontal bottom edge, and its only valid rotation is an upside-down
/// "base" along the TOP, which would let an opening re-home against a frame
/// whose base_z is the piece's top. Keep the raw (frameless) ring instead —
/// a frameless piece simply can't host an opening.
///
/// A frame counts as BOTTOM when its `base_z` sits in the piece's floor band
/// (within [`MAX_FLOOR_STEP_M`](super::wall_regions::MAX_FLOOR_STEP_M) of the
/// ring's minimum z) — a stepped/sloped underside can open a hair above the
/// absolute minimum. Top-edge frames on any real-height piece sit a wall
/// height above the band and stay refused.
fn rebase_piece(piece: WallRegion) -> WallRegion {
    let z_min = piece
        .polygon
        .exterior
        .iter()
        .map(|v| v.z)
        .fold(f64::MAX, f64::min);
    let n = piece.polygon.exterior.len();
    let mut frameless: Option<WallRegion> = None;
    for k in 0..n {
        let mut ring = piece.polygon.exterior.clone();
        ring.rotate_left(k);
        let candidate = WallRegion {
            polygon: Polygon3D { exterior: ring },
            ..piece.clone()
        };
        match candidate.base_frame() {
            // A rotation whose frame sits in the piece's floor band: take it.
            Some((_, _, _, base_z)) if base_z <= z_min + MAX_FLOOR_STEP_M => return candidate,
            // Remember a frameless rotation as the fallback.
            None if frameless.is_none() => frameless = Some(candidate),
            _ => {}
        }
    }
    frameless.unwrap_or(piece)
}

/// Drop consecutive duplicate vertices (and a duplicated closing vertex) a
/// corner-grazing cut can leave on a piece's ring.
fn dedupe_ring(region: &mut WallRegion) {
    let ext = &mut region.polygon.exterior;
    ext.dedup_by(|a, b| {
        (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9 && (a.z - b.z).abs() < 1e-9
    });
    while ext.len() >= 2 {
        let (first, last) = (ext[0], ext[ext.len() - 1]);
        if (first.x - last.x).abs() < 1e-9
            && (first.y - last.y).abs() < 1e-9
            && (first.z - last.z).abs() < 1e-9
        {
            ext.pop();
        } else {
            break;
        }
    }
}

/// Split `wall` by the roof plane over it (when it crosses) and push the
/// piece(s) onto `out`, stamping pieces under the roof `SemiExposed`.
fn split_and_stamp(wall: WallRegion, job: &VoidJob, out: &mut Vec<WallRegion>) {
    let Some((a, b)) = wall.base_edge() else {
        out.push(wall);
        return;
    };
    let (a2, b2) = (Point2f64::new(a.x, a.y), Point2f64::new(b.x, b.y));
    let mid = Point2f64::new((a2.x + b2.x) / 2.0, (a2.y + b2.y) / 2.0);
    let len = ((b2.x - a2.x).powi(2) + (b2.y - a2.y).powi(2)).sqrt();
    if len < 1e-6
        || face_area(&wall.polygon.exterior) < MIN_PIECE_AREA_M2
        || !strictly_inside(&job.rings, mid)
    {
        out.push(wall);
        return;
    }
    // Plan normal of the wall line (unit).
    let n = ((a2.y - b2.y) / len, (b2.x - a2.x) / len);

    // The governing roof face for the SPLIT LINE: the face over the VOID
    // (lower) side at the wall's midpoint. Its plane, evaluated along the
    // wall's base line, is the cut — split_wall_by_line cuts by the infinite
    // line, which is exact whenever one face spans the wall (the normal
    // case; a wall crossing SEVERAL roof faces is cut by the dominant one).
    let low_mid = {
        let q1 = Point2f64::new(mid.x + n.0 * SIDE_NUDGE_M, mid.y + n.1 * SIDE_NUDGE_M);
        let q2 = Point2f64::new(mid.x - n.0 * SIDE_NUDGE_M, mid.y - n.1 * SIDE_NUDGE_M);
        match (job.roof.top_z_at(q1), job.roof.top_z_at(q2)) {
            (Some(z1), Some(z2)) => {
                if z1 <= z2 {
                    q1
                } else {
                    q2
                }
            }
            (Some(_), None) => q1,
            (None, Some(_)) => q2,
            (None, None) => mid,
        }
    };
    let gp = GeoPoint::new(low_mid.x, low_mid.y);
    let face = job.roof.regions.iter().find(|r| {
        use geo::Intersects;
        Polygon::<f64>::from(r.polygon.clone()).intersects(&gp)
    });
    let pieces = match face.and_then(|f| {
        Some((
            f.polygon.plane_z(a2.x, a2.y)?,
            f.polygon.plane_z(b2.x, b2.y)?,
        ))
    }) {
        Some((za, zb)) => {
            let p0 = Point3f64::new(a2.x, a2.y, za + job.shift);
            let p1 = Point3f64::new(b2.x, b2.y, zb + job.shift);
            // Clean each half (a corner-grazing cut leaves duplicated
            // vertices), drop slivers, and rebase each piece's ring so it's a
            // first-class wall where possible (the same normalisation the
            // inter-storey derivation applies to its horizontal-cut outputs).
            // A cut that doesn't produce TWO real pieces didn't divide the
            // wall — keep it whole.
            let halves: Vec<WallRegion> = split_wall_by_line(&wall, p0, p1)
                .into_iter()
                .map(|mut piece| {
                    dedupe_ring(&mut piece);
                    piece
                })
                .filter(|piece| {
                    piece.polygon.exterior.len() >= 3
                        && face_area(&piece.polygon.exterior) >= MIN_PIECE_AREA_M2
                })
                .map(rebase_piece)
                .collect();
            if halves.len() < 2 {
                vec![wall]
            } else {
                halves
            }
        }
        None => vec![wall],
    };
    for mut piece in pieces {
        if overridable(piece.exposure) && piece_is_under_the_roof(&piece, job, n) {
            piece.exposure = WallExposure::SemiExposed;
        }
        out.push(piece);
    }
}

/// The piece's centroid lies BELOW the void-side roof surface (with
/// [`Z_EPS_M`] slack, so a piece bounded by the roof plane itself doesn't
/// flip on rounding).
fn piece_is_under_the_roof(piece: &WallRegion, job: &VoidJob, n: (f64, f64)) -> bool {
    let ext = &piece.polygon.exterior;
    if ext.is_empty() {
        return false;
    }
    let k = ext.len() as f64;
    let (cx, cy, cz) = ext.iter().fold((0.0, 0.0, 0.0), |(x, y, z), v| {
        (x + v.x / k, y + v.y / k, z + v.z / k)
    });
    void_side_roof_z(job, Point2f64::new(cx, cy), n).is_some_and(|roof_z| cz < roof_z - Z_EPS_M)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::roof::{RoofArea, RoofShape};
    use crate::ber::structural::{Opes, Polygon3D, Storey};

    fn p2(x: f64, y: f64) -> Point2f64 {
        Point2f64::new(x, y)
    }

    /// Two storeys under one 45° gable (ridge x = 5) spanning the full 10×10:
    /// base storey 0 (ceiling 2.4 — the eave), attic storey 1 above it. The
    /// west slope in ATTIC-local z is exactly `z = x` (base-local `2.4 + x`,
    /// attic datum 2.4).
    fn gabled_two_storey_ber() -> Ber {
        let mut ber = Ber::default();
        let mut lower = Storey::default();
        lower.ceiling_height_m = Some(2.4);
        ber.storeys.push(lower);
        let mut attic = Storey::default();
        attic.ceiling_height_m = Some(2.2);
        ber.storeys.push(attic);
        let footprint = vec![vec![
            p2(0.0, 0.0),
            p2(10.0, 0.0),
            p2(10.0, 10.0),
            p2(0.0, 10.0),
        ]];
        let ridge = (p2(5.0, 0.0), p2(5.0, 10.0));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &footprint);
        ber.roof.areas.push(RoofArea {
            id: ItemId::default(),
            storey_idx: 0,
            covered_storeys: vec![0, 1],
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint,
            apexes,
            ends: None,
        });
        ber
    }

    /// A vertical wall over base segment a→b spanning z0..z1 (storey-local).
    fn wall(a: Point2f64, b: Point2f64, z0: f64, z1: f64, exposure: WallExposure) -> WallRegion {
        WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(a.x, a.y, z0),
                Point3f64::new(b.x, b.y, z0),
                Point3f64::new(b.x, b.y, z1),
                Point3f64::new(a.x, a.y, z1),
            ]),
            exposure,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        }
    }

    /// A dormer cheek crossing the roof plane is SPLIT by it: the piece under
    /// the slope goes SemiExposed (the eaves void), the piece above keeps its
    /// painted Exposed.
    #[test]
    fn cheek_crossing_the_roof_is_split_below_semi_exposed() {
        let mut ber = gabled_two_storey_ber();
        // Cheek along y = 5, x ∈ [0.5, 2], attic-local z 0..2. The west-slope
        // roof over it is z = x, crossing the face diagonally.
        ber.storeys[1].walls.regions.push(wall(
            p2(0.5, 5.0),
            p2(2.0, 5.0),
            0.0,
            2.0,
            WallExposure::Exposed,
        ));
        derive_roof_voids(&mut ber);

        let walls = &ber.storeys[1].walls.regions;
        assert_eq!(walls.len(), 2, "the cheek was split by the roof plane");
        let semi: Vec<_> = walls
            .iter()
            .filter(|w| w.exposure == WallExposure::SemiExposed)
            .collect();
        let kept: Vec<_> = walls
            .iter()
            .filter(|w| w.exposure == WallExposure::Exposed)
            .collect();
        assert_eq!(semi.len(), 1, "one piece under the roof");
        assert_eq!(kept.len(), 1, "one piece above keeps its paint");
        // The under piece really is under (centroid below z = x), the kept
        // piece above.
        let centroid = |w: &WallRegion| {
            let k = w.polygon.exterior.len() as f64;
            w.polygon
                .exterior
                .iter()
                .fold((0.0, 0.0), |(x, z), v| (x + v.x / k, z + v.z / k))
        };
        let (sx, sz) = centroid(semi[0]);
        assert!(sz < sx, "semi piece under the slope: z {sz} < roof {sx}");
        let (kx, kz) = centroid(kept[0]);
        assert!(kz > kx, "kept piece above the slope: z {kz} > roof {kx}");
    }

    /// A knee wall wholly under the slope (no crossing) is stamped whole; a
    /// wall ON the roof footprint boundary (a gable end) and the BASE
    /// storey's own walls are left untouched.
    #[test]
    fn knee_wall_stamped_whole_boundary_and_base_walls_untouched() {
        let mut ber = gabled_two_storey_ber();
        // Knee wall x ∈ [3, 4] at y = 5, z 0..1 — roof z = x ≥ 3 over all of it.
        ber.storeys[1].walls.regions.push(wall(
            p2(3.0, 5.0),
            p2(4.0, 5.0),
            0.0,
            1.0,
            WallExposure::Unset,
        ));
        // Attic gable end ON the footprint boundary (x = 0).
        ber.storeys[1].walls.regions.push(wall(
            p2(0.0, 2.0),
            p2(0.0, 8.0),
            0.0,
            2.0,
            WallExposure::Exposed,
        ));
        // A base-storey wall inside the footprint — below its own eave.
        ber.storeys[0].walls.regions.push(wall(
            p2(3.0, 5.0),
            p2(7.0, 5.0),
            0.0,
            2.4,
            WallExposure::Exposed,
        ));
        derive_roof_voids(&mut ber);

        let attic = &ber.storeys[1].walls.regions;
        assert_eq!(attic.len(), 2, "no splits — neither wall crosses the roof");
        assert_eq!(
            attic[0].exposure,
            WallExposure::SemiExposed,
            "knee wall wholly in the void"
        );
        assert_eq!(
            attic[1].exposure,
            WallExposure::Exposed,
            "boundary gable end untouched"
        );
        let base = &ber.storeys[0].walls.regions;
        assert_eq!(base.len(), 1);
        assert_eq!(
            base[0].exposure,
            WallExposure::Exposed,
            "base-storey wall untouched"
        );
    }

    /// The cheek cut exits exactly AT the wall's top corner (a corner graze).
    /// The pass must stay stable when re-run on its own output (only the
    /// active storey re-resolves between derive tails): same piece count, no
    /// sliver pieces, no duplicated ring vertices, no panic.
    #[test]
    fn pass_is_stable_and_sliver_free_on_its_own_output() {
        let mut ber = gabled_two_storey_ber();
        // Roof z = x crosses this face from (0.5, z 0.5) and exits exactly at
        // the top corner (2.0, z 2.0).
        ber.storeys[1].walls.regions.push(wall(
            p2(0.5, 5.0),
            p2(2.0, 5.0),
            0.0,
            2.0,
            WallExposure::Exposed,
        ));
        derive_roof_voids(&mut ber);
        derive_roof_voids(&mut ber); // re-run on the pass's own output
        derive_roof_voids(&mut ber);

        let walls = &ber.storeys[1].walls.regions;
        assert_eq!(walls.len(), 2, "stable piece count across re-runs");
        for w in walls.iter() {
            assert!(
                face_area(&w.polygon.exterior) >= MIN_PIECE_AREA_M2,
                "no sliver pieces: area {}",
                face_area(&w.polygon.exterior)
            );
            let ext = &w.polygon.exterior;
            for i in 0..ext.len() {
                let (a, b) = (ext[i], ext[(i + 1) % ext.len()]);
                assert!(
                    (a.x - b.x).abs() > 1e-9
                        || (a.y - b.y).abs() > 1e-9
                        || (a.z - b.z).abs() > 1e-9,
                    "no duplicated ring vertices"
                );
            }
        }
        assert_eq!(
            walls
                .iter()
                .filter(|w| w.exposure == WallExposure::SemiExposed)
                .count(),
            1,
            "stamp stable across re-runs"
        );
    }

    /// A wall whose TOP runs exactly ALONG the roof slope (a cheek fully under
    /// the rafters) is not divided — the graze produces no second real piece —
    /// and is stamped whole.
    #[test]
    fn top_edge_on_the_slope_is_not_divided() {
        let mut ber = gabled_two_storey_ber();
        // Top edge exactly on z = x from (0.5, 0.5) to (2.0, 2.0).
        ber.storeys[1].walls.regions.push(WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.5, 5.0, 0.0),
                Point3f64::new(2.0, 5.0, 0.0),
                Point3f64::new(2.0, 5.0, 2.0),
                Point3f64::new(0.5, 5.0, 0.5),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        });
        derive_roof_voids(&mut ber);

        let walls = &ber.storeys[1].walls.regions;
        assert_eq!(walls.len(), 1, "graze along the top divides nothing");
        assert!(
            face_area(&walls[0].polygon.exterior) > 1.0,
            "the wall kept its full face"
        );
        assert_eq!(
            walls[0].exposure,
            WallExposure::SemiExposed,
            "wholly under the slope → the void stamp"
        );
    }

    /// No emitted piece carries an INVERTED base frame (a "base" along its
    /// top edge, which would garbage an opening's re-homing): each piece
    /// either has its frame at the BOTTOM, or — a piece above a diagonal cut,
    /// which has no horizontal bottom — no frame at all. The piece under the
    /// cut keeps a real bottom frame.
    #[test]
    fn split_pieces_never_carry_an_inverted_base_frame() {
        let mut ber = gabled_two_storey_ber();
        ber.storeys[1].walls.regions.push(wall(
            p2(0.5, 5.0),
            p2(2.0, 5.0),
            0.0,
            2.0,
            WallExposure::Exposed,
        ));
        derive_roof_voids(&mut ber);
        let walls = &ber.storeys[1].walls.regions;
        for w in walls {
            if let Some((_, _, _, base_z)) = w.base_frame() {
                let z_min = w
                    .polygon
                    .exterior
                    .iter()
                    .map(|v| v.z)
                    .fold(f64::MAX, f64::min);
                assert!(
                    (base_z - z_min).abs() < 1e-6,
                    "a frame, if any, sits at the piece's BOTTOM ({base_z} vs {z_min})"
                );
            }
        }
        // The under-roof piece is a first-class wall (bottom frame intact).
        let semi = walls
            .iter()
            .find(|w| w.exposure == WallExposure::SemiExposed)
            .expect("the under-roof piece");
        assert!(
            semi.base_frame()
                .is_some_and(|(_, _, _, base_z)| base_z.abs() < 1e-6),
            "the lower piece keeps its floor-level base frame"
        );
    }
}
