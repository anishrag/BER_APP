//! Screen-point → 3D-surface picking for the orbitable 3D views.
//!
//! Inverts the same orthographic orbit projection the 3D renderers draw with
//! ([`super::project_3d`] + the auto-fit [`super::fit_transform`]): the cursor
//! defines a view ray (orthographic, so a line of constant projected
//! coordinates); every candidate surface plane is intersected with it, hits
//! are kept when the cursor lies inside the surface's PROJECTED polygon and
//! the surface FACES the viewer (outward normal · view direction < 0), and
//! the nearest facing hit along the view direction wins.
//!
//! Candidate surfaces mirror what the renderers actually draw:
//! - single-storey 3D (`current_storey_idx = Some(i)`): that storey's floor /
//!   wall / ceiling regions at datum 0 (z storey-local). No roof planes —
//!   `storey_3d` doesn't draw them.
//! - full-house 3D (`None`): every storey's regions at its floor datum, MINUS
//!   the derived `InterStorey` junction regions (hidden in the stack view),
//!   PLUS the dwelling roof planes (`Ber.roof.areas`, skipping areas that
//!   coincide with the ceiling below, exactly as `full_house_3d` skips them).
//!
//! The renderer's DEFENSIVE fallback (flat rings drawn when a storey's
//! resolved ceilings are missing) is not offered as a pick target — there is
//! no region id to reference; the resolve projection fills the ceilings in
//! normal operation.
//!
//! Pitch convention (drives the objects dialog's "Roof pitch" line):
//! `pitch_deg` is `Some` only for surfaces a roof angle is meaningful for —
//! every ROOF plane (a flat roof reports `Some(0.0)`) and a SLOPED ceiling
//! region (> [`SLOPE_EPS_DEG`] off horizontal). Walls, floors, and flat
//! ceilings report `None` (not 90° / 0°): the dialog shows the line only when
//! a pitch is present. A Gable/Hip roof reports the assessor's drawn
//! `RoofArea::pitch_deg` for its sloped faces (the value they'd quote); flat
//! faces and fan-derived planes (`FollowCeiling`) report the plane's own
//! slope computed from its normal.

use ber_model::ber::ber_struct::Ber;
use ber_model::ber::common::ItemId;
use ber_model::ber::structural::{CeilingExposure, FloorExposure, Polygon3D, Storey, WallExposure};

use crate::{fit_transform, storey_3d, union_bbox, Transform2d, ViewParams};

/// A surface plane's tilt must exceed this (degrees) to count as SLOPED —
/// below it a ceiling is "flat" (no pitch reported).
const SLOPE_EPS_DEG: f64 = 0.5;

/// Which surface a 3D pick landed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickedSurface {
    Wall(ItemId),
    Floor(ItemId),
    Ceiling(ItemId),
    Roof { area_id: ItemId },
}

/// One resolved 3D pick: the surface, the storey it belongs to (a roof plane
/// belongs to the storey it covers), the hit point, and the surface's pitch
/// per the module-level convention.
#[derive(Debug, Clone, Copy)]
pub struct SurfaceHit {
    pub storey_idx: usize,
    pub surface: PickedSurface,
    /// The hit point in the coordinates the view was drawn in: plan (x, y)
    /// world metres; z relative to storey `storey_idx`'s floor datum in the
    /// single-storey view, absolute (datum included) in the full-house view.
    pub world_point: (f64, f64, f64),
    /// The hit's height above storey `storey_idx`'s floor datum (metres) —
    /// the durable per-storey z, identical in both views.
    pub storey_local_z: f64,
    /// Degrees from horizontal — `Some` only for roof planes / sloped ceilings.
    pub pitch_deg: Option<f64>,
}

/// The world→screen transform the 3D view at (`rect`, `current_storey_idx`,
/// `view`) is drawn with — the exact fit `storey_3d::render` /
/// `full_house_3d::render` computes. `None` when that view can't render (no
/// geometry, or a storey without its default ceiling height).
pub fn transform_3d(
    rect: egui::Rect,
    ber: &Ber,
    current_storey_idx: Option<usize>,
    view: ViewParams,
) -> Option<Transform2d> {
    let bb = match current_storey_idx {
        Some(i) => storey_3d::projected_bb_for_storey(ber.storeys.get(i)?, 0.0, view)?,
        None => {
            // Mirror full_house_3d: every storey needs a ceiling height first.
            if ber.storeys.is_empty() || ber.storeys.iter().any(|s| s.ceiling_height_m.is_none()) {
                return None;
            }
            let datums = ber.storey_floor_datums();
            ber.storeys
                .iter()
                .zip(&datums)
                .fold(None, |acc, (storey, &z)| {
                    union_bbox(acc, storey_3d::projected_bb_for_storey(storey, z, view))
                })?
        }
    };
    Some(fit_transform(rect, bb.0, bb.1, view))
}

/// Pick the closest viewer-facing surface under `screen` in the 3D view drawn
/// at (`rect`, `current_storey_idx`, `view`). See the module docs for the
/// candidate set and the pitch convention.
/// Every surface whose projected polygon contains the cursor, with its view
/// depth and whether it FACES the viewer — the shared core behind the
/// pickers.
fn collect_hits(
    rect: egui::Rect,
    ber: &Ber,
    current_storey_idx: Option<usize>,
    view: ViewParams,
    screen: egui::Pos2,
    slop_px: f32,
) -> Vec<(f64, bool, SurfaceHit)> {
    let Some(t) = transform_3d(rect, ber, current_storey_idx, view) else {
        return Vec::new();
    };
    // Click slop in projected-world units: lets a cursor land on thin or
    // nearly edge-on faces (interior risers) whose projected polygon is a
    // sliver. 0 = exact containment (the tablet's tap).
    let slop_world = if slop_px > 0.0 {
        slop_px as f64 / t.scale().max(1e-9)
    } else {
        0.0
    };
    let cursor = t.inverse(screen.x, screen.y);

    let yaw = (view.yaw_deg as f64).to_radians();
    let pitch = (view.pitch_deg as f64).to_radians();
    // The camera direction (unit, from the viewer INTO the scene): the negative
    // gradient of the projection's dropped depth coordinate
    // h = (x·sinγ + y·cosγ)·cosβ + z·sinβ (larger h = closer to the viewer).
    let dir = [
        -yaw.sin() * pitch.cos(),
        -yaw.cos() * pitch.cos(),
        -pitch.sin(),
    ];
    // A world point on the cursor's view ray (at depth h = 0): un-rotate the
    // projected coords (u, w) with v = w·sinβ, z = −w·cosβ.
    let (u0, w0) = cursor;
    let v0 = w0 * pitch.sin();
    let ray_p0 = [
        u0 * yaw.cos() + v0 * yaw.sin(),
        -u0 * yaw.sin() + v0 * yaw.cos(),
        -w0 * pitch.cos(),
    ];

    let mut hits: Vec<(f64, bool, SurfaceHit)> = Vec::new();
    let mut consider = |storey_idx: usize,
                        datum: f64,
                        surface: PickedSurface,
                        ring: &[(f64, f64, f64)],
                        normal: [f64; 3],
                        pitch_deg: Option<f64>| {
        if ring.len() < 3 {
            return;
        }
        // BOTH facings are collected (a caller may pick through the model);
        // only view-parallel planes have no ray intersection.
        let ndotd = dot(normal, dir);
        if ndotd.abs() <= 1e-9 {
            return;
        }
        let facing = ndotd < 0.0;
        // Containment: the cursor inside the surface's projected polygon.
        let projected: Vec<(f64, f64)> = ring
            .iter()
            .map(|&(x, y, z)| super::project_3d(x, y, z, view.yaw_deg, view.pitch_deg))
            .collect();
        if !ring_contains(&projected, cursor)
            && (slop_world <= 0.0 || ring_distance(&projected, cursor) > slop_world)
        {
            return;
        }
        // Ray ∩ plane; smaller s = closer to the viewer (h = −s along the ray).
        let p0 = ring[0];
        let s = dot(
            normal,
            [p0.0 - ray_p0[0], p0.1 - ray_p0[1], p0.2 - ray_p0[2]],
        ) / ndotd;
        let hit_point = (
            ray_p0[0] + s * dir[0],
            ray_p0[1] + s * dir[1],
            ray_p0[2] + s * dir[2],
        );
        hits.push((
            s,
            facing,
            SurfaceHit {
                storey_idx,
                surface,
                world_point: hit_point,
                storey_local_z: hit_point.2 - datum,
                pitch_deg,
            },
        ));
    };

    // Per-storey regions. In the full-house stack the derived InterStorey
    // junction regions are hidden (`fade_internal`) — mirror that by not
    // offering them as pick targets there.
    let (storeys, datums, stack): (Vec<(usize, &Storey)>, Vec<f64>, bool) = match current_storey_idx
    {
        Some(i) => {
            let Some(st) = ber.storeys.get(i) else {
                return Vec::new();
            };
            (vec![(i, st)], vec![0.0], false)
        }
        None => (
            ber.storeys.iter().enumerate().collect(),
            ber.storey_floor_datums(),
            true,
        ),
    };
    for (slot, &(idx, storey)) in storeys.iter().enumerate() {
        let datum = datums[slot];
        for region in &storey.floors.regions {
            if stack && region.exposure == FloorExposure::InterStorey {
                continue;
            }
            let mut ring: Vec<(f64, f64, f64)> = region
                .polygon
                .exterior()
                .coords()
                .map(|c| (c.x, c.y, datum + region.level_m))
                .collect();
            drop_closing_duplicate(&mut ring);
            consider(
                idx,
                datum,
                PickedSurface::Floor(region.id),
                &ring,
                [0.0, 0.0, 1.0],
                None,
            );
        }
        for wall in &storey.walls.regions {
            if stack && wall.exposure == WallExposure::InterStorey {
                continue;
            }
            let ring: Vec<(f64, f64, f64)> = wall
                .polygon
                .exterior
                .iter()
                .map(|v| (v.x, v.y, datum + v.z))
                .collect();
            let Some(n) = outward_wall_normal(&wall.polygon, storey) else {
                continue;
            };
            consider(idx, datum, PickedSurface::Wall(wall.id), &ring, n, None);
        }
        for ceiling in &storey.ceilings.regions {
            if stack && ceiling.exposure == CeilingExposure::InterStorey {
                continue;
            }
            let ring: Vec<(f64, f64, f64)> = ceiling
                .polygon
                .exterior
                .iter()
                .map(|v| (v.x, v.y, datum + v.z))
                .collect();
            let Some(n) = upward_normal(&ceiling.polygon) else {
                continue;
            };
            let slope = slope_deg(n);
            let pitch = (slope > SLOPE_EPS_DEG).then_some(slope);
            consider(
                idx,
                datum,
                PickedSurface::Ceiling(ceiling.id),
                &ring,
                n,
                pitch,
            );
        }
    }

    // Dwelling roof planes — full-house view only (storey_3d doesn't draw them),
    // skipping areas that coincide with the ceiling beneath (not drawn there).
    if current_storey_idx.is_none() {
        let datums = ber.storey_floor_datums();
        for area in &ber.roof.areas {
            let Some(&datum) = datums.get(area.storey_idx) else {
                continue;
            };
            if ber
                .storeys
                .get(area.storey_idx)
                .is_some_and(|s| area.follows_ceiling(&s.ceilings, &area.dormer_patches(ber)))
            {
                continue;
            }
            for region in area
                .regions_with_dormers(
                    ber,
                    ber_model::ber::roof::DEFAULT_OVERHANG_M,
                    ItemId::default(),
                )
                .regions
            {
                let ring: Vec<(f64, f64, f64)> = region
                    .polygon
                    .exterior
                    .iter()
                    .map(|v| (v.x, v.y, datum + v.z))
                    .collect();
                let Some(n) = upward_normal(&region.polygon) else {
                    continue;
                };
                let slope = slope_deg(n);
                // The assessor's drawn pitch for a sloped Gable/Hip face; the
                // plane's own slope for fan-derived planes; 0 for flat faces.
                let pitch = if area.pitch_deg > 0.05 && slope > SLOPE_EPS_DEG {
                    area.pitch_deg as f64
                } else {
                    slope
                };
                consider(
                    area.storey_idx,
                    datum,
                    PickedSurface::Roof { area_id: area.id },
                    &ring,
                    n,
                    Some(pitch),
                );
            }
        }
    }

    hits
}

/// The nearest FRONT-FACING surface under the cursor (the tablet's tap
/// semantics) — see [`collect_hits`].
pub fn pick_surface_3d(
    rect: egui::Rect,
    ber: &Ber,
    current_storey_idx: Option<usize>,
    view: ViewParams,
    screen: egui::Pos2,
) -> Option<SurfaceHit> {
    collect_hits(rect, ber, current_storey_idx, view, screen, 0.0)
        .into_iter()
        .filter(|(_, facing, _)| *facing)
        .min_by(|(a, ..), (b, ..)| a.total_cmp(b))
        .map(|(_, _, hit)| hit)
}

/// EVERY surface under the cursor, nearest first, both facings — callers
/// that filter by group/kind pick through the stack (a wall stays pickable
/// from behind, and a background surface of the wanted group is reachable
/// under a foreground one).
pub fn pick_surfaces_3d(
    rect: egui::Rect,
    ber: &Ber,
    current_storey_idx: Option<usize>,
    view: ViewParams,
    screen: egui::Pos2,
) -> Vec<SurfaceHit> {
    // A few pixels of slop so thin / edge-on faces (interior risers) are
    // clickable — the exact-containment tap stays pick_surface_3d's.
    let mut hits = collect_hits(rect, ber, current_storey_idx, view, screen, 6.0);
    hits.sort_by(|(a, ..), (b, ..)| a.total_cmp(b));
    hits.into_iter().map(|(_, _, hit)| hit).collect()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

/// Drop a geo-style closing duplicate vertex (last == first).
fn drop_closing_duplicate(ring: &mut Vec<(f64, f64, f64)>) {
    if ring.len() >= 2 {
        let (f, l) = (ring[0], ring[ring.len() - 1]);
        if (f.0 - l.0).abs() < 1e-9 && (f.1 - l.1).abs() < 1e-9 {
            ring.pop();
        }
    }
}

/// Even-odd point-in-polygon on a 2D ring (no closing duplicate required).
fn ring_contains(ring: &[(f64, f64)], p: (f64, f64)) -> bool {
    let n = ring.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = ring[i];
        let (xj, yj) = ring[j];
        if ((yi > p.1) != (yj > p.1)) && (p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The plane's tilt from horizontal in degrees, from its (any-sign) normal.
fn slope_deg(n: [f64; 3]) -> f64 {
    (n[0].hypot(n[1])).atan2(n[2].abs()).to_degrees()
}

/// A ceiling/roof plane's normal oriented UPWARD (+z) — the outward (weather)
/// side. `None` when degenerate or vertical (no up component to orient by).
fn upward_normal(polygon: &Polygon3D) -> Option<[f64; 3]> {
    let (n, _) = polygon.get_normal_and_distance_to_origin()?;
    if n.z.abs() < 1e-9 {
        return None;
    }
    let flip = if n.z < 0.0 { -1.0 } else { 1.0 };
    Some([n.x * flip, n.y * flip, n.z * flip])
}

/// A wall face's normal oriented OUTWARD (away from the storey interior):
/// take the plane normal (walls are vertical, so it's horizontal) and flip it
/// if a point nudged along it from the face's plan midpoint lands inside one
/// of the storey's floor rings. A wall whose both sides are outside every
/// floor ring (detached edge) keeps the un-flipped normal.
/// Distance from `p` to the closed ring's nearest edge (projected 2D).
fn ring_distance(ring: &[(f64, f64)], p: (f64, f64)) -> f64 {
    let n = ring.len();
    let mut best = f64::MAX;
    for i in 0..n {
        let (a, b) = (ring[i], ring[(i + 1) % n]);
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len2 = dx * dx + dy * dy;
        let t = if len2 > 1e-18 {
            (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let (qx, qy) = (a.0 + t * dx, a.1 + t * dy);
        best = best.min((p.0 - qx).hypot(p.1 - qy));
    }
    best
}

fn outward_wall_normal(polygon: &Polygon3D, storey: &Storey) -> Option<[f64; 3]> {
    let (n, _) = polygon.get_normal_and_distance_to_origin()?;
    let len_h = n.x.hypot(n.y);
    if len_h < 1e-9 {
        return None; // horizontal "wall" — not a wall face
    }
    let (nx, ny) = (n.x / len_h, n.y / len_h);
    let m = plan_midpoint(polygon);
    let probe = (m.0 + 0.05 * nx, m.1 + 0.05 * ny);
    let inside = storey.floors.regions.iter().any(|r| {
        let mut ring: Vec<(f64, f64, f64)> = r
            .polygon
            .exterior()
            .coords()
            .map(|c| (c.x, c.y, 0.0))
            .collect();
        drop_closing_duplicate(&mut ring);
        let plan: Vec<(f64, f64)> = ring.iter().map(|&(x, y, _)| (x, y)).collect();
        ring_contains(&plan, probe)
    });
    let flip = if inside { -1.0 } else { 1.0 };
    Some([nx * flip, ny * flip, 0.0])
}

/// The plan (x, y) average of a polygon's vertices.
fn plan_midpoint(polygon: &Polygon3D) -> (f64, f64) {
    let n = polygon.exterior.len().max(1) as f64;
    let (sx, sy) = polygon
        .exterior
        .iter()
        .fold((0.0, 0.0), |(sx, sy), v| (sx + v.x, sy + v.y));
    (sx / n, sy / n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ber_model::ber::roof::{RoofArea, RoofShape};
    use ber_model::ber::structural::{CeilingRegions, FloorRegions, Opes, WallRegion, WallRegions};
    use domain::points::{Point2f64, Point3f64};

    const RECT: egui::Rect = egui::Rect {
        min: egui::pos2(0.0, 0.0),
        max: egui::pos2(800.0, 600.0),
    };

    /// A 4 m × 1 m storey (deep-and-short so opposite walls OVERLAP on screen
    /// at yaw 0): floor ring, four vertical walls, flat ceiling at 2.4 m.
    fn box_storey() -> Storey {
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 1.0),
            Point2f64::new(0.0, 1.0),
        ];
        let wall = |a: (f64, f64), b: (f64, f64)| WallRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(a.0, a.1, 0.0),
                Point3f64::new(b.0, b.1, 0.0),
                Point3f64::new(b.0, b.1, 2.4),
                Point3f64::new(a.0, a.1, 2.4),
            ]),
            exposure: WallExposure::Exposed,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        };
        Storey {
            floors: FloorRegions::from_vertex_lists(vec![ring.clone()], ItemId::default()),
            walls: WallRegions {
                regions: vec![
                    wall((0.0, 0.0), (4.0, 0.0)), // south (y = 0)
                    wall((4.0, 0.0), (4.0, 1.0)),
                    wall((4.0, 1.0), (0.0, 1.0)), // north (y = 1)
                    wall((0.0, 1.0), (0.0, 0.0)),
                ],
            },
            ceilings: CeilingRegions::default_for_footprint(vec![ring], 2.4, ItemId::default()),
            ceiling_height_m: Some(2.4),
            ..Storey::default()
        }
    }

    /// Two box storeys stacked, with a 45° gable roof (ridge along x at
    /// y = 0.5) over the top storey.
    fn two_storey_gabled_ber() -> Ber {
        let mut ber = Ber::default();
        ber.storeys.push(box_storey());
        ber.storeys.push(box_storey());
        let footprint = vec![vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(4.0, 0.0),
            Point2f64::new(4.0, 1.0),
            Point2f64::new(0.0, 1.0),
        ]];
        let ridge = (Point2f64::new(0.0, 0.5), Point2f64::new(4.0, 0.5));
        let apexes = RoofArea::ridge_apexes(ridge, 45.0, 2.4, &footprint);
        assert_eq!(apexes.len(), 2, "gable ridge yields two apexes");
        ber.roof.areas.push(RoofArea {
            id: ItemId::default(),
            storey_idx: 1,
            covered_storeys: Vec::new(),
            shape: RoofShape::Gable,
            pitch_deg: 45.0,
            eave_z: 2.4,
            ridge: Some(ridge),
            footprint,
            apexes,
        });
        ber
    }

    /// A yaw-0 view: the camera sits on the +y side, so the y = 1 (north)
    /// walls face the viewer and the y = 0 (south) walls face away.
    fn view_yaw0() -> ViewParams {
        ViewParams {
            yaw_deg: 0.0,
            ..ViewParams::identity()
        }
    }

    /// Project a world point through the SAME transform the view is drawn
    /// (and picked) with, giving the cursor position for the tests.
    fn screen_at(
        ber: &Ber,
        idx: Option<usize>,
        view: ViewParams,
        p: (f64, f64, f64),
    ) -> egui::Pos2 {
        let t = transform_3d(RECT, ber, idx, view).expect("view renders");
        let (u, w) = super::super::project_3d(p.0, p.1, p.2, view.yaw_deg, view.pitch_deg);
        t.apply(u, w)
    }

    /// (a) A cursor over the roof face picks the ROOF (with the drawn pitch),
    /// not the top-storey wall that projects behind it.
    #[test]
    fn roof_face_beats_the_wall_behind_it() {
        let ber = two_storey_gabled_ber();
        let view = view_yaw0();
        let datum1 = ber.storey_floor_datums()[1];
        // A point on the viewer-side roof slope (y = 1.1 is on the overhang
        // lip, z = 2.9 − (1.1 − 0.5) = 2.3 storey-local) — chosen so the
        // top-storey north wall's projection also contains the cursor.
        let on_roof = (2.0, 1.1, datum1 + 2.3);
        let cursor = screen_at(&ber, None, view, on_roof);
        let hit = pick_surface_3d(RECT, &ber, None, view, cursor).expect("hit");
        assert!(
            matches!(hit.surface, PickedSurface::Roof { area_id } if area_id == ber.roof.areas[0].id),
            "roof wins over the wall behind: {:?}",
            hit.surface
        );
        assert_eq!(hit.storey_idx, 1);
        assert_eq!(hit.pitch_deg, Some(45.0), "the drawn gable pitch");
        assert!(
            (hit.world_point.2 - on_roof.2).abs() < 1e-6,
            "hit lands on the roof plane (z {} vs {})",
            hit.world_point.2,
            on_roof.2
        );
        assert!((hit.storey_local_z - 2.3).abs() < 1e-6, "storey-local z");
    }

    /// (b) Front-facing beats back-facing: over a cursor where the near
    /// (viewer-facing) and far (back-facing) walls overlap on screen, the near
    /// wall is picked; with the near wall REMOVED the far wall is NOT picked
    /// (it faces away). (d) A wall pick reports no pitch.
    #[test]
    fn front_facing_wall_beats_back_facing() {
        let ber = two_storey_gabled_ber();
        let view = view_yaw0();
        let datum1 = ber.storey_floor_datums()[1];
        let north_id = ber.storeys[1].walls.regions[2].id;
        // Mid-height on the top storey's NORTH wall (y = 1, faces the viewer);
        // the south wall (y = 0) projects over the same cursor but faces away.
        let on_north = (2.0, 1.0, datum1 + 1.0);
        let cursor = screen_at(&ber, None, view, on_north);
        let hit = pick_surface_3d(RECT, &ber, None, view, cursor).expect("hit");
        assert_eq!(
            hit.surface,
            PickedSurface::Wall(north_id),
            "the viewer-facing wall wins"
        );
        assert_eq!(hit.pitch_deg, None, "walls carry no pitch");

        // Remove the near wall: the far wall under the same cursor must NOT
        // become the hit — it's back-facing (ignored), so nothing is picked.
        let mut without_near = ber.clone();
        without_near.storeys[1].walls.regions.remove(2);
        let hit = pick_surface_3d(RECT, &without_near, None, view, cursor);
        assert!(
            hit.is_none(),
            "a back-facing far wall is never picked: {:?}",
            hit.map(|h| h.surface)
        );
    }

    /// (c) Nearest of two facing surfaces wins: looking straight down (pitch
    /// 90°) in the single-storey view, the floor and the flat ceiling both
    /// face the viewer under the cursor — the ceiling (closer) is picked.
    #[test]
    fn nearest_facing_surface_wins() {
        let ber = two_storey_gabled_ber();
        let view = ViewParams {
            yaw_deg: 0.0,
            pitch_deg: 90.0,
            ..ViewParams::identity()
        };
        let ceiling_id = ber.storeys[0].ceilings.regions[0].id;
        let cursor = screen_at(&ber, Some(0), view, (2.0, 0.5, 2.4));
        let hit = pick_surface_3d(RECT, &ber, Some(0), view, cursor).expect("hit");
        assert_eq!(
            hit.surface,
            PickedSurface::Ceiling(ceiling_id),
            "the ceiling is nearer the top-down viewer than the floor"
        );
        assert_eq!(hit.pitch_deg, None, "a flat ceiling carries no pitch");
        assert!((hit.storey_local_z - 2.4).abs() < 1e-6);
    }

    /// A fan-derived roof plane with NO drawn pitch (pitch_deg = 0, e.g.
    /// FollowCeiling-style apexes) reports the plane's own slope from its
    /// normal — ~45° for a 0.5 m rise over a 0.5 m run.
    #[test]
    fn roof_pitch_derived_from_the_plane_when_not_drawn() {
        let mut ber = two_storey_gabled_ber();
        ber.roof.areas[0].pitch_deg = 0.0;
        let view = view_yaw0();
        let datum1 = ber.storey_floor_datums()[1];
        let cursor = screen_at(&ber, None, view, (2.0, 1.1, datum1 + 2.3));
        let hit = pick_surface_3d(RECT, &ber, None, view, cursor).expect("hit");
        assert!(matches!(hit.surface, PickedSurface::Roof { .. }));
        let pitch = hit.pitch_deg.expect("roof planes always carry a pitch");
        assert!(
            (pitch - 45.0).abs() < 1.0,
            "derived pitch ≈ 45° (got {pitch})"
        );
    }
}
