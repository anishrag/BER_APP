use geo::{LineString, Polygon};

use crate::ber::common::ItemId;
use domain::points::{point_to_segment_distance, Point2f64, Point3f64, Vector3f64};

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

/// Drop vertices that lie on (or very nearly on) the straight line between their
/// neighbours — used to clean a merged outline ring, the suggestion rings in
/// `ber::suggest`, and (public since the fan-marker feature) the group merged
/// rings behind the document layer's tap-to-unfan markers: a collinear ring
/// vertex contributes no fan plane of its own (its hub triangles are coplanar
/// and merge away), so it is not a marker.
///
/// `cross` is twice the signed area of the (prev, cur, next) triangle, in m²
/// (coords are metres). Real building corners are far off collinear — even a
/// shallow one clears ~0.7 (e.g. a 10° bend of 2 m edges). Spurious vertices —
/// a corner of the storey below landing on this outline's straight edge, then
/// snapped exactly collinear by f32 quantization — sit near zero (≤ ~1e-4).
/// A real outline has no genuine slight bends, so `TOL` sits between the two:
/// anything below it is noise to drop. (Was 1e-9, which only caught EXACTLY
/// collinear and so missed near-collinear vertices, splitting one wall in two.)
pub fn strip_collinear(ring: &[Point2f64]) -> Vec<Point2f64> {
    // Perpendicular deviation this catches is TOL / span: ~1–5 mm over typical
    // 2–8 m edges — far tighter than any real corner, far looser than f32 noise.
    const TOL: f64 = 1e-2;
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
        if cross.abs() > TOL {
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

/// The minor bounding-box dimension (m) below which a boolean-op piece is a
/// degenerate SLIVER — a long-thin strip that slips past the area guard. A
/// 6 m × 0.48 µm strip is 2.87e-6 m² (> [`SLIVER_AREA_M2`], so the
/// area-only check misses it) yet is physically not a region (field report
/// 797a7ff5: a phase boundary one f32 ULP off the shared storey edge). ~1 mm
/// is far below any real floor/ceiling division and far above the
/// f32↔f64 / near-coincident-edge noise that produces these strips.
pub(crate) const MIN_PIECE_MINOR_M: f64 = 1e-3;

/// Whether a plan piece must be dropped at split time: sub-[`SLIVER_AREA_M2`]
/// area OR a minor bbox dimension below [`MIN_PIECE_MINOR_M`] (the
/// thickness-aware guard the area-only checks miss). Defence in depth for
/// every boolean split — `FloorRegions::partition`,
/// `CeilingRegions::split_faces_coplanar`, and inter-storey
/// `split_floors_intact` — so a near-coincident-edge mismatch can't
/// resurrect a degenerate strip.
pub(crate) fn plan_piece_is_degenerate(poly: &Polygon<f64>) -> bool {
    use geo::{Area, BoundingRect};
    if poly.unsigned_area() < SLIVER_AREA_M2 {
        return true;
    }
    match poly.bounding_rect() {
        Some(bb) => bb.width().min(bb.height()) < MIN_PIECE_MINOR_M,
        None => true,
    }
}

/// Plan tolerance (m) for matching a ring vertex to a stored UNFANNED point
/// (`Storey::ceiling_unfanned`). Looser than the 1e-6 apex-corner identity:
/// the unfanned points are f32-quantized ring vertices, but the ring a later
/// resolve feeds back may be boolean-op output sitting a few 1e-7 m off.
/// Same rationale (and value) as [`APEX_BOUNDARY_EPS_M`]; real neighbouring
/// vertices are centimetres apart, so no false capture.
pub const UNFAN_MATCH_EPS_M: f64 = 1e-4;

/// Whether ring vertex `v` plan-matches a point of the UNFANNED set (within
/// [`UNFAN_MATCH_EPS_M`]).
fn is_unfanned(v: &Point3f64, unfanned: &[Point2f64]) -> bool {
    unfanned
        .iter()
        .any(|u| (v.x - u.x).abs() <= UNFAN_MATCH_EPS_M && (v.y - u.y).abs() <= UNFAN_MATCH_EPS_M)
}

/// Evaluate the plane through 3 points at plan position `(x, y)`; `None`
/// when the plane is vertical / degenerate over plan (|n·ẑ| ≈ 0).
/// `pub(crate)`: the roof-area overhang solve evaluates the SAME gable side
/// planes over its offset ring.
pub(crate) fn plane_z_at(
    p0: Point3f64,
    p1: Point3f64,
    p2: Point3f64,
    x: f64,
    y: f64,
) -> Option<f64> {
    let nrm = (p1 - p0).cross(&(p2 - p0));
    if nrm.z.abs() <= 1e-9 {
        return None;
    }
    Some(p0.z - (nrm.x * (x - p0.x) + nrm.y * (y - p0.y)) / nrm.z)
}

/// The per-vertex UNFAN targets of a ring: plan-matches the `unfanned` set
/// and is NOT an apex corner (apex corners belong to `pre_lift`). `None`
/// (nothing to lift) when the set misses the ring entirely or fewer than two
/// fanned vertices would remain (no span to embed against — warned).
fn unfan_targets(
    ext: &[Point3f64],
    unfanned: &[Point2f64],
    apexes: &[&CeilingApex],
) -> Option<Vec<bool>> {
    // NOISE ABSORBER: matches a ring vertex to an apex CORNER copied verbatim
    // into the fan triangle (exact f64 copy). 1e-4 aligns with the codebase's
    // apex-corner convention ([`APEX_BOUNDARY_EPS_M`]/[`UNFAN_MATCH_EPS_M`]) —
    // real ring corners are never that close to an apex.
    const EPS_M: f64 = 1e-4;
    if unfanned.is_empty() || ext.len() < 3 {
        return None;
    }
    let is_apex = |v: &Point3f64| {
        apexes
            .iter()
            .any(|a| (v.x - a.point.x).abs() <= EPS_M && (v.y - a.point.y).abs() <= EPS_M)
    };
    let target: Vec<bool> = ext
        .iter()
        .map(|v| is_unfanned(v, unfanned) && !is_apex(v))
        .collect();
    if !target.iter().any(|t| *t) {
        return None;
    }
    if target.iter().filter(|t| !**t).count() < 2 {
        log::warn!(
            "unfan: fewer than two fanned vertices would remain on a ceiling ring — \
             ignoring the unfanned set for this region"
        );
        return None;
    }
    Some(target)
}

/// The UNFAN EMBED for a single-apex FAN — the tap-to-unfan escape hatch,
/// run right after [`pre_lift`]: every unfanned ring vertex
/// ([`unfan_targets`]) stops being a fan target, and its z is RE-COMPUTED on
/// the plane of the face the EFFECTIVE solve (the fan over only the FANNED
/// vertices) spans across it: the plane through its nearest fanned ring
/// neighbours `u`/`w` (at their current — pre-lifted — z) and the apex. That
/// is exactly the effective fan's `(u, w, apex)` hub face extended over the
/// vertex's sector, so the vertex lands ON the solved slope by construction.
/// The triangulation then emits its usual per-edge hub triangles over the
/// SAME plan ring — any z assignment tiles watertight — and the coplanar
/// merge folds the vertex's facets into that single plane: the surface
/// re-solves as if the vertex weren't a fan target, and the wall below rises
/// (or the plane keeps falling past the eave line, for a corner poking
/// beyond the chord — the "roof continues over the bay" reading).
///
/// Degenerate cases keep the vertex's current z: a vertical chord plane
/// (e.g. a chord endpoint that IS the apex, as happens when re-applying to a
/// face carrying the apex as a corner — the vertex was already embedded on
/// the first pass, so keeping its z is exactly the re-apply identity).
fn lift_unfanned_fan(
    mut ext: Vec<Point3f64>,
    unfanned: &[Point2f64],
    apex: &CeilingApex,
    apex_v: Point3f64,
) -> Vec<Point3f64> {
    let Some(target) = unfan_targets(&ext, unfanned, &[apex]) else {
        return ext;
    };
    let n = ext.len();
    let orig = ext.clone();
    for i in 0..n {
        if !target[i] {
            continue;
        }
        // Nearest fanned neighbours, walking the ring each way.
        let mut j = (i + n - 1) % n;
        while target[j] {
            j = (j + n - 1) % n;
        }
        let mut k = (i + 1) % n;
        while target[k] {
            k = (k + 1) % n;
        }
        if let Some(z) = plane_z_at(orig[j], orig[k], apex_v, ext[i].x, ext[i].y) {
            ext[i].z = z;
        }
    }
    ext
}

/// The UNFAN EMBED for a NON-GABLE (hip / interior-endpoint) ridge: like
/// [`lift_unfanned_fan`], but the effective solve's face across an unfanned
/// vertex depends on the ridge MIDLINE. With fanned neighbours `u`/`w`, the
/// effective ridge solve splits the chord `u→w` at the midline crossing `m`
/// (z interpolated along the chord) and spans `(u′, m, hub A)` on A's side
/// and `(m, w′, hub B)` on B's — the vertex embeds on the plane of the
/// sub-face on ITS side of the midline (by its own ridge parameter). A chord
/// that doesn't cross keeps one plane `(u, w, hub)`. Vertical/degenerate
/// planes keep the current z (the re-apply-identity posture).
fn lift_unfanned_hip_ridge(
    mut ext: Vec<Point3f64>,
    unfanned: &[Point2f64],
    a: &CeilingApex,
    b: &CeilingApex,
) -> Vec<Point3f64> {
    let Some(target) = unfan_targets(&ext, unfanned, &[a, b]) else {
        return ext;
    };
    let (dx, dy) = (b.point.x - a.point.x, b.point.y - a.point.y);
    let d2 = dx * dx + dy * dy;
    let a3 = Point3f64::new(a.point.x, a.point.y, a.z_m);
    let b3 = Point3f64::new(b.point.x, b.point.y, b.z_m);
    if d2 < 1e-12 {
        // Degenerate ridge — the caller will fan to `a`; embed like a fan.
        return lift_unfanned_fan(ext, unfanned, a, a3);
    }
    let t_of = |p: &Point3f64| ((p.x - a.point.x) * dx + (p.y - a.point.y) * dy) / d2;
    let n = ext.len();
    let orig = ext.clone();
    for i in 0..n {
        if !target[i] {
            continue;
        }
        let mut j = (i + n - 1) % n;
        while target[j] {
            j = (j + n - 1) % n;
        }
        let mut k = (i + 1) % n;
        while target[k] {
            k = (k + 1) % n;
        }
        let (u, w) = (orig[j], orig[k]);
        let (tu, tw, tv) = (t_of(&u), t_of(&w), t_of(&orig[i]));
        let v_is_b = tv >= 0.5;
        let (p0, p1, h) = if (tu - 0.5) * (tw - 0.5) < 0.0 {
            // The chord crosses the midline: embed on the sub-face of the
            // vertex's own side.
            let s = (0.5 - tu) / (tw - tu);
            let m = u + s * (w - u);
            let u_is_b = tu >= 0.5;
            if v_is_b == u_is_b {
                (u, m, if v_is_b { b3 } else { a3 })
            } else {
                (m, w, if v_is_b { b3 } else { a3 })
            }
        } else {
            (u, w, if v_is_b { b3 } else { a3 })
        };
        if let Some(z) = plane_z_at(p0, p1, h, ext[i].x, ext[i].y) {
            ext[i].z = z;
        }
    }
    ext
}

/// Distance tie tolerance (m) for the gable anchor election and side
/// classification — same order as [`APEX_BOUNDARY_EPS_M`]: real eave walls
/// differ by centimetres or more; boolean noise by ~1e-7.
pub(crate) const GABLE_DIST_EPS_M: f64 = 1e-4;

/// Per-vertex classification of a TRUE-GABLE ridge over a plan ring — the
/// zero-taps automatic rule behind "a gable is just two planes":
///
/// Detection: BOTH ridge endpoints lie on the ring's boundary (within
/// [`APEX_BOUNDARY_EPS_M`] — the assign-time gable-end snap puts them exactly
/// on). Otherwise `None`, and the ridge keeps the hip/fan solve.
///
/// Classification, per ring vertex:
/// - `side`: signed perpendicular offset from the ridge LINE (m).
/// - `eligible`: the vertex is CONVEX and owns at least one NON-GABLE
///   boundary edge. REFLEX (concave) corners — the armpits of a T / notch
///   plan — are never anchors (and never take an override): an eave plane
///   meets base at a WALL, and an armpit can sit strictly nearer the ridge
///   than any real eave (a reflex anchor would pitch the plane so steeply
///   the far wall plunges toward the floor — the test2 field report).
///   Vertices whose every incident edge is part of a vertical gable END
///   (edges collinear with their hub — e.g. an extra vertex on the gable
///   wall) never anchor either; their z never surfaces (their faces are
///   all slivers).
/// - `anchors`: ONE anchor per side — the eave vertex the side's slope
///   plane is pitched to. AUTO election picks the side's DOMINANT EAVE:
///   among the side's ridge-PARALLEL non-gable boundary edges (endpoint
///   distances equal within [`GABLE_DIST_EPS_M`] — real eave runs), band
///   them by distance and take the band with the LARGEST summed length
///   (tie → the nearer band), then the lex-smallest `(x, y)` eligible
///   vertex at that distance (deterministic — solve and markers must agree
///   on which tied corner anchors). So a T with the ridge down its stem
///   pitches on the long MAIN walls (the stem tucks under the plane, its
///   corners embedding above base), not on the short stem walls nearer the
///   ridge. A side with no parallel eave run — or none carrying an eligible
///   vertex — falls back to the NEAREST eligible vertex. A point of
///   `anchor_overrides` (the storey's durable `ceiling_plane_anchors`,
///   matched within [`UNFAN_MATCH_EPS_M`]) landing on an ELIGIBLE vertex of
///   a side OVERRIDES the auto election — the user's "this corner sets the
///   pitch"; several matches on one side take the nearest of them (warned);
///   a stale override matching nothing is ignored.
/// - `fanned`: exactly the anchors — every other vertex EMBEDS on its side
///   plane ([`ridge_triangles`]). Note the stored UNFANNED set does NOT
///   apply on gable sides (the old tap-to-pin made kinks; anchor selection
///   replaced it) — it still drives hip/fan groups unchanged.
pub(crate) struct GablePlanClass {
    pub side: Vec<f64>,
    pub eligible: Vec<bool>,
    pub fanned: Vec<bool>,
    /// The ring index of each side's current anchor, `[negative, positive]`
    /// by side sign; `None` for an empty (or anchor-less) side.
    pub anchors: [Option<usize>; 2],
    /// Whether each side's anchor came from a matched override (vs auto).
    pub overridden: [bool; 2],
}

pub(crate) fn gable_plan_classify(
    ring: &[Point2f64],
    a: Point2f64,
    b: Point2f64,
    anchor_overrides: &[Point2f64],
) -> Option<GablePlanClass> {
    let n = ring.len();
    if n < 3 {
        return None;
    }
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let d2 = dx * dx + dy * dy;
    if d2 < 1e-12 {
        return None;
    }
    let len = d2.sqrt();
    let on_boundary = |p: Point2f64| {
        (0..n).any(|i| {
            let (u, w) = (ring[i], ring[(i + 1) % n]);
            point_to_segment_distance(p.x, p.y, u.x, u.y, w.x, w.y) <= APEX_BOUNDARY_EPS_M
        })
    };
    if !(on_boundary(a) && on_boundary(b)) {
        return None;
    }
    // Signed side offset from the ridge line.
    let side: Vec<f64> = ring
        .iter()
        .map(|v| (dx * (v.y - a.y) - dy * (v.x - a.x)) / len)
        .collect();
    // A GABLE edge: its hub triangle is plan-degenerate (the hub sits on the
    // edge's line — the edge is part of a vertical gable end).
    let edge_gable: Vec<bool> = (0..n)
        .map(|i| {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            let (mx, my) = ((p.x + q.x) / 2.0, (p.y + q.y) / 2.0);
            let t = ((mx - a.x) * dx + (my - a.y) * dy) / d2;
            let h = if t >= 0.5 { b } else { a };
            let cross2 = (q.x - p.x) * (h.y - p.y) - (q.y - p.y) * (h.x - p.x);
            let el = ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
            el <= 1e-9 || cross2.abs() / el <= APEX_BOUNDARY_EPS_M
        })
        .collect();
    // Ring turning sense (shoelace sign) for the convexity test.
    let orient: f64 = (0..n)
        .map(|i| {
            let (p, q) = (ring[i], ring[(i + 1) % n]);
            p.x * q.y - q.x * p.y
        })
        .sum();
    // CONVEX corner: turns with the ring's own sense (reflex armpits and
    // collinear filler vertices don't — neither may anchor an eave plane).
    let convex = |i: usize| {
        let p = ring[(i + n - 1) % n];
        let c = ring[i];
        let q = ring[(i + 1) % n];
        let cr = (c.x - p.x) * (q.y - c.y) - (c.y - p.y) * (q.x - c.x);
        cr * orient > 0.0
    };
    // Vertex i touches edges (i−1) and i.
    let eligible: Vec<bool> = (0..n)
        .map(|i| (!edge_gable[(i + n - 1) % n] || !edge_gable[i]) && convex(i))
        .collect();
    // Anchor election per side: an eligible matched OVERRIDE wins (nearest
    // of several, warned); else the DOMINANT-EAVE auto rule (band the
    // side's ridge-parallel eave edges by distance, longest band wins, tie
    // → nearer), falling back to the nearest eligible vertex. Deterministic
    // tie-break by (distance, x, y).
    let matches_override = |i: usize| {
        anchor_overrides.iter().any(|o| {
            (ring[i].x - o.x).abs() <= UNFAN_MATCH_EPS_M
                && (ring[i].y - o.y).abs() <= UNFAN_MATCH_EPS_M
        })
    };
    let key = |i: usize| (side[i].abs(), ring[i].x, ring[i].y);
    let cmp = |i: &usize, j: &usize| {
        let (da, xa, ya) = key(*i);
        let (db, xb, yb) = key(*j);
        da.total_cmp(&db)
            .then(xa.total_cmp(&xb))
            .then(ya.total_cmp(&yb))
    };
    let mut anchors: [Option<usize>; 2] = [None, None];
    let mut overridden = [false, false];
    for s in [false, true] {
        let side_sign = |i: usize| (side[i] > 0.0) == s && side[i].abs() > GABLE_DIST_EPS_M;
        let on_side = |i: usize| side_sign(i) && eligible[i];
        let hits: Vec<usize> = (0..n)
            .filter(|&i| on_side(i) && matches_override(i))
            .collect();
        if hits.len() > 1 {
            log::warn!(
                "gable: {} plane-anchor overrides match one ridge side — using the nearest",
                hits.len()
            );
        }
        let (choice, is_override) = match hits.iter().copied().min_by(cmp) {
            Some(i) => (Some(i), true),
            None => {
                // DOMINANT EAVE: the side's ridge-parallel non-gable edges,
                // banded by distance; largest summed length wins, tie → the
                // nearer band.
                let mut bands: Vec<(f64, f64)> = Vec::new(); // (dist, total len)
                for i in 0..n {
                    let j = (i + 1) % n;
                    if edge_gable[i] || !side_sign(i) || !side_sign(j) {
                        continue;
                    }
                    if (side[i].abs() - side[j].abs()).abs() > GABLE_DIST_EPS_M {
                        continue; // not ridge-parallel — no eave run
                    }
                    let d = (side[i].abs() + side[j].abs()) / 2.0;
                    let el =
                        ((ring[j].x - ring[i].x).powi(2) + (ring[j].y - ring[i].y).powi(2)).sqrt();
                    match bands
                        .iter_mut()
                        .find(|(bd, _)| (*bd - d).abs() <= GABLE_DIST_EPS_M)
                    {
                        Some((_, l)) => *l += el,
                        None => bands.push((d, el)),
                    }
                }
                let dominant = bands
                    .iter()
                    .copied()
                    .max_by(|(da, la), (db, lb)| la.total_cmp(lb).then(db.total_cmp(da)));
                let banded = dominant.and_then(|(d, _)| {
                    (0..n)
                        .filter(|&i| on_side(i) && (side[i].abs() - d).abs() <= GABLE_DIST_EPS_M)
                        .min_by(cmp)
                });
                (
                    banded.or_else(|| (0..n).filter(|&i| on_side(i)).min_by(cmp)),
                    false,
                )
            }
        };
        anchors[usize::from(s)] = choice;
        overridden[usize::from(s)] = is_override && choice.is_some();
    }
    let fanned: Vec<bool> = (0..n)
        .map(|i| anchors.iter().any(|a| *a == Some(i)))
        .collect();
    Some(GablePlanClass {
        side,
        eligible,
        fanned,
        anchors,
        overridden,
    })
}

/// The public per-vertex ROLES of a TRUE-GABLE ring (both ridge endpoints on
/// the ring boundary), or `None` for any other ridge — the document layer's
/// marker/tap-routing view, index-parallel to `ring`, built from the SAME
/// classification the solve uses ([`gable_plan_classify`]) so the markers
/// show exactly what the solve does.
pub struct GableRingRoles {
    /// This vertex is a side's CURRENT pitch anchor (auto or override).
    pub anchor: Vec<bool>,
    /// This vertex may anchor a slope (CONVEX, and owns a non-gable eave
    /// edge — reflex armpits and gable-wall extras never anchor).
    pub eligible: Vec<bool>,
    /// The vertex's embedded z DIFFERS from its own base — the roof
    /// genuinely deviates over it (its ridge distance differs from its
    /// side's anchor distance). Anchors never deviate.
    pub deviates: Vec<bool>,
    /// Any side's anchor currently comes from a stored override.
    pub overridden: bool,
}

pub fn gable_ring_roles(
    ring: &[Point2f64],
    a: Point2f64,
    b: Point2f64,
    anchor_overrides: &[Point2f64],
) -> Option<GableRingRoles> {
    let cls = gable_plan_classify(ring, a, b, anchor_overrides)?;
    let n = ring.len();
    let deviates = (0..n)
        .map(|i| {
            if cls.fanned[i] {
                return false;
            }
            let s = usize::from(cls.side[i] > 0.0);
            match cls.anchors[s] {
                Some(ai) => (cls.side[i].abs() - cls.side[ai].abs()).abs() > GABLE_DIST_EPS_M,
                None => false,
            }
        })
        .collect();
    Some(GableRingRoles {
        anchor: cls.fanned.clone(),
        eligible: cls.eligible.clone(),
        deviates,
        overridden: cls.overridden.iter().any(|o| *o),
    })
}

/// The TRUE-GABLE embed (both ridge endpoints on the region boundary): each
/// side of the ridge is ONE PLANE through the ridge line and that side's
/// ANCHOR ([`gable_plan_classify`] — the nearest eligible eave vertex, or
/// the user's stored override) at its own z: the slope meets base height at
/// the anchor wall and CONTINUES over everything else (the assessor's "one
/// of them extends a bit more over the bay"). Every non-anchor vertex
/// embeds at the side plane's z over its plan position — INCLUDING corners
/// closer to the ridge than an overridden anchor, which then embed ABOVE
/// base (the roof-void wall above them fills to the roof). The hub
/// triangulation then runs unchanged; every triangle (and every midline
/// split point, interpolated along on-plane edges) lies on its side plane,
/// so the coplanar merge folds each side into ONE face — the classic
/// two-plane gable, bays and notches embedded.
fn embed_gable_sides(
    mut ext: Vec<Point3f64>,
    cls: &GablePlanClass,
    a: &CeilingApex,
    b: &CeilingApex,
) -> Vec<Point3f64> {
    // NOISE ABSORBER: `is_hub` matches a ring vertex to the gable apexes a/b
    // (their plan points are copied into the ring). 1e-4 per the apex-corner
    // convention ([`APEX_BOUNDARY_EPS_M`]) — never fuses distinct corners.
    const EPS_M: f64 = 1e-4;
    let a3 = Point3f64::new(a.point.x, a.point.y, a.z_m);
    let b3 = Point3f64::new(b.point.x, b.point.y, b.z_m);
    let n = ext.len();
    let is_hub = |v: &Point3f64| {
        [(a.point), (b.point)]
            .iter()
            .any(|p| (v.x - p.x).abs() <= EPS_M && (v.y - p.y).abs() <= EPS_M)
    };
    for s in [false, true] {
        let on_side = |i: usize| (cls.side[i] > 0.0) == s && cls.side[i].abs() > GABLE_DIST_EPS_M;
        let Some(ai) = cls.anchors[usize::from(s)] else {
            continue; // an empty / anchor-less side — nothing to pitch
        };
        let (p0, p1, p2) = (a3, b3, ext[ai]);
        for i in 0..n {
            if !on_side(i) || i == ai || is_hub(&ext[i]) {
                continue;
            }
            if let Some(z) = plane_z_at(p0, p1, p2, ext[i].x, ext[i].y) {
                // A vertex ALREADY on the plane keeps its exact z: a co-nearest
                // eave corner (same distance as the anchor) or a re-applied,
                // previously-embedded corner must round-trip bit-identically —
                // the re-apply/reload fixpoint; plane evaluation would smear
                // ~1e-16 over it.
                if (z - ext[i].z).abs() > 1e-9 {
                    ext[i].z = z;
                }
            }
        }
    }
    ext
}

/// D2 step 0 — PRE-LIFT: any ring vertex plan-matching one of `apexes`
/// (within 1e-4 m, the `fan_base_z` predicate) takes that apex's z.
/// Load-bearing for the re-apply-to-self identity: a fan/ridge triangle fed
/// back through `with_apexes` carries the apexes as ring CORNERS, and the
/// emitted hub/transition triangles keep the (lifted) boundary z — without
/// the lift a re-emitted corner could revert to a stale height.
fn pre_lift(mut ext: Vec<Point3f64>, apexes: &[&CeilingApex]) -> Vec<Point3f64> {
    // NOISE ABSORBER: matches a ring vertex to an apex plan-corner (an exact
    // copy when the triangle is re-fed through `with_apexes`). 1e-4 per the
    // apex-corner convention — real corners are never that close to an apex.
    const EPS_M: f64 = 1e-4;
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
/// pre-lift, unfan-embed ([`lift_unfanned_fan`] — vertices of the `unfanned`
/// set land on the effective fan's face plane instead of anchoring their
/// own), then one triangle per boundary edge `(vᵢ, vᵢ₊₁, apex)`, dropping
/// sub-[`SLIVER_AREA_M2`] slivers — exactly the degenerate pair incident to
/// an apex that is already a corner. Fresh ids, default `opes`, inherited
/// `construction_id`.
pub(crate) fn fan_triangles(
    region: CeilingRegion,
    apex: &CeilingApex,
    unfanned: &[Point2f64],
) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let exposure = region.exposure;
    let apex_v = Point3f64::new(apex.point.x, apex.point.y, apex.z_m);
    let ext = pre_lift(region.polygon.exterior, &[apex]);
    let ext = lift_unfanned_fan(ext, unfanned, apex, apex_v);
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
    a: &CeilingApex,
    b: &CeilingApex,
    unfanned: &[Point2f64],
    plane_anchors: &[Point2f64],
) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let exposure = region.exposure;
    let ext = pre_lift(region.polygon.exterior, &[a, b]);
    // Z ASSIGNMENT, before the midline split (the embed walks/planes must
    // see only REAL ring vertices; the split then interpolates z along the
    // already-embedded edges, staying on their planes):
    // - TRUE GABLE (both endpoints on the region boundary,
    //   [`gable_plan_classify`]): each side embeds onto its single slope
    //   plane ([`embed_gable_sides`]), pitched to the auto-nearest eave
    //   corner or the user's stored plane-anchor override — two planes with
    //   zero taps, bays and notches carried along. The stored UNFANNED set
    //   does not apply here (anchor selection replaced tap-to-pin).
    // - otherwise (hip / interior endpoints): only stored-unfanned vertices
    //   embed, on the effective solve's containing sub-face
    //   ([`lift_unfanned_hip_ridge`]); the anchor set is a gable-only
    //   concern and is ignored.
    let plan_ring: Vec<Point2f64> = ext.iter().map(|v| Point2f64::new(v.x, v.y)).collect();
    let ext = match gable_plan_classify(&plan_ring, a.point, b.point, plane_anchors) {
        Some(cls) => embed_gable_sides(ext, &cls, a, b),
        None => lift_unfanned_hip_ridge(ext, unfanned, a, b),
    };
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
            // The ring above is already unfan-lifted (a ≈ b, so the hub
            // choice was moot) — don't lift twice.
            &[],
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
pub fn apexes_form_rect(apexes: &[&CeilingApex]) -> Option<(Point2f64, Point2f64)> {
    // NOISE ABSORBER: the four apexes of a flat-topped hip come from the
    // rect flow, all set to ONE typed height, so their z's are exactly equal
    // (one f32-widened value). 1e-4 swallows any rounding while staying far
    // below a real height difference (apex heights are typed on the cm grid).
    const Z_EPS: f64 = 1e-4;
    if apexes.len() != 4 {
        return None;
    }
    let z0 = apexes[0].z_m;
    if apexes.iter().any(|a| (a.z_m - z0).abs() > Z_EPS) {
        return None;
    }
    let pts: Vec<Point2f64> = apexes
        .iter()
        .map(|a| CeilingApex::quantize_point(a.point))
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
    corners: [&CeilingApex; 4],
) -> Vec<CeilingRegion> {
    let cid = region.construction_id;
    let (mut x0, mut y0) = (f64::INFINITY, f64::INFINITY);
    let (mut x1, mut y1) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for a in &corners {
        let q = CeilingApex::quantize_point(a.point);
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
                let q = CeilingApex::quantize_point(a.point);
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
    let mut ext = pre_lift(region.polygon.exterior, &corners[..]);
    // WINDING NORMALISATION — load-bearing, not tidiness. Each side face below
    // is closed back along its rect side in REVERSE (`S_b` then `S_a`) against
    // the canonical CCW corner ring `cs`. That keeps the face simple only while
    // the boundary run itself advances CCW. Fed a CLOCKWISE ring the run walks
    // the other way, the closing pair no longer reverses it, and EVERY side
    // face comes out a BOW-TIE — self-intersecting, with the hips drawn from
    // each boundary corner to the FAR rect corner instead of its nearest.
    //
    // Ring winding is not something the model guarantees: it follows the
    // direction the assessor happened to draw the storey outline, so the same
    // flat top worked or produced crossed faces depending on which way round
    // the plan was sketched (live session, 5×5 storey with a 3 m plateau: the
    // three side faces came out at |area| 0.5/1.0/0.5 m² instead of
    // 4.5/4.0/4.5, covering 14 m² of a 25 m² storey).
    //
    // Normalising HERE rather than at the callers keeps the guarantee local to
    // the one algorithm that needs it. The flat TOP is already emitted from
    // `cs` (always CCW) whatever the input winding, so the output was mixed
    // already — this makes the side faces agree with it.
    if ring_signed_plan_area(&ext) < 0.0 {
        ext.reverse();
    }
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

/// Vertex / plane tolerance for the coplanar face merge, in metres — a pure
/// NOISE ABSORBER: the faces it fuses are fan/ridge triangles the SAME solve
/// built over the SAME apex/base points, so a shared edge's vertices are
/// bit-identical copies and a genuinely-coplanar pair deviates only by f64
/// Newell-fit rounding (~1e-13). 1e-4 (0.1 mm) sits comfortably above that
/// and above the f32 building-scale grid (~1e-6 at 10 m coords), while
/// staying far below any real FOLD: two faces that should stay separate meet
/// at a slope, and the shallowest slope the model can create is an apex
/// [`MIN_APEX_DELTA_M`] (1 cm) above its base — a plane deviation of order
/// cm across the face, ~100× above 1e-4, so no real fold can be fused.
///
/// [`MIN_APEX_DELTA_M`]: (document::ceiling_point) the 1 cm minimum apex rise.
const MERGE_EPS_M: f64 = 1e-4;

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
        // A strip carved WALL-TO-WALL (its edge exactly on a region side)
        // leaves a self-touching EXTERIOR ring with a zero-width neck — one
        // vertex visited twice — and NO interior hole, so the donut path below
        // never fires. Decompose it at the pinch into two simple loops.
        if let Some(pieces) = split_exterior_pinch(poly) {
            return pieces.iter().flat_map(split_to_simple).collect();
        }
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

/// Decompose a SELF-TOUCHING exterior ring — a pinch / zero-width neck where a
/// single vertex is visited twice — into the two loops that meet at that
/// vertex. Returns `None` when the ring is already simple (no repeated vertex,
/// as every well-formed boolean-op piece is) or when a clean two-loop split
/// isn't found, so the caller keeps the input unchanged rather than emit
/// garbage.
///
/// The pinch arises when a carved strip's edge lands EXACTLY on a region side
/// (a strip spanning a bay wall-to-wall): `geo`'s `difference` then returns the
/// exposed remainder as ONE ring that walks up the shared wall, across the
/// strip, and back down the same wall — the up- and down-runs coincide in a
/// zero-width neck pinned at the shared vertex. A simple polygon never repeats
/// an exterior vertex, so a repeat is an unambiguous pinch signal.
///
/// Splitting at the pinch `v_a == v_b` (a < b) yields the inner loop `v[a..b]`
/// and the outer loop `v[b..] ++ v[..a]`; both close back to the shared vertex.
/// Each REAL loop is re-run through [`split_to_simple`] by the caller so a
/// multiply-pinched ring fully decomposes. A loop that isn't a real polygon
/// (< 3 vertices or sub-[`SLIVER_AREA_M2`] area) is the zero-width neck artefact
/// of the pinch and is dropped, not returned — a real strip is still filtered
/// by the caller's thickness guard, so dropping only the area-less neck here is
/// safe. `None` when no pinch is found (the ring is already simple) or when
/// NEITHER loop is real (a wholly degenerate ring), so the caller keeps the
/// input unchanged rather than emit garbage.
fn split_exterior_pinch(poly: &Polygon<f64>) -> Option<Vec<Polygon<f64>>> {
    use geo::{Area, Coord};
    // Open ring: drop the closing duplicate `geo` appends.
    let mut pts: Vec<Coord<f64>> = poly.exterior().0.clone();
    if pts.len() > 1 && pts.first() == pts.last() {
        pts.pop();
    }
    let n = pts.len();
    if n < 4 {
        return None; // a triangle can't carry a meaningful pinch
    }
    // Pinch vertices are copied bit-for-bit through the boolean op (same shared
    // corner), so an exact-equality match is the intent; a hair of tolerance
    // guards f64 round-off without risking a false capture of distinct corners
    // (real ring vertices are centimetres apart).
    const EPS: f64 = 1e-9;
    let same =
        |p: &Coord<f64>, q: &Coord<f64>| (p.x - q.x).abs() <= EPS && (p.y - q.y).abs() <= EPS;
    let mut pinch: Option<(usize, usize)> = None;
    'outer: for a in 0..n {
        for b in (a + 1)..n {
            if same(&pts[a], &pts[b]) {
                pinch = Some((a, b));
                break 'outer;
            }
        }
    }
    let (a, b) = pinch?;
    let inner: Vec<Coord<f64>> = pts[a..b].to_vec();
    let mut outer: Vec<Coord<f64>> = pts[b..].to_vec();
    outer.extend_from_slice(&pts[..a]);
    let mk = |ring: Vec<Coord<f64>>| Polygon::new(LineString::new(ring), vec![]);
    let real: Vec<Polygon<f64>> = [inner, outer]
        .into_iter()
        .filter(|ring| ring.len() >= 3)
        .map(mk)
        .filter(|p| p.unsigned_area() > SLIVER_AREA_M2)
        .collect();
    if real.is_empty() {
        return None;
    }
    Some(real)
}

#[cfg(test)]
mod strip_collinear_tests {
    use super::strip_collinear;
    use domain::points::Point2f64;

    // A vertex a fraction of a mm off the straight top edge — the shape f32
    // quantization leaves when a storey-below corner lands on this outline's
    // straight run — must be dropped, so it can't split that wall into two
    // regions. (Its cross ~8e-4 cleared the old 1e-9 tolerance.)
    #[test]
    fn drops_near_collinear_vertex() {
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(8.0, 0.0),
            Point2f64::new(8.0, 5.0),
            Point2f64::new(4.0, 5.0001), // ~0.1 mm off the top edge
            Point2f64::new(0.0, 5.0),
        ];
        assert_eq!(
            strip_collinear(&ring).len(),
            4,
            "near-collinear top-edge vertex must be stripped"
        );
    }

    // A genuine corner (a 2 m bump) is kept — the tolerance is far below any
    // real bend.
    #[test]
    fn keeps_real_corner() {
        let ring = vec![
            Point2f64::new(0.0, 0.0),
            Point2f64::new(8.0, 0.0),
            Point2f64::new(8.0, 5.0),
            Point2f64::new(4.0, 7.0), // real bump
            Point2f64::new(0.0, 5.0),
        ];
        assert_eq!(
            strip_collinear(&ring).len(),
            5,
            "a real corner must be kept"
        );
    }
}
