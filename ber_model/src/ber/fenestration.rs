// src/data/fenestration.rs

use manip_plan::SurfaceRef;
use serde::{Deserialize, Serialize};

use super::common::{Evidence, ItemId, Year};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum Glazing {
    Single,
    #[default]
    Double,
    Triple,
    Other(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum Frame {
    #[default]
    WoodUPVC,
    Metal,
    Other(String),
}

/// How overshadowed a window is by surrounding obstacles (buildings, trees) —
/// a DEAP solar-gain input that is per-WINDOW, not per type (two identical
/// windows on different walls can be shaded differently). Defaults to `Average`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum Overshading {
    Heavy,
    MoreThanAverage,
    #[default]
    Average,
    VeryLittle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum WindowNameSpecifics {
    Standard,
    Custom(String),
}

impl WindowNameSpecifics {
    /// A user-given name (`Custom`), or `None` for the unnamed `Standard` type.
    pub fn name(&self) -> Option<&str> {
        match self {
            WindowNameSpecifics::Standard => None,
            WindowNameSpecifics::Custom(s) => Some(s),
        }
    }

    /// Build from an optional name: a non-empty name → `Custom`, else `Standard`.
    pub fn from_name(name: Option<String>) -> Self {
        match name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
            Some(s) => WindowNameSpecifics::Custom(s),
            None => WindowNameSpecifics::Standard,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowType {
    pub id: ItemId,
    pub year: Year,
    pub glazing: Glazing,
    pub gap: Vec<u32>, // gap in mm for double/triple glazing
    pub frame: Frame,
    /// Whether this window has a CERTIFIED U-/g-value (manufacturer / NSAI cert)
    /// versus an uncertified type that falls back to DEAP default table values.
    /// Part of the type identity, so a certified and an uncertified window of the
    /// same glazing/frame/year are distinct types. `#[serde(default)]` → `false`
    /// (uncertified) on records written before the field existed.
    #[serde(default)]
    pub certified: bool,
    /// Whether this window type's opening sections are DRAUGHT-STRIPPED (the
    /// Structure-phase checkbox; feeds DEAP's ventilation openings count).
    /// `None` on records from before the field existed — the office resolves
    /// those downstream (BER_deap query / year inference).
    #[serde(default)]
    pub draught_stripped: Option<bool>,
    pub specifics: WindowNameSpecifics,
    pub photos: Vec<Evidence>,
    pub notes: Option<String>,
}

/// Durable identity of a PORCH inside its storey: the porch plan ring's
/// area-weighted centroid quantized to the mm grid `(x_mm, y_mm)` — the same
/// centroid-key convention the floor/ceiling exposure side-tables use. Chosen
/// over a porch INDEX (deletion reorders `porches`) and over the porch plan's
/// own `VertexId`s alone (each porch plan mints its own 0..n space, so a vid
/// can't say WHICH porch). Resolution matches the key exactly first, then
/// heals by containment (the stored point inside a current porch ring) when a
/// re-dimension drifted the centroid.
pub type PorchKey = (i64, i64);

/// Durable PARAMETRIC anchor for a wall opening: the perimeter wall it sits on
/// (by stable `VertexId`, via [`SurfaceRef`]) plus the opening centre's
/// along-position as a fraction (0..1) of that perimeter wall. The fraction is
/// PARENT-relative, so a structure split is handled for free — the fraction
/// picks which sub-wall the centre falls in. The `wall_id`/`along_m` inside the
/// opening's `placement` are a per-resolve CACHE re-derived from this anchor.
///
/// PORCH-WALL openings (`porch: Some`): `wall` then names an edge of that
/// porch's OWN `FloorPlan` (its vid space, not the outline's), and the anchor
/// resolves against `Storey::porch_walls` instead of `walls`. `None` — the
/// serde default, so every pre-porch record loads unchanged — is an outline
/// anchor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct WallAnchor {
    pub wall: SurfaceRef,
    pub along_fraction: f32,
    /// The porch whose plan `wall` refers to, for an opening hosted on a porch
    /// INNER wall — see [`PorchKey`]. `None` = an outline-wall anchor.
    #[serde(default)]
    pub porch: Option<PorchKey>,
}

/// Where an opening sits, parametrically — the DURABLE record of its location.
/// The 3D `vertices` on `Window`/`Door` are a CACHE derived from this at resolve
/// (`super::openings::derive_opening_vertices`); `placement: None` marks a legacy
/// record whose stored `vertices` stay authoritative.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum OpeningPlacement {
    /// On a vertical wall. `along_m` is the CENTRE distance along the wall's
    /// base edge from `exterior[0]` toward `exterior[1]`; `sill_m` is the
    /// opening's bottom height above the wall's `base_z`.
    Wall {
        wall_id: ItemId,
        along_m: f32,
        sill_m: f32,
    },
    /// On a (possibly sloped) roof/ceiling region — free placement at a plan
    /// point inside the region; the z's derive from the region plane.
    Roof {
        ceiling_region_id: ItemId,
        plan: (f32, f32),
    },
}

/// The make-up of a door panel. `AssemblyPanel` is the door element of a
/// door+sidelights assembly (the glazed sidelights/transom are separate
/// `Window`s sharing the door's `assembly_id`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum DoorKind {
    #[default]
    Solid,
    HalfGlazed,
    FullyGlazed,
    AssemblyPanel,
}

/// The OUTLINE shape of a window within its `width_m × height_m` bounding box.
/// `Rectangular` is the default; the rest are the "complex" shapes the assessor
/// picks from the Complex-window tool. Params are measured INSIDE the bbox — e.g.
/// `RoundedTop.rise_m` is the arc's rise ABOVE the rectangular part, so the
/// rectangle's height is `height_m − rise_m`. Because `width_m`/`height_m` always
/// span the whole shape, placement / wall-fit / sill / hit-test logic all stay
/// shape-agnostic; only the tessellation (`window_outline`) and area
/// (`window_shape_area`) read the shape. See [`window_outline`].
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub enum WindowShape {
    #[default]
    Rectangular,
    /// Half-ellipse fanlight: flat bottom, the top edge arcs up the full height
    /// (the legacy arched window; a semicircular door+sidelights transom).
    Arched,
    /// PLAIN-FLOW shaped top (the "semi-circular top" tick — see
    /// [`WindowTopShape`]): a rectangle capped by a TRUE SEMICIRCLE of radius
    /// `width_m / 2` spanning the full width. No parameters — `height_m` is the
    /// TOTAL height measured at the CENTRE (the arc apex), so the rectangular
    /// body is `height_m − width_m/2` tall. Because the radius re-derives from
    /// the current width, the shape stays consistent through resizes.
    SemiCircularTop,
    /// PLAIN-FLOW shaped top (the "pointed top" tick): a rectangle capped by a
    /// centred triangle `width_m / 2` tall (apex at the centre of the top). As
    /// with [`WindowShape::SemiCircularTop`], `height_m` is the TOTAL height at
    /// the apex and there are no free parameters.
    PointedTop,
    /// A rectangle with a segmental (elliptical) arc on top rising `rise_m`.
    RoundedTop { rise_m: f32 },
    /// A rectangle topped by a centred triangle peaking `peak_m` above it.
    TriangularTop { peak_m: f32 },
    /// A rectangle with a two-sided pointed (Gothic) arch rising `rise_m`.
    PointedArch { rise_m: f32 },
    /// A full circle inscribed in the box (`width_m == height_m` = diameter).
    Circular,
    /// A half-round: flat bottom, semicircular top (`height_m == width_m / 2`).
    Semicircle,
    /// A trapezoid with a sloped top between edge heights `left_m`/`right_m`
    /// (`height_m == max(left_m, right_m)`) — the raking/attic window.
    Raking { left_m: f32, right_m: f32 },
    /// A triangle (gable apex): base `width_m`, centred apex at `height_m`.
    Triangle,
    /// A T: a top band `band_m` tall spanning `width_m`, over a centred stem
    /// `stem_w_m` wide filling the rest (`height_m == band_m + stem height`).
    TShape { band_m: f32, stem_w_m: f32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Window {
    pub id: ItemId,
    /// DERIVED cache (recomputed at resolve from `placement`); authoritative
    /// only for legacy records with `placement: None`.
    pub vertices: Vec<(f32, f32, f32)>,
    pub width_m: f32,
    pub height_m: f32,
    pub window_type_id: ItemId,
    #[serde(default)]
    pub placement: Option<OpeningPlacement>,
    /// Durable PARAMETRIC anchor for a wall opening: the perimeter wall it sits
    /// on, by stable [`SurfaceRef`]. The host `wall_id` inside `placement` is a
    /// resolved-region id that the wall amend-match recovers across derivation
    /// edits and `rehome` rebases across splits — but an outline RESHAPE mints a
    /// fresh wall id it can't recover. The resolve tail re-points a stale
    /// `wall_id` from this anchor (and seeds the anchor for new/legacy openings).
    /// `None` for roof openings and not-yet-seeded records.
    #[serde(default)]
    pub wall_anchor: Option<WallAnchor>,
    /// Groups the members of a door+sidelights assembly (this window is a
    /// sidelight/transom of the door with the same id). `None` for standalone.
    #[serde(default)]
    pub assembly_id: Option<ItemId>,
    /// A MULTI-STOREY window: it may span up past its host storey's ceiling (the
    /// roofline top-clamp is lifted in the vertex derivation), as far as the
    /// same-type vertically-aligned wall run allows. The "Windows" checklist gate
    /// flags one that hangs off the building. `#[serde(default)]` → false on old
    /// records.
    #[serde(default)]
    pub multi_storey: bool,
    /// The window's OUTLINE shape (rectangular by default; a complex shape from
    /// the Complex-window tool, or `Arched` for a semicircular door+sidelights
    /// transom). The derived `vertices` tessellate it. `#[serde(default)]` →
    /// `Rectangular` on records written before the field existed.
    #[serde(default)]
    pub shape: WindowShape,
    /// Per-window overshading (DEAP solar-gain input). `#[serde(default)]` →
    /// `Average` on records written before the field existed.
    #[serde(default)]
    pub overshading: Overshading,
    /// How many OPENING sections (opes) this window unit has — asked at
    /// placement; feeds DEAP's ventilation openings count. `None` on records
    /// from before the field existed (downstream assumes 1).
    #[serde(default)]
    pub num_opes: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Door {
    pub id: ItemId,
    pub type_name: String,
    /// DERIVED cache (see `Window::vertices`).
    pub vertices: Vec<(f32, f32, f32)>,
    pub photos: Vec<Evidence>,
    #[serde(default)]
    pub width_m: f32,
    #[serde(default)]
    pub height_m: f32,
    #[serde(default)]
    pub kind: Option<DoorKind>,
    #[serde(default)]
    pub placement: Option<OpeningPlacement>,
    /// Durable parametric anchor for a wall opening — see [`Window::wall_anchor`].
    #[serde(default)]
    pub wall_anchor: Option<WallAnchor>,
    /// Set on the door panel of a door+sidelights assembly; its sidelight/
    /// transom `Window`s carry the same id.
    #[serde(default)]
    pub assembly_id: Option<ItemId>,
    /// Whether the door is DRAUGHT-STRIPPED (the placement-box checkbox; a
    /// door counts as one opening in DEAP's ventilation count). `None` on
    /// records from before the field — resolved downstream (door-year
    /// inference / query).
    #[serde(default)]
    pub draught_stripped: Option<bool>,
}

/// The non-rectangular shapes the Complex-window tool offers as picker options
/// (no dimensions — the placement box collects those, then
/// [`window_shape_from_inputs`] turns kind + dims into a [`WindowShape`] + bbox).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComplexWindowKind {
    #[default]
    RoundedTop,
    TriangularTop,
    PointedArch,
    Circular,
    Semicircle,
    Triangle,
    Raking,
    TShape,
}

impl ComplexWindowKind {
    /// Every kind, in picker order.
    pub const ALL: [ComplexWindowKind; 8] = [
        ComplexWindowKind::RoundedTop,
        ComplexWindowKind::TriangularTop,
        ComplexWindowKind::PointedArch,
        ComplexWindowKind::Circular,
        ComplexWindowKind::Semicircle,
        ComplexWindowKind::Triangle,
        ComplexWindowKind::Raking,
        ComplexWindowKind::TShape,
    ];

    /// Label for the shape dropdown.
    pub fn label(self) -> &'static str {
        match self {
            ComplexWindowKind::RoundedTop => "Rounded top",
            ComplexWindowKind::TriangularTop => "Triangular top",
            ComplexWindowKind::PointedArch => "Pointed (Gothic) arch",
            ComplexWindowKind::Circular => "Circular",
            ComplexWindowKind::Semicircle => "Half-round",
            ComplexWindowKind::Triangle => "Triangle (gable)",
            ComplexWindowKind::Raking => "Raking (sloped top)",
            ComplexWindowKind::TShape => "T-shape",
        }
    }

    /// The measurement fields the box collects for this kind, in order, as
    /// `(label, hint)`. Their parsed values feed [`window_shape_from_inputs`]
    /// positionally, so the order here is the binding contract.
    pub fn field_labels(self) -> &'static [(&'static str, &'static str)] {
        match self {
            ComplexWindowKind::RoundedTop => &[
                ("width (m)", "e.g. 1.2"),
                ("rect height (m)", "the straight part"),
                ("arc rise (m)", "curve above"),
            ],
            ComplexWindowKind::TriangularTop => &[
                ("width (m)", "e.g. 1.2"),
                ("rect height (m)", "the straight part"),
                ("peak rise (m)", "apex above"),
            ],
            ComplexWindowKind::PointedArch => &[
                ("width (m)", "e.g. 1.2"),
                ("rect height (m)", "the straight part"),
                ("point rise (m)", "apex above"),
            ],
            ComplexWindowKind::Circular => &[("diameter (m)", "e.g. 0.6")],
            ComplexWindowKind::Semicircle => &[("width (m)", "= diameter")],
            ComplexWindowKind::Triangle => {
                &[("base width (m)", "e.g. 1.2"), ("height (m)", "apex")]
            }
            ComplexWindowKind::Raking => &[
                ("width (m)", "e.g. 1.2"),
                ("left height (m)", "left edge"),
                ("right height (m)", "right edge"),
            ],
            ComplexWindowKind::TShape => &[
                ("overall width (m)", "the top band"),
                ("top band height (m)", "the wide part"),
                ("stem width (m)", "the narrow part"),
                ("stem height (m)", "below the band"),
            ],
        }
    }
}

/// The optional SHAPED TOP a plain window can take in the NORMAL placement flow
/// — a tick, not a shape tool: no dimensions beyond the usual width/height
/// (contrast [`ComplexWindowKind`], whose shapes collect their own
/// measurements). `SemiCircular` caps the rectangle with a true semicircle of
/// radius `width/2`; `Pointed` with a centred triangle `width/2` tall; either
/// way the entered height is the TOTAL height at the centre (apex). Not
/// serialized: the durable record is the mapped [`WindowShape`] on the window
/// (`to_shape`), and `from_shape` recovers the tick state for editing — one
/// `shape` field holds both the plain-with-top and complex families, so the
/// two can never contradict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum WindowTopShape {
    #[default]
    Flat,
    SemiCircular,
    Pointed,
}

impl WindowTopShape {
    /// Every top shape, in picker order.
    pub const ALL: [WindowTopShape; 3] = [
        WindowTopShape::Flat,
        WindowTopShape::SemiCircular,
        WindowTopShape::Pointed,
    ];

    /// Label for the tick / picker.
    pub fn label(self) -> &'static str {
        match self {
            WindowTopShape::Flat => "Flat top",
            WindowTopShape::SemiCircular => "Semi-circular top",
            WindowTopShape::Pointed => "Pointed top",
        }
    }

    /// The [`WindowShape`] this tick stores on the window.
    pub fn to_shape(self) -> WindowShape {
        match self {
            WindowTopShape::Flat => WindowShape::Rectangular,
            WindowTopShape::SemiCircular => WindowShape::SemiCircularTop,
            WindowTopShape::Pointed => WindowShape::PointedTop,
        }
    }

    /// The tick state a stored shape maps back to — `None` for a COMPLEX shape
    /// (one from [`ComplexWindowKind`], edited by its own tool, not this tick).
    pub fn from_shape(shape: WindowShape) -> Option<WindowTopShape> {
        match shape {
            WindowShape::Rectangular => Some(WindowTopShape::Flat),
            WindowShape::SemiCircularTop => Some(WindowTopShape::SemiCircular),
            WindowShape::PointedTop => Some(WindowTopShape::Pointed),
            _ => None,
        }
    }
}

/// Segments used to tessellate a curved arc (a full semicircle → `ARC_N` steps).
const ARC_N: usize = 32;

/// The closed OUTLINE of a window `shape` in LOCAL coordinates: `x ∈ [0, width]`
/// along the wall from the left edge, `y ∈ [0, height]` up from the sill. The
/// polygon is implicitly closed (last point → first). Curves are tessellated.
/// This is the single source of truth for a window's silhouette — both the 3D
/// vertex derivation (`openings::opening_vertices`) and the 2D elevation drawer
/// map these points into their own frame, so the two never drift.
pub fn window_outline(shape: WindowShape, w: f32, h: f32) -> Vec<(f32, f32)> {
    use std::f32::consts::PI;
    // A half-ellipse (semi-axes `a`×`b`) centred at (`cx`, `base`), swept from the
    // right springer up and over to the left — the interior arc points only.
    let half_ellipse = |cx: f32, base: f32, a: f32, b: f32| -> Vec<(f32, f32)> {
        (1..ARC_N)
            .map(|i| {
                let th = PI * (i as f32) / (ARC_N as f32);
                (cx + a * th.cos(), base + b * th.sin())
            })
            .collect()
    };
    // A quadratic Bézier P0→P2 with control P1 (interior points), for the two
    // bulging sides of a pointed arch.
    let bezier = |p0: (f32, f32), p1: (f32, f32), p2: (f32, f32)| -> Vec<(f32, f32)> {
        (1..ARC_N / 2)
            .map(|i| {
                let t = (i as f32) / ((ARC_N / 2) as f32);
                let u = 1.0 - t;
                (
                    u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0,
                    u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1,
                )
            })
            .collect()
    };
    let mut pts = Vec::new();
    match shape {
        WindowShape::Rectangular => pts.extend([(0.0, 0.0), (w, 0.0), (w, h), (0.0, h)]),
        // Flat bottom, half-ellipse (or half-circle for Semicircle) over the top.
        WindowShape::Arched | WindowShape::Semicircle => {
            pts.push((0.0, 0.0));
            pts.push((w, 0.0));
            pts.extend(half_ellipse(w / 2.0, 0.0, w / 2.0, h));
        }
        // Plain-flow shaped tops: the cap height is width/2 by definition, the
        // body the rest. Should the total height be shorter than width/2 the
        // body vanishes and the cap flattens to fit the box (degenerate input,
        // still well-defined and inside the bbox).
        WindowShape::SemiCircularTop => {
            let rh = (h - w / 2.0).max(0.0);
            pts.extend([(0.0, 0.0), (w, 0.0), (w, rh)]);
            pts.extend(half_ellipse(w / 2.0, rh, w / 2.0, h - rh));
            pts.push((0.0, rh));
        }
        WindowShape::PointedTop => {
            let rh = (h - w / 2.0).max(0.0);
            pts.extend([(0.0, 0.0), (w, 0.0), (w, rh), (w / 2.0, h), (0.0, rh)]);
        }
        WindowShape::RoundedTop { rise_m } => {
            let rh = (h - rise_m).max(0.0);
            pts.extend([(0.0, 0.0), (w, 0.0), (w, rh)]);
            pts.extend(half_ellipse(w / 2.0, rh, w / 2.0, rise_m));
            pts.push((0.0, rh));
        }
        WindowShape::TriangularTop { peak_m } => {
            let rh = (h - peak_m).max(0.0);
            pts.extend([(0.0, 0.0), (w, 0.0), (w, rh), (w / 2.0, h), (0.0, rh)]);
        }
        WindowShape::PointedArch { rise_m } => {
            let rh = (h - rise_m).max(0.0);
            pts.extend([(0.0, 0.0), (w, 0.0), (w, rh)]);
            pts.extend(bezier((w, rh), (w, h), (w / 2.0, h))); // right side up to apex
            pts.push((w / 2.0, h));
            pts.extend(bezier((w / 2.0, h), (0.0, h), (0.0, rh))); // left side down
            pts.push((0.0, rh));
        }
        WindowShape::Circular => {
            let (cx, cy, r) = (w / 2.0, h / 2.0, w / 2.0);
            pts.extend((0..2 * ARC_N).map(|i| {
                let th = 2.0 * PI * (i as f32) / ((2 * ARC_N) as f32);
                (cx + r * th.cos(), cy + r * th.sin())
            }));
        }
        WindowShape::Raking { left_m, right_m } => {
            pts.extend([(0.0, 0.0), (w, 0.0), (w, right_m), (0.0, left_m)]);
        }
        WindowShape::Triangle => pts.extend([(0.0, 0.0), (w, 0.0), (w / 2.0, h)]),
        WindowShape::TShape { band_m, stem_w_m } => {
            let stem_h = (h - band_m).max(0.0);
            let (x0, x1) = ((w - stem_w_m) / 2.0, (w + stem_w_m) / 2.0);
            pts.extend([
                (x0, 0.0),
                (x1, 0.0),
                (x1, stem_h),
                (w, stem_h),
                (w, h),
                (0.0, h),
                (0.0, stem_h),
                (x0, stem_h),
            ]);
        }
    }
    pts
}

/// The true geometric AREA (m²) of a window `shape` in its `width_m × height_m`
/// box. This is the glazing area a future DEAP wiring should use, and what
/// [`super::openings::wall_net_area`] subtracts from the host wall.
///
/// The plain-flow shaped tops ([`WindowShape::SemiCircularTop`] /
/// [`WindowShape::PointedTop`]) use their EXACT closed forms —
/// `w·(h − w/2) + π·(w/2)²/2` and `w·(h − w/2) + w²/4` — because the derived
/// area is a DEAP input, so it should be the true figure; the tessellated
/// outline is only the drawing (its shoelace tracks the exact value within a
/// fraction of a percent at `ARC_N` facets). Every other shape is the shoelace
/// of [`window_outline`], so it always matches the drawn silhouette.
pub fn window_shape_area(shape: WindowShape, w: f32, h: f32) -> f32 {
    use std::f32::consts::PI;
    match shape {
        WindowShape::SemiCircularTop => {
            // Body + semicircle (radius w/2). A degenerate `h < w/2` flattens
            // the cap to a half-ellipse of rise `h`, mirroring the outline.
            let rh = (h - w / 2.0).max(0.0);
            return w * rh + PI * (w / 2.0) * (h - rh) / 2.0;
        }
        WindowShape::PointedTop => {
            // Body + centred triangle (height w/2, clamped to the box like the
            // outline: apex at `h`, base at `rh`).
            let rh = (h - w / 2.0).max(0.0);
            return w * rh + w * (h - rh) / 2.0;
        }
        _ => {}
    }
    let pts = window_outline(shape, w, h);
    let mut a = 0.0f32;
    for i in 0..pts.len() {
        let (x0, y0) = pts[i];
        let (x1, y1) = pts[(i + 1) % pts.len()];
        a += x0 * y1 - x1 * y0;
    }
    (a * 0.5).abs()
}

/// Map a Complex-window kind + the box's ordered dimension inputs to the stored
/// `(width_m, height_m, shape)` — `width_m`/`height_m` being the bounding box.
/// Missing/short inputs read as 0. The `dims` order matches
/// [`ComplexWindowKind::field_labels`].
pub fn window_shape_from_inputs(kind: ComplexWindowKind, dims: &[f32]) -> (f32, f32, WindowShape) {
    let d = |i: usize| dims.get(i).copied().unwrap_or(0.0);
    match kind {
        ComplexWindowKind::RoundedTop => {
            (d(0), d(1) + d(2), WindowShape::RoundedTop { rise_m: d(2) })
        }
        ComplexWindowKind::TriangularTop => (
            d(0),
            d(1) + d(2),
            WindowShape::TriangularTop { peak_m: d(2) },
        ),
        ComplexWindowKind::PointedArch => {
            (d(0), d(1) + d(2), WindowShape::PointedArch { rise_m: d(2) })
        }
        ComplexWindowKind::Circular => (d(0), d(0), WindowShape::Circular),
        ComplexWindowKind::Semicircle => (d(0), d(0) / 2.0, WindowShape::Semicircle),
        ComplexWindowKind::Triangle => (d(0), d(1), WindowShape::Triangle),
        ComplexWindowKind::Raking => (
            d(0),
            d(1).max(d(2)),
            WindowShape::Raking {
                left_m: d(1),
                right_m: d(2),
            },
        ),
        ComplexWindowKind::TShape => (
            d(0),
            d(1) + d(3),
            WindowShape::TShape {
                band_m: d(1),
                stem_w_m: d(2),
            },
        ),
    }
}

#[cfg(test)]
mod shape_tests {
    use super::*;
    use std::f32::consts::PI;

    #[test]
    fn area_matches_closed_forms() {
        // Rectangle.
        assert!((window_shape_area(WindowShape::Rectangular, 1.2, 1.4) - 1.68).abs() < 1e-3);
        // Triangle = ½·base·height.
        assert!((window_shape_area(WindowShape::Triangle, 1.2, 0.8) - 0.48).abs() < 1e-3);
        // Trapezoid (raking) = width·(left+right)/2.
        let raking = WindowShape::Raking {
            left_m: 1.0,
            right_m: 2.0,
        };
        assert!((window_shape_area(raking, 1.4, 2.0) - 1.4 * 1.5).abs() < 1e-3);
        // T = band + stem.
        let t = WindowShape::TShape {
            band_m: 0.3,
            stem_w_m: 0.4,
        };
        // overall W 1.2, band 0.3, stem 0.4×0.7 → 1.2·0.3 + 0.4·0.7 = 0.64.
        assert!((window_shape_area(t, 1.2, 1.0) - 0.64).abs() < 1e-3);
        // Circle ≈ π·r² (tessellated, so allow a small polygon-vs-circle gap).
        let circ = window_shape_area(WindowShape::Circular, 1.0, 1.0);
        assert!((circ - PI * 0.25).abs() < 0.01, "circle area {circ}");
        // Rounded top = rect + half-ellipse segment (π·w·rise/4).
        let rt = window_shape_area(WindowShape::RoundedTop { rise_m: 0.3 }, 1.2, 1.3);
        let expect = 1.2 * 1.0 + PI * 1.2 * 0.3 / 4.0;
        assert!((rt - expect).abs() < 0.01, "rounded-top {rt} vs {expect}");
    }

    /// Shoelace of a local outline — the FACET area, for comparing the drawn
    /// polygon against the exact derivation.
    fn shoelace(pts: &[(f32, f32)]) -> f32 {
        let mut a = 0.0f32;
        for i in 0..pts.len() {
            let (x0, y0) = pts[i];
            let (x1, y1) = pts[(i + 1) % pts.len()];
            a += x0 * y1 - x1 * y0;
        }
        (a * 0.5).abs()
    }

    #[test]
    fn plain_flow_top_areas_are_the_exact_closed_forms() {
        let (w, h) = (1.2f32, 1.5f32);
        // Semi-circular top: w·(h − w/2) + π·(w/2)²/2.
        let semi = window_shape_area(WindowShape::SemiCircularTop, w, h);
        let semi_exact = w * (h - w / 2.0) + PI * (w / 2.0) * (w / 2.0) / 2.0;
        assert!((semi - semi_exact).abs() < 1e-6, "{semi} vs {semi_exact}");
        // Pointed top: w·(h − w/2) + w²/4.
        let pointed = window_shape_area(WindowShape::PointedTop, w, h);
        let pointed_exact = w * (h - w / 2.0) + w * w / 4.0;
        assert!(
            (pointed - pointed_exact).abs() < 1e-6,
            "{pointed} vs {pointed_exact}"
        );
        // The FACET polygon (what gets drawn / subtracted geometrically) stays
        // within 1% of the exact area used for the derivation.
        let semi_facet = shoelace(&window_outline(WindowShape::SemiCircularTop, w, h));
        assert!(
            (semi_facet - semi_exact).abs() / semi_exact < 0.01,
            "facet {semi_facet} vs exact {semi_exact}"
        );
        // Pointed is polygonal, so its facet area IS the exact area.
        let pointed_facet = shoelace(&window_outline(WindowShape::PointedTop, w, h));
        assert!((pointed_facet - pointed_exact).abs() < 1e-4);
    }

    #[test]
    fn plain_flow_top_outlines_apex_at_centre_and_within_bbox() {
        let (w, h) = (1.2f32, 1.5f32);
        for shape in [WindowShape::SemiCircularTop, WindowShape::PointedTop] {
            let pts = window_outline(shape, w, h);
            // The base spans the full width at y = 0.
            assert!(pts.contains(&(0.0, 0.0)) && pts.contains(&(w, 0.0)));
            // The topmost point is the CENTRE at the full entered height — the
            // "height is measured at the centre" contract.
            let top = pts
                .iter()
                .copied()
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap();
            assert!((top.0 - w / 2.0).abs() < 1e-4, "{shape:?} apex x {}", top.0);
            assert!((top.1 - h).abs() < 1e-4, "{shape:?} apex y {}", top.1);
            // Nothing leaves the w×h bounding box (host confinement / clamps
            // reason over that box).
            assert!(pts
                .iter()
                .all(|p| (-1e-4..=w + 1e-4).contains(&p.0) && (-1e-4..=h + 1e-4).contains(&p.1)));
            // The cap springs from the body top at h − w/2 on both sides.
            let rh = h - w / 2.0;
            assert!(pts.contains(&(0.0, rh)) && pts.contains(&(w, rh)));
        }
    }

    #[test]
    fn window_shape_serde_defaults_flat_and_round_trips_new_variants() {
        let win = Window {
            id: crate::ber::common::ItemId::default(),
            vertices: vec![],
            width_m: 1.2,
            height_m: 1.5,
            window_type_id: crate::ber::common::ItemId::default(),
            placement: None,
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            shape: WindowShape::SemiCircularTop,
            overshading: Overshading::Average,
            num_opes: None,
        };
        // Round trip: the new variant survives serde intact.
        let json = serde_json::to_value(&win).unwrap();
        let back: Window = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back.shape, WindowShape::SemiCircularTop);
        // A record written BEFORE the `shape` field existed (key absent) loads
        // as a plain rectangular window.
        let mut old = json;
        old.as_object_mut().unwrap().remove("shape");
        let legacy: Window = serde_json::from_value(old).unwrap();
        assert_eq!(legacy.shape, WindowShape::Rectangular);
    }

    #[test]
    fn top_shape_tick_maps_to_and_from_shapes() {
        for t in WindowTopShape::ALL {
            assert_eq!(WindowTopShape::from_shape(t.to_shape()), Some(t));
        }
        // Complex shapes are outside the tick's domain.
        assert_eq!(WindowTopShape::from_shape(WindowShape::Circular), None);
        assert_eq!(
            WindowTopShape::from_shape(WindowShape::RoundedTop { rise_m: 0.3 }),
            None
        );
    }

    #[test]
    fn from_inputs_maps_kind_and_dims() {
        // Rounded top: [width, rect-h, rise] → bbox (w, rect+rise).
        let (w, h, s) = window_shape_from_inputs(ComplexWindowKind::RoundedTop, &[1.2, 1.0, 0.3]);
        assert_eq!((w, h), (1.2, 1.3));
        assert_eq!(s, WindowShape::RoundedTop { rise_m: 0.3 });
        // Circular: one diameter → square bbox.
        let (w, h, s) = window_shape_from_inputs(ComplexWindowKind::Circular, &[0.6]);
        assert_eq!((w, h, s), (0.6, 0.6, WindowShape::Circular));
        // Half-round: height = width/2.
        let (w, h, _) = window_shape_from_inputs(ComplexWindowKind::Semicircle, &[1.0]);
        assert_eq!((w, h), (1.0, 0.5));
        // T-shape: [overall W, band H, stem W, stem H] → bbox (W, band+stemH).
        let (w, h, s) = window_shape_from_inputs(ComplexWindowKind::TShape, &[1.2, 0.3, 0.4, 0.7]);
        assert_eq!((w, h), (1.2, 1.0));
        assert_eq!(
            s,
            WindowShape::TShape {
                band_m: 0.3,
                stem_w_m: 0.4
            }
        );
        // Raking: bbox height = max(left, right).
        let (_, h, s) = window_shape_from_inputs(ComplexWindowKind::Raking, &[1.4, 1.0, 2.0]);
        assert_eq!(h, 2.0);
        assert_eq!(
            s,
            WindowShape::Raking {
                left_m: 1.0,
                right_m: 2.0
            }
        );
    }
}
