# Plan: Construction-phase (year-of-construction) area tagging

## Goal

Let the assessor define the house's construction year(s) up front, then **select an area
of a storey and assign it a construction phase**. The phase is the year-of-construction;
it determines the DEAP age band downstream. Selection reuses the living-area mechanism.
Tagging is the first stage of the 3D side, doable with a single storey.

## Settled design (from discussion)

- **Phase == year of construction.** `ConstructionPhase` (`datatypes/src/ber/common.rs:42`:
  `Original`, `Extension1..4`, `Other(String)`) is the label; the year lives in
  `ConstructionPhases { years: HashMap<ConstructionPhase, Year> }`
  (`common.rs:53`) on **`Ber.construction_phases`** (`ber_struct.rs`). This field already
  exists and is currently always empty.
- **Phase already participates in the construction identity.** `ensure_{wall,floor,ceiling}
  _construction(type, makeup, phase)` (`structural.rs:173/201/229`) finds-or-creates by the
  triple `(type, makeup, phase)`. Every caller currently passes `Default` = `Original`, so
  phase is dead today. Feed a real phase and two identical makeups in different years
  correctly become two constructions (→ two DEAP elements). **No change to the construction
  model.**
- **The durable record of a surface's phase is its `construction_id`** (already persisted).
- **Phase polygons are session-only controller state — NOT persisted in `Ber`.** Their only
  job is to stamp *newly drawn* surfaces during the session. Symmetric with the
  upward-propagation rule, which also needn't survive a session.
- **`gen_3d_vol` is untouched** — phase flows through `construction_id`, which it already
  consumes. No selection logic goes near it.

### Commit-time resolution rule (per surface)

1. Read the surface's current phase via its `construction_id` → the construction's
   `base.construction_phase` (and keep its `construction_type` + `base.construction_makeup`).
   A surface with no/Uninitialised construction reads as `Original` / defaults.
2. New phase:
   - **Representative point inside some phase polygon** → that polygon's phase
     (**polygon always wins**, even over an existing non-`Original` phase — re-tagging
     re-assigns).
   - **In no polygon, but current phase ≠ `Original`** → keep it (never clobber a tag).
   - **In no polygon, current phase == `Original`** → `Original` (no-op).
3. `construction_id = ensure_*_construction(existing_type, existing_makeup, new_phase)` —
   only the phase axis changes; type/makeup preserved.

Net: a polygon keeps stamping anything new in its footprint during the session; reload (no
polygons) leaves every tagged surface exactly as committed; only genuinely-untagged
surfaces sit at `Original`.

## Architecture placement

- **Geometry** (`carve_subarea` `lib.rs:203`, `split_by_cut` `lib.rs:548`, `clamp_inside`
  `lib.rs:1476`, `contains_point` `lib.rs:1450`) — already feature-agnostic in
  `manip_plan`. Reused as-is; no moves.
- **Workflow** — new controller module `src/controller/app_controller/phase_regions.rs`,
  sibling to `living_areas.rs`. Owns the mode, the in-memory regions, the pending-pick
  state, and the commit resolution sequencing.
- **Result** — surface `construction_id` (existing, persisted). Plus session-only
  `Vec<PhaseRegion>` in the controller. **No new datatypes type.**

## Stages

### Stage 1 — Capture construction dates at NewBer → populate `construction_phases`

- `datatypes/src/ber/common.rs`: add small helpers on `ConstructionPhases` if needed —
  `defined_phases() -> Vec<ConstructionPhase>` (ordered: Original, then Extension1..n that
  have years) for the tagging picker, and `set_year(phase, Year)`. (`get_year` exists but
  panics on missing — picker must only offer defined phases.)
- `src/ui/pages/new_ber.rs` + `NewBerState` (`src/ui/app.rs:73`): add a **house construction
  year** field (→ `Original`) and an **"add extension"** repeater (→ `Extension1..4`, each a
  year), capped at 4. Parse/validate to `Year` (u32); surface a parse error like the
  existing `date_error`.
- `src/ui/app.rs` create path (~519–547): build a `ConstructionPhases` from the inputs and
  hand it to the controller.
- `src/controller/app_controller/mod.rs` `create_ber`: accept the `ConstructionPhases` and
  set `ber.construction_phases` before the record is stored. (Signature gains one param.)

### Stage 2 — Phase-tagging workflow (`phase_regions.rs`, session-only)

Controller-side type (NOT datatypes):
```rust
struct PhaseRegion { polygon: Vec<Point2f32>, phase: ConstructionPhase } // world/metre ring
```
State on `AppController` (`mod.rs` ~line 315 alongside `living_areas`):
```rust
phase_regions: HashMap<usize, Vec<PhaseRegion>>,      // keyed by storey index, session-only
phase_mode: bool,
pending_phase_polygon: Option<Vec<Point2f32>>,        // carved, awaiting phase pick
```
Methods (mirror `living_areas.rs`):
- `set_phase_mode(on)` — toggle; sets `EditTools::DrawPerpWalls` like `set_living_area_mode`.
- `apply_phase_region_stroke_event(world_points)` — `clamp_inside` → straight-cut detection
  (`split_by_cut`, with a region picker) else `carve_subarea`; store the resulting ring in
  `pending_phase_polygon`. Reuses the same geometry calls as living areas.
- `assign_phase_to_pending(phase)` — move pending ring into `phase_regions[current_storey]`,
  `push_history_snapshot`, then `commit_to_ber()` (which now resolves phases).
- `choose_phase_cut_region(x, y)` — analog of `choose_cut_region` for the split path.
- `delete_phase_region(idx)` then re-`commit_to_ber()`.

UI (`src/ui/pages/sketch.rs` + `src/ui/app.rs`):
- New `SketchEvent`s beside the living-area ones (`sketch.rs:244`): `SetPhaseMode(bool)`,
  `PhaseRegionStrokeWorld { points }`, `AssignPhase(ConstructionPhase)`,
  `ChoosePhaseCutRegion { world_x, world_y }`, `DeletePhaseRegion(usize)`.
- Stroke capture: extend the `living_area_draw` world-conversion branch (`sketch.rs:733`) so
  phase mode emits `PhaseRegionStrokeWorld` (same world-coord path).
- Route them in `src/ui/app.rs` (~370) to the new controller methods.
- A phase-mode toggle button + a phase-picker panel shown when `pending_phase_polygon`
  is `Some` (list = `construction_phases.defined_phases()`), and a list of tagged regions
  with delete. `ConstructionPhase` crosses the boundary as owned data — fine.
- Render tagged regions as tinted overlays (reuse the living-area overlay path in
  `src/ui/render/`). Visual polish can be minimal for the MVP.

### Stage 3 — Commit-time phase resolution

- Add `resolved_phase_for(point, existing_phase, storey_idx) -> ConstructionPhase` on the
  controller implementing the rule above; containment via `manip_plan` point-in-ring
  (reuse `FloorPlan::contains_point` / a `geom` helper — **no coordinate math inlined in the
  controller**). Representative point per surface: floor/ceiling region → ring centroid;
  wall region → base-segment midpoint — both as `manip_plan::geom` helpers fed owned
  vertices.
- In `commit_to_ber` (`mod.rs` ~940) and `persist` (~984): replace the `Default::default()`
  phase handed to every `ensure_*_construction` with the resolved phase. Factor a single
  shared helper so both paths agree (they both assign `construction_id`s — see Risks).
- Apply to `walls`, `floors`, `roofs`, and `living_areas` regions of the committed storey.

## Files

- `datatypes/src/ber/common.rs` — `ConstructionPhases::defined_phases` / `set_year`.
- `src/ui/pages/new_ber.rs`, `src/ui/app.rs` — date capture + new events + routing.
- `src/controller/app_controller/mod.rs` — `create_ber` param, new state fields,
  commit/persist phase resolution.
- `src/controller/app_controller/phase_regions.rs` (NEW) — the workflow.
- `src/ui/pages/sketch.rs` — new `SketchEvent`s + phase-mode stroke capture.
- `src/ui/render/*` — tint tagged regions (reuse living-area overlay).
- `manip_plan/src/geom.rs` — centroid / segment-midpoint helpers if not already present
  (point-in-ring already exists behind `contains_point`).

## Tests (keep `cargo test --workspace` green; fmt + boundary scan clean)

Controller, beside the living-area tests:
1. `phase_polygon_stamps_contained_surfaces` — tag an area `Extension1`, commit; surfaces
   inside resolve to a construction with `phase == Extension1`, outside `Original`.
2. `polygon_overwrites_existing_phase` — surface at `Extension2`, polygon `Extension1` over
   it, commit → `Extension1`.
3. `preserve_existing_phase_when_no_polygon` — surface `Extension1`, clear polygons, commit
   → stays `Extension1`; an `Original` surface stays `Original`.
4. `new_ber_populates_construction_phases` — `create_ber` with phases sets
   `ber.construction_phases`.
datatypes: `defined_phases` ordering / `set_year` round-trip (trivial).

## Deferred (explicitly out of MVP)

- **Upward propagation**: session rule + re-project the polygon onto a newly drawn storey +
  confirm-once-dimensioned. Hook point: `add_storey` (`mod.rs:744`). Stage-2 feature.
- **Cross-session auto-tagging** of surfaces drawn in a later session — intentionally not
  done (the preserve-existing rule covers what matters).
- **Boundary surfaces** (a wall between two phases): MVP assigns one phase per surface by a
  single representative point; per-surface splitting deferred (mirrors the wall-exposure
  diagonal-split deferral).

## Risks / edge cases

- **`commit_to_ber` vs `persist` divergence.** Both assign `construction_id`s today
  (`persist` re-derives a filtered copy from points — see [[persistence-model]]). The phase
  rule must produce the same result in both, or persist could overwrite a tag with
  `Original`. Prefer: `commit_to_ber` resolves on the in-memory model; `persist` copies the
  already-resolved `construction_id` for unchanged geometry and only re-resolves genuinely
  new regions. Verify against the current `persist` body before coding.
- **Representative-point choice** for an L-shaped/cut region — centroid can fall outside a
  concave ring. Use an interior point (e.g. first-vertex-adjacent or a point-in-polygon-safe
  sample), not a raw average. `contains_point` already handles concavity for the test side.
- **Picker offers only defined phases** — guard against `get_year` panicking on an undefined
  phase by sourcing the list from `defined_phases()`.

## Verification

`cargo test --workspace`, `cargo fmt --all --check`, boundary scan (no `egui` outside
`src/ui`, no coordinate math inlined in the controller, no `nalgebra` leak). Manual: new BER
→ enter house year + an extension year → dimension storey 0 → enter phase mode → carve an
area → pick `Extension1` → confirm surfaces inside resolve to the extension construction and
the rest stay `Original`; reload and confirm tags survive.
