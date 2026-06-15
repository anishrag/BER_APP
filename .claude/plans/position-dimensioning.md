# Position dimensioning for phase/step regions — interface spec (api-engineer, 2026-06-10)

Status: approved. Slices 1 (manip) → 2 (document) → 3 (controller+UI).
Open questions resolved by default: Wall-kind re-ask scoped to non-free axes (cut walls) only;
sibling-edge contact counts as fixed (transitive approximation accepted — translating would tear the
shared edge); apply_position guardrail checks the storey outline only (matches apply_dimension's
existing exposure); step-cut UX strictly sequences position prompt then amount (the gates do this).

## Why / findings

- A carved sub-area is an independent FloorPlan (carve.rs build_region): V0 becomes a Constant;
  if the area touches an outline corner both axes are exact; a mid-span wall hug fixes ONE axis
  (the perpendicular); a fully interior area has both axes approximate (stroke coords verbatim).
- Fixedness CANNOT be derived from known_params (exact coords may be Constants; known_params gets
  polluted by length dimensioning). It must be computed geometrically against the CARVE BOUNDARY at
  slot-creation time (the boundary isn't retained later) — the same scan params_pinned_to_outline does.
- Cut regions: outline arc pins both translation axes; the free DOF is the cut wall's perpendicular
  param — already length-prompted, but a SKIP leaves it at the stroke value → needs a Wall-kind
  position prompt.
- repair_unset_params never moves exact coords → position prompts AFTER the length loop is safe.
- Prompt lives in document (NOT the grandfathered EditDragPrompt family): gates stay field checks,
  undo/slot-sync correctness free via Document membership, solve is model mutation anyway.
- Skippable, confirmed: references may be unmeasurable; zero-option prompts must AUTO-RESOLVE
  (log, never silently claim precision) or the flow wedges.
- Living areas: carry the free-axes record (uniform pinned_slot_for) but are NEVER prompted.

## Slice 1 — manip_2d_plan (geometry-engineer)

lib.rs, next to params_pinned_to_outline (refactor the existing contact scan into a shared private
helper so the two stay in lockstep — behaviour-preserving for existing callers):

  /// Axes along which `area`'s absolute position is NOT fixed by this boundary
  /// plan — the whole area could translate along a listed axis without violating
  /// any snapped wall / shared corner / diagonal-facet contact, so its drawn
  /// position on that axis is approximate. Same contact tests as
  /// `params_pinned_to_outline`, but per AXIS and including Constant coords
  /// (a pin on V0 fixes an axis without any param existing).
  pub fn free_position_axes(&self, area: &FloorPlan) -> Vec<ParameterAxis>

Semantics: axis A fixed iff (any area wall perpendicular to A overlaps a parallel boundary wall at
the same coordinate) OR (any area vertex coincides with a boundary corner / lies on a boundary
diagonal edge / sits on a curve facet — fixes BOTH axes). Receiver = boundary plan.
Interior → [X, Y]; one mid-span wall hug → the along-wall axis only; corner contact → []; cut → [].

parametric.rs + query.rs wrapper:
  /// Translate every coordinate in the plan by (dx, dy): each X/Y parameter
  /// value and each X/Y Constant. Wall lengths are invariant (pure rigid
  /// translation); parameter IDs and expressions are untouched.
  pub fn translate_all(&mut self, dx: f32, dy: f32)

parametric.rs: ParameterAxis gains #[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
(pattern: CoordinateExpression).

Tests: interior → both axes; mid-wall hug → one; corner/diagonal contact → none; cut region → none;
translate_all preserves wall lengths and shifts get_points uniformly.

## Slice 2 — document (document-engineer)

New document/src/position.rs (sibling of dimensioning.rs; types re-exported from document root):

  /// One "where exactly is this area?" question: pin its position along `axis`
  /// by a measured distance from a storey-outline wall.
  pub struct PositionPrompt {
      pub axis: ParameterAxis,
      pub kind: PositionPromptKind,
      /// Cyclable (area wall, outline wall) measurement options, nearest first.
      /// Never empty (an axis with no references auto-resolves instead).
      pub options: Vec<PositionOption>,
      pub current_idx: usize,
  }
  /// How applying the entered distance resolves the prompt.
  pub enum PositionPromptKind {
      /// No exact coordinate on `axis` — applying rigid-translates the whole
      /// area (`FloorPlan::translate_all`) so the measured wall lands exactly.
      Translate,
      /// The area is anchored but this wall's perpendicular coord (e.g. a cut
      /// line) is unset — applying sets the parameter (+ repair).
      Wall { param: ParameterId },
  }
  /// One candidate measurement. NOTE the two VertexIds index DIFFERENT plans.
  pub struct PositionOption {
      pub area_wall_vid: VertexId,      // in the ACTIVE (area) plan
      pub outline_wall_vid: VertexId,   // in the storey OUTLINE plan
      pub reference_value: f32,         // outline wall's coordinate on `axis`
      pub sign: f32,                    // signum(area − reference) at build (side preservation)
      pub current_distance: f32,        // |area − reference| — the approximate drawn distance
  }

DimensionState gains (all serde(default) except the skip-marked prompt):
  pub position_free_axes: Vec<ParameterAxis>,        // set once by pinned_slot_for vs carve boundary
  pub position_resolved_axes: Vec<ParameterAxis>,    // entered OR skipped OR auto-resolved
  pub position_skipped_params: HashSet<ParameterId>, // skipped Wall-kind prompts
  #[serde(skip)] pub current_position_prompt: Option<PositionPrompt>,  // re-derived

pinned_slot_for additionally sets position_free_axes: boundary.free_position_axes(&plan)
(unconditional — uniform constructor; living areas never prompted anyway).

impl StoreyDoc (position.rs):
  /// Recompute `current_position_prompt`. None unless active is PhaseArea/StepArea
  /// AND the wall-length loop is done. Order: Translate per free-unresolved axis,
  /// then Wall per unset movable param on a NON-free axis (cut line) not skipped.
  /// Zero-reference items auto-resolve (logged). Preserves current_idx when the
  /// prompt identity (axis+kind) is unchanged.
  pub fn recompute_position_prompt(&mut self);
  /// Apply the entered distance via the selected option. Translate: rigid-move on
  /// the axis; Wall: set param + validate/repair like apply_dimension. Rolls back
  /// + returns a user-facing message on self-intersect / edge flip / outside the
  /// storey outline. Marks the measured wall's perpendicular param (if any) known,
  /// records the axis resolved, recomputes the next prompt.
  pub fn apply_position(&mut self, value: f32) -> Result<(), String>;
  /// Skip — accept the drawn position. Records axis (Translate) or param (Wall),
  /// recomputes.
  pub fn skip_current_position(&mut self);
  /// Advance to the next reference option (wraps). No-op without a prompt.
  pub fn cycle_position_reference(&mut self);

Hook: document/src/dimensioning.rs recompute_prompt calls recompute_position_prompt after setting
current_prompt (covers every existing call site). resync_dimensioning retains
position_skipped_params against active params.

Option building: refs for axis A = outline ring-0 walls whose PERPENDICULAR axis is A (vertical
walls for X), deduped by coordinate within tolerance; Translate pairs each ref with the area wall
minimising |Δ| (Δ > tol); Wall pairs the param's wall with every ref; sorted by current_distance asc.

Tests: free-standing phase area prompts X then Y after lengths; skipped cut length resurfaces as
Wall prompt; living area never prompts; apply translate rejected when pushed outside the storey;
slot sync round-trip preserves position state.

## Slice 3 — controller + UI (shell-engineer)

src/controller/app_controller/dimensioning.rs (NOT editing.rs — don't grow the grandfathered family):
  pub fn apply_position_dimension(&mut self, value: f32) -> Result<(), String>;
     // current_history_snapshot → document.apply_position(value)? → mark_dirty → push snapshot
     // (discard snapshot on rejection — apply_dimension's pattern)
  pub fn skip_position_dimension(&mut self);   // record_history BEFORE the document call
  pub fn cycle_position_reference(&mut self);  // not a mutation — no history
  pub fn position_prompt_for_ui(&self) -> Option<PositionPromptUiData>;
  pub struct PositionPromptUiData {
      pub area_wall: WallPositions,
      pub reference_wall: WallPositions,
      pub axis: ParameterAxis,
      pub current_distance: f32,
      pub option_idx: usize,
      pub option_count: usize,
  }
  // CRITICAL: area-wall endpoints resolve against the LIVE buffer; reference endpoints against
  // document.outline.floor_plan — different VertexId namespaces.

Gates: phase_pick_pending and step_pick_pending each gain
  && self.document.dimensioning.current_position_prompt.is_none()

UI: SketchEvents ApplyPositionDimension(f32), SkipPositionDimension, CyclePositionReference;
app.rs dispatch mirrors ApplyEditDrag/CycleEditDragMeasurement arms; sketch page renders the prompt
panel (textbox prefilled with current_distance, Apply / Skip / "Measure from another wall") whenever
position_prompt_for_ui() is Some, highlighting both walls like the edit-drag render. Re-export
PositionPromptUiData from controller mod.rs alongside DimensionPromptUiData.

Controller tests: phase/step_pick_pending stay false until position entered or skipped; undo after
apply_position_dimension restores geometry AND prompt progress.

## Behaviour traps

- Undo: position progress is in Document → snapshots automatic; skip records history BEFORE; apply
  pushes only on success.
- Serde: new fields serde(default); current_position_prompt serde(skip), re-derived by the hook on
  load/activation. Do NOT bump PARAMETRIC_SNAPSHOT_VERSION.
- Two VertexId namespaces (area vs outline plan) — resolve each against the right plan.
- Translate solve still runs the outside-the-storey guardrail (any_point_outside, apply_dimension's
  tolerance) with full rollback; Wall solve needs apply_dimension's validate/repair/flip-guard.
- Prompt-identity preservation: keep current_idx when (axis, kind) unchanged across recomputes.
- Approximate-stroke principle: a free axis resolves ONLY by entered value, explicit skip, or
  logged zero-reference auto-resolve — never silently from geometry coincidence.
- abandon_phase/step_shape → compute_next_dimension_prompt → hook clears the position prompt.
