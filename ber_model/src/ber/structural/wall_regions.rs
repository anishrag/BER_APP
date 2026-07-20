use geo::{LineString, Point as GeoPoint, Polygon};
use serde::{Deserialize, Serialize};

use crate::ber::common::ItemId;
use domain::points::{segment_cross_param, Point2f64, Point3f64, Vector3f64};

#[allow(unused_imports)]
use super::*;

/// A vertical planar wall region — pure geometry + exposure + openings. Its
/// fabric (construction type/make-up/phase, photos, description) lives on the
/// `WallConstruction` referenced by `construction_id`; exposure stays here so a
/// single construction can be split into exposed / semi-exposed sections.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WallRegion {
    pub id: ItemId,
    pub polygon: Polygon3D, // 3D polygon (vertical plane, typically 4 vertices)
    pub exposure: WallExposure,
    pub opes: Opes,
    pub construction_id: ItemId,
}

impl WallRegion {
    /// Midpoint of the wall's base edge (footprint), in plan (2D). Used to
    /// hit-test which construction-phase area the wall falls in. `None` for a
    /// degenerate polygon with fewer than two vertices.
    pub fn base_midpoint(&self) -> Option<Point2f64> {
        let (a, b) = self.base_edge()?;
        Some(Point2f64::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5))
    }

    /// Index where the wall's TOP profile begins in `exterior` — equivalently
    /// the BASE profile's vertex count. The ring is `[base a→b][top b→a]`: the
    /// base traces the FLOOR steps (`floor_profile_base`) along `a`→`b`, the
    /// top traces the ROOF (`ceiling_profile_top`) back `b`→`a`. A flat base is
    /// exactly `[a, b]` (the legacy 2-vertex contract), so this returns 2 —
    /// `top_*` readers stay byte-for-byte identical there; a STEPPED base
    /// inserts the step breakpoints (and a vertical jump) before `b`, pushing
    /// the index up.
    ///
    /// THE SEAM is the turn at the far base endpoint `b`: the base walks
    /// FORWARD along the footprint line (along-distance from `a` non-
    /// decreasing — flat spans advance, a step's vertical jump repeats the
    /// plan point), then the top turns and walks BACK toward `a` (along-
    /// distance decreasing). So the base is the leading run of non-decreasing
    /// along-distance; the top starts at the first vertex whose along-distance
    /// DROPS. (A coplanar-flush merged host — the L-shape of
    /// `merge_coplanar_flush` — has its guest spliced into the TOP edges, past
    /// the seam, so this still cuts at the original footprint `b`; see
    /// [`base_edge`], which clamps to the host base for such rings.)
    /// Returns 2 for any ring too short to split.
    ///
    /// [`base_edge`]: WallRegion::base_edge
    pub(crate) fn top_start_index(&self) -> usize {
        let e = &self.polygon.exterior;
        if e.len() <= 2 {
            return e.len();
        }
        let a = e[0];
        let along = |v: &Point3f64| (v.x - a.x).hypot(v.y - a.y);
        // The base is the LEADING run of non-decreasing along-distance (flat
        // spans advance, a step's vertical jump repeats the plan point); the
        // top then turns BACK toward `a` (along-distance drops). `b` is the
        // FIRST vertex in that run at the run's maximum along-distance — the
        // far footprint corner. The base/top vertical seam at `b` is plan-
        // coincident (the top's first vertex repeats `b`'s plan), so it sits
        // at the same along-distance and is NOT counted into the base; the
        // top starts there. A flat base's run is just `[a, b]` → returns 2.
        let mut run_end = 1; // last index still in the non-decreasing run
        let mut prev = along(&e[1]);
        for (off, v) in e[2..].iter().enumerate() {
            let d = along(v);
            if d >= prev - 1e-9 {
                run_end = off + 2;
                prev = d;
            } else {
                break;
            }
        }
        let maxd = along(&e[run_end]);
        // First index at the run's max along-distance = `b`'s index.
        let bi = (1..=run_end)
            .find(|&i| along(&e[i]) >= maxd - 1e-9)
            .unwrap_or(1);
        bi + 1
    }

    /// The base edge endpoints `(a, b)` in plan — the FULL footprint edge,
    /// robust to a stepped base AND to a coplanar-flush merged host. `None`
    /// for a degenerate wall.
    ///
    /// The footprint `b` is the base vertex at maximum along-distance whose z
    /// sits in the FLOOR band (within [`MAX_FLOOR_STEP_M`] of the base z) — a
    /// merged guest that continues the wall LINE past `b` (the L-shape's
    /// forward arm) is excluded because it rises out of the floor band, so a
    /// merged wall's base edge stays its original footprint, keeping the amend
    /// match stable across re-derivation. For a clean fresh ring this is just
    /// the last base vertex (`exterior[top_start_index() - 1]`).
    pub fn base_edge(&self) -> Option<(Point3f64, Point3f64)> {
        let e = &self.polygon.exterior;
        let k = self.top_start_index();
        if e.len() < 2 || k < 2 {
            return None;
        }
        let a = e[0];
        // The floor band: base z (at `a`) ± the largest plausible floor step.
        // A merged guest's forward arm sits well above this (it rises toward
        // the ceiling), so restricting `b` to the band drops it.
        let lo = a.z - MAX_FLOOR_STEP_M;
        let hi = a.z + MAX_FLOOR_STEP_M;
        let along = |v: &Point3f64| (v.x - a.x).hypot(v.y - a.y);
        let b = e[1..k]
            .iter()
            .filter(|v| v.z >= lo && v.z <= hi)
            .fold(
                e[1],
                |best, v| {
                    if along(v) > along(&best) {
                        *v
                    } else {
                        best
                    }
                },
            );
        Some((a, b))
    }

    /// World→wall-local frame for the base edge: `(origin = exterior[0] in plan,
    /// u_hat = unit vector along the base toward the FAR base endpoint, length,
    /// base_z)`. `None` for a degenerate wall (base edge under 1e-9 m). The
    /// opening model parameterises a wall placement as a centre distance along
    /// `u_hat` from `origin` plus a sill above `base_z`. `base_z` is the z at
    /// `origin` (`exterior[0]`); over a stepped base the local z varies — the
    /// per-along base z is not needed by openings (sills clamp under the top),
    /// and the frame's job is the plan parameterisation a→b.
    pub fn base_frame(&self) -> Option<(Point2f64, (f64, f64), f64, f64)> {
        let (a, b) = self.base_edge()?;
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-9 {
            return None;
        }
        Some((Point2f64::new(a.x, a.y), (dx / len, dy / len), len, a.z))
    }

    /// Invert the side-elevation projection for this wall: the along-distance
    /// (metres from the base origin toward the far endpoint) of a composite
    /// elevation `x`, given the side's view axis `h` (see
    /// [`ExternalSide::view_axis`](crate::ber::common::ExternalSide::view_axis)).
    /// The forward projection in [`Ber::side_elevation`](crate::ber::Ber::side_elevation)
    /// is `x = h·origin + along·(h·u_hat)`; this solves it for `along`. `None`
    /// for a degenerate wall, or one whose edge is parallel to the view axis
    /// (`h·u_hat ≈ 0` — which by construction is never a facing wall). The drag
    /// inverse lives here, next to the forward projection's `base_frame`.
    pub fn elevation_along_at(&self, h: (f64, f64), x: f64) -> Option<f64> {
        let (origin, (ux, uy), _len, _base_z) = self.base_frame()?;
        let denom = h.0 * ux + h.1 * uy;
        if denom.abs() < 1e-9 {
            return None;
        }
        Some((x - (h.0 * origin.x + h.1 * origin.y)) / denom)
    }

    /// The highest sill (height above `base_z`) an opening of `height_m` can have
    /// at along-distance `along_m` and still fit UNDER the wall top
    /// ([`top_z_at_along`](Self::top_z_at_along)) — i.e. `top_z - base_z -
    /// height`, clamped to ≥ 0. Keeps a dragged opening from poking above the
    /// roofline. (0 for a wall shorter than the opening: best effort.)
    pub fn max_sill_for(&self, along_m: f64, height_m: f64) -> f64 {
        let base_z = self.base_frame().map(|(_, _, _, bz)| bz).unwrap_or(0.0);
        (self.top_z_at_along(along_m) - base_z - height_m).max(0.0)
    }

    /// The lowest sill (height above `base_z`) an opening may have at
    /// along-distance `along_m` so its BOTTOM (= `base_z + sill`) sits at/above
    /// the wall's UNDERSIDE ([`base_z_at_along`](Self::base_z_at_along)) there —
    /// i.e. `base_z_at_along - base_z`, clamped to ≥ 0. The MIRROR of
    /// [`max_sill_for`](Self::max_sill_for): where `max_sill_for` LOWERS a tall
    /// opening to keep its top under the roofline, this RAISES an opening off a
    /// SLOPED underside (a sloped inter-storey-split remainder wall, whose base
    /// runs the neighbour roof profile) so it can't poke below the wall into the
    /// buried piece beneath.
    ///
    /// 0 wherever the underside sits at/below `base_z` (`base_z` is the z at
    /// `exterior[0]`) — so a FLAT base, and any span that steps/slopes DOWN from
    /// the start, is unchanged from the pre-clamp behaviour. Where the base steps
    /// or slopes UP from `exterior[0]` this is > 0, correctly raising a low-sill
    /// opening off the higher underside (a window shouldn't sit below a stepped-up
    /// floor); no existing test relied on the old under-floor placement there.
    pub fn min_sill_for(&self, along_m: f64) -> f64 {
        let base_z = self.base_frame().map(|(_, _, _, bz)| bz).unwrap_or(0.0);
        (self.base_z_at_along(along_m) - base_z).max(0.0)
    }

    /// The wall's TOP profile in wall-local coordinates: each vertex of the top
    /// polyline (`exterior[2..]`, running b→a per the vertex-order contract) as
    /// `(along, z)` — `along` projected onto the base direction from the base
    /// origin. Empty for a degenerate wall (no base frame). The elevation editor
    /// draws this as the unrolled wall's top edge; it's the same projection
    /// [`top_z_at_along`](Self::top_z_at_along) samples.
    pub fn top_profile(&self) -> Vec<(f32, f32)> {
        let Some((origin, (ux, uy), _len, _base_z)) = self.base_frame() else {
            return Vec::new();
        };
        let mut prof: Vec<(f32, f32)> = self.polygon.exterior[self.top_start_index()..]
            .iter()
            .map(|v| {
                let along = (v.x - origin.x) * ux + (v.y - origin.y) * uy;
                (along as f32, v.z as f32)
            })
            .collect();
        // If the top polyline doesn't return to above `a` (along ≈ 0), its
        // CLOSING edge back to `exterior[0]` (`a`) is part of the top boundary —
        // a ceiling RISER whose base sits at a step and whose top slopes DOWN to
        // `a`. (A normal floor wall has a top vertex directly above `a`, so it
        // already ends at along 0 and needs nothing.) Append `a` at along 0 so
        // that sloped face is drawn (and measured by `top_z_at_along`).
        if prof.last().is_some_and(|&(along, _)| along.abs() > 1e-3) {
            prof.push((0.0, self.polygon.exterior[0].z as f32));
        }
        prof
    }

    /// The top-profile z directly above the base point at along-distance `d`
    /// (0..length), interpolating the top polyline (`exterior[2..]`, which runs
    /// b→a per the vertex-order contract). At a vertical jump (a ceiling-step
    /// crossing — two top vertices at one plan point) the LOWER z is returned,
    /// so an opening clamped under this can never poke through the ceiling.
    /// Falls back to the lowest top vertex when `d` is out of range, else
    /// `base_z`.
    pub(crate) fn top_z_at_along(&self, d: f64) -> f64 {
        let e = &self.polygon.exterior;
        let Some((origin, (ux, uy), _len, base_z)) = self.base_frame() else {
            return e.first().map(|v| v.z).unwrap_or(0.0);
        };
        let k = self.top_start_index();
        if e.len() <= k {
            return base_z;
        }
        let mut tops: Vec<(f64, f64)> = e[k..]
            .iter()
            .map(|v| ((v.x - origin.x) * ux + (v.y - origin.y) * uy, v.z))
            .collect();
        // Close a ceiling-riser's sloped top back down to `a` (along 0) — the
        // same wrap `top_profile` draws, so an opening near `a` clamps under the
        // slope, not the flat top above it.
        if tops.last().is_some_and(|&(along, _)| along.abs() > 1e-9) {
            tops.push((0.0, e[0].z));
        }
        let mut best: Option<f64> = None;
        for w in tops.windows(2) {
            let ((a0, z0), (a1, z1)) = (w[0], w[1]);
            let (lo, hi) = (a0.min(a1), a0.max(a1));
            if d >= lo - 1e-9 && d <= hi + 1e-9 {
                let z = if (a1 - a0).abs() < 1e-9 {
                    z0.min(z1) // vertical jump → lower edge
                } else {
                    z0 + (z1 - z0) * ((d - a0) / (a1 - a0))
                };
                best = Some(best.map_or(z, |b: f64| b.min(z)));
            }
        }
        best.or_else(|| {
            tops.iter()
                .map(|&(_, z)| z)
                .fold(None, |m: Option<f64>, z| Some(m.map_or(z, |b| b.min(z))))
        })
        .unwrap_or(base_z)
    }

    /// The base/underside-profile z directly below the base point at
    /// along-distance `d` (0..length) — the UNDERSIDE analogue of
    /// [`top_z_at_along`](Self::top_z_at_along). Samples the BASE polyline
    /// (`exterior[..top_start_index()]`, running `a`→`b` per the vertex-order
    /// contract). At a vertical jump (a floor-step crossing — two base vertices
    /// at one plan point) the HIGHER z is returned (mirroring the top's LOWER-z
    /// rule), so an opening whose bottom clamps to this can never poke below the
    /// underside. Falls back to `base_z` when `d` is out of the base's range.
    /// For a FLAT base (`[a, b]`, one z) this is the constant `base_z`, so
    /// [`min_sill_for`](Self::min_sill_for) is 0 and flat-wall behaviour is
    /// unchanged; a stepped/sloped base returns the real per-along underside
    /// (which `min_sill_for` then floors at 0 relative to `base_z`).
    pub(crate) fn base_z_at_along(&self, d: f64) -> f64 {
        let e = &self.polygon.exterior;
        let Some((origin, (ux, uy), _len, base_z)) = self.base_frame() else {
            return e.first().map(|v| v.z).unwrap_or(0.0);
        };
        let k = self.top_start_index();
        // The base run is `exterior[..k]` (a→b); project each vertex to
        // (along, z), the same unroll `top_z_at_along` applies to the top.
        let bases: Vec<(f64, f64)> = e[..k.min(e.len())]
            .iter()
            .map(|v| ((v.x - origin.x) * ux + (v.y - origin.y) * uy, v.z))
            .collect();
        let mut best: Option<f64> = None;
        for w in bases.windows(2) {
            let ((a0, z0), (a1, z1)) = (w[0], w[1]);
            let (lo, hi) = (a0.min(a1), a0.max(a1));
            if d >= lo - 1e-9 && d <= hi + 1e-9 {
                let z = if (a1 - a0).abs() < 1e-9 {
                    z0.max(z1) // vertical jump → higher edge (worst clearance)
                } else {
                    z0 + (z1 - z0) * ((d - a0) / (a1 - a0))
                };
                best = Some(best.map_or(z, |b: f64| b.max(z)));
            }
        }
        best.unwrap_or(base_z)
    }

    /// The wall's single TOP z when its top profile is FLAT (all top vertices
    /// within 1e-6 m), else `None` (a sloped/gabled top whose height varies).
    /// The top vertices are `exterior[top_start_index()..]`. Used by the
    /// "copy height" pick to read a flat-topped wall's known height.
    pub fn flat_top_z(&self) -> Option<f64> {
        let top = &self.polygon.exterior[self.top_start_index()..];
        let z0 = top.first()?.z;
        top.iter().all(|v| (v.z - z0).abs() <= 1e-6).then_some(z0)
    }
}

/// Collection of wall regions
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct WallRegions {
    pub regions: Vec<WallRegion>,
}

impl WallRegions {
    /// Re-derive the storey's walls from `segments` (each a footprint edge
    /// `(a, b)` with its thermal `exposure`), tracing each wall's BASE along
    /// the `floors` step profile and its TOP along the `ceilings` profile, and
    /// carrying identity over from the `previous` regions (replaces the
    /// flat-top `rebuild_preserving`).
    ///
    /// Geometry — vertex-order CONTRACT: `exterior[0]` is ALWAYS `a` at its
    /// base z; the BASE profile runs `a`→`b` (`floor_profile_base`) and the
    /// TOP profile appends after it, running `b`→`a` so the ring closes.
    /// [`top_start_index`] marks the seam; for the COMMON single-level base
    /// the base collapses to exactly `[a, b]` (indices 0/1), byte-identical
    /// to the old flat base, so legacy positional readers
    /// ([`base_frame`]/[`base_midpoint`]/exposure recovery) are unaffected.
    /// The BASE follows the `floors`' `level_m` (composed onto `base_z`): a
    /// raised/lowered floor region lifts/drops the base under it, with a
    /// VERTICAL JUMP where a step line crosses the wall (symmetric to a
    /// ceiling step's top jump); a same-level partition seam (a phase split)
    /// leaves the base a straight edge. The TOP follows the `ceilings` profile:
    /// piecewise-LINEAR across each planar roof region (flat pieces and fan
    /// triangles alike), with a VERTICAL JUMP where a ceiling step crosses
    /// the wall. Spans the ceilings don't cover — including when `ceilings` is
    /// empty (ceiling height unset) — top out flat at `fallback_top_z`
    /// (ABSOLUTE z). A flat roof over a flat floor yields the classic
    /// 4-vertex quad: a-bottom, b-bottom, b-top, a-top.
    ///
    /// [`top_start_index`]: WallRegion::top_start_index
    /// [`base_frame`]: WallRegion::base_frame
    /// [`base_midpoint`]: WallRegion::base_midpoint
    ///
    /// Identity: every region is FRESH-minted (`ItemId::default()`) with
    /// `default_construction_id` and empty `opes`; exposure comes from the
    /// segment (the painted sections via `wall_exposure_segments`). Durable
    /// per-wall data is recovered downstream by parametric key, not by an id the
    /// projection tries to keep stable: openings re-home from their `wall_anchor`
    /// (`reanchor_openings`), riser exposure from the `riser_exposure` table
    /// (`stamp_riser_exposure`), wall construction from the `wall_constructions`
    /// store (`stamp_wall_constructions`), and the phase axis is re-derived by
    /// `stamp_wall_phases`. (Before 2026-06 this ran an injective base-edge amend
    /// against the previous regions to carry ids/opes/exposure/construction; that
    /// is retired now nothing durable rides the wall id.)
    ///
    /// Runs on every materialize: O(segments × roof vertices), no caching.
    pub fn regenerate(
        segments: Vec<(Point2f64, Point2f64, WallExposure)>,
        ceilings: &CeilingRegions,
        floors: &FloorRegions,
        base_z: f64,
        fallback_top_z: f64,
        default_construction_id: ItemId,
    ) -> Self {
        let regions = segments
            .into_iter()
            .map(|(a, b, exposure)| {
                // Base profile first (a→b, tracing the floor steps — a flat
                // base collapses to the 2-vertex `[a, b]` of the contract),
                // then the roof-profile top (b→a) to close the ring.
                let mut ext = floor_profile_base(floors, a, b, base_z);
                ext.extend(ceiling_profile_top(ceilings, a, b, fallback_top_z));
                WallRegion {
                    id: ItemId::default(),
                    polygon: Polygon3D::new(ext),
                    exposure,
                    opes: Opes::default(),
                    construction_id: default_construction_id,
                }
            })
            .collect();
        Self { regions }
    }

    /// Divide walls crossed by structure-phase `cuts` (each a plan SEGMENT of a
    /// finalized cut) into two sub-walls along a vertical plane at the crossing,
    /// copying `exposure` + `construction_id` to BOTH halves (only the id is
    /// fresh). A cut that misses a wall's base edge (or grazes an endpoint)
    /// leaves it whole, so a cut spanning two walls splits both. Runs AFTER
    /// [`regenerate`](Self::regenerate) (which re-makes the unsplit perimeter
    /// wall) and BEFORE [`with_risers`](Self::with_risers).
    ///
    /// Identity is STABLE across re-derivation: a re-made full wall's base edge
    /// is ~twice a previous sub-wall's, so it never amends one in `regenerate`
    /// (error ≫ [`MATCH_EPS_M`]); it comes out whole, and here each half recovers
    /// its previous sub-wall's `id` (+ `construction_id`/`exposure`/`opes`, the
    /// durability rule shared with `with_risers`) by the same injective
    /// base-edge match — EXCLUDING ids the untouched perimeter walls already
    /// hold. A first split (no previous half) takes fresh ids; the controller
    /// then re-homes the crossing wall's openings onto those ids at commit.
    pub fn apply_splits(self, cuts: &[[Point2f64; 2]]) -> Self {
        if cuts.is_empty() {
            return self;
        }
        // Geometric split pass: each crossed wall yields two halves, each with
        // the parent's exposure/construction and a fresh id (per
        // `split_wall_by_segment`). Openings re-home onto the correct half from
        // their `wall_anchor` at the resolve tail, so no id recovery is needed.
        let mut regions = self.regions;
        for cut in cuts {
            let mut next: Vec<WallRegion> = Vec::new();
            for wall in regions.into_iter() {
                match split_wall_by_segment(&wall, cut) {
                    Some((h0, h1)) => {
                        next.push(h0);
                        next.push(h1);
                    }
                    None => next.push(wall),
                }
            }
            regions = next;
        }
        Self { regions }
    }

    /// Append derived RISER faces (`faces` pairs each face with the exposure the
    /// caller derived as its DEFAULT — `Unset`). Each becomes a FRESH region with
    /// `default_construction_id` and empty `opes`. A user's riser exposure
    /// override is NOT recovered here: it's stamped afterward from the durable
    /// `riser_exposure` table (`stamp_riser_exposure`), keyed by the riser's base
    /// edge — its parametric identity now the amend is gone.
    ///
    /// THE COPLANAR-FLUSH MERGE runs last ([`merge_coplanar_flush`]): a riser
    /// that is a coplanar continuation of another wall region (same vertical
    /// plane, sharing a positive-length boundary segment) is the same physical
    /// wall, so it merges INTO that region. Only risers that REMAIN separate
    /// reach the output — those the 2D dashed-line picker lists.
    ///
    /// [`merge_coplanar_flush`]: WallRegions::merge_coplanar_flush
    pub fn with_risers(
        mut self,
        faces: Vec<(Polygon3D, WallExposure)>,
        default_construction_id: ItemId,
    ) -> Self {
        let first_riser = self.regions.len();
        for (polygon, exposure) in faces.into_iter().filter(|(p, _)| p.exterior.len() >= 3) {
            self.regions.push(WallRegion {
                id: ItemId::default(),
                polygon,
                exposure,
                opes: Opes::default(),
                construction_id: default_construction_id,
            });
        }
        self.merge_coplanar_flush(first_riser)
    }

    /// THE COPLANAR-FLUSH MERGE (live report, 2026-06: a ceiling-step carve
    /// whose boundary lines CONTINUE perimeter wall planes derived riser
    /// regions that visually split one physical wall in two): a riser region
    /// that is (a) COPLANAR with another wall region — every vertex of its
    /// ring on the host's base line in plan, i.e. the same vertical plane —
    /// and (b) FLUSH with it — the two rings share a positive-length
    /// boundary segment in wall-plane (along, z) coordinates — merges INTO
    /// that region. The host keeps its `id`, BASE EDGE (`exterior[0]`/`[1]`
    /// — the vertex-order contract, so exposure recovery and section
    /// painting keep keying off the unchanged base), `exposure`, `opes` and
    /// `construction_id`; only its polygon grows by the riser's ring
    /// ([`stitch_flush_coplanar`]), so a merged host can be L-shaped
    /// (non-convex). The merged riser's own identity, openings and any
    /// previously-painted exposure are DISCARDED — deliberately: it is one
    /// physical wall, and the host's attribution wins.
    ///
    /// The rule is purely geometric, so it applies to floor and ceiling
    /// risers alike, and it iterates to a fixpoint so CHAINS merge
    /// transitively (a second riser flush with an already-grown host, or
    /// riser-into-riser where no wall is adjacent). Deterministic: risers
    /// are visited in derivation order, and a riser flush with several
    /// hosts prefers a NON-RISER host, tie-broken by the lexicographically
    /// smallest base edge. A riser that touches a coplanar wall at a single
    /// point (no positive-length shared segment) stays a separate region,
    /// and perimeter walls never merge into each other — exposure sections
    /// split over one outline edge stay separate regions.
    fn merge_coplanar_flush(mut self, first_riser: usize) -> Self {
        let mut is_riser: Vec<bool> = (0..self.regions.len()).map(|k| k >= first_riser).collect();
        loop {
            let mut merge: Option<(usize, usize, Polygon3D)> = None;
            'risers: for ri in 0..self.regions.len() {
                if !is_riser[ri] {
                    continue;
                }
                let mut best: Option<(usize, Polygon3D)> = None;
                for hi in 0..self.regions.len() {
                    if hi == ri {
                        continue;
                    }
                    let Some(stitched) =
                        stitch_flush_coplanar(&self.regions[hi].polygon, &self.regions[ri].polygon)
                    else {
                        continue;
                    };
                    let better = match &best {
                        None => true,
                        Some((bi, _)) => match (is_riser[*bi], is_riser[hi]) {
                            (true, false) => true,
                            (false, true) => false,
                            _ => base_lt(&self.regions[hi], &self.regions[*bi]),
                        },
                    };
                    if better {
                        best = Some((hi, stitched));
                    }
                }
                if let Some((hi, stitched)) = best {
                    merge = Some((ri, hi, stitched));
                    break 'risers;
                }
            }
            let Some((ri, hi, stitched)) = merge else {
                break; // no flush riser left — fixpoint reached
            };
            self.regions[hi].polygon = stitched;
            self.regions.remove(ri);
            is_riser.remove(ri);
        }
        self
    }
}

/// The shortest sub-wall a straight cut may leave (m). A cut that would leave a
/// piece shorter than this on either side is rejected as GRAZING an endpoint or
/// an existing near-coincident cut — never a real division. Sized to swallow the
/// ~sub-mm coordinate drift a reload's geo-boolean phase/exposure reconstruction
/// introduces (the reported 0.8 mm sliver) while staying orders of magnitude
/// below any genuine wall segment.
pub(crate) const MIN_SUBWALL_M: f64 = 0.01;

/// Split one wall along the vertical plane where the plan `cut` segment crosses
/// its base edge, via [`Polygon3D::split_by_line`]. Returns the two sub-walls
/// (each with the parent's `exposure`/`construction_id`, a fresh id, empty
/// `opes`), or `None` when the cut misses the base edge, grazes an endpoint, or
/// doesn't actually divide the face. `split_by_line` preserves the base-edge-
/// first vertex contract for a flat base (the common case): a vertical cut of
/// `[a, b, b_top, a_top]` yields `[a, x, x_top, a_top]` and `[x, b, b_top, x_top]`.
pub(crate) fn split_wall_by_segment(
    wall: &WallRegion,
    cut: &[Point2f64; 2],
) -> Option<(WallRegion, WallRegion)> {
    let (a, b) = wall.base_edge()?;
    let t = segment_cross_param(
        Point2f64::new(a.x, a.y),
        Point2f64::new(b.x, b.y),
        cut[0],
        cut[1],
    )?;
    // Reject a cut that would leave a sub-[`MIN_SUBWALL_M`] sliver on either side.
    // The guard is ABSOLUTE, not fractional: two cut lines that should COINCIDE
    // but drift apart on reload (a construction-phase boundary vs an
    // exposure/partition boundary reconstructed by geo booleans, ~sub-mm apart)
    // would otherwise each split the wall and spawn a sub-mm "sliver" wall between
    // them — mis-counting the walls and destabilising `host_wall_at`, so an
    // opening near the boundary re-homes onto the wrong sub-wall. A drifted cut is
    // treated as GRAZING the existing boundary (no split), collapsing the pair to
    // one. No real wall region is ever this short. (A fractional epsilon caught
    // only cuts within microns of an endpoint on a metres-long wall.)
    let wall_len = ((b.x - a.x).powi(2) + (b.y - a.y).powi(2)).sqrt();
    if t.min(1.0 - t) * wall_len < MIN_SUBWALL_M {
        return None; // the cut grazes an endpoint / an existing near-cut — no real split
    }
    let line = SplitLine3D {
        point: Point3f64::new(a.x + t * (b.x - a.x), a.y + t * (b.y - a.y), a.z),
        direction: Vector3f64::new(0.0, 0.0, 1.0), // vertical cut within the wall plane
        normal: Vector3f64::new(0.0, 0.0, 0.0),    // unused by `split_by_line`
    };
    let [h0, h1] = <[Polygon3D; 2]>::try_from(wall.polygon.split_by_line(&line)).ok()?;
    if h0.exterior.len() < 3 || h1.exterior.len() < 3 {
        return None; // one side came out empty — the cut didn't divide the face
    }
    let mk = |polygon: Polygon3D| WallRegion {
        id: ItemId::default(),
        polygon,
        exposure: wall.exposure,
        opes: Opes::default(),
        construction_id: wall.construction_id,
    };
    Some((mk(h0), mk(h1)))
}

/// Split one wall along the in-plane line through plan/height points `p0`→`p1`
/// (both on the wall's plane), the ARBITRARY-DIAGONAL generalisation of
/// [`split_wall_by_segment`] (which only cuts on a VERTICAL plane through the
/// base). Returns one sub-wall per non-degenerate half (≥ 3 vertices) the cut
/// produces — usually two, but one (or even zero) when the line misses or only
/// grazes the face. Each sub-wall copies the parent's `exposure` +
/// `construction_id`, gets a fresh id and empty `opes`; the CALLER re-attributes
/// exposure per half. `p0`/`p1` carry the height (z) directly, so the line may
/// be diagonal across the elevation — the wall-face partition's drawn cut.
pub fn split_wall_by_line(wall: &WallRegion, p0: Point3f64, p1: Point3f64) -> Vec<WallRegion> {
    let dir = p1 - p0;
    let len = (dir.x * dir.x + dir.y * dir.y + dir.z * dir.z).sqrt();
    if len < 1e-9 {
        return vec![wall.clone()]; // degenerate cut — no division
    }
    let line = SplitLine3D {
        point: p0,
        direction: Vector3f64::new(dir.x / len, dir.y / len, dir.z / len),
        normal: Vector3f64::new(0.0, 0.0, 0.0), // unused by `split_by_line`
    };
    wall.polygon
        .split_by_line(&line)
        .into_iter()
        .filter(|poly| poly.exterior.len() >= 3)
        .map(|polygon| WallRegion {
            id: ItemId::default(),
            polygon,
            exposure: wall.exposure,
            opes: Opes::default(),
            construction_id: wall.construction_id,
        })
        .collect()
}

/// ROTATE `region`'s ring so it satisfies the wall vertex-order contract
/// (base-edge first: `exterior[0]` = the base start `a`, walking the base
/// forward, then the top back) with the LOWEST possible base — making a
/// horizontal-cut UPPER piece a first-class wall whose base edge is the full
/// plan span at the cut height.
///
/// `Polygon3D::split_by_line` preserves the contract for the LOWER piece but
/// the upper piece's ring opens ON the cut line (it starts mid-ring with a
/// vertical edge), so `base_edge()` degenerates and the piece can neither
/// host an opening nor recover its painted span. Every cyclic ROTATION of the
/// ring (never a reversal — orientation is preserved) is scored through the
/// real `base_frame()`; the rotation with a valid frame at MINIMAL `base_z`
/// wins (an upper piece's ring also admits an upside-down "base" along its
/// TOP edge — minimal-z rejects it), ties broken by the longest base edge.
/// Returns the region unchanged when no rotation yields a valid frame (a
/// degenerate sliver).
///
/// SCOPE: called on every horizontal-cut output — by the InterStorey
/// derivation AND by the wall-face partition resolve (`document`'s
/// `split_partitioned_wall`). A piece with a degenerate base is invisible to
/// every base-edge consumer (selection marquee, click hit-test, construction/
/// phase stamping, span recovery, opening hosting), so BOTH splitters must
/// normalise. The 1-D section recovery keeps partitioned walls out by the
/// partition RECIPE (`sections_from_wall_regions` skips face-partitioned
/// walls), not by leaving their rings base-less.
pub fn rebase_ring_start(mut region: WallRegion) -> WallRegion {
    let n = region.polygon.exterior.len();
    if n < 3 || region.base_frame().is_some() {
        return region; // degenerate, or already contract-conforming
    }
    let mut best: Option<(f64, f64, usize)> = None; // (base_z, -len, rotation)
    for k in 1..n {
        let mut ring = region.polygon.exterior.clone();
        ring.rotate_left(k);
        let candidate = WallRegion {
            polygon: Polygon3D { exterior: ring },
            ..region.clone()
        };
        if let Some((_, _, len, base_z)) = candidate.base_frame() {
            if best.is_none_or(|(bz, nl, _)| (base_z, -len) < (bz, nl)) {
                best = Some((base_z, -len, k));
            }
        }
    }
    if let Some((_, _, k)) = best {
        region.polygon.exterior.rotate_left(k);
    }
    region
}

/// Half-width (m) of the FLOOR BAND used by [`WallRegion::base_edge`] to tell
/// a wall's base profile apart from a coplanar-flush-merged riser arm that
/// continues the wall LINE: a base vertex sits within this of the base z (a
/// floor STEP), while a merged guest rises toward the ceiling, well above it.
/// 0.6 m comfortably exceeds any real interior floor step (sub-grade
/// thresholds, sunken rooms) yet stays far below the wall-to-ceiling rise, so
/// the band cleanly separates the two. Only `base_edge` uses it — the base
/// GEOMETRY itself is exact (`floor_profile_base`), this only disambiguates a
/// post-MERGE ring whose base is a CONVENTION (the host's footprint).
const MAX_FLOOR_STEP_M: f64 = 0.6;

// ── Shared-boundary riser extraction (private helpers for `riser_faces`) ──

/// Minimum 1D overlap (m) for two near-collinear boundary edges to count as
/// a SHARED plan piece — anything shorter is boolean-op noise, not a real
/// region boundary.
const RISER_MIN_OVERLAP_M: f64 = 1e-3;

/// Z discontinuity (m) above which two surfaces meeting at a plan edge need
/// a riser face between them. At or below it they are the SAME continuous
/// surface — a fan/ridge seam, or floor pieces one phase boundary splits at
/// a single level. Also the line between "same level" and "a real step" for
/// the upper-storey inheritance ([`CeilingRegions::inherited_floor_levels`])
/// and the distinct-level count in `ber::suggest` — one constant so a riser
/// the 3D view draws is exactly a level the suggestion engine counts.
pub(crate) const RISER_Z_EPS_M: f64 = 1e-4;

/// The plan pieces two region boundary rings SHARE: every edge pair that is
/// metrically collinear (each endpoint of one within [`PROFILE_EPS_M`] of
/// the other's line — the rings are boolean-op output, so a conceptually
/// shared edge is never an exact vertex match) whose 1D overlap along the
/// line exceeds [`RISER_MIN_OVERLAP_M`], as the overlap's plan endpoints.
/// Simple rings never retrace themselves, so one pair's pieces are disjoint.
pub(crate) fn shared_plan_pieces(a: &[Point2f64], b: &[Point2f64]) -> Vec<(Point2f64, Point2f64)> {
    let mut out = Vec::new();
    let (na, nb) = (a.len(), b.len());
    if na < 2 || nb < 2 {
        return out;
    }
    for i in 0..na {
        let (p, q) = (a[i], a[(i + 1) % na]);
        let (dx, dy) = (q.x - p.x, q.y - p.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len <= RISER_MIN_OVERLAP_M {
            continue;
        }
        let (ux, uy) = (dx / len, dy / len);
        // Perpendicular distance (m) to / parameter (m) along edge a's line.
        let dist_to_line = |c: Point2f64| ((c.x - p.x) * uy - (c.y - p.y) * ux).abs();
        let t_of = |c: Point2f64| (c.x - p.x) * ux + (c.y - p.y) * uy;
        for j in 0..nb {
            let (r, s) = (b[j], b[(j + 1) % nb]);
            if dist_to_line(r) > PROFILE_EPS_M || dist_to_line(s) > PROFILE_EPS_M {
                continue; // not (near-)collinear — no shared run
            }
            let (t0, t1) = (t_of(r), t_of(s));
            let (lo, hi) = (t0.min(t1).max(0.0), t0.max(t1).min(len));
            if hi - lo <= RISER_MIN_OVERLAP_M {
                continue;
            }
            out.push((
                Point2f64::new(p.x + ux * lo, p.y + uy * lo),
                Point2f64::new(p.x + ux * hi, p.y + uy * hi),
            ));
        }
    }
    out
}

/// One vertical riser face over the plan run `p`→`q`, from a LOWER surface
/// (`lo_*` at each end) up to an upper one (`hi_*`): `exterior[0]`/`[1]` are
/// the base edge along the lower (the `WallRegion` vertex-order contract),
/// then the top traces back `q`→`p`. A top vertex that wouldn't open past
/// [`RISER_Z_EPS_M`] is skipped, so a face whose profiles meet at one end
/// comes out a triangle. `None` when degenerate — a sub-
/// [`RISER_MIN_OVERLAP_M`] plan run or no end opening past the z epsilon
/// (the sub-sliver halves of a crossing split).
pub(crate) fn riser_face(
    p: Point2f64,
    q: Point2f64,
    lo_p: f64,
    hi_p: f64,
    lo_q: f64,
    hi_q: f64,
) -> Option<Polygon3D> {
    let plan_len = ((q.x - p.x).powi(2) + (q.y - p.y).powi(2)).sqrt();
    if plan_len <= RISER_MIN_OVERLAP_M {
        return None;
    }
    if hi_p - lo_p <= RISER_Z_EPS_M && hi_q - lo_q <= RISER_Z_EPS_M {
        return None;
    }
    let mut ext = vec![
        Point3f64::new(p.x, p.y, lo_p),
        Point3f64::new(q.x, q.y, lo_q),
    ];
    if hi_q - lo_q > RISER_Z_EPS_M {
        ext.push(Point3f64::new(q.x, q.y, hi_q));
    }
    if hi_p - lo_p > RISER_Z_EPS_M {
        ext.push(Point3f64::new(p.x, p.y, hi_p));
    }
    Some(Polygon3D::new(ext))
}

// ── Coplanar-flush riser merging (private helpers for `with_risers`) ──────

/// Stitch a flush coplanar `guest` ring into `host` — the geometric half of
/// [`WallRegions::merge_coplanar_flush`]. `Some(merged)` when the guest lies
/// in the host's vertical plane (every vertex of both rings within
/// [`PROFILE_EPS_M`] of the host's base line in plan) and some guest edge
/// runs collinear with a NON-BASE host edge in wall-plane (along, z)
/// coordinates with more than [`RISER_MIN_OVERLAP_M`] of 1D overlap — the
/// "shares a positive-length boundary segment" test; a single-point touch
/// fails it and the rings stay separate. `None` when any gate fails.
///
/// The merged ring is the union boundary, built by splicing: the host ring
/// walked from its base, with a detour around the guest ring where the
/// shared segment sits. The shared segment may be a strict SUB-SEGMENT of
/// either ring's edge (the host's profile edge usually outspans the riser's
/// side edge) — its endpoints are always existing vertices of one of the
/// two rings, so no new coordinates are ever invented and re-derivation is
/// exact. The guest ring is reversed when needed so the two rings traverse
/// the shared segment in opposite directions (two simple polygons on
/// opposite sides of a shared edge), and junction vertices the splice makes
/// collinear (or duplicate) are dropped afterwards — never the base pair.
/// The host's base edge (`exterior[0]`/`[1]`) is preserved verbatim; a
/// guest whose only shared segment lies ON the base edge (a face glued
/// under the base) never merges, keeping the vertex-order contract intact.
fn stitch_flush_coplanar(host: &Polygon3D, guest: &Polygon3D) -> Option<Polygon3D> {
    let he = &host.exterior;
    if he.len() < 3 || guest.exterior.len() < 3 {
        return None;
    }
    // `he[0]`→`he[1]` fixes the host's vertical-plane DIRECTION. A wall
    // footprint is a straight line, so every base vertex is collinear — when
    // the host base is STEPPED, `he[1]` is a step breakpoint, but it lies on
    // the same base line, so the direction (and the plan-offset coplanarity
    // gate below) is unchanged.
    let (a, b) = (he[0], he[1]);
    let (bx, by) = (b.x - a.x, b.y - a.y);
    let base_len = (bx * bx + by * by).sqrt();
    if base_len <= RISER_MIN_OVERLAP_M {
        return None;
    }
    let (ux, uy) = (bx / base_len, by / base_len);
    // COPLANARITY gate: every vertex of BOTH rings on the host's base line
    // in plan — i.e. the rings share one vertical plane.
    let plan_off = |v: &Point3f64| ((v.x - a.x) * uy - (v.y - a.y) * ux).abs();
    if he
        .iter()
        .chain(guest.exterior.iter())
        .any(|v| plan_off(v) > PROFILE_EPS_M)
    {
        return None;
    }
    // Wall-plane coordinates: (metres along the base line, z).
    let tz = |v: &Point3f64| ((v.x - a.x) * ux + (v.y - a.y) * uy, v.z);
    let h_tz: Vec<(f64, f64)> = he.iter().map(tz).collect();
    let nh = he.len();

    // The shared edge pair: a guest edge collinear with a non-base host
    // edge, overlapping it over a positive 1D run. Longest overlap wins
    // (deterministic). `same_dir` notes a guest ring that traverses the
    // shared run in the host's direction and must be reversed.
    let find_shared = |g_tz: &[(f64, f64)]| -> Option<(usize, usize, bool, f64)> {
        let ng = g_tz.len();
        let mut best: Option<(usize, usize, bool, f64)> = None;
        // i starts at 1: pairing across the BASE edge (i == 0) would splice
        // into it, and the base is the vertex-order contract — such a
        // configuration never merges.
        for i in 1..nh {
            let (p, q) = (h_tz[i], h_tz[(i + 1) % nh]);
            let (ex, ez) = (q.0 - p.0, q.1 - p.1);
            let el = (ex * ex + ez * ez).sqrt();
            if el <= RISER_MIN_OVERLAP_M {
                continue;
            }
            let (dx, dz) = (ex / el, ez / el);
            for j in 0..ng {
                let (r, s) = (g_tz[j], g_tz[(j + 1) % ng]);
                let off_r = ((r.0 - p.0) * dz - (r.1 - p.1) * dx).abs();
                let off_s = ((s.0 - p.0) * dz - (s.1 - p.1) * dx).abs();
                if off_r > PROFILE_EPS_M || off_s > PROFILE_EPS_M {
                    continue; // not collinear with the host edge
                }
                let sr = (r.0 - p.0) * dx + (r.1 - p.1) * dz;
                let ss = (s.0 - p.0) * dx + (s.1 - p.1) * dz;
                let overlap = sr.max(ss).min(el) - sr.min(ss).max(0.0);
                if overlap <= RISER_MIN_OVERLAP_M {
                    continue; // disjoint runs, or a single-point touch
                }
                if best.map_or(true, |(_, _, _, o)| overlap > o) {
                    best = Some((i, j, ss > sr, overlap));
                }
            }
        }
        best
    };

    let mut ge: Vec<Point3f64> = guest.exterior.clone();
    let g_tz: Vec<(f64, f64)> = ge.iter().map(tz).collect();
    let (i, mut j, same_dir, _) = find_shared(&g_tz)?;
    if same_dir {
        // Two simple polygons on opposite sides of a shared edge traverse
        // it in OPPOSITE directions — normalize the guest's winding.
        ge.reverse();
        j = (2 * ge.len() - 2 - j) % ge.len();
    }
    let ng = ge.len();
    let g_tz: Vec<(f64, f64)> = ge.iter().map(tz).collect();
    // Host edge frame, and the guest edge's run along it (sr > ss after the
    // normalization above: the guest edge opposes the host's direction).
    let (p, q) = (h_tz[i], h_tz[(i + 1) % nh]);
    let (ex, ez) = (q.0 - p.0, q.1 - p.1);
    let el = (ex * ex + ez * ez).sqrt();
    let (dx, dz) = (ex / el, ez / el);
    let s_of = |c: (f64, f64)| (c.0 - p.0) * dx + (c.1 - p.1) * dz;
    let sr = s_of(g_tz[j]);
    let ss = s_of(g_tz[(j + 1) % ng]);
    debug_assert!(sr > ss, "normalized guest edge opposes the host edge");
    // A guest end that lands on the host edge's endpoint reuses the host
    // vertex; otherwise the guest endpoint splits the host edge (the
    // partial-overlap case — the split point is the guest's own vertex).
    let lo_coincides = ss.abs() <= RISER_MIN_OVERLAP_M;
    let hi_coincides = (sr - el).abs() <= RISER_MIN_OVERLAP_M;

    let mut merged: Vec<Point3f64> = Vec::with_capacity(nh + ng);
    for (k, v) in he.iter().enumerate() {
        merged.push(*v);
        if k == i {
            // Detour around the guest: out to its low end (unless it sits
            // on he[i]), around the ring, back in from its high end.
            if !lo_coincides {
                merged.push(ge[(j + 1) % ng]);
            }
            for step in 2..ng {
                merged.push(ge[(j + step) % ng]);
            }
            if !hi_coincides {
                merged.push(ge[j]);
            }
        }
    }
    // Junction vertices the splice made collinear (or duplicate) drop out —
    // never the base pair (indices 0/1, the vertex-order contract).
    loop {
        let n = merged.len();
        if n <= 3 {
            break;
        }
        match (2..n).find(|&k| collinear_between(merged[k - 1], merged[k], merged[(k + 1) % n])) {
            Some(k) => {
                merged.remove(k);
            }
            None => break,
        }
    }
    Some(Polygon3D::new(merged))
}

/// `b` lies ON the segment `a`→`c` (within 1e-9 m of the line, between the
/// endpoints) — the post-splice junction-vertex cleanup test of
/// [`stitch_flush_coplanar`]. Also true for an exact duplicate (`b == a`).
fn collinear_between(a: Point3f64, b: Point3f64, c: Point3f64) -> bool {
    let u = (c.x - a.x, c.y - a.y, c.z - a.z);
    let v = (b.x - a.x, b.y - a.y, b.z - a.z);
    let ulen = (u.0 * u.0 + u.1 * u.1 + u.2 * u.2).sqrt();
    if ulen <= 1e-12 {
        return true; // a == c — a degenerate corner; drop b
    }
    let cross = (
        u.1 * v.2 - u.2 * v.1,
        u.2 * v.0 - u.0 * v.2,
        u.0 * v.1 - u.1 * v.0,
    );
    let dist = (cross.0 * cross.0 + cross.1 * cross.1 + cross.2 * cross.2).sqrt() / ulen;
    let between = v.0 * (c.x - b.x) + v.1 * (c.y - b.y) + v.2 * (c.z - b.z) >= -1e-12;
    dist <= 1e-9 && between
}

/// Strict lexicographic order on two regions' base edges — the deterministic
/// host tie-break of [`WallRegions::merge_coplanar_flush`].
fn base_lt(a: &WallRegion, b: &WallRegion) -> bool {
    let key = |r: &WallRegion| {
        // Order on the FULL base edge (a, b) so a stepped host sorts by its
        // footprint endpoints, not an interior step breakpoint.
        match r.base_edge() {
            Some((a, b)) => [a.x, a.y, a.z, b.x, b.y, b.z],
            None => {
                let e = &r.polygon.exterior;
                [e[0].x, e[0].y, e[0].z, e[1].x, e[1].y, e[1].z]
            }
        }
    };
    let (ka, kb) = (key(a), key(b));
    for (x, y) in ka.iter().zip(kb.iter()) {
        match x.total_cmp(y) {
            std::cmp::Ordering::Less => return true,
            std::cmp::Ordering::Greater => return false,
            std::cmp::Ordering::Equal => {}
        }
    }
    false
}

// ── Roof-profile sampling (private helpers for `WallRegions::regenerate`) ──

/// Plan-coordinate tolerance (m) of the roof-profile sampling. The derived
/// roof regions are boolean-op output (geo/i_overlay snaps coordinates to an
/// adaptive grid) and round-trip through f32 on canonicalize/reload, so a
/// region boundary that conceptually COINCIDES with a wall base line can sit
/// up to a few 1e-7 m off it. 1e-4 m (0.1 mm) is comfortably above that
/// noise and far below any real architectural feature, so a pick/breakpoint
/// within this band is the same geometry, not a different roof.
pub(crate) const PROFILE_EPS_M: f64 = 1e-4;

/// The plane `z = c0 + cx·x + cy·y` through a planar ceiling region, from its
/// first vertex and the first vertex pair with a non-degenerate plan cross
/// product. Exact for flat pieces (`cx = cy = 0`) and fan triangles alike.
/// `None` for regions degenerate in plan (< 3 vertices or collinear).
pub(crate) fn region_plane(region: &CeilingRegion) -> Option<(f64, f64, f64)> {
    let ext = &region.polygon.exterior;
    if ext.len() < 3 {
        return None;
    }
    let o = ext[0];
    for i in 1..ext.len() {
        for j in (i + 1)..ext.len() {
            let (ux, uy, uz) = (ext[i].x - o.x, ext[i].y - o.y, ext[i].z - o.z);
            let (vx, vy, vz) = (ext[j].x - o.x, ext[j].y - o.y, ext[j].z - o.z);
            let det = ux * vy - uy * vx;
            if det.abs() > 1e-12 {
                let cx = (uz * vy - vz * uy) / det;
                let cy = (vz * ux - uz * vx) / det;
                return Some((o.z - cx * o.x - cy * o.y, cx, cy));
            }
        }
    }
    None
}

/// The (region index, plane) of the first roof region whose plan projection
/// touches `p`. Containment is boundary-INCLUSIVE (`Intersects`): a wall
/// base lies ON the outline edge that bounds its roof region, so the strict
/// predicate would never match. When two regions share the boundary `p` sits
/// on (a ceiling step running ALONG the wall), the first in region order
/// wins — callers sample interval midpoints, which are never at a crossing
/// breakpoint.
///
/// When the EXACT point touches no region, retry NUDGED by [`PROFILE_EPS_M`]
/// perpendicular to the wall (`n` is the caller's unit normal; both sides
/// tried). The derived regions are boolean-op output whose boundary can sit
/// a few 1e-7 m INSIDE the wall line it conceptually coincides with (grid
/// snapping + f32 round-trips — see [`PROFILE_EPS_M`]); without the retry
/// the pick fails along exactly the walls that bound a stepped/ridged roof
/// piece and the caller's fallback silently flattens them to the default
/// height. The exact pick stays first so behaviour on clean geometry — tie
/// order included — is unchanged.
pub(crate) fn covering_roof_plane(
    ceilings: &CeilingRegions,
    p: Point2f64,
    n: (f64, f64),
) -> Option<(usize, (f64, f64, f64))> {
    use geo::Intersects;
    let pick = |q: Point2f64| {
        let gq = GeoPoint::new(q.x, q.y);
        ceilings.regions.iter().enumerate().find_map(|(i, r)| {
            Polygon::<f64>::from(r.polygon.clone())
                .intersects(&gq)
                .then(|| region_plane(r).map(|plane| (i, plane)))
                .flatten()
        })
    };
    pick(p).or_else(|| {
        [1.0, -1.0].iter().find_map(|s| {
            pick(Point2f64::new(
                p.x + s * n.0 * PROFILE_EPS_M,
                p.y + s * n.1 * PROFILE_EPS_M,
            ))
        })
    })
}

/// Parameters t ∈ [0, 1] along the base `a`→`b` where the covering roof
/// region can change: every crossing of a region boundary edge with the base
/// (near-collinear overlapping edges contribute their projected endpoints),
/// plus the ends 0 and 1. Sorted ascending; the caller skips sub-µm spans.
///
/// Tolerances are METRIC ([`PROFILE_EPS_M`]), not exact: the derived roof
/// rings are boolean-op output that can sit a few 1e-7 m off the wall line
/// they conceptually coincide with. An edge whose BOTH endpoints lie within
/// the band counts as collinear-overlapping (the old exact cross-product
/// test missed displaced eaves edges); a crossing edge may start/stop up to
/// [`PROFILE_EPS_M`] (in metres, scaled to its own parameter) short of the
/// wall line and still contribute (the old `s` tolerance of 1e-9 dropped
/// step-boundary edges that stopped ~1e-7 m shy, losing the step's jump).
pub(crate) fn profile_breakpoints(
    ceilings: &CeilingRegions,
    a: Point2f64,
    b: Point2f64,
) -> Vec<f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let mut ts = vec![0.0, 1.0];
    if len2 <= 1e-18 {
        return ts;
    }
    let len = len2.sqrt();
    for r in &ceilings.regions {
        let ext = &r.polygon.exterior;
        let n = ext.len();
        if n < 2 {
            continue;
        }
        for i in 0..n {
            let p = ext[i];
            let q = ext[(i + 1) % n];
            // Perpendicular distance (m) of each edge endpoint to the wall
            // LINE — the metric collinearity measure.
            let dist_to_line = |c: &Point3f64| ((c.x - a.x) * dy - (c.y - a.y) * dx).abs() / len;
            if dist_to_line(&p) <= PROFILE_EPS_M && dist_to_line(&q) <= PROFILE_EPS_M {
                // (Near-)collinear overlap (e.g. the roof's eaves edge along
                // the wall base itself, possibly displaced by boolean-op
                // noise): the region can start/stop at the edge's endpoints.
                for c in [p, q] {
                    let t = ((c.x - a.x) * dx + (c.y - a.y) * dy) / len2;
                    if (0.0..=1.0).contains(&t) {
                        ts.push(t);
                    }
                }
                continue;
            }
            let (ex, ey) = (q.x - p.x, q.y - p.y);
            let (wx, wy) = (p.x - a.x, p.y - a.y);
            let denom = dx * ey - dy * ex;
            if denom.abs() > 1e-12 {
                // Proper crossing: base(t) == edge(s); keep t when both
                // parameters land on their segments, letting the edge fall
                // short of / overshoot the wall line by PROFILE_EPS_M.
                let t = (wx * ey - wy * ex) / denom;
                let s = (wx * dy - wy * dx) / denom;
                let edge_len = (ex * ex + ey * ey).sqrt();
                let eps_s = if edge_len > 0.0 {
                    PROFILE_EPS_M / edge_len
                } else {
                    0.0
                };
                if (-eps_s..=1.0 + eps_s).contains(&s) && (0.0..=1.0).contains(&t) {
                    ts.push(t);
                }
            }
        }
    }
    ts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    ts
}

/// The wall TOP for the base `a`→`b`, traced from `b` BACK to `a` (so the
/// caller's ring closes base → top in one loop): one linear span per
/// breakpoint interval, z evaluated at the interval ends on the covering
/// region's plane (region chosen at the interval MIDPOINT — never a
/// crossing), `fallback_top_z` over uncovered spans. Where adjacent spans
/// disagree at a breakpoint, both vertices are kept — the VERTICAL JUMP of a
/// ceiling step crossing the wall; where they agree, consecutive duplicates
/// collapse, so a flat roof (or empty `ceilings`) yields exactly
/// `[b_top, a_top]`.
fn ceiling_profile_top(
    ceilings: &CeilingRegions,
    a: Point2f64,
    b: Point2f64,
    fallback_top_z: f64,
) -> Vec<Point3f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    // Unit normal of the base — the nudge direction for the tolerant pick.
    let n = if len > 0.0 {
        (-dy / len, dx / len)
    } else {
        (0.0, 0.0)
    };
    let at = |t: f64| Point2f64::new(a.x + dx * t, a.y + dy * t);
    // (t0, t1, z at t0, z at t1) per interval, ascending t.
    let mut spans: Vec<(f64, f64, f64, f64)> = Vec::new();
    for w in profile_breakpoints(ceilings, a, b).windows(2) {
        let (t0, t1) = (w[0], w[1]);
        // Skip degenerate and sub-µm spans (boolean-op noise can put two
        // breakpoints ~1e-8 m apart; a hairline span samples unreliably and
        // its vertices are meaningless).
        if t1 - t0 <= 1e-9 || (t1 - t0) * len <= 1e-6 {
            continue;
        }
        let mid = at((t0 + t1) / 2.0);
        let span = match covering_roof_plane(ceilings, mid, n) {
            Some((ri, (c0, cx, cy))) => {
                let (p0, p1) = (at(t0), at(t1));
                let (z0, z1) = (c0 + cx * p0.x + cy * p0.y, c0 + cx * p1.x + cy * p1.y);
                log::debug!(
                    "wall profile [{:.3},{:.3}]→[{:.3},{:.3}] span t=[{t0:.4},{t1:.4}] \
                     mid=({:.4},{:.4}): roof region #{ri} plane z={c0:.4}{cx:+.4}·x{cy:+.4}·y \
                     → top {z0:.4}..{z1:.4}",
                    a.x,
                    a.y,
                    b.x,
                    b.y,
                    mid.x,
                    mid.y
                );
                (t0, t1, z0, z1)
            }
            None => {
                log::debug!(
                    "wall profile [{:.3},{:.3}]→[{:.3},{:.3}] span t=[{t0:.4},{t1:.4}] \
                     mid=({:.4},{:.4}): NO covering roof region → fallback z={fallback_top_z:.4}",
                    a.x,
                    a.y,
                    b.x,
                    b.y,
                    mid.x,
                    mid.y
                );
                (t0, t1, fallback_top_z, fallback_top_z)
            }
        };
        spans.push(span);
    }
    if spans.is_empty() {
        spans.push((0.0, 1.0, fallback_top_z, fallback_top_z));
    }
    let mut out: Vec<Point3f64> = Vec::new();
    let push = |out: &mut Vec<Point3f64>, p: Point2f64, z: f64| {
        let keep = out.last().is_none_or(|l| {
            (l.x - p.x).abs() > 1e-9 || (l.y - p.y).abs() > 1e-9 || (l.z - z).abs() > 1e-9
        });
        if keep {
            out.push(Point3f64::new(p.x, p.y, z));
        }
    };
    for (t0, t1, z0, z1) in spans.iter().rev() {
        push(&mut out, at(*t1), *z1);
        push(&mut out, at(*t0), *z0);
    }
    if log::log_enabled!(log::Level::Debug) {
        let pl: Vec<String> = out
            .iter()
            .map(|v| format!("({:.4},{:.4},{:.4})", v.x, v.y, v.z))
            .collect();
        log::debug!(
            "wall profile [{:.3},{:.3}]→[{:.3},{:.3}] top polyline (b→a): {}",
            a.x,
            a.y,
            b.x,
            b.y,
            pl.join(" ")
        );
    }
    out
}

/// Parameters t ∈ [0, 1] along the base `a`→`b` where the covering FLOOR
/// region can change: every crossing of a floor region boundary edge with the
/// base (near-collinear overlapping edges contribute their projected
/// endpoints), plus the ends 0 and 1. Sorted ascending; the caller skips
/// sub-µm spans. The floor analogue of [`profile_breakpoints`] — floor regions
/// are `geo::Polygon`s (2D, no per-vertex z), so this walks the exterior AND
/// interior rings in plan. Same metric tolerances ([`PROFILE_EPS_M`]) as the
/// roof version: floor rings are boolean-op output too.
pub(crate) fn floor_breakpoints(floors: &FloorRegions, a: Point2f64, b: Point2f64) -> Vec<f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let mut ts = vec![0.0, 1.0];
    if len2 <= 1e-18 {
        return ts;
    }
    let len = len2.sqrt();
    let mut push_ring = |ext: &LineString<f64>| {
        let pts: Vec<Point2f64> = ext.coords().map(|c| Point2f64::new(c.x, c.y)).collect();
        let n = pts.len();
        if n < 2 {
            return;
        }
        // A `geo` ring closes (first == last); walking 0..n with wraparound
        // would double the closing edge, but a duplicate breakpoint is
        // harmless (the caller dedups by spans), so iterate uniformly.
        for i in 0..n {
            let p = pts[i];
            let q = pts[(i + 1) % n];
            let dist_to_line = |c: &Point2f64| ((c.x - a.x) * dy - (c.y - a.y) * dx).abs() / len;
            if dist_to_line(&p) <= PROFILE_EPS_M && dist_to_line(&q) <= PROFILE_EPS_M {
                // Collinear overlap: the region can start/stop at the edge's
                // projected endpoints.
                for c in [p, q] {
                    let t = ((c.x - a.x) * dx + (c.y - a.y) * dy) / len2;
                    if (0.0..=1.0).contains(&t) {
                        ts.push(t);
                    }
                }
                continue;
            }
            let (ex, ey) = (q.x - p.x, q.y - p.y);
            let (wx, wy) = (p.x - a.x, p.y - a.y);
            let denom = dx * ey - dy * ex;
            if denom.abs() > 1e-12 {
                let t = (wx * ey - wy * ex) / denom;
                let s = (wx * dy - wy * dx) / denom;
                let edge_len = (ex * ex + ey * ey).sqrt();
                let eps_s = if edge_len > 0.0 {
                    PROFILE_EPS_M / edge_len
                } else {
                    0.0
                };
                if (-eps_s..=1.0 + eps_s).contains(&s) && (0.0..=1.0).contains(&t) {
                    ts.push(t);
                }
            }
        }
    };
    for r in &floors.regions {
        push_ring(r.polygon.exterior());
        for hole in r.polygon.interiors() {
            push_ring(hole);
        }
    }
    ts.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    ts
}

/// The `level_m` of the first FLOOR region whose plan projection contains `p`,
/// with the tolerant normal-nudge retry of [`covering_roof_plane`] (a wall
/// base lies ON the region boundary, and boolean-op noise can put it a few
/// 1e-7 m to one side). Floor regions are FLAT — a region contributes its
/// single `level_m`, no plane to evaluate. `None` over an uncovered span.
fn covering_floor_level(floors: &FloorRegions, p: Point2f64, n: (f64, f64)) -> Option<f64> {
    use geo::Intersects;
    let pick = |q: Point2f64| {
        let gq = GeoPoint::new(q.x, q.y);
        floors
            .regions
            .iter()
            .find(|r| r.polygon.intersects(&gq))
            .map(|r| r.level_m)
    };
    pick(p).or_else(|| {
        [1.0, -1.0].iter().find_map(|s| {
            pick(Point2f64::new(
                p.x + s * n.0 * PROFILE_EPS_M,
                p.y + s * n.1 * PROFILE_EPS_M,
            ))
        })
    })
}

/// The wall BASE for the base edge `a`→`b`, traced from `a` to `b` (the
/// caller's ring opens with this, then closes via [`ceiling_profile_top`] running
/// `b`→`a`): one flat span per [`floor_breakpoints`] interval, z =
/// `base_z + covering_region.level_m` (the slab-band offset already in
/// `base_z` composed with the local floor step), `base_z` over uncovered
/// spans. The floor analogue of [`ceiling_profile_top`], but SIMPLER — floor
/// regions are flat, so a span's z is constant (no slope), and the only
/// breakpoints that matter are STEP lines (a level change). Where adjacent
/// spans disagree at a breakpoint, both vertices are kept — the VERTICAL JUMP
/// of a floor step crossing the wall (symmetric to a ceiling step's top jump);
/// where they agree, consecutive duplicates collapse, so a single-level base
/// (or empty `floors`) yields exactly `[a@base_z, b@base_z]` — the byte-
/// identical 2-vertex base every legacy reader keys off.
fn floor_profile_base(
    floors: &FloorRegions,
    a: Point2f64,
    b: Point2f64,
    base_z: f64,
) -> Vec<Point3f64> {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len = (dx * dx + dy * dy).sqrt();
    let n = if len > 0.0 {
        (-dy / len, dx / len)
    } else {
        (0.0, 0.0)
    };
    let at = |t: f64| Point2f64::new(a.x + dx * t, a.y + dy * t);
    // (t0, t1, z over the interval) ascending t — z is constant per span.
    let mut spans: Vec<(f64, f64, f64)> = Vec::new();
    for w in floor_breakpoints(floors, a, b).windows(2) {
        let (t0, t1) = (w[0], w[1]);
        if t1 - t0 <= 1e-9 || (t1 - t0) * len <= 1e-6 {
            continue;
        }
        let mid = at((t0 + t1) / 2.0);
        let z = base_z + covering_floor_level(floors, mid, n).unwrap_or(0.0);
        // MERGE into the previous span when the level is the SAME: a floor
        // boundary that is a phase split (or any partition seam) at one level
        // is NOT a step, so it must leave the base a straight edge — no
        // intermediate vertex. Only a LEVEL CHANGE keeps a separate span (the
        // step's vertical jump). This is what preserves byte-identity for the
        // common single-level base: every boundary collapses into one span
        // `[a@base_z, b@base_z]`.
        match spans.last_mut() {
            Some((_, pt1, pz)) if (*pz - z).abs() <= RISER_Z_EPS_M => *pt1 = t1,
            _ => spans.push((t0, t1, z)),
        }
    }
    if spans.is_empty() {
        spans.push((0.0, 1.0, base_z));
    }
    let mut out: Vec<Point3f64> = Vec::new();
    let push = |out: &mut Vec<Point3f64>, p: Point2f64, z: f64| {
        let keep = out.last().is_none_or(|l| {
            (l.x - p.x).abs() > 1e-9 || (l.y - p.y).abs() > 1e-9 || (l.z - z).abs() > 1e-9
        });
        if keep {
            out.push(Point3f64::new(p.x, p.y, z));
        }
    };
    for (t0, t1, z) in spans.iter() {
        push(&mut out, at(*t0), *z);
        push(&mut out, at(*t1), *z);
    }
    out
}
