//! The Sketch CANVAS: paints the plan + every overlay, routes pointer input
//! (strokes, taps, drags, pan/zoom) into [`SketchEvent`]s, and hosts the
//! floating 2D/3D toggle in its top-right corner.

use egui::epaint::PathShape;

use super::touch::TouchNav;
use super::{EguiStroke, SketchEvent, SketchView};
use crate::controller::app_controller::{AssessmentPhase, PhaseDisplayRegion, SelectMode};
use crate::ui::pages::events::UiEvent;
use crate::ui::render;
use crate::ui::render::ViewParams;
use crate::ui::widgets::HintExt;
use ber_model::ber::ber_struct::Ber;
use ber_model::ber::structural::WallExposure;
use domain::sketch::{EditTools, SketchMode};

/// Width an in-place "Edit attributes" window occupies (the window's own
/// content is capped so the OUTER frame lands on this width), and the
/// fallback height estimate used until the box has rendered once — after the
/// first frame the layout uses each box's MEASURED height (egui remembers
/// every `Area`'s rect frame to frame).
const EDIT_BOX_W: f32 = 165.0;
const EDIT_SLOT_H: f32 = 155.0;
/// Gap between a window column and the canvas edge.
const EDIT_COL_PAD: f32 = 10.0;
/// Clear gap kept BETWEEN adjacent floaties along an edge, so boxes never
/// touch/overlap.
const EDIT_GAP: f32 = 12.0;
/// Top/bottom inset reserved for the window columns.
const EDIT_TOP_PAD: f32 = 12.0;
/// Clearance kept below the canvas top for the phase signpost + surface banner
/// (the floating bar at `center_top + 8` and the banner just under it), so the
/// top row of attribute windows and the side columns don't sit on top of them.
const EDIT_TOP_SAFE: f32 = 84.0;
/// Footprint of a collapsed CHIP — the compact title-bar a group degrades to
/// when the margins can't fit its full box without intersecting another
/// (group colour + name; tap to expand). Chips pack shelf-wise into a band
/// along the canvas bottom.
const CHIP_W: f32 = 120.0;
const CHIP_H: f32 = 30.0;

/// One in-place attribute window's placement (see [`compute_structure_edit_layout`]).
struct EditWindow {
    group_index: usize,
    /// Top-left of the floating `Area` (screen px).
    pos: egui::Pos2,
    /// The box footprint the layout reserved (measured height for a full box,
    /// the chip footprint for a chip) — what the no-intersect packing packs.
    size: egui::Vec2,
    /// Collapsed to a compact title-bar chip (margins over capacity — tap to
    /// expand; expanding one may collapse another rather than overlap).
    chip: bool,
    /// The group's connector target (world, in its storey's own frame).
    centroid: (f64, f64),
    /// Whether `centroid` is a REAL target — an interior point of one of the
    /// group's own in-scope member surfaces. `false` when no member resolves
    /// in scope (a stale session): the box still lays out, but no connector
    /// line/dot is drawn — a dot into a surface the group doesn't cover
    /// (e.g. an inter-storey area) must never render.
    has_target: bool,
    /// Which storey CELL `centroid` lives in — `Some` on the all-storeys
    /// layout (the target maps through that cell's transform); `None` on the
    /// single-storey layout (the active storey's own transform applies).
    storey_idx: Option<usize>,
    color: egui::Color32,
}

/// The in-place "Edit attributes" layout: the per-group windows (prescriptive
/// slots on the single-storey view, margin packing on all-storeys), plus the
/// view that fits the plan into the space left over.
struct StructureEditLayout {
    view: ViewParams,
    windows: Vec<EditWindow>,
    /// ALL-STOREYS only: the inner rect the storey ROW is fitted into (the gap
    /// between the reserved margins, shrunk clear of any user-dragged boxes —
    /// the plan never renders under a box). The row's per-cell fit is rect-driven
    /// (`all_storeys_2d::cell_transforms`), so the canvas renders + inverts
    /// the row against THIS rect — the rect-shrink is the all-storeys
    /// equivalent of the single-storey layout's zoomed-out `view`. `None` on
    /// the single-storey layout.
    all_storeys_inner: Option<egui::Rect>,
    /// SINGLE-STOREY only: how many groups past the prescriptive layout's
    /// [`MAX_SLOTS`] are NOT displayed — drives the small canvas note
    /// ("not all groups shown — N more"). Always 0 on the all-storeys layout
    /// (its overflow degrades to chips instead).
    hidden_groups: usize,
    /// SINGLE-STOREY only: a freshly OPTIMISED base slot order the caller
    /// must persist into the session store ([`slot_order_store_id`]) — set
    /// exactly when no valid stored order existed (session open / group-set
    /// change), so the optimisation runs once, not per frame, and user swaps
    /// permute the cached base. `None` otherwise (and always on all-storeys).
    store_slot_order: Option<Vec<usize>>,
}

/// Slots per band of the PRESCRIPTIVE single-storey layout
/// ([`place_windows_prescriptive`]): 3 across the top, then up to 5 down the
/// right side (the right column owns both right corners), then 3 along the
/// bottom — [`MAX_SLOTS`] at most; groups beyond that are not displayed (a
/// small canvas note says how many).
const SLOTS_TOP: usize = 3;
const SLOTS_RIGHT: usize = 5;
const MAX_SLOTS: usize = SLOTS_TOP + SLOTS_RIGHT + 3;

/// One group's input to the prescriptive layout: identity + colour + the
/// connector anchor (like [`MarginSeed`]) plus its measured box height.
struct SlotSeed {
    group_index: usize,
    color: egui::Color32,
    anchor: (f64, f64),
    /// Whether `anchor` is a real member-interior target (see
    /// [`EditWindow::has_target`]).
    has_target: bool,
    h: f32,
}

/// The PRESCRIPTIVE single-storey slot layout (the all-storeys tab keeps the
/// adaptive margin packing, [`place_windows_in_margins`]). `seeds` arrive in
/// SLOT order — the session's slot order: the connector-optimised base
/// ([`optimise_slot_order`]) permuted by the user's drag-SWAPS — and fill
/// top row left→right, then right column top→bottom, then bottom row
/// left→right; each box `EDIT_BOX_W` wide, sized to its measured height,
/// packed from the band's start with the usual gap. Boxes never rest outside
/// the slot grid, so the plan rect derives EXACTLY from the occupied bands
/// (top = tallest top-row box, right = the column width, bottom = tallest
/// bottom-row box). Returns the windows, that inner rect, and how many
/// groups past [`MAX_SLOTS`] are hidden.
fn place_windows_prescriptive(
    rect: egui::Rect,
    seeds: &[SlotSeed],
) -> (Vec<EditWindow>, egui::Rect, usize) {
    let shown = seeds.len().min(MAX_SLOTS);
    let hidden = seeds.len() - shown;
    let col_x = rect.right() - EDIT_COL_PAD - EDIT_BOX_W;
    let top_y = rect.top() + EDIT_TOP_SAFE;

    let mut windows = Vec::with_capacity(shown);
    let (mut top_band, mut bottom_band) = (0.0f32, 0.0f32);
    let mut col_occupied = false;
    let mut col_y = top_y;
    for (slot, s) in seeds.iter().take(shown).enumerate() {
        let pos = if slot < SLOTS_TOP {
            top_band = top_band.max(s.h);
            egui::pos2(
                rect.left() + EDIT_COL_PAD + slot as f32 * (EDIT_BOX_W + EDIT_GAP),
                top_y,
            )
        } else if slot < SLOTS_TOP + SLOTS_RIGHT {
            col_occupied = true;
            let pos = egui::pos2(col_x, col_y);
            col_y += s.h + EDIT_GAP;
            pos
        } else {
            bottom_band = bottom_band.max(s.h);
            let b = slot - SLOTS_TOP - SLOTS_RIGHT;
            egui::pos2(
                rect.left() + EDIT_COL_PAD + b as f32 * (EDIT_BOX_W + EDIT_GAP),
                rect.bottom() - EDIT_TOP_PAD - s.h,
            )
        };
        windows.push(EditWindow {
            group_index: s.group_index,
            pos,
            size: egui::vec2(EDIT_BOX_W, s.h),
            chip: false,
            centroid: s.anchor,
            has_target: s.has_target,
            storey_idx: None,
            color: s.color,
        });
    }

    let top_h = if top_band > 0.0 {
        EDIT_TOP_SAFE + top_band + EDIT_TOP_PAD
    } else {
        EDIT_TOP_PAD
    };
    let right_w = if col_occupied {
        EDIT_BOX_W + 2.0 * EDIT_COL_PAD
    } else {
        EDIT_COL_PAD
    };
    let bottom_h = if bottom_band > 0.0 {
        bottom_band + 2.0 * EDIT_TOP_PAD
    } else {
        EDIT_TOP_PAD
    };
    let inner = egui::Rect::from_min_max(
        egui::pos2(rect.left() + EDIT_COL_PAD, rect.top() + top_h),
        egui::pos2(rect.right() - right_w, rect.bottom() - bottom_h),
    );
    (windows, inner, hidden)
}

/// The point a slot's connector LEAVES its box, estimated with the uniform
/// `EDIT_SLOT_H` slot grid (assignment must not depend on who occupies a
/// slot, or the optimisation wouldn't be well-defined): the box-edge
/// midpoint facing the plan — bottom-centre for the top row, left-centre
/// for the right column, top-centre for the bottom row.
fn slot_anchor_estimate(rect: egui::Rect, slot: usize) -> egui::Pos2 {
    let pitch = EDIT_BOX_W + EDIT_GAP;
    if slot < SLOTS_TOP {
        egui::pos2(
            rect.left() + EDIT_COL_PAD + slot as f32 * pitch + EDIT_BOX_W * 0.5,
            rect.top() + EDIT_TOP_SAFE + EDIT_SLOT_H,
        )
    } else if slot < SLOTS_TOP + SLOTS_RIGHT {
        let k = (slot - SLOTS_TOP) as f32;
        egui::pos2(
            rect.right() - EDIT_COL_PAD - EDIT_BOX_W,
            rect.top() + EDIT_TOP_SAFE + k * (EDIT_SLOT_H + EDIT_GAP) + EDIT_SLOT_H * 0.5,
        )
    } else {
        let b = (slot - SLOTS_TOP - SLOTS_RIGHT) as f32;
        egui::pos2(
            rect.left() + EDIT_COL_PAD + b * pitch + EDIT_BOX_W * 0.5,
            rect.bottom() - EDIT_TOP_PAD - EDIT_SLOT_H,
        )
    }
}

/// Whether segments `a1→a2` and `b1→b2` PROPERLY intersect (a single
/// strictly-interior crossing; shared endpoints and collinear touches don't
/// count) — the connector-crossing predicate of [`optimise_slot_order`].
/// Presentation math on screen points, so it lives here rather than in
/// `manip_plan` (whose f64 equivalent is private to its geom internals).
fn segments_properly_cross(a1: egui::Pos2, a2: egui::Pos2, b1: egui::Pos2, b2: egui::Pos2) -> bool {
    let ccw = |p: egui::Pos2, q: egui::Pos2, r: egui::Pos2| {
        (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)
    };
    let d1 = ccw(b1, b2, a1);
    let d2 = ccw(b1, b2, a2);
    let d3 = ccw(a1, a2, b1);
    let d4 = ccw(a1, a2, b2);
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}

/// The OPTIMISED base group→slot assignment for the prescriptive layout:
/// `order[slot] = group`, minimising the summed connector lengths (slot
/// anchor → group anchor, `anchors[group]` on screen) plus a heavy penalty —
/// 10 × the canvas diagonal — per pair of connectors that properly cross.
/// Greedy start (each group, in group order, takes the nearest free slot),
/// then 2-opt hill-climbing: apply the best improving pairwise swap until
/// none improves. n ≤ [`MAX_SLOTS`] slots, so this is microseconds, and every
/// step is deterministic (fixed iteration order, strict improvement) — the
/// same input always yields the same layout. Groups past [`MAX_SLOTS`]
/// aren't displayed and keep their identity tail. Run ONCE per session /
/// group-set change; user swaps then permute the cached result.
fn optimise_slot_order(rect: egui::Rect, anchors: &[egui::Pos2]) -> Vec<usize> {
    let n = anchors.len();
    let m = n.min(MAX_SLOTS);
    let slots: Vec<egui::Pos2> = (0..m).map(|k| slot_anchor_estimate(rect, k)).collect();

    // Greedy: each group takes the nearest still-free slot, in group order.
    let mut order: Vec<usize> = vec![0; m];
    let mut taken = vec![false; m];
    for g in 0..m {
        let k = (0..m)
            .filter(|&k| !taken[k])
            .min_by(|&a, &b| {
                let (da, db) = (
                    (slots[a] - anchors[g]).length(),
                    (slots[b] - anchors[g]).length(),
                );
                da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
            })
            .expect("g < m free slots remain");
        taken[k] = true;
        order[k] = g;
    }

    let penalty = rect.size().length() * 10.0;
    let cost = |order: &[usize]| -> f32 {
        let segs: Vec<(egui::Pos2, egui::Pos2)> = order
            .iter()
            .enumerate()
            .map(|(k, &g)| (slots[k], anchors[g]))
            .collect();
        let mut c: f32 = segs.iter().map(|(a, b)| (*a - *b).length()).sum();
        for i in 0..segs.len() {
            for j in i + 1..segs.len() {
                if segments_properly_cross(segs[i].0, segs[i].1, segs[j].0, segs[j].1) {
                    c += penalty;
                }
            }
        }
        c
    };

    // 2-opt: best improving swap, repeated to a fixpoint.
    let mut current = cost(&order);
    loop {
        let mut best: Option<(f32, usize, usize)> = None;
        for i in 0..m {
            for j in i + 1..m {
                order.swap(i, j);
                let c = cost(&order);
                order.swap(i, j);
                if c + 1e-3 < current && best.is_none_or(|(bc, _, _)| c < bc) {
                    best = Some((c, i, j));
                }
            }
        }
        match best {
            Some((c, i, j)) => {
                order.swap(i, j);
                current = c;
            }
            None => break,
        }
    }
    order.extend(m..n);
    order
}

/// Apply one drag-swap to the session slot order: transpose the SLOTS of
/// groups `a` and `b` — identified by their INDEX in the session's group
/// list, the one identity that is unique for the session (names are not:
/// real records hold several groups named "Group 4", and a duplicate name
/// must never move a bystander box). Unknown indices are a no-op.
fn transpose_slot_order(order: &mut [usize], a: usize, b: usize) {
    let (pa, pb) = (
        order.iter().position(|&g| g == a),
        order.iter().position(|&g| g == b),
    );
    if let (Some(pa), Some(pb)) = (pa, pb) {
        order.swap(pa, pb);
    }
}

/// The SWAP target for a box dropped in the single-storey prescriptive
/// layout: the other box whose rect the dropped rect overlaps MOST (greatest
/// positive overlap area). `None` when the drop touches no other box —
/// releasing over empty space snaps the box back to its own slot.
fn swap_target(windows: &[EditWindow], dragged_group: usize, dropped: egui::Rect) -> Option<usize> {
    windows
        .iter()
        .filter(|w| w.group_index != dragged_group)
        .filter_map(|w| {
            let r = egui::Rect::from_min_size(w.pos, w.size);
            rects_strictly_overlap(dropped, r).then(|| (w.group_index, dropped.intersect(r).area()))
        })
        .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(g, _)| g)
}

/// Place items (window top-left coords, given in their PREFERRED order) along
/// one axis with NO overlap: each item spans `exts[i]` from its coordinate,
/// and a forward pass pushes each to at least `gap` past the previous item's
/// far edge (and ≥ `lo`), then a backward pass pulls any whose span overran
/// `hi` back (keeping `gap`). The result keeps each near its preferred spot
/// while every span stays inside `[lo, hi]`. The caller guarantees capacity —
/// `Σ exts + (n−1)·gap ≤ hi − lo` (the margin assignment sends anything past
/// capacity to the chip band instead) — so the two passes can never be forced
/// to overlap; the final clamp is a numeric safety net only.
fn declutter_extents(prefs: &mut [f32], exts: &[f32], gap: f32, lo: f32, hi: f32) {
    let n = prefs.len();
    if n == 0 {
        return;
    }
    let hi = hi.max(lo);
    for i in 0..n {
        let floor = if i == 0 {
            lo
        } else {
            prefs[i - 1] + exts[i - 1] + gap
        };
        if prefs[i] < floor {
            prefs[i] = floor;
        }
    }
    for i in (0..n).rev() {
        let ceil = if i + 1 == n {
            hi - exts[i]
        } else {
            prefs[i + 1] - gap - exts[i]
        };
        if prefs[i] > ceil {
            prefs[i] = ceil;
        }
    }
    for (p, e) in prefs.iter_mut().zip(exts) {
        *p = p.clamp(lo, (hi - e).max(lo));
    }
}

/// One group's seed for the margin placement: identity + colour, the world
/// connector TARGET (walls → nearest selected wall, else the centroid — see
/// `group_connector_anchor`), and where that target sits on screen under the
/// plain FULL-canvas fit. Placement — edge assignment AND ordering — is
/// driven by `screen` (where the line points), so the boxes sit near their
/// targets and the connectors don't cross.
struct MarginSeed {
    group_index: usize,
    color: egui::Color32,
    /// World-space connector target, in `storey_idx`'s frame.
    anchor: (f64, f64),
    /// See [`EditWindow::has_target`].
    has_target: bool,
    /// See [`EditWindow::storey_idx`].
    storey_idx: Option<usize>,
    /// The anchor under the full-rect fit (pre-reservation).
    screen: egui::Pos2,
    /// The box's height on screen — measured from its `Area` rect last frame,
    /// `EDIT_SLOT_H` until it has rendered once (width is fixed: `EDIT_BOX_W`).
    h: f32,
}

/// The margin-placement CORE shared by the single-storey and all-storeys
/// layouts: lay the in-place attribute windows into the canvas MARGINS,
/// leaving the gap in the middle for the plan. Each group's window goes to
/// whichever canvas EDGE its group is nearest (top / bottom / left / right)
/// and is positioned along that edge close to the group's own spot. Spreading
/// across all four margins — rather than forcing every window into one pair
/// of gutters — keeps each connector short and roughly radial and keeps the
/// plan large in the centre. Corners are kept clear by confining the side
/// columns to the un-reserved height and the top/bottom rows to the
/// un-reserved width.
///
/// NO BOX EVER INTERSECTS ANOTHER — auto-placed or PINNED (user-dragged,
/// `pinned` rects): every edge packs only what fits edge-to-edge at real
/// (measured) box sizes with an `EDIT_GAP` gap into the free segments AROUND
/// the pinned rects (a dropped box evicts whatever auto boxes sat under it —
/// they repack elsewhere or demote), and any group past capacity COLLAPSES
/// to a compact title-bar CHIP instead — shelf-packed, also gap-separated
/// and pinned-avoiding, into a band along the canvas bottom (tap a chip to
/// expand it; the expansion demotes whichever full box then falls past
/// capacity, rather than overlapping). `expand_first` names the group
/// guaranteed a full box (the one the user just expanded). Returns the
/// placed windows plus the inner rect left for the plan; `None` when the
/// canvas is too small.
fn place_windows_in_margins(
    rect: egui::Rect,
    plan_centre: egui::Pos2,
    seeds: &[MarginSeed],
    expand_first: Option<usize>,
    pinned: &[egui::Rect],
) -> Option<(Vec<EditWindow>, egui::Rect)> {
    let col_w = EDIT_BOX_W + 2.0 * EDIT_COL_PAD;

    // Assign each group to a side by WHERE IT SITS RELATIVE TO THE PLAN'S OWN
    // CENTRE (its quadrant), not the nearest canvas edge — otherwise a
    // letterboxed plan (wide → tall side gaps, or tall → wide top/bottom gaps)
    // pushes every group onto the same axis and we're back to two-sided.
    //
    // But the quadrant is only a PREFERENCE: each edge holds only what fits at
    // full size, and groups are placed onto their most preferred edge that
    // still has room — decisive groups (clearly on one side) claim their edge
    // first, and ones near a corner spill to a perpendicular edge that's
    // free. Edge codes: 0 = left, 1 = right, 2 = top, 3 = bottom.
    let pref_and_decisiveness = |p: egui::Pos2| -> ([usize; 4], f32) {
        let (dx, dy) = (p.x - plan_centre.x, p.y - plan_centre.y);
        let (h, opp_h) = if dx < 0.0 { (0, 1) } else { (1, 0) };
        let (v, opp_v) = if dy < 0.0 { (2, 3) } else { (3, 2) };
        let prefs = if dx.abs() >= dy.abs() {
            [h, v, opp_v, opp_h]
        } else {
            [v, h, opp_h, opp_v]
        };
        (prefs, (dx.abs() - dy.abs()).abs())
    };

    // Decisive groups first, so corner-ish ones are the ones that spill over —
    // but the group the user just expanded from a chip goes FIRST of all: it
    // must get a full box, whatever else that displaces.
    let mut order: Vec<usize> = (0..seeds.len()).collect();
    order.sort_by(|&a, &b| {
        let da = pref_and_decisiveness(seeds[a].screen).1;
        let db = pref_and_decisiveness(seeds[b].screen).1;
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });
    if let Some(g) = expand_first {
        if let Some(pos) = order.iter().position(|&k| seeds[k].group_index == g) {
            let k = order.remove(pos);
            order.insert(0, k);
        }
    }

    // The tallest box in play — the conservative cross-extent of the top /
    // bottom row strips for the pinned-obstacle overlap test.
    let max_h_all = seeds.iter().map(|s| s.h).fold(EDIT_SLOT_H, f32::max);
    // Full-width chips per shelf row — the fallback when pinned boxes block
    // the whole band (some overlap is then unavoidable; the user's box wins).
    let per_chip_row_full = (((rect.width() - 2.0 * EDIT_COL_PAD + EDIT_GAP) / (CHIP_W + EDIT_GAP))
        .floor() as usize)
        .max(1);

    let mk_window = |k: usize, pos: egui::Pos2, size: egui::Vec2, chip: bool| EditWindow {
        group_index: seeds[k].group_index,
        pos,
        size,
        chip,
        centroid: seeds[k].anchor,
        has_target: seeds[k].has_target,
        storey_idx: seeds[k].storey_idx,
        color: seeds[k].color,
    };

    // One packing attempt with the trailing `forced` groups in `order` sent
    // straight to chips. PINNED (user-dragged) boxes are OBSTACLES: each
    // margin band packs its boxes into the free SEGMENTS around every pinned
    // rect that intrudes on it ([`free_segments`]), so an auto box under a
    // dropped box repacks elsewhere or demotes to a chip; the chip shelves
    // likewise skip pinned rects. Iterated to a fixpoint on the chip band's
    // ROW count: more rows → a taller band → shorter columns / a higher
    // bottom row → possibly more chips → possibly more rows; rows never need
    // to shrink mid-attempt, so it terminates (≤ one iteration per seed).
    // Returns the fully-placed windows (boxes + chips), the band height and
    // the inner rect the margins leave for the plan.
    let attempt = |forced: usize| -> (Vec<EditWindow>, egui::Rect) {
        let mut chip_rows = 0usize;
        loop {
            let band = chip_rows as f32 * (CHIP_H + EDIT_GAP);
            let col_lo = rect.top() + EDIT_TOP_SAFE;
            let col_hi = (rect.bottom() - band - EDIT_TOP_PAD).max(col_lo);
            // A top/bottom row only has the width BETWEEN the side columns —
            // else rows over-fill when columns are also occupied. The top row
            // hangs below the signpost clearance; the bottom row bottom-aligns
            // on the columns' lower bound (just above the chip band).
            let row_lo = rect.left() + col_w;
            let row_hi = (rect.right() - col_w).max(row_lo);
            let row_top_y = rect.top() + EDIT_TOP_SAFE;
            let row_bot_base = col_hi;

            // The strip each edge's boxes occupy — a pinned box overlapping a
            // strip blocks its interval of that edge's run.
            let strips = [
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), col_lo),
                    egui::pos2(rect.left() + col_w, col_hi),
                ),
                egui::Rect::from_min_max(
                    egui::pos2(rect.right() - col_w, col_lo),
                    egui::pos2(rect.right(), col_hi),
                ),
                egui::Rect::from_min_max(
                    egui::pos2(row_lo, row_top_y),
                    egui::pos2(row_hi, row_top_y + max_h_all),
                ),
                egui::Rect::from_min_max(
                    egui::pos2(row_lo, row_bot_base - max_h_all),
                    egui::pos2(row_hi, row_bot_base),
                ),
            ];
            let mut edges: Vec<Vec<RunSegment>> = (0..4)
                .map(|e| {
                    let along_y = e < 2;
                    let (lo, hi) = if along_y {
                        (col_lo, col_hi)
                    } else {
                        (row_lo, row_hi)
                    };
                    let blocks = pinned
                        .iter()
                        .filter(|p| rects_strictly_overlap(**p, strips[e]))
                        .map(|p| {
                            if along_y {
                                (p.min.y - EDIT_GAP, p.max.y + EDIT_GAP)
                            } else {
                                (p.min.x - EDIT_GAP, p.max.x + EDIT_GAP)
                            }
                        })
                        .collect();
                    free_segments(lo, hi, blocks)
                })
                .collect();

            // Assignment: the most preferred edge with a segment that has
            // room; within the edge, the segment nearest the group's spot.
            let mut chips: Vec<usize> = Vec::new();
            for (rank, &k) in order.iter().enumerate() {
                if rank >= order.len() - forced {
                    chips.push(k);
                    continue;
                }
                let (prefs, _) = pref_and_decisiveness(seeds[k].screen);
                let mut placed = false;
                for e in prefs {
                    let ext = if e < 2 { seeds[k].h } else { EDIT_BOX_W };
                    let pref_c = if e < 2 {
                        seeds[k].screen.y
                    } else {
                        seeds[k].screen.x
                    };
                    let mut best: Option<(f32, usize)> = None;
                    for (si, seg) in edges[e].iter().enumerate() {
                        let need = if seg.items.is_empty() {
                            ext
                        } else {
                            ext + EDIT_GAP
                        };
                        if seg.used + need <= seg.hi - seg.lo {
                            let d = (pref_c - pref_c.clamp(seg.lo, seg.hi)).abs();
                            if best.is_none_or(|(bd, _)| d < bd) {
                                best = Some((d, si));
                            }
                        }
                    }
                    if let Some((_, si)) = best {
                        let seg = &mut edges[e][si];
                        seg.used += if seg.items.is_empty() {
                            ext
                        } else {
                            ext + EDIT_GAP
                        };
                        seg.items.push(k);
                        placed = true;
                        break;
                    }
                }
                if !placed {
                    chips.push(k);
                }
            }

            // Chip shelves: the band's free x-runs around any pinned box
            // touching it (a pinned box blocks the whole band's height — a
            // box is taller than a chip row anyway). A band the pinned boxes
            // cover wall-to-wall falls back to the full width.
            let band_strip = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.bottom() - band),
                egui::pos2(rect.right(), rect.bottom()),
            );
            let chip_blocks: Vec<(f32, f32)> = pinned
                .iter()
                .filter(|p| rects_strictly_overlap(**p, band_strip))
                .map(|p| (p.min.x - EDIT_GAP, p.max.x + EDIT_GAP))
                .collect();
            let mut chip_segs = free_segments(
                rect.left() + EDIT_COL_PAD,
                rect.right() - EDIT_COL_PAD,
                chip_blocks,
            );
            let seg_fit =
                |s: &RunSegment| ((s.hi - s.lo + EDIT_GAP) / (CHIP_W + EDIT_GAP)).floor() as usize;
            let mut per_row: usize = chip_segs.iter().map(seg_fit).sum();
            if per_row == 0 {
                chip_segs = free_segments(
                    rect.left() + EDIT_COL_PAD,
                    rect.right() - EDIT_COL_PAD,
                    Vec::new(),
                );
                per_row = per_chip_row_full;
            }
            let rows_needed = if chips.is_empty() {
                0
            } else {
                chips.len().div_ceil(per_row)
            };
            if rows_needed > chip_rows {
                chip_rows = rows_needed;
                continue;
            }

            // ── The attempt is feasible: place everything. ──
            let mut windows = Vec::with_capacity(seeds.len());

            // LEFT / RIGHT columns: window y tracks the group's screen y,
            // de-cluttered within its free segment. Columns never collide
            // with the rows: those are confined to the width between the
            // column strips.
            for (e, left_side) in [(0usize, true), (1, false)] {
                let x = if left_side {
                    rect.left() + EDIT_COL_PAD
                } else {
                    rect.right() - EDIT_COL_PAD - EDIT_BOX_W
                };
                for seg in &mut edges[e] {
                    seg.items.sort_by(|&a, &b| {
                        seeds[a]
                            .screen
                            .y
                            .partial_cmp(&seeds[b].screen.y)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let exts: Vec<f32> = seg.items.iter().map(|&k| seeds[k].h).collect();
                    let mut ys: Vec<f32> = seg
                        .items
                        .iter()
                        .map(|&k| seeds[k].screen.y - seeds[k].h * 0.5)
                        .collect();
                    declutter_extents(&mut ys, &exts, EDIT_GAP, seg.lo, seg.hi);
                    for (slot, &k) in seg.items.iter().enumerate() {
                        windows.push(mk_window(
                            k,
                            egui::pos2(x, ys[slot]),
                            egui::vec2(EDIT_BOX_W, seeds[k].h),
                            false,
                        ));
                    }
                }
            }

            // TOP / BOTTOM rows: window x tracks the group's screen x within
            // its free segment; the bottom row bottom-aligns just above the
            // chip band so uneven heights grow AWAY from the canvas edge.
            for (e, top_side) in [(2usize, true), (3, false)] {
                for seg in &mut edges[e] {
                    seg.items.sort_by(|&a, &b| {
                        seeds[a]
                            .screen
                            .x
                            .partial_cmp(&seeds[b].screen.x)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let exts: Vec<f32> = vec![EDIT_BOX_W; seg.items.len()];
                    let mut xs: Vec<f32> = seg
                        .items
                        .iter()
                        .map(|&k| seeds[k].screen.x - EDIT_BOX_W * 0.5)
                        .collect();
                    declutter_extents(&mut xs, &exts, EDIT_GAP, seg.lo, seg.hi);
                    for (slot, &k) in seg.items.iter().enumerate() {
                        let y = if top_side {
                            row_top_y
                        } else {
                            row_bot_base - seeds[k].h
                        };
                        windows.push(mk_window(
                            k,
                            egui::pos2(xs[slot], y),
                            egui::vec2(EDIT_BOX_W, seeds[k].h),
                            false,
                        ));
                    }
                }
            }

            // CHIP BAND: the overflow groups as compact title-bar chips,
            // shelf-packed in group order across the free segments —
            // deterministic, gap-separated, never intersecting each other,
            // any full box, or (band width permitting) any pinned box.
            chips.sort_by_key(|&k| seeds[k].group_index);
            let chips_top = rect.bottom() - band + EDIT_GAP * 0.5;
            let mut it = chips.iter();
            'shelves: for row_i in 0..chip_rows {
                let y = chips_top + row_i as f32 * (CHIP_H + EDIT_GAP);
                for seg in &chip_segs {
                    for c in 0..seg_fit(seg) {
                        let Some(&k) = it.next() else { break 'shelves };
                        windows.push(mk_window(
                            k,
                            egui::pos2(seg.lo + c as f32 * (CHIP_W + EDIT_GAP), y),
                            egui::vec2(CHIP_W, CHIP_H),
                            true,
                        ));
                    }
                }
            }

            // Reserve a margin only on edges that actually hold a window.
            let has = |e: usize| edges[e].iter().any(|s| !s.items.is_empty());
            let edge_max_h = |e: usize| {
                edges[e]
                    .iter()
                    .flat_map(|s| s.items.iter())
                    .map(|&k| seeds[k].h)
                    .fold(EDIT_SLOT_H, f32::max)
            };
            let left_w = if has(0) { col_w } else { EDIT_COL_PAD };
            let right_w = if has(1) { col_w } else { EDIT_COL_PAD };
            // A top row sits BELOW the signpost, so the plan must clear the
            // signpost band PLUS the row height; the bottom has no overlay,
            // so a normal band suffices.
            let top_h = if has(2) {
                EDIT_TOP_SAFE + edge_max_h(2) + EDIT_TOP_PAD
            } else {
                EDIT_TOP_PAD
            };
            let bottom_h = if has(3) {
                edge_max_h(3) + 2.0 * EDIT_TOP_PAD
            } else {
                EDIT_TOP_PAD
            };
            let inner = egui::Rect::from_min_max(
                egui::pos2(rect.left() + left_w, rect.top() + top_h),
                egui::pos2(rect.right() - right_w, rect.bottom() - bottom_h - band),
            );
            break (windows, inner);
        }
    };

    // Start with no forced chips; while the margins squeeze the plan below a
    // usable minimum, demote the least-decisive groups (the tail of `order`,
    // never the just-expanded head) to chips one by one — full boxes trade
    // for plan space rather than ever overlapping. Only a canvas too small
    // for even the all-chips layout gives up entirely.
    let mut forced = 0usize;
    loop {
        let (windows, inner) = attempt(forced);
        if inner.width() >= 60.0 && inner.height() >= 60.0 {
            return Some((windows, inner));
        }
        if forced >= order.len() {
            return None;
        }
        forced += 1;
    }
}

/// Whether two rects overlap with positive area (a shared edge/corner does
/// not count) — the collision predicate every no-intersect rule here uses.
fn rects_strictly_overlap(a: egui::Rect, b: egui::Rect) -> bool {
    a.min.x < b.max.x && b.min.x < a.max.x && a.min.y < b.max.y && b.min.y < a.max.y
}

/// One free interval of a margin band's run, plus the packing bookkeeping for
/// the items assigned into it (`used` = occupied extent including inter-item
/// gaps).
struct RunSegment {
    lo: f32,
    hi: f32,
    used: f32,
    items: Vec<usize>,
}

/// Subtract `blocks` (unsorted, possibly overlapping intervals) from
/// `[lo, hi]`: the free sub-intervals in ascending order. Slivers under a
/// pixel are dropped — nothing packs into them anyway.
fn free_segments(lo: f32, hi: f32, mut blocks: Vec<(f32, f32)>) -> Vec<RunSegment> {
    blocks.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut segs: Vec<RunSegment> = Vec::new();
    let mut cursor = lo;
    let push = |a: f32, b: f32, segs: &mut Vec<RunSegment>| {
        if b - a > 1.0 {
            segs.push(RunSegment {
                lo: a,
                hi: b,
                used: 0.0,
                items: Vec::new(),
            });
        }
    };
    for (b0, b1) in blocks {
        if b0 > cursor {
            push(cursor, b0.min(hi), &mut segs);
        }
        cursor = cursor.max(b1);
        if cursor >= hi {
            break;
        }
    }
    push(cursor, hi, &mut segs);
    segs
}

/// The per-frame layout inputs that live in egui memory rather than the
/// controller: each group's MEASURED box height from the previous frame
/// (`EDIT_SLOT_H` until it has rendered once); the single-storey SLOT ORDER
/// (`None` until a session's base has been optimised — the compute then runs
/// [`optimise_slot_order`] once and hands the result back for storage; user
/// drag-SWAPS transpose the stored order in place); the all-storeys DRAGGED
/// pin positions (that tab keeps free pinning); and the group most recently
/// expanded from an all-storeys chip (guaranteed a full box).
struct EditBoxUiState {
    heights: Vec<f32>,
    /// `order[slot] = group`; validated as a permutation of `0..n` — a
    /// group-set change invalidates it, which re-runs the base optimisation.
    slot_order: Option<Vec<usize>>,
    dragged: Vec<Option<egui::Pos2>>,
    expand_first: Option<usize>,
}

/// The session-scoped store of user-dragged box positions on the ALL-STOREYS
/// tab, keyed by group index, kept in egui temp memory (cleared when the
/// edit session closes). The single-storey view has no pins — its drags SWAP
/// slots in the [`slot_order_store_id`] order instead.
type DraggedBoxes = std::collections::HashMap<usize, egui::Pos2>;

/// The `Area` id of group `i`'s attribute box — shared by the renderer and
/// the measured-size lookup so both address the same egui memory.
fn attr_area_id(ui: &egui::Ui, i: usize) -> egui::Id {
    ui.id().with(("structure_attr", i))
}

/// Where [`DraggedBoxes`] lives in egui temp memory.
fn drag_store_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("structure_attr_dragged")
}

/// Where the expanded-chip choice (`usize` group index) lives in egui temp
/// memory.
fn expand_store_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("structure_attr_expanded")
}

/// Where the single-storey slot order (`Vec<usize>`, `order[slot] = group`)
/// lives in egui temp memory: the optimised session base permuted by the
/// user's drag-swaps. Cleared when the edit session closes.
fn slot_order_store_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("structure_slot_order")
}

/// Where the LIVE box-drag state lives while a box is carried by its grip
/// handle: `(group, offset from its laid-out position)`. The renderer alone
/// reads it (the layout keeps every box in its slot/pin until the DROP), so
/// the plan never re-fits under the finger. Cleared on release.
fn box_drag_store_id(ui: &egui::Ui) -> egui::Id {
    ui.id().with("structure_box_drag")
}

/// The grip strip at the top of every attribute box — its explicit DRAG
/// HANDLE. Area movability was retired here deliberately: the layout forces
/// each box's position every frame (`current_pos` overwrites the area's
/// stored pivot, so egui's own drag fought the force), and on touch a drag
/// starting over the box's interactive content (title field, dropdowns)
/// never reached the area background anyway. A dedicated `Sense::drag()`
/// strip has no competing children, claims the touch immediately, and keeps
/// the drag via egui's pointer capture even as the box follows with a
/// one-frame lag.
fn drag_handle_ui(ui: &mut egui::Ui, color: egui::Color32) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 16.0), egui::Sense::drag());
    // Three grip lines, painted (no glyph — font coverage on-device is not
    // a given; see the ✔ swap), brighter while grabbed.
    let stroke = egui::Stroke::new(
        1.5,
        if resp.dragged() {
            egui::Color32::WHITE
        } else {
            color
        },
    );
    let c = rect.center();
    for dy in [-3.0, 0.0, 3.0] {
        ui.painter().line_segment(
            [
                egui::pos2(c.x - 14.0, c.y + dy),
                egui::pos2(c.x + 14.0, c.y + dy),
            ],
            stroke,
        );
    }
    resp.on_hover_cursor(egui::CursorIcon::Grab)
}

/// Gather [`EditBoxUiState`] for this frame from egui memory.
fn edit_box_ui_state(
    ui: &egui::Ui,
    edit: &crate::controller::app_controller::StructureEdit,
) -> EditBoxUiState {
    let n = edit.groups.len();
    let dragged_map: DraggedBoxes = ui
        .ctx()
        .data(|d| d.get_temp(drag_store_id(ui)))
        .unwrap_or_default();
    EditBoxUiState {
        heights: (0..n)
            .map(|i| {
                ui.ctx()
                    .memory(|m| m.area_rect(attr_area_id(ui, i)))
                    .map(|r| r.height())
                    .unwrap_or(EDIT_SLOT_H)
            })
            .collect(),
        slot_order: ui
            .ctx()
            .data(|d| d.get_temp::<Vec<usize>>(slot_order_store_id(ui)))
            .filter(|order| {
                let mut sorted = order.clone();
                sorted.sort_unstable();
                order.len() == n && sorted == (0..n).collect::<Vec<_>>()
            }),
        dragged: (0..n).map(|i| dragged_map.get(&i).copied()).collect(),
        expand_first: ui
            .ctx()
            .data(|d| d.get_temp::<usize>(expand_store_id(ui)))
            .filter(|&g| g < n),
    }
}

/// ALL-STOREYS only: split a group list into auto-packed seeds and
/// directly-placed windows for the groups the user has DRAGGED — a dragged
/// box sits exactly where it was dropped (anywhere on the canvas), keeps its
/// measured size, and frees its margin slot for the others; the PLAN then
/// re-fits to clear it ([`plan_rect_avoiding_pinned`]), so the drawing never
/// renders under a box. (Single-storey drags SWAP slots instead.)
fn dragged_window(
    box_ui: &EditBoxUiState,
    group_index: usize,
    anchor: (f64, f64),
    has_target: bool,
    storey_idx: Option<usize>,
    color: egui::Color32,
) -> Option<EditWindow> {
    let pos = box_ui.dragged.get(group_index).copied().flatten()?;
    Some(EditWindow {
        group_index,
        pos,
        size: egui::vec2(EDIT_BOX_W, box_ui.heights[group_index]),
        chip: false,
        centroid: anchor,
        has_target,
        storey_idx,
        color,
    })
}

/// The SINGLE-STOREY in-place "Edit attributes" layout: the PRESCRIPTIVE
/// slot arrangement ([`place_windows_prescriptive`]) — up to 3 boxes across
/// the top, 5 down the right, 3 along the bottom — in the session's slot
/// order: the connector-OPTIMISED base ([`optimise_slot_order`], computed
/// once when no valid stored order exists and handed back via
/// `store_slot_order`) permuted by the user's drag-swaps. The plan then fits
/// the exact space the occupied bands leave (boxes never rest off the slot
/// grid, so no obstacle subtraction is needed here). Groups past the 11
/// slots aren't displayed (`hidden_groups` drives the canvas note). `None`
/// when there's no fittable plan or no groups, leaving the caller on the
/// normal view.
fn compute_structure_edit_layout(
    rect: egui::Rect,
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    storey: &ber_model::ber::structural::Storey,
    edit: &crate::controller::app_controller::StructureEdit,
    base: ViewParams,
    box_ui: &EditBoxUiState,
) -> Option<StructureEditLayout> {
    let i = current_storey_idx?;
    let ber = ber?;
    let (min_xy, max_xy) = render::fit_bbox_for_storey(ber, i)?;
    let fans = render::structure_fan::group_fan_layout(storey, edit);
    if fans.is_empty() {
        return None;
    }

    // Per GROUP (fans are in group order): the connector's world anchor —
    // `None` when no member resolves IN SCOPE (the connector then doesn't
    // draw; the group's fan centroid still steers the box layout).
    let anchors: Vec<Option<(f64, f64)>> = fans
        .iter()
        .map(|f| render::structure_fan::group_connector_anchor(storey, &edit.groups[f.group_index]))
        .collect();
    let layout_anchor =
        |g: usize, fallback: (f64, f64)| -> (f64, f64) { anchors[g].unwrap_or(fallback) };

    // The session's slot order: the stored one (base optimisation already
    // done — user swaps live in it), or a freshly optimised base handed back
    // to the caller for storage. The optimisation sees each anchor under the
    // plain full-rect fit — the final view depends on the assignment itself,
    // and the approximation only steers WHICH slot a group gets.
    let (slot_order, store_slot_order) = match &box_ui.slot_order {
        Some(order) => (order.clone(), None),
        None => {
            let t_full = render::fit_transform(rect, min_xy, max_xy, base);
            let screen: Vec<egui::Pos2> = fans
                .iter()
                .map(|f| {
                    let a = layout_anchor(f.group_index, f.centroid);
                    t_full.apply(a.0, a.1)
                })
                .collect();
            let order = optimise_slot_order(rect, &screen);
            (order.clone(), Some(order))
        }
    };

    let seeds: Vec<SlotSeed> = slot_order
        .iter()
        .map(|&g| SlotSeed {
            group_index: g,
            color: render::structure_fan::group_color(g),
            anchor: layout_anchor(g, fans[g].centroid),
            has_target: anchors[g].is_some(),
            h: box_ui.heights[g],
        })
        .collect();
    let (windows, inner, hidden_groups) = place_windows_prescriptive(rect, &seeds);
    // Tiny-window edge case: the prescriptive bands are never shrunk or
    // demoted — just keep the fit finite and let the plan take what's left.
    let inner =
        egui::Rect::from_center_size(inner.center(), inner.size().max(egui::vec2(40.0, 40.0)));

    // Fit the plan into `inner`: zoom relative to the full-rect auto-fit,
    // offset to recentre on the gap the occupied bands leave.
    let base_scale = render::auto_fit_scale(rect, min_xy, max_xy);
    let inner_scale = render::auto_fit_scale(inner, min_xy, max_xy);
    let view = ViewParams {
        zoom: (inner_scale / base_scale) as f32,
        offset_x: inner.center().x - rect.center().x,
        offset_y: inner.center().y - rect.center().y,
        yaw_deg: base.yaw_deg,
        pitch_deg: base.pitch_deg,
    };

    Some(StructureEditLayout {
        view,
        windows,
        all_storeys_inner: None,
        hidden_groups,
        store_slot_order,
    })
}

/// The ALL-STOREYS in-place "Edit attributes" layout — the same margin
/// placement as the single-storey view, over the storey ROW. Each group is
/// seeded from the storey CELL holding its largest surface cluster
/// ([`structure_fan::group_anchor_all_storeys`] — a group can span storeys;
/// its recolour still shows in every cell, the connector points at the main
/// cluster). The plan can't be "zoomed into the gap" via `ViewParams` here —
/// the row's per-cell fit is rect-driven — so the layout instead carries the
/// inner RECT (`all_storeys_inner`) and the canvas renders + inverts the row
/// against it. `None` when there are no cells/groups or the canvas is too
/// small, leaving the caller on the normal view.
fn compute_structure_edit_layout_all_storeys(
    rect: egui::Rect,
    ber: &Ber,
    edit: &crate::controller::app_controller::StructureEdit,
    base: ViewParams,
    box_ui: &EditBoxUiState,
) -> Option<StructureEditLayout> {
    let cells = render::all_storeys_2d::cell_transforms(rect, ber, base);
    if cells.is_empty() || edit.groups.is_empty() {
        return None;
    }
    let mut seeds: Vec<MarginSeed> = Vec::with_capacity(edit.groups.len());
    let mut pinned: Vec<EditWindow> = Vec::new();
    for (i, g) in edit.groups.iter().enumerate() {
        let Some((s, anchor, has_target)) = render::structure_fan::group_anchor_all_storeys(ber, g)
        else {
            continue;
        };
        let Some(t) = cells.get(s) else { continue };
        let color = render::structure_fan::group_color(i);
        if let Some(w) = dragged_window(box_ui, i, anchor, has_target, Some(s), color) {
            pinned.push(w);
            continue;
        }
        seeds.push(MarginSeed {
            group_index: i,
            color,
            anchor,
            has_target,
            storey_idx: Some(s),
            screen: t.apply(anchor.0, anchor.1),
            h: box_ui.heights[i],
        });
    }
    if seeds.is_empty() && pinned.is_empty() {
        return None;
    }
    // The row spans the whole canvas, so its centre IS the canvas centre —
    // left-cell groups prefer the left margin, right-cell groups the right,
    // middle cells spill to top/bottom.
    let pinned_rects: Vec<egui::Rect> = pinned
        .iter()
        .map(|w| egui::Rect::from_min_size(w.pos, w.size))
        .collect();
    let (mut windows, inner) = place_windows_in_margins(
        rect,
        rect.center(),
        &seeds,
        box_ui.expand_first,
        &pinned_rects,
    )?;
    // Same as the single-storey layout: the storey row must dodge DRAGGED
    // boxes too, not just the reserved margins.
    let inner = plan_rect_avoiding_pinned(inner, &pinned_rects);
    windows.append(&mut pinned);
    Some(StructureEditLayout {
        view: base,
        windows,
        all_storeys_inner: Some(inner),
        hidden_groups: 0,
        store_slot_order: None,
    })
}

/// Shrink the margin-layout plan gap so it clears every PINNED (user-dragged)
/// box: the largest empty sub-rectangle of `inner` minus the pinned rects
/// ([`largest_empty_rect`]). Auto-placed boxes and chips already live outside
/// `inner` by construction; only dragged boxes can intrude. When a dragged
/// box leaves no usable gap (the user covered essentially the whole plan
/// area), the plan keeps the best rect there is — tiny beats vanished, and
/// the user's box is never demoted to make room.
fn plan_rect_avoiding_pinned(inner: egui::Rect, pinned: &[egui::Rect]) -> egui::Rect {
    let best = largest_empty_rect(inner, pinned);
    if best.width() >= 20.0 && best.height() >= 20.0 {
        best
    } else {
        inner
    }
}

/// Resolve a DROPPED box's rect against the already-pinned boxes: while it
/// strictly overlaps one, push it out along the cheapest of the four cardinal
/// directions (minimal displacement), preferring pushes that stay inside
/// `bounds`. The previously-pinned box always stays put — the newly dropped
/// one yields. A few iterations handle knock-on overlaps; a pathological pen
/// with no clear spot returns the best-effort position rather than looping.
fn nudge_clear_of(mut r: egui::Rect, obstacles: &[egui::Rect], bounds: egui::Rect) -> egui::Rect {
    for _ in 0..8 {
        let Some(o) = obstacles
            .iter()
            .find(|o| rects_strictly_overlap(r, **o))
            .copied()
        else {
            return r;
        };
        let pushes = [
            egui::vec2(o.min.x - r.max.x, 0.0), // out to the left
            egui::vec2(o.max.x - r.min.x, 0.0), // out to the right
            egui::vec2(0.0, o.min.y - r.max.y), // out above
            egui::vec2(0.0, o.max.y - r.min.y), // out below
        ];
        let by_len = |a: &egui::Vec2, b: &egui::Vec2| {
            a.length()
                .partial_cmp(&b.length())
                .unwrap_or(std::cmp::Ordering::Equal)
        };
        r = match pushes
            .iter()
            .filter(|d| bounds.contains_rect(r.translate(**d)))
            .min_by(|a, b| by_len(a, b))
        {
            Some(d) => r.translate(*d),
            // No in-bounds cardinal push-out: take the smallest anyway and
            // clamp back onto the canvas (may still touch the obstacle — the
            // canvas edge wins over a perfect separation).
            None => {
                let d = pushes
                    .iter()
                    .min_by(|a, b| by_len(a, b))
                    .copied()
                    .unwrap_or_default();
                let moved = r.translate(d);
                let dx = (bounds.left() - moved.left()).max(0.0)
                    - (moved.right() - bounds.right()).max(0.0);
                let dy = (bounds.top() - moved.top()).max(0.0)
                    - (moved.bottom() - bounds.bottom()).max(0.0);
                moved.translate(egui::vec2(dx, dy))
            }
        };
    }
    r
}

/// The largest-area axis-aligned rectangle inside `container` that STRICTLY
/// overlaps none of `obstacles` (edge touches allowed). Candidate search over
/// the coordinate grid the obstacle edges induce — O((4 + 2n)⁴) candidates
/// with n obstacles, and n here is the handful of dragged boxes, so brute
/// force is fine. `container` itself when no obstacle intrudes.
fn largest_empty_rect(container: egui::Rect, obstacles: &[egui::Rect]) -> egui::Rect {
    let obs: Vec<egui::Rect> = obstacles
        .iter()
        .filter(|o| rects_strictly_overlap(**o, container))
        .map(|o| o.intersect(container))
        .collect();
    if obs.is_empty() {
        return container;
    }
    let mut xs = vec![container.left(), container.right()];
    let mut ys = vec![container.top(), container.bottom()];
    for o in &obs {
        xs.extend([o.left(), o.right()]);
        ys.extend([o.top(), o.bottom()]);
    }
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup();
    ys.dedup();
    let mut best = egui::Rect::from_min_size(container.min, egui::Vec2::ZERO);
    for (i, &x0) in xs.iter().enumerate() {
        for &x1 in &xs[i + 1..] {
            for (k, &y0) in ys.iter().enumerate() {
                for &y1 in &ys[k + 1..] {
                    if (x1 - x0) * (y1 - y0) <= best.area() {
                        continue;
                    }
                    let cand = egui::Rect::from_min_max(egui::pos2(x0, y0), egui::pos2(x1, y1));
                    if obs.iter().all(|o| !rects_strictly_overlap(*o, cand)) {
                        best = cand;
                    }
                }
            }
        }
    }
    best
}

/// The floating UI of an open in-place "Edit attributes" session: a centred
/// takeover while the build-up layer editor or the photos view is up,
/// otherwise the per-group attribute windows — each joined to its recoloured
/// group by a connector line to `target_of(window)` (`None` skips the line,
/// e.g. a cell that no longer resolves). Shared by the single-storey and
/// all-storeys canvases so the boxes look and edit identically on both.
fn structure_edit_windows_ui<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    painter: &egui::Painter,
    view: &SketchView<'_>,
    edit: &crate::controller::app_controller::StructureEdit,
    layout: &StructureEditLayout,
    editable: bool,
    target_of: impl Fn(&EditWindow) -> Option<egui::Pos2>,
    on_event: &mut F,
) {
    if let Some(spec) = edit.specifying {
        // "Specify…" is open: a centred build-up LAYER editor takes over
        // (the small per-group windows are hidden while it's up).
        if let Some(group) = edit.groups.get(spec) {
            egui::Area::new(ui.id().with("specify_layers"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(300.0);
                        super::structure::layer_editor_ui(
                            ui,
                            spec,
                            group,
                            view.layer_library,
                            on_event,
                        );
                    });
                });
        }
    } else if let Some(pg) = edit.viewing_photos {
        // Camera button is open: a centred PHOTOS view (thumbnails + add).
        if let Some(group) = edit.groups.get(pg) {
            egui::Area::new(ui.id().with("view_photos"))
                .order(egui::Order::Foreground)
                .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(440.0);
                        super::structure::photos_view_ui(ui, pg, group, on_event);
                    });
                });
        }
    } else {
        let canvas_rect = painter.clip_rect();
        for w in &layout.windows {
            let i = w.group_index;
            let Some(group) = edit.groups.get(i) else {
                continue;
            };
            if w.chip {
                // Collapsed CHIP: colour + name only. Tapping it expands the
                // group to a full box next frame — the margin packing then
                // demotes whichever box falls past capacity, rather than
                // letting two boxes overlap.
                let area = egui::Area::new(ui.id().with(("structure_chip", i)))
                    .order(egui::Order::Foreground)
                    .fixed_pos(w.pos)
                    .show(ui.ctx(), |ui| {
                        egui::Frame::popup(ui.style())
                            .fill(w.color.gamma_multiply(0.35))
                            .stroke(egui::Stroke::new(1.5, w.color))
                            .show(ui, |ui| {
                                // Hold the reserved chip footprint (frame
                                // margins + stroke make up the difference).
                                ui.set_width(w.size.x - 16.0);
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(&group.name).small().strong(),
                                    )
                                    .truncate()
                                    .selectable(false),
                                );
                            });
                    });
                if area.response.clicked() {
                    ui.ctx().data_mut(|d| d.insert_temp(expand_store_id(ui), i));
                }
                continue;
            }
            // Full box, positioned by the LAYOUT (slot / margin / pin) plus
            // any live grip-handle drag offset. The box is carried by the
            // grip strip at its top ([`drag_handle_ui`]) — not by Area
            // movability, which the per-frame position force neutered and
            // touch rarely reached. While carried it renders offset from its
            // laid-out spot (one frame behind the finger); the layout itself
            // never moves until the DROP, so the plan can't jitter.
            let drag_state: Option<(usize, egui::Vec2)> =
                ui.ctx().data(|d| d.get_temp(box_drag_store_id(ui)));
            let my_drag = drag_state
                .filter(|(g, _)| *g == i)
                .map(|(_, off)| off)
                .unwrap_or(egui::Vec2::ZERO);
            let mut handle: Option<egui::Response> = None;
            let area = egui::Area::new(attr_area_id(ui, i))
                .order(egui::Order::Foreground)
                .fixed_pos(w.pos + my_drag)
                .constrain_to(canvas_rect)
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style())
                        .fill(w.color.gamma_multiply(0.22))
                        .stroke(egui::Stroke::new(2.0, w.color))
                        .show(ui, |ui| {
                            ui.set_max_width(EDIT_BOX_W - 16.0);
                            handle = Some(drag_handle_ui(ui, w.color));
                            // The two-level heading (kind + editable name)
                            // is rendered by `group_editor_ui` itself now.
                            super::structure::group_editor_ui(
                                ui,
                                group.ty.kind(),
                                i,
                                group,
                                view.phase_choices,
                                &view.structure_existing_groups,
                                editable,
                                edit.name_error
                                    .as_ref()
                                    .filter(|(gi, _)| *gi == i)
                                    .map(|(_, m)| m.as_str()),
                                on_event,
                            );
                        });
                });
            let handle = handle.expect("the area closure always runs");
            // Connector from the box's edge to the group's true anchor,
            // ending in a dot ON the recoloured surfaces. Drawn from the
            // box's ACTUAL rect (after `show`) so the line tracks a live
            // drag; the painter sits under the Foreground areas, so drawing
            // after the box still renders beneath it.
            if let Some(target) = target_of(w) {
                let rect = area.response.rect;
                if !rect.contains(target) {
                    painter.line_segment(
                        [rect_edge_toward(rect, target), target],
                        egui::Stroke::new(2.0, w.color),
                    );
                }
                painter.circle_filled(target, 3.5, w.color);
            }
            let single_storey = layout.all_storeys_inner.is_none();
            // The box's rect where the FINGER has it: the rendered rect plus
            // this frame's not-yet-applied handle delta.
            let carried_rect = area.response.rect.translate(handle.drag_delta());
            // Grip-drag lifecycle. The offset lives in `box_drag_store_id`;
            // updating it AFTER the area was shown gives the one-frame lag.
            if handle.drag_started() {
                ui.ctx()
                    .data_mut(|d| d.insert_temp(box_drag_store_id(ui), (i, egui::Vec2::ZERO)));
                // The carried box rides above everything.
                ui.ctx().move_to_top(area.response.layer_id);
            }
            if handle.dragged() {
                let delta = handle.drag_delta();
                ui.ctx().data_mut(|d| {
                    let state: &mut (usize, egui::Vec2) = d
                        .get_temp_mut_or_insert_with(box_drag_store_id(ui), || {
                            (i, egui::Vec2::ZERO)
                        });
                    if state.0 == i {
                        state.1 += delta;
                    }
                });
                ui.ctx().request_repaint();
                // SINGLE-STOREY, mid-drag: ring the box the drop would SWAP
                // with, so the target is visible before release. Painted on
                // the canvas layer just OUTSIDE the target's frame — the
                // ring stays visible around the Foreground box it marks.
                if single_storey {
                    if let Some(t) = swap_target(&layout.windows, i, carried_rect) {
                        if let Some(tw) = layout.windows.iter().find(|w| w.group_index == t) {
                            painter.rect_stroke(
                                egui::Rect::from_min_size(tw.pos, tw.size).expand(3.0),
                                4.0,
                                egui::Stroke::new(2.5, egui::Color32::WHITE),
                                egui::StrokeKind::Outside,
                            );
                        }
                    }
                }
            } else if drag_state.is_some_and(|(g, _)| g == i) && !handle.drag_stopped() {
                // Stale carry state (the drag ended out of sight — e.g. a
                // cancelled touch): drop it, the box snaps home.
                ui.ctx()
                    .data_mut(|d| d.remove::<(usize, egui::Vec2)>(box_drag_store_id(ui)));
            }
            // Drop handling, on RELEASE only.
            //
            // SINGLE-STOREY (prescriptive slots): dragging means SWAP — a
            // drop on another box transposes the two groups' slots in the
            // session order ([`slot_order_store_id`]); a drop on empty space
            // stores nothing, so the box snaps back to its own slot next
            // frame. Boxes never rest off the grid.
            //
            // ALL-STOREYS (margin packing): free pinning as before — the drop
            // takes the minimal cardinal nudge clear of every other box (they
            // stay put), the position persists, and the packing + plan-fit
            // re-run around it next frame.
            if handle.drag_stopped() {
                ui.ctx()
                    .data_mut(|d| d.remove::<(usize, egui::Vec2)>(box_drag_store_id(ui)));
                if single_storey {
                    if let Some(target) = swap_target(&layout.windows, i, carried_rect) {
                        // Transpose the order THE USER IS LOOKING AT — the
                        // displayed windows are in slot sequence — rather
                        // than whatever the store holds: if store and
                        // display ever diverged, writing a transposed store
                        // would teleport bystander boxes on the drop.
                        // Groups are identified by their session INDEX —
                        // never by name (duplicates exist in real data).
                        let mut order: Vec<usize> =
                            layout.windows.iter().map(|w| w.group_index).collect();
                        // Groups past the slot cap aren't displayed; keep
                        // them as a stable ascending tail so the stored
                        // order stays a full permutation.
                        for g in 0..edit.groups.len() {
                            if !order.contains(&g) {
                                order.push(g);
                            }
                        }
                        transpose_slot_order(&mut order, i, target);
                        let store = slot_order_store_id(ui);
                        ui.ctx().data_mut(|d| d.insert_temp(store, order));
                    }
                } else {
                    let store = drag_store_id(ui);
                    let others: Vec<egui::Rect> = layout
                        .windows
                        .iter()
                        .filter(|o| o.group_index != i)
                        .map(|o| egui::Rect::from_min_size(o.pos, o.size))
                        .collect();
                    let dropped = nudge_clear_of(carried_rect, &others, canvas_rect);
                    ui.ctx().data_mut(|d| {
                        let map: &mut DraggedBoxes = d.get_temp_mut_or_default(store);
                        map.insert(i, dropped.min);
                    });
                }
                ui.ctx().request_repaint();
            }
            // Tap-to-raise: a press anywhere on a box brings it above its
            // peers (a dragged box dropped over another stays reachable).
            if area.response.contains_pointer() && ui.ctx().input(|inp| inp.pointer.any_pressed()) {
                ui.ctx().move_to_top(area.response.layer_id);
            }
        }
        // Prescriptive single-storey layout with more groups than slots:
        // say so, unobtrusively (the extra groups are not displayed).
        if layout.hidden_groups > 0 {
            painter.text(
                egui::pos2(canvas_rect.left() + 8.0, canvas_rect.bottom() - 6.0),
                egui::Align2::LEFT_BOTTOM,
                format!("not all groups shown — {} more", layout.hidden_groups),
                egui::FontId::proportional(12.0),
                egui::Color32::from_gray(150),
            );
        }
    }
}

/// Where the connector line leaves a box: the point on `rect`'s boundary
/// along the ray from its centre toward `target` (the rect's centre when the
/// direction degenerates).
fn rect_edge_toward(rect: egui::Rect, target: egui::Pos2) -> egui::Pos2 {
    let c = rect.center();
    let (dx, dy) = (target.x - c.x, target.y - c.y);
    let mut t = f32::INFINITY;
    if dx.abs() > 1e-3 {
        t = t.min(rect.width() * 0.5 / dx.abs());
    }
    if dy.abs() > 1e-3 {
        t = t.min(rect.height() * 0.5 / dy.abs());
    }
    if !t.is_finite() {
        return c;
    }
    egui::pos2(c.x + dx * t, c.y + dy * t)
}

pub(super) fn render<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    view: &SketchView<'_>,
    touch: &mut TouchNav,
    on_event: &mut F,
) {
    let reviewing = view.review.is_some();
    let dim_prompting = view.dim_prompt.is_some();
    let ber = view.ber;
    let current_storey_idx = view.current_storey_idx;
    let mode = view.mode;
    let display = view.display;
    let edit_tool = view.edit_tool;
    let vp = view.view_params;
    // In the Objects phase the plan is kept minimal (a sense of place) — the
    // ceiling-step and roof-apex/ridge overlays are suppressed.
    let objects_phase = matches!(view.current_phase, AssessmentPhase::Objects);
    // Phase-4 north-align step: the satellite image is being moved/rotated over
    // the fixed plan, so the canvas owns input (no nav/draw).
    let final_north = view.north_image.is_some();

    // Wall-length labels gated on the Dimensions-category toggle: passing
    // empty label lists leaves the geometry untouched and just drops the
    // text.
    let dimensioned_walls: &[(usize, usize)] = if view.cat_state.show_wall_lengths {
        view.dimensioned_walls
    } else {
        &[]
    };
    let living_area_wall_labels: &[Vec<((f64, f64), (f64, f64), f32)>] =
        if view.cat_state.show_wall_lengths {
            view.living_area_wall_labels
        } else {
            &[]
        };

    // `click_and_drag` so middle/secondary drags register too
    // (plain `drag` is biased toward primary-button drag).
    let (rect, response) =
        ui.allocate_exact_size(ui.available_size(), egui::Sense::click_and_drag());

    // Stage-1 (Mapping) REVIEW: when open it REPLACES the plan canvas — the
    // current side's external elevation beside its photo, with draggable
    // openings and the scroll / confirm controls. Owns its own input.
    if let Some(rv) = view.mapping_review_view.as_ref() {
        mapping_review_overlay(ui, rect, &response, rv, vp, on_event);
        return;
    }

    // Stage-2 (Structure) REVIEW: when open it REPLACES the plan canvas — the
    // current group's details + photos beside the orbitable 3D house with that
    // group's surfaces shaded. Owns its own input (orbit on the 3D side).
    if let Some(srv) = view.structure_review_view.as_ref() {
        if let Some(ber) = ber {
            structure_review_overlay(ui, rect, &response, srv, ber, vp, on_event);
        }
        return;
    }

    // Phase-3 (Objects) REVIEW: house-wide object counts beside the 3D house,
    // with the selected kind's locations marked.
    if let Some(orv) = view.object_review_view.as_ref() {
        if let Some(ber) = ber {
            object_review_overlay(ui, rect, &response, orv, ber, vp, on_event);
        }
        return;
    }

    // Phase-4 (Final): a full phase view — the details form + mode controls on one
    // side, the reduced plan (over the satellite image) on the other.
    if matches!(view.current_phase, AssessmentPhase::Final) {
        if let Some(ber) = ber {
            final_overlay(ui, rect, view, ber, vp, on_event);
        }
        // Keep the phase signpost + checklist on top (the "Mark BER complete"
        // action lives in the Final checklist) — the takeover reserves room for it.
        phase_signpost(ui, rect, view, on_event);
        return;
    }

    // Elevation editor: when a wall's elevation is open it REPLACES the plan
    // canvas — the unrolled wall with draggable openings. Owns its own input.
    if let Some(elev) = view.elevation_view.as_ref() {
        elevation_overlay(ui, rect, &response, elev, vp, on_event);
        return;
    }

    // Wall-face PARTITION editor: like the elevation, when a face is open for
    // partitioning it REPLACES the plan canvas (flat-top unrolled face + cuts +
    // tinted regions + a you-are-here plan inset). Owns its own input.
    if let Some(pv) = view.partition_view {
        partition_overlay(ui, rect, &response, pv, vp, on_event);
        return;
    }

    // Structure "Edit attributes" (in-place view): lay the per-group windows out
    // in the margins (each on the side nearest its group), then fit the plan into
    // the gap they leave.
    // Single storey: the fitted view REPLACES the normal pan/zoom for the whole
    // render so the plan, its overlays, and the windows all share one transform.
    // All storeys: the view stays put and the storey ROW shrinks into the inner
    // rect instead (`all_storeys_inner` → `plan_area` below). `None` (no
    // session, the exploded view, 3D, or a canvas too small) leaves `vp` as-is.
    let structure_edit_layout = view
        .structure_edit
        .filter(|e| !e.exploded && matches!(mode, SketchMode::Storey2D))
        .and_then(|edit| {
            // Frame inputs from egui memory: measured box sizes, user-dragged
            // positions, the expanded-chip choice.
            let box_ui = edit_box_ui_state(ui, edit);
            match current_storey_idx {
                Some(i) => {
                    let storey = ber.and_then(|b| b.storeys.get(i))?;
                    compute_structure_edit_layout(
                        rect,
                        ber,
                        current_storey_idx,
                        storey,
                        edit,
                        vp,
                        &box_ui,
                    )
                }
                None => compute_structure_edit_layout_all_storeys(rect, ber?, edit, vp, &box_ui),
            }
        });
    // A freshly OPTIMISED single-storey slot order (session open / group-set
    // change) is persisted NOW, before the windows render — the drag-swap
    // handler transposes this same store.
    if let Some(order) = structure_edit_layout
        .as_ref()
        .and_then(|l| l.store_slot_order.clone())
    {
        let store = slot_order_store_id(ui);
        ui.ctx().data_mut(|d| d.insert_temp(store, order));
    }
    if view.structure_edit.is_none() {
        // No edit session: forget the session-scoped box state (dragged
        // positions, the slot order + its swaps, the expanded-chip choice)
        // so the next session starts on a freshly optimised layout.
        ui.ctx().data_mut(|d| {
            d.remove::<DraggedBoxes>(drag_store_id(ui));
            d.remove::<Vec<usize>>(slot_order_store_id(ui));
            d.remove::<usize>(expand_store_id(ui));
            d.remove::<(usize, egui::Vec2)>(box_drag_store_id(ui));
        });
    }
    let vp = structure_edit_layout.as_ref().map(|l| l.view).unwrap_or(vp);
    // The rect the PLAN occupies this frame: the whole canvas, except while the
    // all-storeys "Edit attributes" layout reserves its margins — then the
    // storey row (render AND every per-cell transform, including input
    // inversion) lives in the inner rect so plan and windows never overlap.
    let plan_area = structure_edit_layout
        .as_ref()
        .and_then(|l| l.all_storeys_inner)
        .unwrap_or(rect);

    // Push the diagonal (metres) of the currently-VISIBLE world rectangle to the
    // controller each frame, so its snap tolerances scale with zoom. Computed by
    // inverting the two canvas corners through the SAME transform the storey is
    // drawn with; only this scalar crosses into the controller. `None` (no
    // session / storey) leaves the controller on its plan-bbox fallback.
    if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, true) {
        let (x0, y0) = t.inverse(rect.left(), rect.top());
        let (x1, y1) = t.inverse(rect.right(), rect.bottom());
        let diag = (((x1 - x0).powi(2) + (y1 - y0).powi(2)).sqrt()) as f32;
        if diag.is_finite() && diag > 0.0 {
            on_event(UiEvent::Sketch(SketchEvent::ViewportWorldDiagonal {
                world_diagonal_m: diag,
            }));
        }
    }

    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(30));

    // Floating "Add external pictures" button + panel, anchored BOTTOM-RIGHT
    // (mirror of `view_buttons`, pivoted to the bottom-right corner). Shown in
    // the Mapping phase once a record is loaded.
    if matches!(view.current_phase, AssessmentPhase::Mapping) && ber.is_some() {
        external_photos_button(ui, rect, view, on_event);
    }

    // Phase signpost (top-centre), clickable to switch phase, with the live
    // per-phase completion checklist folded in behind a triangle to its right
    // (collapsed by default). Structure unlocks once Mapping is complete (every
    // checklist item green on all storeys); Objects isn't built yet. The expanded
    // checklist shows a "Review ▸" button in the Mapping phase once every item is
    // green; clicking it opens the review. Drawn BEFORE the view buttons so they
    // can be positioned to never overlap the (centred, variable-width) signpost.
    // The "Review ▸" entry + its ✓ are per-phase: Mapping uses its checklist
    // completeness / reviewed flag; Structure uses the whole-house attribution
    // completeness / its own reviewed flag.
    let phase_bar_rect = phase_signpost(ui, rect, view, on_event);

    // View buttons (2D/3D toggle) in the top-right — but dropped BELOW the phase
    // signpost whenever the centred bar would reach their column, so they never
    // overlap at any screen width.
    view_buttons(ui, rect, view, on_event, phase_bar_rect);

    // Structure phase: when a surface kind is selected for highlighting, name it
    // (+ its count) in a banner under the signpost so it's explicit which
    // surfaces the shaded faces on the canvas are.
    super::overlays::highlight_caption(ui, rect, view.display, view.surface_counts);

    // Floor-exposure tool (post-gate): the OVERLAID all-storeys floor view
    // REPLACES the normal single-storey plan (independent of the active tab —
    // it shows every storey). While the gate is up, fall through to the normal
    // plan so the assessor still sees their work behind the gate prompt.
    let floor_exposure_active = view.floor_exposure_mode && !view.floor_exposure_gate_pending;
    let ceiling_exposure_active = view.ceiling_exposure_mode && !view.ceiling_exposure_gate_pending;
    // Roof step (post-gate): the whole-house outline shaded by which storey is
    // the top one at each location. Footprints are drawn bottom→top in the same
    // footprint-union transform the exposure overlays use, so the higher (later)
    // storeys cover the lower ones — what shows at a location IS its top storey
    // (= the roof there). While the gate is up, fall through to the normal plan.
    let roof_active = view.roof_mode && !view.roof_gate_pending;
    // The all-storeys overlay modes (roof / floor-exposure / ceiling-exposure)
    // REPLACE the single-storey plan, so its plan-detail overlays must not leak
    // on top — they render on the ACTIVE-storey transform and would sit
    // misaligned over the union-fit overlay. Exception: a floor/ceiling carve
    // dimensions ITSELF through the dim/position prompts, so those stay live
    // while a carve ring is in flight.
    // Same derivation the number-box gate in `mod.rs` uses (via `SketchView`), so
    // the canvas and the separately-rendered number box suppress in lockstep.
    let overlay_mode = view.overlay_active();
    let carve_dimensioning = view.carve_dimensioning();
    if roof_active {
        if let Some(t) = floor_exposure_transform(ber, rect, vp) {
            // Faint whole-house footprints first: the top-storey-shading CONTEXT
            // (drawn bottom→top so the top storey shows = the roof there).
            for region in view.roof_context {
                if region.ring.len() < 3 {
                    continue;
                }
                let pts: Vec<egui::Pos2> =
                    region.ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
                let (r, g, b) = roof_storey_tint(region.storey_idx);
                let fill = egui::Color32::from_rgba_unmultiplied(r, g, b, 45);
                let stroke = egui::Stroke::new(1.0, egui::Color32::from_rgb(r, g, b));
                painter.add(egui::Shape::convex_polygon(pts, fill, stroke));
            }
            // Selectable ceiling regions on top: tap to group into a roof area.
            // Selected regions read bright; the rest show their storey tint.
            for region in view.roof_regions {
                if region.ring.len() < 3 {
                    continue;
                }
                let pts: Vec<egui::Pos2> =
                    region.ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
                let (fill, stroke) = if region.selected {
                    (
                        egui::Color32::from_rgba_unmultiplied(120, 220, 140, 150),
                        egui::Stroke::new(3.0, egui::Color32::from_rgb(60, 180, 90)),
                    )
                } else {
                    let (r, g, b) = roof_storey_tint(region.storey_idx);
                    (
                        egui::Color32::from_rgba_unmultiplied(r, g, b, 90),
                        egui::Stroke::new(1.5, egui::Color32::from_rgb(r, g, b)),
                    )
                };
                painter.add(egui::Shape::convex_polygon(pts, fill, stroke));
            }
        }
    } else if floor_exposure_active {
        if let Some(ber) = ber {
            render::all_storeys_floors::render(
                &painter,
                rect,
                ber,
                vp,
                view.floor_exposure_storeys,
            );
            // In-flight CARVE ring (being dimensioned): drawn in the SAME
            // union-fit transform the overlaid floors use, so the region the
            // assessor is measuring lines up with the floors beneath it. The
            // floating number box (rendered unconditionally below the canvas)
            // carries the actual dimension question + current length.
            if view.floor_exposure_carve_polygon.len() >= 3 {
                if let Some(t) = floor_exposure_transform(Some(ber), rect, vp) {
                    let pts: Vec<egui::Pos2> = view
                        .floor_exposure_carve_polygon
                        .iter()
                        .map(|(x, y)| t.apply(*x, *y))
                        .collect();
                    let fill = egui::Color32::from_rgba_unmultiplied(120, 200, 255, 70);
                    let stroke = egui::Stroke::new(2.5, egui::Color32::from_rgb(120, 200, 255));
                    painter.add(egui::Shape::convex_polygon(pts, fill, stroke));
                }
            }
            // Pending straight-cut candidates: shade the two halves so the
            // assessor taps the side they want (routed to ChooseFloorExposureCut).
            shade_exposure_cut_candidates(&painter, rect, ber, vp, view.floor_exposure_cut_regions);
        }
    } else if ceiling_exposure_active {
        // The OVERLAID all-storeys CEILING view REPLACES the normal plan (mirror
        // of the floor branch above). Both fit to the same footprint-union
        // transform, so the in-flight carve lines up with the ceilings beneath it.
        if let Some(ber) = ber {
            render::all_storeys_ceilings::render(
                &painter,
                rect,
                ber,
                vp,
                view.ceiling_exposure_storeys,
            );
            if view.ceiling_exposure_carve_polygon.len() >= 3 {
                if let Some(t) = floor_exposure_transform(Some(ber), rect, vp) {
                    let pts: Vec<egui::Pos2> = view
                        .ceiling_exposure_carve_polygon
                        .iter()
                        .map(|(x, y)| t.apply(*x, *y))
                        .collect();
                    let fill = egui::Color32::from_rgba_unmultiplied(120, 200, 255, 70);
                    let stroke = egui::Stroke::new(2.5, egui::Color32::from_rgb(120, 200, 255));
                    painter.add(egui::Shape::convex_polygon(pts, fill, stroke));
                }
            }
            // Pending straight-cut candidates (mirror of the floor branch).
            shade_exposure_cut_candidates(
                &painter,
                rect,
                ber,
                vp,
                view.ceiling_exposure_cut_regions,
            );
        }
    } else {
        // BER view layer with optional collinearity highlight overlay. In the
        // Objects phase the plan is reduced (no ceilings / roof windows) and the
        // room pins show as hotspots in 3D — lifted ~1 m above the storey floor.
        let objects_view = matches!(view.current_phase, AssessmentPhase::Objects);
        let hotspots: Vec<(f64, f64, f64)> = if objects_view {
            if current_storey_idx.is_some() {
                // Single-storey 3D: storey-local points (surface height where
                // the pin was placed on a surface, the legacy 1 m otherwise).
                view.object_pins
                    .iter()
                    .map(|p| (p.x, p.y, p.z_m.unwrap_or(1.0)))
                    .collect()
            } else {
                // Full-house 3D (the All tab): every storey's pins in world z.
                view.object_pins_world.clone()
            }
        } else {
            Vec::new()
        };
        render::render_ber(
            &painter,
            // `plan_area` ≠ `rect` only under the all-storeys "Edit
            // attributes" layout — the storey row then fits the gap between
            // the reserved window margins.
            plan_area,
            ber,
            current_storey_idx,
            mode,
            display,
            view.review,
            dimensioned_walls,
            vp,
            view.structure_selected,
            view.structure_edit,
            objects_view,
            &hotspots,
        );
    }

    // Phase-4 north-align: the satellite image, moved / zoomed / rotated over the
    // FIXED plan. The image's rotation is the recorded north. Two-finger gestures
    // (Android) or left-drag = move, right-drag = rotate, scroll = zoom (PC).
    if let Some(img) = view.north_image.as_ref() {
        let base = (rect.width().min(rect.height()) * 0.6 * img.zoom).max(20.0);
        let center = rect.center() + egui::vec2(img.offset.0, img.offset.1);
        let img_rect = egui::Rect::from_center_size(center, egui::vec2(base, base));
        egui::Image::new(format!("file://{}", img.path))
            .rotate(img.rotation_deg.to_radians(), egui::vec2(0.5, 0.5))
            .tint(egui::Color32::from_white_alpha(170))
            .show_loading_spinner(false)
            .paint_at(ui, img_rect);

        // One gesture per frame → NorthImageGesture.
        let mt = ui.input(|i| i.multi_touch());
        if let Some(mt) = mt {
            on_event(UiEvent::Sketch(SketchEvent::NorthImageGesture {
                pan_dx: mt.translation_delta.x,
                pan_dy: mt.translation_delta.y,
                zoom_factor: mt.zoom_delta,
                rotate_deg: mt.rotation_delta.to_degrees(),
            }));
        } else {
            let mut pan = egui::vec2(0.0, 0.0);
            let mut rotate_deg = 0.0;
            let mut zoom_factor = 1.0;
            if response.dragged_by(egui::PointerButton::Primary) {
                pan = response.drag_delta();
            }
            if response.dragged_by(egui::PointerButton::Secondary) {
                rotate_deg = response.drag_delta().x * 0.4;
            }
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll.abs() > 0.0 {
                zoom_factor = (scroll * 0.002).exp();
            }
            if pan != egui::vec2(0.0, 0.0) || rotate_deg != 0.0 || zoom_factor != 1.0 {
                on_event(UiEvent::Sketch(SketchEvent::NorthImageGesture {
                    pan_dx: pan.x,
                    pan_dy: pan.y,
                    zoom_factor,
                    rotate_deg,
                }));
            }
        }
        // A north indicator: an arrow pointing where the image's north now is.
        let north_dir = egui::Vec2::angled((img.rotation_deg - 90.0).to_radians());
        let origin = rect.right_top() + egui::vec2(-40.0, 40.0);
        painter.arrow(
            origin,
            north_dir * 26.0,
            egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 80, 80)),
        );
        painter.text(
            origin + north_dir * 34.0,
            egui::Align2::CENTER_CENTER,
            "N",
            egui::FontId::proportional(13.0),
            egui::Color32::from_rgb(255, 80, 80),
        );
    }

    // ── Single-storey plan chrome (overlay-mode suppression) ──────────────────
    // Everything in this block renders on the ACTIVE-storey transform, so under
    // an all-storeys overlay mode (roof / floor-exposure / ceiling-exposure) —
    // which REPLACES the single-storey plan and fits to the footprint union — it
    // would leak on top, mis-scaled. Gate the whole run off. The carve-exception
    // prompt blocks further down (dim / position) keep their own
    // `!overlay_mode || carve_dimensioning` guard so a floor/ceiling carve can
    // still dimension itself while a ring is in flight.
    if !overlay_mode {
        // Structure "Edit attributes" — EXPLODED view only: place a small group-picker
        // popup at each group's pulled-out position (clicking makes that group active;
        // the side panel edits it). The DEFAULT in-place windows are drawn later, on
        // top of the wall-exposure layer, by `structure_edit_overlay`.
        if let (Some(edit), Some(storey)) = (
            view.structure_edit,
            ber.and_then(|b| current_storey_idx.and_then(|i| b.storeys.get(i))),
        ) {
            if edit.exploded && matches!(mode, SketchMode::Storey2D) {
                if let Some(t) = render::structure_fan::fan_transform(rect, storey, vp) {
                    for fan in render::structure_fan::group_fan_layout(storey, edit) {
                        let (wx, wy) = fan.fanned_centroid();
                        let at = t.apply(wx, wy);
                        let active = fan.group_index == edit.active;
                        egui::Area::new(ui.id().with(("structure_fan", fan.group_index)))
                            .order(egui::Order::Foreground)
                            .fixed_pos(at)
                            .show(ui.ctx(), |ui| {
                                egui::Frame::popup(ui.style())
                                    .fill(if active {
                                        fan.color.gamma_multiply(0.5)
                                    } else {
                                        ui.style().visuals.window_fill
                                    })
                                    .show(ui, |ui| {
                                        let label = format!(
                                            "Group {}  ·  {}",
                                            fan.group_index + 1,
                                            fan.count
                                        );
                                        if ui.add(egui::Button::selectable(active, label)).clicked()
                                        {
                                            on_event(UiEvent::Sketch(
                                                SketchEvent::SetActiveStructureGroup(
                                                    fan.group_index,
                                                ),
                                            ));
                                        }
                                    });
                            });
                    }
                }
            }
        }

        // New-storey browse stage: tint the current outline suggestion's ring
        // translucently over the previous-storey watermark so the assessor sees
        // the candidate footprint in place while cycling ◀ ▶ in the dock.
        if let Some((ring, _label, _idx, _count)) = view
            .new_storey_prompt
            .as_ref()
            .and_then(|p| p.suggestion.as_ref())
        {
            render::render_suggestion_ring(&painter, rect, ber, current_storey_idx, mode, vp, ring);
        }

        // Living-area sub-shapes overlaid in green (active brighter),
        // with length labels on their known walls (like the storey).
        render::render_living_areas(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            display,
            vp,
            view.living_area_polygons,
            living_area_wall_labels,
            view.active_living_area,
        );

        // Pending cut: show both candidate regions to tap between.
        render::render_cut_candidates(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.pending_cut_regions,
        );
        // Structure-phase carve: the same two-candidate highlight while picking
        // which region's surfaces to select.
        render::render_cut_candidates(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.structure_cut_regions,
        );
        // Structure split: the in-flight ring, highlighted while it's being
        // dimensioned (same treatment as the construction-phase pending area).
        if view.pending_structure_split_polygon.len() >= 3 {
            render::render_phase_regions(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                std::slice::from_ref(&PhaseDisplayRegion::from_ring(
                    view.pending_structure_split_polygon.to_vec(),
                    "?".to_string(),
                )),
                egui::Color32::from_rgb(120, 200, 255), // blue: structure split
            );
        }
        // Staged carve rings: the pending SELECTION (not yet split). A gold dashed
        // marquee INSET just inside the carved area, so the selected region reads
        // clearly even where its edges run along the plan perimeter (outline only,
        // no fill). The geometry only splits at "Edit attributes".
        render::render_selection_area(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.pending_structure_carves,
        );
        // Wall-inclusion ask: halo the carve edge currently being asked about, so
        // the panel's Include / Exclude visibly refers to THAT wall. Only when the
        // asked edge is on THIS storey — a cross-storey lasso ask (tagged with its
        // storey) draws in the all-storeys overlay below instead, and its
        // coordinates would be wrong in another storey's frame.
        if let Some(wi) = &view.wall_inclusion_prompt {
            if wi.storey_idx.is_none() || wi.storey_idx == current_storey_idx {
                render::render_wall_inclusion_edge(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    vp,
                    wi.edge,
                );
            }
        }

        // ALL-STOREYS Structure overlays: the staged lasso rings + the ask edge,
        // each drawn into its own storey's cell with that cell's transform.
        if current_storey_idx.is_none()
            && matches!(mode, SketchMode::Storey2D)
            && view.current_phase == AssessmentPhase::Structure
        {
            if let Some(b) = ber {
                let cells = render::all_storeys_2d::cell_transforms(plan_area, b, vp);
                let gold = egui::Color32::from_rgb(235, 195, 80);
                for (i, ring) in view.pending_structure_carves_by_storey {
                    let (Some(t), true) = (cells.get(*i), ring.len() >= 3) else {
                        continue;
                    };
                    let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
                    let n = pts.len();
                    for k in 0..n {
                        painter
                            .line_segment([pts[k], pts[(k + 1) % n]], egui::Stroke::new(2.5, gold));
                    }
                }
                if let Some(wi) = &view.wall_inclusion_prompt {
                    if let Some(t) = wi.storey_idx.and_then(|i| cells.get(i)) {
                        let a = t.apply(wi.edge.0 .0, wi.edge.0 .1);
                        let b2 = t.apply(wi.edge.1 .0, wi.edge.1 .1);
                        // Same halo treatment as the single-storey ask edge: a
                        // thick amber stroke so Include/Exclude visibly refers to
                        // THAT wall in THAT cell.
                        painter.line_segment(
                            [a, b2],
                            egui::Stroke::new(5.0, egui::Color32::from_rgb(255, 170, 40)),
                        );
                    }
                }
            }
        }

        // DEBUG: splice-pipeline phases of the last extend/trim.
        render::render_splice_debug(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.splice_debug,
        );

        // Elective-snap tags of the last action (orange markers + kind).
        render::render_snap_tags(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.snap_tags,
        );

        // Left-click menu per snap tag: pick a different candidate, or
        // "Don't snap", to replay the action with just that snap changed.
        if !view.snap_tags.is_empty() {
            if let Some(t) =
                render::active_storey_transform(rect, ber, current_storey_idx, mode, vp)
            {
                for (ti, tag) in view.snap_tags.iter().enumerate() {
                    let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
                    let label = match tag.current_kind {
                        Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                        None => "no snap ▾".to_string(),
                    };
                    egui::Area::new(ui.id().with(("snap_tag", ti)))
                        .fixed_pos(sp + egui::vec2(7.0, -7.0))
                        .order(egui::Order::Foreground)
                        .show(ui.ctx(), |ui| {
                            ui.menu_button(label, |ui| {
                                // What this snap DID (point moved vs wall pinned),
                                // so the choice below is informed.
                                if let Some(k) = tag
                                    .current_kind
                                    .or_else(|| tag.options.first().map(|(k, _)| *k))
                                {
                                    ui.label(
                                        egui::RichText::new(render::snap_kind_hint(k))
                                            .small()
                                            .weak(),
                                    );
                                    ui.separator();
                                }
                                for (kind, idx) in &tag.options {
                                    if ui.button(render::snap_kind_label(*kind)).clicked() {
                                        on_event(UiEvent::Sketch(SketchEvent::ChooseSnap {
                                            from: tag.from,
                                            choice: Some(*idx),
                                        }));
                                        ui.close();
                                    }
                                }
                                if tag.skippable && ui.button("Don't snap").clicked() {
                                    on_event(UiEvent::Sketch(SketchEvent::ChooseSnap {
                                        from: tag.from,
                                        choice: None,
                                    }));
                                    ui.close();
                                }
                            });
                        });
                }
            }
        }

        // Construction-phase areas: committed regions (purple, labelled),
        // plus the pending carved area / cut candidates while picking.
        let phase_tint = egui::Color32::from_rgb(200, 120, 255); // purple
        render::render_phase_regions(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.phase_region_polygons,
            phase_tint,
        );
        render::render_cut_candidates(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.pending_phase_cut_regions,
        );
        if view.pending_phase_polygon.len() >= 3 {
            render::render_phase_regions(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                std::slice::from_ref(&PhaseDisplayRegion::from_ring(
                    view.pending_phase_polygon.to_vec(),
                    "?".to_string(),
                )),
                phase_tint,
            );
        }

        // Floor-step areas: committed regions (teal, labelled with the
        // signed level), plus the pending carved area / cut candidates. Steps
        // never carry a hole, so each ring becomes a simple display region.
        let step_tint = egui::Color32::from_rgb(60, 220, 200); // teal
        let step_display: Vec<PhaseDisplayRegion> = view
            .step_region_polygons
            .iter()
            .map(|(ring, label)| PhaseDisplayRegion::from_ring(ring.clone(), label.clone()))
            .collect();
        render::render_phase_regions(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            &step_display,
            step_tint,
        );
        render::render_cut_candidates(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.pending_step_cut_regions,
        );
        if view.pending_step_polygon.len() >= 3 {
            render::render_phase_regions(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                std::slice::from_ref(&PhaseDisplayRegion::from_ring(
                    view.pending_step_polygon.to_vec(),
                    "?".to_string(),
                )),
                step_tint,
            );
        }

        // Ceiling-step areas: committed regions (amber, labelled with
        // the signed delta — hue distinct from the floor steps' teal),
        // the two side half-rects while the side tap is pending, and
        // the dashed provisional line.
        let ceiling_step_tint = egui::Color32::from_rgb(255, 170, 60); // amber
        if !objects_phase {
            let ceiling_step_display: Vec<PhaseDisplayRegion> = view
                .ceiling_step_region_polygons
                .iter()
                .map(|(ring, label)| PhaseDisplayRegion::from_ring(ring.clone(), label.clone()))
                .collect();
            render::render_phase_regions(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                &ceiling_step_display,
                ceiling_step_tint,
            );
            // Same-height dividers: a faint dashed pass, distinct from the amber
            // height steps (divider regions are filtered out of the amber feed above).
            render::render_ceiling_dividers(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                view.ceiling_divider_lines,
            );
        }
        render::render_cut_candidates(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.pending_ceiling_step_side_rects,
        );
        if let Some((a, b)) = view.pending_ceiling_step_line {
            render::render_ceiling_step_line(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                a,
                b,
            );
        }

        // The pending line's wall snap, drawn through the SAME snap-tag
        // renderer as the stroke snaps — but its menu is a separate
        // widget routed to UnsnapCeilingStepLine, never ChooseSnap:
        // the generic replay rewinds history, and the pending line
        // lives outside it. With no alternative candidates the menu
        // offers exactly "Don't snap".
        if let Some(tag) = view.ceiling_step_snap_tag {
            render::render_snap_tags(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                std::slice::from_ref(tag),
            );
            if let Some(t) =
                render::active_storey_transform(rect, ber, current_storey_idx, mode, vp)
            {
                let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
                let label = match tag.current_kind {
                    Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                    None => "no snap ▾".to_string(),
                };
                egui::Area::new(ui.id().with("ceiling_step_snap_tag"))
                    .fixed_pos(sp + egui::vec2(7.0, -7.0))
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        ui.menu_button(label, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "line pinned onto the parallel wall — its position \
                                 isn't asked; un-snap reopens it",
                                )
                                .small()
                                .weak(),
                            );
                            ui.separator();
                            if tag.skippable && ui.button("Don't snap").clicked() {
                                on_event(UiEvent::Sketch(SketchEvent::UnsnapCeilingStepLine));
                                ui.close();
                            }
                        });
                    });
            }
        }

        // Roof apexes: committed high points ("▲ 4.20 m" markers) plus the
        // pending tapped point as a hollow dot, in a distinct rose-magenta.
        // Suppressed in the Objects phase (ceiling points aren't relevant there).
        if !objects_phase {
            render::render_roof_apexes(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                view.ceiling_apex_views,
                view.pending_roof_point,
            );
        }

        // Tap-to-unfan fan-vertex markers (roof-point mode only — the
        // controller feeds them empty otherwise): filled dot = fanned (tap
        // unfans), struck hollow dot = unfanned (tap re-fans). The tap
        // hit-test lives with the roof-point tap dispatch below.
        if !objects_phase && !view.ceiling_fan_markers.is_empty() {
            render::render_ceiling_fan_markers(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                view.ceiling_fan_markers,
            );
        }

        // The pending roof point's wall snap(s) as ONE tag, mirroring the
        // ceiling-step line's: drawn through the same snap-tag renderer, but
        // its menu is a separate widget routed to UnsnapCeilingPoint, never
        // ChooseSnap — the generic replay rewinds history, and the pending
        // point lives outside it. The menu offers exactly "Don't snap".
        if let Some(tag) = view.ceiling_point_snap_tag {
            render::render_snap_tags(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                std::slice::from_ref(tag),
            );
            if let Some(t) =
                render::active_storey_transform(rect, ber, current_storey_idx, mode, vp)
            {
                let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
                let label = match tag.current_kind {
                    Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                    None => "no snap ▾".to_string(),
                };
                egui::Area::new(ui.id().with("ceiling_point_snap_tag"))
                    .fixed_pos(sp + egui::vec2(7.0, -7.0))
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        ui.menu_button(label, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "point pinned onto the outline wall(s) — its position \
                                 isn't asked; un-snap reverts to the tapped point",
                                )
                                .small()
                                .weak(),
                            );
                            ui.separator();
                            if tag.skippable && ui.button("Don't snap").clicked() {
                                on_event(UiEvent::Sketch(SketchEvent::UnsnapCeilingPoint));
                                ui.close();
                            }
                        });
                    });
            }
        }

        // Roof ridges: committed two-apex groups as solid labelled segments
        // ("⌃ 4.20 m"), plus the pending ridge as a dashed SEGMENT between its
        // own endpoints (the stroke's extent, not bbox-spanning). Violet-blue,
        // distinct from the apex rose-magenta. Suppressed in the Objects phase.
        if !objects_phase {
            render::render_roof_ridges(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                view.ceiling_ridge_views,
            );
        }
        if let Some((a, b)) = view.pending_roof_ridge_line {
            render::render_roof_ridge_line(&painter, rect, ber, current_storey_idx, mode, vp, a, b);
        }

        // Roof RECTS (flat tops): committed four-apex rect groups as solid
        // labelled outlines, plus the pending rect as a dashed outline — the
        // ridge family's closed-stroke sibling, same violet-blue. Suppressed in
        // the Objects phase.
        if !objects_phase {
            render::render_roof_rects(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                view.ceiling_rect_views,
                view.pending_roof_rect_outline,
            );
        }

        // Committed DORMERS: each footprint as a solid labelled outline
        // ("⌐ 2.60 m"), distinct green from the ridge family (a dormer is a
        // non-resloping patch, not a ridge). The pending dormer footprint shows
        // through the shared pending-rect dashed outline above. Suppressed in
        // the Objects phase.
        if !objects_phase {
            render::render_dormers(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                view.dormer_views,
            );
        }

        // The pending ridge's wall snap, mirroring the ceiling-step line's:
        // drawn through the same snap-tag renderer, but its menu is a separate
        // widget routed to UnsnapCeilingRidge, never ChooseSnap — the generic
        // replay rewinds history, and the pending ridge lives outside it. The
        // menu offers exactly "Don't snap".
        if let Some(tag) = view.ceiling_ridge_snap_tag {
            render::render_snap_tags(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                std::slice::from_ref(tag),
            );
            if let Some(t) =
                render::active_storey_transform(rect, ber, current_storey_idx, mode, vp)
            {
                let sp = t.apply(tag.pos.0 as f64, tag.pos.1 as f64);
                let label = match tag.current_kind {
                    Some(k) => format!("{} ▾", render::snap_kind_label(k)),
                    None => "no snap ▾".to_string(),
                };
                egui::Area::new(ui.id().with("ceiling_ridge_snap_tag"))
                    .fixed_pos(sp + egui::vec2(7.0, -7.0))
                    .order(egui::Order::Foreground)
                    .show(ui.ctx(), |ui| {
                        ui.menu_button(label, |ui| {
                            ui.label(
                                egui::RichText::new(
                                    "ridge pinned onto the parallel wall — its position \
                                 isn't asked; un-snap reopens it",
                                )
                                .small()
                                .weak(),
                            );
                            ui.separator();
                            if tag.skippable && ui.button("Don't snap").clicked() {
                                on_event(UiEvent::Sketch(SketchEvent::UnsnapCeilingRidge));
                                ui.close();
                            }
                        });
                    });
            }
        }
    } // ── end single-storey plan chrome (overlay-mode suppression) ────────────

    // Wall exposure: colour each outline wall by its heat-loss class
    // (brighter in exposure mode; the selected wall is haloed). Derived
    // riser walls draw dashed alongside, selectable in exposure mode.
    // Suppressed under an all-storeys overlay mode (single-storey outline).
    if !overlay_mode {
        render::render_wall_exposure(
            &painter,
            rect,
            ber,
            current_storey_idx,
            mode,
            vp,
            view.exposure_walls,
            view.riser_views,
            view.exposure_selection.as_ref(),
            view.exposure_mode,
        );
    }

    // Single-storey plan chrome (continued): the in-place structure editors and
    // the Move-mode selection highlight also ride the active-storey transform, so
    // they're suppressed under an overlay mode too. (The gated wall-exposure
    // block above sits between the two runs, so this is a second contiguous wrap
    // rather than an extension of the first.)
    if !overlay_mode {
        // Structure "Edit attributes" (in-place): now the wall-exposure colours are
        // down, recolour each group's surfaces in its group colour ON TOP (so the
        // selection is unmistakable over exposure), draw a connector from each group
        // to its window, and float the per-group attribute editors in their columns.
        if let (Some(layout), Some(edit), Some(storey)) = (
            structure_edit_layout.as_ref(),
            view.structure_edit,
            ber.and_then(|b| current_storey_idx.and_then(|i| b.storeys.get(i))),
        ) {
            if let Some((_s, t)) = render::storey_transform(rect, ber, current_storey_idx, mode, vp)
            {
                render::structure_fan::render_groups_inplace(&painter, &t, storey, edit);
                let editable = !reviewing && !dim_prompting;
                structure_edit_windows_ui(
                    ui,
                    &painter,
                    view,
                    edit,
                    layout,
                    editable,
                    |w| w.has_target.then(|| t.apply(w.centroid.0, w.centroid.1)),
                    on_event,
                );
            }
        }

        // Structure "Edit attributes" on the ALL-STOREYS tab: the same in-place
        // session over the storey row. Each group's surfaces are recoloured in
        // EVERY cell that holds one (a group can span storeys); the connector
        // points into the cell holding the group's largest cluster; the same
        // floating attribute boxes sit in the margins reserved around
        // `plan_area` — the all-storeys view now looks like the single-storey
        // one instead of degrading to a side-panel strip.
        if let (Some(layout), Some(edit), Some(b), None) = (
            structure_edit_layout.as_ref(),
            view.structure_edit,
            ber,
            current_storey_idx,
        ) {
            if layout.all_storeys_inner.is_some() {
                let cells = render::all_storeys_2d::cell_transforms(plan_area, b, vp);
                for (s, storey) in b.storeys.iter().enumerate() {
                    if let Some(t) = cells.get(s) {
                        render::structure_fan::render_groups_inplace(&painter, t, storey, edit);
                    }
                }
                let editable = !reviewing && !dim_prompting;
                structure_edit_windows_ui(
                    ui,
                    &painter,
                    view,
                    edit,
                    layout,
                    editable,
                    |w| {
                        if !w.has_target {
                            return None;
                        }
                        w.storey_idx
                            .and_then(|s| cells.get(s))
                            .map(|t| t.apply(w.centroid.0, w.centroid.1))
                    },
                    on_event,
                );
            }
        }

        // Move-mode selection highlight: paint the selected wall
        // in red so the user sees which wall their drag is moving.
        // The inserted-vertex marker (if any) takes precedence —
        // both can't be selected at once.
        if matches!(edit_tool, EditTools::Move) {
            if let Some((wa, wb)) = view.edit_selected_wall {
                render::render_equivalent_wall(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    vp,
                    wa,
                    wb,
                );
            }
            if let Some((vx, vy)) = view.edit_selected_vertex {
                render::render_vertex_marker(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    vp,
                    vx,
                    vy,
                );
            }
        }
    } // ── end single-storey plan chrome (continued) ───────────────────────────

    // Vertex-drop prompt: draw a perpendicular distance arrow
    // for each still-active measurement (axis not yet pinned),
    // so the user sees what each text box refers to.
    // Single-storey editing — suppressed under an overlay mode.
    if !overlay_mode {
        if let Some(vdp) = &view.vertex_drop_prompt {
            for m in &vdp.active {
                use crate::controller::app_controller::VertexMeasurementKind;
                use manip_plan::ParameterAxis;
                // Axis measurements get a perpendicular arrow at
                // the vertex's row/column. WallLength measurements
                // draw the arrow directly along the diagonal from
                // neighbour → vertex (= the new wall itself).
                let (arrow_a, arrow_b) = match m.kind {
                    VertexMeasurementKind::Axis(ParameterAxis::X) => (
                        (m.neighbor_pos.0, vdp.vertex_pos.1),
                        (vdp.vertex_pos.0, vdp.vertex_pos.1),
                    ),
                    VertexMeasurementKind::Axis(ParameterAxis::Y) => (
                        (vdp.vertex_pos.0, m.neighbor_pos.1),
                        (vdp.vertex_pos.0, vdp.vertex_pos.1),
                    ),
                    VertexMeasurementKind::WallLength => (m.neighbor_pos, vdp.vertex_pos),
                };
                render::render_dimension_prompt(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    vp,
                    arrow_a,
                    arrow_b,
                );
            }
        }
    }

    // Post-drag measurement prompt overlay:
    //
    //   * Reference wall highlighted in red (same as the moved
    //     wall) so the user sees both walls involved.
    //   * A cyan arrow drawn PERPENDICULAR to both walls,
    //     spanning the actual distance being measured. For a
    //     vertical wall pair, the arrow is horizontal; for a
    //     horizontal wall pair, vertical.
    // Single-storey editing — suppressed under an overlay mode (the
    // carve does not use edit-drag).
    if !overlay_mode {
        if let Some(ep) = &view.edit_drag_prompt {
            render::render_equivalent_wall(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                ep.reference_wall.wall_a,
                ep.reference_wall.wall_b,
            );
            // Two parallel axis-aligned walls. Build a virtual
            // arrow perpendicular to both at the midpoint of the
            // moved wall's parallel range. render_dimension_prompt
            // handles drawing the arrow + arrowheads; the
            // "interior offset" auto-detects None for non-vertex
            // points so the arrow lands directly on the line.
            let m_dx = (ep.moved_wall.wall_b.0 - ep.moved_wall.wall_a.0).abs();
            let m_dy = (ep.moved_wall.wall_b.1 - ep.moved_wall.wall_a.1).abs();
            let is_vertical = m_dy > m_dx;
            let (arrow_a, arrow_b) = if is_vertical {
                // Horizontal arrow at moved wall's midpoint y.
                let mid_y = (ep.moved_wall.wall_a.1 + ep.moved_wall.wall_b.1) * 0.5;
                (
                    (ep.reference_wall.wall_a.0, mid_y),
                    (ep.moved_wall.wall_a.0, mid_y),
                )
            } else {
                // Vertical arrow at moved wall's midpoint x.
                let mid_x = (ep.moved_wall.wall_a.0 + ep.moved_wall.wall_b.0) * 0.5;
                (
                    (mid_x, ep.reference_wall.wall_a.1),
                    (mid_x, ep.moved_wall.wall_a.1),
                )
            };
            render::render_dimension_prompt(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                arrow_a,
                arrow_b,
            );
        }
    }

    // Dimension-prompt overlay. Every wall the user can
    // equivalently measure (including the primary) is highlighted
    // in red. The primary additionally gets the cyan interior
    // arrow + length label, drawn last so it sits on top.
    // Kept live while a carve ring is being dimensioned (the carve's own
    // length prompt); otherwise suppressed under an overlay mode so the
    // leftover single-storey outline prompt doesn't leak on top.
    if !overlay_mode || carve_dimensioning {
        if let Some(p) = &view.dim_prompt {
            for wall in &p.walls {
                render::render_equivalent_wall(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    vp,
                    wall.wall_a,
                    wall.wall_b,
                );
            }
            if let Some(primary) = p.walls.first() {
                render::render_dimension_prompt(
                    &painter,
                    rect,
                    ber,
                    current_storey_idx,
                    mode,
                    vp,
                    primary.wall_a,
                    primary.wall_b,
                );
            }
        }
    }

    // Position-prompt overlay (mirrors the edit-drag prompt's):
    //
    //   * Area wall being measured highlighted in ORANGE, the
    //     outline reference wall in RED — two different plans, so
    //     two colours keep them apart.
    //   * A cyan arrow drawn along the prompt's axis between the
    //     two walls, spanning the distance being measured.
    // Kept live while a carve ring is being dimensioned (mirrors the
    // dim-prompt gate); otherwise suppressed under an overlay mode.
    if !overlay_mode || carve_dimensioning {
        if let Some(pp) = &view.position_prompt {
            render::render_wall_highlight(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                pp.area_wall.wall_a,
                pp.area_wall.wall_b,
                egui::Color32::from_rgb(255, 170, 40), // orange: the area wall
            );
            render::render_wall_highlight(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                pp.reference_wall.wall_a,
                pp.reference_wall.wall_b,
                egui::Color32::from_rgb(255, 80, 80), // red: the outline reference
            );
            let (arrow_a, arrow_b) = match pp.axis {
                manip_plan::ParameterAxis::X => {
                    // Horizontal arrow at the area wall's midpoint y.
                    let mid_y = (pp.area_wall.wall_a.1 + pp.area_wall.wall_b.1) * 0.5;
                    (
                        (pp.reference_wall.wall_a.0, mid_y),
                        (pp.area_wall.wall_a.0, mid_y),
                    )
                }
                manip_plan::ParameterAxis::Y => {
                    // Vertical arrow at the area wall's midpoint x.
                    let mid_x = (pp.area_wall.wall_a.0 + pp.area_wall.wall_b.0) * 0.5;
                    (
                        (mid_x, pp.reference_wall.wall_a.1),
                        (mid_x, pp.area_wall.wall_a.1),
                    )
                }
            };
            render::render_dimension_prompt(
                &painter,
                rect,
                ber,
                current_storey_idx,
                mode,
                vp,
                arrow_a,
                arrow_b,
            );
        }
    }

    // Provisional curved-wall preview (amber arc) drawn on top, so
    // the user sees the curve while dimensioning its attach lines.
    // Single-storey editing — suppressed under an overlay mode.
    if !overlay_mode {
        if let Some(pts) = view.arc_preview {
            render::render_arc_preview(&painter, rect, ber, current_storey_idx, mode, vp, pts);
        }
    }

    // Two-finger navigation (touch): pinch zoom + two-finger pan, with
    // retraction of a stroke begun before the second finger landed. While a
    // gesture is active (until EVERY finger lifts) all primary-drag input
    // below — strokes, Move drags, exposure drags, 3D orbit — is suppressed.
    let touch_nav_active = touch.handle(ui, rect, view.current.is_some(), on_event);

    // Pan / zoom / orbit on the canvas.
    //   - Scroll wheel  → zoom about the cursor (2D and 3D)
    //   - Middle/right-drag → pan (2D and 3D)
    //   - Primary drag in 3D → orbit (Google-Earth style; primary is the
    //     stroke-drawing button in 2D, but draws nothing in 3D)
    if response.hovered() {
        let scroll_y = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll_y.abs() > 0.0 {
            let factor = if scroll_y > 0.0 { 1.1 } else { 1.0 / 1.1 };
            let anchor = ui
                .input(|i| i.pointer.hover_pos())
                .unwrap_or_else(|| rect.center());
            on_event(UiEvent::Sketch(SketchEvent::ZoomView {
                factor,
                anchor_x: anchor.x,
                anchor_y: anchor.y,
                canvas_center_x: rect.center().x,
                canvas_center_y: rect.center().y,
            }));
        }
    }
    // Middle or right mouse drag pans the canvas. We deliberately
    // skip primary (left) because that's the stroke-drawing button.
    if response.dragged_by(egui::PointerButton::Middle)
        || response.dragged_by(egui::PointerButton::Secondary)
    {
        let d = response.drag_delta();
        if d.x.abs() > 0.0 || d.y.abs() > 0.0 {
            on_event(UiEvent::Sketch(SketchEvent::PanView { dx: d.x, dy: d.y }));
        }
    }
    // In 3D the primary drag ORBITS the camera (it draws strokes only in
    // 2D — see `primary_available` below). Trackball feel: the near side of
    // the model follows the cursor — drag right swings it right (yaw),
    // drag down pulls it down revealing the top (pitch up, toward plan).
    // Reset via the Drawing category's Fit button.
    if matches!(mode, SketchMode::Storey3D)
        && !touch_nav_active
        && response.dragged_by(egui::PointerButton::Primary)
    {
        const ORBIT_DEG_PER_PIXEL: f32 = 0.4;
        let d = response.drag_delta();
        if d.x.abs() > 0.0 || d.y.abs() > 0.0 {
            on_event(UiEvent::Sketch(SketchEvent::OrbitView {
                dyaw: -d.x * ORBIT_DEG_PER_PIXEL,
                dpitch: d.y * ORBIT_DEG_PER_PIXEL,
            }));
        }
    }
    // In 2D NAVIGATION mode (no tool selected) the primary drag PANS — there's
    // no stroke to draw, so the canvas moves like every other CAD navigate tool.
    // Gated on every tool/mode being idle so it never double-fires with a tool's
    // own drag (a mode picked after navigation was entered, or Structure phase).
    let tools_idle = !view.openings_mode
        && !view.exposure_mode
        && !view.floor_exposure_mode
        && !view.ceiling_exposure_mode
        && !view.roof_mode
        && !view.phase_mode
        && !view.step_mode
        && !view.ceiling_step_mode
        && !view.ceiling_point_mode
        && !view.dormer_mode
        && !view.partition_mode
        && !view.living_area_mode
        && view.current_phase != AssessmentPhase::Structure;
    if matches!(mode, SketchMode::Storey2D)
        && view.navigation
        && tools_idle
        && !touch_nav_active
        && !final_north
        && response.dragged_by(egui::PointerButton::Primary)
    {
        let d = response.drag_delta();
        if d.x.abs() > 0.0 || d.y.abs() > 0.0 {
            on_event(UiEvent::Sketch(SketchEvent::PanView { dx: d.x, dy: d.y }));
        }
    }

    // Copy-height pick: while active it OWNS the primary tap (a click reads the
    // KNOWN height at the tapped surface and commits it). Placed FIRST and
    // threaded through every other pick's guard below (`!copy_pick`) so nothing
    // else fires while picking.
    let copy_pick = view.copy_height_pick_pending;
    if copy_pick
        && matches!(mode, SketchMode::Storey2D)
        && !touch_nav_active
        && response.clicked_by(egui::PointerButton::Primary)
    {
        if let (Some(pos), Some(t)) = (
            response.interact_pointer_pos(),
            canvas_transform(ber, current_storey_idx, rect, vp, true),
        ) {
            let (wx, wy) = t.inverse(pos.x, pos.y);
            let tol_world = (12.0 / t.scale()) as f64;
            on_event(UiEvent::Sketch(SketchEvent::CopyHeightTap {
                world_x: wx,
                world_y: wy,
                tol: tol_world,
            }));
        }
    }

    // Pending cut: a primary click picks which candidate region to
    // keep as the living area. Takes priority over stroke/edit input.
    let cut_pending = !view.pending_cut_regions.is_empty();
    if cut_pending && !copy_pick {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::ChooseCutRegion {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Pending PHASE cut: a primary click picks which candidate region
    // to tag with a construction phase. Mirrors the living-area cut.
    let phase_cut_pending = !view.pending_phase_cut_regions.is_empty();
    if phase_cut_pending && !copy_pick {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::ChoosePhaseCutRegion {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Pending STEP cut: a primary click picks which candidate region
    // gets the floor level. Mirrors the phase cut.
    let step_cut_pending = !view.pending_step_cut_regions.is_empty();
    if step_cut_pending && !copy_pick {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::ChooseStepCutRegion {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Ceiling-step taps: in mode with nothing pending, a tap places
    // the line's point (H/V picked next) — but only under the draw
    // tool, so Move/Trim clicks in-mode don't spawn phantom points.
    // While the side pick is pending — including with the mode
    // toggled off — the tap picks the stepping side. A pending
    // cut-pick owns the tap instead (those handlers above have no
    // mode guard, so one tap would otherwise fire both). Other
    // stages ignore taps (the controller's stage dispatch owns
    // that), so this can stay coarse.
    if (view.ceiling_step_side_pending
        || (view.ceiling_step_mode && matches!(edit_tool, EditTools::DrawPerpWalls)))
        && !copy_pick
        && !cut_pending
        && !phase_cut_pending
        && !step_cut_pending
    {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some((wx, wy)) = response
                .interact_pointer_pos()
                .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
            {
                on_event(UiEvent::Sketch(SketchEvent::CeilingStepTap {
                    world_x: wx,
                    world_y: wy,
                }));
            }
        }
    }

    // Roof-point tap: in mode with nothing pending, a tap places the
    // apex's point — but only under the draw tool, so Move/Trim clicks
    // in-mode don't spawn phantom points (the ceiling-step gating). A
    // pending cut-pick owns the tap instead. Once a stage is pending —
    // a point's OR a ridge's (stroke = ridge shares the mode) — taps do
    // nothing (the prompts are toolbar/panel-driven).
    if ((view.ceiling_point_mode
        && !view.ceiling_point_pending
        && !view.ceiling_ridge_pending
        && !view.ceiling_rect_pending)
        || view.neighbour_continuation_mode
        || view.storey_continuation_mode)
        && matches!(edit_tool, EditTools::DrawPerpWalls)
        && !copy_pick
        && !cut_pending
        && !phase_cut_pending
        && !step_cut_pending
    {
        if response.clicked_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                // Fan-marker hit-test FIRST, in SCREEN space (a constant
                // finger target regardless of zoom): the nearest marker
                // within 14 px claims the tap and toggles its vertex —
                // otherwise the tap would ALSO try to place a new roof
                // point on top of the marker. The event carries the
                // MARKER's own world position, not the finger's.
                // A continuation pick owns the tap — the markers must not
                // steal it.
                let continuation_pick = view.ceiling_continuation_picking_target
                    || view.ceiling_continuation_picking_source;
                let marker_hit = if continuation_pick {
                    None
                } else {
                    render::active_storey_transform(rect, ber, current_storey_idx, mode, vp)
                        .and_then(|t| {
                            view.ceiling_fan_markers
                                .iter()
                                .map(|((mx, my), _, _)| {
                                    ((*mx, *my), t.apply(*mx, *my).distance(pos))
                                })
                                .filter(|(_, d)| *d <= 14.0)
                                .min_by(|(_, a), (_, b)| a.total_cmp(b))
                                .map(|(m, _)| m)
                        })
                };
                if let Some((mx, my)) = marker_hit {
                    on_event(UiEvent::Sketch(SketchEvent::CeilingFanMarkerTap {
                        world_x: mx,
                        world_y: my,
                    }));
                } else if let Some((wx, wy)) =
                    screen_to_world(ber, current_storey_idx, rect, vp, pos)
                {
                    on_event(UiEvent::Sketch(SketchEvent::CeilingPointTap {
                        world_x: wx,
                        world_y: wy,
                    }));
                }
            }
        }
    }

    // Structure phase: the canvas has its OWN tools. In the CLICK select mode a
    // primary tap (2D) toggles the surface under it in/out of the selection;
    // the CARVE mode instead captures a stroke (handled in the stroke path
    // below). All Mapping primary input is gated off (`primary_available`).
    let structure_phase = view.current_phase == AssessmentPhase::Structure;
    let structure_click =
        structure_phase && matches!(view.structure_select, Some(SelectMode::Click));
    let structure_carve =
        structure_phase && matches!(view.structure_select, Some(SelectMode::Carve));
    if structure_click && matches!(mode, SketchMode::Storey2D) && !touch_nav_active && !copy_pick {
        // `watermark = true`: the storey is DRAWN with the previous-storey
        // watermark folded into the fit (`fit_bbox_for_storey`), so a setback
        // upper storey renders smaller than its own bbox would. Invert the tap
        // through that SAME transform, or the hit-test point is offset and walls
        // never get picked on upper storeys (ground storey has no watermark, so
        // it worked). Matches the other interaction paths already on `true`.
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, true) {
            let tol_world = (12.0 / t.scale()) as f64;
            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::StructureSurfaceTap {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
        } else if current_storey_idx.is_none() {
            // ALL-STOREYS tab: route the tap to the CELL it landed in — invert
            // that cell's transform into its storey's own world frame and let
            // the controller hit-test that storey. Taps across cells build a
            // cross-storey selection just like the lasso.
            if response.clicked_by(egui::PointerButton::Primary) {
                if let (Some(pos), Some(b)) = (response.interact_pointer_pos(), ber) {
                    // `plan_area`: with the "Edit attributes" margins reserved,
                    // the cells the row was DRAWN into are the ones to invert.
                    if let Some(i) = render::all_storeys_2d::cell_index_at(plan_area, b, pos) {
                        if let Some(t) =
                            render::all_storeys_2d::cell_transforms(plan_area, b, vp).get(i)
                        {
                            let (wx, wy) = t.inverse(pos.x, pos.y);
                            on_event(UiEvent::Sketch(
                                SketchEvent::StructureSurfaceTapAllStoreys {
                                    storey_idx: i,
                                    world_x: wx,
                                    world_y: wy,
                                    tol: (12.0 / t.scale()) as f64,
                                },
                            ));
                        }
                    }
                }
            }
        }
    }

    // Structure CARVE mode (2D): while a cut is awaiting a pick, a tap chooses
    // which region's surfaces to select; otherwise a primary drag draws the
    // carve stroke (captured in pixels, converted to world on release — the
    // same path living-area cuts use), then routed to the carve handler.
    let structure_cut_picking = !view.structure_cut_regions.is_empty();
    // On the ALL-STOREYS tab the carve is the free LASSO below, not this
    // single-storey path.
    if structure_carve
        && current_storey_idx.is_some()
        && matches!(mode, SketchMode::Storey2D)
        && !touch_nav_active
    {
        if structure_cut_picking {
            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some((wx, wy)) = response
                    .interact_pointer_pos()
                    .and_then(|pos| screen_to_world(ber, current_storey_idx, rect, vp, pos))
                {
                    on_event(UiEvent::Sketch(SketchEvent::ChooseStructureCutRegion {
                        world_x: wx,
                        world_y: wy,
                    }));
                }
            }
        } else {
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    on_event(UiEvent::Sketch(SketchEvent::StartStroke(
                        pos - rect.left_top().to_vec2(),
                    )));
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    on_event(UiEvent::Sketch(SketchEvent::AddPoint(
                        pos - rect.left_top().to_vec2(),
                    )));
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                if let (Some(stroke), Some(t)) = (
                    view.current.as_ref(),
                    // watermark = true: match the render transform, or the carve
                    // stroke lands in the wrong world spot on a setback upper
                    // storey (same fix as the structure click above).
                    canvas_transform(ber, current_storey_idx, rect, vp, true),
                ) {
                    let topleft = rect.left_top();
                    let world_points: Vec<(f32, f32)> = stroke
                        .points
                        .iter()
                        .map(|p| {
                            let (wx, wy) = t.inverse(p.x + topleft.x, p.y + topleft.y);
                            (wx as f32, wy as f32)
                        })
                        .collect();
                    on_event(UiEvent::Sketch(SketchEvent::StructureCarveStrokeWorld {
                        points: world_points,
                    }));
                } else {
                    on_event(UiEvent::Sketch(SketchEvent::CancelStroke));
                }
            }
            if let Some(stroke) = &view.current {
                draw_stroke(&painter, rect, stroke);
            }
        }
    }

    // Structure CARVE on the ALL-STOREYS tab: a FREE LASSO. The stroke is
    // captured in pixels like the single-storey carve, but on release it's
    // converted to EVERY storey's own world frame via the per-cell transforms
    // (the same screen stroke means a different world polyline per cell — the
    // controller clips each against its storey's footprint, so the gap
    // between cells contributes nothing). No outline attach/collapse and no
    // dimensioning: the lasso is a pure selection gesture.
    if structure_carve
        && current_storey_idx.is_none()
        && matches!(mode, SketchMode::Storey2D)
        && !touch_nav_active
    {
        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                on_event(UiEvent::Sketch(SketchEvent::StartStroke(
                    pos - rect.left_top().to_vec2(),
                )));
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                on_event(UiEvent::Sketch(SketchEvent::AddPoint(
                    pos - rect.left_top().to_vec2(),
                )));
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            let cells = ber
                .map(|b| render::all_storeys_2d::cell_transforms(plan_area, b, vp))
                .unwrap_or_default();
            if let (Some(stroke), false) = (view.current.as_ref(), cells.is_empty()) {
                let topleft = rect.left_top();
                let per_storey: Vec<Vec<(f32, f32)>> = cells
                    .iter()
                    .map(|t| {
                        stroke
                            .points
                            .iter()
                            .map(|p| {
                                let (wx, wy) = t.inverse(p.x + topleft.x, p.y + topleft.y);
                                (wx as f32, wy as f32)
                            })
                            .collect()
                    })
                    .collect();
                on_event(UiEvent::Sketch(SketchEvent::StructureLassoStrokeWorld {
                    per_storey,
                }));
            } else {
                on_event(UiEvent::Sketch(SketchEvent::CancelStroke));
            }
        }
        if let Some(stroke) = &view.current {
            draw_stroke(&painter, rect, stroke);
        }
    }

    // Floor-exposure tool (post-gate): the overlaid all-storeys floor view owns
    // the canvas. In Click a primary tap toggles the floor region under it
    // (across the stack); in Carve a primary drag captures a stroke (pixels →
    // world on release) that DIMENSIONS the carve, which then splits the floor.
    // While a dimension/position prompt is up (the carve is being measured) the
    // canvas input is suppressed so the floating number box owns interaction.
    // Uses the union-fit transform so taps/strokes match the drawn floors. 2D only.
    if floor_exposure_active
        && matches!(mode, SketchMode::Storey2D)
        && !touch_nav_active
        && !dim_prompting
        && view.position_prompt.is_none()
    {
        if let Some(t) = floor_exposure_transform(ber, rect, vp) {
            let tol_world = (12.0 / t.scale()) as f64;
            match view.floor_exposure_select {
                Some(SelectMode::Click) => {
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let (wx, wy) = t.inverse(pos.x, pos.y);
                            on_event(UiEvent::Sketch(SketchEvent::FloorExposureTap {
                                world_x: wx,
                                world_y: wy,
                                tol: tol_world,
                            }));
                        }
                    }
                }
                Some(SelectMode::Carve) if !view.floor_exposure_cut_regions.is_empty() => {
                    // A straight cut is awaiting a side pick — a primary tap
                    // chooses which half to dimension + split (carve stroke is
                    // suppressed until the pick is made).
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let (wx, wy) = t.inverse(pos.x, pos.y);
                            on_event(UiEvent::Sketch(SketchEvent::ChooseFloorExposureCutRegion {
                                world_x: wx,
                                world_y: wy,
                            }));
                        }
                    }
                }
                Some(SelectMode::Carve) => {
                    if response.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            on_event(UiEvent::Sketch(SketchEvent::StartStroke(
                                pos - rect.left_top().to_vec2(),
                            )));
                        }
                    }
                    if response.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            on_event(UiEvent::Sketch(SketchEvent::AddPoint(
                                pos - rect.left_top().to_vec2(),
                            )));
                        }
                    }
                    if response.drag_stopped_by(egui::PointerButton::Primary) {
                        if let Some(stroke) = view.current.as_ref() {
                            let topleft = rect.left_top();
                            let world_points: Vec<(f32, f32)> = stroke
                                .points
                                .iter()
                                .map(|p| {
                                    let (wx, wy) = t.inverse(p.x + topleft.x, p.y + topleft.y);
                                    (wx as f32, wy as f32)
                                })
                                .collect();
                            on_event(UiEvent::Sketch(
                                SketchEvent::FloorExposureCarveStrokeWorld {
                                    points: world_points,
                                },
                            ));
                        } else {
                            on_event(UiEvent::Sketch(SketchEvent::CancelStroke));
                        }
                    }
                    if let Some(stroke) = &view.current {
                        draw_stroke(&painter, rect, stroke);
                    }
                }
                _ => {}
            }
        }
    }

    // Ceiling-exposure tool (post-gate): the canvas input mirror of the floor
    // tool — Click toggles the ceiling region under a tap (across the stack);
    // Carve captures a stroke that DIMENSIONS the carve, which then splits the
    // ceilings. Uses the same footprint-union transform the overlaid view fits to.
    if ceiling_exposure_active
        && matches!(mode, SketchMode::Storey2D)
        && !touch_nav_active
        && !dim_prompting
        && view.position_prompt.is_none()
    {
        if let Some(t) = floor_exposure_transform(ber, rect, vp) {
            let tol_world = (12.0 / t.scale()) as f64;
            match view.ceiling_exposure_select {
                Some(SelectMode::Click) => {
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let (wx, wy) = t.inverse(pos.x, pos.y);
                            on_event(UiEvent::Sketch(SketchEvent::CeilingExposureTap {
                                world_x: wx,
                                world_y: wy,
                                tol: tol_world,
                            }));
                        }
                    }
                }
                Some(SelectMode::Carve) if !view.ceiling_exposure_cut_regions.is_empty() => {
                    // A straight cut is awaiting a side pick — a primary tap
                    // chooses which half to dimension + split.
                    if response.clicked_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            let (wx, wy) = t.inverse(pos.x, pos.y);
                            on_event(UiEvent::Sketch(
                                SketchEvent::ChooseCeilingExposureCutRegion {
                                    world_x: wx,
                                    world_y: wy,
                                },
                            ));
                        }
                    }
                }
                Some(SelectMode::Carve) => {
                    if response.drag_started_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            on_event(UiEvent::Sketch(SketchEvent::StartStroke(
                                pos - rect.left_top().to_vec2(),
                            )));
                        }
                    }
                    if response.dragged_by(egui::PointerButton::Primary) {
                        if let Some(pos) = response.interact_pointer_pos() {
                            on_event(UiEvent::Sketch(SketchEvent::AddPoint(
                                pos - rect.left_top().to_vec2(),
                            )));
                        }
                    }
                    if response.drag_stopped_by(egui::PointerButton::Primary) {
                        if let Some(stroke) = view.current.as_ref() {
                            let topleft = rect.left_top();
                            let world_points: Vec<(f32, f32)> = stroke
                                .points
                                .iter()
                                .map(|p| {
                                    let (wx, wy) = t.inverse(p.x + topleft.x, p.y + topleft.y);
                                    (wx as f32, wy as f32)
                                })
                                .collect();
                            on_event(UiEvent::Sketch(
                                SketchEvent::CeilingExposureCarveStrokeWorld {
                                    points: world_points,
                                },
                            ));
                        } else {
                            on_event(UiEvent::Sketch(SketchEvent::CancelStroke));
                        }
                    }
                    if let Some(stroke) = &view.current {
                        draw_stroke(&painter, rect, stroke);
                    }
                }
                _ => {}
            }
        }
    }

    // Roof step (post-gate). Two input modes share the takeover transform:
    // while a ridge is awaited (`roof_area_ridge_pending`) a primary DRAG
    // captures the ridge stroke; otherwise (grouping) a primary TAP toggles the
    // ceiling region under it in/out of the roof-area-in-progress.
    if roof_active
        && matches!(mode, SketchMode::Storey2D)
        && !touch_nav_active
        && !dim_prompting
        && view.position_prompt.is_none()
    {
        if let Some(t) = floor_exposure_transform(ber, rect, vp) {
            if view.roof_area_ridge_pending {
                if response.drag_started_by(egui::PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        on_event(UiEvent::Sketch(SketchEvent::StartStroke(
                            pos - rect.left_top().to_vec2(),
                        )));
                    }
                }
                if response.dragged_by(egui::PointerButton::Primary) {
                    if let Some(pos) = response.interact_pointer_pos() {
                        on_event(UiEvent::Sketch(SketchEvent::AddPoint(
                            pos - rect.left_top().to_vec2(),
                        )));
                    }
                }
                if response.drag_stopped_by(egui::PointerButton::Primary) {
                    if let Some(stroke) = view.current.as_ref() {
                        let topleft = rect.left_top();
                        let world_points: Vec<(f32, f32)> = stroke
                            .points
                            .iter()
                            .map(|p| {
                                let (wx, wy) = t.inverse(p.x + topleft.x, p.y + topleft.y);
                                (wx as f32, wy as f32)
                            })
                            .collect();
                        on_event(UiEvent::Sketch(SketchEvent::RoofAreaRidgeStroke {
                            points: world_points,
                        }));
                    } else {
                        on_event(UiEvent::Sketch(SketchEvent::CancelStroke));
                    }
                }
                if let Some(stroke) = &view.current {
                    draw_stroke(&painter, rect, stroke);
                }
            } else if !view.roof_area_pitch_pending
                && !copy_pick
                && response.clicked_by(egui::PointerButton::Primary)
            {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    let tol_world = (12.0 / t.scale()) as f64;
                    on_event(UiEvent::Sketch(SketchEvent::RoofRegionTap {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
            // Pending ridge(s) — a hip's collected set while more are drawn,
            // the full set while the pitch is entered: segments over the area.
            for ((ax, ay), (bx, by)) in &view.roof_area_ridge_lines {
                let a = t.apply(*ax, *ay);
                let b = t.apply(*bx, *by);
                painter.line_segment(
                    [a, b],
                    egui::Stroke::new(2.5, egui::Color32::from_rgb(60, 180, 90)),
                );
            }
        }
    }

    // Exposure mode: a tap selects the wall under the pointer; a
    // primary drag near a selection handle slides that handle along
    // the wall. Takes priority over stroke/edit input (suppressed via
    // `primary_available` above).
    if view.exposure_mode && !cut_pending && !touch_nav_active {
        // Watermark-inclusive fit (`true`) so this matches the SAME transform the
        // exposure walls are RENDERED with (`render_wall_exposure` →
        // `storey_transform` → `fit_bbox_for_storey`). On a setback upper storey
        // the watermark enlarges the fit box; using the storey-alone box here
        // (`false`) put clicks in a different scale/origin than the drawn walls,
        // so only a near-coincident wall stayed selectable.
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, true) {
            let tol_world = (12.0 / t.scale()) as f64;
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::ExposurePointerDown {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::ExposurePointerDrag {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                on_event(UiEvent::Sketch(SketchEvent::ExposurePointerUp));
            }
            // A plain tap (no drag) still selects the wall under it.
            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::ExposurePointerDown {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
        }
    }

    // Openings mode (no elevation open): a tap near a wall drops the selected
    // opening type on the nearest wall (or opens the wall's elevation in Edit).
    // The elevation overlay, when open, captures its own input (see the
    // elevation render path), so plan taps are gated on it being closed.
    if view.openings_mode
        && matches!(mode, SketchMode::Storey2D)
        && view.elevation_view.is_none()
        && !cut_pending
        && !touch_nav_active
    {
        // Watermark-inclusive fit (`true`) so this matches the SAME transform the
        // walls + openings are RENDERED with (`render_ber` → `storey_transform` →
        // `fit_bbox_for_storey`). On a setback upper storey the watermark enlarges
        // the fit box; using the storey-alone box here (`false`) put clicks in a
        // different scale/origin than the drawn walls, so the tap never landed on
        // a wall — the exposure-mode bug (above), same cause. Ground storey has no
        // watermark, so it worked; upper setback storeys didn't.
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, true) {
            let tol_world = (14.0 / t.scale()) as f64;
            let tap = response.clicked_by(egui::PointerButton::Primary)
                || response.drag_started_by(egui::PointerButton::Primary);
            if tap {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::OpeningsPointerDown {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
            // Continue / end a plan drag of a grabbed ROOF window (Edit tool). A
            // no-op for the placement tools (nothing is grabbed).
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::OpeningsPointerDrag {
                        world_x: wx,
                        world_y: wy,
                    }));
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                on_event(UiEvent::Sketch(SketchEvent::OpeningsPointerUp));
            }
        }
    }

    // Phase-3 (Objects): a primary tap on the 2D plan drops / opens a room pin
    // at that world point (the entry panel then adds objects to it). Gated to the
    // Objects phase + 2D, and off while navigating / two-finger panning.
    // A CLICK places/opens a pin; a DRAG still pans (handled by nav), so no
    // navigation gate here — only suppress while two-finger panning.
    if matches!(view.current_phase, AssessmentPhase::Objects)
        && matches!(mode, SketchMode::Storey2D)
        && !touch_nav_active
    {
        // watermark = true: match the storey render, or the pin drops at the
        // wrong world point on a setback upper storey (same fix as elsewhere).
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, true) {
            let tol_world = (16.0 / t.scale()) as f64;
            if response.clicked_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::ObjectsPointerDown {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
        } else if current_storey_idx.is_none() {
            // ALL-STOREYS tab: canvas_transform is None (no single storey), so
            // route the tap to the CELL it landed in — invert that cell's
            // transform into its storey's own frame and drop/open a pin on THAT
            // storey (mirrors the structure all-storeys tap).
            if response.clicked_by(egui::PointerButton::Primary) {
                if let (Some(pos), Some(b)) = (response.interact_pointer_pos(), ber) {
                    if let Some(i) = render::all_storeys_2d::cell_index_at(rect, b, pos) {
                        if let Some(t) = render::all_storeys_2d::cell_transforms(rect, b, vp).get(i)
                        {
                            let (wx, wy) = t.inverse(pos.x, pos.y);
                            on_event(UiEvent::Sketch(SketchEvent::ObjectsPointerDownAllStoreys {
                                storey_idx: i,
                                world_x: wx,
                                world_y: wy,
                                tol: (16.0 / t.scale()) as f64,
                            }));
                        }
                    }
                }
            }
        }
    }

    // Phase-3 (Objects) in a 3D view: a primary CLICK (a drag orbits — see the
    // orbit block above) picks the closest viewer-facing surface under the
    // cursor — wall / floor / ceiling, plus the dwelling roof planes in the
    // full-house view (the All tab) — and drops / opens a room pin ON it. The
    // controller receives the resolved hit (storey + storey-local point +
    // pitch), never the screen point.
    if matches!(view.current_phase, AssessmentPhase::Objects)
        && matches!(mode, SketchMode::Storey3D)
        && !touch_nav_active
        && response.clicked_by(egui::PointerButton::Primary)
    {
        if let (Some(ber), Some(pos)) = (ber, response.interact_pointer_pos()) {
            if let Some(hit) =
                render::pick_3d::pick_surface_3d(rect, ber, current_storey_idx, vp, pos)
            {
                // Same 16-px reopen tolerance as the 2D tap, converted to
                // metres through the 3D view's fit scale.
                let tol = render::pick_3d::transform_3d(rect, ber, current_storey_idx, vp)
                    .map(|t| 16.0 / t.scale())
                    .unwrap_or(0.3);
                on_event(UiEvent::Sketch(SketchEvent::ObjectsPointerDown3D {
                    storey_idx: hit.storey_idx,
                    world_x: hit.world_point.0,
                    world_y: hit.world_point.1,
                    z_m: hit.storey_local_z,
                    pitch_deg: hit.pitch_deg.map(|p| p as f32),
                    tol,
                }));
            }
        }
    }

    // Phase-3 (Objects): draw the room pins on the 2D plan — a dot per pin with
    // its object count, the open one ringed.
    if matches!(view.current_phase, AssessmentPhase::Objects)
        && matches!(mode, SketchMode::Storey2D)
        && !view.object_pins.is_empty()
    {
        // watermark = true: draw the pins with the SAME transform the storey is
        // rendered with, so they sit on the plan on a setback upper storey
        // (and stay under the taps handled above).
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, true) {
            let mut active_sp = None;
            for pin in &view.object_pins {
                let sp = t.apply(pin.x, pin.y);
                let fill = if pin.active {
                    egui::Color32::from_rgb(90, 200, 180)
                } else {
                    egui::Color32::from_rgb(70, 150, 200)
                };
                if pin.active {
                    painter.circle_stroke(sp, 13.0, egui::Stroke::new(2.0, egui::Color32::WHITE));
                    active_sp = Some(sp);
                }
                painter.circle_filled(sp, 9.0, fill);
                painter.text(
                    sp,
                    egui::Align2::CENTER_CENTER,
                    pin.count.to_string(),
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
                if !pin.name.trim().is_empty() {
                    painter.text(
                        sp + egui::vec2(0.0, -16.0),
                        egui::Align2::CENTER_BOTTOM,
                        &pin.name,
                        egui::FontId::proportional(11.0),
                        egui::Color32::from_gray(220),
                    );
                }
            }
            // Dropdown of the OPEN room's objects, anchored just below its pin.
            if let (Some(sp), Some(room)) = (active_sp, view.active_object_room.as_ref()) {
                object_pin_dropdown(ui, sp, room, on_event);
            }
        }
    }

    // Highlight the roof window selected for Edit-on-surface (a bright border on
    // its plan footprint), drawn on top of the openings painted by `render_ber`.
    if view.openings_mode && view.elevation_view.is_none() {
        if let Some(sel) = view.roof_window_selected {
            // Watermark-inclusive (`true`) to overlay exactly on the openings
            // `render_ber` painted (`storey_transform` → `fit_bbox_for_storey`) —
            // else the highlight drifts off the window on a setback upper storey.
            if let (Some(t), Some(storey)) = (
                canvas_transform(ber, current_storey_idx, rect, vp, true),
                ber.and_then(|b| current_storey_idx.and_then(|i| b.storeys.get(i))),
            ) {
                if let Some(w) = storey.windows.iter().find(|w| w.id == sel) {
                    let pts: Vec<egui::Pos2> = w
                        .vertices
                        .iter()
                        .map(|(x, y, _z)| t.apply(*x as f64, *y as f64))
                        .collect();
                    if pts.len() >= 3 {
                        painter.add(egui::Shape::closed_line(
                            pts,
                            egui::Stroke::new(2.5, egui::Color32::from_rgb(255, 180, 40)),
                        ));
                    }
                }
            }
        }
    }

    // Primary-button input: stroke drawing OR Move-mode
    // click/drag, depending on `edit_tool`. Both gated on
    // 2D mode and "no overlay is open" (review / pending
    // redraw / dim prompt / cut-pick take priority).
    let primary_available = matches!(mode, SketchMode::Storey2D)
        && !view.navigation
        && !reviewing
        && !view.pending_redraw
        && !dim_prompting
        && !cut_pending
        && !phase_cut_pending
        && !view.phase_pick_pending
        && !step_cut_pending
        && !view.step_pick_pending
        && !view.ceiling_step_pending
        && !view.ceiling_point_pending
        && !view.ceiling_ridge_pending
        && !view.ceiling_rect_pending
        && !view.exposure_mode
        && !view.floor_exposure_mode
        && !view.ceiling_exposure_mode
        && !view.roof_mode
        && !view.partition_mode
        && !view.openings_mode
        // Structure phase owns the canvas with its own selection tools; the
        // Objects phase owns it too (tap to drop pins) — neither should let a
        // primary drag run the mapping draw/edit tools (which replace geometry).
        && !structure_phase
        && !matches!(view.current_phase, AssessmentPhase::Objects)
        // North-align owns input (the image is being moved/rotated, not the plan).
        && !final_north;

    if primary_available && !touch_nav_active && !matches!(edit_tool, EditTools::Move) {
        if response.drag_started_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                let local_pos = pos - rect.left_top().to_vec2();
                on_event(UiEvent::Sketch(SketchEvent::StartStroke(local_pos)));
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                let local_pos = pos - rect.left_top().to_vec2();
                on_event(UiEvent::Sketch(SketchEvent::AddPoint(local_pos)));
            }
        }
        if response.drag_stopped_by(egui::PointerButton::Primary) {
            if let Some(pos) = response.interact_pointer_pos() {
                let local_pos = pos - rect.left_top().to_vec2();
                // Extend and Trim both operate on the existing
                // polygon, which may be in real (meter) units while
                // the stroke is in canvas pixels. Convert each
                // captured stroke point to world coords via the fit
                // transform and send the tool's dedicated world-coord
                // event. Other stroke tools fall through to the
                // normal EndStroke flow (canvas pixels).
                // A living-area DrawPerpWalls is drawn in the storey's
                // (metre) space too, so it also needs world coords.
                let living_area_draw =
                    view.living_area_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // Construction-phase tagging draws in the storey's
                // metre space too (carve an area to assign a year).
                let phase_draw = view.phase_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // Floor-step tagging draws in the storey's metre
                // space too (carve an area to raise/lower its floor).
                let step_draw = view.step_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // A ceiling-step stroke is the LINE the ceiling
                // steps along — drawn in metre space as well.
                let ceiling_step_draw =
                    view.ceiling_step_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // In roof-point mode a STROKE draws a RIDGE (taps stay
                // the point input — the ceiling-step stroke/tap split),
                // in metre space too.
                let ceiling_ridge_draw =
                    view.ceiling_point_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // In dormer mode a closed stroke draws the dormer footprint —
                // the rect flow's sibling, in metre space too.
                let dormer_draw = view.dormer_mode && matches!(edit_tool, EditTools::DrawPerpWalls);
                // First outline of a fresh upper storey: drawn over
                // the previous-storey watermark, so it must be
                // captured in that storey's metre space (mirrors the
                // watermark predicate in render::render_ber).
                let outline_on_watermark = matches!(edit_tool, EditTools::DrawPerpWalls)
                    && !view.living_area_mode
                    && !view.phase_mode
                    && !view.step_mode
                    && !view.ceiling_step_mode
                    && !view.ceiling_point_mode
                    && !view.dormer_mode
                    && matches!(current_storey_idx, Some(i) if i > 0)
                    && ber
                        .and_then(|b| b.storeys.get(current_storey_idx?))
                        .is_some_and(|s| s.floors.regions.is_empty());
                let needs_world = living_area_draw
                    || phase_draw
                    || step_draw
                    || ceiling_step_draw
                    || ceiling_ridge_draw
                    || dormer_draw
                    || outline_on_watermark
                    || matches!(
                        edit_tool,
                        EditTools::ExtendOrtho
                            | EditTools::ExtendCurve
                            | EditTools::Trim
                            | EditTools::TrimOrtho
                            | EditTools::TrimCurve
                    );
                let world_converted = if needs_world {
                    // Outline capture must invert the SAME transform
                    // the watermark was drawn with (bbox includes the
                    // previous storey), hence `watermark = true`.
                    if let (Some(stroke), Some(t)) = (
                        view.current.as_ref(),
                        canvas_transform(ber, current_storey_idx, rect, vp, true),
                    ) {
                        let topleft = rect.left_top();
                        let world_points: Vec<(f32, f32)> = stroke
                            .points
                            .iter()
                            .map(|p| {
                                let abs_x = p.x + topleft.x;
                                let abs_y = p.y + topleft.y;
                                let (wx, wy) = t.inverse(abs_x, abs_y);
                                (wx as f32, wy as f32)
                            })
                            .collect();
                        // Living-area draw starts a new sub-shape;
                        // an outline-on-watermark sends
                        // OutlineStrokeWorld; trims send
                        // TrimStrokeWorld, extends send
                        // ExtendStrokeWorld; the controller dispatches
                        // the exact variant by the active tool.
                        let event = if living_area_draw {
                            SketchEvent::LivingAreaStrokeWorld {
                                points: world_points,
                            }
                        } else if phase_draw {
                            SketchEvent::PhaseRegionStrokeWorld {
                                points: world_points,
                            }
                        } else if step_draw {
                            SketchEvent::StepRegionStrokeWorld {
                                points: world_points,
                            }
                        } else if ceiling_step_draw {
                            SketchEvent::CeilingStepStrokeWorld {
                                points: world_points,
                            }
                        } else if ceiling_ridge_draw {
                            SketchEvent::CeilingRidgeStrokeWorld {
                                points: world_points,
                            }
                        } else if dormer_draw {
                            SketchEvent::DormerStrokeWorld {
                                points: world_points,
                            }
                        } else if outline_on_watermark {
                            SketchEvent::OutlineStrokeWorld {
                                points: world_points,
                            }
                        } else if matches!(
                            edit_tool,
                            EditTools::Trim | EditTools::TrimOrtho | EditTools::TrimCurve
                        ) {
                            SketchEvent::TrimStrokeWorld {
                                points: world_points,
                            }
                        } else {
                            SketchEvent::ExtendStrokeWorld {
                                points: world_points,
                            }
                        };
                        on_event(UiEvent::Sketch(event));
                        true
                    } else {
                        false
                    }
                } else {
                    false
                };
                if !world_converted {
                    // Diagnostic: an outline-on-watermark stroke that
                    // can't convert to world coords falls back to the
                    // pixel path (no snap, re-scales from scratch).
                    // Surface WHY so the failure isn't silent.
                    if outline_on_watermark {
                        let has_stroke = view.current.is_some();
                        let has_bbox =
                            canvas_transform(ber, current_storey_idx, rect, vp, true).is_some();
                        log::warn!(
                            "[new-storey] WARNING: outline-on-watermark stroke could \
                             NOT convert to world coords (stroke present={has_stroke}, \
                             fit bbox present={has_bbox}) — falling back to the pixel \
                             path. If bbox is missing, the previous storey has no \
                             committed floor outline to anchor the transform."
                        );
                    }
                    on_event(UiEvent::Sketch(SketchEvent::EndStroke(local_pos)));
                }
            }
        }

        if let Some(stroke) = &view.current {
            draw_stroke(&painter, rect, stroke);
        }
    }

    // Move-mode primary input: click to select a wall, drag to
    // translate it. Suspended while a post-drag measurement
    // prompt is up — that prompt has to be Apply / Cycle /
    // Cancel first.
    if primary_available
        && !touch_nav_active
        && matches!(edit_tool, EditTools::Move)
        && view.edit_drag_prompt.is_none()
        && view.vertex_drop_prompt.is_none()
    {
        // One transform for both the double-click vertex-insert and
        // the click/drag below (same storey, same frame). watermark = true to
        // match the render, or Move-mode wall selection is offset on a setback
        // upper storey (as the Structure click was).
        if let Some(t) = canvas_transform(ber, current_storey_idx, rect, vp, true) {
            // World-coord tolerance corresponds to ~12 pixels
            // on screen at the current zoom.
            let tol_world = 12.0 / t.scale();
            // Double-click first — egui fires this on the second
            // press of a quick double-press. We insert the vertex
            // and treat any subsequent drag movement as moving the
            // just-inserted vertex.
            if response.double_clicked_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::EditDoubleClick {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
            if response.drag_started_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::EditMouseDown {
                        world_x: wx,
                        world_y: wy,
                        tol: tol_world,
                    }));
                }
            }
            if response.dragged_by(egui::PointerButton::Primary) {
                if let Some(pos) = response.interact_pointer_pos() {
                    let (wx, wy) = t.inverse(pos.x, pos.y);
                    on_event(UiEvent::Sketch(SketchEvent::EditMouseDrag {
                        world_x: wx,
                        world_y: wy,
                    }));
                }
            }
            if response.drag_stopped_by(egui::PointerButton::Primary) {
                on_event(UiEvent::Sketch(SketchEvent::EditMouseUp));
            }
        }
    }
}

/// The floating 2D/3D toggle, anchored to the canvas's top-right corner. Plain
/// text label for now — visual design deferred. Shows the current view
/// ("2D"/"3D"); a click switches to the other. Disabled until the current
/// scope has its ceiling height(s) — heights are entered via the Heights
/// category, and the controller's `set_sketch_mode` re-enforces the gate as a
/// no-op.
fn view_buttons<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view: &SketchView<'_>,
    on_event: &mut F,
    // The phase-signpost rect to steer clear of. The signpost is centred and its
    // width varies (the triangle, the expanded checklist), so on a narrow screen
    // its right edge can reach this top-right column — when it does, drop below it.
    avoid: egui::Rect,
) {
    let left = rect.right() - 110.0;
    let top = rect.top() + 8.0;
    let y = if avoid.right() > left - 6.0 {
        avoid.bottom() + 6.0
    } else {
        top
    };
    egui::Area::new(ui.id().with("canvas_view_buttons"))
        .fixed_pos(egui::pos2(left, y))
        .order(egui::Order::Foreground)
        .show(ui.ctx(), |ui| {
            ui.vertical(|ui| {
                let (label, other) = match view.mode {
                    SketchMode::Storey2D => ("2D", SketchMode::Storey3D),
                    SketchMode::Storey3D => ("3D", SketchMode::Storey2D),
                };
                // The gate is on ENTERING 3D only — going back to 2D is
                // always allowed (otherwise switching onto a fresh storey
                // while in 3D would trap the user there). Mirrors the
                // controller guard, which only refuses Storey3D.
                let enabled = matches!(view.mode, SketchMode::Storey3D) || view.ceiling_heights_set;
                if ui
                    .add_enabled(enabled, egui::Button::new(label))
                    .hint("Toggle between the 2D plan and the 3D view")
                    .disabled_hint("Set a default ceiling height (Heights) first")
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ChangeMode(other)));
                }
            });
        });
}

/// The floating "📷 Add external pictures" control, anchored to the canvas's
/// BOTTOM-RIGHT corner. A button toggles a panel showing the four sides
/// (Front / Right / Left / Back) as a two-column grid, each entry showing a
/// ✓ when it has a photo (with a thumbnail) or "missing" when it doesn't,
/// with an Add/Replace action that opens a
/// desktop file picker (`rfd`). The chosen path is emitted as
/// [`SketchEvent::AddExternalPhoto`]; the controller never sees a file dialog.
///
/// TODO(android-camera): the file picker is a DESKTOP stand-in. On Android the
/// Add/Replace action should instead launch the camera; it would emit the same
/// `AddExternalPhoto { side, path }` with the captured image's path, so nothing
/// downstream changes.
fn external_photos_button<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view: &SketchView<'_>,
    on_event: &mut F,
) {
    // Anchor by the bottom-right pivot so the panel grows UP-and-LEFT into the
    // canvas (mirror of the top-right `view_buttons`).
    egui::Area::new(ui.id().with("canvas_external_photos"))
        .order(egui::Order::Foreground)
        .pivot(egui::Align2::RIGHT_BOTTOM)
        .fixed_pos(rect.right_bottom() + egui::vec2(-8.0, -8.0))
        // Keep the whole panel inside the canvas — anchored bottom-right it grows
        // UP, so without this it can run off the top of the screen.
        .constrain_to(rect)
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.vertical(|ui| {
                    // Collapsing header doubles as the toggle button; the panel
                    // body is self-contained so no cross-frame page state is
                    // needed for open/closed.
                    egui::CollapsingHeader::new("📷 Add external pictures")
                        .default_open(false)
                        .show(ui, |ui| {
                            // Two-column grid of side entries (Front | Right,
                            // Left | Back), sized to FILL the canvas
                            // vertically when expanded: the two grid rows
                            // split the available canvas height (minus room
                            // for the header + frame chrome + the 8px anchor
                            // margins ≈ 70, and the inter-row grid spacing),
                            // and each thumbnail scales to fill its row after
                            // the label + button stack takes its share. On
                            // the ~2000px-wide tablet canvas that lands the
                            // thumbnails around 600x450 (4:3 kept). The panel
                            // is anchored bottom-right and grows UP-and-LEFT;
                            // constrain_to keeps it on-screen regardless.
                            let max_h = (rect.height() - 70.0).max(220.0);
                            let grid_spacing = egui::vec2(20.0, 16.0);
                            let row_h = (max_h - grid_spacing.y) / 2.0;
                            // ≈ status-label row + capture-button row + the
                            // item spacing around the thumbnail between them.
                            const ENTRY_TEXT_AND_BUTTONS_H: f32 = 62.0;
                            // Floor keeps entries usable on a tiny canvas —
                            // the ScrollArea then caps + scrolls instead.
                            let thumb_h = (row_h - ENTRY_TEXT_AND_BUTTONS_H).max(150.0);
                            let thumb_w = thumb_h * (4.0 / 3.0);
                            // Column fits the thumbnail (floored at 300 so the
                            // capture-button row never drives layout); the
                            // panel widens to match so the taller thumbnails
                            // aren't letterboxed by a narrow column.
                            let col_w = thumb_w.max(300.0);
                            ui.set_max_width(2.0 * col_w + grid_spacing.x + 16.0);
                            // The viewport height must be PINNED, not left to
                            // available space: an egui `Area` sizes its inner
                            // `Ui` from the PREVIOUS frame's content
                            // (`Area::content_ui` uses `state.rect()`), and
                            // while the header was collapsed that's
                            // header-sized — so a default ScrollArea
                            // (`outer = available.at_most(max_size)`, then
                            // floored to `min_scrolled_size` = 64) collapses
                            // to a 64px-tall viewport and, with vertical
                            // auto_shrink, STAYS there across frames (the
                            // area's size feeds back on itself; only the
                            // width — where scrolling is disabled — follows
                            // content). `min_scrolled_height(max_h)` floors
                            // the viewport independently of that stale
                            // available height, and `auto_shrink` = false
                            // vertically holds it at max_h even while entries
                            // are photo-less — so the expanded panel always
                            // fills the canvas, and the scrollbar only appears
                            // in the floored small-canvas case above.
                            egui::ScrollArea::vertical()
                                .max_height(max_h)
                                .min_scrolled_height(max_h)
                                .auto_shrink([true, false])
                                .show(ui, |ui| {
                                    egui::Grid::new(ui.id().with("external_photo_grid"))
                                        .num_columns(2)
                                        .min_col_width(col_w)
                                        .min_row_height(row_h)
                                        .spacing(grid_spacing)
                                        .show(ui, |ui| {
                                            for (i, entry) in
                                                view.external_photos.iter().enumerate()
                                            {
                                                external_photo_entry(
                                                    ui,
                                                    entry,
                                                    view.deferred_photo_sides.contains(&entry.0),
                                                    egui::vec2(thumb_w, thumb_h),
                                                    on_event,
                                                );
                                                // Two entries per grid row.
                                                if i % 2 == 1 {
                                                    ui.end_row();
                                                }
                                            }
                                        });
                                });
                        });
                });
            });
        });
}

/// One side's entry in the [`external_photos_button`] grid: status label,
/// thumbnail (when a photo is stored, scaled to `thumb_size` — computed by the
/// caller so the two grid rows fill the canvas height), and the capture /
/// annotate / rotate / remove button row — plus, on the deferrable sides, the
/// "Defer" toggle (`deferred` is its current state). Front never shows the
/// toggle: deferring exists for apartments where you can't tell which unit is
/// which from outside until you've been inside, but you always know the front
/// because you walk in through it (see `ExternalSide::photo_deferrable`).
fn external_photo_entry<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    (side, photo): &(
        ber_model::ber::common::ExternalSide,
        Option<crate::controller::app_controller::PhotoView>,
    ),
    deferred: bool,
    thumb_size: egui::Vec2,
    on_event: &mut F,
) {
    ui.vertical(|ui| {
        match photo {
            Some(_) => {
                ui.label(format!("✔ {}", side.label()));
            }
            None if deferred => {
                // Deferred = will be collected at the END of the assessment
                // (counts as set for the checklist meanwhile). Amber, not
                // red: it's an acknowledged postponement, not a gap.
                ui.colored_label(
                    egui::Color32::from_rgb(230, 190, 80),
                    format!("⏳ {}: deferred to end", side.label()),
                );
            }
            None if side.photo_required() => {
                ui.colored_label(
                    egui::Color32::from_rgb(220, 120, 120),
                    format!("✗ {}: missing", side.label()),
                );
            }
            None => {
                // Left / Right are optional — a neutral
                // prompt, no red "missing" nag.
                ui.weak(format!("○ {} (optional)", side.label()));
            }
        }
        // Thumbnail preview of the stored photo (prefer the
        // lightweight thumbnail; fall back to the original).
        if let Some(p) = photo {
            let src = p.thumbnail_path.as_deref().unwrap_or(&p.file_path);
            ui.add(
                egui::Image::new(format!("file://{src}"))
                    .fit_to_exact_size(thumb_size)
                    .maintain_aspect_ratio(true)
                    .corner_radius(4.0),
            );
        }
        ui.horizontal(|ui| {
            // The shared capture widget: desktop file dialog
            // / Android camera, returning the captured path.
            // Elevation shots want legible detail, so the hi
            // tier is High (TEMPORARY dual hi/lo buttons —
            // see `photo_button_pair`).
            let action = if photo.is_some() { "Replace" } else { "Add" };
            if let Some((path, quality)) = crate::ui::widgets::photo_button_pair(
                ui,
                action,
                true,
                crate::ui::widgets::PhotoQuality::High,
                false,
            ) {
                on_event(UiEvent::Sketch(SketchEvent::AddExternalPhoto {
                    side: *side,
                    path,
                    quality,
                }));
            }
            // Defer toggle: only while the side has NO photo (capturing
            // fulfils/clears a deferral) and only on deferrable sides —
            // never Front (see the fn doc for the apartment rationale).
            if photo.is_none() && side.photo_deferrable() {
                let label = if deferred { "Deferred ⏳" } else { "Defer" };
                if ui
                    .selectable_label(deferred, label)
                    .on_hover_text(
                        "Postpone this side's photo to the end of the assessment \
                         (you'll be prompted for it before the BER can complete)",
                    )
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ToggleDeferExternalPhoto {
                        side: *side,
                    }));
                }
            }
            if photo.is_some() {
                // Paintbrush annotation over the stored photo
                // (opens the takeover; see `photo_annotate`).
                if ui.button("🖌").on_hover_text("Draw on this photo").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::BeginAnnotateExternalPhoto {
                        side: *side,
                    }));
                }
                // Quarter-turn rotate so the shot
                // can be set upright / matched to
                // the plan (⟲ anti-clockwise, ⟳
                // clockwise).
                if ui.button("⟲").on_hover_text("Rotate left").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::RotateExternalPhoto {
                        side: *side,
                        clockwise: false,
                    }));
                }
                if ui.button("⟳").on_hover_text("Rotate right").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::RotateExternalPhoto {
                        side: *side,
                        clockwise: true,
                    }));
                }
                if ui.button("Remove").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::RemoveExternalPhoto {
                        side: *side,
                    }));
                }
            }
        });
    });
}

/// The current storey's screen↔world transform for the 2D canvas, or `None`
/// when there's no BER / storey / fittable bbox. `watermark = true` fits the
/// previous-storey watermark in too (used while capturing a new upper storey's
/// outline, matching what's drawn); `false` fits just the storey's own footprint
/// (the usual pointer hit-testing). Single source for the canvas's transform so
/// every interaction inverts the same mapping the renderer used.
fn canvas_transform(
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    rect: egui::Rect,
    view: ViewParams,
    watermark: bool,
) -> Option<render::Transform2d> {
    let b = ber?;
    let i = current_storey_idx?;
    let (min_xy, max_xy) = if watermark {
        render::fit_bbox_for_storey(b, i)?
    } else {
        b.storeys.get(i).and_then(render::storey_2d::bounding_box)?
    };
    Some(render::fit_transform(rect, min_xy, max_xy, view))
}

/// The transform the OVERLAID floor-exposure view is drawn with: fitted to the
/// union of every storey's floor bbox (so it matches
/// [`render::all_storeys_floors::render`]). Lets taps/strokes convert to world
/// coords against the same mapping the floors are drawn with.
/// A distinct fill tint per storey index for the roof-context overlay (cycles
/// through a small palette; higher storeys land on top so their colour shows at
/// each location = the top storey there).
fn roof_storey_tint(storey_idx: usize) -> (u8, u8, u8) {
    const PALETTE: &[(u8, u8, u8)] = &[
        (120, 170, 230), // storey 1 — blue
        (235, 175, 90),  // storey 2 — amber
        (140, 200, 140), // storey 3 — green
        (210, 140, 200), // storey 4 — mauve
        (230, 130, 120), // storey 5 — terracotta
    ];
    PALETTE[storey_idx % PALETTE.len()]
}

fn floor_exposure_transform(
    ber: Option<&Ber>,
    rect: egui::Rect,
    view: ViewParams,
) -> Option<render::Transform2d> {
    let b = ber?;
    let bb = b
        .storeys
        .iter()
        .filter_map(render::storey_2d::bounding_box)
        .fold(None, |acc, x| render::union_bbox(acc, Some(x)))?;
    Some(render::fit_transform(rect, bb.0, bb.1, view))
}

/// Shade the two candidate halves of a pending floor/ceiling-exposure straight
/// CUT over the overlaid all-storeys view, in the SAME footprint-union transform
/// the floors/ceilings are drawn with, so the assessor taps the side they want.
/// The two halves take distinct tints (amber / teal) for clarity. No-op when no
/// cut is pending (empty slice).
fn shade_exposure_cut_candidates(
    painter: &egui::Painter,
    rect: egui::Rect,
    ber: &Ber,
    vp: ViewParams,
    regions: &[Vec<(f64, f64)>],
) {
    if regions.is_empty() {
        return;
    }
    let Some(t) = floor_exposure_transform(Some(ber), rect, vp) else {
        return;
    };
    // Mirror the Structure-phase cut pick (`render::render_cut_candidates`): each
    // half a thick DISTINCT-colour outline + a "tap ①/②" label at its centroid,
    // so the two pickable sides read clearly. (Per-edge line segments rather than
    // a polygon FILL — a cut half can be concave, which `convex_polygon` would
    // mis-fill; outlines are concave-safe and match the other cut UIs.)
    let colors = [
        egui::Color32::from_rgb(255, 180, 40), // amber
        egui::Color32::from_rgb(40, 190, 255), // cyan
    ];
    let labels = ["tap ①", "tap ②"];
    for (i, ring) in regions.iter().enumerate() {
        if ring.len() < 3 {
            continue;
        }
        let pts: Vec<egui::Pos2> = ring.iter().map(|(x, y)| t.apply(*x, *y)).collect();
        let color = colors[i % colors.len()];
        let n = pts.len();
        for k in 0..n {
            painter.line_segment([pts[k], pts[(k + 1) % n]], egui::Stroke::new(4.0, color));
        }
        // Label at the centroid — the two centroids sit on opposite sides of the
        // cut, so they disambiguate the shared boundary.
        let (mut cx, mut cy) = (0.0f32, 0.0f32);
        for p in &pts {
            cx += p.x;
            cy += p.y;
        }
        let c = egui::pos2(cx / n as f32, cy / n as f32);
        painter.text(
            c,
            egui::Align2::CENTER_CENTER,
            labels[i % labels.len()],
            egui::FontId::proportional(15.0),
            color,
        );
    }
    painter.text(
        rect.center_top() + egui::vec2(0.0, 8.0),
        egui::Align2::CENTER_TOP,
        "Tap the side you want to set",
        egui::FontId::proportional(14.0),
        egui::Color32::from_rgb(230, 160, 60),
    );
}

/// World coords of a canvas-pixel position via the SAME transform the storey is
/// drawn with. `watermark = true` because the render fits the previous-storey
/// watermark in on any upper storey (`fit_bbox_for_storey`); inverting through
/// the storey-alone box instead would offset every tap on a setback upper storey
/// (the ground storey has no watermark, so the two agree there).
fn screen_to_world(
    ber: Option<&Ber>,
    current_storey_idx: Option<usize>,
    rect: egui::Rect,
    view: ViewParams,
    pos: egui::Pos2,
) -> Option<(f64, f64)> {
    canvas_transform(ber, current_storey_idx, rect, view, true).map(|t| t.inverse(pos.x, pos.y))
}

fn draw_stroke(painter: &egui::Painter, rect: egui::Rect, stroke: &EguiStroke) {
    if stroke.points.is_empty() {
        return;
    }

    let points: Vec<egui::Pos2> = stroke
        .points
        .iter()
        .map(|p| egui::pos2(rect.left() + p.x, rect.top() + p.y))
        .collect();

    if points.len() >= 2 {
        painter.add(PathShape::line(points.clone(), stroke.stroke.clone()));
    }

    // Endpoint markers — visible snap targets for continuing the stroke on the
    // next touch (handled by Storey2DPerpWalls::start_stroke).
    let snap_color = egui::Color32::from_rgb(255, 200, 60);
    let first = *points.first().expect("non-empty checked above");
    painter.circle_filled(first, 6.0, snap_color);
    if points.len() >= 2 {
        let last = *points.last().expect("non-empty checked above");
        painter.circle_filled(last, 6.0, snap_color);
    }
}

/// Render the per-wall ELEVATION editor: the unrolled wall (base line + roof
/// top profile) with each opening as a draggable rectangle. Drags reposition
/// (horizontal → along, vertical → sill); sizes are fixed. The panel owns its
/// own primary-button input, emitting `Elevation*` events.
fn elevation_overlay<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    elev: &crate::controller::app_controller::ElevationView,
    vp: render::ViewParams,
    on_event: &mut F,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(24));

    let len = (elev.wall_len_m as f64).max(0.1);
    let base = elev.base_z as f64;
    let top_max = elev
        .top_profile
        .iter()
        .map(|(_, z)| *z as f64)
        .fold(base + 0.5, f64::max);
    // World frame: x = along (0..len), y = -z (so height grows UP on screen),
    // with a small margin so the wall sits off the panel edges.
    let margin = (top_max - base).max(0.5) * 0.12;
    let t = render::fit_transform(
        rect,
        (-len * 0.05, -(top_max + margin)),
        (len * 1.05, -(base - margin)),
        vp,
    );
    let p = |along: f64, z: f64| t.apply(along, -z);
    let wall_col = egui::Color32::from_gray(150);
    let line = |a: egui::Pos2, b: egui::Pos2, w: f32, c: egui::Color32| {
        painter.line_segment([a, b], egui::Stroke::new(w, c));
    };
    // Base, top profile, and the two side edges connecting them.
    line(p(0.0, base), p(len, base), 2.0, wall_col);
    if elev.top_profile.len() >= 2 {
        for w in elev.top_profile.windows(2) {
            line(
                p(w[0].0 as f64, w[0].1 as f64),
                p(w[1].0 as f64, w[1].1 as f64),
                2.0,
                wall_col,
            );
        }
        // The profile runs b→a (along len→0); join the base ends to it.
        let first = elev.top_profile.first().unwrap();
        let last = elev.top_profile.last().unwrap();
        line(
            p(len, base),
            p(first.0 as f64, first.1 as f64),
            2.0,
            wall_col,
        );
        line(p(0.0, base), p(last.0 as f64, last.1 as f64), 2.0, wall_col);
    } else {
        line(p(0.0, base), p(0.0, top_max), 2.0, wall_col);
        line(p(len, base), p(len, top_max), 2.0, wall_col);
        line(p(0.0, top_max), p(len, top_max), 2.0, wall_col);
    }
    // Openings as filled shapes with a (heavier when selected) border — a
    // rectangle, or a half-ellipse arch for a semicircular transom.
    for o in &elev.openings {
        let l = (o.along_m - o.width_m / 2.0) as f64;
        let r = (o.along_m + o.width_m / 2.0) as f64;
        let b = base + o.sill_m as f64;
        let tp = b + o.height_m as f64;
        let (fill, stroke) = elevation_colors(o.kind, o.selected);
        let sw = if o.selected { 2.5 } else { 1.5 };
        draw_elevation_opening(&painter, &p, l, r, b, tp, o.shape, fill, stroke, sw);
    }
    painter.text(
        rect.left_top() + egui::vec2(8.0, 8.0),
        egui::Align2::LEFT_TOP,
        "Elevation — drag openings to reposition · tap a window, then Edit to change its size/type. Done in the toolbar.",
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(160),
    );
    // "Edit window…" for the selected STANDALONE window (doors + assembly
    // members keep their placement flow) — opens the attribute box prefilled.
    let window_selected = elev.openings.iter().any(|o| {
        o.selected
            && matches!(
                o.kind,
                crate::controller::app_controller::OpeningViewKind::Window
            )
    });
    if window_selected {
        egui::Area::new(egui::Id::new("ber_elevation_edit_window"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.left_top() + egui::vec2(8.0, 30.0))
            .show(ui.ctx(), |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    if ui
                        .button("Edit window…")
                        .hint("Change this window's size, glazing type, or overshading")
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::EditSelectedOpening));
                    }
                });
            });
    }
    // Primary input → wall-local (along, height-above-base).
    let to_local = |pos: egui::Pos2| {
        let (wx, wy) = t.inverse(pos.x, pos.y);
        (wx as f32, ((-wy) - base) as f32)
    };
    let emit_down = |on_event: &mut F, pos: egui::Pos2| {
        let (a, h) = to_local(pos);
        on_event(UiEvent::Sketch(SketchEvent::ElevationPointerDown {
            along_m: a,
            height_m: h,
        }));
    };
    if response.drag_started_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            emit_down(on_event, pos);
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            let (a, h) = to_local(pos);
            on_event(UiEvent::Sketch(SketchEvent::ElevationPointerDrag {
                along_m: a,
                height_m: h,
            }));
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        on_event(UiEvent::Sketch(SketchEvent::ElevationPointerUp));
    }
    if response.clicked_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            emit_down(on_event, pos);
        }
    }
}

/// Draw one opening in an elevation, given its bounds in the elevation `(x, z)`
/// frame and a mapping `p(x, z) → screen`. Rectangles get a crisp box; every
/// other shape is filled + outlined from the shared [`window_outline`] silhouette
/// (so it matches the derived 3D `vertices` exactly). Shared by BOTH the
/// edit-on-surface and Mapping review elevations.
#[allow(clippy::too_many_arguments)]
fn draw_elevation_opening(
    painter: &egui::Painter,
    p: &impl Fn(f64, f64) -> egui::Pos2,
    l: f64,
    r: f64,
    b: f64,
    tp: f64,
    shape: ber_model::ber::fenestration::WindowShape,
    fill: egui::Color32,
    stroke: egui::Color32,
    sw: f32,
) {
    use ber_model::ber::fenestration::{window_outline, WindowShape};
    let line =
        |a: egui::Pos2, c: egui::Pos2| painter.line_segment([a, c], egui::Stroke::new(sw, stroke));
    let (w, h) = (r - l, tp - b);
    if matches!(shape, WindowShape::Rectangular) {
        let ro = egui::Rect::from_two_pos(p(l, b), p(r, tp));
        painter.rect_filled(ro, 1.0, fill);
        line(ro.left_top(), ro.right_top());
        line(ro.right_top(), ro.right_bottom());
        line(ro.right_bottom(), ro.left_bottom());
        line(ro.left_bottom(), ro.left_top());
        return;
    }
    // The tessellated silhouette (local metres → elevation x/z → screen).
    let pts: Vec<egui::Pos2> = window_outline(shape, w as f32, h as f32)
        .iter()
        .map(|&(u, v)| p(l + u as f64, b + v as f64))
        .collect();
    // Fill: convex shapes as one polygon; the (concave) T as its two rects.
    if let WindowShape::TShape { band_m, stem_w_m } = shape {
        let stem_h = (h - band_m as f64).max(0.0);
        let (x0, x1) = (
            l + (w - stem_w_m as f64) / 2.0,
            l + (w + stem_w_m as f64) / 2.0,
        );
        painter.rect_filled(
            egui::Rect::from_two_pos(p(l, b + stem_h), p(r, tp)),
            1.0,
            fill,
        );
        painter.rect_filled(
            egui::Rect::from_two_pos(p(x0, b), p(x1, b + stem_h)),
            1.0,
            fill,
        );
    } else {
        painter.add(egui::Shape::convex_polygon(
            pts.clone(),
            fill,
            egui::Stroke::NONE,
        ));
    }
    for wnd in pts.windows(2) {
        line(wnd[0], wnd[1]);
    }
    if let (Some(&first), Some(&last)) = (pts.first(), pts.last()) {
        line(last, first);
    }
}

fn elevation_colors(
    kind: crate::controller::app_controller::OpeningViewKind,
    selected: bool,
) -> (egui::Color32, egui::Color32) {
    use crate::controller::app_controller::OpeningViewKind::*;
    let base = match kind {
        Window | RoofWindow | SidelightWindow => egui::Color32::from_rgb(90, 160, 220),
        Door | DoorPanel => egui::Color32::from_rgb(180, 130, 80),
    };
    let a = if selected { 130 } else { 70 };
    let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a);
    let stroke = if selected { egui::Color32::WHITE } else { base };
    (fill, stroke)
}

/// The Stage-1 (Mapping) REVIEW takeover: the current side's external elevation
/// beside its external photo (stacked in portrait, side-by-side in landscape),
/// with a top control bar (scroll the four sides / confirm "all's good"). Drag
/// an opening to reposition it (the controller constrains it to its wall). Owns
/// its own input — emits review pointer + nav events.
/// The open room's objects as a small dropdown list anchored just below its pin
/// (`sp` = the pin's screen position). Each row can be removed; a "Done" closes
/// the room. New objects added via the top bar appear here.
fn object_pin_dropdown<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    sp: egui::Pos2,
    room: &crate::controller::app_controller::ActiveObjectRoomView,
    on_event: &mut F,
) {
    let title = if room.name.trim().is_empty() {
        "Room".to_string()
    } else {
        room.name.clone()
    };
    egui::Area::new(egui::Id::new("ber_object_pin_dropdown"))
        .order(egui::Order::Foreground)
        .fixed_pos(sp + egui::vec2(14.0, 10.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_max_width(220.0);
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&title).strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Done").clicked() {
                            on_event(UiEvent::Sketch(SketchEvent::CloseObjectRoom));
                        }
                    });
                });
                if room.objects.is_empty() {
                    ui.label(
                        egui::RichText::new("Pick a category above to add.")
                            .italics()
                            .color(egui::Color32::from_gray(150)),
                    );
                } else {
                    for o in &room.objects {
                        ui.horizontal(|ui| {
                            ui.label(format!("{}  ×{}", o.label, o.count));
                            if o.photo_count > 0 {
                                ui.label(
                                    egui::RichText::new(format!("📷{}", o.photo_count))
                                        .color(egui::Color32::from_gray(150)),
                                );
                            }
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    if ui.small_button("✕").on_hover_text("Remove").clicked() {
                                        on_event(UiEvent::Sketch(SketchEvent::DeleteObject {
                                            room: room.id,
                                            object: o.id,
                                        }));
                                    }
                                },
                            );
                        });
                    }
                }
            });
        });
}

/// The phase signpost (Mapping → … → Final) + its per-phase checklist/action
/// button. Drawn in every phase — including the Final takeover, which is why it's
/// factored out. Returns the bar's rect (for laying the view buttons beside it).
fn phase_signpost<F: FnMut(UiEvent)>(
    ui: &egui::Ui,
    rect: egui::Rect,
    view: &SketchView<'_>,
    on_event: &mut F,
) -> egui::Rect {
    let (show_review, reviewed) = match view.current_phase {
        AssessmentPhase::Mapping => (view.mapping_complete, view.mapping_reviewed),
        AssessmentPhase::Structure => (view.structure_complete, view.structure_reviewed),
        AssessmentPhase::Objects => (view.objects_present, view.objects_reviewed),
        AssessmentPhase::Final => (view.final_complete, view.details_reviewed),
    };
    let review_label = match view.current_phase {
        AssessmentPhase::Final => {
            if view.details_reviewed {
                "BER complete ✔"
            } else {
                "Mark BER complete"
            }
        }
        _ if reviewed => "Re-review ▸",
        _ => "Review ▸",
    };
    let (phase_clicked, review_clicked, phase_bar_rect) = super::overlays::phase_bar(
        ui,
        rect,
        view.current_phase,
        view.mapping_reviewed,
        view.structure_reviewed,
        view.objects_reviewed,
        view.details_reviewed,
        &view.checklist,
        show_review,
        review_label,
    );
    if let Some(phase) = phase_clicked {
        on_event(UiEvent::Sketch(SketchEvent::SetAssessmentPhase(phase)));
    }
    if review_clicked {
        let ev = match view.current_phase {
            AssessmentPhase::Structure => SketchEvent::OpenStructureReview,
            AssessmentPhase::Objects => SketchEvent::OpenObjectReview,
            AssessmentPhase::Final => SketchEvent::ConfirmFinalReview,
            _ => SketchEvent::OpenMappingReview,
        };
        on_event(UiEvent::Sketch(ev));
    }
    phase_bar_rect
}

/// Phase-4 (Final) full-phase view: details + mode controls on one side; the
/// reduced plan over the (movable) satellite image on the other.
fn final_overlay<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    view: &SketchView<'_>,
    ber: &ber_model::ber::ber_struct::Ber,
    vp: render::ViewParams,
    on_event: &mut F,
) {
    use crate::controller::app_controller::FinalMode;
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(20));

    // Reserve a strip at the top for the phase signpost (drawn on top by the
    // caller) so the details/plan don't sit under it.
    let rect = egui::Rect::from_min_max(rect.left_top() + egui::vec2(0.0, 64.0), rect.max);

    // Details on top (portrait) / left (landscape); plan on the other side. The
    // plan keeps roughly the Phase-3 footprint share.
    let landscape = rect.width() > rect.height();
    let (details_rect, plan_rect) = if landscape {
        let mid = rect.left() + rect.width() * 0.38;
        (
            egui::Rect::from_min_max(rect.min, egui::pos2(mid, rect.bottom())),
            egui::Rect::from_min_max(egui::pos2(mid, rect.top()), rect.max),
        )
    } else {
        let mid = rect.top() + rect.height() * 0.42;
        (
            egui::Rect::from_min_max(rect.min, egui::pos2(rect.right(), mid)),
            egui::Rect::from_min_max(egui::pos2(rect.left(), mid), rect.max),
        )
    };

    // ── Plan side ──
    painter.rect_filled(plan_rect, 4.0, egui::Color32::from_gray(28));
    // Satellite image BEHIND the plan, transformed (only while aligning).
    if let Some(img) = view.north_image.as_ref() {
        let base = (plan_rect.size().min_elem() * 0.7 * img.zoom).max(20.0);
        let center = plan_rect.center() + egui::vec2(img.offset.0, img.offset.1);
        let img_rect = egui::Rect::from_center_size(center, egui::vec2(base, base));
        let b = egui::UiBuilder::new().max_rect(plan_rect);
        ui.scope_builder(b, |ui| {
            ui.set_clip_rect(plan_rect);
            egui::Image::new(format!("file://{}", img.path))
                .rotate(img.rotation_deg.to_radians(), egui::vec2(0.5, 0.5))
                .tint(egui::Color32::from_white_alpha(180))
                .show_loading_spinner(false)
                .paint_at(ui, img_rect);
        });
    }
    // The reduced plan (walls + wall-windows + floors), clipped to the plan side.
    let plan_painter = ui.painter_at(plan_rect);
    render::render_ber(
        &plan_painter,
        plan_rect,
        Some(ber),
        view.current_storey_idx,
        SketchMode::Storey2D,
        domain::sketch::DisplayMode::AllSurfaces,
        None,
        &[],
        vp,
        &std::collections::HashSet::new(),
        None,
        true, // objects_view: reduced plan
        &[],
    );

    let resp = ui.interact(
        plan_rect,
        ui.id().with("ber_final_plan"),
        egui::Sense::click_and_drag(),
    );

    match view.final_mode {
        FinalMode::Align => {
            // Move / rotate / zoom the image (two-finger or left/right-drag/scroll).
            let mt = ui.input(|i| i.multi_touch());
            if let Some(mt) = mt {
                on_event(UiEvent::Sketch(SketchEvent::NorthImageGesture {
                    pan_dx: mt.translation_delta.x,
                    pan_dy: mt.translation_delta.y,
                    zoom_factor: mt.zoom_delta,
                    rotate_deg: mt.rotation_delta.to_degrees(),
                }));
            } else {
                let mut pan = egui::vec2(0.0, 0.0);
                let mut rotate_deg = 0.0;
                let mut zoom_factor = 1.0;
                if resp.dragged_by(egui::PointerButton::Primary) {
                    pan = resp.drag_delta();
                }
                if resp.dragged_by(egui::PointerButton::Secondary) {
                    rotate_deg = resp.drag_delta().x * 0.4;
                }
                if resp.hovered() {
                    let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                    if scroll.abs() > 0.0 {
                        zoom_factor = (scroll * 0.002).exp();
                    }
                }
                if pan != egui::vec2(0.0, 0.0) || rotate_deg != 0.0 || zoom_factor != 1.0 {
                    on_event(UiEvent::Sketch(SketchEvent::NorthImageGesture {
                        pan_dx: pan.x,
                        pan_dy: pan.y,
                        zoom_factor,
                        rotate_deg,
                    }));
                }
            }
            // North arrow.
            let dir = egui::Vec2::angled((view.orientation_deg - 90.0).to_radians());
            let o = plan_rect.right_top() + egui::vec2(-30.0, 30.0);
            painter.arrow(o, dir * 22.0, egui::Stroke::new(3.0, egui::Color32::RED));
            painter.text(
                o + dir * 30.0,
                egui::Align2::CENTER_CENTER,
                "N",
                egui::FontId::proportional(13.0),
                egui::Color32::RED,
            );
        }
        FinalMode::Sheltered => {
            use ber_model::ber::common::ExternalSide;
            // Coloured bands on the four edges; tap a side to cycle it.
            let band = 18.0;
            let edges = [
                (
                    ExternalSide::Back,
                    egui::Rect::from_min_max(
                        plan_rect.left_top(),
                        egui::pos2(plan_rect.right(), plan_rect.top() + band),
                    ),
                ),
                (
                    ExternalSide::Front,
                    egui::Rect::from_min_max(
                        egui::pos2(plan_rect.left(), plan_rect.bottom() - band),
                        plan_rect.right_bottom(),
                    ),
                ),
                (
                    ExternalSide::Left,
                    egui::Rect::from_min_max(
                        plan_rect.left_top(),
                        egui::pos2(plan_rect.left() + band, plan_rect.bottom()),
                    ),
                ),
                (
                    ExternalSide::Right,
                    egui::Rect::from_min_max(
                        egui::pos2(plan_rect.right() - band, plan_rect.top()),
                        plan_rect.right_bottom(),
                    ),
                ),
            ];
            for (side, er) in edges {
                let c = shelter_color(view.sheltering.get(side));
                painter.rect_filled(er, 0.0, c);
                painter.text(
                    er.center(),
                    egui::Align2::CENTER_CENTER,
                    side.label(),
                    egui::FontId::proportional(11.0),
                    egui::Color32::WHITE,
                );
            }
            if resp.clicked() {
                if let Some(p) = resp.interact_pointer_pos() {
                    let d = p - plan_rect.center();
                    let side = if d.x.abs() > d.y.abs() {
                        if d.x > 0.0 {
                            ExternalSide::Right
                        } else {
                            ExternalSide::Left
                        }
                    } else if d.y > 0.0 {
                        ExternalSide::Front
                    } else {
                        ExternalSide::Back
                    };
                    on_event(UiEvent::Sketch(SketchEvent::CycleShelter(side)));
                }
            }
        }
        FinalMode::Form => {}
    }

    // ── Details side ──
    let inner = egui::Rect::from_min_max(
        details_rect.left_top() + egui::vec2(10.0, 8.0),
        details_rect.right_bottom() - egui::vec2(10.0, 8.0),
    );
    let b = egui::UiBuilder::new()
        .max_rect(inner)
        .layout(egui::Layout::top_down(egui::Align::Min));
    ui.scope_builder(b, |ui| {
        ui.set_max_width(inner.width());
        egui::ScrollArea::vertical().show(ui, |ui| {
            super::final_panel::details_panel(ui, view, on_event);
        });
    });
}

/// Colour for a shelter category band.
fn shelter_color(c: ber_model::ber::final_details::ShelterCategory) -> egui::Color32 {
    use ber_model::ber::final_details::ShelterCategory::*;
    match c {
        Sheltered => egui::Color32::from_rgb(90, 190, 110),
        HalfSheltered => egui::Color32::from_rgb(225, 190, 70),
        Unsheltered => egui::Color32::from_rgb(220, 110, 90),
        Unset => egui::Color32::from_gray(90),
    }
}

fn object_review_overlay<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    orv: &crate::controller::app_controller::ObjectReviewView,
    ber: &ber_model::ber::ber_struct::Ber,
    vp: render::ViewParams,
    on_event: &mut F,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(20));

    let bar_h = 38.0;
    egui::Area::new(ui.id().with("object_review_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_top() + egui::vec2(8.0, 6.0))
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Objects — house totals")
                        .strong()
                        .size(15.0),
                );
                ui.separator();
                // Confirm is blocked while a category flagged "there is one"
                // still has no recorded object (the controller refuses too).
                let confirmable = orv.unresolved_present.is_empty();
                if ui
                    .add_enabled(
                        confirmable,
                        egui::Button::new(egui::RichText::new("✔ All reviewed & OK").strong()),
                    )
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ConfirmObjectReview));
                }
                if ui.button("Close").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::CloseObjectReview));
                }
            });
        });

    let content = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 4.0, rect.top() + bar_h),
        rect.right_bottom() - egui::vec2(4.0, 4.0),
    );
    let portrait = content.height() >= content.width();
    let (list_rect, three_d_rect) = if portrait {
        let mid = content.top() + content.height() * 0.4;
        (
            egui::Rect::from_min_max(content.min, egui::pos2(content.right(), mid)),
            egui::Rect::from_min_max(egui::pos2(content.left(), mid), content.max),
        )
    } else {
        let mid = content.left() + content.width() * 0.36;
        (
            egui::Rect::from_min_max(content.min, egui::pos2(mid, content.bottom())),
            egui::Rect::from_min_max(egui::pos2(mid, content.top()), content.max),
        )
    };

    // ── 3D house with the selected kind's hotspots ──
    painter.rect_filled(three_d_rect, 4.0, egui::Color32::from_gray(28));
    let three_d_painter = ui.painter_at(three_d_rect);
    render::full_house_3d::render(
        &three_d_painter,
        three_d_rect,
        ber,
        domain::sketch::DisplayMode::AllSurfaces,
        vp,
        &std::collections::HashSet::new(),
        &orv.hotspots,
    );
    painter.text(
        three_d_rect.right_bottom() - egui::vec2(8.0, 6.0),
        egui::Align2::RIGHT_BOTTOM,
        "drag to orbit",
        egui::FontId::proportional(11.0),
        egui::Color32::from_gray(110),
    );
    if response.dragged_by(egui::PointerButton::Primary) {
        let over_3d = ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| three_d_rect.contains(p));
        if over_3d {
            const ORBIT_DEG_PER_PIXEL: f32 = 0.4;
            let d = response.drag_delta();
            on_event(UiEvent::Sketch(SketchEvent::OrbitView {
                dyaw: -d.x * ORBIT_DEG_PER_PIXEL,
                dpitch: d.y * ORBIT_DEG_PER_PIXEL,
            }));
        }
    }

    // ── The flat totals list (click a row to locate it) ──
    let inner = egui::Rect::from_min_max(
        list_rect.left_top() + egui::vec2(10.0, 8.0),
        list_rect.right_bottom() - egui::vec2(10.0, 8.0),
    );
    let builder = egui::UiBuilder::new()
        .max_rect(inner)
        .layout(egui::Layout::top_down(egui::Align::Min));
    ui.scope_builder(builder, |ui| {
        ui.set_max_width(inner.width());
        ui.label(
            egui::RichText::new("Click an object to see where it is")
                .color(egui::Color32::from_gray(150))
                .size(12.0),
        );
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .max_height(inner.height() - 70.0)
            .show(ui, |ui| {
                for (i, row) in orv.totals.iter().enumerate() {
                    let text = format!("{}  ·  {}", row.count, row.label);
                    if ui.selectable_label(row.selected, text).clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::SelectObjectReviewKind(i)));
                    }
                }
            });

        // Absence confirmation, one empty category at a time ("No cylinder?").
        if let Some(cat) = orv.absent_prompt {
            ui.separator();
            ui.label(
                egui::RichText::new(format!("No {}?", cat.label().to_lowercase()))
                    .strong()
                    .size(15.0),
            );
            ui.horizontal(|ui| {
                if ui.button("Yes — none").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::ConfirmObjectAbsent(cat)));
                }
                if ui.button("No — there is one").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::SkipObjectAbsent(cat)));
                }
            });
            ui.label(
                egui::RichText::new(format!("{} left to confirm", orv.absent_remaining))
                    .size(11.0)
                    .color(egui::Color32::from_gray(150)),
            );
        } else {
            ui.separator();
            ui.label(
                egui::RichText::new("All empty categories confirmed ✔")
                    .color(egui::Color32::from_gray(150)),
            );
        }
        // Flagged-present categories with no object yet: confirm stays
        // disabled until each promised item is actually added.
        if !orv.unresolved_present.is_empty() {
            let names = orv
                .unresolved_present
                .iter()
                .map(|c| c.label().to_lowercase())
                .collect::<Vec<_>>()
                .join(", ");
            ui.label(
                egui::RichText::new(format!("Add the {names} you flagged before finishing"))
                    .strong()
                    .color(egui::Color32::from_rgb(220, 160, 80)),
            );
        }
    });
}

#[allow(clippy::too_many_arguments)]
fn structure_review_overlay<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    srv: &crate::controller::app_controller::StructureReviewView,
    ber: &ber_model::ber::ber_struct::Ber,
    vp: render::ViewParams,
    on_event: &mut F,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(20));

    // Top control bar (its own Area so its buttons own their clicks).
    let bar_h = 38.0;
    egui::Area::new(ui.id().with("structure_review_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_top() + egui::vec2(8.0, 6.0))
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("◀ Prev").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::StepStructureReview(-1)));
                }
                ui.label(
                    egui::RichText::new(format!("{} / {}", srv.cursor + 1, srv.total))
                        .strong()
                        .size(15.0),
                );
                if ui.button("Next ▶").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::StepStructureReview(1)));
                }
                ui.separator();
                if ui
                    .button(egui::RichText::new("✔ All looks good").strong())
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ConfirmStructureReview));
                }
                if ui.button("Close").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::CloseStructureReview));
                }
            });
        });

    // Content area below the bar, split details | 3D by orientation.
    let content = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 4.0, rect.top() + bar_h),
        rect.right_bottom() - egui::vec2(4.0, 4.0),
    );
    let portrait = content.height() >= content.width();
    let (details_rect, three_d_rect) = if portrait {
        let mid = content.top() + content.height() * 0.42;
        (
            egui::Rect::from_min_max(content.min, egui::pos2(content.right(), mid)),
            egui::Rect::from_min_max(egui::pos2(content.left(), mid), content.max),
        )
    } else {
        let mid = content.left() + content.width() * 0.4;
        (
            egui::Rect::from_min_max(content.min, egui::pos2(mid, content.bottom())),
            egui::Rect::from_min_max(egui::pos2(mid, content.top()), content.max),
        )
    };

    // ── 3D panel: the whole house with this group's surfaces shaded ──
    painter.rect_filled(three_d_rect, 4.0, egui::Color32::from_gray(28));
    let three_d_painter = ui.painter_at(three_d_rect);
    render::full_house_3d::render(
        &three_d_painter,
        three_d_rect,
        ber,
        domain::sketch::DisplayMode::AllSurfaces,
        vp,
        &srv.highlight,
        &[],
    );
    painter.text(
        three_d_rect.right_bottom() - egui::vec2(8.0, 6.0),
        egui::Align2::RIGHT_BOTTOM,
        "drag to orbit",
        egui::FontId::proportional(11.0),
        egui::Color32::from_gray(110),
    );
    // Primary-drag over the 3D side orbits the camera (same feel as the 3D view).
    if response.dragged_by(egui::PointerButton::Primary) {
        let over_3d = ui
            .input(|i| i.pointer.interact_pos())
            .is_some_and(|p| three_d_rect.contains(p));
        if over_3d {
            const ORBIT_DEG_PER_PIXEL: f32 = 0.4;
            let d = response.drag_delta();
            on_event(UiEvent::Sketch(SketchEvent::OrbitView {
                dyaw: -d.x * ORBIT_DEG_PER_PIXEL,
                dpitch: d.y * ORBIT_DEG_PER_PIXEL,
            }));
        }
    }

    // ── Details panel: kind + title + attribute lines + photos ──
    let details_inner = egui::Rect::from_min_max(
        details_rect.left_top() + egui::vec2(10.0, 8.0),
        details_rect.right_bottom() - egui::vec2(10.0, 8.0),
    );
    let builder = egui::UiBuilder::new()
        .max_rect(details_inner)
        .layout(egui::Layout::top_down(egui::Align::Min));
    ui.scope_builder(builder, |ui| {
        ui.set_max_width(details_inner.width());
        egui::ScrollArea::vertical()
            .max_height(details_inner.height())
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new(srv.kind.label())
                        .color(egui::Color32::from_gray(160))
                        .size(13.0),
                );
                ui.label(egui::RichText::new(&srv.title).strong().size(20.0));
                ui.add_space(8.0);
                for (label, value) in &srv.detail_lines {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{label}:"))
                                .color(egui::Color32::from_gray(160)),
                        );
                        ui.label(egui::RichText::new(value).strong());
                    });
                }
                ui.add_space(10.0);
                if srv.photos.is_empty() {
                    ui.label(
                        egui::RichText::new("No photos")
                            .italics()
                            .color(egui::Color32::from_gray(120)),
                    );
                } else {
                    ui.label(egui::RichText::new("Photos").color(egui::Color32::from_gray(160)));
                    ui.horizontal_wrapped(|ui| {
                        for p in &srv.photos {
                            let src = p.thumbnail_path.as_deref().unwrap_or(&p.file_path);
                            ui.add(
                                egui::Image::new(format!("file://{src}"))
                                    .fit_to_exact_size(egui::vec2(120.0, 120.0))
                                    .maintain_aspect_ratio(true)
                                    .corner_radius(4.0)
                                    .show_loading_spinner(false),
                            );
                        }
                    });
                }
            });
    });
}

fn mapping_review_overlay<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    rv: &crate::controller::app_controller::ReviewView,
    vp: render::ViewParams,
    on_event: &mut F,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(20));

    // Top control bar (its own Area so its buttons own their clicks).
    let bar_h = 38.0;
    egui::Area::new(ui.id().with("mapping_review_bar"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.left_top() + egui::vec2(8.0, 6.0))
        .show(ui.ctx(), |ui| {
            ui.horizontal(|ui| {
                if ui.button("◀ Prev").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::StepMappingReviewSide(-1)));
                }
                ui.label(
                    egui::RichText::new(format!("Side: {}", rv.side.label()))
                        .strong()
                        .size(15.0),
                );
                if ui.button("Next ▶").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::StepMappingReviewSide(1)));
                }
                ui.separator();
                if ui
                    .button(egui::RichText::new("✔ All looks good").strong())
                    .clicked()
                {
                    on_event(UiEvent::Sketch(SketchEvent::ConfirmMappingReview));
                }
                if ui.button("Close").clicked() {
                    on_event(UiEvent::Sketch(SketchEvent::CloseMappingReview));
                }
            });
        });

    // Content area below the bar, split elevation | photo by orientation.
    let content = egui::Rect::from_min_max(
        egui::pos2(rect.left() + 4.0, rect.top() + bar_h),
        rect.right_bottom() - egui::vec2(4.0, 4.0),
    );
    let portrait = content.height() >= content.width();
    let (elev_rect, photo_rect) = if portrait {
        let mid = content.top() + content.height() * 0.55;
        (
            egui::Rect::from_min_max(content.min, egui::pos2(content.right(), mid)),
            egui::Rect::from_min_max(egui::pos2(content.left(), mid), content.max),
        )
    } else {
        let mid = content.left() + content.width() * 0.55;
        (
            egui::Rect::from_min_max(content.min, egui::pos2(mid, content.bottom())),
            egui::Rect::from_min_max(egui::pos2(mid, content.top()), content.max),
        )
    };

    // ── Photo panel ──
    painter.text(
        photo_rect.left_top() + egui::vec2(6.0, 4.0),
        egui::Align2::LEFT_TOP,
        "Photo",
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(160),
    );
    let photo_inner = egui::Rect::from_min_max(
        photo_rect.left_top() + egui::vec2(6.0, 24.0),
        photo_rect.right_bottom() - egui::vec2(6.0, 6.0),
    );
    match &rv.photo {
        Some(p) => {
            let src = p.thumbnail_path.as_deref().unwrap_or(&p.file_path);
            egui::Image::new(format!("file://{src}"))
                .maintain_aspect_ratio(true)
                .show_loading_spinner(false)
                .paint_at(ui, photo_inner);
            // Paintbrush annotation over the stored photo (its own Area so the
            // button owns its clicks over the painted image).
            egui::Area::new(ui.id().with("mapping_review_annotate"))
                .order(egui::Order::Foreground)
                .fixed_pos(photo_rect.left_top() + egui::vec2(52.0, 0.0))
                .show(ui.ctx(), |ui| {
                    if ui
                        .button("🖌 Annotate")
                        .on_hover_text("Draw on this photo")
                        .clicked()
                    {
                        on_event(UiEvent::Sketch(SketchEvent::BeginAnnotateExternalPhoto {
                            side: rv.side,
                        }));
                    }
                });
        }
        None => {
            painter.text(
                photo_inner.center(),
                egui::Align2::CENTER_CENTER,
                format!("No {} photo yet", rv.side.label()),
                egui::FontId::proportional(14.0),
                egui::Color32::from_gray(120),
            );
        }
    }

    // ── Elevation panel ──
    let elev = &rv.elevation;
    painter.text(
        elev_rect.left_top() + egui::vec2(6.0, 4.0),
        egui::Align2::LEFT_TOP,
        format!(
            "{} elevation — drag openings to reposition",
            rv.side.label()
        ),
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(160),
    );
    if elev.panels.is_empty() {
        painter.text(
            elev_rect.center(),
            egui::Align2::CENTER_CENTER,
            "No external walls face this side.",
            egui::FontId::proportional(14.0),
            egui::Color32::from_gray(120),
        );
        return;
    }

    // Fit the composite (x, z) bounds into the elevation sub-rect; world frame
    // is x = composite x, y = −z (height grows UP on screen).
    let draw_rect = egui::Rect::from_min_max(
        elev_rect.left_top() + egui::vec2(6.0, 24.0),
        elev_rect.right_bottom() - egui::vec2(6.0, 6.0),
    );
    let mx = (elev.max_x - elev.min_x).max(0.5) * 0.06;
    let mz = (elev.max_z - elev.min_z).max(0.5) * 0.10;
    let t = render::fit_transform(
        draw_rect,
        (elev.min_x - mx, -(elev.max_z + mz)),
        (elev.max_x + mx, -(elev.min_z - mz)),
        vp,
    );
    let p = |x: f64, z: f64| t.apply(x, -z);
    let wall_col = egui::Color32::from_gray(150);
    let line = |a: egui::Pos2, b: egui::Pos2, w: f32, c: egui::Color32| {
        painter.line_segment([a, b], egui::Stroke::new(w, c));
    };
    for panel in &elev.panels {
        // Base edge + the top polyline; close the two ends down to the base.
        line(
            p(panel.x0, panel.base_z),
            p(panel.x1, panel.base_z),
            2.0,
            wall_col,
        );
        for w in panel.top.windows(2) {
            line(p(w[0].0, w[0].1), p(w[1].0, w[1].1), 2.0, wall_col);
        }
        if let (Some(first), Some(last)) = (panel.top.first(), panel.top.last()) {
            line(p(first.0, first.1), p(first.0, panel.base_z), 2.0, wall_col);
            line(p(last.0, last.1), p(last.0, panel.base_z), 2.0, wall_col);
        }
    }
    for o in &elev.openings {
        let l = o.x_center - o.width_m / 2.0;
        let r = o.x_center + o.width_m / 2.0;
        let (b, tp) = (o.sill_z, o.sill_z + o.height_m);
        let selected = rv.selection == Some(o.opening);
        let base = if o.is_door {
            egui::Color32::from_rgb(180, 130, 80)
        } else {
            egui::Color32::from_rgb(90, 160, 220)
        };
        let a = if selected { 130 } else { 70 };
        let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), a);
        let (sw, sc) = if selected {
            (2.5, egui::Color32::WHITE)
        } else {
            (1.5, base)
        };
        draw_elevation_opening(&painter, &p, l, r, b, tp, o.shape, fill, sc, sw);
    }

    // Input: drag openings within the elevation sub-rect → composite (x, z).
    let to_comp = |pos: egui::Pos2| {
        let (wx, wy) = t.inverse(pos.x, pos.y);
        (wx, -wy)
    };
    let in_elev = |pos: egui::Pos2| elev_rect.contains(pos);
    if response.drag_started_by(egui::PointerButton::Primary)
        || response.clicked_by(egui::PointerButton::Primary)
    {
        if let Some(pos) = response.interact_pointer_pos() {
            if in_elev(pos) {
                let (x, z) = to_comp(pos);
                on_event(UiEvent::Sketch(SketchEvent::MappingReviewPointerDown {
                    x,
                    z,
                }));
            }
        }
    }
    if response.dragged_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            if in_elev(pos) {
                let (x, z) = to_comp(pos);
                on_event(UiEvent::Sketch(SketchEvent::MappingReviewPointerDrag {
                    x,
                    z,
                }));
            }
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        on_event(UiEvent::Sketch(SketchEvent::MappingReviewPointerUp));
    }
}

/// The base exposure-class colour for a wall partition region — the SAME
/// `WallExposure`→colour mapping the 2D plan exposure overlay uses
/// (`render::style`), so a face reads consistently in plan and partition views.
fn partition_exposure_color(e: WallExposure) -> egui::Color32 {
    use crate::ui::render::style;
    match e {
        WallExposure::Exposed => style::EXPOSED_WALL,
        WallExposure::SemiExposed => style::SEMI_EXPOSED_WALL,
        WallExposure::NonHeatLoss => style::NON_HEAT_LOSS_WALL,
        WallExposure::Ground => style::GROUND_WALL,
        WallExposure::Unset => style::UNSET_WALL,
        // UNREACHABLE in practice: partition face regions are assessor-painted
        // and every setter refuses InterStorey (it's the derivation's stamp,
        // and a partitioned wall's 2-D regions aren't derivation targets).
        // Kept exhaustive — grey, the ghost/unclassified read.
        WallExposure::InterStorey => style::UNSET_WALL,
    }
}

/// Render the wall-face PARTITION editor: the flat-top unrolled face with each
/// subdivided region tinted by its exposure, the committed/in-flight cuts, and a
/// little plan inset that orients the viewer. Owns its own primary-button input
/// — a drag draws a cut, a click selects the region under it — emitting
/// `Partition*` events in the canonical a→b (un-flipped) wall frame.
fn partition_overlay<F: FnMut(UiEvent)>(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    response: &egui::Response,
    pv: &crate::controller::app_controller::PartitionView,
    vp: render::ViewParams,
    on_event: &mut F,
) {
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 0.0, egui::Color32::from_gray(24));

    let len = (pv.wall_len_m as f64).max(0.1);
    let height = (pv.height_m as f64).max(0.1);
    // World frame: x = along (0..len), y = -z (height grows UP on screen), base
    // = 0. A small margin keeps the face off the panel edges.
    let margin = height.max(0.5) * 0.12;
    let t = render::fit_transform(
        rect,
        (-len * 0.05, -(height + margin)),
        (len * 1.05, margin),
        vp,
    );
    // `flip_along`: a→b runs right→left from inside, so mirror the along-axis at
    // the screen boundary (both forward draw and inverse hit-test) — emitted
    // events stay in the canonical a→b frame.
    let flip = pv.inset.flip_along;
    let p = |along: f64, z: f64| {
        let a = if flip { len - along } else { along };
        t.apply(a, -z)
    };
    let line = |a: egui::Pos2, b: egui::Pos2, w: f32, c: egui::Color32| {
        painter.line_segment([a, b], egui::Stroke::new(w, c));
    };

    // Tinted regions first (so cuts + the face frame draw on top).
    for r in &pv.regions {
        if r.poly.len() < 3 {
            continue;
        }
        let pts: Vec<egui::Pos2> = r
            .poly
            .iter()
            .map(|(a, z)| p(*a as f64, *z as f64))
            .collect();
        let base = partition_exposure_color(r.exposure);
        // Unset stays a faint grey so the user can see what's still unmarked;
        // the real exposure classes read as saturated tints (matching/exceeding
        // the elevation openings' selected alpha of 130).
        let alpha = if r.exposure == WallExposure::Unset {
            60
        } else {
            150
        };
        let fill = egui::Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha);
        painter.add(egui::Shape::convex_polygon(
            pts.clone(),
            fill,
            egui::Stroke::NONE,
        ));
        // Region border: brighter + heavier on the selected region.
        let (bw, bc) = if r.selected {
            (2.5, egui::Color32::WHITE)
        } else {
            (1.0, base)
        };
        painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(bw, bc)));
    }

    // The flat-top face frame: corners (0,0),(len,0),(len,height),(0,height).
    let wall_col = egui::Color32::from_gray(150);
    line(p(0.0, 0.0), p(len, 0.0), 2.0, wall_col);
    line(p(len, 0.0), p(len, height), 2.0, wall_col);
    line(p(len, height), p(0.0, height), 2.0, wall_col);
    line(p(0.0, height), p(0.0, 0.0), 2.0, wall_col);

    // Committed cuts solid; the in-flight pending cut highlighted (dashed).
    let cut_col = egui::Color32::from_rgb(220, 220, 120);
    for c in &pv.cuts {
        line(
            p(c[0].0 as f64, c[0].1 as f64),
            p(c[1].0 as f64, c[1].1 as f64),
            2.0,
            cut_col,
        );
    }
    if let Some(pc) = &pv.pending {
        let a = p(pc[0].0 as f64, pc[0].1 as f64);
        let b = p(pc[1].0 as f64, pc[1].1 as f64);
        let hi = egui::Color32::from_rgb(255, 210, 70);
        // Dashed: sample the segment so the pending cut reads apart from the
        // committed solid ones.
        let segs = 14;
        for i in (0..segs).step_by(2) {
            let f0 = i as f32 / segs as f32;
            let f1 = (i + 1) as f32 / segs as f32;
            line(a + (b - a) * f0, a + (b - a) * f1, 2.5, hi);
        }
    }

    // Snap tags: one per pending (or just-instant-committed) cut endpoint
    // with ≥1 silently-snapped coordinate. The dot matches the canvas'
    // `render_snap_tags` (orange fill, white ring) but is drawn here because
    // the tag lives on the face overlay's own transform (`p` handles the
    // along-axis flip). The menu offers exactly "Don't snap", routed to
    // UnsnapPartitionEnd — never ChooseSnap: the cut lives outside history.
    for tag in &pv.snap_tags {
        let sp = p(tag.pos.0 as f64, tag.pos.1 as f64);
        painter.circle_filled(sp, 5.0, egui::Color32::from_rgb(255, 140, 0));
        painter.circle_stroke(sp, 5.0, egui::Stroke::new(1.5, egui::Color32::WHITE));
        let label = if tag.corner { "corner ▾" } else { "edge ▾" };
        egui::Area::new(ui.id().with(("partition_snap_tag", tag.end_b)))
            .fixed_pos(sp + egui::vec2(7.0, -7.0))
            .order(egui::Order::Foreground)
            .show(ui.ctx(), |ui| {
                ui.menu_button(label, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "endpoint moved onto the face edge / an existing cut — \
                             un-snap reverts to the drawn point and asks for it",
                        )
                        .small()
                        .weak(),
                    );
                    ui.separator();
                    if ui.button("Don't snap").clicked() {
                        on_event(UiEvent::Sketch(SketchEvent::UnsnapPartitionEnd {
                            end_b: tag.end_b,
                        }));
                        ui.close();
                    }
                });
            });
    }

    // Live freehand ink: the in-flight stroke tracking the pen as it's drawn.
    if pv.stroke.len() >= 2 {
        let ink = egui::Color32::from_rgb(120, 200, 255);
        let pts: Vec<egui::Pos2> = pv
            .stroke
            .iter()
            .map(|(a, z)| p(*a as f64, *z as f64))
            .collect();
        painter.add(egui::Shape::line(pts, egui::Stroke::new(2.0, ink)));
    }

    // Dimension prompt: an on-elevation measurement arrow for the quantity being
    // typed (the plan's `render_dimension_prompt` feel, in elevation space). A
    // "distance along" prompt draws a horizontal arrow from the start edge to
    // the cut's column; a "height" prompt a vertical arrow from the floor.
    if let Some(dp) = &pv.dim_prompt {
        use crate::controller::app_controller::PartitionDimAxis;
        let dim_col = egui::Color32::from_rgb(120, 230, 160);
        // The arrow endpoints in wall-local coords.
        let (wa, wb): ((f64, f64), (f64, f64)) = match dp.axis {
            PartitionDimAxis::Along => (
                (0.0, dp.height_m as f64),
                (dp.along_m as f64, dp.height_m as f64),
            ),
            PartitionDimAxis::Height => (
                (dp.along_m as f64, 0.0),
                (dp.along_m as f64, dp.height_m as f64),
            ),
        };
        let sa = p(wa.0, wa.1);
        let sb = p(wb.0, wb.1);
        line(sa, sb, 2.0, dim_col);
        // Arrowheads at both ends.
        let dir = (sb - sa).normalized();
        let perp = egui::vec2(-dir.y, dir.x);
        let head = 8.0;
        for (tip, into) in [(sa, dir), (sb, -dir)] {
            line(tip, tip + into * head + perp * (head * 0.5), 2.0, dim_col);
            line(tip, tip + into * head - perp * (head * 0.5), 2.0, dim_col);
        }
        // Label with the value being measured (the drawn/pending magnitude).
        let value_m = match dp.axis {
            PartitionDimAxis::Along => dp.along_m,
            PartitionDimAxis::Height => dp.height_m,
        };
        let mid = sa + (sb - sa) * 0.5;
        painter.text(
            mid + perp * 12.0,
            egui::Align2::CENTER_CENTER,
            format!("{value_m:.2} m"),
            egui::FontId::proportional(13.0),
            dim_col,
        );
    }

    painter.text(
        rect.left_top() + egui::vec2(8.0, 8.0),
        egui::Align2::LEFT_TOP,
        "Draw a partition · tap a region to set its exposure · Done in the toolbar",
        egui::FontId::proportional(13.0),
        egui::Color32::from_gray(160),
    );

    partition_inset(&painter, rect, &pv.inset);

    // Primary input → wall-local (along, height-above-base), un-flipped so the
    // emitted events stay in the canonical a→b frame.
    let to_local = |pos: egui::Pos2| {
        let (wx, wy) = t.inverse(pos.x, pos.y);
        let along = if flip { len - wx } else { wx };
        (along as f32, (-wy) as f32)
    };
    if response.drag_started_by(egui::PointerButton::Primary)
        || response.dragged_by(egui::PointerButton::Primary)
    {
        if let Some(pos) = response.interact_pointer_pos() {
            let (along_m, height_m) = to_local(pos);
            on_event(UiEvent::Sketch(SketchEvent::PartitionStrokePoint {
                along_m,
                height_m,
            }));
        }
    }
    if response.drag_stopped_by(egui::PointerButton::Primary) {
        on_event(UiEvent::Sketch(SketchEvent::PartitionStrokeEnd));
    }
    if response.clicked_by(egui::PointerButton::Primary) {
        if let Some(pos) = response.interact_pointer_pos() {
            let (along_m, height_m) = to_local(pos);
            on_event(UiEvent::Sketch(SketchEvent::PartitionSelectRegion {
                along_m,
                height_m,
            }));
        }
    }
}

/// The plan-view you-are-here inset for the partition overlay: the active
/// shape's outline ring scaled into a small top-right corner, with the active
/// wall highlighted and the interior-side eye + view direction drawn so the
/// assessor can tell which way they're looking at the face.
fn partition_inset(
    painter: &egui::Painter,
    rect: egui::Rect,
    inset: &crate::controller::app_controller::PartitionInset,
) {
    if inset.outline.len() < 2 {
        return;
    }
    // A square sub-rect in the top-right corner, ~22% of the panel min dim.
    let side = rect.size().min_elem() * 0.22;
    let pad = 10.0;
    let sub = egui::Rect::from_min_size(
        egui::pos2(rect.right() - side - pad, rect.top() + pad),
        egui::vec2(side, side),
    );
    painter.rect_filled(sub, 4.0, egui::Color32::from_gray(18));

    // Fit the outline bbox into the sub-rect, with the eye folded in so the
    // arrow stays inside view too.
    let mut min = (f64::MAX, f64::MAX);
    let mut max = (f64::MIN, f64::MIN);
    let mut grow = |x: f64, y: f64| {
        min.0 = min.0.min(x);
        min.1 = min.1.min(y);
        max.0 = max.0.max(x);
        max.1 = max.1.max(y);
    };
    for (x, y) in &inset.outline {
        grow(*x, *y);
    }
    grow(inset.eye.0, inset.eye.1);
    let inner = sub.shrink(6.0);
    // Plan y grows downward on screen (no z-flip), aspect-preserving fit.
    let t = render::fit_transform(inner, (min.0, min.1), (max.0, max.1), ViewParams::default());

    let ring: Vec<egui::Pos2> = inset.outline.iter().map(|(x, y)| t.apply(*x, *y)).collect();
    painter.add(egui::Shape::closed_line(
        ring,
        egui::Stroke::new(1.5, egui::Color32::from_gray(170)),
    ));
    // Active wall in a bright colour.
    painter.line_segment(
        [
            t.apply(inset.wall_a.0, inset.wall_a.1),
            t.apply(inset.wall_b.0, inset.wall_b.1),
        ],
        egui::Stroke::new(3.0, egui::Color32::from_rgb(255, 210, 70)),
    );
    // The interior-side eye + a short look-direction arrow toward the wall.
    let eye = t.apply(inset.eye.0, inset.eye.1);
    painter.circle_filled(eye, 3.5, egui::Color32::from_rgb(120, 200, 255));
    let tip = eye + egui::vec2(inset.view_dir.0 as f32, inset.view_dir.1 as f32) * (side * 0.25);
    painter.line_segment(
        [eye, tip],
        egui::Stroke::new(2.0, egui::Color32::from_rgb(120, 200, 255)),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(i: usize, screen: egui::Pos2) -> MarginSeed {
        MarginSeed {
            group_index: i,
            color: egui::Color32::RED,
            anchor: (0.0, 0.0),
            has_target: true,
            storey_idx: None,
            screen,
            h: EDIT_SLOT_H,
        }
    }

    fn window_rect(w: &EditWindow) -> egui::Rect {
        egui::Rect::from_min_size(w.pos, w.size)
    }

    fn assert_no_pairwise_intersection(windows: &[EditWindow]) {
        for (i, a) in windows.iter().enumerate() {
            for b in &windows[i + 1..] {
                let (ra, rb) = (window_rect(a), window_rect(b));
                let overlap = ra.min.x < rb.max.x
                    && rb.min.x < ra.max.x
                    && ra.min.y < rb.max.y
                    && rb.min.y < ra.max.y;
                assert!(
                    !overlap,
                    "boxes {} ({ra:?}) and {} ({rb:?}) intersect",
                    a.group_index, b.group_index
                );
            }
        }
    }

    /// Under capacity the de-clutter keeps every item near its preferred
    /// spot, in order, at least `gap` past the previous item's extent,
    /// inside `[lo, hi]` — the original contract, carried over to the
    /// per-item-extent form.
    #[test]
    fn declutter_extents_under_capacity_keeps_spacing_and_bounds() {
        let mut ys = vec![10.0_f32, 15.0, 300.0];
        declutter_extents(&mut ys, &[40.0, 40.0, 40.0], 10.0, 0.0, 400.0);
        assert_eq!(ys, vec![10.0, 60.0, 300.0]);
        // Items overrunning `hi` are pulled back keeping their extents clear.
        let mut ys = vec![350.0_f32, 380.0];
        declutter_extents(&mut ys, &[40.0, 40.0], 10.0, 0.0, 400.0);
        assert_eq!(ys, vec![310.0, 360.0]);
    }

    /// UNDER total capacity nothing collapses to a chip, and the placed
    /// boxes respect the no-intersect contract with real gaps.
    #[test]
    fn margin_placement_under_capacity_places_full_boxes_only() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let seeds: Vec<MarginSeed> = (0..4)
            .map(|i| seed(i, egui::pos2(100.0 + 200.0 * i as f32, 300.0)))
            .collect();
        let (windows, inner) = place_windows_in_margins(rect, rect.center(), &seeds, None, &[])
            .expect("canvas is big enough");
        assert_eq!(windows.len(), 4);
        assert!(windows.iter().all(|w| !w.chip), "no chips under capacity");
        assert!(inner.width() >= 60.0 && inner.height() >= 60.0);
        assert_no_pairwise_intersection(&windows);
    }

    /// With far more groups than the four margins hold at full size, NO two
    /// boxes may intersect — the overflow collapses to compact chips (also
    /// non-intersecting, on-canvas) instead of cascading with overlap.
    #[test]
    fn margin_placement_over_capacity_collapses_overflow_to_chips() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // All left of centre → all first-prefer the LEFT edge; the capacity
        // spill has to distribute them and chip the rest.
        let seeds: Vec<MarginSeed> = (0..30).map(|i| seed(i, egui::pos2(150.0, 300.0))).collect();
        let (windows, inner) = place_windows_in_margins(rect, rect.center(), &seeds, None, &[])
            .expect("canvas is big enough");
        assert_eq!(windows.len(), 30, "every group gets a box or a chip");
        assert!(inner.width() >= 60.0 && inner.height() >= 60.0);
        let chips = windows.iter().filter(|w| w.chip).count();
        assert!(chips > 0, "30 groups cannot all fit as full boxes");
        assert!(chips < 30, "the margins hold at least a few full boxes");
        for w in &windows {
            let r = window_rect(w);
            assert!(
                rect.contains_rect(r),
                "box {} off-canvas: {r:?}",
                w.group_index
            );
        }
        assert_no_pairwise_intersection(&windows);
    }

    /// The plan rect must intersect NO box — auto-placed boxes stay outside
    /// the margin gap by construction, and a simulated DRAGGED box dropped
    /// onto the plan shrinks the plan rect clear of it (largest empty
    /// sub-rectangle; the user's box is never moved or demoted).
    #[test]
    fn plan_rect_avoids_dragged_boxes() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let seeds: Vec<MarginSeed> = (0..4)
            .map(|i| seed(i, egui::pos2(100.0 + 200.0 * i as f32, 300.0)))
            .collect();
        let (windows, inner) =
            place_windows_in_margins(rect, rect.center(), &seeds, None, &[]).expect("fits");
        // A dragged box dropped INSIDE the margin gap, off-centre (as on the
        // tablet screenshot: dragged boxes sitting on the plan).
        let dragged_rect = egui::Rect::from_min_size(
            inner.min + egui::vec2(10.0, 10.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        );
        let plan = plan_rect_avoiding_pinned(inner, &[dragged_rect]);
        assert!(
            inner.contains_rect(plan),
            "plan {plan:?} must stay within the margin gap {inner:?}"
        );
        assert!(plan.width() >= 60.0 && plan.height() >= 60.0, "{plan:?}");
        // Strictly clear of the dragged box AND of every auto-placed box.
        for r in windows.iter().map(window_rect).chain([dragged_rect]) {
            assert!(
                !rects_strictly_overlap(plan, r),
                "plan {plan:?} intersects box {r:?}"
            );
        }
        // A dragged box covering essentially the whole gap: the plan takes
        // the best rect there is rather than vanishing (or demoting the box).
        let wall_to_wall = egui::Rect::from_min_size(
            inner.min + egui::vec2(5.0, 5.0),
            inner.size() - egui::vec2(10.0, 10.0),
        );
        let plan = plan_rect_avoiding_pinned(inner, &[wall_to_wall]);
        assert!(plan.width() > 0.0 && plan.height() > 0.0);
    }

    /// Rule "no box overlaps ANY other box": the auto-packing treats a
    /// PINNED (dragged) rect as an obstacle — auto boxes that would have
    /// landed under it repack into the free segments around it (or demote to
    /// chips), so nothing intersects it or each other.
    #[test]
    fn margin_placement_repacks_around_pinned_box() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        // Six groups, all preferring the LEFT edge.
        let seeds: Vec<MarginSeed> = (0..6).map(|i| seed(i, egui::pos2(150.0, 300.0))).collect();
        // A dragged box dropped ON the left column, mid-height.
        let pinned_rect =
            egui::Rect::from_min_size(egui::pos2(10.0, 220.0), egui::vec2(EDIT_BOX_W, EDIT_SLOT_H));
        let (windows, _inner) =
            place_windows_in_margins(rect, rect.center(), &seeds, None, &[pinned_rect])
                .expect("fits");
        assert_eq!(windows.len(), 6, "every group still gets a box or a chip");
        for w in &windows {
            let r = window_rect(w);
            assert!(
                rect.contains_rect(r),
                "box {} off-canvas: {r:?}",
                w.group_index
            );
            assert!(
                !rects_strictly_overlap(r, pinned_rect),
                "auto box {} ({r:?}) sits under the pinned box {pinned_rect:?}",
                w.group_index
            );
        }
        assert_no_pairwise_intersection(&windows);
    }

    /// Pinned-vs-pinned resolves at DROP time: a box dropped onto an
    /// already-pinned box takes the smallest of the four cardinal push-outs
    /// (minimal displacement, on-canvas); a drop with no overlap is
    /// untouched.
    #[test]
    fn dropped_box_nudges_clear_of_pinned() {
        let bounds = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let pinned = egui::Rect::from_min_size(
            egui::pos2(100.0, 100.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        ); // → (100,100)-(265,255)
        let dropped = egui::Rect::from_min_size(
            egui::pos2(150.0, 120.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        );
        let nudged = nudge_clear_of(dropped, &[pinned], bounds);
        // Push-outs: left 215, right 115, up 175, down 135 → right wins.
        assert_eq!(nudged.min, egui::pos2(265.0, 120.0), "{nudged:?}");
        assert!(!rects_strictly_overlap(nudged, pinned));
        assert!(bounds.contains_rect(nudged));
        // No overlap → no movement.
        let clear = egui::Rect::from_min_size(
            egui::pos2(400.0, 400.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        );
        assert_eq!(nudge_clear_of(clear, &[pinned], bounds).min, clear.min);
        // Near the canvas edge the cheapest IN-BOUNDS push wins instead.
        let pinned_edge = egui::Rect::from_min_size(
            egui::pos2(800.0 - EDIT_BOX_W, 200.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        );
        let dropped_edge = egui::Rect::from_min_size(
            egui::pos2(800.0 - EDIT_BOX_W - 2.0, 210.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        );
        let nudged = nudge_clear_of(dropped_edge, &[pinned_edge], bounds);
        assert!(!rects_strictly_overlap(nudged, pinned_edge), "{nudged:?}");
        assert!(bounds.contains_rect(nudged), "{nudged:?}");
    }

    /// `expand_first` guarantees THAT group a full box even when it would
    /// otherwise be past capacity (a tapped chip expands; another box
    /// collapses rather than overlaps).
    #[test]
    fn margin_placement_expand_first_gets_a_full_box() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(800.0, 600.0));
        let seeds: Vec<MarginSeed> = (0..30).map(|i| seed(i, egui::pos2(150.0, 300.0))).collect();
        let (without, _) =
            place_windows_in_margins(rect, rect.center(), &seeds, None, &[]).expect("fits");
        // Pick a group that came out as a chip, then ask for it expanded.
        let victim = without
            .iter()
            .find(|w| w.chip)
            .expect("over capacity yields chips")
            .group_index;
        let (with, _) =
            place_windows_in_margins(rect, rect.center(), &seeds, Some(victim), &[]).expect("fits");
        let w = with
            .iter()
            .find(|w| w.group_index == victim)
            .expect("still placed");
        assert!(!w.chip, "the expanded group must get a full box");
        assert_no_pairwise_intersection(&with);
    }

    // ── The PRESCRIPTIVE single-storey slot layout ──────────────────────────

    /// A roomy canvas for the prescriptive tests: tall enough that the
    /// 5-slot right column (5 × (155 + 12) px from y = 84) fits.
    fn tall_rect() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1000.0, 1200.0))
    }

    fn slot_seed(i: usize) -> SlotSeed {
        SlotSeed {
            group_index: i,
            color: egui::Color32::RED,
            anchor: (0.0, 0.0),
            has_target: true,
            h: EDIT_SLOT_H,
        }
    }

    /// The slot position spec, with uniform `EDIT_SLOT_H` boxes on
    /// [`tall_rect`]: top row (slots 0-2) left→right at the signpost
    /// clearance, right column (slots 3-7) stacked down the right edge from
    /// the top (owning both right corners), bottom row (slots 8-10)
    /// left→right, bottom-aligned.
    fn expected_slot_pos(slot: usize) -> egui::Pos2 {
        let pitch = EDIT_BOX_W + EDIT_GAP;
        let vpitch = EDIT_SLOT_H + EDIT_GAP;
        match slot {
            0..=2 => egui::pos2(10.0 + slot as f32 * pitch, 84.0),
            3..=7 => egui::pos2(
                1000.0 - 10.0 - EDIT_BOX_W,
                84.0 + (slot - 3) as f32 * vpitch,
            ),
            _ => egui::pos2(
                10.0 + (slot - 8) as f32 * pitch,
                1200.0 - 12.0 - EDIT_SLOT_H,
            ),
        }
    }

    fn assert_prescriptive_contract(windows: &[EditWindow], inner: egui::Rect, n: usize) {
        assert_eq!(windows.len(), n.min(MAX_SLOTS));
        for (slot, w) in windows.iter().enumerate() {
            assert_eq!(w.group_index, slot, "slots follow group order");
            assert!(!w.chip, "the prescriptive layout has no chips");
            assert_eq!(
                w.pos,
                expected_slot_pos(slot),
                "slot {slot} position off-spec"
            );
        }
        assert_no_pairwise_intersection(windows);
        // The plan rect avoids every box.
        for w in windows {
            assert!(
                !rects_strictly_overlap(inner, window_rect(w)),
                "plan {inner:?} intersects box {} at {:?}",
                w.group_index,
                window_rect(w)
            );
        }
    }

    /// N ≤ 3: three slots across the top row; the plan gets everything below.
    #[test]
    fn prescriptive_three_groups_fill_the_top_row() {
        let seeds: Vec<SlotSeed> = (0..3).map(slot_seed).collect();
        let (windows, inner, hidden) = place_windows_prescriptive(tall_rect(), &seeds);
        assert_eq!(hidden, 0);
        assert_prescriptive_contract(&windows, inner, 3);
        // No column/bottom band → the plan reaches the right and bottom pads.
        assert_eq!(inner.max, egui::pos2(1000.0 - 10.0, 1200.0 - 12.0));
        assert!((inner.min.y - (84.0 + EDIT_SLOT_H + 12.0)).abs() < 0.5);
    }

    /// 4 ≤ N ≤ 8: 3 on top + the rest down the right column; the plan gets
    /// the rect left of the column, below the top row.
    #[test]
    fn prescriptive_eight_groups_fill_top_and_right() {
        let seeds: Vec<SlotSeed> = (0..8).map(slot_seed).collect();
        let (windows, inner, hidden) = place_windows_prescriptive(tall_rect(), &seeds);
        assert_eq!(hidden, 0);
        assert_prescriptive_contract(&windows, inner, 8);
        assert!((inner.max.x - (1000.0 - EDIT_BOX_W - 20.0)).abs() < 0.5);
    }

    /// 9 ≤ N ≤ 11: 3 top + 5 right (owning both right corners) + 3 bottom;
    /// the plan gets the central remaining rect.
    #[test]
    fn prescriptive_eleven_groups_fill_all_three_bands() {
        let seeds: Vec<SlotSeed> = (0..11).map(slot_seed).collect();
        let (windows, inner, hidden) = place_windows_prescriptive(tall_rect(), &seeds);
        assert_eq!(hidden, 0);
        assert_prescriptive_contract(&windows, inner, 11);
        assert!((inner.max.y - (1200.0 - EDIT_SLOT_H - 24.0)).abs() < 0.5);
    }

    /// N > 11: exactly the first 11 groups get boxes; the rest are hidden
    /// (the count drives the "not all groups shown" canvas note).
    #[test]
    fn prescriptive_overflow_hides_groups_past_eleven() {
        let seeds: Vec<SlotSeed> = (0..13).map(slot_seed).collect();
        let (windows, inner, hidden) = place_windows_prescriptive(tall_rect(), &seeds);
        assert_eq!(hidden, 2, "13 groups → 2 hidden behind the note");
        assert_prescriptive_contract(&windows, inner, 13);
        assert!(
            windows.iter().all(|w| w.group_index < 11),
            "only the first 11 groups are displayed"
        );
    }

    /// A user SWAP is a transposition of the slot order: with groups 1 and 5
    /// swapped, they trade exact slot positions and everyone else keeps
    /// theirs — no reflow, no off-grid rest positions.
    #[test]
    fn prescriptive_swap_transposes_two_slots_only() {
        let mut order: Vec<usize> = (0..8).collect();
        // What the drop handler does: swap the two groups' POSITIONS in the
        // stored order (group 1 dropped onto group 5's box).
        let (pa, pb) = (
            order.iter().position(|&g| g == 1).unwrap(),
            order.iter().position(|&g| g == 5).unwrap(),
        );
        order.swap(pa, pb);
        let seeds: Vec<SlotSeed> = order.iter().map(|&g| slot_seed(g)).collect();
        let (windows, inner, _) = place_windows_prescriptive(tall_rect(), &seeds);
        let pos_of = |g: usize| windows.iter().find(|w| w.group_index == g).unwrap().pos;
        assert_eq!(pos_of(5), expected_slot_pos(1), "5 took 1's slot");
        assert_eq!(pos_of(1), expected_slot_pos(5), "1 took 5's slot");
        for g in [0, 2, 3, 4, 6, 7] {
            assert_eq!(pos_of(g), expected_slot_pos(g), "group {g} unmoved");
        }
        assert_no_pairwise_intersection(&windows);
        for w in &windows {
            assert!(!rects_strictly_overlap(inner, window_rect(w)));
        }
    }

    /// The on-device duplicate-name scenario: the record holds TWO groups
    /// both displayed as "Group 4" (groups 1 and 3 here — the display name
    /// deliberately NEVER enters the slot pipeline: `SlotSeed` carries no
    /// name and [`transpose_slot_order`] keys strictly on the session group
    /// INDEX). Swapping group 1 with group 2 must move exactly those two;
    /// the same-named bystander (group 3) and everyone else stay put.
    #[test]
    fn duplicate_names_swap_moves_exactly_one_pair() {
        let mut order: Vec<usize> = (0..6).collect();
        // Baseline placement.
        let seeds: Vec<SlotSeed> = order.iter().map(|&g| slot_seed(g)).collect();
        let (before, ..) = place_windows_prescriptive(tall_rect(), &seeds);
        let pos_before: Vec<egui::Pos2> = (0..6)
            .map(|g| before.iter().find(|w| w.group_index == g).unwrap().pos)
            .collect();
        // The drop: group 1 (one "Group 4") dropped onto group 2.
        transpose_slot_order(&mut order, 1, 2);
        assert_eq!(order, vec![0, 2, 1, 3, 4, 5], "exactly one transposition");
        let seeds: Vec<SlotSeed> = order.iter().map(|&g| slot_seed(g)).collect();
        let (after, ..) = place_windows_prescriptive(tall_rect(), &seeds);
        let pos_after: Vec<egui::Pos2> = (0..6)
            .map(|g| after.iter().find(|w| w.group_index == g).unwrap().pos)
            .collect();
        assert_eq!(pos_after[1], pos_before[2], "group 1 took group 2's slot");
        assert_eq!(pos_after[2], pos_before[1], "group 2 took group 1's slot");
        for g in [0, 3, 4, 5] {
            assert_eq!(
                pos_after[g], pos_before[g],
                "bystander group {g} (incl. the same-named twin, group 3) must not move"
            );
        }
        // Unknown indices (a stale drop target) are a strict no-op.
        let snapshot = order.clone();
        transpose_slot_order(&mut order, 1, 99);
        assert_eq!(order, snapshot);
    }

    /// The drop hit-test: greatest positive overlap picks the swap target;
    /// a drop over empty space is a miss (→ snap back to own slot).
    #[test]
    fn swap_target_picks_greatest_overlap_or_none() {
        let seeds: Vec<SlotSeed> = (0..3).map(slot_seed).collect();
        let (windows, ..) = place_windows_prescriptive(tall_rect(), &seeds);
        // Dropped rect overlapping slot 1 by a sliver and slot 2 by a lot.
        let dropped = egui::Rect::from_min_size(
            expected_slot_pos(2) + egui::vec2(-20.0, 10.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        );
        assert_eq!(swap_target(&windows, 0, dropped), Some(2));
        // The dragged box's own slot never counts as a target.
        assert_eq!(swap_target(&windows, 2, dropped), Some(1));
        // Empty space → no swap.
        let empty = egui::Rect::from_min_size(
            egui::pos2(400.0, 600.0),
            egui::vec2(EDIT_BOX_W, EDIT_SLOT_H),
        );
        assert_eq!(swap_target(&windows, 0, empty), None);
    }

    /// The base optimiser uncrosses connectors: anchors constructed so the
    /// identity assignment crosses two lines come out swapped — zero proper
    /// crossings, the expected assignment — and the result is deterministic.
    #[test]
    fn optimise_slot_order_uncrosses_and_is_deterministic() {
        let rect = tall_rect();
        // Group 0's region sits under slot 1, group 1's under slot 0 —
        // identity order would cross; groups 2 and 3 sit under their own
        // slots (slot 3 = the right column's top slot).
        let anchors = vec![
            egui::pos2(270.0, 400.0),
            egui::pos2(92.0, 400.0),
            egui::pos2(446.0, 400.0),
            egui::pos2(700.0, 300.0),
        ];
        let order = optimise_slot_order(rect, &anchors);
        assert_eq!(order, vec![1, 0, 2, 3], "0 and 1 trade slots");
        // Zero proper crossings in the optimised assignment.
        let segs: Vec<(egui::Pos2, egui::Pos2)> = order
            .iter()
            .enumerate()
            .map(|(k, &g)| (slot_anchor_estimate(rect, k), anchors[g]))
            .collect();
        for (i, a) in segs.iter().enumerate() {
            for b in &segs[i + 1..] {
                assert!(
                    !segments_properly_cross(a.0, a.1, b.0, b.1),
                    "optimised connectors must not cross: {a:?} × {b:?}"
                );
            }
        }
        // Determinism: same input, same output.
        assert_eq!(order, optimise_slot_order(rect, &anchors));
        // Past MAX_SLOTS the identity tail is preserved.
        let many: Vec<egui::Pos2> = (0..13)
            .map(|i| egui::pos2(100.0 + i as f32, 500.0))
            .collect();
        let order = optimise_slot_order(rect, &many);
        assert_eq!(order.len(), 13);
        assert_eq!(&order[11..], &[11, 12]);
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..13).collect::<Vec<_>>(), "a permutation");
    }
}
