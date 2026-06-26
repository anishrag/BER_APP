//! Window/door OPENINGS over a resolved `Storey` — pure data transforms (no
//! UI, DB, or `StoreyDoc`). Openings live durably on `Storey.windows`/`.doors`
//! as parametric `OpeningPlacement`s; their 3D `vertices` are a CACHE this
//! module derives from the resolved wall/roof geometry at the resolve tail.
//!
//! Placement ops (pick / add / move / redistribute / delete) live alongside the
//! derivation here so all opening logic is in one pure module the controller
//! drives.
use super::common::ItemId;
use super::fenestration::{Door, DoorKind, OpeningPlacement, Window};
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

/// A structured door+sidelights assembly: a door panel with optional flanking
/// sidelights and a transom (top light) over the door. The composer (`compose_
/// door_sidelights`) lays these out and emits one `Door` + the side/top
/// `Window`s as one rigid assembly.
#[derive(Debug, Clone)]
pub struct DoorSidelightsSpec {
    pub door_width_m: f32,
    pub door_height_m: f32,
    /// Width of the left/right sidelight window (full door height), if present.
    pub left_sidelight_m: Option<f32>,
    pub right_sidelight_m: Option<f32>,
    /// Height of the transom over the door (door width), if present.
    pub transom_m: Option<f32>,
    pub window_type_id: ItemId,
    pub door_type_name: String,
}

/// The length of the wall with `wall_id`, or `None` if absent/degenerate.
fn wall_length(storey: &Storey, wall_id: ItemId) -> Option<f32> {
    storey
        .walls
        .regions
        .iter()
        .find(|r| r.id == wall_id)
        .and_then(|r| r.base_frame())
        .map(|(_, _, len, _)| len as f32)
}

/// The nearest OUTLINE wall to a world plan point and the along-distance of the
/// projected point from that wall's `exterior[0]`. Operates on the resolved
/// storey's walls (so it returns a real `wall_id`). `(wall_id, along_m, len_m)`
/// or `None` if no wall base edge is within `tol`.
pub fn pick_wall_along(storey: &Storey, wx: f64, wy: f64, tol: f64) -> Option<(ItemId, f32, f32)> {
    let mut best: Option<(ItemId, f32, f32, f64)> = None;
    for r in &storey.walls.regions {
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
        .roofs
        .region_strictly_containing(domain::points::Point2f64::new(wx, wy))
        .map(|r| (r.id, (wx as f32, wy as f32)))
}

/// Add a window to a wall at `along_m` (centre), then redistribute the wall so
/// nothing overlaps. Rolls back and returns `WallTooShort` if it can't fit.
pub fn add_wall_window(
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
/// redistribution — rooflights don't line up like wall windows).
pub fn add_roof_window(
    storey: &mut Storey,
    roof_region_id: ItemId,
    plan: (f32, f32),
    width_m: f32,
    height_m: f32,
    window_type_id: ItemId,
) -> Result<ItemId, OpeningError> {
    if width_m <= 0.0 || height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    let Some(r) = storey
        .roofs
        .regions
        .iter_mut()
        .find(|r| r.id == roof_region_id)
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
            roof_region_id,
            plan,
        }),
        wall_anchor: None,
        assembly_id: None,
    });
    Ok(id)
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
    if spec.door_width_m <= 0.0 || spec.door_height_m <= 0.0 {
        return Err(OpeningError::ZeroSize);
    }
    if wall_length(storey, wall_id).is_none() {
        return Err(OpeningError::NoHostRegion);
    }
    let assembly_id = ItemId::default();
    let left_w = spec.left_sidelight_m.unwrap_or(0.0).max(0.0);
    let right_w = spec.right_sidelight_m.unwrap_or(0.0).max(0.0);
    let total_w = left_w + spec.door_width_m + right_w;
    let assembly_left = along_m - total_w / 2.0;
    let mut added: Vec<OpeningRef> = Vec::new();
    let mut place_window = |storey: &mut Storey, centre: f32, w: f32, h: f32, sill: f32| {
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
        });
        link_window(storey, wall_id, id);
        added.push(OpeningRef::Window(id));
    };
    if left_w > 0.0 {
        place_window(
            storey,
            assembly_left + left_w / 2.0,
            left_w,
            spec.door_height_m,
            0.0,
        );
    }
    let door_centre = assembly_left + left_w + spec.door_width_m / 2.0;
    if right_w > 0.0 {
        place_window(
            storey,
            assembly_left + left_w + spec.door_width_m + right_w / 2.0,
            right_w,
            spec.door_height_m,
            0.0,
        );
    }
    if let Some(th) = spec.transom_m.filter(|t| *t > 0.0) {
        place_window(
            storey,
            door_centre,
            spec.door_width_m,
            th,
            spec.door_height_m,
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
    redistribute_or_rollback(storey, wall_id, &added)?;
    Ok(assembly_id)
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
    // mutation, instead of each re-implementing it (only the review path did).
    let new_sill_m = {
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
    for r in &mut storey.roofs.regions {
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
pub(crate) fn redistribute_wall_openings(
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
/// `resolve_storey`, after walls/roofs are regenerated, so the host geometry is
/// final. An opening whose host id no longer resolves keeps its last vertices
/// (then [`prune_orphaned_openings`] drops it).
pub fn derive_opening_vertices(storey: &mut Storey) {
    let win: Vec<Option<Vec<(f32, f32, f32)>>> = storey
        .windows
        .iter()
        .map(|w| {
            w.placement
                .as_ref()
                .and_then(|p| opening_vertices(storey, p, w.width_m, w.height_m))
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
            d.placement
                .as_ref()
                .and_then(|p| opening_vertices(storey, p, d.width_m, d.height_m))
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
            .roofs
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
    for r in &mut storey.roofs.regions {
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
                let id = storey.roofs.region_strictly_containing(pt)?.id;
                storey
                    .roofs
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
            Some(Tgt::Roof(i)) => storey.roofs.regions[i].opes.windows.push(id),
            None => {}
        }
    }
    for (id, t) in drs {
        match t {
            Some(Tgt::Wall(i)) => storey.walls.regions[i].opes.doors.push(id),
            Some(Tgt::Roof(i)) => storey.roofs.regions[i].opes.doors.push(id),
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
            let top_cap = wall.top_z_at_along(left).min(wall.top_z_at_along(right));
            let top = (bottom + height_m as f64).min(top_cap).max(bottom);
            let p = |d: f64| (origin.x + ux * d, origin.y + uy * d);
            let (lx, ly) = p(left);
            let (rx, ry) = p(right);
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
                .roofs
                .region_strictly_containing(domain::points::Point2f64::new(cx, cy))?;
            let (hw, hh) = (width_m as f64 * 0.5, height_m as f64 * 0.5);
            // Plan rectangle corners (CCW); lift each onto the region plane.
            let corners = [
                (cx - hw, cy - hh),
                (cx + hw, cy - hh),
                (cx + hw, cy + hh),
                (cx - hw, cy + hh),
            ];
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
    use crate::ber::fenestration::{Door, OpeningPlacement, Window};
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
            roofs: CeilingRegions {
                regions: vec![region],
            },
            windows: vec![window(
                ItemId::default(),
                1.0,
                1.0,
                Some(OpeningPlacement::Roof {
                    roof_region_id: rid,
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
    fn redistribution_rejects_and_rolls_back_when_too_short() {
        let (mut s, wid) = storey_with_wall(1.0);
        // 1.2 m window is WIDER than the 1.0 m wall — can't fit at ANY sill, so the
        // vertical-stack fallback can't rescue it either: it still rolls back.
        let err = add_wall_window(&mut s, wid, 0.5, 1.2, 1.2, 1.0, ItemId::default()).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert!(s.windows.is_empty(), "rolled back — no opening added");
        assert!(s.walls.regions[0].opes.windows.is_empty(), "opes scrubbed");
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
                transom_m: Some(0.4),
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
            roofs: CeilingRegions {
                regions: vec![region(r1)],
            },
            ..Storey::default()
        };
        let wid =
            add_roof_window(&mut storey, r1, (3.0, 3.0), 1.0, 1.0, ItemId::default()).unwrap();
        // Re-resolve regenerates the ceiling with a FRESH id.
        storey.roofs.regions = vec![region(ItemId::default())];
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
            roofs: CeilingRegions {
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
        storey.roofs.regions[0].opes = Opes::default();
        rebuild_opes(&mut storey);
        assert_eq!(
            storey.walls.regions[0].opes.windows,
            vec![w1],
            "the wall knows its window"
        );
        assert_eq!(
            storey.roofs.regions[0].opes.windows,
            vec![w2],
            "the roof region knows its roof window"
        );
    }
}
