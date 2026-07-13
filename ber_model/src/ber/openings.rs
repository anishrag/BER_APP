//! Window/door OPENINGS over a resolved `Storey` — pure data transforms (no
//! UI, DB, or `StoreyDoc`). Openings live durably on `Storey.windows`/`.doors`
//! as parametric `OpeningPlacement`s; their 3D `vertices` are a CACHE this
//! module derives from the resolved wall/roof geometry at the resolve tail.
//!
//! Placement ops (pick / add / move / redistribute / delete) live alongside the
//! derivation here so all opening logic is in one pure module the controller
//! drives.
use super::common::ItemId;
use super::fenestration::{Door, DoorKind, OpeningPlacement, Overshading, Window, WindowShape};
use super::structural::Storey;
use domain::points::point_to_segment_distance;

/// Default door sill height (metres above the wall base) + spacing constants for
/// auto-redistribution. Tuning-overridable later; sensible Irish-BER defaults.
pub const DEFAULT_DOOR_SILL_M: f32 = 0.0;
/// Default minimum gap between side-by-side openings. Auto-spacing aims for this,
/// but COMPRESSES toward `GAP_FLOOR_M` to cram more openings into a tight wall
/// before declaring it too short.
const MIN_GAP_M: f32 = 0.1;
/// The gap will compress no further than this (openings may touch, never overlap)
/// before a layout is rejected as `WallTooShort`.
const GAP_FLOOR_M: f32 = 0.0;
const END_MARGIN_M: f32 = 0.05;

/// Reasons an opening op can't proceed (mapped to a user message by the shell).
#[derive(Debug, Clone, PartialEq)]
pub enum OpeningError {
    /// The wall can't fit all its openings: `needed_m` of span required,
    /// `available_m` (wall length) on offer.
    WallTooShort { needed_m: f32, available_m: f32 },
    /// No host wall/roof region for the placement (degenerate or missing).
    NoHostRegion,
    /// Width or height was non-positive.
    ZeroSize,
}

/// Identifies one opening within a `Storey`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpeningRef {
    Window(ItemId),
    Door(ItemId),
}

/// One opening placed on a wall, with everything a caller needs to lay it out:
/// which opening, whether it's a door (vs window), its assembly membership (a
/// door+sidelights group, else `None`), and its wall-local placement/size. The
/// shell maps `is_door` + `assembly_id` to its own view kind; redistribution
/// reads the centre/width/assembly.
#[derive(Debug, Clone, Copy)]
pub struct WallOpening {
    pub opening: OpeningRef,
    pub is_door: bool,
    pub assembly_id: Option<ItemId>,
    pub along_m: f32,
    pub sill_m: f32,
    pub width_m: f32,
    pub height_m: f32,
    /// The window's outline shape (the elevation draws it accordingly). Always
    /// `Rectangular` for doors.
    pub shape: WindowShape,
}

/// Every opening placed on wall `wall_id` (windows then doors, placement order),
/// as [`WallOpening`]s. THE single gather over a wall's openings — both the
/// elevation view and [`redistribute_wall_openings`] build from this.
pub fn wall_openings(storey: &Storey, wall_id: ItemId) -> Vec<WallOpening> {
    let mut out = Vec::new();
    for w in &storey.windows {
        if let Some(OpeningPlacement::Wall {
            wall_id: wid,
            along_m,
            sill_m,
        }) = &w.placement
        {
            if *wid == wall_id {
                out.push(WallOpening {
                    opening: OpeningRef::Window(w.id),
                    is_door: false,
                    assembly_id: w.assembly_id,
                    along_m: *along_m,
                    sill_m: *sill_m,
                    width_m: w.width_m,
                    height_m: w.height_m,
                    shape: w.shape,
                });
            }
        }
    }
    for d in &storey.doors {
        if let Some(OpeningPlacement::Wall {
            wall_id: wid,
            along_m,
            sill_m,
        }) = &d.placement
        {
            if *wid == wall_id {
                out.push(WallOpening {
                    opening: OpeningRef::Door(d.id),
                    is_door: true,
                    assembly_id: d.assembly_id,
                    along_m: *along_m,
                    sill_m: *sill_m,
                    width_m: d.width_m,
                    height_m: d.height_m,
                    shape: WindowShape::Rectangular,
                });
            }
        }
    }
    out
}

/// The opening on `wall_id` whose elevation AABB (centred at `along_m`, sill at
/// `sill_m`, spanning `width_m × height_m`) contains the point
/// `(along_m, height_m)` — the elevation hit-test for selecting/grabbing an
/// opening. `None` when the point misses every opening on the wall.
pub fn opening_at(
    storey: &Storey,
    wall_id: ItemId,
    along_m: f32,
    height_m: f32,
) -> Option<OpeningRef> {
    wall_openings(storey, wall_id).into_iter().find_map(|o| {
        let within = along_m >= o.along_m - o.width_m / 2.0
            && along_m <= o.along_m + o.width_m / 2.0
            && height_m >= o.sill_m
            && height_m <= o.sill_m + o.height_m;
        within.then_some(o.opening)
    })
}

/// The height (m) of an opening — for centre-on-cursor dragging.
pub fn opening_height(storey: &Storey, r: OpeningRef) -> f32 {
    match r {
        OpeningRef::Window(id) => storey
            .windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| w.height_m)
            .unwrap_or(0.0),
        OpeningRef::Door(id) => storey
            .doors
            .iter()
            .find(|d| d.id == id)
            .map(|d| d.height_m)
            .unwrap_or(0.0),
    }
}

/// Which side(s) the leftover width (`overall − door`) becomes sidelight glass
/// in the "measure the whole doorset first" input mode (see
/// [`DoorSidelightsSpec::from_overall`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidelightLayout {
    /// Split the leftover evenly into a left and a right sidelight.
    Both,
    /// All the leftover goes to a single left sidelight (no right).
    LeftOnly,
    /// All the leftover goes to a single right sidelight (no left).
    RightOnly,
}

/// A structured door+sidelights assembly: a door panel with optional flanking
/// sidelights and a transom (top light) over the door. The composer (`compose_
/// door_sidelights`) lays these out and emits one `Door` + the side/top
/// `Window`s as one rigid assembly.
#[derive(Debug, Clone)]
pub struct DoorSidelightsSpec {
    pub door_width_m: f32,
    pub door_height_m: f32,
    /// Width of the left/right sidelight window, if present (`None`/`0` → absent).
    pub left_sidelight_m: Option<f32>,
    pub right_sidelight_m: Option<f32>,
    /// Sill height (m above floor) of the left/right sidelight — `None`/`0` sits
    /// it on the floor; a value lifts it (its top still aligns with the door top,
    /// so its height shrinks to `door_height − sill`).
    pub left_sill_m: Option<f32>,
    pub right_sill_m: Option<f32>,
    /// Height of the transom over the door, if present (`None`/`0` → absent). The
    /// transom spans the WHOLE assembly width (door + both sidelights).
    pub transom_m: Option<f32>,
    /// Make the transom a half-ellipse (arched/fanlight) instead of a rectangle.
    pub transom_semicircular: bool,
    pub window_type_id: ItemId,
    pub door_type_name: String,
}

impl DoorSidelightsSpec {
    /// Build a spec from a "measure the whole doorset first" reading: the OVERALL
    /// width/height of the assembly and the DOOR leaf within it — the four laser
    /// shots the assessor takes in the field. The leftover width
    /// (`overall_w − door_w`) becomes sidelight glass per `layout`; the leftover
    /// height (`overall_h − door_h`) becomes a full-width transom (absent when the
    /// door reaches the top). `sill_m` (when `Some(>0)`) lifts the present
    /// sidelight(s) off the floor — the composer clamps them to `[0, door_h]` and
    /// shrinks their height so their top still meets the door top.
    #[allow(clippy::too_many_arguments)]
    pub fn from_overall(
        overall_w: f32,
        overall_h: f32,
        door_w: f32,
        door_h: f32,
        layout: SidelightLayout,
        sill_m: Option<f32>,
        transom_semicircular: bool,
        window_type_id: ItemId,
        door_type_name: String,
    ) -> Self {
        let leftover = (overall_w - door_w).max(0.0);
        let (left, right) = match layout {
            SidelightLayout::Both => (Some(leftover / 2.0), Some(leftover / 2.0)),
            SidelightLayout::LeftOnly => (Some(leftover), None),
            SidelightLayout::RightOnly => (None, Some(leftover)),
        };
        // One sill for the whole assembly, applied to whichever sidelight(s) the
        // layout actually places (`and` keeps it only on the present side).
        let sill = sill_m.filter(|s| *s > 0.0);
        DoorSidelightsSpec {
            door_width_m: door_w,
            door_height_m: door_h,
            left_sidelight_m: left,
            right_sidelight_m: right,
            left_sill_m: left.and(sill),
            right_sill_m: right.and(sill),
            transom_m: Some(overall_h - door_h).filter(|d| *d > 0.0),
            transom_semicircular,
            window_type_id,
            door_type_name,
        }
    }
}

/// The length of the wall with `wall_id`, or `None` if absent/degenerate.
/// Public so the shell can display the span a "Full width" opening will take.
pub fn wall_length(storey: &Storey, wall_id: ItemId) -> Option<f32> {
    storey
        .walls
        .regions
        .iter()
        .find(|r| r.id == wall_id)
        .and_then(|r| r.base_frame())
        .map(|(_, _, len, _)| len as f32)
}

/// The wall FACE height (base → roofline top) at `along_m` — what a
/// "Full height" opening spans (sill 0 up to the wall top). `None` when the
/// wall is absent. Degenerate walls report 0 (mirrors `max_sill_for`).
pub fn wall_face_height(storey: &Storey, wall_id: ItemId, along_m: f32) -> Option<f32> {
    storey
        .walls
        .regions
        .iter()
        .find(|r| r.id == wall_id)
        .map(|r| r.max_sill_for(along_m as f64, 0.0) as f32)
}

/// The nearest OUTLINE wall to a world plan point and the along-distance of the
/// projected point from that wall's `exterior[0]`. Operates on the resolved
/// storey's walls (so it returns a real `wall_id`). `(wall_id, along_m, len_m)`
/// or `None` if no wall base edge is within `tol`.
///
/// Derived `InterStorey` pieces (storey junctions buried behind a neighbour
/// storey — `derive_inter_storey`'s stamp) are SKIPPED: a new opening tap must
/// never home onto a buried piece. On a derivation-split wall the tap lands on
/// the exposed remainder (its base runs along the same plan segment at the cut
/// height); a FULLY buried wall offers no tap target at all — an opening
/// through it would open into the dwelling's own neighbouring storey, a
/// modelling error, so the tap is refused rather than mis-hosted.
pub fn pick_wall_along(storey: &Storey, wx: f64, wy: f64, tol: f64) -> Option<(ItemId, f32, f32)> {
    let mut best: Option<(ItemId, f32, f32, f64)> = None;
    for r in &storey.walls.regions {
        if r.exposure == super::structural::WallExposure::InterStorey {
            continue; // buried behind a neighbour storey — never a tap target
        }
        let Some((origin, (ux, uy), len, _)) = r.base_frame() else {
            continue;
        };
        let (bx, by) = (origin.x + ux * len, origin.y + uy * len);
        let d = point_to_segment_distance(wx, wy, origin.x, origin.y, bx, by);
        if d <= tol && best.as_ref().is_none_or(|b| d < b.3) {
            let along = ((wx - origin.x) * ux + (wy - origin.y) * uy).clamp(0.0, len);
            best = Some((r.id, along as f32, len as f32, d));
        }
    }
    best.map(|(id, a, l, _)| (id, a, l))
}

/// The roof/ceiling region strictly containing a plan point, with the point
/// echoed back. `None` if the point is inside no region.
pub fn pick_roof_region(storey: &Storey, wx: f64, wy: f64) -> Option<(ItemId, (f32, f32))> {
    storey
        .ceilings
        .region_strictly_containing(domain::points::Point2f64::new(wx, wy))
        .map(|r| (r.id, (wx as f32, wy as f32)))
}

/// Add a rectangular window to a wall — the common case; a thin wrapper over
/// [`add_shaped_wall_window`].
pub fn add_wall_window(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    width_m: f32,
    height_m: f32,
    sill_m: f32,
    window_type_id: ItemId,
) -> Result<ItemId, OpeningError> {
    add_shaped_wall_window(
        storey,
        wall_id,
        along_m,
        width_m,
        height_m,
        sill_m,
        window_type_id,
        WindowShape::Rectangular,
    )
}

/// Add a window of a given `shape` to a wall at `along_m` (centre), then
/// redistribute the wall so nothing overlaps. Rolls back and returns
/// `WallTooShort` if it can't fit. Redistribution uses the bounding
/// `width_m`/`height_m`, so it's shape-agnostic.
#[allow(clippy::too_many_arguments)]
pub fn add_shaped_wall_window(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    width_m: f32,
    height_m: f32,
    sill_m: f32,
    window_type_id: ItemId,
    shape: WindowShape,
) -> Result<ItemId, OpeningError> {
    if width_m <= 0.0 || height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    if wall_length(storey, wall_id).is_none() {
        return Err(OpeningError::NoHostRegion);
    }
    let id = ItemId::default();
    storey.windows.push(Window {
        id,
        vertices: vec![],
        width_m,
        height_m,
        window_type_id,
        placement: Some(OpeningPlacement::Wall {
            wall_id,
            along_m,
            sill_m,
        }),
        wall_anchor: None,
        assembly_id: None,
        multi_storey: false,
        shape,
        overshading: Overshading::Average,
    });
    link_window(storey, wall_id, id);
    // Windows may STACK: if the requested row is horizontally full, place above /
    // below existing openings rather than refusing. (Doors stay at the floor.)
    let r = OpeningRef::Window(id);
    match place_window_with_vertical_fallback(storey, wall_id, r, height_m) {
        Ok(()) => Ok(id),
        Err(e) => {
            rollback_added(storey, wall_id, &[r]);
            Err(e)
        }
    }
}

/// Add a door to a wall at `along_m` (centre), then redistribute.
pub fn add_wall_door(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    width_m: f32,
    height_m: f32,
    kind: DoorKind,
    type_name: String,
    sill_m: f32,
) -> Result<ItemId, OpeningError> {
    if width_m <= 0.0 || height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    if wall_length(storey, wall_id).is_none() {
        return Err(OpeningError::NoHostRegion);
    }
    let id = ItemId::default();
    storey.doors.push(Door {
        id,
        type_name,
        vertices: vec![],
        photos: vec![],
        width_m,
        height_m,
        kind: Some(kind),
        placement: Some(OpeningPlacement::Wall {
            wall_id,
            along_m,
            sill_m,
        }),
        wall_anchor: None,
        assembly_id: None,
    });
    link_door(storey, wall_id, id);
    redistribute_or_rollback(storey, wall_id, &[OpeningRef::Door(id)])?;
    Ok(id)
}

/// Add a roof window at a free plan point inside a roof region (no
/// redistribution — rooflights don't line up like wall windows). Overshading
/// DEFAULTS to `VeryLittle` — a rooflight looks at the sky, so surrounding
/// buildings/trees rarely shade it (the assessor can still override).
pub fn add_roof_window(
    storey: &mut Storey,
    ceiling_region_id: ItemId,
    plan: (f32, f32),
    width_m: f32,
    height_m: f32,
    window_type_id: ItemId,
) -> Result<ItemId, OpeningError> {
    if width_m <= 0.0 || height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    let Some(r) = storey
        .ceilings
        .regions
        .iter_mut()
        .find(|r| r.id == ceiling_region_id)
    else {
        return Err(OpeningError::NoHostRegion);
    };
    let id = ItemId::default();
    r.opes.windows.push(id);
    storey.windows.push(Window {
        id,
        vertices: vec![],
        width_m,
        height_m,
        window_type_id,
        placement: Some(OpeningPlacement::Roof {
            ceiling_region_id,
            plan,
        }),
        wall_anchor: None,
        assembly_id: None,
        multi_storey: false,
        shape: WindowShape::Rectangular,
        // Rooflights face the sky: obstacles rarely overshade them.
        overshading: Overshading::VeryLittle,
    });
    Ok(id)
}

/// Add a window spanning the WHOLE wall length at a TYPED height — the
/// "Full width" checkbox with the height still measured. Width = the wall
/// length (centred, no end margins), sill at `sill_m`. NOT redistributed: it
/// spans the entire wall, so it requires an EMPTY wall — a full-width opening
/// plus anything else can never fit, refused as `WallTooShort` here (same rule
/// as [`add_full_wall_door`]).
pub fn add_full_width_window(
    storey: &mut Storey,
    wall_id: ItemId,
    height_m: f32,
    sill_m: f32,
    window_type_id: ItemId,
) -> Result<ItemId, OpeningError> {
    let Some(len) = wall_length(storey, wall_id) else {
        return Err(OpeningError::NoHostRegion);
    };
    if height_m <= 0.0 || len <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    let existing = wall_openings(storey, wall_id);
    if !existing.is_empty() {
        // A full-width window plus anything else can never fit the wall.
        let others: f32 = existing.iter().map(|o| o.width_m).sum();
        return Err(OpeningError::WallTooShort {
            needed_m: len + others,
            available_m: len,
        });
    }
    let id = ItemId::default();
    storey.windows.push(Window {
        id,
        vertices: vec![],
        width_m: len,
        height_m,
        window_type_id,
        placement: Some(OpeningPlacement::Wall {
            wall_id,
            along_m: len / 2.0,
            sill_m,
        }),
        wall_anchor: None,
        assembly_id: None,
        multi_storey: false,
        shape: WindowShape::Rectangular,
        overshading: Overshading::Average,
    });
    link_window(storey, wall_id, id);
    Ok(id)
}

/// Add a window that fills the WHOLE wall face — a "window wall" (Full width +
/// Full height). Width = the wall length, sill 0; `height_m` is the face
/// height (the caller resolves it, e.g. via [`wall_face_height`]). Requires an
/// EMPTY wall (checked in [`add_full_width_window`]).
pub fn add_full_wall_window(
    storey: &mut Storey,
    wall_id: ItemId,
    height_m: f32,
    window_type_id: ItemId,
) -> Result<ItemId, OpeningError> {
    add_full_width_window(storey, wall_id, height_m, 0.0, window_type_id)
}

/// Add a window spanning the FULL face height at a TYPED width — the
/// "Full height" checkbox with the width still measured. Sill 0, height = the
/// face height at `along_m` (see [`wall_face_height`]); the width takes the
/// NORMAL placement rules — end margins + auto-redistribution against the
/// wall's other openings, rolled back atomically when nothing fits. Unlike the
/// full-WIDTH ops this does NOT require an empty wall: at a typed width it
/// coexists with other openings whenever redistribution fits it.
pub fn add_full_height_window(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    width_m: f32,
    window_type_id: ItemId,
) -> Result<ItemId, OpeningError> {
    let Some(height_m) = wall_face_height(storey, wall_id, along_m) else {
        return Err(OpeningError::NoHostRegion);
    };
    add_wall_window(
        storey,
        wall_id,
        along_m,
        width_m,
        height_m,
        0.0,
        window_type_id,
    )
}

/// Add a door spanning the FULL face height at a TYPED width — sill 0, height
/// = the face height at `along_m`; the width under the normal margin +
/// redistribution rules (see [`add_full_height_window`]).
pub fn add_full_height_door(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    width_m: f32,
    kind: DoorKind,
    type_name: String,
) -> Result<ItemId, OpeningError> {
    let Some(height_m) = wall_face_height(storey, wall_id, along_m) else {
        return Err(OpeningError::NoHostRegion);
    };
    add_wall_door(
        storey, wall_id, along_m, width_m, height_m, kind, type_name, 0.0,
    )
}

/// Add a door that fills the WHOLE wall length — e.g. a porch door as wide as
/// its wall. Width = the wall length (centred, no end margins); `height_m`,
/// `kind`, `type_name` and `sill_m` mean the same as for [`add_wall_door`].
/// NOT redistributed: it spans the entire wall, so it requires an EMPTY wall —
/// any existing opening would be overlapped. Callers route here explicitly
/// (the placement box's "Full width" checkbox); the end-margin path would
/// always refuse a door as wide as its wall (width + 2 margins > wall length).
pub fn add_full_wall_door(
    storey: &mut Storey,
    wall_id: ItemId,
    height_m: f32,
    kind: DoorKind,
    type_name: String,
    sill_m: f32,
) -> Result<ItemId, OpeningError> {
    let Some(len) = wall_length(storey, wall_id) else {
        return Err(OpeningError::NoHostRegion);
    };
    if height_m <= 0.0 || len <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    let existing = wall_openings(storey, wall_id);
    if !existing.is_empty() {
        // A full-wall door plus anything else can never fit the wall.
        let others: f32 = existing.iter().map(|o| o.width_m).sum();
        return Err(OpeningError::WallTooShort {
            needed_m: len + others,
            available_m: len,
        });
    }
    let id = ItemId::default();
    storey.doors.push(Door {
        id,
        type_name,
        vertices: vec![],
        photos: vec![],
        width_m: len,
        height_m,
        kind: Some(kind),
        placement: Some(OpeningPlacement::Wall {
            wall_id,
            along_m: len / 2.0,
            sill_m,
        }),
        wall_anchor: None,
        assembly_id: None,
    });
    link_door(storey, wall_id, id);
    Ok(id)
}

/// Add a MULTI-STOREY wall window at `along_m`/`sill_m` — a window that may span
/// UP past this storey's ceiling (its roofline top-clamp is lifted; see
/// [`Window::multi_storey`]). Placed at the requested sill + full height, then
/// horizontally redistributed against the wall's other openings (no vertical-stack
/// fallback — the sill is meaningful for a spanning window). The "Windows"
/// checklist gate is what validates it doesn't hang off the building.
pub fn add_multi_storey_window(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    width_m: f32,
    height_m: f32,
    sill_m: f32,
    window_type_id: ItemId,
) -> Result<ItemId, OpeningError> {
    if width_m <= 0.0 || height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    if wall_length(storey, wall_id).is_none() {
        return Err(OpeningError::NoHostRegion);
    }
    let id = ItemId::default();
    storey.windows.push(Window {
        id,
        vertices: vec![],
        width_m,
        height_m,
        window_type_id,
        placement: Some(OpeningPlacement::Wall {
            wall_id,
            along_m,
            sill_m,
        }),
        wall_anchor: None,
        assembly_id: None,
        multi_storey: true,
        shape: WindowShape::Rectangular,
        overshading: Overshading::Average,
    });
    link_window(storey, wall_id, id);
    redistribute_or_rollback(storey, wall_id, &[OpeningRef::Window(id)])?;
    Ok(id)
}

/// Reposition a ROOF window to a new plan point, re-homing it to the roof region
/// under that point (so it derives onto the right face). Keeps the old region if
/// the point is off every roof. No-op if `window_id` isn't a roof window. `opes`
/// membership is rebuilt at the resolve tail, so it isn't touched here.
pub fn move_roof_window(storey: &mut Storey, window_id: ItemId, new_plan: (f32, f32)) {
    let new_region = storey
        .ceilings
        .region_strictly_containing(domain::points::Point2f64::new(
            new_plan.0 as f64,
            new_plan.1 as f64,
        ))
        .map(|r| r.id);
    if let Some(w) = storey.windows.iter_mut().find(|w| w.id == window_id) {
        if let Some(OpeningPlacement::Roof {
            ceiling_region_id,
            plan,
        }) = &mut w.placement
        {
            *plan = new_plan;
            if let Some(rid) = new_region {
                *ceiling_region_id = rid;
            }
        }
    }
}

/// The four PLAN corners (CCW) of a roof window's footprint. The stored
/// `width_m`/`height_m` are CEILING-PLANE measurements: **height runs UP the
/// slope, width runs ACROSS it**. Laid out in the region's plane and projected
/// straight down, the up-slope extent foreshortens by `cos(pitch)` while the
/// across-slope extent is unchanged — so a rooflight the assessor measured on
/// the slope keeps that true size instead of overshooting in plan. On a FLAT
/// region (or one with no resolvable plane) this degenerates to the
/// world-axis-aligned box (`cos = 1`, width → x, height → y), matching the
/// legacy footprint exactly. Shared by the vertex derivation, the tap
/// hit-test, and the fit check so all three see one footprint.
pub(crate) fn roof_window_plan_corners(
    region: &super::structural::CeilingRegion,
    center: (f64, f64),
    width_m: f64,
    height_m: f64,
) -> [(f64, f64); 4] {
    let (cx, cy) = center;
    let (hw, hh) = (width_m * 0.5, height_m * 0.5);
    // Plane gradient `(gx, gy)` = tan(pitch) in the steepest-descent direction.
    let (gx, gy) = super::structural::region_plane(region)
        .map(|(_, gx, gy)| (gx, gy))
        .unwrap_or((0.0, 0.0));
    let g2 = gx * gx + gy * gy;
    if g2 <= 1e-12 {
        // Flat / degenerate plane: width along +x, height along +y (legacy box).
        return [
            (cx - hw, cy - hh),
            (cx + hw, cy - hh),
            (cx + hw, cy + hh),
            (cx - hw, cy + hh),
        ];
    }
    let gmag = g2.sqrt();
    let down = (gx / gmag, gy / gmag); // up/down-slope plan unit direction
    let across = (-down.1, down.0); // perpendicular — across the slope
    let cos_pitch = 1.0 / (1.0 + g2).sqrt();
    // HEIGHT foreshortens along the slope; WIDTH is unchanged across it.
    let up = (down.0 * hh * cos_pitch, down.1 * hh * cos_pitch);
    let ac = (across.0 * hw, across.1 * hw);
    [
        (cx - ac.0 - up.0, cy - ac.1 - up.1),
        (cx + ac.0 - up.0, cy + ac.1 - up.1),
        (cx + ac.0 + up.0, cy + ac.1 + up.1),
        (cx - ac.0 + up.0, cy - ac.1 + up.1),
    ]
}

/// Point-in-quad by ray casting — winding-agnostic, so it holds whichever way
/// the slope-projected corners wind.
fn point_in_quad(px: f64, py: f64, quad: &[(f64, f64); 4]) -> bool {
    let mut inside = false;
    let mut j = 3;
    for i in 0..4 {
        let (xi, yi) = quad[i];
        let (xj, yj) = quad[j];
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// The ROOF window whose CEILING-PLANE footprint contains `(x, y)` in plan, if
/// any (topmost — the last added — wins). Uses the same slope-projected
/// footprint as the derived vertices ([`roof_window_plan_corners`]) so the
/// drawn shape and the pick target stay identical on a sloped ceiling. Used to
/// pick a rooflight to drag/delete in the plan view.
pub fn roof_window_at(storey: &Storey, x: f32, y: f32) -> Option<ItemId> {
    storey.windows.iter().rev().find_map(|w| {
        let Some(OpeningPlacement::Roof { plan, .. }) = &w.placement else {
            return None;
        };
        let region = storey
            .ceilings
            .region_strictly_containing(domain::points::Point2f64::new(
                plan.0 as f64,
                plan.1 as f64,
            ))?;
        let corners = roof_window_plan_corners(
            region,
            (plan.0 as f64, plan.1 as f64),
            w.width_m as f64,
            w.height_m as f64,
        );
        point_in_quad(x as f64, y as f64, &corners).then_some(w.id)
    })
}

/// Roof windows whose ceiling-plane footprint spills off the single ceiling
/// region they sit on — a rooflight larger than, or hanging past the edge of,
/// its sloped surface. A footprint "fits" when all four plan corners
/// ([`roof_window_plan_corners`]) still resolve to the SAME host region as the
/// placement centre; a corner that lands on a neighbouring surface (across a
/// ridge) or off every region means the rooflight overshoots. Surfaced through
/// the "Windows" mapping gate so the assessor repositions, resizes, or rotates
/// it to fit — the stored size/area is never silently changed. Empty = all fit.
pub fn oversized_roof_windows(storey: &Storey) -> Vec<ItemId> {
    let mut out = Vec::new();
    for w in &storey.windows {
        let Some(OpeningPlacement::Roof { plan, .. }) = &w.placement else {
            continue;
        };
        let centre = (plan.0 as f64, plan.1 as f64);
        let Some(host) = storey
            .ceilings
            .region_strictly_containing(domain::points::Point2f64::new(centre.0, centre.1))
        else {
            continue; // centre off every region (transient) — not a fit failure
        };
        let host_id = host.id;
        let corners = roof_window_plan_corners(host, centre, w.width_m as f64, w.height_m as f64);
        // Nudge each corner a hair toward the placement centre before the strict
        // containment test: a rooflight sized flush to a ridge/eave has corners
        // conceptually ON the shared boundary, which the epsilon-free predicate
        // would read as `None` (off the region) after ~1e-7 m boolean-op / f32
        // drift. `PROFILE_EPS_M` (0.1 mm) is above that noise and far below any
        // real overshoot, so only a corner WELL past the boundary still flags.
        let nudged = |(x, y): (f64, f64)| -> (f64, f64) {
            let (dx, dy) = (centre.0 - x, centre.1 - y);
            let d = (dx * dx + dy * dy).sqrt();
            if d <= f64::EPSILON {
                (x, y)
            } else {
                let t = (super::structural::PROFILE_EPS_M / d).min(1.0);
                (x + dx * t, y + dy * t)
            }
        };
        let fits = corners.iter().all(|&corner| {
            let (x, y) = nudged(corner);
            storey
                .ceilings
                .region_strictly_containing(domain::points::Point2f64::new(x, y))
                .map(|r| r.id)
                == Some(host_id)
        });
        if !fits {
            out.push(w.id);
        }
    }
    out
}

/// Compose a door+sidelights assembly on a wall at `along_m` (centre): one
/// `Door` (panel) + flanking sidelight `Window`s + an optional transom, all
/// sharing a fresh `assembly_id`, laid out left→right. Redistributes the wall
/// treating the assembly as one rigid unit. Returns the `assembly_id`.
pub fn compose_door_sidelights(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    spec: DoorSidelightsSpec,
) -> Result<ItemId, OpeningError> {
    compose_door_sidelights_at(storey, wall_id, along_m, spec, None)
}

/// [`compose_door_sidelights`] with the members fn's layout switch threaded
/// through: `face_m = Some` is the full-height layout (see
/// [`push_door_sidelights_members`]).
fn compose_door_sidelights_at(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    spec: DoorSidelightsSpec,
    face_m: Option<f32>,
) -> Result<ItemId, OpeningError> {
    if spec.door_width_m <= 0.0 || spec.door_height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    if wall_length(storey, wall_id).is_none() {
        return Err(OpeningError::NoHostRegion);
    }
    let (assembly_id, added) =
        push_door_sidelights_members(storey, wall_id, along_m, &spec, face_m);
    redistribute_or_rollback(storey, wall_id, &added)?;
    Ok(assembly_id)
}

/// Push the members of a door+sidelights assembly (sidelight windows, transom,
/// door panel) centred at `along_m`, all under one fresh assembly id. LAYOUT
/// ONLY — no feasibility check: [`compose_door_sidelights`] redistributes (and
/// rolls back) afterwards; the full-width composer has already sized the
/// members to exactly the wall. Returns the assembly id + the pushed members.
///
/// `face_m = Some(face)` switches to the FULL-HEIGHT layout: the sidelights
/// run floor → the face (roofline) instead of top-aligning with the door, and
/// the transom — the strip between the door top and the face — spans the DOOR
/// width only (the full-height sidelights already own their columns). `None`
/// is the normal layout: sidelight tops at the door top, transom across the
/// whole assembly.
fn push_door_sidelights_members(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    spec: &DoorSidelightsSpec,
    face_m: Option<f32>,
) -> (ItemId, Vec<OpeningRef>) {
    let assembly_id = ItemId::default();
    let left_w = spec.left_sidelight_m.unwrap_or(0.0).max(0.0);
    let right_w = spec.right_sidelight_m.unwrap_or(0.0).max(0.0);
    let total_w = left_w + spec.door_width_m + right_w;
    let assembly_left = along_m - total_w / 2.0;
    let mut added: Vec<OpeningRef> = Vec::new();
    let mut place_window =
        |storey: &mut Storey, centre: f32, w: f32, h: f32, sill: f32, shape: WindowShape| {
            let id = ItemId::default();
            storey.windows.push(Window {
                id,
                vertices: vec![],
                width_m: w,
                height_m: h,
                window_type_id: spec.window_type_id,
                placement: Some(OpeningPlacement::Wall {
                    wall_id,
                    along_m: centre,
                    sill_m: sill,
                }),
                wall_anchor: None,
                assembly_id: Some(assembly_id),
                multi_storey: false,
                shape,
                overshading: Overshading::Average,
            });
            link_window(storey, wall_id, id);
            added.push(OpeningRef::Window(id));
        };
    // A sidelight's sill lifts it off the floor; its top aligns with the door
    // top — or the FACE (roofline) in the full-height layout — so its height
    // shrinks to `top − sill`.
    let side_top = face_m.unwrap_or(spec.door_height_m);
    let sidelight_sill = |s: Option<f32>| s.unwrap_or(0.0).clamp(0.0, side_top);
    if left_w > 0.0 {
        let sill = sidelight_sill(spec.left_sill_m);
        place_window(
            storey,
            assembly_left + left_w / 2.0,
            left_w,
            (side_top - sill).max(0.01),
            sill,
            WindowShape::Rectangular,
        );
    }
    if right_w > 0.0 {
        let sill = sidelight_sill(spec.right_sill_m);
        place_window(
            storey,
            assembly_left + left_w + spec.door_width_m + right_w / 2.0,
            right_w,
            (side_top - sill).max(0.01),
            sill,
            WindowShape::Rectangular,
        );
    }
    let door_centre = assembly_left + left_w + spec.door_width_m / 2.0;
    if let Some(th) = spec.transom_m.filter(|t| *t > 0.0) {
        // The transom sits on the door top: across the WHOLE assembly width
        // normally, or over the DOOR only at full height (the sidelights
        // already reach the face); semicircular → a half-ellipse fanlight.
        let (t_centre, t_width) = if face_m.is_some() {
            (door_centre, spec.door_width_m)
        } else {
            (along_m, total_w)
        };
        place_window(
            storey,
            t_centre,
            t_width,
            th,
            spec.door_height_m,
            if spec.transom_semicircular {
                WindowShape::Arched
            } else {
                WindowShape::Rectangular
            },
        );
    }
    let door_id = ItemId::default();
    storey.doors.push(Door {
        id: door_id,
        type_name: spec.door_type_name.clone(),
        vertices: vec![],
        photos: vec![],
        width_m: spec.door_width_m,
        height_m: spec.door_height_m,
        kind: Some(DoorKind::AssemblyPanel),
        placement: Some(OpeningPlacement::Wall {
            wall_id,
            along_m: door_centre,
            sill_m: 0.0,
        }),
        wall_anchor: None,
        assembly_id: Some(assembly_id),
    });
    link_door(storey, wall_id, door_id);
    added.push(OpeningRef::Door(door_id));
    (assembly_id, added)
}

/// Rewrite a composite spec to run FULL HEIGHT — the assembly as a whole
/// (door + sidelights + transom) fills floor → `face_m`: the DOOR LEAF keeps
/// its typed height (clamped to the face), the transom becomes exactly the
/// remaining strip above it (`face − door`; gone when the door reaches the
/// face), and the sidelight sills drop to 0 — the members layout then runs
/// the sidelights all the way up to the face (see
/// [`push_door_sidelights_members`]).
fn spec_at_full_height(mut spec: DoorSidelightsSpec, face_m: f32) -> DoorSidelightsSpec {
    spec.door_height_m = spec.door_height_m.min(face_m);
    spec.left_sill_m = None;
    spec.right_sill_m = None;
    spec.transom_m = Some(face_m - spec.door_height_m).filter(|t| *t > 1e-4);
    spec
}

/// Compose a door+sidelights assembly that spans the WHOLE wall length — the
/// composite "Full width": no end margins, the assembly's total width equals
/// the wall length exactly. The DOOR width is as typed in the spec; the
/// leftover (`wall − door`) goes to the PRESENT sidelight(s) — all of it to a
/// lone side, an equal split when both (or neither) are present — mirroring
/// [`DoorSidelightsSpec::from_overall`]'s layout rule. Requires an EMPTY wall
/// (same rule as [`add_full_wall_door`]): full width plus anything else can
/// never fit, refused as `WallTooShort`. Not redistributed. Heights/sills/
/// transom come from the spec unchanged.
pub fn compose_full_width_door_sidelights(
    storey: &mut Storey,
    wall_id: ItemId,
    spec: DoorSidelightsSpec,
) -> Result<ItemId, OpeningError> {
    compose_full_width_door_sidelights_at(storey, wall_id, spec, None)
}

/// [`compose_full_width_door_sidelights`] with the members fn's layout switch
/// threaded through: `face_m = Some` is the full-height layout (see
/// [`push_door_sidelights_members`]).
fn compose_full_width_door_sidelights_at(
    storey: &mut Storey,
    wall_id: ItemId,
    mut spec: DoorSidelightsSpec,
    face_m: Option<f32>,
) -> Result<ItemId, OpeningError> {
    if spec.door_width_m <= 0.0 || spec.door_height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    let Some(len) = wall_length(storey, wall_id) else {
        return Err(OpeningError::NoHostRegion);
    };
    let existing = wall_openings(storey, wall_id);
    if !existing.is_empty() {
        // A full-width assembly plus anything else can never fit the wall.
        let others: f32 = existing.iter().map(|o| o.width_m).sum();
        return Err(OpeningError::WallTooShort {
            needed_m: len + others,
            available_m: len,
        });
    }
    let leftover = len - spec.door_width_m;
    if leftover < 0.0 {
        return Err(OpeningError::WallTooShort {
            needed_m: spec.door_width_m,
            available_m: len,
        });
    }
    let present = |w: Option<f32>| w.is_some_and(|w| w > 0.0);
    match (
        present(spec.left_sidelight_m),
        present(spec.right_sidelight_m),
    ) {
        (true, false) => {
            spec.left_sidelight_m = Some(leftover);
            spec.right_sidelight_m = None;
        }
        (false, true) => {
            spec.left_sidelight_m = None;
            spec.right_sidelight_m = Some(leftover);
        }
        // Both present — or neither (a bare door): even split, mirroring
        // `from_overall`'s `Both` fallback.
        _ => {
            spec.left_sidelight_m = Some(leftover / 2.0);
            spec.right_sidelight_m = Some(leftover / 2.0);
        }
    }
    let (assembly_id, _) = push_door_sidelights_members(storey, wall_id, len / 2.0, &spec, face_m);
    Ok(assembly_id)
}

/// Compose a door+sidelights assembly filling the FULL face height with the
/// widths as typed — the composite "Full height": the assembly as a whole
/// spans floor → roofline at `along_m`. The DOOR LEAF keeps its typed height
/// (clamped to the face); the sidelights run sill 0 all the way up to the
/// face, and the transom fills exactly the strip left above the door
/// (`face − door`, over the door width — degenerating to nothing when the
/// door reaches the face). Width under the NORMAL margin + redistribution
/// rules (see [`compose_door_sidelights`]).
pub fn compose_full_height_door_sidelights(
    storey: &mut Storey,
    wall_id: ItemId,
    along_m: f32,
    spec: DoorSidelightsSpec,
) -> Result<ItemId, OpeningError> {
    let Some(face) = wall_face_height(storey, wall_id, along_m) else {
        return Err(OpeningError::NoHostRegion);
    };
    compose_door_sidelights_at(
        storey,
        wall_id,
        along_m,
        spec_at_full_height(spec, face),
        Some(face),
    )
}

/// Compose a door+sidelights assembly filling the WHOLE wall face — both
/// checkboxes: full width (empty-wall rule, door width typed, sidelights share
/// the leftover — see [`compose_full_width_door_sidelights`]) AND full height
/// (door at its typed height, sidelights floor → roofline, the transom the
/// strip above the door — see [`compose_full_height_door_sidelights`]).
pub fn compose_full_wall_door_sidelights(
    storey: &mut Storey,
    wall_id: ItemId,
    spec: DoorSidelightsSpec,
) -> Result<ItemId, OpeningError> {
    let Some(len) = wall_length(storey, wall_id) else {
        return Err(OpeningError::NoHostRegion);
    };
    let Some(face) = wall_face_height(storey, wall_id, len / 2.0) else {
        return Err(OpeningError::NoHostRegion);
    };
    compose_full_width_door_sidelights_at(
        storey,
        wall_id,
        spec_at_full_height(spec, face),
        Some(face),
    )
}

/// Move an opening to a new along-centre and sill. An assembly member moves the
/// WHOLE assembly rigidly. Horizontal change auto-redistributes (no overlap);
/// vertical (sill) is set directly. No-op if the opening isn't on a wall.
pub fn move_opening(
    storey: &mut Storey,
    opening: OpeningRef,
    new_along_m: f32,
    new_sill_m: f32,
) -> Result<(), OpeningError> {
    let Some((wall_id, old_along, old_sill, assembly)) = wall_placement_of(storey, opening) else {
        return Ok(());
    };
    // Clamp the dragged opening's TOP under the wall roofline at its new along
    // (and its bottom at the floor). Lives HERE so every drag path — the review
    // elevation AND the 2D placement view — shares one clamp via this single
    // mutation, instead of each re-implementing it (only the review path did). A
    // MULTI-STOREY window is exempt from the roofline cap (it spans up); only its
    // floor is clamped.
    let multi_storey = matches!(opening, OpeningRef::Window(id)
        if storey.windows.iter().any(|w| w.id == id && w.multi_storey));
    let new_sill_m = if multi_storey {
        new_sill_m.max(0.0)
    } else {
        let h = opening_height(storey, opening) as f64;
        let max_sill = storey
            .walls
            .regions
            .iter()
            .find(|r| r.id == wall_id)
            .map(|r| r.max_sill_for(new_along_m as f64, h) as f32)
            .unwrap_or(f32::INFINITY);
        new_sill_m.clamp(0.0, max_sill.max(0.0))
    };
    let (d_along, d_sill) = (new_along_m - old_along, new_sill_m - old_sill);
    let shift = |p: &mut Option<OpeningPlacement>| {
        if let Some(OpeningPlacement::Wall {
            along_m, sill_m, ..
        }) = p
        {
            *along_m += d_along;
            *sill_m = (*sill_m + d_sill).max(0.0);
        }
    };
    match assembly {
        Some(aid) => {
            for w in storey
                .windows
                .iter_mut()
                .filter(|w| w.assembly_id == Some(aid))
            {
                shift(&mut w.placement);
            }
            for dr in storey
                .doors
                .iter_mut()
                .filter(|d| d.assembly_id == Some(aid))
            {
                shift(&mut dr.placement);
            }
        }
        None => match opening {
            OpeningRef::Window(id) => {
                if let Some(w) = storey.windows.iter_mut().find(|w| w.id == id) {
                    shift(&mut w.placement);
                }
            }
            OpeningRef::Door(id) => {
                if let Some(dr) = storey.doors.iter_mut().find(|d| d.id == id) {
                    shift(&mut dr.placement);
                }
            }
        },
    }
    // Pin the moved opening: it holds the spot the assessor dragged it to while
    // the rest auto-arrange minimally around it. (Pinning a member pins its whole
    // assembly.) If pinning it there is infeasible (e.g. dragged so hard against a
    // neighbour and the wall that nothing fits), fall back to a full re-arrange
    // that may nudge the dragged one too — better than refusing the move.
    redistribute_wall_openings_pinned(storey, wall_id, &[opening])
        .or_else(|_| redistribute_wall_openings(storey, wall_id))
}

/// Resize a WALL opening in place (width/height), under the same feasibility
/// rules as placement: end margins + auto-redistribution, the resized opening
/// PINNED at its along while the rest flow around it (if pinning is infeasible
/// a full re-arrange may nudge it too — same fallback as [`move_opening`]).
/// When even touching openings can't fit, the resize is refused with
/// `WallTooShort`: the opening's size and sill are restored and the storey is
/// left exactly as it was (redistribution applies nothing on error) — the same
/// rollback discipline as the add ops. A full-wall opening resized SMALLER
/// re-enters the margin regime here via the normal redistribute path. No-op
/// `Ok` if the opening isn't on a wall (mirrors [`move_opening`]); an assembly
/// member resizes just that member (its assembly re-packs as one unit).
pub fn resize_opening(
    storey: &mut Storey,
    opening: OpeningRef,
    new_width_m: f32,
    new_height_m: f32,
) -> Result<(), OpeningError> {
    if new_width_m <= 0.0 || new_height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    let Some((wall_id, along, old_sill, _)) = wall_placement_of(storey, opening) else {
        return Ok(());
    };
    let size_of = |storey: &Storey| match opening {
        OpeningRef::Window(id) => storey
            .windows
            .iter()
            .find(|w| w.id == id)
            .map(|w| (w.width_m, w.height_m)),
        OpeningRef::Door(id) => storey
            .doors
            .iter()
            .find(|d| d.id == id)
            .map(|d| (d.width_m, d.height_m)),
    };
    let Some((old_w, old_h)) = size_of(storey) else {
        return Ok(());
    };
    let set_size = |storey: &mut Storey, w: f32, h: f32| match opening {
        OpeningRef::Window(id) => {
            if let Some(win) = storey.windows.iter_mut().find(|x| x.id == id) {
                win.width_m = w;
                win.height_m = h;
            }
        }
        OpeningRef::Door(id) => {
            if let Some(d) = storey.doors.iter_mut().find(|x| x.id == id) {
                d.width_m = w;
                d.height_m = h;
            }
        }
    };
    set_size(storey, new_width_m, new_height_m);
    // Keep a now-taller opening under the wall roofline by lowering its sill —
    // the same clamp as `move_opening`; a multi-storey window is exempt (it
    // spans up past the roofline by design).
    let multi_storey = matches!(opening, OpeningRef::Window(id)
        if storey.windows.iter().any(|w| w.id == id && w.multi_storey));
    if !multi_storey {
        let max_sill = storey
            .walls
            .regions
            .iter()
            .find(|r| r.id == wall_id)
            .map(|r| r.max_sill_for(along as f64, new_height_m as f64) as f32)
            .unwrap_or(f32::INFINITY);
        let clamped = old_sill.clamp(0.0, max_sill.max(0.0));
        if clamped != old_sill {
            set_wall_opening_sill(storey, opening, clamped);
        }
    }
    match redistribute_wall_openings_pinned(storey, wall_id, &[opening])
        .or_else(|_| redistribute_wall_openings(storey, wall_id))
    {
        Ok(()) => Ok(()),
        Err(e) => {
            set_size(storey, old_w, old_h);
            set_wall_opening_sill(storey, opening, old_sill);
            Err(e)
        }
    }
}

/// Resize a WALL opening to span the WHOLE wall length — the edit box's "Full
/// width": width = the wall length, re-centred, height set to `new_height_m`
/// (for Full height the caller passes the face height). Requires the opening
/// to be ALONE on its wall — full width plus anything else can never fit — so
/// it refuses with `WallTooShort` (storey untouched) when the wall carries any
/// other opening. No redistribution (nothing else on the wall); the sill is
/// clamped under the roofline like [`resize_opening`] (multi-storey windows
/// exempt). No-op `Ok` if the opening isn't on a wall.
pub fn resize_opening_full_width(
    storey: &mut Storey,
    opening: OpeningRef,
    new_height_m: f32,
) -> Result<(), OpeningError> {
    if new_height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    let Some((wall_id, _, old_sill, _)) = wall_placement_of(storey, opening) else {
        return Ok(());
    };
    let Some(len) = wall_length(storey, wall_id) else {
        return Err(OpeningError::NoHostRegion);
    };
    let others: f32 = wall_openings(storey, wall_id)
        .iter()
        .filter(|o| o.opening != opening)
        .map(|o| o.width_m)
        .sum();
    if others > 0.0 {
        return Err(OpeningError::WallTooShort {
            needed_m: len + others,
            available_m: len,
        });
    }
    // Keep the (possibly taller) opening under the roofline — same clamp as
    // `resize_opening`; multi-storey windows span past it by design.
    let multi_storey = matches!(opening, OpeningRef::Window(id)
        if storey.windows.iter().any(|w| w.id == id && w.multi_storey));
    let sill = if multi_storey {
        old_sill
    } else {
        let max_sill = storey
            .walls
            .regions
            .iter()
            .find(|r| r.id == wall_id)
            .map(|r| r.max_sill_for((len / 2.0) as f64, new_height_m as f64) as f32)
            .unwrap_or(f32::INFINITY);
        old_sill.clamp(0.0, max_sill.max(0.0))
    };
    let place = |placement: &mut Option<OpeningPlacement>| {
        if let Some(OpeningPlacement::Wall {
            along_m, sill_m, ..
        }) = placement
        {
            *along_m = len / 2.0;
            *sill_m = sill;
        }
    };
    match opening {
        OpeningRef::Window(id) => {
            if let Some(w) = storey.windows.iter_mut().find(|x| x.id == id) {
                w.width_m = len;
                w.height_m = new_height_m;
                place(&mut w.placement);
            }
        }
        OpeningRef::Door(id) => {
            if let Some(d) = storey.doors.iter_mut().find(|x| x.id == id) {
                d.width_m = len;
                d.height_m = new_height_m;
                place(&mut d.placement);
            }
        }
    }
    Ok(())
}

/// Retype an existing WINDOW in place: point it at a (possibly different)
/// shared window type and set its per-window overshading. Geometry (size,
/// placement, shape) is untouched — pair with [`resize_opening`] for size
/// edits. No-op if `window_id` isn't in the storey.
pub fn set_window_attributes(
    storey: &mut Storey,
    window_id: ItemId,
    window_type_id: ItemId,
    overshading: Overshading,
) {
    if let Some(w) = storey.windows.iter_mut().find(|w| w.id == window_id) {
        w.window_type_id = window_type_id;
        w.overshading = overshading;
    }
}

/// Delete an opening (the whole assembly if it's a member), then redistribute
/// its wall.
pub fn delete_opening(storey: &mut Storey, opening: OpeningRef) {
    let info = wall_placement_of(storey, opening);
    let assembly = info.as_ref().and_then(|(_, _, _, a)| *a);
    let on_wall = info.as_ref().map(|(w, _, _, _)| *w);
    match assembly {
        Some(aid) => {
            storey.windows.retain(|w| w.assembly_id != Some(aid));
            storey.doors.retain(|d| d.assembly_id != Some(aid));
        }
        None => match opening {
            OpeningRef::Window(id) => storey.windows.retain(|w| w.id != id),
            OpeningRef::Door(id) => storey.doors.retain(|d| d.id != id),
        },
    }
    // Scrub opes refs everywhere (cheap; ids are unique).
    let win_ids: std::collections::HashSet<_> = storey.windows.iter().map(|w| w.id).collect();
    let door_ids: std::collections::HashSet<_> = storey.doors.iter().map(|d| d.id).collect();
    for r in &mut storey.walls.regions {
        r.opes.windows.retain(|id| win_ids.contains(id));
        r.opes.doors.retain(|id| door_ids.contains(id));
    }
    for r in &mut storey.ceilings.regions {
        r.opes.windows.retain(|id| win_ids.contains(id));
        r.opes.doors.retain(|id| door_ids.contains(id));
    }
    if let Some(wall_id) = on_wall {
        let _ = redistribute_wall_openings(storey, wall_id);
    }
}

/// A packing unit: a door+sidelights assembly is ONE rigid unit (its bounding
/// box), everything else a singleton. `centre`/`width` are the along-axis box;
/// `[vmin, vmax] = [sill, sill + height]` the vertical band. A `pinned` unit
/// holds its along (a dragged opening keeps the spot the assessor put it in while
/// the rest flow around it).
struct PackUnit {
    members: Vec<(OpeningRef, f32)>, // (ref, current centre)
    centre: f32,
    width: f32,
    vmin: f32,
    vmax: f32,
    pinned: bool,
}

/// Two units must be along-separated iff their vertical bands OVERLAP. This is
/// PAIRWISE (not the transitive connected-component "band" it replaced): a tall
/// door no longer bridges a low row and a high window into one packing set, so a
/// wide window may sit directly above two narrow ones it never actually touches.
fn units_height_overlap(a: &PackUnit, b: &PackUnit) -> bool {
    a.vmin < b.vmax - 1e-4 && b.vmin < a.vmax - 1e-4
}

fn build_pack_units(raw: &[WallOpening], pinned: &[OpeningRef]) -> Vec<PackUnit> {
    let mut by_assembly: std::collections::HashMap<ItemId, Vec<WallOpening>> =
        std::collections::HashMap::new();
    let mut units: Vec<PackUnit> = Vec::new();
    for o in raw {
        match o.assembly_id {
            Some(aid) => by_assembly.entry(aid).or_default().push(*o),
            None => units.push(PackUnit {
                members: vec![(o.opening, o.along_m)],
                centre: o.along_m,
                width: o.width_m,
                vmin: o.sill_m,
                vmax: o.sill_m + o.height_m,
                pinned: pinned.contains(&o.opening),
            }),
        }
    }
    for (_, members) in by_assembly {
        let left = members
            .iter()
            .map(|o| o.along_m - o.width_m / 2.0)
            .fold(f32::INFINITY, f32::min);
        let right = members
            .iter()
            .map(|o| o.along_m + o.width_m / 2.0)
            .fold(f32::NEG_INFINITY, f32::max);
        let vmin = members
            .iter()
            .map(|o| o.sill_m)
            .fold(f32::INFINITY, f32::min);
        let vmax = members
            .iter()
            .map(|o| o.sill_m + o.height_m)
            .fold(f32::NEG_INFINITY, f32::max);
        units.push(PackUnit {
            members: members.iter().map(|o| (o.opening, o.along_m)).collect(),
            centre: (left + right) / 2.0,
            width: right - left,
            vmin,
            vmax,
            pinned: members.iter().any(|o| pinned.contains(&o.opening)),
        });
    }
    units
}

/// The minimal wall length these units need at gap `g`: the widest "scanline".
/// At any height the openings crossing it all pairwise overlap (a clique), so
/// they must sit side-by-side — sum of widths + gaps + the two end margins. The
/// max over every height interval is EXACT feasibility (height-overlap is an
/// interval graph, whose cliques are exactly the scanline sets).
fn max_scanline_needed(units: &[PackUnit], g: f32) -> f32 {
    let mut ys: Vec<f32> = Vec::with_capacity(units.len() * 2);
    for u in units {
        ys.push(u.vmin);
        ys.push(u.vmax);
    }
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mut needed = 2.0 * END_MARGIN_M;
    for w in ys.windows(2) {
        if w[1] - w[0] < 1e-6 {
            continue;
        }
        let y = (w[0] + w[1]) / 2.0;
        let active = units.iter().filter(|u| u.vmin < y && y < u.vmax);
        let (k, sum_w) = active.fold((0u32, 0.0f32), |(k, s), u| (k + 1, s + u.width));
        if k == 0 {
            continue;
        }
        needed = needed.max(sum_w + g * (k as f32 - 1.0) + 2.0 * END_MARGIN_M);
    }
    needed
}

/// Place units along the wall NEAR their desired centres, separating every
/// height-overlapping pair by gap `g`, holding pinned units fixed. Iterative
/// pairwise projection: only pairs that actually overlap are pushed apart, by the
/// minimum, so non-conflicting openings keep their place (minimal movement).
/// Left/right order is taken from the DESIRED centres (stable; a dragged opening's
/// new centre defines its order, so it can move past a neighbour). Returns `None`
/// if it can't fully separate within the wall bounds at this gap.
fn project_along(units: &[PackUnit], len: f32, g: f32) -> Option<Vec<f32>> {
    let n = units.len();
    let m = END_MARGIN_M;
    let lo: Vec<f32> = units.iter().map(|u| m + u.width / 2.0).collect();
    let hi: Vec<f32> = units.iter().map(|u| len - m - u.width / 2.0).collect();
    if (0..n).any(|i| lo[i] > hi[i] + 1e-4) {
        return None; // a single unit is wider than the wall
    }
    let mut c: Vec<f32> = (0..n)
        .map(|i| units[i].centre.clamp(lo[i], hi[i]))
        .collect();
    let conflicts: Vec<(usize, usize)> = (0..n)
        .flat_map(|i| ((i + 1)..n).map(move |j| (i, j)))
        .filter(|&(i, j)| units_height_overlap(&units[i], &units[j]))
        .collect();
    let max_iters = 64 + 16 * n;
    for _ in 0..max_iters {
        let mut moved = false;
        for &(i, j) in &conflicts {
            let req = (units[i].width + units[j].width) / 2.0 + g;
            // Order by DESIRED centre so units don't oscillate past each other.
            let (a, b) = if units[i].centre <= units[j].centre {
                (i, j)
            } else {
                (j, i)
            };
            let dist = c[b] - c[a];
            if dist < req - 1e-5 {
                let deficit = req - dist;
                let (da, db) = match (units[a].pinned, units[b].pinned) {
                    (true, true) => (0.0, 0.0), // can't move either — caught by the final check
                    (true, false) => (0.0, deficit),
                    (false, true) => (deficit, 0.0),
                    (false, false) => (deficit / 2.0, deficit / 2.0),
                };
                c[a] = (c[a] - da).max(lo[a]);
                c[b] = (c[b] + db).min(hi[b]);
                moved = true;
            }
        }
        if !moved {
            break;
        }
    }
    // Confirm no residual overlap (clamping to bounds can leave one unresolved).
    for &(i, j) in &conflicts {
        let req = (units[i].width + units[j].width) / 2.0 + g;
        if (c[i] - c[j]).abs() < req - 1e-3 {
            return None;
        }
    }
    Some(c)
}

/// Space the wall's openings so none overlap as 2D rectangles (along × height).
/// See [`redistribute_wall_openings_pinned`]; this is the no-pin entry point.
/// Public: the document relink's buried-piece NUDGE redistributes each
/// affected host after moving an opening clear of an inter-storey band.
pub fn redistribute_wall_openings(
    storey: &mut Storey,
    wall_id: ItemId,
) -> Result<(), OpeningError> {
    redistribute_wall_openings_pinned(storey, wall_id, &[])
}

/// Space the wall's openings (true 2D no-overlap), keeping `pinned` openings at
/// their along while the rest flow around them with minimal movement. Openings
/// whose vertical bands are disjoint may share an along (stacking); only a real
/// 2D overlap is resolved. The gap COMPRESSES from `MIN_GAP_M` toward
/// `GAP_FLOOR_M` to fit more in a tight wall before failing. `WallTooShort` (with
/// the storey left unchanged) only when even touching openings can't fit.
pub(crate) fn redistribute_wall_openings_pinned(
    storey: &mut Storey,
    wall_id: ItemId,
    pinned: &[OpeningRef],
) -> Result<(), OpeningError> {
    let len = wall_length(storey, wall_id).ok_or(OpeningError::NoHostRegion)?;
    let raw = wall_openings(storey, wall_id);
    if raw.is_empty() {
        return Ok(());
    }
    let units = build_pack_units(&raw, pinned);
    // Try the default gap, then COMPRESS toward the floor to cram more in before
    // giving up. Feasibility (widest scanline) is checked per gap; if it fits, the
    // projection places everything near its desired along.
    let gaps = [MIN_GAP_M, 0.075, 0.05, 0.025, GAP_FLOOR_M];
    let centres = gaps
        .iter()
        .find_map(|&g| {
            (max_scanline_needed(&units, g) <= len + 1e-4)
                .then(|| project_along(&units, len, g))
                .flatten()
        })
        .ok_or(OpeningError::WallTooShort {
            needed_m: max_scanline_needed(&units, GAP_FLOOR_M),
            available_m: len,
        })?;
    // Apply: translate each unit's members by (new centre − unit centre).
    let mut new_centre: std::collections::HashMap<OpeningRef, f32> =
        std::collections::HashMap::new();
    for (i, u) in units.iter().enumerate() {
        let delta = centres[i] - u.centre;
        for (r, c) in &u.members {
            new_centre.insert(*r, *c + delta);
        }
    }
    let apply = |p: &mut Option<OpeningPlacement>, r: OpeningRef| {
        if let (Some(OpeningPlacement::Wall { along_m, .. }), Some(c)) =
            (p.as_mut(), new_centre.get(&r))
        {
            *along_m = *c;
        }
    };
    for w in &mut storey.windows {
        apply(&mut w.placement, OpeningRef::Window(w.id));
    }
    for d in &mut storey.doors {
        apply(&mut d.placement, OpeningRef::Door(d.id));
    }
    Ok(())
}

// ── small helpers ───────────────────────────────────────────────────────────

fn link_window(storey: &mut Storey, wall_id: ItemId, win_id: ItemId) {
    if let Some(r) = storey.walls.regions.iter_mut().find(|r| r.id == wall_id) {
        r.opes.windows.push(win_id);
    }
}
fn link_door(storey: &mut Storey, wall_id: ItemId, door_id: ItemId) {
    if let Some(r) = storey.walls.regions.iter_mut().find(|r| r.id == wall_id) {
        r.opes.doors.push(door_id);
    }
}

/// `(wall_id, along_m, sill_m, assembly_id)` of an opening if it has a Wall
/// placement, else `None`.
fn wall_placement_of(
    storey: &Storey,
    opening: OpeningRef,
) -> Option<(ItemId, f32, f32, Option<ItemId>)> {
    let (placement, assembly) = match opening {
        OpeningRef::Window(id) => {
            let w = storey.windows.iter().find(|w| w.id == id)?;
            (&w.placement, w.assembly_id)
        }
        OpeningRef::Door(id) => {
            let d = storey.doors.iter().find(|d| d.id == id)?;
            (&d.placement, d.assembly_id)
        }
    };
    match placement {
        Some(OpeningPlacement::Wall {
            wall_id,
            along_m,
            sill_m,
        }) => Some((*wall_id, *along_m, *sill_m, assembly)),
        _ => None,
    }
}

/// Redistribute; on `WallTooShort`, remove the just-added `refs` (rollback) and
/// propagate the error so the caller leaves no partial state.
/// Remove just-added openings + scrub their `opes` refs — the rollback half of a
/// failed add.
fn rollback_added(storey: &mut Storey, wall_id: ItemId, added: &[OpeningRef]) {
    for r in added {
        match r {
            OpeningRef::Window(id) => storey.windows.retain(|w| w.id != *id),
            OpeningRef::Door(id) => storey.doors.retain(|d| d.id != *id),
        }
    }
    if let Some(reg) = storey.walls.regions.iter_mut().find(|r| r.id == wall_id) {
        reg.opes
            .windows
            .retain(|id| !added.contains(&OpeningRef::Window(*id)));
        reg.opes
            .doors
            .retain(|id| !added.contains(&OpeningRef::Door(*id)));
    }
}

fn redistribute_or_rollback(
    storey: &mut Storey,
    wall_id: ItemId,
    added: &[OpeningRef],
) -> Result<(), OpeningError> {
    match redistribute_wall_openings(storey, wall_id) {
        Ok(()) => Ok(()),
        Err(e) => {
            rollback_added(storey, wall_id, added);
            Err(e)
        }
    }
}

/// Set a wall opening's sill directly (no redistribute).
fn set_wall_opening_sill(storey: &mut Storey, opening: OpeningRef, sill: f32) {
    let set = |p: &mut Option<OpeningPlacement>| {
        if let Some(OpeningPlacement::Wall { sill_m, .. }) = p {
            *sill_m = sill;
        }
    };
    match opening {
        OpeningRef::Window(id) => {
            if let Some(w) = storey.windows.iter_mut().find(|w| w.id == id) {
                set(&mut w.placement);
            }
        }
        OpeningRef::Door(id) => {
            if let Some(d) = storey.doors.iter_mut().find(|d| d.id == id) {
                set(&mut d.placement);
            }
        }
    }
}

/// Place a freshly-added wall WINDOW, using VERTICAL space when the row is full.
/// Tries the requested sill first; if the openings sharing that height can't all
/// fit along the wall, it STACKS the window — searching sills at the tops of the
/// existing openings (lowest first) and the floor — so it lands above/below them
/// rather than being refused. Existing openings auto-rearrange (redistribute) at
/// each try; the window's height bounds the search under the wall top. A failed
/// try leaves positions untouched (redistribute applies nothing on error), so the
/// only state it carries between tries is the candidate sill. Returns the last
/// `WallTooShort` if no 2D slot fits at all.
fn place_window_with_vertical_fallback(
    storey: &mut Storey,
    wall_id: ItemId,
    new_ref: OpeningRef,
    height: f32,
) -> Result<(), OpeningError> {
    let (along, requested) = wall_placement_of(storey, new_ref)
        .map(|(_, a, s, _)| (a, s))
        .unwrap_or((0.0, 0.0));
    let max_sill = storey
        .walls
        .regions
        .iter()
        .find(|r| r.id == wall_id)
        .map(|r| r.max_sill_for(along as f64, height as f64) as f32)
        .unwrap_or(0.0);
    // Candidate sills, in preference order: the requested one, then stacked at the
    // top of each existing opening (lowest first), then the floor.
    let mut tops: Vec<f32> = wall_openings(storey, wall_id)
        .into_iter()
        .filter(|o| o.opening != new_ref)
        .map(|o| o.sill_m + o.height_m)
        .collect();
    tops.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let candidates = std::iter::once(requested)
        .chain(tops)
        .chain(std::iter::once(0.0));
    let mut last_err = OpeningError::WallTooShort {
        needed_m: 0.0,
        available_m: 0.0,
    };
    for sill in candidates {
        if sill < -1e-4 || sill > max_sill + 1e-4 {
            continue;
        }
        set_wall_opening_sill(storey, new_ref, sill.clamp(0.0, max_sill));
        match redistribute_wall_openings(storey, wall_id) {
            Ok(()) => return Ok(()),
            Err(e) => last_err = e,
        }
    }
    Err(last_err)
}

/// Recompute the `vertices` cache of every PLACED opening (`placement: Some`)
/// from its durable placement + the resolved host region geometry. Legacy
/// openings (`placement: None`) keep their stored vertices. Call at the END of
/// `resolve_storey`, after walls/ceilings are regenerated, so the host geometry is
/// final. An opening whose host id no longer resolves keeps its last vertices
/// (then [`prune_orphaned_openings`] drops it).
pub fn derive_opening_vertices(storey: &mut Storey) {
    let win: Vec<Option<Vec<(f32, f32, f32)>>> = storey
        .windows
        .iter()
        .map(|w| {
            w.placement.as_ref().and_then(|p| {
                opening_vertices(storey, p, w.width_m, w.height_m, w.multi_storey, w.shape)
            })
        })
        .collect();
    for (w, v) in storey.windows.iter_mut().zip(win) {
        if let Some(v) = v {
            w.vertices = v;
        }
    }
    let dr: Vec<Option<Vec<(f32, f32, f32)>>> = storey
        .doors
        .iter()
        .map(|d| {
            d.placement.as_ref().and_then(|p| {
                opening_vertices(
                    storey,
                    p,
                    d.width_m,
                    d.height_m,
                    false,
                    WindowShape::Rectangular,
                )
            })
        })
        .collect();
    for (d, v) in storey.doors.iter_mut().zip(dr) {
        if let Some(v) = v {
            d.vertices = v;
        }
    }
}

/// Drop placed openings whose host wall/roof id no longer resolves (an outline
/// edit replaced the region with a fresh id), and scrub any now-dangling ids
/// from every region's `opes`. Legacy openings (`placement: None`) are kept.
pub fn prune_orphaned_openings(storey: &mut Storey) {
    let wall_ids: std::collections::HashSet<_> =
        storey.walls.regions.iter().map(|r| r.id).collect();
    let host_present = |p: &Option<OpeningPlacement>| match p {
        None => true, // legacy — not placement-bound
        // Walls amend, so their id is stable.
        Some(OpeningPlacement::Wall { wall_id, .. }) => wall_ids.contains(wall_id),
        // Ceiling ids are NOT stable across resolve — keep a roof window while
        // its plan point still falls inside some roof region.
        Some(OpeningPlacement::Roof { plan, .. }) => storey
            .ceilings
            .region_strictly_containing(domain::points::Point2f64::new(
                plan.0 as f64,
                plan.1 as f64,
            ))
            .is_some(),
    };
    storey.windows.retain(|w| {
        let keep = host_present(&w.placement);
        if !keep {
            log::warn!("dropping orphaned window {:?} (host region gone)", w.id);
        }
        keep
    });
    storey.doors.retain(|d| {
        let keep = host_present(&d.placement);
        if !keep {
            log::warn!("dropping orphaned door {:?} (host region gone)", d.id);
        }
        keep
    });
    // (Opes are rebuilt from placements by `rebuild_opes` at the resolve tail,
    // so no scrub is needed here.)
}

/// Rebuild every wall/roof region's `Opes` from the openings' PLACEMENTS, so
/// each surface knows exactly which openings sit on it. The placement is the
/// durable truth: wall ids amend across resolve, but ceiling ids are
/// regenerated, so the link must be RE-DERIVED every time rather than carried.
/// Call at the resolve tail, after `derive_opening_vertices` +
/// `prune_orphaned_openings`.
pub fn rebuild_opes(storey: &mut Storey) {
    for r in &mut storey.walls.regions {
        r.opes.windows.clear();
        r.opes.doors.clear();
    }
    for r in &mut storey.ceilings.regions {
        r.opes.windows.clear();
        r.opes.doors.clear();
    }
    let wall_idx: std::collections::HashMap<ItemId, usize> = storey
        .walls
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| (r.id, i))
        .collect();
    enum Tgt {
        Wall(usize),
        Roof(usize),
    }
    let target = |p: &Option<OpeningPlacement>| -> Option<Tgt> {
        match p {
            Some(OpeningPlacement::Wall { wall_id, .. }) => {
                wall_idx.get(wall_id).copied().map(Tgt::Wall)
            }
            Some(OpeningPlacement::Roof { plan, .. }) => {
                let pt = domain::points::Point2f64::new(plan.0 as f64, plan.1 as f64);
                let id = storey.ceilings.region_strictly_containing(pt)?.id;
                storey
                    .ceilings
                    .regions
                    .iter()
                    .position(|r| r.id == id)
                    .map(Tgt::Roof)
            }
            None => None, // legacy (no placement) — can't reattach
        }
    };
    let win: Vec<(ItemId, Option<Tgt>)> = storey
        .windows
        .iter()
        .map(|w| (w.id, target(&w.placement)))
        .collect();
    let drs: Vec<(ItemId, Option<Tgt>)> = storey
        .doors
        .iter()
        .map(|d| (d.id, target(&d.placement)))
        .collect();
    for (id, t) in win {
        match t {
            Some(Tgt::Wall(i)) => storey.walls.regions[i].opes.windows.push(id),
            Some(Tgt::Roof(i)) => storey.ceilings.regions[i].opes.windows.push(id),
            None => {}
        }
    }
    for (id, t) in drs {
        match t {
            Some(Tgt::Wall(i)) => storey.walls.regions[i].opes.doors.push(id),
            Some(Tgt::Roof(i)) => storey.ceilings.regions[i].opes.doors.push(id),
            None => {}
        }
    }
}

/// The 3D `vertices` for one placed opening, or `None` if its host region is
/// missing/degenerate. A WALL opening is a rectangle in the wall plane (the top
/// clamped under the local roof profile so it can't poke through the ceiling); a
/// ROOF opening is a plan rectangle lifted onto the (possibly sloped) region
/// plane.
fn opening_vertices(
    storey: &Storey,
    placement: &OpeningPlacement,
    width_m: f32,
    height_m: f32,
    multi_storey: bool,
    shape: WindowShape,
) -> Option<Vec<(f32, f32, f32)>> {
    match placement {
        OpeningPlacement::Wall {
            wall_id,
            along_m,
            sill_m,
        } => {
            let wall = storey.walls.regions.iter().find(|r| r.id == *wall_id)?;
            let (origin, (ux, uy), len, base_z) = wall.base_frame()?;
            let half = (width_m as f64) * 0.5;
            let centre = (*along_m as f64).clamp(0.0, len);
            let left = (centre - half).clamp(0.0, len);
            let right = (centre + half).clamp(0.0, len);
            let bottom = base_z + *sill_m as f64;
            // A multi-storey window may rise past THIS storey's roofline (it spans
            // up the same-type wall run) — so don't cap its top at the wall top.
            let top = if multi_storey {
                bottom + height_m as f64
            } else {
                let top_cap = wall.top_z_at_along(left).min(wall.top_z_at_along(right));
                (bottom + height_m as f64).min(top_cap).max(bottom)
            };
            let p = |d: f64| (origin.x + ux * d, origin.y + uy * d);
            let (lx, ly) = p(left);
            let (rx, ry) = p(right);
            // Non-rectangular shapes: map the shared local outline
            // (`window_outline`, in 0..width × 0..height) into the (possibly
            // clamped) wall-plane box [left,right] × [bottom,top], so the
            // silhouette matches what the elevation draws from the same source.
            if !matches!(shape, WindowShape::Rectangular) && width_m > 0.0 && height_m > 0.0 {
                let (span, rise) = (right - left, top - bottom);
                let v = super::fenestration::window_outline(shape, width_m, height_m)
                    .iter()
                    .map(|&(u, w)| {
                        let (ax, ay) = p(left + (u as f64 / width_m as f64) * span);
                        (
                            ax as f32,
                            ay as f32,
                            (bottom + (w as f64 / height_m as f64) * rise) as f32,
                        )
                    })
                    .collect();
                return Some(v);
            }
            Some(vec![
                (lx as f32, ly as f32, bottom as f32),
                (rx as f32, ry as f32, bottom as f32),
                (rx as f32, ry as f32, top as f32),
                (lx as f32, ly as f32, top as f32),
            ])
        }
        OpeningPlacement::Roof { plan, .. } => {
            // Ceiling region ids are regenerated each resolve (unlike walls,
            // which amend), so the durable anchor is the PLAN POINT: re-find the
            // region containing it rather than trusting the stored id.
            let (cx, cy) = (plan.0 as f64, plan.1 as f64);
            let region = storey
                .ceilings
                .region_strictly_containing(domain::points::Point2f64::new(cx, cy))?;
            // The stored size is a CEILING-PLANE measurement (height up-slope,
            // width across): lay the footprint out in the region plane and lift
            // each corner's z, so a slope foreshortens the plan extent instead
            // of overshooting the surface.
            let corners =
                roof_window_plan_corners(region, (cx, cy), width_m as f64, height_m as f64);
            let flat = region.flat_z();
            let mut out = Vec::with_capacity(4);
            for (x, y) in corners {
                let z = region.polygon.plane_z(x, y).or(flat)?;
                out.push((x as f32, y as f32, z as f32));
            }
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ber::common::ItemId;
    use crate::ber::fenestration::{Door, OpeningPlacement, Window, WindowShape};
    use crate::ber::structural::{
        CeilingExposure, CeilingRegion, CeilingRegions, Opes, Polygon3D, Storey, WallExposure,
        WallRegion, WallRegions,
    };
    use domain::points::Point3f64;

    fn p3(x: f64, y: f64, z: f64) -> Point3f64 {
        Point3f64::new(x, y, z)
    }

    /// A wall with the given base a→b and top vertices (b-top then a-top).
    fn wall(id: ItemId, ext: Vec<Point3f64>) -> WallRegion {
        WallRegion {
            id,
            polygon: Polygon3D::new(ext),
            exposure: WallExposure::Unset,
            opes: Opes::default(),
            construction_id: ItemId::default(),
        }
    }

    fn window(id: ItemId, w: f32, h: f32, placement: Option<OpeningPlacement>) -> Window {
        Window {
            id,
            vertices: vec![],
            width_m: w,
            height_m: h,
            window_type_id: ItemId::default(),
            placement,
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            shape: WindowShape::Rectangular,
            overshading: Overshading::Average,
        }
    }

    /// A ceiling region whose plane rises with +y: `z = base + grad·y` (a
    /// mono-pitch face over the plan rectangle 0..4 × 0..2). `grad = √3` gives a
    /// 60° pitch (`cos = 0.5`) — clean foreshortening for the assertions below.
    fn sloped_ceiling(id: ItemId, base: f64, grad: f64) -> CeilingRegion {
        CeilingRegion {
            id,
            polygon: Polygon3D::new(vec![
                p3(0.0, 0.0, base),
                p3(4.0, 0.0, base),
                p3(4.0, 2.0, base + grad * 2.0),
                p3(0.0, 2.0, base + grad * 2.0),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        }
    }

    #[test]
    fn derive_wall_window_on_a_flat_wall() {
        let wid = ItemId::default();
        // 4 m wall, flat top at 2.4.
        let mut storey = Storey {
            walls: WallRegions {
                regions: vec![wall(
                    wid,
                    vec![
                        p3(0.0, 0.0, 0.0),
                        p3(4.0, 0.0, 0.0),
                        p3(4.0, 0.0, 2.4),
                        p3(0.0, 0.0, 2.4),
                    ],
                )],
            },
            windows: vec![window(
                ItemId::default(),
                1.0,
                1.2,
                Some(OpeningPlacement::Wall {
                    wall_id: wid,
                    along_m: 2.0,
                    sill_m: 1.0,
                }),
            )],
            ..Storey::default()
        };
        derive_opening_vertices(&mut storey);
        let v = &storey.windows[0].vertices;
        assert_eq!(v.len(), 4);
        let near = |a: (f32, f32, f32), b: (f32, f32, f32)| {
            (a.0 - b.0).abs() < 1e-4 && (a.1 - b.1).abs() < 1e-4 && (a.2 - b.2).abs() < 1e-4
        };
        assert!(near(v[0], (1.5, 0.0, 1.0)), "bl {:?}", v[0]);
        assert!(near(v[1], (2.5, 0.0, 1.0)), "br {:?}", v[1]);
        assert!(near(v[2], (2.5, 0.0, 2.2)), "tr {:?}", v[2]);
        assert!(near(v[3], (1.5, 0.0, 2.2)), "tl {:?}", v[3]);
    }

    #[test]
    fn derive_clamps_top_under_a_sloped_roof() {
        let wid = ItemId::default();
        // Top rises a=2.0 (above a) → b=3.0 (above b) over the 4 m base.
        let mut storey = Storey {
            walls: WallRegions {
                regions: vec![wall(
                    wid,
                    vec![
                        p3(0.0, 0.0, 0.0),
                        p3(4.0, 0.0, 0.0),
                        p3(4.0, 0.0, 3.0),
                        p3(0.0, 0.0, 2.0),
                    ],
                )],
            },
            windows: vec![window(
                ItemId::default(),
                1.0,
                1.0, // would want top 3.0, but the roof caps it lower
                Some(OpeningPlacement::Wall {
                    wall_id: wid,
                    along_m: 2.0,
                    sill_m: 2.0,
                }),
            )],
            ..Storey::default()
        };
        derive_opening_vertices(&mut storey);
        // cap = min(top_z_at_along(1.5)=2.375, top_z_at_along(2.5)=2.625) = 2.375.
        let top = storey.windows[0].vertices[2].2;
        assert!((top - 2.375).abs() < 1e-3, "clamped top = {top}");
    }

    #[test]
    fn derive_multi_storey_window_is_not_clamped_under_the_wall_top() {
        let wid = ItemId::default();
        // Same 2.4 m-tall flat wall, but a multi-storey window whose sill+height
        // (2.0 + 1.0 = 3.0) pokes a clear 0.6 m above the wall top: the clamp is
        // LIFTED, so its top must read 3.0, not the 2.4 a plain window would get.
        let mut w = window(
            ItemId::default(),
            1.0,
            1.0,
            Some(OpeningPlacement::Wall {
                wall_id: wid,
                along_m: 2.0,
                sill_m: 2.0,
            }),
        );
        w.multi_storey = true;
        let mut storey = Storey {
            walls: WallRegions {
                regions: vec![wall(
                    wid,
                    vec![
                        p3(0.0, 0.0, 0.0),
                        p3(4.0, 0.0, 0.0),
                        p3(4.0, 0.0, 2.4),
                        p3(0.0, 0.0, 2.4),
                    ],
                )],
            },
            windows: vec![w],
            ..Storey::default()
        };
        derive_opening_vertices(&mut storey);
        let top = storey.windows[0].vertices[2].2;
        assert!((top - 3.0).abs() < 1e-3, "unclamped top = {top}");
    }

    #[test]
    fn derive_roof_window_lifts_onto_a_flat_ceiling() {
        let rid = ItemId::default();
        let region = CeilingRegion {
            id: rid,
            polygon: Polygon3D::new(vec![
                p3(0.0, 0.0, 2.7),
                p3(4.0, 0.0, 2.7),
                p3(4.0, 4.0, 2.7),
                p3(0.0, 4.0, 2.7),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        };
        let mut storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![region],
            },
            windows: vec![window(
                ItemId::default(),
                1.0,
                1.0,
                Some(OpeningPlacement::Roof {
                    ceiling_region_id: rid,
                    plan: (2.0, 2.0),
                }),
            )],
            ..Storey::default()
        };
        derive_opening_vertices(&mut storey);
        let v = &storey.windows[0].vertices;
        assert_eq!(v.len(), 4);
        assert!(
            v.iter().all(|c| (c.2 - 2.7).abs() < 1e-4),
            "flat z 2.7: {v:?}"
        );
        assert!(v
            .iter()
            .any(|c| (c.0 - 1.5).abs() < 1e-4 && (c.1 - 1.5).abs() < 1e-4));
    }

    #[test]
    fn derive_roof_window_on_a_flat_ceiling_keeps_the_axis_aligned_box() {
        let rid = ItemId::default();
        let region = CeilingRegion {
            id: rid,
            polygon: Polygon3D::new(vec![
                p3(0.0, 0.0, 2.7),
                p3(4.0, 0.0, 2.7),
                p3(4.0, 4.0, 2.7),
                p3(0.0, 4.0, 2.7),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        };
        let mut storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![region],
            },
            windows: vec![window(
                ItemId::default(),
                1.0,
                2.0,
                Some(OpeningPlacement::Roof {
                    ceiling_region_id: rid,
                    plan: (2.0, 2.0),
                }),
            )],
            ..Storey::default()
        };
        derive_opening_vertices(&mut storey);
        let v = &storey.windows[0].vertices;
        // Flat ceiling: NO foreshortening — width → x (±0.5), height → y (±1.0).
        let (min_x, max_x) = v
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), c| (a.min(c.0), b.max(c.0)));
        let (min_y, max_y) = v
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), c| (a.min(c.1), b.max(c.1)));
        assert!(
            ((max_x - min_x) - 1.0).abs() < 1e-4 && ((max_y - min_y) - 2.0).abs() < 1e-4,
            "flat axis box: x-extent {}, y-extent {}",
            max_x - min_x,
            max_y - min_y
        );
    }

    #[test]
    fn derive_roof_window_foreshortens_up_the_slope() {
        let rid = ItemId::default();
        let grad = 3.0_f64.sqrt(); // 60° pitch → cos = 0.5
        let mut storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![sloped_ceiling(rid, 2.4, grad)],
            },
            windows: vec![window(
                ItemId::default(),
                1.0, // width ACROSS the slope
                2.0, // height UP the slope → plan extent 2.0·cos = 1.0
                Some(OpeningPlacement::Roof {
                    ceiling_region_id: rid,
                    plan: (2.0, 1.0),
                }),
            )],
            ..Storey::default()
        };
        derive_opening_vertices(&mut storey);
        let v = &storey.windows[0].vertices;
        assert_eq!(v.len(), 4);
        let (min_x, max_x) = v
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), c| (a.min(c.0), b.max(c.0)));
        let (min_y, max_y) = v
            .iter()
            .fold((f32::MAX, f32::MIN), |(a, b), c| (a.min(c.1), b.max(c.1)));
        // Across the slope (+x): unchanged width. Up the slope (+y, the gradient
        // direction): height foreshortened by cos = 0.5 → 2.0·0.5 = 1.0.
        assert!(
            ((max_x - min_x) - 1.0).abs() < 1e-3,
            "across-slope extent {} (want width 1.0)",
            max_x - min_x
        );
        assert!(
            ((max_y - min_y) - 1.0).abs() < 1e-3,
            "up-slope plan extent {} (want height·cos = 1.0)",
            max_y - min_y
        );
        // Every corner lies ON the region plane z = 2.4 + grad·y.
        for c in v {
            let want = 2.4 + grad * c.1 as f64;
            assert!(
                (c.2 as f64 - want).abs() < 1e-3,
                "corner {c:?} off the region plane (want z {want})"
            );
        }
    }

    #[test]
    fn roof_window_at_uses_the_slope_foreshortened_footprint() {
        let rid = ItemId::default();
        let grad = 3.0_f64.sqrt();
        let wid = ItemId::default();
        let storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![sloped_ceiling(rid, 2.4, grad)],
            },
            windows: vec![window(
                wid,
                1.0,
                2.0,
                Some(OpeningPlacement::Roof {
                    ceiling_region_id: rid,
                    plan: (2.0, 1.0),
                }),
            )],
            ..Storey::default()
        };
        // Inside the foreshortened footprint (up-slope half-extent 0.5 → y 0.5..1.5).
        assert_eq!(roof_window_at(&storey, 2.0, 1.4), Some(wid));
        // Past it up-slope: the OLD plan box (height 2.0 → y 0..2) would still
        // hit here, but the ceiling-plane footprint stops at y 1.5 — so it MISSES.
        assert_eq!(roof_window_at(&storey, 2.0, 1.9), None);
    }

    #[test]
    fn oversized_roof_window_is_flagged() {
        let rid = ItemId::default();
        let grad = 3.0_f64.sqrt();
        let wid = ItemId::default();
        let storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![sloped_ceiling(rid, 2.4, grad)],
            },
            windows: vec![window(
                wid,
                1.0,
                6.0, // up-slope plan extent 6.0·0.5 = 3.0 → spills past the 2 m region
                Some(OpeningPlacement::Roof {
                    ceiling_region_id: rid,
                    plan: (2.0, 1.0),
                }),
            )],
            ..Storey::default()
        };
        assert_eq!(oversized_roof_windows(&storey), vec![wid]);
    }

    #[test]
    fn roof_window_flush_to_the_region_edge_is_not_flagged() {
        let rid = ItemId::default();
        let grad = 3.0_f64.sqrt();
        let storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![sloped_ceiling(rid, 2.4, grad)],
            },
            windows: vec![window(
                ItemId::default(),
                4.0, // across → x 0..4, flush to both eaves
                4.0, // up-slope plan extent 4.0·0.5 = 2.0 → y 0..2, flush to the edges
                Some(OpeningPlacement::Roof {
                    ceiling_region_id: rid,
                    plan: (2.0, 1.0),
                }),
            )],
            ..Storey::default()
        };
        // Corners land exactly on the region boundary; the centre-ward epsilon
        // nudge keeps a flush-fit rooflight from tripping the strict predicate.
        assert!(
            oversized_roof_windows(&storey).is_empty(),
            "a rooflight sized flush to the region edge must NOT be flagged"
        );
    }

    #[test]
    fn in_bounds_roof_window_is_not_flagged() {
        let rid = ItemId::default();
        let grad = 3.0_f64.sqrt();
        let storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![sloped_ceiling(rid, 2.4, grad)],
            },
            windows: vec![window(
                ItemId::default(),
                1.0,
                2.0, // footprint y 0.5..1.5, x 1.5..2.5 — inside the region
                Some(OpeningPlacement::Roof {
                    ceiling_region_id: rid,
                    plan: (2.0, 1.0),
                }),
            )],
            ..Storey::default()
        };
        assert!(oversized_roof_windows(&storey).is_empty());
    }

    #[test]
    fn prune_drops_orphans_and_keeps_legacy_and_placed() {
        let wid = ItemId::default();
        let placed = window(
            ItemId::default(),
            1.0,
            1.0,
            Some(OpeningPlacement::Wall {
                wall_id: wid,
                along_m: 1.0,
                sill_m: 1.0,
            }),
        );
        let orphan = window(
            ItemId::default(),
            1.0,
            1.0,
            Some(OpeningPlacement::Wall {
                wall_id: ItemId::default(), // no such wall
                along_m: 1.0,
                sill_m: 1.0,
            }),
        );
        let legacy = window(ItemId::default(), 1.0, 1.0, None);
        let (placed_id, legacy_id) = (placed.id, legacy.id);
        let mut storey = Storey {
            walls: WallRegions {
                regions: vec![wall(
                    wid,
                    vec![
                        p3(0.0, 0.0, 0.0),
                        p3(4.0, 0.0, 0.0),
                        p3(4.0, 0.0, 2.4),
                        p3(0.0, 0.0, 2.4),
                    ],
                )],
            },
            windows: vec![placed, orphan, legacy],
            ..Storey::default()
        };
        prune_orphaned_openings(&mut storey);
        let ids: Vec<_> = storey.windows.iter().map(|w| w.id).collect();
        assert_eq!(ids.len(), 2, "the orphan is dropped");
        assert!(ids.contains(&placed_id) && ids.contains(&legacy_id));
    }

    #[test]
    fn legacy_window_vertices_are_left_untouched() {
        let mut storey = Storey {
            doors: vec![Door {
                id: ItemId::default(),
                type_name: "front".into(),
                vertices: vec![(1.0, 2.0, 3.0)],
                photos: vec![],
                width_m: 0.0,
                height_m: 0.0,
                kind: None,
                placement: None, // legacy
                wall_anchor: None,
                assembly_id: None,
            }],
            ..Storey::default()
        };
        derive_opening_vertices(&mut storey);
        assert_eq!(storey.doors[0].vertices, vec![(1.0, 2.0, 3.0)]);
    }

    /// A storey with one straight wall of length `len` along +x at y=0, flat top.
    fn storey_with_wall(len: f64) -> (Storey, ItemId) {
        let wid = ItemId::default();
        let storey = Storey {
            walls: WallRegions {
                regions: vec![wall(
                    wid,
                    vec![
                        p3(0.0, 0.0, 0.0),
                        p3(len, 0.0, 0.0),
                        p3(len, 0.0, 2.4),
                        p3(0.0, 0.0, 2.4),
                    ],
                )],
            },
            ..Storey::default()
        };
        (storey, wid)
    }

    /// The [left, right] spans of every wall opening on the wall, sorted.
    fn spans(storey: &Storey) -> Vec<(f32, f32)> {
        let mut v: Vec<(f32, f32)> = storey
            .windows
            .iter()
            .filter_map(|w| match &w.placement {
                Some(OpeningPlacement::Wall { along_m, .. }) => {
                    Some((along_m - w.width_m / 2.0, along_m + w.width_m / 2.0))
                }
                _ => None,
            })
            .chain(storey.doors.iter().filter_map(|d| match &d.placement {
                Some(OpeningPlacement::Wall { along_m, .. }) => {
                    Some((along_m - d.width_m / 2.0, along_m + d.width_m / 2.0))
                }
                _ => None,
            }))
            .collect();
        v.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        v
    }

    #[test]
    fn add_two_windows_redistributes_with_no_overlap() {
        let (mut s, wid) = storey_with_wall(4.0);
        // Both dropped near the centre — must separate, in order, within bounds.
        add_wall_window(&mut s, wid, 1.8, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        add_wall_window(&mut s, wid, 2.2, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        let sp = spans(&s);
        assert_eq!(sp.len(), 2);
        assert!(sp[0].0 >= END_MARGIN_M - 1e-4, "left margin: {sp:?}");
        assert!(sp[1].1 <= 4.0 - END_MARGIN_M + 1e-4, "right margin: {sp:?}");
        assert!(
            sp[1].0 - sp[0].1 >= MIN_GAP_M - 1e-4,
            "min gap honoured: {sp:?}"
        );
    }

    #[test]
    fn full_wall_window_fills_the_face() {
        let (mut s, wid) = storey_with_wall(5.0);
        let id = add_full_wall_window(&mut s, wid, 2.4, ItemId::default()).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert!((w.width_m - 5.0).abs() < 1e-3, "width = wall length");
        assert!((w.height_m - 2.4).abs() < 1e-3, "height = face height");
        match &w.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((along_m - 2.5).abs() < 1e-3, "centred on the wall");
                assert!(sill_m.abs() < 1e-3, "sill at the floor");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn full_wall_door_fills_the_wall() {
        // The field case: a 1.66 m door on a 1.66 m porch wall — the normal
        // path would demand 1.76 m (width + 2 end margins) and refuse. The
        // "Full width" checkbox routes here with the TYPED height kept.
        let (mut s, wid) = storey_with_wall(1.66);
        let id = add_full_wall_door(&mut s, wid, 2.0, DoorKind::Solid, "Door".into(), 0.0)
            .expect("a door as wide as its wall must place");
        let d = s.doors.iter().find(|d| d.id == id).unwrap();
        assert!((d.width_m - 1.66).abs() < 1e-3, "width = wall length");
        assert!((d.height_m - 2.0).abs() < 1e-3, "height as entered");
        match &d.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((along_m - 0.83).abs() < 1e-3, "centred on the wall");
                assert!(sill_m.abs() < 1e-3, "sill at the floor");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn full_width_window_keeps_typed_height_and_sill() {
        // "Full width" ticked, height still typed: width = wall length with no
        // end margins, the entered height and sill untouched.
        let (mut s, wid) = storey_with_wall(4.0);
        let id = add_full_width_window(&mut s, wid, 1.2, 0.9, ItemId::default()).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert!((w.width_m - 4.0).abs() < 1e-3, "width = wall length");
        assert!((w.height_m - 1.2).abs() < 1e-3, "height as typed");
        match &w.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((along_m - 2.0).abs() < 1e-3, "centred on the wall");
                assert!((sill_m - 0.9).abs() < 1e-3, "sill as given");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn full_width_window_rejected_on_occupied_wall() {
        // The empty-wall rule applies to every full-WIDTH placement.
        let (mut s, wid) = storey_with_wall(4.0);
        add_wall_window(&mut s, wid, 1.0, 0.8, 1.0, 1.0, ItemId::default()).unwrap();
        let err = add_full_width_window(&mut s, wid, 1.2, 0.9, ItemId::default()).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert_eq!(s.windows.len(), 1, "no window added");
        assert_eq!(s.walls.regions[0].opes.windows.len(), 1, "opes untouched");
    }

    #[test]
    fn full_height_window_takes_typed_width_and_redistributes() {
        // "Full height" ticked, width typed: sill 0 + face height, but the
        // width obeys the NORMAL margin/redistribution regime — it coexists
        // with an existing window (no empty-wall rule for full-height).
        let (mut s, wid) = storey_with_wall(4.0);
        add_wall_window(&mut s, wid, 1.8, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        let id = add_full_height_window(&mut s, wid, 2.2, 1.0, ItemId::default())
            .expect("fits beside the existing window");
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert!((w.height_m - 2.4).abs() < 1e-3, "height = face height");
        match &w.placement {
            Some(OpeningPlacement::Wall { sill_m, .. }) => {
                assert!(sill_m.abs() < 1e-3, "sill at the floor");
            }
            _ => panic!("expected a wall placement"),
        }
        let sp = spans(&s);
        assert_eq!(sp.len(), 2);
        assert!(sp[0].0 >= END_MARGIN_M - 1e-4, "left margin: {sp:?}");
        assert!(sp[1].1 <= 4.0 - END_MARGIN_M + 1e-4, "right margin: {sp:?}");
        assert!(sp[1].0 - sp[0].1 >= -1e-4, "no overlap: {sp:?}");
    }

    #[test]
    fn full_height_window_too_wide_fails_atomically() {
        let (mut s, wid) = storey_with_wall(4.0);
        add_wall_window(&mut s, wid, 1.8, 2.0, 1.2, 1.0, ItemId::default()).unwrap();
        let before = spans(&s);
        // 2.0 + 2.5 wide + margins can never fit 4.0 m; a full-height window
        // can't use the vertical-stack fallback either (it spans the face).
        let err = add_full_height_window(&mut s, wid, 2.2, 2.5, ItemId::default()).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert_eq!(s.windows.len(), 1, "rolled back");
        assert_eq!(spans(&s), before, "existing window untouched");
    }

    #[test]
    fn full_height_door_spans_floor_to_wall_top() {
        let (mut s, wid) = storey_with_wall(4.0);
        let id = add_full_height_door(&mut s, wid, 1.5, 0.9, DoorKind::Solid, "Door".into())
            .expect("door places");
        let d = s.doors.iter().find(|d| d.id == id).unwrap();
        assert!((d.width_m - 0.9).abs() < 1e-3, "width as typed");
        assert!((d.height_m - 2.4).abs() < 1e-3, "height = face height");
        match &d.placement {
            Some(OpeningPlacement::Wall { sill_m, .. }) => {
                assert!(sill_m.abs() < 1e-3, "sill at the floor");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn both_full_flags_equal_the_whole_face() {
        // Full width + Full height = the existing "window wall": the caller
        // resolves the face height and the result covers the entire face.
        let (mut s, wid) = storey_with_wall(5.0);
        let h = wall_face_height(&s, wid, 2.5).expect("wall exists");
        assert!((h - 2.4).abs() < 1e-3, "face height = wall top − base");
        let id = add_full_wall_window(&mut s, wid, h, ItemId::default()).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert!((w.width_m - 5.0).abs() < 1e-3, "width = wall length");
        assert!((w.height_m - 2.4).abs() < 1e-3, "height = face height");
        match &w.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((along_m - 2.5).abs() < 1e-3, "centred");
                assert!(sill_m.abs() < 1e-3, "sill at the floor");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn resize_full_width_takes_the_wall_and_recentres() {
        // The edit box's "Full width": an existing lone window grows to span
        // the whole wall, re-centred, at the given height.
        let (mut s, wid) = storey_with_wall(3.0);
        let id = add_wall_window(&mut s, wid, 0.8, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        resize_opening_full_width(&mut s, OpeningRef::Window(id), 1.5).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert!((w.width_m - 3.0).abs() < 1e-3, "width = wall length");
        assert!((w.height_m - 1.5).abs() < 1e-3, "height as given");
        match &w.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((along_m - 1.5).abs() < 1e-3, "re-centred");
                // 2.4 face − 1.5 height leaves 0.9 headroom: the 1 m sill is
                // clamped down to keep the window under the wall top.
                assert!((sill_m - 0.9).abs() < 1e-3, "sill clamped under the top");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn resize_full_width_refused_beside_another_opening() {
        let (mut s, wid) = storey_with_wall(4.0);
        let id = add_wall_window(&mut s, wid, 1.0, 0.8, 1.2, 1.0, ItemId::default()).unwrap();
        add_wall_window(&mut s, wid, 3.0, 0.8, 1.2, 1.0, ItemId::default()).unwrap();
        let before = spans(&s);
        let err = resize_opening_full_width(&mut s, OpeningRef::Window(id), 1.5).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert_eq!(spans(&s), before, "nothing moved or resized");
    }

    #[test]
    fn full_wall_door_rejected_on_occupied_wall() {
        let (mut s, wid) = storey_with_wall(3.0);
        add_wall_window(&mut s, wid, 1.0, 0.8, 1.0, 1.0, ItemId::default()).unwrap();
        let err =
            add_full_wall_door(&mut s, wid, 2.0, DoorKind::Solid, "Door".into(), 0.0).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert!(s.doors.is_empty(), "no door added");
        assert!(s.walls.regions[0].opes.doors.is_empty(), "opes untouched");
    }

    #[test]
    fn roof_window_hit_test_and_move() {
        // A flat 6×6 ceiling: the hit-test is region-aware (it builds the
        // footprint from the host plane), so a rooflight needs a region under it.
        let rid = ItemId::default();
        let mut s = Storey {
            ceilings: CeilingRegions {
                regions: vec![CeilingRegion {
                    id: rid,
                    polygon: Polygon3D::new(vec![
                        p3(0.0, 0.0, 2.7),
                        p3(6.0, 0.0, 2.7),
                        p3(6.0, 6.0, 2.7),
                        p3(0.0, 6.0, 2.7),
                    ]),
                    opes: Opes::default(),
                    construction_id: ItemId::default(),
                    exposure: CeilingExposure::default(),
                }],
            },
            ..Storey::default()
        };
        let id = ItemId::default();
        s.windows.push(Window {
            id,
            vertices: vec![],
            width_m: 1.0,
            height_m: 1.0,
            window_type_id: ItemId::default(),
            placement: Some(OpeningPlacement::Roof {
                ceiling_region_id: rid,
                plan: (2.0, 2.0),
            }),
            wall_anchor: None,
            assembly_id: None,
            multi_storey: false,
            shape: WindowShape::Rectangular,
            overshading: Overshading::Average,
        });
        assert_eq!(
            roof_window_at(&s, 2.3, 2.3),
            Some(id),
            "inside the plan rect"
        );
        assert_eq!(roof_window_at(&s, 5.0, 5.0), None, "outside the rect");
        move_roof_window(&mut s, id, (3.5, 4.0));
        match &s.windows[0].placement {
            Some(OpeningPlacement::Roof { plan, .. }) => assert_eq!(*plan, (3.5, 4.0)),
            _ => panic!("expected a roof placement"),
        }
        assert_eq!(
            roof_window_at(&s, 3.5, 4.0),
            Some(id),
            "hit-tests at the new spot"
        );
    }

    #[test]
    fn redistribution_rejects_and_rolls_back_when_too_short() {
        let (mut s, wid) = storey_with_wall(1.0);
        // 1.2 m window is WIDER than the 1.0 m wall — can't fit at ANY sill, so the
        // vertical-stack fallback can't rescue it either: it still rolls back.
        let err = add_wall_window(&mut s, wid, 0.5, 1.2, 1.2, 1.0, ItemId::default()).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert!(s.windows.is_empty(), "rolled back — no opening added");
        assert!(s.walls.regions[0].opes.windows.is_empty(), "opes scrubbed");
    }

    #[test]
    fn resize_opening_within_bounds_updates_size_in_place() {
        let (mut s, wid) = storey_with_wall(4.0);
        let id = add_wall_window(&mut s, wid, 2.0, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        resize_opening(&mut s, OpeningRef::Window(id), 1.5, 1.4).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert_eq!((w.width_m, w.height_m), (1.5, 1.4));
        match &w.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((along_m - 2.0).abs() < 1e-3, "fits in place — no nudge");
                assert!((sill_m - 1.0).abs() < 1e-3, "sill kept ({sill_m})");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn resize_that_cannot_fit_fails_atomically() {
        let (mut s, wid) = storey_with_wall(3.0);
        let t = ItemId::default();
        let a = add_wall_window(&mut s, wid, 0.8, 1.0, 1.0, 1.0, t).unwrap();
        add_wall_window(&mut s, wid, 2.2, 1.0, 1.0, 1.0, t).unwrap();
        let before = spans(&s);
        // Growing `a` to 2.2 m needs 2.2 + 1.0 + end margins > 3.0 even with the
        // gap compressed to touching → refused, nothing changed.
        let err = resize_opening(&mut s, OpeningRef::Window(a), 2.2, 1.0).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        let wa = s.windows.iter().find(|w| w.id == a).unwrap();
        assert_eq!((wa.width_m, wa.height_m), (1.0, 1.0), "size restored");
        match &wa.placement {
            Some(OpeningPlacement::Wall { sill_m, .. }) => {
                assert!((sill_m - 1.0).abs() < 1e-6, "sill restored");
            }
            _ => panic!("expected a wall placement"),
        }
        assert_eq!(spans(&s), before, "positions untouched");
    }

    #[test]
    fn resize_pushes_a_neighbour_that_is_in_the_way() {
        let (mut s, wid) = storey_with_wall(4.0);
        let t = ItemId::default();
        let a = add_wall_window(&mut s, wid, 1.0, 1.0, 1.0, 1.0, t).unwrap();
        add_wall_window(&mut s, wid, 2.2, 1.0, 1.0, 1.0, t).unwrap();
        // Growing `a` to 1.8 m still fits the wall, but only if the neighbour
        // gives way — the resized one is pinned, the other flows.
        resize_opening(&mut s, OpeningRef::Window(a), 1.8, 1.0).unwrap();
        let wa = s.windows.iter().find(|w| w.id == a).unwrap();
        assert_eq!((wa.width_m, wa.height_m), (1.8, 1.0));
        let sp = spans(&s);
        assert!(
            sp[1].0 - sp[0].1 >= -1e-4,
            "no overlap after resize: {sp:?}"
        );
        assert!(sp[0].0 >= END_MARGIN_M - 1e-4, "left margin: {sp:?}");
        assert!(sp[1].1 <= 4.0 - END_MARGIN_M + 1e-4, "right margin: {sp:?}");
    }

    #[test]
    fn full_wall_window_resized_smaller_reenters_the_margin_regime() {
        let (mut s, wid) = storey_with_wall(4.0);
        let id = add_full_wall_window(&mut s, wid, 2.4, ItemId::default()).unwrap();
        resize_opening(&mut s, OpeningRef::Window(id), 2.0, 2.0).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert_eq!((w.width_m, w.height_m), (2.0, 2.0));
        let sp = spans(&s);
        assert!(
            sp[0].0 >= END_MARGIN_M - 1e-4 && sp[0].1 <= 4.0 - END_MARGIN_M + 1e-4,
            "margins honoured: {sp:?}"
        );
    }

    #[test]
    fn resize_taller_lowers_the_sill_under_the_roofline() {
        let (mut s, wid) = storey_with_wall(4.0); // flat top at 2.4
        let id = add_wall_window(&mut s, wid, 2.0, 1.0, 1.0, 1.0, ItemId::default()).unwrap();
        // sill 1.0 + new height 2.0 would poke past 2.4 → sill clamps to 0.4.
        resize_opening(&mut s, OpeningRef::Window(id), 1.0, 2.0).unwrap();
        match &s.windows[0].placement {
            Some(OpeningPlacement::Wall { sill_m, .. }) => {
                assert!((sill_m - 0.4).abs() < 1e-3, "sill lowered, got {sill_m}");
            }
            _ => panic!("expected a wall placement"),
        }
    }

    #[test]
    fn set_window_attributes_round_trips() {
        let (mut s, wid) = storey_with_wall(4.0);
        let id = add_wall_window(&mut s, wid, 2.0, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        let new_type = ItemId::default();
        set_window_attributes(&mut s, id, new_type, Overshading::Heavy);
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert_eq!(w.window_type_id, new_type);
        assert_eq!(w.overshading, Overshading::Heavy);
        assert_eq!((w.width_m, w.height_m), (1.0, 1.2), "geometry untouched");
        match &w.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((along_m - 2.0).abs() < 1e-6 && (sill_m - 1.0).abs() < 1e-6);
            }
            _ => panic!("expected a wall placement"),
        }
    }

    /// When the row is horizontally FULL, a newly-placed window STACKS into the
    /// vertical space above instead of being refused. A 2 m wall fits two 0.9 m
    /// windows side-by-side; the third can't share that row, so it lands above it.
    #[test]
    fn full_row_stacks_the_next_window_into_vertical_space() {
        let (mut s, wid) = storey_with_wall(2.0); // 2.4 m tall → room to stack
        let t = ItemId::default();
        let a = add_wall_window(&mut s, wid, 0.5, 0.9, 0.9, 0.0, t).unwrap();
        let b = add_wall_window(&mut s, wid, 1.5, 0.9, 0.9, 0.0, t).unwrap();
        // Row [0,0.9] is full (3×0.9 + gaps > 2.0); this one must stack above.
        let c = add_wall_window(&mut s, wid, 1.0, 0.9, 0.9, 0.0, t).unwrap();

        assert_eq!(
            s.windows.len(),
            3,
            "all three placed — the third stacked, not refused"
        );
        let sill = |id: ItemId| {
            s.windows
                .iter()
                .find(|w| w.id == id)
                .and_then(|w| match &w.placement {
                    Some(OpeningPlacement::Wall { sill_m, .. }) => Some(*sill_m),
                    _ => None,
                })
                .unwrap()
        };
        assert!(
            sill(a) < 0.1 && sill(b) < 0.1,
            "first two stay on the floor row"
        );
        assert!(
            sill(c) >= 0.9 - 1e-3,
            "third stacked above the row, got sill {}",
            sill(c)
        );

        // No 2D overlap anywhere.
        let ops = wall_openings(&s, wid);
        for (i, o1) in ops.iter().enumerate() {
            for o2 in &ops[i + 1..] {
                let vert = o1.sill_m < o2.sill_m + o2.height_m - 1e-4
                    && o2.sill_m < o1.sill_m + o1.height_m - 1e-4;
                let along =
                    (o1.along_m - o2.along_m).abs() < (o1.width_m + o2.width_m) / 2.0 - 1e-3;
                assert!(!(vert && along), "2D overlap: {o1:?} vs {o2:?}");
            }
        }
    }

    #[test]
    fn exact_fit_succeeds_and_packs_in_order() {
        // Two 0.4 m windows + gap 0.1 + 2×margin 0.05 = 1.0 exactly.
        let (mut s, wid) = storey_with_wall(1.0);
        add_wall_window(&mut s, wid, 0.2, 0.4, 1.0, 1.0, ItemId::default()).unwrap();
        add_wall_window(&mut s, wid, 0.8, 0.4, 1.0, 1.0, ItemId::default()).unwrap();
        let sp = spans(&s);
        assert!(
            (sp[0].0 - 0.05).abs() < 1e-3,
            "first packs to the left: {sp:?}"
        );
        assert!(
            (sp[1].1 - 0.95).abs() < 1e-3,
            "second packs to the right: {sp:?}"
        );
    }

    #[test]
    fn move_opening_pushes_a_neighbour() {
        let (mut s, wid) = storey_with_wall(4.0);
        let a = add_wall_window(&mut s, wid, 1.0, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        let _b = add_wall_window(&mut s, wid, 3.0, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        // Drag A right toward B; B must yield, no overlap.
        move_opening(&mut s, OpeningRef::Window(a), 2.6, 1.0).unwrap();
        let sp = spans(&s);
        assert!(
            sp[1].0 - sp[0].1 >= MIN_GAP_M - 1e-4,
            "no overlap after move: {sp:?}"
        );
    }

    #[test]
    fn vertically_disjoint_windows_may_share_an_along() {
        let (mut s, wid) = storey_with_wall(4.0);
        // A low window (sill 0.2, height 0.9 → top 1.1) and a high window (sill
        // 1.3, height 0.9 → top 2.2) both dropped at along 2.0. Being vertically
        // disjoint, they must NOT be pushed apart — a window above another is
        // allowed; only a true 2D overlap is resolved.
        add_wall_window(&mut s, wid, 2.0, 1.0, 0.9, 0.2, ItemId::default()).unwrap();
        add_wall_window(&mut s, wid, 2.0, 1.0, 0.9, 1.3, ItemId::default()).unwrap();
        let ops = wall_openings(&s, wid);
        assert_eq!(ops.len(), 2);
        for o in &ops {
            assert!(
                (o.along_m - 2.0).abs() < 1e-3,
                "stacked window kept its along (got {})",
                o.along_m
            );
        }

        // But two windows in the SAME vertical band at the same along still
        // separate (no 2D overlap).
        let (mut s2, wid2) = storey_with_wall(4.0);
        add_wall_window(&mut s2, wid2, 2.0, 1.0, 1.0, 0.5, ItemId::default()).unwrap();
        add_wall_window(&mut s2, wid2, 2.0, 1.0, 1.0, 0.5, ItemId::default()).unwrap();
        let sp = spans(&s2);
        assert!(
            sp[1].0 - sp[0].1 >= MIN_GAP_M - 1e-4,
            "same-band windows still separate: {sp:?}"
        );
    }

    /// The motivating case: a 5 m wall with a row of 3 windows + 1 tall door, plus
    /// a wide low window stacked ABOVE the leftmost windows. The old transitive
    /// band packer rejected this — the 1.8 m door's band bridged the low row and
    /// the high window into one packing set needing 7 m > 5 m (`WallTooShort`).
    /// The 2D solver sees the door and the wide window never share an along, so it
    /// places all five with no real overlap.
    #[test]
    fn wide_window_stacks_above_a_row_with_a_tall_door() {
        let (mut s, wid) = storey_with_wall(5.0);
        let t = ItemId::default();
        add_wall_window(&mut s, wid, 0.6, 1.0, 1.0, 0.0, t).unwrap(); // band [0,1]
        add_wall_window(&mut s, wid, 1.7, 1.0, 1.0, 0.0, t).unwrap();
        add_wall_window(&mut s, wid, 2.8, 1.0, 1.0, 0.0, t).unwrap();
        add_wall_door(&mut s, wid, 3.9, 1.0, 1.8, DoorKind::Solid, "D".into(), 0.0).unwrap(); // [0,1.8]
                                                                                              // Wide window directly above the leftmost two (sill 1.0 → band [1.0,1.2]).
        add_wall_window(&mut s, wid, 1.15, 2.5, 0.2, 1.0, t).unwrap();

        let ops = wall_openings(&s, wid);
        assert_eq!(ops.len(), 5, "all five openings placed, none rolled back");

        // No genuine 2D overlap (overlap in BOTH along and height) between any pair.
        for (i, a) in ops.iter().enumerate() {
            for b in &ops[i + 1..] {
                let vert = a.sill_m < b.sill_m + b.height_m - 1e-4
                    && b.sill_m < a.sill_m + a.height_m - 1e-4;
                let along = (a.along_m - b.along_m).abs() < (a.width_m + b.width_m) / 2.0 - 1e-3;
                assert!(!(vert && along), "2D overlap: {a:?} vs {b:?}");
            }
        }

        // The wide window genuinely shares along-space with a bottom-row window —
        // i.e. it sits ABOVE it (the thing the old 1D-band packer forbade).
        let wide = ops.iter().find(|o| (o.width_m - 2.5).abs() < 1e-3).unwrap();
        let stacked = ops.iter().any(|o| {
            (o.width_m - 1.0).abs() < 1e-3
                && o.sill_m < 0.5
                && (o.along_m - wide.along_m).abs() < (o.width_m + wide.width_m) / 2.0
        });
        assert!(stacked, "wide window stacks above a bottom-row window");
    }

    /// The gap COMPRESSES below the default to fit more openings before declaring
    /// the wall too short. 3 × 1 m windows in a 3.2 m wall need 3.3 m at the
    /// default 0.1 m gap (would be `WallTooShort`), but fit at a 0.05 m gap.
    #[test]
    fn gap_compresses_to_fit_more_before_failing() {
        let (mut s, wid) = storey_with_wall(3.2);
        let t = ItemId::default();
        add_wall_window(&mut s, wid, 0.6, 1.0, 1.0, 0.5, t).unwrap();
        add_wall_window(&mut s, wid, 1.6, 1.0, 1.0, 0.5, t).unwrap();
        add_wall_window(&mut s, wid, 2.6, 1.0, 1.0, 0.5, t).unwrap(); // fixed-gap would fail here
        let sp = spans(&s);
        assert_eq!(sp.len(), 3, "all three fit via gap compression");
        assert!(
            sp[1].0 - sp[0].1 >= -1e-3 && sp[2].0 - sp[1].1 >= -1e-3,
            "compressed but no overlap: {sp:?}"
        );
    }

    /// A pinned opening holds its along while the rest flow around it (the drag
    /// case: the grabbed opening wins, others rearrange minimally).
    #[test]
    fn pinned_opening_holds_while_others_move() {
        let (mut s, wid) = storey_with_wall(4.0);
        let t = ItemId::default();
        let a = add_wall_window(&mut s, wid, 1.0, 1.0, 1.0, 0.5, t).unwrap();
        let _b = add_wall_window(&mut s, wid, 3.0, 1.0, 1.0, 0.5, t).unwrap();
        // Force both to the same along (a 2D overlap), then redistribute, A pinned.
        for w in &mut s.windows {
            if let Some(OpeningPlacement::Wall { along_m, .. }) = &mut w.placement {
                *along_m = 2.0;
            }
        }
        redistribute_wall_openings_pinned(&mut s, wid, &[OpeningRef::Window(a)]).unwrap();
        let along = |id: ItemId| {
            s.windows
                .iter()
                .find(|w| w.id == id)
                .and_then(|w| match &w.placement {
                    Some(OpeningPlacement::Wall { along_m, .. }) => Some(*along_m),
                    _ => None,
                })
                .unwrap()
        };
        assert!(
            (along(a) - 2.0).abs() < 1e-3,
            "pinned A held at 2.0, got {}",
            along(a)
        );
        assert!(
            (along(_b) - along(a)).abs() >= 1.0 - 1e-3,
            "B moved clear of pinned A, got {}",
            along(_b)
        );
    }

    #[test]
    fn delete_opening_removes_record_and_opes() {
        let (mut s, wid) = storey_with_wall(4.0);
        let a = add_wall_window(&mut s, wid, 1.0, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        add_wall_window(&mut s, wid, 3.0, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        delete_opening(&mut s, OpeningRef::Window(a));
        assert_eq!(s.windows.len(), 1, "one window left");
        assert_eq!(s.walls.regions[0].opes.windows.len(), 1, "opes pruned");
        assert!(!s.walls.regions[0].opes.windows.contains(&a));
    }

    #[test]
    fn from_overall_derives_lights_transom_and_sill() {
        // Both sides: leftover width splits evenly; the top strip (overall −
        // door height) becomes a transom; no sill given → sidelights on the floor.
        let s = DoorSidelightsSpec::from_overall(
            2.1,
            2.4,
            0.9,
            2.05,
            SidelightLayout::Both,
            None,
            false,
            ItemId::default(),
            "front".into(),
        );
        assert!((s.left_sidelight_m.unwrap() - 0.6).abs() < 1e-4);
        assert!((s.right_sidelight_m.unwrap() - 0.6).abs() < 1e-4);
        assert!((s.transom_m.unwrap() - 0.35).abs() < 1e-4);
        assert_eq!(s.left_sill_m, None);
        assert_eq!(s.right_sill_m, None);

        // Left only, with a sill and no transom (door reaches the top): all the
        // leftover goes left, and the sill lands only on the present (left) side.
        let s = DoorSidelightsSpec::from_overall(
            2.1,
            2.05,
            0.9,
            2.05,
            SidelightLayout::LeftOnly,
            Some(0.15),
            false,
            ItemId::default(),
            "front".into(),
        );
        assert!((s.left_sidelight_m.unwrap() - 1.2).abs() < 1e-4);
        assert_eq!(s.right_sidelight_m, None);
        assert_eq!(s.left_sill_m, Some(0.15));
        assert_eq!(s.right_sill_m, None);
        assert_eq!(s.transom_m, None, "door reaches the top → no transom");
    }

    #[test]
    fn compose_door_sidelights_builds_a_rigid_assembly() {
        let (mut s, wid) = storey_with_wall(6.0);
        let aid = compose_door_sidelights(
            &mut s,
            wid,
            3.0,
            DoorSidelightsSpec {
                door_width_m: 0.9,
                door_height_m: 2.0,
                left_sidelight_m: Some(0.4),
                right_sidelight_m: Some(0.4),
                left_sill_m: None,
                right_sill_m: None,
                transom_m: Some(0.4),
                transom_semicircular: false,
                window_type_id: ItemId::default(),
                door_type_name: "front".into(),
            },
        )
        .unwrap();
        // One door + 3 windows (two sidelights + transom), all in the assembly.
        assert_eq!(s.doors.len(), 1);
        assert_eq!(s.windows.len(), 3);
        assert!(s.doors[0].assembly_id == Some(aid));
        assert!(s.windows.iter().all(|w| w.assembly_id == Some(aid)));
        // The sidelights sit flush against the door (no gap inside the assembly).
        let door_along = match &s.doors[0].placement {
            Some(OpeningPlacement::Wall { along_m, .. }) => *along_m,
            _ => panic!(),
        };
        let sidelight_touch = s.windows.iter().any(|w| match &w.placement {
            Some(OpeningPlacement::Wall { along_m, .. }) => {
                // a sidelight's inner edge meets the door edge (within 1mm)
                ((along_m + w.width_m / 2.0) - (door_along - 0.9 / 2.0)).abs() < 1e-3
                    || ((along_m - w.width_m / 2.0) - (door_along + 0.9 / 2.0)).abs() < 1e-3
            }
            _ => false,
        });
        assert!(
            sidelight_touch,
            "sidelights are flush against the door panel"
        );
    }

    #[test]
    fn roof_window_defaults_to_very_little_overshading() {
        // A rooflight looks at the sky — new roof windows default to
        // `VeryLittle`; wall windows keep the `Average` default.
        let rid = ItemId::default();
        let mut s = Storey {
            ceilings: CeilingRegions {
                regions: vec![CeilingRegion {
                    id: rid,
                    polygon: Polygon3D::new(vec![
                        p3(0.0, 0.0, 2.7),
                        p3(6.0, 0.0, 2.7),
                        p3(6.0, 6.0, 2.7),
                        p3(0.0, 6.0, 2.7),
                    ]),
                    opes: Opes::default(),
                    construction_id: ItemId::default(),
                    exposure: CeilingExposure::default(),
                }],
            },
            ..Storey::default()
        };
        let id = add_roof_window(&mut s, rid, (2.0, 2.0), 0.8, 1.1, ItemId::default()).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert_eq!(w.overshading, Overshading::VeryLittle);

        let (mut s, wid) = storey_with_wall(4.0);
        let id = add_wall_window(&mut s, wid, 2.0, 1.0, 1.2, 1.0, ItemId::default()).unwrap();
        let w = s.windows.iter().find(|w| w.id == id).unwrap();
        assert_eq!(
            w.overshading,
            Overshading::Average,
            "wall default unchanged"
        );
    }

    /// The composite spec used by the full-span composer tests: a 1.0 m door
    /// with token sidelights either side (their widths are re-derived by the
    /// full-width ops) and no transom/sills unless a test says otherwise.
    fn composite_spec() -> DoorSidelightsSpec {
        DoorSidelightsSpec {
            door_width_m: 1.0,
            door_height_m: 2.0,
            left_sidelight_m: Some(0.3),
            right_sidelight_m: Some(0.3),
            left_sill_m: None,
            right_sill_m: None,
            transom_m: None,
            transom_semicircular: false,
            window_type_id: ItemId::default(),
            door_type_name: "front".into(),
        }
    }

    #[test]
    fn compose_full_width_spans_the_wall_exactly() {
        // Full width on an empty 4 m wall: door width typed (1.0), the two
        // sidelights share the 3 m leftover equally — the widths sum EXACTLY
        // to the wall length, flush to both ends (no end margins).
        let (mut s, wid) = storey_with_wall(4.0);
        let aid = compose_full_width_door_sidelights(&mut s, wid, composite_spec()).unwrap();
        assert_eq!(s.doors.len(), 1);
        assert_eq!(s.windows.len(), 2);
        assert!((s.doors[0].width_m - 1.0).abs() < 1e-4, "door as typed");
        assert!(s.windows.iter().all(|w| w.assembly_id == Some(aid)));
        assert!(
            s.windows.iter().all(|w| (w.width_m - 1.5).abs() < 1e-4),
            "even sidelight split"
        );
        let sp = spans(&s);
        let total: f32 = sp.iter().map(|(l, r)| r - l).sum();
        assert!(
            (total - 4.0).abs() < 1e-3,
            "widths sum to the wall: {total}"
        );
        assert!(sp.first().unwrap().0.abs() < 1e-3, "flush left: {sp:?}");
        assert!(
            (sp.last().unwrap().1 - 4.0).abs() < 1e-3,
            "flush right: {sp:?}"
        );
    }

    #[test]
    fn compose_full_width_lone_sidelight_takes_all_the_leftover() {
        let (mut s, wid) = storey_with_wall(4.0);
        let spec = DoorSidelightsSpec {
            right_sidelight_m: None,
            ..composite_spec()
        };
        compose_full_width_door_sidelights(&mut s, wid, spec).unwrap();
        assert_eq!(s.windows.len(), 1, "left sidelight only");
        assert!(
            (s.windows[0].width_m - 3.0).abs() < 1e-4,
            "the lone side takes the whole leftover"
        );
    }

    #[test]
    fn compose_full_width_rejected_on_occupied_wall() {
        let (mut s, wid) = storey_with_wall(4.0);
        add_wall_window(&mut s, wid, 1.0, 0.8, 1.0, 1.0, ItemId::default()).unwrap();
        let err = compose_full_width_door_sidelights(&mut s, wid, composite_spec()).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert_eq!(s.windows.len(), 1, "nothing added");
        assert!(s.doors.is_empty(), "no door panel added");
    }

    #[test]
    fn compose_full_height_keeps_the_door_and_tops_it_with_a_transom() {
        // Full height with typed widths: the ASSEMBLY fills floor → the 2.4 m
        // face — the door keeps its TYPED 2.0 m height, the sidelights run
        // sill 0 up to the face, and the transom fills exactly the 0.4 m strip
        // left above the door (over the door width). The NORMAL margin path
        // still applies — it redistributes beside an existing window rather
        // than demanding an empty wall.
        let (mut s, wid) = storey_with_wall(6.0);
        add_wall_window(&mut s, wid, 5.5, 0.6, 1.0, 1.0, ItemId::default()).unwrap();
        let spec = DoorSidelightsSpec {
            left_sill_m: Some(0.6),
            right_sill_m: Some(0.6),
            transom_m: Some(0.1), // typed value is superseded by face − door
            ..composite_spec()
        };
        let aid = compose_full_height_door_sidelights(&mut s, wid, 2.0, spec).unwrap();
        assert!(
            (s.doors[0].height_m - 2.0).abs() < 1e-3,
            "door keeps its typed height"
        );
        let members: Vec<_> = s
            .windows
            .iter()
            .filter(|w| w.assembly_id == Some(aid))
            .collect();
        assert_eq!(members.len(), 3, "two sidelights + the transom");
        // Sidelights: the two 0.3 m-wide members — sill 0 up to the face.
        for w in members.iter().filter(|w| (w.width_m - 0.3).abs() < 1e-4) {
            assert!((w.height_m - 2.4).abs() < 1e-3, "sidelight = face height");
            match &w.placement {
                Some(OpeningPlacement::Wall { sill_m, .. }) => {
                    assert!(sill_m.abs() < 1e-4, "sidelight sill forced to 0");
                }
                _ => panic!("expected a wall placement"),
            }
        }
        // The transom: face − door = 0.4 high, over the DOOR width, sitting
        // on the door top at the door's centre.
        let door_along = match &s.doors[0].placement {
            Some(OpeningPlacement::Wall { along_m, .. }) => *along_m,
            _ => panic!("expected a wall placement"),
        };
        let transom = members
            .iter()
            .find(|w| (w.width_m - 1.0).abs() < 1e-4)
            .expect("a transom as wide as the door");
        assert!((transom.height_m - 0.4).abs() < 1e-3, "face − door strip");
        match &transom.placement {
            Some(OpeningPlacement::Wall {
                along_m, sill_m, ..
            }) => {
                assert!((sill_m - 2.0).abs() < 1e-3, "sits on the door top");
                assert!((along_m - door_along).abs() < 1e-3, "over the door");
            }
            _ => panic!("expected a wall placement"),
        }
        let sp = spans(&s);
        assert!(sp.first().unwrap().0 >= END_MARGIN_M - 1e-4, "margins kept");
    }

    #[test]
    fn compose_full_height_transom_degenerates_when_door_reaches_the_face() {
        // Door typed at exactly the face height: no strip remains above it,
        // so no transom window is placed.
        let (mut s, wid) = storey_with_wall(6.0);
        let spec = DoorSidelightsSpec {
            door_height_m: 2.4,
            transom_m: Some(0.4), // typed value is superseded (→ none)
            ..composite_spec()
        };
        let aid = compose_full_height_door_sidelights(&mut s, wid, 3.0, spec).unwrap();
        assert!((s.doors[0].height_m - 2.4).abs() < 1e-3, "door = face");
        assert_eq!(
            s.windows
                .iter()
                .filter(|w| w.assembly_id == Some(aid))
                .count(),
            2,
            "two sidelights, no degenerate transom"
        );
    }

    #[test]
    fn compose_full_wall_fills_the_whole_face() {
        // Both checkboxes: widths span the wall exactly AND the assembly runs
        // floor → roofline — door at its typed height, sidelights up to the
        // face, the transom the strip above the door.
        let (mut s, wid) = storey_with_wall(4.0);
        let aid = compose_full_wall_door_sidelights(&mut s, wid, composite_spec()).unwrap();
        let sp = spans(&s);
        assert!(sp.first().unwrap().0.abs() < 1e-3, "flush left: {sp:?}");
        assert!(
            (sp.last().unwrap().1 - 4.0).abs() < 1e-3,
            "flush right: {sp:?}"
        );
        assert!(
            (s.doors[0].height_m - 2.0).abs() < 1e-3,
            "door keeps its typed height"
        );
        let members: Vec<_> = s
            .windows
            .iter()
            .filter(|w| w.assembly_id == Some(aid))
            .collect();
        assert_eq!(members.len(), 3, "two sidelights + the transom");
        // Sidelights split the 3 m leftover evenly and run up to the face.
        for w in members.iter().filter(|w| (w.width_m - 1.5).abs() < 1e-4) {
            assert!((w.height_m - 2.4).abs() < 1e-3, "sidelight = face height");
        }
        let transom = members
            .iter()
            .find(|w| (w.width_m - 1.0).abs() < 1e-4)
            .expect("a transom as wide as the door");
        assert!((transom.height_m - 0.4).abs() < 1e-3, "face − door strip");
    }

    #[test]
    fn door_sidelights_custom_sill_full_width_and_semicircular_transom() {
        let (mut s, wid) = storey_with_wall(6.0);
        compose_door_sidelights(
            &mut s,
            wid,
            3.0,
            DoorSidelightsSpec {
                door_width_m: 1.0,
                door_height_m: 2.0,
                left_sidelight_m: Some(0.4),
                right_sidelight_m: Some(0.0), // 0 → no right sidelight
                left_sill_m: Some(0.6),       // custom sill lifts the left light
                right_sill_m: None,
                transom_m: Some(0.5),
                transom_semicircular: true,
                window_type_id: ItemId::default(),
                door_type_name: "front".into(),
            },
        )
        .unwrap();
        derive_opening_vertices(&mut s);
        // Door + left sidelight + transom = 2 windows (right was 0 → absent).
        assert_eq!(s.windows.len(), 2, "right sidelight (0) omitted");
        let total_w = 0.4 + 1.0; // left + door (no right)
                                 // The left sidelight: custom 0.6 sill, height shrunk so its top meets the
                                 // door top (2.0 − 0.6 = 1.4).
        let left = s
            .windows
            .iter()
            .find(|w| (w.width_m - 0.4).abs() < 1e-4)
            .expect("left sidelight");
        match &left.placement {
            Some(OpeningPlacement::Wall { sill_m, .. }) => {
                assert!((sill_m - 0.6).abs() < 1e-4, "custom sill, got {sill_m}");
            }
            _ => panic!(),
        }
        assert!(
            (left.height_m - 1.4).abs() < 1e-4,
            "top aligns with door top"
        );
        // The transom spans the WHOLE assembly width and is arched (its derived
        // outline is a tessellated arch, not a 4-corner rectangle).
        let transom = s
            .windows
            .iter()
            .find(|w| w.shape == WindowShape::Arched)
            .expect("arched transom");
        assert!(
            (transom.width_m - total_w).abs() < 1e-4,
            "transom spans the assembly: {} vs {total_w}",
            transom.width_m
        );
        assert!(
            transom.vertices.len() > 4,
            "arched transom is tessellated, got {} verts",
            transom.vertices.len()
        );
    }

    #[test]
    fn pick_wall_along_finds_nearest_and_projects() {
        let (s, wid) = storey_with_wall(4.0);
        // Click 0.2 m off the wall, above x=2.5.
        let (id, along, len) = pick_wall_along(&s, 2.5, 0.2, 0.5).unwrap();
        assert_eq!(id, wid);
        assert!((along - 2.5).abs() < 1e-4, "projected along = {along}");
        assert!((len - 4.0).abs() < 1e-4);
        // Too far → None.
        assert!(pick_wall_along(&s, 2.5, 5.0, 0.5).is_none());
    }

    /// A roof window is anchored by its PLAN POINT, not the ceiling region id
    /// (which is regenerated every resolve) — so it survives a re-resolve that
    /// replaces the region with a fresh id.
    #[test]
    fn roof_window_survives_ceiling_id_change() {
        let region = |id: ItemId| CeilingRegion {
            id,
            polygon: Polygon3D::new(vec![
                p3(0.0, 0.0, 2.7),
                p3(6.0, 0.0, 2.7),
                p3(6.0, 6.0, 2.7),
                p3(0.0, 6.0, 2.7),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: CeilingExposure::default(),
        };
        let r1 = ItemId::default();
        let mut storey = Storey {
            ceilings: CeilingRegions {
                regions: vec![region(r1)],
            },
            ..Storey::default()
        };
        let wid =
            add_roof_window(&mut storey, r1, (3.0, 3.0), 1.0, 1.0, ItemId::default()).unwrap();
        // Re-resolve regenerates the ceiling with a FRESH id.
        storey.ceilings.regions = vec![region(ItemId::default())];
        derive_opening_vertices(&mut storey);
        prune_orphaned_openings(&mut storey);
        assert_eq!(storey.windows.len(), 1, "the roof window is not pruned");
        assert_eq!(storey.windows[0].id, wid);
        assert_eq!(storey.windows[0].vertices.len(), 4, "vertices re-derived");
    }

    /// `rebuild_opes` makes each surface aware of the openings on it: a wall
    /// window lands in its wall's `opes`, a roof window in its roof region's —
    /// rebuilt purely from placements (so it's correct after any re-resolve).
    #[test]
    fn rebuild_opes_links_wall_and_roof_openings() {
        let wid = ItemId::default();
        let rid = ItemId::default();
        let mut storey = Storey {
            walls: WallRegions {
                regions: vec![wall(
                    wid,
                    vec![
                        p3(0.0, 0.0, 0.0),
                        p3(4.0, 0.0, 0.0),
                        p3(4.0, 0.0, 2.4),
                        p3(0.0, 0.0, 2.4),
                    ],
                )],
            },
            ceilings: CeilingRegions {
                regions: vec![CeilingRegion {
                    id: rid,
                    polygon: Polygon3D::new(vec![
                        p3(0.0, 0.0, 2.4),
                        p3(4.0, 0.0, 2.4),
                        p3(4.0, 4.0, 2.4),
                        p3(0.0, 4.0, 2.4),
                    ]),
                    opes: Opes::default(),
                    construction_id: ItemId::default(),
                    exposure: CeilingExposure::default(),
                }],
            },
            ..Storey::default()
        };
        let w1 = add_wall_window(&mut storey, wid, 2.0, 1.0, 1.0, 1.0, ItemId::default()).unwrap();
        let w2 =
            add_roof_window(&mut storey, rid, (2.0, 2.0), 1.0, 1.0, ItemId::default()).unwrap();
        // Wipe opes to prove rebuild repopulates them from placements alone.
        storey.walls.regions[0].opes = Opes::default();
        storey.ceilings.regions[0].opes = Opes::default();
        rebuild_opes(&mut storey);
        assert_eq!(
            storey.walls.regions[0].opes.windows,
            vec![w1],
            "the wall knows its window"
        );
        assert_eq!(
            storey.ceilings.regions[0].opes.windows,
            vec![w2],
            "the roof region knows its roof window"
        );
    }
}
