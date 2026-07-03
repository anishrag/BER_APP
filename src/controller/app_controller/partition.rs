//! Wall-face PARTITION tool: the app-shell workflow over the `document`
//! wall-face-partition model. A perimeter (outline) wall's internal elevation —
//! a flat wall-local rectangle (x = along_m from the wall's start vertex, y =
//! height_m off the floor) — is cut by drawn lines into regions, each given a
//! `WallExposure`. This is the 2-D counterpart to the 1-D along-wall exposure
//! sections; a wall is EITHER 1-D sectioned OR face-partitioned.
//!
//! Thin shell, per CLAUDE.md: the geometry (simplify / snap / subdivide /
//! point-in-polygon) lives in `manip_plan`, and the cut/exposure model mutation
//! lives in `document` (`StoreyDoc::commit_partition_cut` etc.). This file only
//! sequences them — pointer/stroke plumbing, the snap→dimension→commit
//! sequencing, undo snapshots, persistence, and the owned `PartitionView`.
//!
//! The face is opened from the wall-exposure toolbar (`open_wall_partition`)
//! keyed by the selected wall's stable `VertexId` on the EDITABLE outline
//! (pre-resolve) — the analogue of the windows/doors elevation editor, which is
//! keyed by a resolved `WallRegion` id instead.
use super::*;
use manip_plan::{point_in_polygon, segment_len as wall_len, simplify_to_cut, snap_on_face};

/// Flat-top fallback ceiling height (metres) when the storey has none set.
const DEFAULT_HEIGHT_M: f32 = 2.4;
/// RDP tolerance (wall-local metres) for collapsing a shaky freehand stroke to
/// its first+last points — a near-straight drag well within this stays a cut.
const SIMPLIFY_TOL_M: f32 = 0.1;
/// Snap radius (wall-local metres) for catching a cut endpoint on a face edge
/// or an existing cut's column/row.
const SNAP_EPS_M: f32 = 0.15;

/// A cut mid-dimensioning: its two endpoints in wall-local coords (snapped
/// coordinates already filled) plus the full snap picture — which coordinates
/// still await a typed dimension and which were silently snapped (so the
/// overlay can tag them and "Don't snap" can revert them to the drawn values).
/// Once nothing is free the cut commits.
///
/// The free-slot QUEUE is derived on demand ([`queue`](Self::queue)) from
/// `free_flags` + the DRAWN coordinates rather than stored, so a coordinate
/// freed by an un-snap simply rejoins it. Axis-aligned cuts collapse to ONE
/// shared slot: a near-vertical cut with both x's free asks a single "distance
/// along" applied to both endpoints; a near-horizontal cut with both y's free
/// asks a single "height" applied to both. Genuinely diagonal cuts keep the
/// per-endpoint slots. Slot order is shared-axis first, then a.x, a.y, b.x,
/// b.y. Flag/index order throughout is `[a.x, a.y, b.x, b.y]`.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PendingCut {
    pub a: Point2f32,
    pub b: Point2f32,
    /// The RDP endpoints as DRAWN, before `snap_on_face` — the un-snap targets.
    pub drawn_a: Point2f32,
    pub drawn_b: Point2f32,
    /// Which coordinates are awaiting a dimension (a.x, a.y, b.x, b.y). The
    /// free-slot queue is DERIVED from these + the drawn coords (shared-slot
    /// collapse), so a freed axis can rejoin it after an un-snap.
    pub free_flags: [bool; 4],
    /// Which coordinates were SNAPPED by `snap_on_face` and not yet un-snapped
    /// (a.x, a.y, b.x, b.y) — drives the tags. Typed/skipped axes are neither
    /// free nor snapped.
    pub snapped: [bool; 4],
}

impl PendingCut {
    /// Build the free-slot queue for a cut, collapsing an axis-aligned cut
    /// whose shared coordinate is free on both ends to one shared slot.
    fn slots_for(a: Point2f32, b: Point2f32, snaps: [bool; 4]) -> Vec<FreeSlot> {
        let [a_x_free, a_y_free, b_x_free, b_y_free] = snaps;
        let mut free = Vec::new();
        // Near-vertical: both x's free and (near-)equal → one shared "along".
        if a_x_free && b_x_free && (a.x - b.x).abs() <= SNAP_EPS_M {
            free.push(FreeSlot::SharedX);
            if a_y_free {
                free.push(FreeSlot::Ay);
            }
            if b_y_free {
                free.push(FreeSlot::By);
            }
            return free;
        }
        // Near-horizontal: both y's free and (near-)equal → one shared "height".
        if a_y_free && b_y_free && (a.y - b.y).abs() <= SNAP_EPS_M {
            free.push(FreeSlot::SharedY);
            if a_x_free {
                free.push(FreeSlot::Ax);
            }
            if b_x_free {
                free.push(FreeSlot::Bx);
            }
            return free;
        }
        // Genuinely diagonal: per-endpoint slots in a.x, a.y, b.x, b.y order.
        if a_x_free {
            free.push(FreeSlot::Ax);
        }
        if a_y_free {
            free.push(FreeSlot::Ay);
        }
        if b_x_free {
            free.push(FreeSlot::Bx);
        }
        if b_y_free {
            free.push(FreeSlot::By);
        }
        free
    }

    /// The free-slot queue, derived from `free_flags` + the DRAWN coordinates.
    /// The drawn coords drive the shared-slot collapse test: at build time a
    /// free axis's current coordinate IS its drawn one (snap left it
    /// untouched), so this matches building the queue once at stroke end — and
    /// because the drawn coords never change, typing answers can't reshuffle
    /// the remaining slots.
    fn queue(&self) -> Vec<FreeSlot> {
        Self::slots_for(self.drawn_a, self.drawn_b, self.free_flags)
    }

    /// Mark `slot` resolved (typed or skipped): clear its free flag(s) — a
    /// shared slot clears the axis on BOTH endpoints.
    fn resolve(&mut self, slot: FreeSlot) {
        match slot {
            FreeSlot::SharedX => {
                self.free_flags[0] = false;
                self.free_flags[2] = false;
            }
            FreeSlot::SharedY => {
                self.free_flags[1] = false;
                self.free_flags[3] = false;
            }
            FreeSlot::Ax => self.free_flags[0] = false,
            FreeSlot::Ay => self.free_flags[1] = false,
            FreeSlot::Bx => self.free_flags[2] = false,
            FreeSlot::By => self.free_flags[3] = false,
        }
    }

    /// Revert the chosen end's SNAPPED coordinates to the drawn values,
    /// freeing them for dimensioning again. Typed/skipped axes stay put.
    /// Returns whether anything changed.
    fn unsnap_end(&mut self, end_b: bool) -> bool {
        let (ix, iy) = if end_b { (2, 3) } else { (0, 1) };
        let drawn = if end_b { self.drawn_b } else { self.drawn_a };
        let mut changed = false;
        if self.snapped[ix] {
            if end_b {
                self.b.x = drawn.x;
            } else {
                self.a.x = drawn.x;
            }
            self.free_flags[ix] = true;
            self.snapped[ix] = false;
            changed = true;
        }
        if self.snapped[iy] {
            if end_b {
                self.b.y = drawn.y;
            } else {
                self.a.y = drawn.y;
            }
            self.free_flags[iy] = true;
            self.snapped[iy] = false;
            changed = true;
        }
        changed
    }

    /// Is any coordinate still awaiting a typed dimension?
    fn any_free(&self) -> bool {
        self.free_flags.iter().any(|&f| f)
    }
}

/// The last INSTANTLY-committed cut (both endpoints fully snapped at stroke
/// end, so it committed with no interaction): kept so its snap tags can still
/// be shown and un-snapped. `hlen` is the history length BEFORE the commit
/// (its undo entry sits at index `hlen`); un-snap pops + restores that entry,
/// then reopens the cut as pending with the tapped end's axes freed. Valid
/// only while `history.len() == hlen + 1` (nothing else has happened since);
/// cleared by the next stroke, open/close, any dimension-path commit, and
/// undo/redo (see `history.rs`).
#[derive(Debug, Clone, PartialEq)]
pub(super) struct PartitionInstantCommit {
    pub pending: PendingCut,
    pub hlen: usize,
}

/// One snap tag on the partition overlay: a pending (or just-instant-
/// committed) cut endpoint with ≥1 silently-snapped coordinate, at its
/// wall-local position. The tag's menu offers exactly "Don't snap", which the
/// UI routes (with `end_b`) to
/// [`unsnap_partition_end`](AppController::unsnap_partition_end).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PartitionSnapTag {
    /// Wall-local `(along, height)` of the (snapped) endpoint.
    pub pos: (f32, f32),
    /// Which endpoint the tag reverts (`false` = a, `true` = b).
    pub end_b: bool,
    /// Both axes snapped (label "corner") vs one (label "edge").
    pub corner: bool,
}

/// The snap tags for a cut: one per endpoint with ≥1 snapped coordinate.
fn cut_snap_tags(p: &PendingCut) -> Vec<PartitionSnapTag> {
    let mut tags = Vec::new();
    if p.snapped[0] || p.snapped[1] {
        tags.push(PartitionSnapTag {
            pos: (p.a.x, p.a.y),
            end_b: false,
            corner: p.snapped[0] && p.snapped[1],
        });
    }
    if p.snapped[2] || p.snapped[3] {
        tags.push(PartitionSnapTag {
            pos: (p.b.x, p.b.y),
            end_b: true,
            corner: p.snapped[2] && p.snapped[3],
        });
    }
    tags
}

/// Which on-elevation axis a dimension prompt measures, so the overlay can draw
/// the matching arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionDimAxis {
    /// Horizontal distance along the wall from along=0 to the cut's along.
    Along,
    /// Vertical height off the floor (z=0) to the cut's height.
    Height,
}

/// What the dimension dock should prompt for the NEXT free slot of the in-flight
/// cut, plus the data the overlay needs to draw the measurement arrow.
pub struct PartitionDimPrompt {
    pub label: String,
    pub axis: PartitionDimAxis,
    /// Wall-local along of the quantity being measured (the cut endpoint the
    /// arrow runs to). For a shared "along" prompt this is the cut's column.
    pub along_m: f32,
    /// Wall-local height of the quantity being measured (the row for a shared
    /// "height" prompt; otherwise the endpoint's height).
    pub height_m: f32,
}

/// One subdivided face region for the overlay, in wall-local coords.
pub struct PartitionRegionView {
    pub poly: Vec<(f32, f32)>,
    pub exposure: WallExposure,
    pub selected: bool,
}

/// The plan-view inset that orients the unrolled elevation: the whole active
/// shape's outline ring (world coords), the active wall, and the interior-side
/// eye that "looks at" the wall — so the UI can draw a little you-are-here and
/// mirror the along-axis to read correctly from inside.
pub struct PartitionInset {
    pub outline: Vec<(f64, f64)>,
    pub wall_a: (f64, f64),
    pub wall_b: (f64, f64),
    /// Interior-side eye position (world).
    pub eye: (f64, f64),
    /// Unit vector eye → wall (looking outward).
    pub view_dir: (f64, f64),
    /// True when a→b runs right→left across the viewer's field, so the UI must
    /// mirror the along-axis to match the inside view.
    pub flip_along: bool,
}

/// The unrolled wall face + its cuts/regions, for the partition overlay. All
/// wall-local coords are `(along_m, height_m)` from the wall's START vertex /
/// the floor. `None` (from `partition_view`) when not in partition mode.
pub struct PartitionView {
    pub wall_len_m: f32,
    /// Flat-top face height = the storey ceiling height (or the 2.4 m default).
    pub height_m: f32,
    /// Committed cuts (each `[(along, height); 2]`).
    pub cuts: Vec<[(f32, f32); 2]>,
    /// The in-flight cut being dimensioned, if any (resolved + drawn coords).
    pub pending: Option<[(f32, f32); 2]>,
    /// The in-flight freehand stroke points (wall-local along/height), so the
    /// overlay can draw an ink trail that follows the pen. Empty when idle.
    pub stroke: Vec<(f32, f32)>,
    pub regions: Vec<PartitionRegionView>,
    pub inset: PartitionInset,
    pub dim_prompt: Option<PartitionDimPrompt>,
    /// Snap tags for the pending (or just-instant-committed) cut: one per
    /// endpoint with ≥1 snapped axis, at the endpoint's wall-local position.
    /// `corner` when both axes snapped (label "corner") vs one ("edge").
    pub snap_tags: Vec<PartitionSnapTag>,
}

impl AppController {
    // ── mode ───────────────────────────────────────────────────────────────────

    /// True when a wall face is open for partitioning (routes the canvas to the
    /// elevation overlay rather than the plan).
    pub fn partition_mode(&self) -> bool {
        self.partition_wall.is_some()
    }

    /// The wall (outline start `VertexId`) whose face is open, if any.
    pub fn partition_wall(&self) -> Option<VertexId> {
        self.partition_wall
    }

    /// Open the wall-exposure-selected wall's face for partitioning. Requires a
    /// `selected_exposure_wall` (it's invoked from the exposure toolbar, so
    /// exposure mode is assumed on). Materializes first so the record + ceiling
    /// height are current, then clears the transient stroke/pending/selection.
    /// No-op when no wall is selected.
    pub fn open_wall_partition(&mut self) {
        let Some(vid) = self.selected_exposure_wall else {
            return;
        };
        self.materialize();
        self.partition_wall = Some(vid);
        self.partition_stroke.clear();
        self.partition_pending = None;
        self.partition_instant_commit = None;
        self.partition_selected_region = None;
    }

    /// Close the partition view + clear all its transient state. Called from the
    /// mode-entry methods (mirroring how `elevation_wall` is cleared) so the
    /// partition view can't leak across tools.
    pub fn close_wall_partition(&mut self) {
        self.partition_wall = None;
        self.partition_stroke.clear();
        self.partition_pending = None;
        self.partition_instant_commit = None;
        self.partition_selected_region = None;
    }

    // ── active-wall geometry ─────────────────────────────────────────────────────

    /// `(vid, a, b, len, height)` for the active partition wall, or `None` when
    /// not in partition mode / the wall has gone from the outline. `a`/`b` are
    /// the wall endpoints (world); `len` its length; `height` the face height
    /// (storey ceiling height, or the flat-top default).
    pub fn partition_active_geom(&self) -> Option<(VertexId, Point2f32, Point2f32, f32, f32)> {
        let vid = self.partition_wall?;
        let (_, a, b) = self
            .document
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(v, _, _)| *v == vid)?;
        Some((vid, a, b, wall_len(a, b), self.face_height()))
    }

    /// The face height for the active storey: its set ceiling height, else the
    /// flat-top default.
    pub(super) fn face_height(&self) -> f32 {
        self.current_storey_idx
            .and_then(|idx| self.current_record.as_ref()?.data.storeys.get(idx))
            .and_then(|s| s.ceiling_height_m)
            .map(|h| h as f32)
            .unwrap_or(DEFAULT_HEIGHT_M)
    }

    /// The committed cuts on the active wall as the `[[a, b]; ..]` array the snap
    /// solver expects (empty when not in partition mode).
    fn active_cuts(&self) -> Vec<[Point2f32; 2]> {
        let Some(vid) = self.partition_wall else {
            return Vec::new();
        };
        self.document
            .wall_face_partitions
            .get(&vid)
            .map(|part| part.cuts.iter().map(|c| [c.a, c.b]).collect())
            .unwrap_or_default()
    }

    // ── stroke → snap ─────────────────────────────────────────────────────────────

    /// Append a wall-local stroke point, CLAMPED to the wall face
    /// (`0..len` along, `0..height` up). The UI converts screen → (along, height)
    /// before calling; clamping here keeps both the drawn cut and the live ink
    /// trail inside the wall we're partitioning. No-op when not in partition mode.
    pub fn partition_stroke_point(&mut self, along: f32, height: f32) {
        let Some((_, _, _, len, h)) = self.partition_active_geom() else {
            return;
        };
        self.partition_stroke
            .push(Point2f32::new(along.clamp(0.0, len), height.clamp(0.0, h)));
    }

    /// End the freehand stroke: RDP-collapse it to a straight cut, snap each
    /// endpoint to the face's edges + existing cuts, and either commit
    /// immediately (both endpoints fully snapped) or store a `PendingCut` for
    /// dimensioning. Clears the stroke either way. No-op when there's no cut.
    pub fn partition_stroke_end(&mut self) {
        let stroke = std::mem::take(&mut self.partition_stroke);
        // A new stroke supersedes the last instant-commit's tags.
        self.partition_instant_commit = None;
        let Some((_, _, _, len, height)) = self.partition_active_geom() else {
            return;
        };
        let Some([raw_a, raw_b]) = simplify_to_cut(&stroke, SIMPLIFY_TOL_M) else {
            return; // degenerate / too few points
        };
        let cuts = self.active_cuts();
        let sa = snap_on_face(raw_a, len, height, &cuts, SNAP_EPS_M);
        let sb = snap_on_face(raw_b, len, height, &cuts, SNAP_EPS_M);
        let pending = PendingCut {
            a: sa.point,
            b: sb.point,
            drawn_a: raw_a,
            drawn_b: raw_b,
            free_flags: [!sa.x_fixed, !sa.y_fixed, !sb.x_fixed, !sb.y_fixed],
            snapped: [sa.x_fixed, sa.y_fixed, sb.x_fixed, sb.y_fixed],
        };
        if pending.any_free() {
            self.partition_pending = Some(pending);
        } else {
            // Fully snapped → instant commit, but keep the cut + pre-commit
            // history length so its tags still show and "Don't snap" can
            // un-commit it (see `PartitionInstantCommit`).
            let hlen = self.history.len();
            self.commit_partition(pending.clone());
            self.partition_instant_commit = Some(PartitionInstantCommit { pending, hlen });
        }
    }

    // ── dimensioning ────────────────────────────────────────────────────────────

    /// The prompt for the NEXT free slot of the in-flight cut, or `None` when
    /// nothing is awaiting a dimension. A "distance along" slot measures from the
    /// start corner; a "height" slot measures from the floor. The along/height
    /// fields carry the cut position the overlay arrow should run to.
    pub fn partition_dim_prompt(&self) -> Option<PartitionDimPrompt> {
        let p = self.partition_pending.as_ref()?;
        let slot = p.queue().first().copied()?;
        let (label, axis, along_m, height_m) = match slot {
            FreeSlot::SharedX => (
                "Distance from start corner (m)",
                PartitionDimAxis::Along,
                p.a.x,
                p.a.y.max(p.b.y),
            ),
            FreeSlot::Ax => ("Distance along (m)", PartitionDimAxis::Along, p.a.x, p.a.y),
            FreeSlot::Bx => ("Distance along (m)", PartitionDimAxis::Along, p.b.x, p.b.y),
            FreeSlot::SharedY => (
                "Height from floor (m)",
                PartitionDimAxis::Height,
                p.a.x.min(p.b.x),
                p.a.y,
            ),
            FreeSlot::Ay => (
                "Height from floor (m)",
                PartitionDimAxis::Height,
                p.a.x,
                p.a.y,
            ),
            FreeSlot::By => (
                "Height from floor (m)",
                PartitionDimAxis::Height,
                p.b.x,
                p.b.y,
            ),
        };
        Some(PartitionDimPrompt {
            label: label.to_string(),
            axis,
            along_m,
            height_m,
        })
    }

    /// Fill the next free slot with a typed `value` (along ⇒ distance from the
    /// start vertex, clamped 0..len; height ⇒ off the floor, clamped 0..height)
    /// and pop it. A shared slot applies the value to BOTH endpoints. When the
    /// queue empties, COMMIT. No-op when no cut is in flight.
    pub fn partition_dim_entry(&mut self, value: f32) {
        let Some((_, _, _, len, height)) = self.partition_active_geom() else {
            return;
        };
        let Some(mut p) = self.partition_pending.clone() else {
            return;
        };
        let Some(slot) = p.queue().first().copied() else {
            return;
        };
        let along = value.clamp(0.0, len);
        let h = value.clamp(0.0, height);
        match slot {
            FreeSlot::SharedX => {
                p.a.x = along;
                p.b.x = along;
            }
            FreeSlot::Ax => p.a.x = along,
            FreeSlot::Bx => p.b.x = along,
            FreeSlot::SharedY => {
                p.a.y = h;
                p.b.y = h;
            }
            FreeSlot::Ay => p.a.y = h,
            FreeSlot::By => p.b.y = h,
        }
        p.resolve(slot);
        self.advance_pending(p);
    }

    /// Accept the drawn (un-snapped) value of the next free slot as-is, popping
    /// it without a typed dimension. When the queue empties, COMMIT.
    pub fn partition_dim_skip(&mut self) {
        let Some(mut p) = self.partition_pending.clone() else {
            return;
        };
        let Some(slot) = p.queue().first().copied() else {
            return;
        };
        p.resolve(slot);
        self.advance_pending(p);
    }

    /// "Don't snap" on a pending cut endpoint's tag: revert that endpoint's
    /// SNAPPED coordinates to the drawn values and reopen their dimension
    /// prompts (the derived queue picks the freed axes back up). Bespoke (the
    /// pending cut lives outside undo) — NOT the generic `choose_snap`
    /// replay.
    ///
    /// When there is no pending cut but the LAST stroke instant-committed
    /// (both endpoints fully snapped), un-commit it: pop + restore the
    /// commit's own undo entry and reopen the cut as pending with this end's
    /// axes freed — so the cut that "just happened" is still negotiable.
    /// Nothing pending / nothing snapped / stale record → no-op.
    pub fn unsnap_partition_end(&mut self, end_b: bool) {
        if let Some(p) = self.partition_pending.as_mut() {
            p.unsnap_end(end_b);
            return;
        }
        let Some(rec) = self.partition_instant_commit.take() else {
            return;
        };
        // Stale: something else happened since the instant commit (its undo
        // entry is no longer the top of history) — its tags weren't shown
        // either. Drop the record.
        if self.history.len() != rec.hlen + 1 {
            return;
        }
        // Un-commit: pop + restore the commit's own undo entry (the
        // pre-commit state) — the truncate+restore idiom of the generic snap
        // replay — then persist the rewound document.
        let Some(before) = self.history.pop() else {
            return;
        };
        self.restore_history_snapshot(before);
        let _ = self.mark_dirty();
        let mut p = rec.pending;
        p.unsnap_end(end_b);
        self.partition_pending = Some(p);
    }

    /// Store the updated pending cut, or commit it when no free slots remain.
    fn advance_pending(&mut self, p: PendingCut) {
        if p.any_free() {
            self.partition_pending = Some(p);
        } else {
            self.partition_pending = None;
            self.commit_partition(p);
        }
    }

    // ── commit ──────────────────────────────────────────────────────────────────

    /// Commit a fully-resolved cut to the active wall's face partition.
    /// Snapshot → mutate `document` → persist → push undo. Clears any pending.
    fn commit_partition(&mut self, p: PendingCut) {
        // A dimension-path commit invalidates any earlier instant-commit
        // record (the instant path re-sets it right after this call).
        self.partition_instant_commit = None;
        let Some(vid) = self.partition_wall else {
            return;
        };
        let undo = self.current_history_snapshot();
        self.document
            .commit_partition_cut(vid, document::PartitionCut { a: p.a, b: p.b });
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo);
        self.partition_pending = None;
    }

    // ── region selection + exposure ───────────────────────────────────────────────

    /// Tap a face region at wall-local `(along, height)`: select it (store its
    /// sample point) IF the tap lands inside some region; otherwise clear the
    /// selection. No-op when not in partition mode.
    pub fn select_partition_region(&mut self, along: f32, height: f32) {
        let Some((vid, _, _, len, h)) = self.partition_active_geom() else {
            return;
        };
        let p = Point2f32::new(along, height);
        let hit = self
            .document
            .partition_face_regions(vid, len, h)
            .into_iter()
            .any(|(poly, _)| point_in_polygon(&poly, p));
        self.partition_selected_region = hit.then_some(p);
    }

    /// Attribute `exposure` to the currently selected face region. Needs a
    /// selected region; snapshot → mutate `document` → persist → push undo.
    pub fn set_partition_region_exposure(&mut self, exposure: WallExposure) {
        let Some((vid, _, _, len, height)) = self.partition_active_geom() else {
            return;
        };
        let Some(sample) = self.partition_selected_region else {
            return;
        };
        let undo = self.current_history_snapshot();
        self.document
            .set_partition_region_exposure(vid, sample, exposure, len, height);
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo);
    }

    /// Drop the active wall's face partition (it reverts to 1-D sectioning).
    /// Snapshot → mutate `document` → persist → push undo. Clears the in-flight
    /// pending + region selection. For a future "remove partition" button.
    pub fn clear_active_wall_partition(&mut self) {
        let Some(vid) = self.partition_wall else {
            return;
        };
        let undo = self.current_history_snapshot();
        self.document.clear_wall_partition(vid);
        let _ = self.mark_dirty();
        self.push_history_snapshot(undo);
        self.partition_pending = None;
        self.partition_selected_region = None;
    }

    // ── view-model ──────────────────────────────────────────────────────────────

    /// The instantly-committed cut, while its tags are still valid: nothing
    /// has happened since the commit, i.e. the commit's undo entry is still
    /// the top of history (see `PartitionInstantCommit`).
    fn partition_instant_commit_cut(&self) -> Option<&PendingCut> {
        self.partition_instant_commit
            .as_ref()
            .filter(|r| self.history.len() == r.hlen + 1)
            .map(|r| &r.pending)
    }

    /// The unrolled wall face + its cuts/regions/inset, for the overlay. `None`
    /// when not in partition mode / the wall has gone.
    pub fn partition_view(&self) -> Option<PartitionView> {
        let (vid, a, b, len, height) = self.partition_active_geom()?;
        let cuts: Vec<[(f32, f32); 2]> = self
            .document
            .wall_face_partitions
            .get(&vid)
            .map(|part| {
                part.cuts
                    .iter()
                    .map(|c| [(c.a.x, c.a.y), (c.b.x, c.b.y)])
                    .collect()
            })
            .unwrap_or_default();
        let pending = self
            .partition_pending
            .as_ref()
            .map(|p| [(p.a.x, p.a.y), (p.b.x, p.b.y)]);
        let stroke: Vec<(f32, f32)> = self
            .partition_stroke
            .iter()
            .map(|pt| (pt.x, pt.y))
            .collect();
        let regions = self
            .document
            .partition_face_regions(vid, len, height)
            .into_iter()
            .map(|(poly, exposure)| {
                let selected = self
                    .partition_selected_region
                    .map(|s| point_in_polygon(&poly, s))
                    .unwrap_or(false);
                PartitionRegionView {
                    poly: poly.iter().map(|p| (p.x, p.y)).collect(),
                    exposure,
                    selected,
                }
            })
            .collect();
        // Tag the pending cut's snapped endpoints — or the just-instant-
        // committed cut's, while nothing else has happened since.
        let snap_tags = self
            .partition_pending
            .as_ref()
            .or_else(|| self.partition_instant_commit_cut())
            .map(cut_snap_tags)
            .unwrap_or_default();
        Some(PartitionView {
            wall_len_m: len,
            height_m: height,
            cuts,
            pending,
            stroke,
            regions,
            inset: self.partition_inset(a, b),
            dim_prompt: self.partition_dim_prompt(),
            snap_tags,
        })
    }

    /// Build the plan-view inset that orients the elevation.
    ///
    /// The eye sits on the INTERIOR side of the wall, looking outward at it. We
    /// find that side robustly: take the wall midpoint `m`, the outward-ish edge
    /// normal `n` (perpendicular to a→b), and probe `m ± n·ε`; whichever probe
    /// is INSIDE the outline (`FloorPlan::contains_point`) is the interior side.
    /// `eye = m + interior_n · offset` (offset scaled to the wall length so it
    /// reads on any size plan); `view_dir = unit(m − eye)` points outward.
    ///
    /// `flip_along`: standing at the eye and looking outward, the wall's start→end
    /// (a→b) sweeps either left→right or right→left across the field of view.
    /// With `view_dir` pointing outward, the sign of `cross(view_dir, b−a)` tells
    /// which: a→b reads right→left (needs mirroring so the unrolled along-axis
    /// matches what's seen) when that cross product is negative.
    fn partition_inset(&self, a: Point2f32, b: Point2f32) -> PartitionInset {
        let outline: Vec<(f64, f64)> = self
            .document
            .outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .map(|(_, p, _)| (p.x as f64, p.y as f64))
            .collect();
        let (ax, ay) = (a.x, a.y);
        let (bx, by) = (b.x, b.y);
        let (mx, my) = ((ax + bx) * 0.5, (ay + by) * 0.5);
        // Edge direction and its left-hand normal.
        let (dx, dy) = (bx - ax, by - ay);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (nx, ny) = (-dy / len, dx / len); // unit normal
                                              // Probe distance: small relative to the wall, with a metre floor.
        let eps = (len * 0.1).max(0.05);
        let inside_plus = self
            .document
            .outline
            .floor_plan
            .contains_point(mx + nx * eps, my + ny * eps);
        // The interior normal points to whichever probe is inside the outline.
        let (inx, iny) = if inside_plus { (nx, ny) } else { (-nx, -ny) };
        // Eye offset scaled to the wall (reads on any plan size).
        let offset = (len * 0.5).max(0.3);
        let (ex, ey) = (mx + inx * offset, my + iny * offset);
        // view_dir points outward: eye → wall midpoint.
        let (vx, vy) = (mx - ex, my - ey);
        let vlen = (vx * vx + vy * vy).sqrt().max(1e-6);
        let (vdx, vdy) = (vx / vlen, vy / vlen);
        // cross(view_dir, b−a) sign: negative ⇒ a→b runs right→left from the eye.
        let cross = vdx * dy - vdy * dx;
        PartitionInset {
            outline,
            wall_a: (ax as f64, ay as f64),
            wall_b: (bx as f64, by as f64),
            eye: (ex as f64, ey as f64),
            view_dir: (vdx as f64, vdy as f64),
            flip_along: cross < 0.0,
        }
    }
}

/// Which coordinate slot of the in-flight cut is being resolved. `SharedX`/
/// `SharedY` are the collapsed single-measurement slots for an axis-aligned cut
/// (one typed value drives both endpoints); the rest are per-endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FreeSlot {
    /// One "distance along" applied to both a.x and b.x (vertical cut).
    SharedX,
    /// One "height" applied to both a.y and b.y (horizontal cut).
    SharedY,
    Ax,
    Ay,
    Bx,
    By,
}

#[cfg(test)]
mod tests {
    use super::super::tests::dimensioned_square_controller;
    use super::*;

    /// Open the partition view on the active storey's wall 0 of the
    /// `dimensioned_square_controller` 10×10 outline; returns the wall's vid.
    fn open_partition_on_wall0(c: &mut AppController) -> VertexId {
        let walls = c.document.outline.floor_plan.perimeter_walls();
        let (vid, a, b) = walls[0];
        let mid = ((a.x + b.x) * 0.5, (a.y + b.y) * 0.5);
        c.set_exposure_mode(true);
        c.exposure_pointer_down(mid.0 as f64, mid.1 as f64, 0.5);
        c.open_wall_partition();
        assert!(c.partition_mode());
        assert_eq!(c.partition_wall(), Some(vid));
        vid
    }

    #[test]
    fn stroke_snapping_both_ends_auto_commits_a_cut() {
        let mut c = dimensioned_square_controller();
        let vid = open_partition_on_wall0(&mut c);
        // Wall 0 is 10 m long; default face height 2.4 m. A vertical drag at the
        // start edge (x=0, snaps), floor (y=0) → ceiling (y=2.4) snaps every
        // coordinate, so it commits with no dimension prompt.
        c.partition_stroke_point(0.0, 0.0);
        c.partition_stroke_point(0.01, 1.2);
        c.partition_stroke_point(0.0, 2.4);
        c.partition_stroke_end();
        assert!(c.partition_pending.is_none(), "fully-snapped cut commits");
        assert!(c.document.is_wall_partitioned(vid));
        let part = c.document.wall_face_partitions.get(&vid).unwrap();
        assert_eq!(part.cuts.len(), 1);
    }

    #[test]
    fn vertical_cut_prompts_once_and_commits_on_one_entry() {
        let mut c = dimensioned_square_controller();
        let vid = open_partition_on_wall0(&mut c);
        // A vertical drag mid-wall (x=5, no feature to snap) from floor to
        // ceiling: y snaps both ends, and the two free x's collapse to ONE
        // shared "distance along" prompt.
        c.partition_stroke_point(5.0, 0.0);
        c.partition_stroke_point(5.02, 1.2);
        c.partition_stroke_point(5.0, 2.4);
        c.partition_stroke_end();
        assert!(
            c.partition_pending.is_some(),
            "free coords await a dimension"
        );
        let prompt = c.partition_dim_prompt().expect("a free slot to prompt");
        assert_eq!(prompt.label, "Distance from start corner (m)");
        assert_eq!(prompt.axis, PartitionDimAxis::Along);
        // A single entry resolves both endpoints' along → commit.
        c.partition_dim_entry(5.0);
        assert!(
            c.partition_pending.is_none(),
            "one shared entry resolves a vertical cut → commit"
        );
        assert!(c.document.is_wall_partitioned(vid));
        let part = c.document.wall_face_partitions.get(&vid).unwrap();
        assert_eq!(part.cuts.len(), 1);
        // Both endpoints landed at the entered along.
        let cut = &part.cuts[0];
        assert!((cut.a.x - 5.0).abs() < 1e-3 && (cut.b.x - 5.0).abs() < 1e-3);
    }

    #[test]
    fn horizontal_cut_prompts_once_for_height() {
        let mut c = dimensioned_square_controller();
        let vid = open_partition_on_wall0(&mut c);
        // A horizontal drag mid-height (y=1.2, no row to snap) from the start
        // edge (x=0 snaps) to the end edge (x=10 snaps): both y's free and equal
        // → ONE shared "height" prompt.
        c.partition_stroke_point(0.0, 1.2);
        c.partition_stroke_point(5.0, 1.21);
        c.partition_stroke_point(10.0, 1.2);
        c.partition_stroke_end();
        let prompt = c.partition_dim_prompt().expect("a free slot to prompt");
        assert_eq!(prompt.label, "Height from floor (m)");
        assert_eq!(prompt.axis, PartitionDimAxis::Height);
        c.partition_dim_entry(1.2);
        assert!(c.partition_pending.is_none(), "one entry → commit");
        let part = c.document.wall_face_partitions.get(&vid).unwrap();
        let cut = &part.cuts[0];
        assert!((cut.a.y - 1.2).abs() < 1e-3 && (cut.b.y - 1.2).abs() < 1e-3);
    }

    #[test]
    fn region_select_and_exposure_attributes_the_tapped_region() {
        let mut c = dimensioned_square_controller();
        let vid = open_partition_on_wall0(&mut c);
        // Commit a vertical cut at x=5 splitting the face into two halves.
        c.partition_stroke_point(5.0, 0.0);
        c.partition_stroke_point(5.0, 2.4);
        c.partition_stroke_end();
        // Vertical cut → one shared "along" entry resolves both ends and commits.
        c.partition_dim_entry(5.0);
        assert!(c.document.is_wall_partitioned(vid));
        // Tap the left half and mark it Exposed.
        c.select_partition_region(2.5, 1.2);
        assert!(
            c.partition_selected_region.is_some(),
            "tap lands in a region"
        );
        c.set_partition_region_exposure(WallExposure::Exposed);
        let view = c.partition_view().unwrap();
        let region = view
            .regions
            .iter()
            .find(|r| r.selected)
            .expect("a selected region");
        assert_ne!(
            region.exposure,
            WallExposure::Unset,
            "the tapped region reads back non-Unset"
        );
        assert_eq!(region.exposure, WallExposure::Exposed);
    }

    #[test]
    fn leaving_exposure_mode_closes_the_partition_view() {
        let mut c = dimensioned_square_controller();
        open_partition_on_wall0(&mut c);
        assert!(c.partition_mode());
        c.set_exposure_mode(false);
        assert!(
            !c.partition_mode(),
            "leaving exposure closes the partition view"
        );
    }
}
