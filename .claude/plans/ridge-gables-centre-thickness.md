# Ridges to walls, rect ridges, Centre prompts, reordered dimensioning, floor-thickness walls — interface design (api-engineer, Mode 1)

Six connected features. Cross-checked against `src/controller/app_controller/roof_ridges.rs`,
`document/src/roof_point.rs`, `ber_model/src/ber/structural.rs` (`with_apexes` /
`apex_groups` / `ridge_triangles` / `WallRegions::regenerate` / `roof_profile_top`),
`document/src/resolve.rs`, `document/src/ceiling_step.rs`, `document/src/position.rs`,
`src/controller/app_controller/{snap.rs,dimensioning.rs,ceiling_steps.rs,step_regions.rs}`,
`src/ui/pages/sketch/{prompt_dock.rs,canvas.rs}`, `ber_model/src/ber/ber_struct.rs`
(`storey_floor_datums`), `document/src/exposure.rs` (`riser_walls`,
`sections_from_wall_regions`), `tuning.yaml`.

Ground rules that govern everything below:

- **Durable input is `Storey::roof_apexes`** (f32-quantized via `RoofApex::quantize_point`;
  `Ber::add_roof_apex` quantizes on store). The reload fixpoint is LAW:
  resolve → canonicalize → re-resolve must reproduce the surface.
- **`CeilingRegions::apex_groups` is THE grouping authority** — reconstruction
  (`ceiling_step_regions_from_roofs`), ridge views, and occupancy validation all read it.
- **Two containment predicates by design**: assign-time validation is strict(ish);
  re-application (`with_apexes`) is inclusive. Feature 1 widens both coherently.
- The shell owns the pending-input state machines (outside undo); the document owns
  every validation and computed suggestion; ber_model owns the face generation.

---

## 1. Wall-to-wall (boundary) ridges → gables

### What changes conceptually

An apex ON a ceiling-region boundary (today rejected by the strict predicate) becomes a
first-class **gable apex**. A ridge whose ends sit on the region boundary produces hub
triangles whose third vertex lies on the wall; the triangle for the wall edge *itself*
is plan-degenerate and sliver-drops, while the two adjacent hub triangles each carry an
edge **along** the wall (e.g. ridge along Y at the left wall `x = x0`, apex `A = (x0, c)`:
triangle `((x0,y0),(x_mid,y0),A)` has edge `(x0,y0)→A` on the wall line). Verified against
`ridge_triangles` + `roof_profile_top`: `profile_breakpoints` picks up those edges as
near-collinear overlaps, `covering_roof_plane` evaluates the sloping planes, and the
wall top traces base → apex → base — **the triangular gable top falls out of
`WallRegions::regenerate` with no wall-side change**. `merge_coplanar_faces` then folds
each slope's triangles into one face. Hub-switch count for a boundary-ended ridge over a
rectangle is still 2 (the midline crosses the two long edges only) — the existing
`ridge_hub_switches ≤ 2` guard keeps working; add a test.

### The containment predicate

One new tolerance, shared by resolve and validation:

```rust
// ber_model/src/ber/structural.rs
/// Plan tolerance (m) for claiming an apex that sits ON a region boundary.
/// Same rationale as PROFILE_EPS_M: derived rings are boolean-op output that
/// can sit a few 1e-7 m off the line they conceptually coincide with.
pub const APEX_BOUNDARY_EPS_M: f64 = 1e-4;
```

```rust
// ber_model/src/ber/structural.rs, impl CeilingRegions
/// Indices of the regions that CLAIM plan point `p`: the region strictly
/// containing it (at most one — interiors are disjoint), else every region
/// whose plan boundary passes within APEX_BOUNDARY_EPS_M of it. Empty when
/// p is inside nothing and near no boundary. THE apex-claim predicate:
/// with_apexes takes the FIRST claimant (warn when >1), assign-time
/// validation REQUIRES exactly one (a shared internal boundary is rejected).
pub fn claim_regions(&self, p: Point2f64) -> Vec<usize>
```

```rust
// ber_model/src/ber/structural.rs, impl CeilingRegion
/// p is strictly inside this region's plan polygon OR within eps_m of its
/// boundary (point-to-ring-edge distance). The per-region half of claim_regions.
pub fn plan_contains_within(&self, p: Point2f64, eps_m: f64) -> bool
```

**Which region claims a shared-boundary apex, deterministically:** validation
(`document`) rejects an apex/ridge-end whose `claim_regions` has length ≠ 1, with a
stable message ("a high point can't sit on the line between two ceiling areas — move it
to one side"). So in practice only OUTLINE-boundary apexes (one claimant) are creatable —
exactly the gable use case. Resolve (`with_apexes`) must still never panic on legacy or
hand-edited input: it takes `claim_regions(p).first()` (region order is deterministic
per resolve) and warns on multi-claim — the same warn-and-proceed posture as the
≥3-apex branch. *Alternative considered:* allowing internal-shared-boundary apexes with
a lowest-index tie-break — rejected: both adjacent regions would need coordinated face
generation and the assessor has no use for it; revisit if a ridge ever needs to end on a
ceiling-step line.

### Changes by crate

`ber_model` (data-model-engineer):

- `with_apexes`: replace the per-region `plan.intersects(point)` collect with one
  `claim_regions(apex.point)` pass per apex (first claimant wins). Behaviour on all
  existing content is identical (strict-inside apexes claim the same region; re-applied
  apexes are exact corners of their own triangles). The EPS fallback is what keeps a
  boundary apex claimed when its region ring is boolean-noise-displaced — without it the
  apex warn-skips and the roof flattens on re-resolve (fixpoint break).
- `apex_groups`: no change needed for this feature — after the first resolve the gable
  apex IS a plan corner of its hub triangles, so the 1e-6 corner-identity incidence
  holds. (`fan_base_z` drops apex-matching vertices as today; transition/hub triangles
  keep ≥ 1 base vertex.)
- No change to `ridge_triangles` / `ridge_split_ring` / `fan_triangles` — the
  degenerate wall-edge triangle sliver-drops by the existing `SLIVER_AREA_M2` filter.

`document` (document-engineer) — `roof_point.rs`:

- `validate_roof_ridge(storey, a, b, z_m)` rework: claim the ridge's region by its
  **midpoint** (strictly inside for any ≥1 cm ridge that fits — this is the determinism
  dodge: the midpoint, never the ends, picks the region), then require both quantized
  endpoints `plan_contains_within(end, APEX_BOUNDARY_EPS_M)` of THAT region, and require
  each endpoint's `claim_regions` to be `{that region}` (rejects ends on internal shared
  boundaries). Everything else (distinct quantized ends, un-apexed, flat base, ≥ 1 cm
  delta, hub-switches ≤ 2) unchanged. New stable message for the shared-boundary
  rejection; `RIDGE_ENDS_SAME_REGION_MSG` stays for genuinely different regions.
- `validate_roof_ridge_extent` rework (see §3 — it becomes the finalize check):
  the start may now lie ON the boundary, and the extent may now REACH the boundary —
  the span check runs from the **midpoint** `(coord, lo + len/2)` via
  `plan_span_through`, requiring `lo ≥ mid + t_min − APEX_BOUNDARY_EPS_M` and
  `lo + len ≤ mid + t_max + APEX_BOUNDARY_EPS_M`. `RIDGE_CLEAR_TOL_M` (the deliberate
  1 mm clearance) is **retired** — its reversal is this feature. The over-length message
  keeps the stable `RIDGE_LENGTH_EXCEEDS_AREA_MSG` prefix; the permitted run becomes the
  full wall-to-wall span. Tests asserting the old clearance flip
  (`validate_ridge_extent_checks_length_against_the_region_span`).
- `fit_roof_ridge_line`: the per-endpoint `validate_roof_point` (strictly-inside-bbox)
  is relaxed to "inside the footprint bbox expanded by the end-snap tolerance" — a
  stroke ending on/just past the wall must survive to the snap (§2), which pulls it
  exactly onto the wall. The cross-coordinate test stays strict (a ridge ON a parallel
  wall is still rejected).
- `validate_roof_apex` (tap flow) **unchanged** — strict containment stays. Gables are
  entered by the ridge stroke. *Alternative:* widening the tap flow too; deferred — taps
  have no snap step, so a "boundary" tap is hand noise, not intent.

### f32 quantization + reload fixpoint argument

A snapped end's coordinate IS a reference wall's coordinate — an f32 from the outline
plan — so `quantize_point` is the identity on it; the stored apex sits exactly on the
outline ring (whose vertices are the same f32 values widened to f64). First resolve:
default ceiling ring vertices are those exact coordinates → the apex lies exactly on the
edge → strict `intersects` already claims it (geo's predicate is exact for an on-segment
point); the EPS arm covers stepped-piece rings that boolean ops displaced by ~1e-7.
Re-resolve from canonicalized steps: deterministic boolean output → same ring noise →
same claim → same triangles → same coplanar merge (stable index order) → same faces.
`apex_groups` incidence is corner-exact post-resolve. The gable wall amends (its base
edge in plan never moved), keeping id/opes/exposure.

---

## 2. Snap ridge ends to the boundary

Reuses the ceiling-line snap machinery (`snap_ceiling_step_line` /
`CeilingLineSnap` / the stage-owned single snap tag with "Don't snap" reopening the
question — the established pattern in `roof_ridges.rs`), with one deliberate difference:
**the bbox-extreme-wall exclusion is lifted for extent ends** (snapping an end onto the
boundary is now exactly the point), and kept for the cross coordinate (a ridge lying
ALONG a wall still makes no roof).

```rust
// document/src/ceiling_step.rs (next to snap_ceiling_step_line), impl StoreyDoc
/// Snap a ridge EXTENT END coordinate (on the ALONG axis `axis.other()` —
/// callers pass that axis directly) onto the nearest parallel outline wall
/// within CEILING_LINE_SNAP_TOL_M, INCLUDING walls at the outline bbox
/// extremes — a ridge end on the boundary is a gable, not a no-op.
/// Returns (snapped coordinate, outline wall vid); None beyond tolerance.
pub fn snap_roof_ridge_end(&self, axis: ParameterAxis, coord_m: f32) -> Option<(f32, VertexId)>
```

(Implementation: `reference_walls(...)` without the extreme filter — factor the body of
`snap_ceiling_step_line` into a private helper with an `include_extremes: bool` rather
than duplicating; tolerance stays the shared `CEILING_LINE_SNAP_TOL_M = 0.2`, still a
const with the existing "promotable to tuning.yaml" note.)

### Shell changes (`roof_ridges.rs`, shell-engineer)

`apply_roof_ridge_stroke_event` snaps **three** degrees of freedom at stroke time:

- cross: `snap_ceiling_step_line(axis, coord)` — unchanged;
- `lo`: `snap_roof_ridge_end(axis.other(), lo)` (replaces the current
  `snap_ceiling_step_line(axis.other(), lo)` — same shape, extremes now included);
- `hi` (NEW): `snap_roof_ridge_end(axis.other(), hi)`. Guard: if `lo` and `hi` snap to
  the same wall, keep only the nearer end's snap.

The pending state gains `end_snap: Option<CeilingLineSnap>` (the HI end). Semantics of
the three snaps under the new stage order (§3):

| snapped | effect |
| --- | --- |
| cross | Cross question skipped (as today) |
| lo (start) | Start question skipped — extent anchored at the wall (as today) |
| hi (end) | extent's far end fixed; with lo ALSO snapped the LENGTH is exact by construction → the Length question is skipped (`len = hi − lo`); with lo un-snapped the Length question is still asked (prefill `hi − lo`) and a typed length **re-anchors at hi** (`lo = hi − len`) — the Start question is then skipped too (position is fixed by the hi snap) |

One snap tag at a time, stage-owned (extending the existing scheme): the tag shown at a
given stage is the snap whose question was most recently passed — `end` outranks `start`
outranks `cross`. The tag's menu is exactly "Don't snap" (`options` empty,
`skippable: true`, routed to `unsnap_roof_ridge`, never the generic `choose_snap` replay
— the pending ridge lives outside history). Un-snap transitions:

| tag clicked | revert | reopen |
| --- | --- | --- |
| end (hi) | `hi` → drawn value | the LENGTH question (always — re-asking is cheaper than remembering whether it was skipped; the restart-from-X reasoning already in `unsnap_roof_ridge`), cross/start snaps ride |
| start (lo) | `lo` → drawn value | the START question (resolved length + cross stand) |
| cross | `coord_m` → drawn value | the CROSS question, restarting through Start (resolved length stands) |

The end tag draws at the HI endpoint (mirror of the start tag's LO-endpoint placement in
`roof_ridge_snap_tag`).

---

## 3. Reorder ridge dimensioning: Length → Cross → Start → Height

### New stage machine (`PendingRoofRidge`, shell)

```rust
pub(super) enum PendingRoofRidge {
    /// Question 1: the ridge LENGTH (typed, prefilled with the drawn —
    /// possibly end-snapped — extent). Skipped iff BOTH extent ends snapped.
    Length      { axis, coord_m, lo, len_m, cross_snap, start_snap, end_snap },
    /// Question 2: the cross coordinate (un-snapped lines only). Same
    /// LinePositionPrompt machinery as today.
    CrossPosition { axis, coord_m, lo, len_m, start_snap, end_snap, prompt },
    /// Question 3: the extent's anchor position (un-snapped extents only —
    /// skipped when EITHER end snapped, the position DOF is fixed).
    StartPosition { axis, coord_m, lo, len_m, cross_snap, end_snap, prompt },
    Height        { axis, coord_m, lo, len_m, cross_snap, start_snap, end_snap },
}
```

`RidgeQuestion` becomes `&[RidgeQuestion::Cross, RidgeQuestion::Start]` consumed AFTER
the Length stage; `advance_roof_ridge` keeps its shape (skip-if-snapped /
auto-resolve-if-no-reference) but is entered from `assign/skip_roof_ridge_length`, and
when **no questions remain** it runs the finalize check (below) before moving to
`Height` — returning `Result<(), String>` so the rejection lands on whichever prompt the
user is on (or, for the all-snapped straight-from-stroke path, rejects the stroke with
the message logged, like a fit failure).

### Validation/stage table (the re-derived no-late-rejection invariant)

The invariant in its honest new form: **every stage validates its own value against
everything knowable at that stage; a later stage may reject only its own typed value.**
The full placement check runs at the moment the geometry becomes fully determined
(`finalize`), which is the last open location stage — so its rejection is always
attached to a value the user just typed (or, all-snapped, to the stroke itself).

| stage | validates | document fn | can reject |
| --- | --- | --- | --- |
| stroke fit | line-like; cross strictly crosses footprint; ends within bbox + snap tol; then snaps | `StoreyDoc::fit_roof_ridge_line` (relaxed ends, §1), `snap_ceiling_step_line`, `snap_roof_ridge_end` | the stroke |
| Length | finite, > 1 cm; **optimistic bound**: fits in SOME ceiling area — `len ≤ max over flat, un-apexed regions of the plan-bbox extent along `axis.other()`` (cheap over-approximation; the binding check is finalize's) | NEW `validate_roof_ridge_length(storey: &Storey, axis: ParameterAxis, len_m: f32) -> Result<(), String>`; stable msg "No ceiling area is that long — enter a length under N.NN m." | the typed length |
| Cross | crosses the footprint (`resolve_ceiling_line_position` → `validate_ceiling_step_line`, as today) | existing | the typed distance |
| Start | crosses on the along axis (as today), then **finalize** | existing + finalize | the typed distance |
| finalize (last open stage, or stroke when all snapped) | midpoint strictly inside one flat, un-apexed region; both ends within that region ∪ EPS band; extent within `plan_span_through` from the midpoint, boundary touch allowed | reworked `validate_roof_ridge_extent(storey, axis, coord_m, lo, len_m)` (§1) | the last typed value / the stroke |
| Height | z finite > 0; ≥ 1 cm off the base; quantized ends distinct; same-region claim; hub-switches ≤ 2 (all re-checked — cheap, and `materialize` may have shifted derived rings) | reworked `validate_roof_ridge` (§1) | the typed height |

Note the deliberate softening vs. today: Length's bound is optimistic (bbox extent), so
a length that fits the bbox but not the actual run is rejected at the location stage
with the permitted-run message. That is a rejection of the user's *position* entry,
which is within the invariant; the alternative (exact max-chord-anywhere computation at
Length) buys little and costs a real algorithm. Occupancy/flatness of the region moves
from the Length stage to finalize (it isn't knowable before a location is).

UI: `prompt_dock.rs` reorders nothing structurally — the `ridge_length_pending` panel
simply appears first; copy tweak: "from the highlighted end" hover only when an end is
anchored. The shell's `pending_roof_ridge_length` prefill and the dashed overlay
(`pending_roof_ridge_line`) work unchanged off the accumulated geometry.

---

## 4. 'Centre' on the location prompts

Centre is a **computed suggestion in `document`** routed through the SAME assign path as
a typed value — the controller computes the value, then drives the existing
resolve/advance, so undo/validation/canonicalize behaviour is identical by construction.

```rust
// document/src/roof_point.rs (free fns, like the validators)
/// The cross coordinate that centres the pending ridge within its ceiling
/// region: the midpoint of the region's span through the ridge midpoint
/// along the CROSS axis. The region is the one strictly containing the
/// ridge's current (drawn/resolved-so-far) midpoint; Err (user-facing,
/// stable) when the midpoint sits in no region.
pub fn centred_ridge_cross(storey: &Storey, axis: ParameterAxis,
                           coord_m: f32, lo: f32, len_m: f32) -> Result<f32, String>

/// The extent start that centres the ridge along its region: span through
/// the midpoint along the ALONG axis; returns span_mid − len/2. Err when
/// the midpoint sits in no region or len exceeds the span (carries the
/// permitted run, RIDGE_LENGTH_EXCEEDS_AREA_MSG prefix).
pub fn centred_ridge_start(storey: &Storey, axis: ParameterAxis,
                           coord_m: f32, lo: f32, len_m: f32) -> Result<f32, String>
```

Both are thin over `CeilingRegion::plan_span_through` (ber_model — no new geometry).

Shell (one high-level call per user action):

```rust
// roof_ridges.rs
/// The Centre button: materialize, compute the centred value for the OPEN
/// location stage (cross or start), and advance exactly as if it had been
/// typed. No-op when no location stage is up. Err is user-facing.
pub fn centre_roof_ridge_position(&mut self) -> Result<(), String>
```

Plumbing:

- `SketchEvent::CentreRoofRidgePosition` (and `CentreRoofRectPosition` in §5).
- `PositionPromptUiData` gains `pub offers_centre: bool` — set `true` by the
  `PendingRoofRidge::CrossPosition` / `StartPosition` arms (and the rect arms) in
  `position_prompt_ui_data`, `false` everywhere else.
- `position_prompt_toolbar` renders a `Centre` button (hover: "Centre it within its
  ceiling area") when `offers_centre`, emitting the event. No second toolbar.

Behaviour trap for the implementer: the centred value goes through
`resolve_ceiling_line_position`? **No** — that fn maps a *distance from a reference
wall*; Centre produces the coordinate directly. The shell skips the prompt resolution
and calls `advance_roof_ridge` with the centred coordinate (validated by
`validate_ceiling_step_line` inside the centred_* fns via the span check; the cross
centre is inside the footprint by construction). The prompt stays open on `Err`.

---

## 5. Closed-shape (rectangular) ridge — flat-topped hip / pavilion

### Durable representation — recommendation: (a) four corner apexes, one group

The rect ridge is stored as **four `RoofApex` corners (equal `z_m`, quantized) in
`Storey::roof_apexes`** — no new serde field. Rationale:

- The reload fixpoint stays in the machinery that already enforces it: `apex_groups`
  links the four corners transitively through the faces that carry them;
  `ceiling_step_regions_from_roofs` stays group-aware with one amendment (below);
  composition order (default → steps → apexes) is untouched.
- Degradation: old records simply have no rect groups. A NEW record opened by an OLD
  build hits `with_apexes`' n≥3 warn path (ridge from first two) — ugly but loads and
  never panics; acceptable for a single-user tool.
- *Alternative (b), a durable `roof_ridge_rects: Vec<...>`*: cleaner intent but needs a
  parallel composition slot, its own grouping/reconstruction path, and double bookkeeping
  when an outline change strands it — more fixpoint surface for the same geometry.
  Rejected.

### ber_model (data-model-engineer)

```rust
// structural.rs, private
/// The 4 contained apexes form an axis-aligned rectangle: quantized corner
/// set {x0,x1}×{y0,y1}, sides ≥ MIN_RIDGE_LENGTH_M, all z within 1e-6.
fn apexes_form_rect(apexes: &[&RoofApex]) -> Option<(Point2f64, Point2f64)> // (lo, hi)

/// D3, the pavilion loft (the rect analogue of ridge_triangles):
/// 0. pre-lift ring vertices plan-matching a corner;
/// 1. split boundary edges where they cross a corner's DIAGONAL ray (the
///    hip line from each rect corner outward along (±1,±1)) — the rect
///    analogue of ridge_split_ring, and like it the SINGLE source of the
///    split, shared with rect_hip_switches;
/// 2. per (sub-)edge, hub = the rect SIDE its midpoint faces (which corner
///    sector it falls in): two loft triangles (v, w, S_b), (v, S_b, S_a);
/// 3. one transition (hip) triangle per assignment switch, vertex order
///    preserving CCW;
/// 4. append the flat TOP region (the rect at z);
/// sub-SLIVER_AREA_M2 plan triangles drop; fresh ids, default opes,
/// inherited construction_id; caller merges coplanar.
fn rect_ridge_faces(region: CeilingRegion, corners: [&RoofApex; 4]) -> Vec<CeilingRegion>
```

```rust
/// How many hub-assignment switches walking the ring after the diagonal
/// split — the foldover guard (4 = clean pavilion; >4 = the ring wraps a
/// notch past a hip line). Pure plan-space; shared by validation and faces.
pub fn rect_hip_switches(ring: &[Point2f64], lo: Point2f64, hi: Point2f64) -> usize
```

- `with_apexes`: the `n` match gains a branch — `4` where `apexes_form_rect` hits →
  `merge_coplanar_faces(rect_ridge_faces(...))`; otherwise fall through to the existing
  n≥3 warn path. **Re-apply identity** (the fixpoint core): each output trapezoid
  contains exactly its 2 top corners → the n=2 ridge branch re-triangulates it in its
  own plane and the coplanar merge folds it back; each hip triangle contains 1 corner →
  fan reproduces it; the flat TOP contains all 4 → the rect branch over the rect itself
  emits only sliver lofts + the top — itself. No warns on the canonical path.
- `apex_groups` amendment (REQUIRED for the stepped-rect fixpoint): a member region
  whose `fan_base_z` is `None` because **every** vertex is apex-matching (the rect top)
  currently warn-excludes from `merged_rings`, which would punch a hole in the
  reconstructed step area and revert the under-top base on re-resolve. New rule: such a
  region joins the merge without voting on `base_z`; regions with a *disagreeing* or
  genuinely irregular base stay warn-excluded.

### document (document-engineer)

```rust
// roof_point.rs
/// Stroke-time fit: a CLOSED stroke in roof-point mode to an axis-aligned
/// rect — manip_plan::fit_axis_rect + the footprint test (all four
/// sides within the outline bbox + snap tolerance). Err is user-facing.
impl StoreyDoc { pub fn fit_roof_rect(&self, stroke: &[Point2f32])
                   -> Result<(Point2f32, Point2f32), String> }   // (lo, hi)

/// Stage-1 optimistic bound for the two typed side lengths (per axis, the
/// rect analogue of validate_roof_ridge_length).
pub fn validate_roof_rect_lengths(storey: &Storey, len_x: f32, len_y: f32)
    -> Result<(), String>

/// Finalize: rect centre strictly inside one flat, un-apexed region; all 4
/// quantized corners within that region ∪ EPS band and uniquely claimed;
/// spans through the centre fit both extents (boundary touch allowed);
/// rect_hip_switches == 4.
pub fn validate_roof_rect_placement(storey: &Storey, lo: Point2f64, hi: Point2f64)
    -> Result<(), String>

/// Height-stage check: placement re-checked + z finite > 0, ≥ 1 cm off base.
pub fn validate_roof_rect_ridge(storey: &Storey, lo: Point2f64, hi: Point2f64, z_m: f64)
    -> Result<(), String>

/// Centre suggestions, one per axis (mirrors centred_ridge_*).
pub fn centred_rect_lo(storey: &Storey, axis: ParameterAxis,
                       lo: Point2f64, hi: Point2f64) -> Result<f32, String>
```

### manip_plan (geometry-engineer)

```rust
// geom.rs
/// Fit a CLOSED stroke to an axis-aligned rectangle: the stroke's bbox,
/// accepted when every point sits near the bbox boundary (the rect
/// analogue of fit_axis_segment's straightness heuristic). None for blobs
/// and open strokes (closure is the caller's test).
pub fn fit_axis_rect(points: &[Point2f32]) -> Option<((f32, f32), (f32, f32))>
```

### Shell (shell-engineer) — new `roof_rects.rs`

Stroke routing: `apply_roof_ridge_stroke_event` first tests closure — first/last point
within `self.tuning.subarea.close_gap_m` (the SAME constant the living-area/phase carve
uses; the world-stroke path has no `is_closed` flag, and this matches how
`carve_or_cut_in_available` classifies closure) — closed → `apply_roof_rect_stroke`,
open → the line flow. No canvas change (it already sends `RoofRidgeStrokeWorld` for any
roof-mode stroke).

`PendingRoofRect` stage machine, reusing the §3 ordering and §4 Centre:

```
LenX { lo, hi, snaps } → LenY → PosX(prompt, Centre) → PosY(prompt, Centre) → Height
```

- LenX/LenY: typed side lengths, prefilled drawn, skippable (rendered like the ridge
  length panel; validated by `validate_roof_rect_lengths` progressively).
- PosX/PosY: `ceiling_line_position_prompt(axis, lo_axis)` on the rect's LO side, with
  `offers_centre = true` (→ `centred_rect_lo`). Anchored at the lo corner; a typed
  length keeps lo fixed.
- finalize (`validate_roof_rect_placement`) at the last open stage; Height →
  `validate_roof_rect_ridge` then `Ber::add_roof_apex` ×4, `mark_dirty`, `materialize`,
  `reconstruct_ceiling_step_regions_for` — byte-for-byte the ridge's commit shape.
- Events: `AssignRoofRectLength(f32)`, `SkipRoofRectLength`, `AssignRoofRectHeight(f32)`,
  `CancelPendingRoofRect`, `CentreRoofRectPosition` (or fold Centre into the shared
  `CentreRoofRidgePosition` handler keyed on which pending is up — recommended, one
  event: rename it `CentreRoofPosition`).
- Views: `pending_roof_rect_outline() -> Option<[(f64,f64);4]>` for a dashed overlay;
  committed rects come out of `roof_ridge_views`' sibling
  `roof_rect_views()` (apex groups with exactly 4 apexes forming a rect, label
  "⌃ 4.20 m").
- Side snapping at stroke time: **out of scope for the first slice** — Centre + typed
  positions cover the pavilion case; per-side snaps interact with typed lengths
  (4 anchors × 2 lengths) and can layer on later using `snap_roof_ridge_end` per side.
  Noted as the follow-up.

### Fixpoint argument (rect)

Corners quantized on store; first resolve claims them via `claim_regions` (strict for an
interior rect; EPS for boundary-touching sides) on the SAME region (validated);
`rect_ridge_faces` is deterministic; coplanar merge is stable-order; `apex_groups` links
all four corners through the hip/loft faces (each face carries ≥ 1 corner, transitively
closed through shared regions — hip triangles carry 2 adjacent corners, the top carries
4, so the group is connected); reconstruction emits ONE merged step area (top now
included in the merge) at the group base; re-resolve: steps → `with_apexes` n=4 over the
re-stepped piece → identical faces. Settles after the first canonicalize, like the ridge.

---

## 6. Floor thickness becomes part of the upper-storey walls

DEAP measures upper-storey wall area floor-to-floor: storey N's perimeter walls extend
DOWN through the slab band. Today `storey_floor_datums` sets
`z_N = anchor_z + thickness` and nothing fills `[anchor_z, z_N]` — a visual/model gap.

### The change (one line of intent, in `document/src/resolve.rs`)

In `resolve_storey`, the **perimeter** `WallRegions::regenerate` call's `base_z` drops
from `0.0` to `-(storey.floor_thickness_m.unwrap_or(0.0))` — storey-local coordinates,
where the datum is the floor's top:

```rust
let wall_base_z = -storey.floor_thickness_m.unwrap_or(0.0);
let walls = WallRegions::regenerate(wall_segments, &previous_walls,
                                    roofs_for_walls, wall_base_z, fallback_top_z, wall_cid);
```

Nothing else moves: `fallback_top_z` stays the absolute ceiling height; the riser pass
(`with_risers`) is untouched.

### Verified consequences (no further interface changes)

- **Storey 0 / legacy**: `floor_thickness_m` is `None` on storey 0 and on pre-thickness
  records (which also stack by the legacy ceiling-height rule — no band exists) →
  `base_z = 0` → byte-identical output.
- **Amend identity on a thickness edit**: `match_bases_to_previous` GATES on plan
  endpoints only (`MATCH_EPS_M`, 1e-2 m); the base-z term is rank-only. A thickness
  change re-extrudes every perimeter wall downward and they all AMEND (id +
  construction + opes + painted exposure preserved). Verified in
  `structural.rs::match_bases_to_previous`.
- **Risers**: floor-step riser faces derive from `storey.floors` levels and ceiling
  risers from the roofs — neither sees `base_z`. Interior floor-step risers do NOT
  extend (decided: only the perimeter slab band — an interior step riser spans between
  two slabs of the same storey, not down to the storey below).
- **`riser_walls` classification**: plan-midpoint-on-perimeter discriminator — z-free,
  unaffected.
- **2D exposure painting**: operates on plan segments / along-wall ranges — unaffected.
- **Wall-area reporting** (`sections_from_wall_regions` consumers / future DEAP export):
  reads region polygons, so areas grow by `t × wall length` — exactly the DEAP
  convention. The recovery itself matches by plan base edge — z-free.
- **3D**: `projected_bb_for_storey` folds wall vertices → the bb grows by the band and
  the fit stays correct. Full-house stacking: storey N's walls now occupy
  `[datum_N − t, …]` = `[anchor_z, …]` in house coords — they exactly fill the band the
  datums already reserve; the storey below's walls top out at its ceiling (the anchor
  over this footprint) — no overlap, no double-count.

### Implementer traps

- Add a resolve test: thickness 0.225 on storey 1 → every perimeter region's base z =
  −0.225, riser faces unchanged, ids stable across the thickness edit (amend).
- One subtle ranking note: on the first re-resolve after a thickness edit, the rank-only
  |Δz| term means a floor riser sharing a plan edge with a perimeter wall could in
  principle outrank it for the previous region — the existing z-term was added exactly
  to keep those apart and still does (the riser's previous z matches itself better);
  cover with the existing riser/perimeter shared-edge test pattern if it exists, else
  note in the test above.

---

## Slice plan (implementation order + gates)

Every slice gates on: `cargo build -p document && cargo build -p manip_plan &&
cargo build -p ber_model && cargo test --workspace && cargo fmt --all --check &&
.claude/scripts/check-boundaries.sh`.

| # | slice | agent(s) | contents | extra gate |
| --- | --- | --- | --- | --- |
| S1 | **F6 floor thickness** (independent, smallest) | document-engineer | `resolve_storey` base_z + tests | 3D smoke: band visible, no gap |
| S2 | **F3 reorder** (no behaviour widening yet) | document-engineer (split `validate_roof_ridge_length` out of the extent check) + shell-engineer (stage machine) | new `PendingRoofRidge`, Length-first flow, finalize-at-last-stage, table tests | stage-order tests incl. all-snapped path |
| S3 | **F1 boundary apexes** | data-model-engineer (`claim_regions`, `with_apexes` claim swap) + document-engineer (validator rework) | gable geometry + fixpoint tests (boundary-ridge resolve → reconstruct → re-resolve; gable wall-profile test; quantization round-trip) | the wall-profile gable test |
| S4 | **F2 end snaps** | document-engineer (`snap_roof_ridge_end`) + shell-engineer (end_snap state, tags, unsnap table) | wall-to-wall draw works end-to-end | snap/unsnap transition tests |
| S5 | **F4 Centre** | document-engineer (centred_*) + shell-engineer (event + handler) + UI | Centre on both ridge location stages | — |
| S6 | **F5 rect ridge** | geometry-engineer (`fit_axis_rect`) → data-model-engineer (`rect_ridge_faces`, `rect_hip_switches`, `apex_groups` merge amendment, n=4 branch) → document-engineer (validators, centred_rect) → shell-engineer (`roof_rects.rs`, routing, events) → UI | the pavilion, Centre included | rect fixpoint tests (default base + stepped base), re-apply identity test |

S3 before S4 (snapping onto the boundary is only useful once the boundary is legal);
S6 last — it consumes S2's ordering, S3's claim predicate, S4's snap fn, S5's Centre.

## Risks

1. **Boolean-noise claims (F1/F5)** — the EPS fallback in `claim_regions` is the
   load-bearing piece; if `i_overlay` noise ever exceeds 1e-4 m the apex goes dormant
   and the roof flattens. Mitigation: the constant is shared with `PROFILE_EPS_M`'s
   calibrated band; fixpoint tests over a stepped piece exercise the noisy path.
2. **Multi-claim at resolve** — validation prevents creating shared-internal-boundary
   apexes, but legacy/imported data can contain them; with_apexes' first-claim + warn
   must be deterministic across resolves (region order is — boolean ops are
   deterministic — but assert it in a test).
3. **Optimistic Length bound (F3)** rejects less than today, moving some rejections to
   the location stages — UX regression risk is bounded by the permitted-run message;
   watch for the all-snapped path surfacing finalize errors only in the log.
4. **Rect loft generality (F5)** — `rect_ridge_faces` over non-convex regions is the
   hardest geometry here; the `rect_hip_switches == 4` validation fence keeps resolve
   inside the well-understood case, but resolve itself must still warn-and-proceed on a
   legacy >4 input (never panic). Budget extra test time (L-shaped region, rect touching
   one wall = mono-gable side).
5. **Stage-machine churn (F2+F3)** — three snaps × four stages × unsnap transitions is
   the largest state table in the shell; the transition tables above are normative —
   implement them as table-driven tests first.
6. **Message stability** — several stable user-facing strings change meaning
   (`RIDGE_START_OUTSIDE_CEILING_MSG`'s "can't start on a wall ... yet" is now false;
   the extent message's permitted run grows to the full span). Update the strings AND
   the tests in the same slice; never reuse an old string with new semantics.
7. **F6 is low-risk** but touches every storey's derived walls — verify a reload of an
   existing record with thickness set amends (not recreates) every perimeter wall
   (opes/exposure survive), which the plan-only match gate guarantees; the test must
   prove it.
