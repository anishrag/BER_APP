//! External per-side ELEVATIONS — the Stage-1 review's view of one building
//! side: every wall (across all storeys) whose outward normal faces the side,
//! stacked by storey height, with its openings, projected onto a vertical view
//! plane. A depth-based HIDDEN-LINE pass drops what a NEARER facing wall stands
//! in front of: an OPENING it fully covers is dropped, and a PANEL edge segment
//! it covers is clipped out — so a recessed/rear wall's window and outline don't
//! show through the wall standing in front of it, matching what a camera pointed
//! at that side would (and wouldn't) see.
//!
//! Pure coordinate math over the resolved model (reuses [`WallRegion::base_edge`]
//! / [`base_frame`](WallRegion::base_frame) / [`top_profile`](WallRegion::top_profile)
//! and [`storey_floor_datums`](Ber::storey_floor_datums)); no UI, no I/O.

use crate::ber::ber_struct::Ber;
use crate::ber::common::{ExternalSide, ItemId};
use crate::ber::fenestration::WindowShape;
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
    /// The window's outline shape (drawn accordingly). Always `Rectangular` for
    /// doors.
    pub shape: WindowShape,
    /// Composite x of the opening's CENTRE.
    pub x_center: f64,
    pub width_m: f64,
    /// Composite z of the opening's BOTTOM (sill).
    pub sill_z: f64,
    pub height_m: f64,
}

/// One facing wall projected onto the side's view plane: its horizontal span
/// `[x0, x1]`, its base z, and its VISIBLE outline as a set of `(x, z)→(x, z)`
/// line segments (bottom polyline, top polyline, and end verticals), all in
/// the composite frame. The segments are the wall's full outline already
/// CLIPPED by the hidden-line pass — sub-spans lying behind a nearer facing
/// wall are removed — so a rear wall's edge doesn't draw across the wall in
/// front of it. A wall with nothing in front contributes its whole outline
/// (bottom + top polyline + two verticals), unchanged. Render draws each
/// segment, further clipping it around openings.
#[derive(Debug, Clone)]
pub struct ElevPanel {
    /// Horizontal span of the wall FACE — the extent of the bottom + top
    /// profiles (for the common rectangular wall, just the base edge's
    /// projection; a deep-notched remainder whose base FRAME truncates to its
    /// low run still spans its full drawn face here).
    pub x0: f64,
    pub x1: f64,
    /// z of the wall's frame origin (`base_frame` base_z + storey offset) —
    /// the SILL DATUM: an opening draws at `base_z + sill`. NOT necessarily
    /// the panel's lowest point: a stepped underside is `bottom`'s job.
    pub base_z: f64,
    /// The wall's UNDERSIDE as a composite `(x, z)` polyline — the mirror of
    /// the top profile, from [`WallRegion::base_profile`] (the same
    /// `base_frame` parameterisation as `base_z`/the sill math, so sills and
    /// the drawn bottom stay consistent on EITHER ring orientation of a
    /// deep-notched wall). Flat-bottom walls get the 2-point `[(x0, base_z),
    /// (x1, base_z)]`, matching the old flat base edge exactly.
    pub bottom: Vec<(f64, f64)>,
    pub segments: Vec<((f64, f64), (f64, f64))>,
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
    /// away from the storey's centroid. A depth-based hidden-line pass removes
    /// what a NEARER facing wall (greater depth along `+n`) stands in front of:
    /// an opening it fully covers is dropped, and a panel edge segment it covers
    /// is clipped out (both use the `occ` list built below).
    ///
    /// [`FACING_MIN`]: a wall counts as facing the side when its outward normal
    /// is within ~60° of the side's normal.
    pub fn side_elevation(&self, side: ExternalSide) -> SideElevation {
        /// Min dot(outward_normal, side_normal) for a wall to count as facing.
        const FACING_MIN: f64 = 0.5;
        let n = side.outward_dir();
        let h = side.view_axis(); // unit horizontal view axis, ⟂ n
        let datums = self.storey_floor_datums();

        // A facing wall's raw projection, kept until every wall is collected so
        // the hidden-line pass (below) can clip each panel against ALL nearer
        // walls (which may live in other storeys / later in iteration).
        struct RawPanel {
            id: ItemId,
            depth: f64,
            x0: f64,
            x1: f64,
            base_z: f64,
            top: Vec<(f64, f64)>,
            bottom: Vec<(f64, f64)>,
        }
        let mut raw: Vec<RawPanel> = Vec::new();
        let mut openings: Vec<ElevOpening> = Vec::new();
        // Per facing wall: (id, depth along +n, x0, x1, base_z, top_z) — used to
        // occlude openings AND panel edges on walls that stand BEHIND a nearer
        // facing wall.
        let mut occ: Vec<(ItemId, f64, f64, f64, f64, f64)> = Vec::new();
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

                // Top + bottom edges, mapped to composite (x, z) via along →
                // plan point. Both use the SAME base_frame parameterisation as
                // `bz` (the sill datum), so a stepped underside and the sills
                // drawn against it stay consistent on either ring orientation.
                let unroll = |prof: Vec<(f32, f32)>| -> Vec<(f64, f64)> {
                    prof.iter()
                        .map(|&(along, z)| {
                            let px = origin.x + along as f64 * fux;
                            let py = origin.y + along as f64 * fuy;
                            (h.0 * px + h.1 * py, z as f64 + offset)
                        })
                        .collect()
                };
                let top = unroll(w.top_profile());
                let bottom = unroll(w.base_profile());

                min_x = min_x.min(xa.min(xb));
                max_x = max_x.max(xa.max(xb));
                // Fold the base z into BOTH bounds so a panel with an empty top
                // (a degenerate ≤2-vertex wall) still contributes finite bounds.
                min_z = min_z.min(bz);
                max_z = max_z.max(bz);
                for &(tx, tz) in top.iter().chain(bottom.iter()) {
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
                        shape: op.shape,
                        x_center: h.0 * px + h.1 * py,
                        width_m: op.width_m as f64,
                        sill_z: bz + op.sill_m as f64,
                        height_m: op.height_m as f64,
                    });
                }

                let depth = mx * n.0 + my * n.1;
                // The panel's x-span is the FACE extent — the base edge plus
                // the profiles, which outspan it on a deep-notched remainder
                // whose base FRAME truncated to its low run.
                let (px0, px1) = top
                    .iter()
                    .chain(bottom.iter())
                    .fold((xa.min(xb), xa.max(xb)), |(lo, hi), &(x, _)| {
                        (lo.min(x), hi.max(x))
                    });
                // Occluder box: FLAT top at the peak and FLAT bottom at the
                // underside's LOWEST point — a stepped/notched bottom only
                // ever over-covers the notch gap below the face (the
                // InterStorey band panels fill it anyway), mirroring the
                // flat-top approximation over a sloped top's peak: it can
                // hide but never invent a rear edge. Using the profile MIN
                // (not the frame's `bz`) keeps the box identical on either
                // ring orientation of a deep-notched wall (`bz` may sit on
                // the HIGH shoulder). Flat-bottom walls: min == bz, unchanged.
                let top_z = top.iter().map(|&(_, z)| z).fold(bz, f64::max);
                let bot_z = bottom.iter().map(|&(_, z)| z).fold(bz, f64::min);
                occ.push((w.id, depth, px0, px1, bot_z, top_z));

                raw.push(RawPanel {
                    id: w.id,
                    depth,
                    x0: px0,
                    x1: px1,
                    base_z: bz,
                    top,
                    bottom,
                });
            }
        }

        // Opening occlusion: the viewer stands OUTSIDE the side (along +n), so a
        // facing wall with GREATER depth is NEARER and hides what's behind it.
        // Drop an opening that a nearer facing wall FULLY covers (in x AND z) —
        // e.g. a window on a recessed return wall that the main wall stands in
        // front of, which a camera wouldn't see. Partial overlaps are kept.
        openings.retain(|op| {
            let Some(od) = occ
                .iter()
                .find(|(wid, ..)| *wid == op.wall_id)
                .map(|&(_, d, ..)| d)
            else {
                return true;
            };
            let (ox0, ox1) = (
                op.x_center - op.width_m * 0.5,
                op.x_center + op.width_m * 0.5,
            );
            let (oz0, oz1) = (op.sill_z, op.sill_z + op.height_m);
            !occ.iter().any(|&(wid, d, x0, x1, bz, tz)| {
                wid != op.wall_id
                    && d > od + 1e-6
                    && x0 <= ox0 + 1e-6
                    && x1 >= ox1 - 1e-6
                    && bz <= oz0 + 1e-6
                    && tz >= oz1 - 1e-6
            })
        });

        if raw.is_empty() {
            return SideElevation::empty(side);
        }

        // Panel hidden-line pass: build each facing wall's full outline (base
        // edge + top polyline + the two end verticals) as segments, then clip
        // each segment against every NEARER facing wall's solid box. A rear
        // wall's base/top edge that runs behind the wall in front of it is
        // dropped, so it no longer draws across that front wall (and its
        // windows). A nearer wall's box is `[x0, x1] × [base_z, top_z]` — a
        // FLAT-TOP approximation using its top peak: exact for the common (and
        // reported) flat-top-behind-flat-top case, and for a sloped/stepped
        // nearer top only ever over-covers the triangular gap below its peak, so
        // it can hide but never invent a rear edge (no spurious line — the bug
        // being fixed). Occluders are OTHER walls strictly nearer (depth greater
        // by > eps); a wall never occludes itself or an equal-depth neighbour.
        let panels: Vec<ElevPanel> = raw
            .iter()
            .map(|panel| {
                let occluders: Vec<(f64, f64, f64, f64)> = occ
                    .iter()
                    .filter(|&&(wid, d, ..)| wid != panel.id && d > panel.depth + 1e-6)
                    .map(|&(_, _, x0, x1, bz, tz)| (x0, x1, bz, tz))
                    .collect();

                // The wall's full outline as raw edges: the BOTTOM polyline
                // (the stepped underside — one flat segment for the common
                // flat base), the top polyline, and the end verticals. The
                // base runs a→b and the top b→a (the ring contract), so the
                // seam vertical at `b` joins the top's FIRST point to the
                // bottom's LAST, and the side at `a` the top's LAST to the
                // bottom's FIRST.
                let mut edges: Vec<((f64, f64), (f64, f64))> = Vec::new();
                if panel.bottom.len() >= 2 {
                    for w in panel.bottom.windows(2) {
                        edges.push((w[0], w[1]));
                    }
                } else {
                    edges.push(((panel.x0, panel.base_z), (panel.x1, panel.base_z)));
                }
                for w in panel.top.windows(2) {
                    edges.push((w[0], w[1]));
                }
                if let (Some(&first), Some(&last)) = (panel.top.first(), panel.top.last()) {
                    let seam = panel
                        .bottom
                        .last()
                        .copied()
                        .unwrap_or((first.0, panel.base_z));
                    let side = panel
                        .bottom
                        .first()
                        .copied()
                        .unwrap_or((last.0, panel.base_z));
                    edges.push((first, seam));
                    edges.push((last, side));
                }

                // Clip each edge to the spans no nearer wall covers.
                let mut segments: Vec<((f64, f64), (f64, f64))> = Vec::new();
                for (a, b) in edges {
                    let (dx, dz) = (b.0 - a.0, b.1 - a.1);
                    for (ta, tb) in unoccluded_spans(a, b, &occluders) {
                        segments.push((
                            (a.0 + dx * ta, a.1 + dz * ta),
                            (a.0 + dx * tb, a.1 + dz * tb),
                        ));
                    }
                }

                ElevPanel {
                    x0: panel.x0,
                    x1: panel.x1,
                    base_z: panel.base_z,
                    bottom: panel.bottom.clone(),
                    segments,
                }
            })
            .collect();

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

/// The visible sub-spans (as `t` sub-intervals of `a`→`b`, `t ∈ [0, 1]`) of an
/// elevation outline segment once the parts STRICTLY inside any occluder rect
/// are removed — the hidden-line clip behind the panel pass. `occluders` are
/// `(x0, x1, base_z, top_z)` solid boxes (nearer facing walls) in the same
/// `(x, z)` frame. A segment covered by no box returns the whole `[(0.0, 1.0)]`;
/// one fully covered returns empty. Strictly-interior (open-interval) clipping:
/// a segment lying exactly on a box edge (e.g. a rear top edge coincident with
/// the nearer wall's top line) is kept, so only edges genuinely BEHIND the box
/// are dropped — matching the opening-occlusion convention.
///
/// Mirrors `render::elevation::wall_line_visible_spans` (which clips outline
/// lines around openings); kept here — not shared — because hidden-line removal
/// is a data-model derivation and `ber_model` must own its own geometry.
fn unoccluded_spans(
    a: (f64, f64),
    b: (f64, f64),
    occluders: &[(f64, f64, f64, f64)],
) -> Vec<(f64, f64)> {
    let (dx, dz) = (b.0 - a.0, b.1 - a.1);
    // The t-interval (along a→b) STRICTLY inside each box, via slab clipping on
    // x and z. A component parallel to a slab (d≈0) is "inside" only when its
    // fixed coordinate is strictly within that slab's open interval, so a line
    // lying exactly on a box edge is never treated as covered.
    let mut cuts: Vec<(f64, f64)> = Vec::new();
    for &(x0, x1, bz, tz) in occluders {
        let mut t0 = 0.0_f64;
        let mut t1 = 1.0_f64;
        let slab = |lo: f64, hi: f64, o: f64, d: f64, t0: &mut f64, t1: &mut f64| -> bool {
            if d.abs() < 1e-12 {
                return o > lo + 1e-9 && o < hi - 1e-9; // parallel: fixed coord in the open slab
            }
            let (mut ta, mut tb) = ((lo - o) / d, (hi - o) / d);
            if ta > tb {
                std::mem::swap(&mut ta, &mut tb);
            }
            *t0 = t0.max(ta);
            *t1 = t1.min(tb);
            true
        };
        if slab(x0, x1, a.0, dx, &mut t0, &mut t1)
            && slab(bz, tz, a.1, dz, &mut t0, &mut t1)
            && t1 - t0 > 1e-6
        {
            cuts.push((t0.clamp(0.0, 1.0), t1.clamp(0.0, 1.0)));
        }
    }
    if cuts.is_empty() {
        return vec![(0.0, 1.0)];
    }
    cuts.sort_by(|x, y| x.0.partial_cmp(&y.0).unwrap());
    let mut spans = Vec::new();
    let mut cursor = 0.0_f64;
    for (cl, ch) in cuts {
        if cl > cursor + 1e-6 {
            spans.push((cursor, cl));
        }
        cursor = cursor.max(ch);
    }
    if cursor < 1.0 - 1e-6 {
        spans.push((cursor, 1.0));
    }
    spans
}
