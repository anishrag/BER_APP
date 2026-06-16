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

/// Default sill heights (metres above the wall base) and spacing constants for
/// auto-redistribution. Tuning-overridable later; sensible Irish-BER defaults.
pub const DEFAULT_WINDOW_SILL_M: f32 = 1.0;
pub const DEFAULT_DOOR_SILL_M: f32 = 0.0;
const MIN_GAP_M: f32 = 0.1;
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
        assembly_id: None,
    });
    link_window(storey, wall_id, id);
    redistribute_or_rollback(storey, wall_id, &[OpeningRef::Window(id)])?;
    Ok(id)
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
    redistribute_wall_openings(storey, wall_id)
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

/// Space the wall's openings so none overlap: order-preserving, within
/// `[margin, len-margin]`, min-gap `g`, each kept as close to its desired centre
/// as possible. A door+sidelights assembly is treated as ONE rigid unit (its
/// bounding width) and its members translate together. `WallTooShort` when the
/// total can't fit; the storey is left unchanged in that case.
pub fn redistribute_wall_openings(
    storey: &mut Storey,
    wall_id: ItemId,
) -> Result<(), OpeningError> {
    let len = wall_length(storey, wall_id).ok_or(OpeningError::NoHostRegion)?;
    // Gather (ref, centre, width, assembly) for every opening on this wall.
    let mut items: Vec<(OpeningRef, f32, f32, Option<ItemId>)> = Vec::new();
    for w in &storey.windows {
        if let Some(OpeningPlacement::Wall {
            wall_id: wid,
            along_m,
            ..
        }) = &w.placement
        {
            if *wid == wall_id {
                items.push((OpeningRef::Window(w.id), *along_m, w.width_m, w.assembly_id));
            }
        }
    }
    for d in &storey.doors {
        if let Some(OpeningPlacement::Wall {
            wall_id: wid,
            along_m,
            ..
        }) = &d.placement
        {
            if *wid == wall_id {
                items.push((OpeningRef::Door(d.id), *along_m, d.width_m, d.assembly_id));
            }
        }
    }
    if items.is_empty() {
        return Ok(());
    }
    // Group into UNITS (an assembly = one unit; everything else singleton).
    struct Unit {
        members: Vec<(OpeningRef, f32)>, // (ref, current centre)
        centre: f32,
        width: f32,
    }
    let mut by_assembly: std::collections::HashMap<ItemId, Vec<(OpeningRef, f32, f32)>> =
        std::collections::HashMap::new();
    let mut units: Vec<Unit> = Vec::new();
    for (r, c, w, a) in &items {
        match a {
            Some(aid) => by_assembly.entry(*aid).or_default().push((*r, *c, *w)),
            None => units.push(Unit {
                members: vec![(*r, *c)],
                centre: *c,
                width: *w,
            }),
        }
    }
    for (_, members) in by_assembly {
        let left = members
            .iter()
            .map(|(_, c, w)| c - w / 2.0)
            .fold(f32::INFINITY, f32::min);
        let right = members
            .iter()
            .map(|(_, c, w)| c + w / 2.0)
            .fold(f32::NEG_INFINITY, f32::max);
        units.push(Unit {
            members: members.iter().map(|(r, c, _)| (*r, *c)).collect(),
            centre: (left + right) / 2.0,
            width: right - left,
        });
    }
    units.sort_by(|a, b| a.centre.partial_cmp(&b.centre).unwrap());
    let n = units.len();
    let g = MIN_GAP_M;
    let m = END_MARGIN_M;
    let needed: f32 = units.iter().map(|u| u.width).sum::<f32>() + g * (n as f32 - 1.0) + 2.0 * m;
    if needed > len + 1e-4 {
        return Err(OpeningError::WallTooShort {
            needed_m: needed,
            available_m: len,
        });
    }
    // Left-packed lower bounds and right-packed upper bounds on each unit's
    // LEFT edge, then relax toward the desired left edge, order-preserving.
    let mut lo = vec![0.0f32; n];
    let mut hi = vec![0.0f32; n];
    lo[0] = m;
    for i in 1..n {
        lo[i] = lo[i - 1] + units[i - 1].width + g;
    }
    hi[n - 1] = len - m - units[n - 1].width;
    for i in (0..n - 1).rev() {
        hi[i] = hi[i + 1] - g - units[i].width;
    }
    let mut left = vec![0.0f32; n];
    for i in 0..n {
        // `hi` can dip a hair below `lo` from float rounding at an exact fit;
        // raising the cap keeps `clamp` well-formed (it panics if min > max).
        let hi_i = hi[i].max(lo[i]);
        let desired = (units[i].centre - units[i].width / 2.0).clamp(lo[i], hi_i);
        left[i] = if i == 0 {
            desired
        } else {
            desired.max(left[i - 1] + units[i - 1].width + g)
        };
    }
    for i in (0..n).rev() {
        let cap = if i == n - 1 {
            len - m - units[i].width
        } else {
            left[i + 1] - g - units[i].width
        };
        left[i] = left[i].min(cap).max(lo[i]);
    }
    // New centre per ref, applied as a translation of the unit's members.
    let mut new_centre: std::collections::HashMap<OpeningRef, f32> =
        std::collections::HashMap::new();
    for (i, u) in units.iter().enumerate() {
        let unit_new_centre = left[i] + u.width / 2.0;
        let delta = unit_new_centre - u.centre;
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
fn redistribute_or_rollback(
    storey: &mut Storey,
    wall_id: ItemId,
    added: &[OpeningRef],
) -> Result<(), OpeningError> {
    match redistribute_wall_openings(storey, wall_id) {
        Ok(()) => Ok(()),
        Err(e) => {
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
            Err(e)
        }
    }
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
        CeilingRegion, CeilingRegions, Opes, Polygon3D, Storey, WallExposure, WallRegion,
        WallRegions,
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
        let err = add_wall_window(&mut s, wid, 0.5, 1.2, 1.2, 1.0, ItemId::default()).unwrap_err();
        assert!(matches!(err, OpeningError::WallTooShort { .. }));
        assert!(s.windows.is_empty(), "rolled back — no opening added");
        assert!(s.walls.regions[0].opes.windows.is_empty(), "opes scrubbed");
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
