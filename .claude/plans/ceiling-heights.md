# Ceiling heights — interface spec (api-engineer, 2026-06-10)

Status: approved. Slices 1+2 land as ONE coordinated change (workspace doesn't compile between);
then 3+4 (shell); then review. Open questions resolved by default: re-editing a set height is
DEFERRED to the roofs feature (this spec only opens the first-3D prompt per scope); full-house
stacking keeps ceiling height as the exact storey z-step until DEAP export says otherwise;
validation is height > 0 (finite) only.

## Verified ground truth

- FloorRegion.height is only ever WRITTEN as the 2.4 placeholder in live paths (resolve.rs floors/
  living areas/partition, persistence.rs:287, test fixtures). gen_3d_vol writes real values but is
  dormant. DB = JSON blob, no deny_unknown_fields anywhere → REMOVING the field is load-compatible
  and loses no real data.
- Read by: storey_3d.rs:52,85, full_house_3d.rs:51, storey_2d.rs:394 (corner labels — today always
  "h=2.40m"), gen_3d_vol (as floor z — the floor.rs:31 SEAM), tests.
- CeilingRegion already has id, polygon: Polygon3D (per-vertex z = the height), opes,
  construction_id → NO new field; per-vertex z is right for later sloped roofs.
- resolve_storey clones ber.storeys[idx] → a durable Storey field flows in with NO signature change.

## Model decisions

- `Storey.ceiling_height_m: Option<f64>` (#[serde(default)]) = durable per-storey default;
  derived CeilingRegions carry actual heights as polygon z. None = honest "not set" gate state.
- DEFAULT_CEILING_HEIGHT_M survives ONLY as `pub(crate) PROVISIONAL_WALL_EXTRUSION_M: f64 = 2.4`
  in document/resolve.rs (wall quads need a non-degenerate top pre-height; 3D is gated so it's
  never user-visible; walls re-extrude on next materialize after the height is set).
- Ceiling layout ≠ floor partition: default derivation = ONE ceiling region per OUTLINE RING
  (footprint), not per floor region.

## Slice 1 — ber_model + gen_3d_vol compile-keeping (data-model-engineer)

structural.rs:
- FloorRegion loses `height` (doc: floors carry no vertical extent; level_m is their datum,
  ceilings own heights). Fields: id, polygon, construction_id, level_m(serde default).
- Signatures drop height: FloorRegions::new(vertices, construction_id);
  from_vertex_lists(polygons, construction_id); partition(outline, areas, remainder_cid).
  with_levels signature unchanged (its mk stops copying height).
- impl CeilingRegions:
  /// One FLAT ceiling region per footprint ring: every vertex at z = `height_m`
  /// (metres above the storey's floor datum), all sharing `construction_id`.
  /// Rings with < 3 vertices are skipped. The default layout the assessor's
  /// typed height creates; later roof work re-splits/refines these regions.
  pub fn default_for_footprint(rings: Vec<Vec<Point2f64>>, height_m: f64, construction_id: ItemId) -> Self
- Storey gains:
  /// Default ceiling height in metres above this storey's floor datum.
  /// `None` until the assessor enters it — gates 3D viewing; when `Some`,
  /// resolve derives the default ceiling regions and extrudes walls to it.
  #[serde(default)] pub ceiling_height_m: Option<f64>,
- impl Ber:
  /// Stamp the default ceiling height: on storey `storey_idx` when given, else
  /// on every storey that doesn't already have one (the all-storeys 3D prompt —
  /// one typed value fills the gaps, never overwrites a set storey).
  /// Caller has validated `height_m > 0`.
  pub fn set_default_ceiling_height(&mut self, storey_idx: Option<usize>, height_m: f64)

gen_3d_vol (compile-keeping ONLY): re-point every FloorRegion.height read/write to level_m
(from_polygons, raise_lower_polygon, get_floor_height_at_point, get_clockwise_3d_polygon...,
merge equality floor.rs:316,466, walls.rs:218-219, lib.rs:316 add_ceiling_with_height) and DELETE
the floor.rs:31 SEAM comment — this change RESOLVES it (level_m = floor z unambiguously; ceiling
height arrives via Storey.ceiling_height_m/CeilingRegions at wiring). add_ceiling_with_height keeps
"metres above floor surface" (level_m + height). Tests' height fixtures → level_m.

Tests: update structural.rs:856,868,896,903,932,951,967,979,992 (drop 2.4 args; DELETE the
r.height==2.4 inheritance assert at 951, keep construction_id), tests/database_test.rs:239.
New: default_for_footprint (z stamping, <3-vertex skip); set_default_ceiling_height both scopes
(None-scope fills only missing); old JSON WITH a "height" field still deserializes (serialize,
inject "height": 2.4, deserialize).

## Slice 2 — document resolve/load (document-engineer; same change as slice 1)

resolve.rs (no public signature changes to resolve_storey/ensure_storey_constructions):
- pub(crate) const PROVISIONAL_WALL_EXTRUSION_M: f64 = 2.4; (doc: placeholder top for derived wall
  quads until the storey's ceiling height is set — never visible in 3D (gated); walls re-extrude on
  the next materialize after the height is entered.)
- DELETE DEFAULT_CEILING_HEIGHT_M + its lib.rs re-export (controller mod.rs:26 import +
  persistence.rs:287 use disappear with the signature change; tests.rs:1271 asserts the provisional
  literal or the set height).
- ensure_storey_constructions also ensures the default CEILING construction (same unconditional
  pattern; doc comment gains "plus the default ceiling").
- resolve_storey: floors/living areas/partition drop height args; walls:
  let wall_top = storey.ceiling_height_m.unwrap_or(PROVISIONAL_WALL_EXTRUSION_M) → rebuild_preserving;
  roofs (new, after floors): when Some(h) and outline has a ≥3-vertex ring,
  storey.roofs = CeilingRegions::default_for_footprint(polygons_f64, h, ceiling_cid) (cid looked up
  read-only like floor_cid); when None leave roofs untouched.
- SEAM comment at the roofs derivation site: re-deriving roofs every materialize wipes any future
  hand-refined ceiling layout — fine today (only derived content exists); the later roofs feature
  must add session ceiling areas reconstructed from persisted regions (step/phase pattern) BEFORE
  users can refine.

Tests: golden projection updated (drop floor-height assert resolve.rs:207; wall-z against the
provisional const); load.rs/step.rs/phase.rs fixtures drop 2.4 args. New: (a) height set → roofs =
one region per ring all z=h, walls extrude to h; (b) unset → roofs untouched, walls provisional;
(c) re-resolve after outline edit re-derives roofs to the new footprint.

## Slice 3 — controller gate + prompt (shell-engineer)

New src/controller/app_controller/ceiling.rs; mod.rs field pub(super) ceiling_height_prompt: bool
(session-only, OUTSIDE undo — same deliberate choice as phases/steps; overwriting the value is its
own undo).
  /// Ceiling heights for the CURRENT view scope: Some(i) → storey i set;
  /// None ("All storeys") → every storey set. False with no record/storeys.
  pub fn ceiling_heights_set(&self) -> bool
  /// Is the first-3D "set default ceiling height" prompt open?
  pub fn ceiling_height_prompt_pending(&self) -> bool
  /// Apply the typed default height (m): rejects non-finite/≤0; stamps the
  /// current scope (Ber::set_default_ceiling_height), closes the prompt,
  /// switches to Storey3D, marks dirty.
  pub fn set_default_ceiling_height(&mut self, height_m: f32) -> Result<(), String>
  /// Dismiss the prompt without setting a height (stays in 2D).
  pub fn cancel_ceiling_height_prompt(&mut self)
Gate in strokes.rs::set_sketch_mode (check actual fn — display mode setter): requesting Storey3D
(and full-house 3D) while !ceiling_heights_set() sets ceiling_height_prompt = true and does NOT
switch mode. Controller tests: 3D request without height opens prompt + stays 2D; apply switches
to 3D and materialize() yields roofs; cancel stays 2D; all-storeys scope fills only missing.

## Slice 4 — UI (shell-engineer, with slice 3)

SketchEvents SetDefaultCeilingHeight(f32), CancelCeilingHeightPrompt; app.rs arms; BerApp
ceiling_height_input: String. sketch.rs render gains ceiling_height_prompt + ceiling_heights_set
params; mode toolbar shows the 3D option with a hover hint when unset ("Sets the default ceiling
height first — 3D needs it"); prompt = step_toolbar-style inline row (label + TextEdit + Set/
Cancel, parse_dimension_expression, clear on apply/cancel).
storey_3d.rs: draw_extruded_region/projected_bb_for_storey take ceiling_z: f64 instead of reading
region.height; render/render_at_z_offset read storey.ceiling_height_m; None → hint text ("Set a
ceiling height to view in 3D.") — defensive (storey-tab switch in 3D can land on an unset storey).
full_house_3d.rs: z-step per storey = ceiling_height_m; any None → hint text + return (replaces
unwrap_or(2.4) at line 51). storey_2d.rs draw_corner_height_labels: re-point to storey.roofs
polygon z (nothing drawn before heights set — replaces the always-2.40 labels).
render/mod.rs:1127 test helper drops the 2.4 arg.

## Behavior traps

- Serde: removal is deserialize-compatible (verified: no deny_unknown_fields; old values all 2.4
  placeholder); ceiling_height_m #[serde(default)]; NO PARAMETRIC_SNAPSHOT_VERSION bump (Document
  untouched).
- push_storey/Storey::default → new storeys None → 3D re-gates per storey (correct).
- Tests asserting 2.4 enumerated above — update WITH semantics, don't delete coverage.
- gen_3d_vol judgement: dormant; compile-keeping re-point to level_m resolves the height-vs-level
  convention conflict; SEAM comment deleted as resolved.
