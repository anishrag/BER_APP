// src/data/fenestration.rs

use manip_plan::SurfaceRef;
use serde::{Deserialize, Serialize};

use super::common::{ItemId, Photo, Year};

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
    pub photos: Vec<Photo>,
    pub notes: Option<String>,
}

/// Durable PARAMETRIC anchor for a wall opening: the perimeter wall it sits on
/// (by stable `VertexId`, via [`SurfaceRef`]) plus the opening centre's
/// along-position as a fraction (0..1) of that perimeter wall. The fraction is
/// PARENT-relative, so a structure split is handled for free — the fraction
/// picks which sub-wall the centre falls in. The `wall_id`/`along_m` inside the
/// opening's `placement` are a per-resolve CACHE re-derived from this anchor.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct WallAnchor {
    pub wall: SurfaceRef,
    pub along_fraction: f32,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DoorKind {
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
    pub photos: Vec<Photo>,
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
/// box — the shoelace of [`window_outline`], so it always matches the drawn
/// silhouette (curves are the tessellated area, within a fraction of a percent of
/// the analytic value). This is the glazing area a future DEAP wiring should use.
pub fn window_shape_area(shape: WindowShape, w: f32, h: f32) -> f32 {
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
