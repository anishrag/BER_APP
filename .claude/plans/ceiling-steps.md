# Ceiling step heights — interface spec (api-engineer, 2026-06-10)

Status: approved. Order: slices 1+2 parallel (independent) → 3 → 4+5 → review.
Open questions resolved by default: composed z ≤ 0 is accepted with a log::warn at resolve
(single-delta validation at assign only); default-height re-edit makes deltas ride along
(falls out of the design); 3D fidelity (storey_3d reading per-region roof z) is a SEPARATE
follow-up — 2D labels/tints only for now; minimum |delta| = 0.01 m (floor-step parity).

## Model decisions

- Session/durable split = the floor-step pattern, REGION-based: the line (axis+coord+side) is
  input UX only; session record = CeilingStepRegion { polygon, delta_m } (controller-keyed,
  outside undo, not persisted); durable record = per-vertex z on persisted CeilingRegions.
  Reconstruction: flat region with z != default → delta = z − default. This RESOLVES the
  resolve.rs SEAM — rewrite that comment to describe the implemented pattern.
- delta_m is RELATIVE to the storey default (z − ceiling_height_m); overlapping steps COMPOSE
  (deltas sum, applied in order, later re-split earlier).
- CANONICALIZE AT COMMIT: after assign, materialize then rebuild the session list from the derived
  roofs (ceiling_step_regions_from_roofs) — session list == exact pieces == reload reconstruction
  (idempotence by construction). Fresh input polygon may be a cheap bbox half-rect (resolve clips).
- Interaction flow: gate (height set AND outline finalised, per-storey, false on All tab) → mode
  toggle → (near-straight stroke → axis line | point tap → H/V two-button picker) → SIDE pick
  (tap; sign(tap − coord); both half-rects tinted) → POSITION prompt for the line coordinate
  (one question, distance from parallel outline wall, prefilled, cyclable, skippable,
  no-reference auto-resolves logged) → AMOUNT (Up/Down + metres; commit) → canonicalize.
- Position prompt: dedicated LinePositionPrompt in document (NOT the recompute_position_prompt
  loop — no shape/params behind a line); REUSES the loop's reference-wall scan (promote
  reference_walls pub(crate) in position.rs), the controller's existing ApplyPositionDimension/
  SkipPositionDimension/CyclePositionReference events, PositionPromptUiData, and the UI panel
  VERBATIM (zero new panel code). PositionPromptKind gains a `Line` variant (never serialized —
  current_position_prompt is serde(skip)); Line arms in apply_position/skip_current_position are
  unreachable!("Line prompts are shell-resolved").

## Slice 1 — ber_model (data-model-engineer)

structural.rs:
  impl CeilingRegions {
    /// Re-split the regions by ceiling-step `areas` (world ring + z DELTA in
    /// metres), applied IN ORDER: each area splits every region produced so
    /// far (2D boolean ops on the plan projection); intersected pieces get
    /// every vertex's z shifted by the delta, remainders keep theirs — so
    /// overlapping areas COMPOSE (deltas sum). Empty `areas` → `self`
    /// unchanged (no geo ops). Sub-mm² slivers dropped (same rationale as
    /// `FloorRegions::with_levels`). Pieces inherit `construction_id`, get
    /// fresh ids and default `opes`. Assumes FLAT input regions (all current
    /// content is — see `flat_z`); sloped regions are the later roofs feature.
    pub fn with_z_deltas(self, areas: &[(Vec<Point2f64>, f64)]) -> Self
  }
  impl CeilingRegion {
    /// The region's single height when it is FLAT (every vertex z within
    /// 1e-6 m), `None` for sloped/degenerate regions. Used to reconstruct
    /// session step areas and label heights in 2D.
    pub fn flat_z(&self) -> Option<f64>
  }
Tests: vertical+horizontal compose → 4 z's (h, h+a, h+b, h+a+b); empty no-op exact points;
sliver drop; flat_z Some/None. Document the flatness assumption + opes-dropped-on-split.

## Slice 2 — manip_plan (geometry-engineer)

geom.rs + root re-export:
  /// Fit an AXIS-ALIGNED line to a near-straight stroke: TLS fit (`fit_line`),
  /// orientation snapped to the DOMINANT axis (the step line is orthogonal by
  /// construction). Returns the axis the line's position varies on (X for a
  /// vertical line — the position-prompt convention) and the centroid's
  /// coordinate on it. `None` when the stroke is too short or not line-like
  /// (max perpendicular deviation over ~15% of its span — a loop or an L is
  /// not a step line).
  pub fn fit_axis_line(points: &[Point2f32]) -> Option<(ParameterAxis, f32)>
NOT carve_or_cut — the line doesn't partition a carved shape; side polygons are bbox halves and
the real clipping is with_z_deltas. ~45° snaps to the dominant axis (no diagonal rejection).
Tests: near-vertical/near-horizontal/diagonal-dominant strokes; loop and L rejected; short rejected.

## Slice 3 — document (document-engineer; includes resolve signature ripple, workspace stays green)

New document/src/ceiling_step.rs (re-exports in lib.rs alongside step):
  pub enum LineSide { Less, Greater }   // coordinates on the line's axis less/greater than it
  pub struct CeilingStepRegion { pub polygon: Vec<Point2f32>, pub delta_m: f64 }  // doc per spec
  pub fn ceiling_step_regions_from_roofs(ber: &Ber, idx: usize) -> Vec<CeilingStepRegion>
     // flat regions with |z − default| ≥ 1 mm → delta; empty when height unset; slivers skipped
  pub struct LinePositionPrompt { pub axis, pub options: Vec<LinePositionOption>, pub current_idx }
  pub struct LinePositionOption { pub outline_wall_vid, pub reference_value, pub sign,
                                  pub current_distance }   // refs in the OUTLINE plan
  impl LinePositionPrompt { pub fn cycle(&mut self) }
  impl StoreyDoc {
    pub fn fit_ceiling_step_line(&self, stroke: &[Point2f32]) -> Result<(ParameterAxis, f32), String>
       // geom::fit_axis_line + crosses-the-footprint validation; Err = user-facing
    pub fn validate_ceiling_step_line(&self, axis, coord_m: f32) -> Result<(), String>
    pub fn ceiling_line_position_prompt(&self, axis, coord_m: f32) -> Option<LinePositionPrompt>
       // parallel outline walls nearest first; None → auto-resolve, LOGGED
    pub fn resolve_ceiling_line_position(&self, prompt: &LinePositionPrompt, value: f32)
       -> Result<f32, String>   // reference + sign·value; rejects non-finite/negative/off-footprint
    pub fn ceiling_step_region(&self, axis, coord_m: f32, side: LineSide, delta_m: f64)
       -> Option<CeilingStepRegion>   // bbox half-rect (+margin); resolve clips; shell canonicalizes
  }
position.rs: PositionPromptKind gains Line variant (doc per spec); unreachable! arms.
resolve.rs: resolve_storey gains final `ceiling_steps: &[CeilingStepRegion]`; inside the
Some(height) roofs branch only: default_for_footprint then, when non-empty, .with_z_deltas
(f32→f64); log::warn when a composed flat z ≤ 0.1 m. REWRITE the SEAM comment (now implemented).
Mechanical: storeys.rs both refresh sites pass &[] until slice 4; resolve tests &[].
Tests: one step → h / h+Δ regions; reconstruct→re-resolve FIXPOINT; ignored while height unset;
two crossing steps compose; outline change re-derives + re-clips.

## Slice 4 — controller (shell-engineer)

New src/controller/app_controller/ceiling_steps.rs + mod.rs wiring. Fields:
  ceiling_step_mode: bool,
  ceiling_step_regions: HashMap<usize, Vec<CeilingStepRegion>>,
  pending_ceiling_step: Option<PendingCeilingStep>,
pub(super) enum PendingCeilingStep { Orientation { point }, Side { axis, coord_m },
  Position { axis, coord_m, side, prompt: LinePositionPrompt }, Amount { axis, coord_m, side } }
Methods (doc comments per the spec text): ceiling_step_available (height set AND outline
finalised AND Some(current_storey)), ceiling_step_mode, set_ceiling_step_mode (no-op enter when
unavailable; mutually exclusive both ways — other setters gain ceiling_step_mode = false),
apply_ceiling_step_stroke_event, ceiling_step_tap (stage dispatch: idle→point;
side-pending→side(sign) → position prompt or logged auto-resolve→Amount),
choose_ceiling_step_orientation(horizontal) (horizontal line → Y axis),
ceiling_step_orientation_pending/side_pending/amount_pending, pending_ceiling_step_line,
pending_ceiling_step_side_rects (bbox halves, cut-pick tint pattern),
assign_ceiling_step_delta (rejects |Δ| < 0.01 and Δ ≤ −default; pushes region, mark_dirty,
materialize, CANONICALIZE from derived roofs; outside undo),
cancel_pending_ceiling_step, ceiling_step_region_polygons -> Vec<(ring, "+0.20 m")>,
delete_ceiling_step_region (mark_dirty; pieces re-flatten next derive).
storeys.rs: reconstruct_ceiling_step_regions_for (sibling of the step one); call at the storey-
switch site (contains_key guard) + clear pending/mode in the switch resets; refresh_storey +
refresh_all_storeys gather + pass the slice. persistence.rs: reconstruct at both reload sites.
dimensioning.rs: route the THREE existing position entry points when PendingCeilingStep::Position
is live — position_prompt_for_ui builds PositionPromptUiData (area_wall = the line span endpoints,
reference via wall_positions_in(outline, vid), kind: Line); apply_position_dimension →
resolve_ceiling_line_position then advance to Amount (NO undo snapshot, NO mark_dirty — nothing
moved yet); skip → drawn coord → Amount; cycle → prompt.cycle(). Modal exclusivity: ceiling-step
mode forces the outline active so dimensioning.current_position_prompt can't coexist.
Controller tests: gate refusal while height unset; stroke→side→position→amount→materialize splits
roofs; tap+H/V path == stroke path; position apply moves the line, skip keeps it; reload
reconstructs + no-edit re-derive keeps the split (fixpoint); delete re-flattens; storey switch
clears pending.

## Slice 5 — UI (shell-engineer, with slice 4)

SketchEvents: SetCeilingStepMode(bool), CeilingStepStrokeWorld { points },
CeilingStepTap { world_x, world_y }, ChooseCeilingStepOrientation { horizontal },
AssignCeilingStepDelta(f32) (sign owned by Up/Down buttons, |v|), CancelPendingCeilingStep,
DeleteCeilingStepRegion(usize). Position panel events reused as-is; position_prompt_key prefill
covers Line prompts via the kind. app.rs arms 1:1; BerApp gains ceiling_amount_input: String.
Toolbar: "⬆ Ceiling steps" via add_enabled(ceiling_step_available) with hover hint "Set the
storey's default ceiling height first"; ceiling_step_toolbar mirroring step_toolbar: H/V row
(+Cancel) when orientation pending; "Tap the side of the line where the ceiling steps" label when
side pending; "Step (m)" + Up/Down/Cancel when amount pending; else region list with delete.
Shown when mode || any pending (pending-with-mode-off rule).
Canvas: tap → CeilingStepTap when in mode or side pending; ceiling_step_draw joins needs_world +
world-event dispatch; pending-any joins the primary_available conjunction. Render: dashed pending
line, side half-rect tints, committed tint+labels in a hue distinct from floor steps. storey_2d
roof labels pick up stepped z automatically (no change).

## Behavior traps

- IDEMPOTENT RELOAD IS THE CONTRACT: derive → reconstruct → derive is a fixpoint (canonicalize-at-
  commit makes in-session == post-reload). Test explicitly.
- Serde: NOTHING new serialized (session-only types; CeilingRegion unchanged; StoreyDoc untouched;
  PositionPromptKind never serialized) → NO snapshot version bump.
- Undo: regions outside undo (phases/steps parity); no snapshots in assign/delete; pending cleared
  on storey switch + mode exit.
- Gate double-enforced (disabled toggle + setter guard), per-storey, false on All tab.
- with_z_deltas only in the Some(height) branch; outline edits re-derive + re-clip (grown band
  comes back un-stepped — accepted floor-step parity); persist's unfinalised branch already zeroes
  roofs (tool gated on finalised anyway).
- Keep user-facing error strings stable once written (tests match on them).

## Addendum: line snapping (Anish, 2026-06-10)

Requirement: if the drawn/tapped line lands CLOSE to a parallel wall, it SNAPS to that wall's
coordinate (like every other feature); a snapped line's position is EXACT so the position prompt
is SKIPPED — but the snap TAG must render (the standard clickable snap box), and choosing to
un-snap reverts to the drawn coordinate and asks the position dimensions as normal.

Design (master spec; the seams already exist):

- **Snap candidates = the SAME deduped parallel reference walls the position prompt uses**
  (document::position::reference_walls on the line's axis) — snapping means "the line IS at a
  reference wall", which is exactly why no question is needed. EXCLUDE walls at the outline bbox
  extremes on that axis (snapping there leaves one side empty — useless step).
- Tolerance: `CEILING_LINE_SNAP_TOL_M: f32 = 0.2` const in document/ceiling_step.rs (hand
  precision; documented as promotable to tuning.yaml later).
- document: `pub fn snap_ceiling_step_line(&self, axis: ParameterAxis, coord_m: f32)
  -> Option<(f32, VertexId)>` — nearest qualifying reference within tol → (snapped coord,
  outline wall vid). Pure; the shell decides when to call.
- controller: `struct CeilingLineSnap { drawn_coord_m: f32, outline_wall_vid: VertexId }`
  (pub(super)). PendingCeilingStep::Side and ::Amount gain `snap: Option<CeilingLineSnap>`
  (Position implies un-snapped). Both line-creation paths (stroke fit + point/orientation) try
  the snap immediately after the axis+coord are known: Some → coord = snapped, carry the info.
  Side pick with snap → SKIP Position → Amount (snap carried). Un-snap
  (`unsnap_ceiling_step_line`): revert coord to drawn_coord_m, drop the snap; from Side stay in
  Side (Position comes after the side pick as normal); from Amount build the position prompt →
  Position stage (or logged auto-resolve back to Amount).
  `ceiling_step_snap_tag(&self) -> Option<SnapTagView>` — when pending carries a snap: tag at the
  line's midpoint, `from` = the drawn point (identity), current_kind = the most fitting existing
  SnapKind variant (check the enum — an edge/wall-ish kind), options EMPTY + skippable=true (the
  menu then offers only "Don't snap").
- UI: render the tag through the existing snap-tag renderer (single-tag slice) during
  Side/Position?/Amount stages; its click emits new `SketchEvent::UnsnapCeilingStepLine` (hit-test
  ours before the generic ChooseSnap routing so the two tag families don't collide).
- Tests: within-tol stroke snaps (coord exact, NO Position stage, tag present, assign lands at the
  wall's coordinate); beyond tol unchanged; unsnap from Amount reopens the Position prompt with
  the DRAWN coord; bbox-extreme walls never snap; the point+H/V path snaps identically.
