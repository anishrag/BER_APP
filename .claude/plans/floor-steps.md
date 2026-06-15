# Floor steps (raise/lower floor areas) — interface spec (api-engineer, 2026-06-10)

Status: approved for implementation. Slices A (datatypes) → B (document) → C (controller) → D (UI).
Open questions resolved by default: amount guard |level| >= 0.01 m only (no clamp); no nested steps
(delete-and-redraw, same as phases); no snapping to phase edges (later nicety); step toggle shows
whenever has_floor_plan (no extra precondition).

## Design summary

A step area is drawn with the exact phase-area mechanic (carve against the available region / cut +
pick a side, dimension, then assign), is SESSION-ONLY controller state like phase regions, and its
durable record is a new additive field on the persisted FloorRegion: `level_m`. Composition with
phase areas is SEQUENTIAL: the existing phase partition runs first (unchanged), then the resulting
regions are re-split by step areas — producing regions carrying every needed
(construction_id, level_m) combination without touching the tested phase partition.

## Key decisions

1. **Durable: `FloorRegion.level_m: f64`, #[serde(default)].** Metres relative to the storey floor
   datum; 0.0 = unstepped, + raised, − lowered. Absolute (not delta): step areas are carved against
   the available region so they never overlap each other; `level_m != 0` <=> step region on reload.
2. **Sequential partition.** `FloorRegions::partition` unchanged; new
   `FloorRegions::with_levels(self, areas: &[(Vec<Point2f64>, f64)]) -> Self` splits each region
   along step boundaries, stamps level on intersected pieces, remainders keep existing level.
   Earlier areas claim first (same rule as partition). Empty areas → self unchanged, NO geo ops.
3. **Session: `StepRegion { polygon: Vec<Point2f32>, level_m: f64 }`** in document; stored
   controller-side `step_regions: HashMap<usize, Vec<StepRegion>>`; NOT in undo history, NOT in the
   parametric snapshot; assign/delete take no history snapshot (settled phase semantics). Rebuilt on
   reload via `step_regions_from_floors`.
4. **Amount entry:** step toolbar; when carved area fully dimensioned (`step_pick_pending`): numeric
   field (metres) + Raise/Lower buttons → AssignStepLevel(±v) + Cancel. Input String on BerApp.

## Slice A — datatypes (data-model-engineer)

`datatypes/src/ber/structural.rs`:
- FloorRegion gains:
  /// Floor level in metres relative to the storey's floor datum: 0.0 = the
  /// unstepped storey floor, positive = raised, negative = lowered (steps).
  #[serde(default)]
  pub level_m: f64,
- impl FloorRegions:
  /// Re-partition the regions by step `areas` (ring + floor level in metres
  /// from the storey datum; + raised, − lowered): each region is split along
  /// the area boundaries, intersected pieces take the area's level, the
  /// remainders keep their existing level. Earlier areas claim first where
  /// areas overlap (same rule as `partition`). Empty `areas` → `self`
  /// unchanged (NO geo ops, so stored points round-trip exactly).
  pub fn with_levels(self, areas: &[(Vec<Point2f64>, f64)]) -> Self
- Mechanical: `level_m: 0.0` in every existing FloorRegion literal — structural.rs (new,
  from_vertex_lists, partition's mk, Default) and gen_3d_vol/src/floor.rs (from_polygons,
  from_polygon_3ds, merge_regions) — compile-keeping only, not expansion.
- One-line seam comment on gen_3d_vol FloorRegionsManip: app resolve writes FloorRegion.height =
  ceiling extrusion (2.4); gen_3d_vol reads height as floor-surface z; when wiring, map level_m →
  the z its floor/riser machinery consumes. (FloorRegion::is_merge_compatible comparing level_m:
  flagged, deferred to wiring.)
- Unit tests: split + remainder, empty-noop (no point changes), serde round-trip incl. old JSON
  without the field.

## Slice B — document (document-engineer)

New `document/src/step.rs` (mirrors phase.rs):
- /// A session-only step area: a world-space (metre) ring + the floor level
  /// inside it (metres from the storey datum; + raised, − lowered).
  #[derive(Debug, Clone)]
  pub struct StepRegion { pub polygon: Vec<Point2f32>, pub level_m: f64 }
- /// Rebuild the session step regions for storey `idx` from its persisted floor
  /// regions (each region with non-zero `level_m` → one step region). Step
  /// regions are session-only; without this, the next commit would re-level
  /// with no areas and flatten the floor. Empty when nothing is stepped.
  pub fn step_regions_from_floors(ber: &Ber, idx: usize) -> Vec<StepRegion>

`document/src/lib.rs`:
- ActiveShape::StepArea variant (additive).
- StoreyDoc.step_shape: Option<ShapeSlot> with #[serde(default)] + match arms in
  sync_active / load_active_into_buffer.
- /// Make `slot` the in-flight step area and the active shape (loading it into
  /// the live buffer, dropping any in-flight arc / edit cursor).
  pub fn begin_step_area(&mut self, slot: ShapeSlot)
- pub use step::{step_regions_from_floors, StepRegion};
- carve_or_cut_in_available + pinned_slot_for reused AS-IS (holes = existing step polygons).

`document/src/resolve.rs`:
- resolve_storey gains `step_regions: &[StepRegion]` (after phase_regions). After storey.floors is
  computed (BOTH branches), apply `storey.floors = storey.floors.with_levels(&step_rings)` ONLY when
  step_regions is non-empty. ensure_storey_constructions unchanged. No stamp analogue.
- SEAM comment just before `storey.walls = WallRegions::rebuild_preserving(...)`:
  // SEAM (deferred until after roofs): riser WALL regions will be derived HERE,
  // from adjacent floor regions with differing `level_m` — one wall region per
  // shared boundary segment, spanning the two levels; segments whose lower side
  // sits below grade (level_m < 0) get an exposed-to-ground exposure, plus
  // below-grade perimeter strips along the outline of lowered regions. The
  // shared-edge extraction belongs in datatypes (a FloorRegions method); the
  // derivation belongs here, alongside the rebuild it must merge with.
- Tests: step_regions_from_floors skips level-0; resolve applies levels over a phase partition;
  also load_storey_into_editor path unaffected (StoreyDoc::from_storey ignores levels — footprint
  union dissolves level boundaries, correct).

## Slice C — controller (shell-engineer)

New `src/controller/app_controller/step_regions.rs`, structural mirror of phase_regions.rs:
  pub fn step_mode(&self) -> bool
  pub fn set_step_mode(&mut self, on: bool)           // mutually exclusive with living/exposure/phase
  pub fn apply_step_region_stroke_event(&mut self, world_points: &[(f32, f32)])  // holes = EXISTING step polygons
  pub fn step_pick_pending(&self) -> bool             // dimensioned + awaiting amount
  pub fn pending_step_polygon(&self) -> Vec<(f64, f64)>
  pub fn pending_step_cut_regions(&self) -> Vec<Vec<(f64, f64)>>
  pub fn choose_step_cut_region(&mut self, world_x: f32, world_y: f32)
  pub fn assign_step_level_to_pending(&mut self, level_m: f32)  // rejects |v| < 0.01; NO history snapshot
  pub fn cancel_pending_step(&mut self)
  pub fn step_region_polygons(&self) -> Vec<(Vec<(f64, f64)>, String)>  // label format!("{:+.2} m", v)
  pub fn delete_step_region(&mut self, index: usize)  // NO history snapshot; mark_dirty only
plus private abandon_step_shape / begin_step_area_dimensioning (the latter DOES push undo snapshot —
the carve mutates Document).

mod.rs fields: step_mode: bool, step_regions: HashMap<usize, Vec<StepRegion>>,
pending_step_cut: Option<[ShapeSlot; 2]>.

storeys.rs: refresh_storey passes step_regions.get(&idx) to resolve_storey; add
  pub(super) fn reconstruct_step_regions_for(&mut self, idx: usize)
called at the three reconstruct_phase_regions_for sites (storeys.rs, persistence.rs ×2).

IMPORTANT two-sided exclusivity: set_phase_mode must gain `step_mode = false` (edit existing fn),
and set_step_mode clears the other three modes. Also load_storey_into_editor's session resets:
clear pending_step_cut + step_mode (mirror pending_phase_cut/phase_mode).

Tests (app_controller/tests.rs): step_area_is_dimensioned_before_its_level_is_assigned (styled on
the phase one); step_area_splits_floor_into_level_regions; step_split_survives_reload
(reconstruct_step_regions_for); zero-amount rejected.

## Slice D — UI (shell-engineer, after C)

sketch.rs SketchEvent: SetStepMode(bool), StepRegionStrokeWorld { points }, AssignStepLevel(f32),
ChooseStepCutRegion { world_x, world_y }, CancelPendingStep, DeleteStepRegion(usize).
app.rs: six dispatch arms → slice-C methods.
sketch.rs plumbing (mirror phase lines): "Floor steps" toggle next to phase toggle (gated on
has_floor_plan); step toolbar — pick pending → amount field + Raise/Lower/Cancel, else region list
with delete buttons (amount String on BerApp); stroke routing step_draw joins needs_world;
outline_on_watermark gains && !step_mode; primary_available gains !step_cut_pending &&
!step_pick_pending; pending-step-cut click handler mirroring phase; rendering reuses
render::render_phase_regions with step data (different tint, implementer's choice).

## Behavior traps (all slices)

- serde: level_m + step_shape #[serde(default)]; ActiveShape::StepArea additive; no field reorders.
- Undo: carve/dimension snapshots; assign/delete do NOT (mark_dirty only). Storage controller-side.
- step_pick_pending false until dimensioning.current_prompt.is_none().
- with_levels empty → no geo ops; resolve_storey skips call when no step regions (round-trip /
  collinearity regression guard).
- After a step splits a phase zone, phase_regions_from_floors returns pieces — don't assert exact
  region counts across a step in tests.
- gen_3d_vol: only mechanical literal inits + the seam comment. No expansion.

## gen_3d_vol judgement

Already models per-region levels + risers via FloorRegion.height (raise_lower_polygon,
get_floor_height_at_point, walls.rs::add_from_floor_discontinuities = eventual riser consumer).
Latent conflict: app writes height = ceiling extrusion; gen_3d_vol reads height = floor z. level_m
RESOLVES this at wiring time (map level_m → z; height stays extrusion). Nothing else changes now.
