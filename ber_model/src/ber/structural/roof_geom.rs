use geo::{LineString, Polygon};

use crate::ber::common::ItemId;
use domain::points::{Point2f64, Point3f64, Vector3f64};

#[allow(unused_imports)]
use super::*;

/// Build a `geo` polygon from an OPEN plan ring (no closing duplicate
/// required — `LineString` closes it).
pub(crate) fn ring_to_poly(ring: &[Point2f64]) -> Polygon<f64> {
    Polygon::new(
        LineString::from(ring.iter().map(|p| (p.x, p.y)).collect::<Vec<_>>()),
        vec![],
    )
}

/// Drop vertices that lie on the straight line between their neighbours (within
/// a small cross-product tolerance) — used to clean a merged outline ring and
/// the suggestion rings in `ber::suggest`.
pub(crate) fn strip_collinear(ring: &[Point2f64]) -> Vec<Point2f64> {
    let n = ring.len();
    if n < 3 {
        return ring.to_vec();
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let prev = ring[(i + n - 1) % n];
        let cur = ring[i];
        let next = ring[(i + 1) % n];
        let cross = (cur.x - prev.x) * (next.y - prev.y) - (cur.y - prev.y) * (next.x - prev.x);
        if cross.abs() > 1e-9 {
            out.push(cur);
        }
    }
    if out.len() >= 3 {
        out
    } else {
        ring.to_vec()
    }
}

/// Ceiling pieces and boolean debris with plan area below this (m²) are
/// slivers, not ceilings. ONE shared constant for the ceiling re-split
/// ([`CeilingRegions::with_z_deltas`]), the fan/ridge triangulations
/// ([`CeilingRegions::with_apexes`]), the group merge
/// ([`CeilingRegions::apex_groups`]), the upper-storey anchor/inheritance
/// queries ([`CeilingRegions::lowest_flat_z_over`] /
/// [`CeilingRegions::inherited_floor_levels`]), and the outline-suggestion
/// pipeline (`ber::suggest`) — the re-apply identities depend on all of them
/// drawing the line in the same place.
pub(crate) const SLIVER_AREA_M2: f64 = 1e-6;

/// D2 step 0 — PRE-LIFT: any ring vertex plan-matching one of `apexes`
/// (within 1e-6 m, the `fan_base_z` predicate) takes that apex's z.
/// Load-bearing for the re-apply-to-self identity: a fan/ridge triangle fed
/// back through `with_apexes` carries the apexes as ring CORNERS, and the
/// emitted hub/transition triangles keep the (lifted) boundary z — without
/// the lift a re-emitted corner could revert to a stale height.
fn pre_lift(mut ext: Vec<Point3f64>, apexes: &[&RoofApex]) -> Vec<Point3f64> {
    const EPS_M: f64 = 1e-6;
    for v in &mut ext {
        if let Some(a) = apexes
            .iter()
            .find(|a| (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M)
        {
            v.z = a.z_m;
        }
    }
    ext
}

/// Plan (projected) area of triangle `(p, q, r)` via the cross product —
/// positive CCW, negative CW.
fn tri_signed_plan_area(p: &Point3f64, q: &Point3f64, r: &Point3f64) -> f64 {
    ((q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)) / 2.0
}

/// Plan (projected) area of the closed ring `ext` (shoelace) — positive
/// CCW, negative CW.
fn ring_signed_plan_area(ext: &[Point3f64]) -> f64 {
    let n = ext.len();
    (0..n)
        .map(|i| {
            let (p, q) = (&ext[i], &ext[(i + 1) % n]);
            p.x * q.y - q.x * p.y
        })
        .sum::<f64>()
        / 2.0
}

/// FAN a region to a single `apex` (the 1-apex branch of
/// [`CeilingRegions::with_apexes`], where the semantics are documented):
/// pre-lift, then one triangle per boundary edge `(vᵢ, vᵢ₊₁, apex)`, dropping
/// sub-[`SLIVER_AREA_M2`] slivers — exactly the degenerate pair incident to
/// an apex that is already a corner. Fresh ids, default `opes`, inherited
/// `construction_id`.
pub(crate) fn fan_triangles(region: CeilingRegion, apex: &RoofApex) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let exposure = region.exposure;
    let ext = pre_lift(region.polygon.exterior, &[apex]);
    let apex_v = Point3f64::new(apex.point.x, apex.point.y, apex.z_m);
    let n = ext.len();
    let mut out = Vec::new();
    for i in 0..n {
        let a = ext[i];
        let b = ext[(i + 1) % n];
        if tri_signed_plan_area(&a, &b, &apex_v).abs() < SLIVER_AREA_M2 {
            continue;
        }
        out.push(CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![a, b, apex_v]),
            opes: Opes::default(),
            construction_id: cid,
            exposure,
        });
    }
    out
}

/// Split each edge of `ext` at the point where the ridge parameter
/// `t(P) = (P − a)·d / |d|²` crosses 0.5 STRICTLY INSIDE the edge — the
/// ridge MIDLINE. Run before hub assignment (`ridge_edge_hubs`) so the two
/// hubs split a tied edge between them instead of one hub hogging it: on a
/// centred rectangle ridge both long edges used to midpoint-tie to B, which
/// sent ALL FOUR corners to B and made the roof asymmetric. The inserted
/// vertex's z is linearly interpolated along the edge (boundary edges are
/// typically level, so it's the base z). Edges whose endpoint sits exactly
/// ON the midline don't split (the vertex is already there); a whole edge
/// ALONG the midline has constant `t = 0.5` and never crosses. Degenerate
/// ridge (`|d|² < 1e-12`) → `ext` unchanged (the caller falls back to a
/// fan). The SINGLE source of the split — `ridge_triangles` and
/// [`ridge_hub_switches`] both run it, or the assign-time hub-switch check
/// would validate a different roof than resolve builds.
fn ridge_split_ring(ext: Vec<Point3f64>, a: Point2f64, b: Point2f64) -> Vec<Point3f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let d2 = dx * dx + dy * dy;
    if d2 < 1e-12 {
        return ext;
    }
    let t_of = |v: &Point3f64| ((v.x - a.x) * dx + (v.y - a.y) * dy) / d2;
    let n = ext.len();
    let mut out = Vec::with_capacity(n + 2);
    for i in 0..n {
        let p = ext[i];
        let q = ext[(i + 1) % n];
        out.push(p);
        let (tp, tq) = (t_of(&p), t_of(&q));
        if (tp - 0.5) * (tq - 0.5) < 0.0 {
            let s = (0.5 - tp) / (tq - tp);
            out.push(p + s * (q - p));
        }
    }
    out
}

/// Per-EDGE hub assignment for a ridge from `a` to `b` over a plan ring:
/// with `d = b − a` and the unclamped ridge parameter
/// `t(P) = (P − a)·d / |d|²`, edge `i` (from `ring[i]` to `ring[i+1]`)
/// belongs to hub B iff `t(midpoint) ≥ 0.5` (tie → B — after
/// [`ridge_split_ring`] a tie only remains for an edge lying ALONG the
/// midline, equidistant from both hubs so either choice tiles, or for a
/// zero-length edge, whose triangle is a sliver anyway).
/// Returns one bool per edge (`true` = B), or `None` for a degenerate ridge
/// (`|d|² < 1e-12`). The SINGLE source of the hub rule — `ridge_triangles`
/// and [`ridge_hub_switches`] must agree or the assign-time hub-switch check
/// would validate a different roof than resolve builds.
fn ridge_edge_hubs(ring: &[(f64, f64)], a: Point2f64, b: Point2f64) -> Option<Vec<bool>> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let d2 = dx * dx + dy * dy;
    if d2 < 1e-12 {
        return None;
    }
    let n = ring.len();
    Some(
        (0..n)
            .map(|i| {
                let p = ring[i];
                let q = ring[(i + 1) % n];
                let (mx, my) = ((p.0 + q.0) / 2.0, (p.1 + q.1) / 2.0);
                ((mx - a.x) * dx + (my - a.y) * dy) / d2 >= 0.5
            })
            .collect(),
    )
}

/// How many times the per-edge hub assignment SWITCHES between the two ridge
/// endpoints walking `ring` — after the midline split ([`ridge_split_ring`]),
/// so the count equals the number of boundary crossings of the ridge midline
/// and agrees with what `ridge_triangles` builds (counted at vertices whose
/// entering and leaving edges disagree — each switch is one transition
/// triangle). A convex ring with both apexes interior switches exactly 2;
/// more means the ring wraps past the ridge axis (a deep notch) and the
/// transition triangles would overlap — assign-time validation rejects > 2,
/// resolve warns and proceeds. `0` for rings with fewer than 3 vertices or a
/// degenerate ridge (`a` ≈ `b`). Pure plan-space — usable on a group's
/// merged ring before any apex exists.
pub fn ridge_hub_switches(ring: &[Point2f64], a: Point2f64, b: Point2f64) -> usize {
    if ring.len() < 3 {
        return 0;
    }
    let ext: Vec<Point3f64> = ring.iter().map(|p| Point3f64::new(p.x, p.y, 0.0)).collect();
    let plan: Vec<(f64, f64)> = ridge_split_ring(ext, a, b)
        .iter()
        .map(|p| (p.x, p.y))
        .collect();
    let Some(hubs) = ridge_edge_hubs(&plan, a, b) else {
        return 0;
    };
    let n = hubs.len();
    (0..n).filter(|&i| hubs[i] != hubs[(i + n - 1) % n]).count()
}

/// Triangulate a region under a RIDGE between apexes `a` and `b` (the 2-apex
/// branch of [`CeilingRegions::with_apexes`]), per design D2:
///
/// 0. PRE-LIFT ring vertices plan-matching an apex to that apex's z.
/// 1. Degenerate ridge (`|b − a|² < 1e-12` in plan) → warn + single fan
///    to `a`.
/// 2. SPLIT boundary edges where they cross the ridge MIDLINE (`t = 0.5`),
///    z interpolated along the edge ([`ridge_split_ring`]) — so the two
///    hubs share a tied edge symmetrically instead of B hogging it.
/// 3. HUB triangles, in edge order: each boundary (sub-)edge `(vᵢ, vᵢ₊₁)`
///    rises to its hub `H` — `a` if the edge midpoint's ridge parameter
///    `t < 0.5`, else `b` (see [`ridge_edge_hubs`]) — emitting
///    `(vᵢ, vᵢ₊₁, H)` with the boundary keeping its (pre-lifted /
///    interpolated) z and `H` at the hub's `z_m`.
/// 4. TRANSITION triangles, in vertex order: at each vertex `v` whose
///    entering and leaving edges disagree, `(v, hub(leaving),
///    hub(entering))` — this order preserves CCW winding. After the split
///    these sit at the midline crossings, tiling symmetrically.
///
/// Sub-[`SLIVER_AREA_M2`] (plan) triangles drop in both steps. Fresh ids,
/// default `opes`, inherited `construction_id` (fan parity). For a convex
/// ring (≤ 2 hub switches) the triangles tile the region exactly; every
/// triangle is trivially planar. The caller (`with_apexes`) merges coplanar
/// neighbours afterwards ([`merge_coplanar_faces`]).
pub(crate) fn ridge_triangles(
    region: CeilingRegion,
    a: &RoofApex,
    b: &RoofApex,
) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let exposure = region.exposure;
    let ext = pre_lift(region.polygon.exterior, &[a, b]);
    let ext = ridge_split_ring(ext, a.point, b.point);
    let plan: Vec<(f64, f64)> = ext.iter().map(|v| (v.x, v.y)).collect();
    let Some(hubs) = ridge_edge_hubs(&plan, a.point, b.point) else {
        log::warn!(
            "degenerate ridge: apexes coincide in plan at ({:.3}, {:.3}) — fanning to the first",
            a.point.x,
            a.point.y
        );
        return fan_triangles(
            CeilingRegion {
                id: ItemId::default(),
                polygon: Polygon3D::new(ext),
                opes: Opes::default(),
                construction_id: cid,
                exposure,
            },
            a,
        );
    };
    let hub_v = |is_b: bool| {
        let h = if is_b { b } else { a };
        Point3f64::new(h.point.x, h.point.y, h.z_m)
    };
    let mk = |verts: Vec<Point3f64>| CeilingRegion {
        id: ItemId::default(),
        polygon: Polygon3D::new(verts),
        opes: Opes::default(),
        construction_id: cid,
        exposure,
    };
    let n = ext.len();
    let mut out = Vec::new();
    // Hub triangles, in edge order.
    for i in 0..n {
        let (va, vb) = (ext[i], ext[(i + 1) % n]);
        let h = hub_v(hubs[i]);
        if tri_signed_plan_area(&va, &vb, &h).abs() < SLIVER_AREA_M2 {
            continue;
        }
        out.push(mk(vec![va, vb, h]));
    }
    // Transition triangles, in vertex order.
    for i in 0..n {
        let entering = hubs[(i + n - 1) % n];
        let leaving = hubs[i];
        if entering == leaving {
            continue;
        }
        let (hl, he) = (hub_v(leaving), hub_v(entering));
        if tri_signed_plan_area(&ext[i], &hl, &he).abs() < SLIVER_AREA_M2 {
            continue;
        }
        out.push(mk(vec![ext[i], hl, he]));
    }
    out
}

/// A rect ridge side at or below this (m) is a line, not a rectangle — the
/// ber_model-side floor matching the document's MIN_RIDGE_LENGTH_M.
const RECT_MIN_SIDE_M: f64 = 0.01;

/// The 4 apexes form an AXIS-ALIGNED RECTANGLE: their quantized plan points
/// are exactly the corner set {x0, x1} × {y0, y1} (one apex per corner),
/// both sides exceed [`RECT_MIN_SIDE_M`], and every z agrees within 1e-6 —
/// the flat-topped-hip (pavilion) recognizer gating [`with_apexes`]' n = 4
/// branch. Returns the rect as `(lo, hi)` corners. Public so the shell's
/// committed-rect view applies the SAME predicate the resolve does.
///
/// [`with_apexes`]: CeilingRegions::with_apexes
pub fn apexes_form_rect(apexes: &[&RoofApex]) -> Option<(Point2f64, Point2f64)> {
    const Z_EPS: f64 = 1e-6;
    if apexes.len() != 4 {
        return None;
    }
    let z0 = apexes[0].z_m;
    if apexes.iter().any(|a| (a.z_m - z0).abs() > Z_EPS) {
        return None;
    }
    let pts: Vec<Point2f64> = apexes
        .iter()
        .map(|a| RoofApex::quantize_point(a.point))
        .collect();
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for p in &pts {
        x0 = x0.min(p.x);
        y0 = y0.min(p.y);
        x1 = x1.max(p.x);
        y1 = y1.max(p.y);
    }
    if x1 - x0 <= RECT_MIN_SIDE_M || y1 - y0 <= RECT_MIN_SIDE_M {
        return None;
    }
    // Exactly one apex per corner (exact equality is right here: the bbox
    // extremes come from the same quantized values).
    let one_at = |cx: f64, cy: f64| pts.iter().filter(|p| p.x == cx && p.y == cy).count() == 1;
    (one_at(x0, y0) && one_at(x1, y0) && one_at(x1, y1) && one_at(x0, y1))
        .then_some((Point2f64::new(x0, y0), Point2f64::new(x1, y1)))
}

/// Which rect SIDE plan point `p` faces — the corner-sector classification
/// SHARED by the run loft ([`rect_ridge_faces`], per edge midpoint) and the
/// foldover guard ([`rect_hip_switches`]) (the single-source rule,
/// [`ridge_edge_hubs`]' rect analogue): the side with the LARGEST outward
/// excess, the 45° hip rays from each corner being the ties (first side in
/// order wins a tie — deterministic). `0` = bottom (faces y < lo.y),
/// `1` = right, `2` = top, `3` = left — matching the CCW corner ring
/// c00 → c10 → c11 → c01, where side `i` runs from corner `i` to corner
/// `(i + 1) % 4`.
fn rect_side_sector(p: Point2f64, lo: Point2f64, hi: Point2f64) -> usize {
    let e = [lo.y - p.y, p.x - hi.x, p.y - hi.y, lo.x - p.x];
    let mut best = 0;
    for (i, v) in e.iter().enumerate().skip(1) {
        if *v > e[best] {
            best = i;
        }
    }
    best
}

/// How many times the SIDE assignment switches walking `ring` — per RAW
/// edge, sector by midpoint ([`rect_side_sector`], exactly the convention
/// [`rect_ridge_faces`] lofts with; NO vertices are inserted) — the rect
/// foldover guard. A ring the run loft can tile visits the four sectors
/// once each in order: exactly 4 switches. Fewer means a rect side has no
/// facing run (the loft would fold across the rect or leave a gap); more
/// means the ring wraps a notch into a foreign sector. A VERTEX whose own
/// sector matches NEITHER adjacent edge's (a needle dip toward a corner the
/// edge walk can't see) would hip to a corner neither run face uses — it
/// counts as 2 extra switches (the excursion enters and leaves the foreign
/// sector). Assign-time validation rejects ≠ 4; resolve warns and proceeds
/// (legacy input must never panic). `0` for rings with fewer than 3
/// vertices or a degenerate rect. Pure plan-space.
pub fn rect_hip_switches(ring: &[Point2f64], lo: Point2f64, hi: Point2f64) -> usize {
    if ring.len() < 3 || hi.x - lo.x <= 0.0 || hi.y - lo.y <= 0.0 {
        return 0;
    }
    let n = ring.len();
    let sides: Vec<usize> = (0..n)
        .map(|i| {
            let (p, q) = (&ring[i], &ring[(i + 1) % n]);
            rect_side_sector(Point2f64::new((p.x + q.x) / 2.0, (p.y + q.y) / 2.0), lo, hi)
        })
        .collect();
    (0..n)
        .map(|i| {
            let (entering, leaving) = (sides[(i + n - 1) % n], sides[i]);
            let edge_switch = usize::from(entering != leaving);
            let vs = rect_side_sector(ring[i], lo, hi);
            let vertex_break = 2 * usize::from(vs != entering && vs != leaving);
            edge_switch + vertex_break
        })
        .sum()
}

/// D3 — the PAVILION LOFT (the rect analogue of [`ridge_triangles`], the
/// 4-corner branch of [`CeilingRegions::with_apexes`]):
///
/// 0. PRE-LIFT ring vertices plan-matching a corner to that corner's z.
/// 1. Per boundary edge, side = the rect SIDE its midpoint faces
///    ([`rect_side_sector`] — the same convention [`rect_hip_switches`]
///    guards with). NO boundary vertices are inserted.
/// 2. Per maximal same-side RUN of consecutive edges, ONE face: the run's
///    vertices in ring order, closed back along the facing rect side
///    `S_b → S_a` (the side runs S_a → S_b CCW along the rect; the face
///    walks it in reverse, keeping CCW). A single-edge run is a QUAD; a
///    multi-edge run (an L-shaped boundary) a longer polygon. Each
///    sector-switch vertex thus connects straight to the rect corner
///    BETWEEN its two sectors — its nearest corner, the hip — and
///    adjacent runs share that vertex→corner edge by construction, so
///    the loft tiles and stays watertight.
/// 3. Append the flat TOP — the rect at the corners' z, each corner
///    vertex copied VERBATIM from its apex (the corner-identity
///    convention `apex_groups` reads).
///
/// Sub-[`SLIVER_AREA_M2`] plan faces drop — a run ALONG a rect side ON the
/// region boundary lofts NOTHING (the mono-gable side: the boundary edge
/// IS the rect edge there; re-applying to the top rect itself drops
/// everything but the top). With axis-aligned input every single-edge
/// run's quad is PLANAR (its z is linear in the coordinate perpendicular
/// to its side) even for unequal or off-centre eaves offsets; a
/// multi-edge run's polygon may be non-planar and is kept as built. For a
/// rectangular region this yields EXACTLY 4 side quads + the flat top.
/// Fresh ids, default `opes`, inherited `construction_id`; the caller
/// still runs [`merge_coplanar_faces`] (harmless — runs are already
/// maximal).
pub(crate) fn rect_ridge_faces(
    region: CeilingRegion,
    corners: [&RoofApex; 4],
) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for a in &corners {
        let q = RoofApex::quantize_point(a.point);
        x0 = x0.min(q.x);
        y0 = y0.min(q.y);
        x1 = x1.max(q.x);
        y1 = y1.max(q.y);
    }
    let (lo, hi) = (Point2f64::new(x0, y0), Point2f64::new(x1, y1));
    // The canonical CCW corner ring c00 → c10 → c11 → c01, each at its OWN
    // apex's verbatim coordinates + z (the caller verified one per corner;
    // the fallback never fires on validated input).
    let corner_at = |cx: f64, cy: f64| -> Point3f64 {
        corners
            .iter()
            .find(|a| {
                let q = RoofApex::quantize_point(a.point);
                q.x == cx && q.y == cy
            })
            .map(|a| Point3f64::new(a.point.x, a.point.y, a.z_m))
            .unwrap_or_else(|| Point3f64::new(cx, cy, corners[0].z_m))
    };
    let cs = [
        corner_at(x0, y0),
        corner_at(x1, y0),
        corner_at(x1, y1),
        corner_at(x0, y1),
    ];
    let ext = pre_lift(region.polygon.exterior, &corners[..]);
    let exposure = region.exposure;
    let mk = |verts: Vec<Point3f64>| CeilingRegion {
        id: ItemId::default(),
        polygon: Polygon3D::new(verts),
        opes: Opes::default(),
        construction_id: cid,
        exposure,
    };
    let n = ext.len();
    let sides: Vec<usize> = (0..n)
        .map(|i| {
            let (p, q) = (&ext[i], &ext[(i + 1) % n]);
            rect_side_sector(Point2f64::new((p.x + q.x) / 2.0, (p.y + q.y) / 2.0), lo, hi)
        })
        .collect();
    // Maximal same-side runs, each by the index of its FIRST edge (an edge
    // whose side differs from the previous edge's). A cyclic sequence has
    // either no switch at all or at least two — never exactly one.
    let starts: Vec<usize> = (0..n)
        .filter(|&i| sides[i] != sides[(i + n - 1) % n])
        .collect();
    let mut runs: Vec<(usize, Vec<Point3f64>)> = Vec::new();
    if starts.is_empty() {
        // The ring never leaves one sector — it cannot enclose the rect
        // (the warned, legacy posture): one run of the whole ring.
        runs.push((sides[0], ext.clone()));
    } else {
        for (k, &i0) in starts.iter().enumerate() {
            let i1 = starts[(k + 1) % starts.len()];
            let mut verts = vec![ext[i0]];
            let mut i = i0;
            loop {
                i = (i + 1) % n;
                verts.push(ext[i]);
                if i == i1 {
                    break;
                }
            }
            runs.push((sides[i0], verts));
        }
    }
    let mut out = Vec::new();
    for (side, mut verts) in runs {
        // Close the run back along its rect side: S_b then S_a.
        verts.push(cs[(side + 1) % 4]);
        verts.push(cs[side]);
        // A boundary vertex can coincide with a rect corner (a corner apex
        // ON the ring): drop consecutive duplicates, wrap included.
        verts.dedup_by(|a, b| merge_pts_eq(a, b));
        while verts.len() > 1 && merge_pts_eq(&verts[0], verts.last().unwrap()) {
            verts.pop();
        }
        if verts.len() >= 3 && ring_signed_plan_area(&verts).abs() >= SLIVER_AREA_M2 {
            out.push(mk(verts));
        }
    }
    // The flat TOP.
    out.push(mk(cs.to_vec()));
    out
}

/// Vertex / plane tolerance for the coplanar face merge, in metres.
const MERGE_EPS_M: f64 = 1e-6;

/// Two 3D vertices equal within [`MERGE_EPS_M`] on every coordinate.
fn merge_pts_eq(u: &Point3f64, v: &Point3f64) -> bool {
    (u.x - v.x).abs() <= MERGE_EPS_M
        && (u.y - v.y).abs() <= MERGE_EPS_M
        && (u.z - v.z).abs() <= MERGE_EPS_M
}

/// Unit normal `n` and offset `d` (`n·v = d`) of a face's plane via Newell's
/// method (robust for any planar polygon, not just its first three
/// vertices); `None` when degenerate.
pub(crate) fn face_plane(ext: &[Point3f64]) -> Option<(Vector3f64, f64)> {
    if ext.len() < 3 {
        return None;
    }
    let (mut nx, mut ny, mut nz) = (0.0, 0.0, 0.0);
    for i in 0..ext.len() {
        let p = ext[i];
        let q = ext[(i + 1) % ext.len()];
        nx += (p.y - q.y) * (p.z + q.z);
        ny += (p.z - q.z) * (p.x + q.x);
        nz += (p.x - q.x) * (p.y + q.y);
    }
    let len = (nx * nx + ny * ny + nz * nz).sqrt();
    if len < 1e-12 {
        return None;
    }
    let n = Vector3f64::new(nx / len, ny / len, nz / len);
    let d = ext
        .iter()
        .map(|v| n.x * v.x + n.y * v.y + n.z * v.z)
        .sum::<f64>()
        / ext.len() as f64;
    Some((n, d))
}

/// `w` lies on the straight 3D line through its neighbours `p` → `q` (within
/// [`MERGE_EPS_M`] of it) — a redundant vertex on a merged boundary.
fn collinear3(p: &Point3f64, w: &Point3f64, q: &Point3f64) -> bool {
    let u = *w - *p;
    let v = *q - *p;
    let vl = v.norm();
    if vl <= MERGE_EPS_M {
        // p ≈ q: w sits between duplicate neighbours — treat as redundant
        // only when it coincides with them too.
        return u.norm() <= MERGE_EPS_M;
    }
    u.cross(&v).norm() <= MERGE_EPS_M * vl
}

/// First edge of `r1` whose REVERSE appears in `r2`:
/// `(i1, i2)` with `r2[i2] == r1[i1+1]` and `r2[i2+1] == r1[i1]`
/// ([`merge_pts_eq`], plan + z). Zero-length edges never qualify.
fn shared_edge(r1: &[Point3f64], r2: &[Point3f64]) -> Option<(usize, usize)> {
    let (n1, n2) = (r1.len(), r2.len());
    for i1 in 0..n1 {
        let (u, v) = (r1[i1], r1[(i1 + 1) % n1]);
        if merge_pts_eq(&u, &v) {
            continue;
        }
        for i2 in 0..n2 {
            if merge_pts_eq(&r2[i2], &v) && merge_pts_eq(&r2[(i2 + 1) % n2], &u) {
                return Some((i1, i2));
            }
        }
    }
    None
}

/// Merge two face rings into their union polygon when they (a) share a FULL
/// edge — two consecutive vertices equal within [`MERGE_EPS_M`] (plan + z),
/// either orientation (`r2` is reversed first if the edge matches
/// same-direction) — and (b) are COPLANAR: every vertex of both within
/// [`MERGE_EPS_M`] of the plane of the larger (by plan area, Newell fit).
/// The shared edge is removed, the boundaries spliced, and collinear-in-plane
/// vertices along the merged boundary dropped. `None` when the pair doesn't
/// qualify (or the union would degenerate below 3 vertices).
fn merge_coplanar_pair(r1: &[Point3f64], r2: &[Point3f64]) -> Option<Vec<Point3f64>> {
    if r1.len() < 3 || r2.len() < 3 {
        return None;
    }
    // (a) the shared full edge, normalizing r2 to opposite orientation.
    let (r2v, (i1, i2)) = match shared_edge(r1, r2) {
        Some(hit) => (r2.to_vec(), hit),
        None => {
            let rev: Vec<Point3f64> = r2.iter().rev().copied().collect();
            let hit = shared_edge(r1, &rev)?;
            (rev, hit)
        }
    };
    // (b) coplanarity against the larger face's plane.
    let plan_area = |ext: &[Point3f64]| {
        let n = ext.len();
        (0..n)
            .map(|i| {
                let p = ext[i];
                let q = ext[(i + 1) % n];
                p.x * q.y - q.x * p.y
            })
            .sum::<f64>()
            .abs()
            / 2.0
    };
    let big = if plan_area(r1) >= plan_area(&r2v) {
        r1
    } else {
        &r2v[..]
    };
    let (n, d) = face_plane(big)?;
    let on_plane = |ext: &[Point3f64]| {
        ext.iter()
            .all(|v| (n.x * v.x + n.y * v.y + n.z * v.z - d).abs() <= MERGE_EPS_M)
    };
    if !on_plane(r1) || !on_plane(&r2v) {
        return None;
    }
    // (c) splice: r1 from v all the way round to u, then r2's path from u
    // back round to v with both shared endpoints dropped.
    let (n1, n2) = (r1.len(), r2v.len());
    let mut merged: Vec<Point3f64> = (0..n1).map(|k| r1[(i1 + 1 + k) % n1]).collect();
    merged.extend((2..n2).map(|k| r2v[(i2 + k) % n2]));
    // (d) drop collinear / duplicate vertices along the merged boundary.
    let mut changed = true;
    while changed && merged.len() > 3 {
        changed = false;
        let mut i = 0;
        while i < merged.len() && merged.len() > 3 {
            let m = merged.len();
            let p = merged[(i + m - 1) % m];
            let q = merged[(i + 1) % m];
            if collinear3(&p, &merged[i], &q) {
                merged.remove(i);
                changed = true;
            } else {
                i += 1;
            }
        }
    }
    (merged.len() >= 3).then_some(merged)
}

/// The COPLANAR MERGE pass run on each region's fan/ridge output at the end
/// of [`CeilingRegions::with_apexes`]: repeatedly merge the first pair of
/// faces (in stable index order — determinism is part of the reload
/// fixpoint) that share a full edge and are coplanar
/// ([`merge_coplanar_pair`]), until none qualify. Coplanar triangle
/// neighbours thus collapse into single planar polygons (the rectangle
/// ridge's two slopes become trapezoids). Merged faces inherit
/// `construction_id`, get fresh ids and default `opes` (split parity);
/// untouched faces pass through as-is.
pub(crate) fn merge_coplanar_faces(mut faces: Vec<CeilingRegion>) -> Vec<CeilingRegion> {
    'outer: loop {
        for i in 0..faces.len() {
            for j in (i + 1)..faces.len() {
                if let Some(merged) =
                    merge_coplanar_pair(&faces[i].polygon.exterior, &faces[j].polygon.exterior)
                {
                    let construction_id = faces[i].construction_id;
                    let exposure = faces[i].exposure;
                    faces[i] = CeilingRegion {
                        id: ItemId::default(),
                        polygon: Polygon3D::new(merged),
                        opes: Opes::default(),
                        construction_id,
                        exposure,
                    };
                    faces.remove(j);
                    continue 'outer;
                }
            }
        }
        return faces;
    }
}

/// Decompose a boolean-op result polygon into SIMPLE (exterior-only) pieces.
///
/// `Polygon3D` stores only an exterior ring, so emitting a `geo` polygon that
/// carries interior rings would silently FILL its holes — and `difference`
/// produces exactly that when a step area lies strictly INSIDE a region (the
/// remainder is the region with a hole). The filled phantom would overlap the
/// stepped piece and inflate the total ceiling area on the next re-derive,
/// breaking the canonicalize → re-derive fixpoint.
///
/// All ceiling-step geometry is axis-aligned, so a polygon with holes is cut
/// with an axis-aligned line through the first hole's centre (intersection
/// with the two half-plane rectangles over the polygon's bbox) and the pieces
/// are decomposed recursively. The cut crosses that hole, so neither half can
/// still contain it whole — every split strictly reduces the remaining hole
/// extent and the recursion terminates with simple pieces.
pub(crate) fn split_to_simple(poly: &Polygon<f64>) -> Vec<Polygon<f64>> {
    use geo::bool_ops::BooleanOps;
    use geo::BoundingRect;
    if poly.interiors().is_empty() {
        return vec![poly.clone()];
    }
    let (Some(bbox), Some(hole)) = (poly.bounding_rect(), poly.interiors()[0].bounding_rect())
    else {
        // Degenerate ring with no extent — nothing meaningful to split.
        return vec![poly.clone()];
    };
    // Cut perpendicular to the hole's WIDER dimension so the line genuinely
    // crosses it (a vertical cut through a hair-thin vertical hole may not).
    let c = hole.center();
    let (min, max) = (bbox.min(), bbox.max());
    let pad = 1.0; // clip rects overhang the bbox so its edges never graze
    let rect = |x0: f64, y0: f64, x1: f64, y1: f64| {
        Polygon::new(
            LineString::from(vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]),
            vec![],
        )
    };
    let halves = if hole.width() >= hole.height() {
        [
            rect(min.x - pad, min.y - pad, c.x, max.y + pad),
            rect(c.x, min.y - pad, max.x + pad, max.y + pad),
        ]
    } else {
        [
            rect(min.x - pad, min.y - pad, max.x + pad, c.y),
            rect(min.x - pad, c.y, max.x + pad, max.y + pad),
        ]
    };
    halves
        .iter()
        .flat_map(|half| poly.intersection(half).0)
        .flat_map(|piece| split_to_simple(&piece))
        .collect()
}
