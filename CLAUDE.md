# BER_APP

Rust app for an Irish BER (Building Energy Rating) assessor, eventually targeting an Android tablet so the assessor can transfer their manual workflow onto a device.

## Workspace layout

- `domain/` — shared primitives (`Point2f32`/`Point2f64`/`Point3f64`, sketch enums). Depends on `serde`, `strum_macros`, `nalgebra`.
- `datatypes/` — BER + DEAP data model. Pure data, no I/O.
- `sketch/` — stroke capture engine with a `SketchModeBehaviour` trait. Depends on `domain` only.
- `manip_2d_plan/` — floor-plan post-processing (simplify → orthogonalize → parametric). Depends on `domain` only. Owns the parametric floor-plan model and **all geometry algorithms over it**: `FloorPlan` (public API) wraps `ParametricPlan` (rings of parametric vertices + shared parameters); `geom.rs` holds pure coordinate math. (See "Responsibility placement".)
- `gen_3d_vol/` — 2D floor plan → 3D volume (walls/floors/ceilings). Not currently wired into the app.
- `src/` — the eframe app: `controller/` + `engine/` (persistence + processing) + `ui/`. Page flow: Splash → Home → NewBer / Sketch / KeyDeets / MoreDeets.

## Load-bearing rules

1. **UI dependencies (`egui`, `eframe`, `egui_*`) live only in `src/ui/**`.** Controller, engine, and workspace crates stay UI-toolkit-agnostic — protects the option of swapping the UI layer later.
2. **`nalgebra` is confined to `domain/**`.** Other crates use the `Point2f32` / `Point2f64` / `Point3f64` typedefs from `domain::points`.
3. **No `use slint`** anywhere in source. Slint was an earlier toolkit choice; the project is on egui now.
4. **Database access (`rusqlite`) only through `src/engine/`.** Use `BerRepository` from controllers and UI.
5. **Don't expand `gen_3d_vol/`** until it's wired into the controller — ~3,700 LOC currently unused.
6. **Don't uncomment large commented blocks** (notably in `manip_2d_plan/src/lib.rs`) without a concrete caller.

Rules 1–4 are enforced by `.claude/scripts/check-boundaries.sh`, which runs on Edit/Write and on Stop. More context in `.claude/rules/`.

## Responsibility placement (keep the controller thin)

**The controller (`src/controller/`) is a thin orchestrator. Domain logic belongs in the module that owns the concern — never inlined in the controller.** This isn't script-enforced; it's the easiest rule to slip on, so apply it deliberately on every change.

Each module owns a concern. Put logic where its concern lives:

| Concern | Home |
| --- | --- |
| Floor-plan geometry & the parametric model — points, vertices, parameters, edges, areas, intersections, orthogonality, and the algorithms over them (extend, trim, splice, dimension-solve, repair, collapse, validate, collinearity) | `manip_2d_plan` — `FloorPlan` (public API) over `ParametricPlan` (internals); pure coordinate math in `geom.rs` |
| Stroke/pen capture (raw input → strokes, mode behaviours) | `sketch` |
| BER + DEAP data model and pure data transforms (no I/O) | `datatypes` |
| Shared coordinate primitives & vector math (`Point2*`/`Point3*`) | `domain` |
| 2D plan → 3D volume | `gen_3d_vol` (dormant — see scope rule) |
| Persistence / SQL | `src/engine` (`BerRepository`) |
| Rendering & per-page UI state | `src/ui` |

**The test for any new logic:** if it only manipulates *one module's* domain and needs nothing from the app shell (no UI, no DB, no cross-step workflow state), it belongs in that module — add or call a method there instead of inlining it. Don't inline a module's domain work in the controller: no coordinate math / vertex-edge walks (→ `manip_2d_plan`), no SQL strings (→ `src/engine`), no widget/layout code (→ `src/ui`), no data-model derivations (→ `datatypes`). Prefer one high-level call (`floor_plan.extend(stroke)?`) over a multi-step pipeline in the controller, and pass owned data in / get results out.

**What the controller legitimately owns:** sequencing the modules (the *workflow* — e.g. the dimensioning loop deciding when to scale / repair / roll back), app-shell state that spans steps (`DimensionState`, undo/redo `HistorySnapshot`, `current_record` + `commit_to_ber`, collinearity-review cursor, edit-mode selection), the `UiEvent`/stroke plumbing, and mapping a module `Error` into a user-facing message. Example split: `apply_dimension` owns the prompt/scale/known-params/rollback workflow and *calls* `set_wall_length` / `repair_unset_params` / `collapse_trim_wall` (all in `manip_2d_plan`).

**Allowed exception:** the edit-mode *measurement-prompt builders* (`populate_vertex_drop_prompt`, `populate_edit_drag_prompt`) and the `snap_single`/`joint_solve` solvers compute distances inline because they're bound to the controller's `VertexMeasurement` prompt types — but their pure-math primitives (`project_to_circle`, `intersect_circles`, `solve_axis_wall`) still live in `manip_2d_plan::geom`. New code shouldn't grow more such exceptions.

When in doubt, push the logic down into the owning module and have the controller call it.

## Tests and push gate

- `cargo test --workspace` is the test suite. Should be green at all times on `main`.
- **One-time setup per clone:** `./scripts/setup-githooks.sh` (points `core.hooksPath` at `.githooks/`). After that, `git push` to `main` runs `cargo test --workspace` first and refuses on failure. Pushes to feature branches go through unblocked.
- GitHub Actions (`.github/workflows/test.yml`) runs `cargo fmt --check`, `cargo test --workspace`, and the architecture boundary scan on every push and PR.
- Bypass for emergencies only: `git push --no-verify` (skips the local gate; CI still gates on PR).

## Conventions

- Use the `Point2*` / `Point3*` typedefs from `domain::points`, not raw `nalgebra::Point*` or tuples.
- Page-specific state lives on `BerApp` (`src/ui/app.rs`); page render functions emit `UiEvent`s and stay thin.
- Persistence goes through `BerRepository` (`src/engine/ber_repository.rs`).
- `cargo check --workspace` runs on Stop — keep the tree compiling.

## Target platform

Android tablet (not iOS — see prior analysis: egui's iOS path has unresolved touch-perf and keyboard issues). Sideloaded APK; no Play Store. Touch polish via egui's built-in multi-touch (pinch/zoom on the sketch canvas) plus `egui_virtual_keyboard` for text entry.
