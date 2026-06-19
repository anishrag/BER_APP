# BER_APP

Rust app for an Irish BER (Building Energy Rating) assessor, eventually targeting an Android tablet so the assessor can transfer their manual workflow onto a device.

## Workspace layout

- `domain/` — shared primitives (`Point2f32`/`Point2f64`/`Point3f64`, sketch enums). Depends on `serde`, `strum_macros`, `nalgebra`.
- `datatypes/` — BER + DEAP data model. Pure data, no I/O.
- `sketch/` — stroke capture engine with a `SketchModeBehaviour` trait. Depends on `domain` only.
- `manip_plan/` — floor-plan post-processing (simplify → orthogonalize → parametric). Depends on `domain` only. Owns the parametric floor-plan model and **all geometry algorithms over it**: `FloorPlan` (public API) wraps `ParametricPlan` (rings of parametric vertices + shared parameters); `geom.rs` holds pure coordinate math. (See "Responsibility placement".)
- `document/` — the editable storey **model** the app edits: one storey's shapes (`outline` + `living_areas` + in-flight `phase_shape`), which is active, the live editing buffer (the active shape's `floor_plan`/`dimensioning`/`edit_state`/`pending_arc`), per-wall exposure, and the model *operations* over them (slot switching, cross-shape geometry, the dimensioning loop, exposure painting, drag geometry). UI-free and DB-free — **builds standalone** (so a future native shell can drive it as a library). Depends on `domain`, `datatypes`, `manip_plan`. Logs via the `log` facade, not `println!`.
- `src/` — the eframe app **shell**: `controller/` drives the `document` (UiEvent dispatch, undo/redo, persistence orchestration, session/interaction state, UI-data derivation); `engine/` is persistence; `ui/` is egui. Page flow: Splash → Home (New/Load) → BER workspace [Key Details | Sketch | More Details].

## Load-bearing rules

1. **UI dependencies (`egui`, `eframe`, `egui_*`) live only in `src/ui/**`.** Controller, engine, and workspace crates stay UI-toolkit-agnostic — protects the option of swapping the UI layer later.
2. **`nalgebra` is confined to `domain/**`.** Other crates use the `Point2f32` / `Point2f64` / `Point3f64` typedefs from `domain::points`.
3. **No `use slint`** anywhere in source. Slint was an earlier toolkit choice; the project is on egui now.
4. **Database access (`rusqlite`) only through `src/engine/`.** Use `BerRepository` from controllers and UI.
5. *(Retired 2026-06: the dormant `gen_3d_vol/` crate was dissolved.)* Floor/roof/wall region + volume derivation lives in `datatypes` + `document` (`resolve_storey`); the one surviving primitive, `Polygon3D::split_by_line`, moved to `datatypes::ber::structural`.
6. **Don't uncomment large commented blocks** (notably in `manip_plan/src/lib.rs`) without a concrete caller.
7. **`document/` is the editing model — keep it UI-free and DB-free.** Editing *logic* (model mutation over a storey's shapes) goes in `document` as `Document` methods; the controller is the shell that drives it. No `egui`/`eframe` and no `rusqlite` in `document` (rules 1 + 4 already enforce this via the scan). It must keep building standalone (`cargo build -p document`).

Rules 1–4 are enforced by `.claude/scripts/check-boundaries.sh`, which runs on Edit/Write and on Stop. More context in `.claude/rules/`.

## Responsibility placement (keep the controller thin)

**The controller (`src/controller/`) is a thin shell. Editing state + logic live in `document`; geometry in `manip_plan`. Never inline domain logic in the controller.** This isn't script-enforced; it's the easiest rule to slip on, so apply it deliberately on every change.

Each module owns a concern. Put logic where its concern lives:

| Concern | Home |
| --- | --- |
| Floor-plan geometry & the parametric model — points, vertices, parameters, edges, areas, intersections, orthogonality, and the algorithms over them (extend, trim, splice, dimension-solve, repair, collapse, validate, collinearity) | `manip_plan` — `FloorPlan` (public API) over `ParametricPlan` (internals); pure coordinate math in `geom.rs` |
| The editable storey model & model mutation over it — shapes + active selection, the live editing buffer, the dimensioning loop, exposure painting, drag geometry, cross-shape relations (carve-against-outline, pinned params) | `document` — `Document` (UI-free, DB-free) |
| Stroke/pen capture (raw input → strokes, mode behaviours) | `sketch` |
| BER + DEAP data model and pure data transforms (no I/O) | `datatypes` |
| Shared coordinate primitives & vector math (`Point2*`/`Point3*`) | `domain` |
| Persistence / SQL | `src/engine` (`BerRepository`) |
| Rendering & per-page UI state | `src/ui` |

**The test for any new logic:** if it only manipulates *one module's* domain and needs nothing from the app shell (no UI, no DB, no cross-step workflow state), it belongs in that module — add or call a method there instead of inlining it. Don't inline a module's domain work in the controller: no coordinate math / vertex-edge walks (→ `manip_plan`), no model mutation over a storey's shapes (→ `document`), no SQL strings (→ `src/engine`), no widget/layout code (→ `src/ui`), no data-model derivations (→ `datatypes`). Prefer one high-level call (`document.apply_dimension(v)?`) over a multi-step pipeline in the controller, and pass owned data in / get results out.

**What the controller legitimately owns:** sequencing the modules (the *workflow* — e.g. snapshot → mutate → persist), app-shell state that spans steps (undo/redo — `HistorySnapshot` is a `Document` clone — plus `current_record` + `commit_to_ber`, the cut-pick + collinearity-review cursors, the in-flight measurement prompts), the `UiEvent`/stroke plumbing, deriving UI-facing view data, and mapping an `Error` into a user-facing message. Example split: `AppController::apply_dimension` snapshots for undo, calls `document.apply_dimension(value)` (the model mutation — scale / set / repair / outside-the-storey guardrail, itself calling `manip_plan`), then persists.

**Allowed exception:** the edit-mode *measurement-prompt builders* (`populate_vertex_drop_prompt`, `populate_edit_drag_prompt`) and the `snap_single`/`joint_solve` solvers compute distances inline because they're bound to the controller's `VertexMeasurement` prompt types — but their pure-math primitives (`project_to_circle`, `intersect_circles`, `solve_axis_wall`) still live in `manip_plan::geom`. New code shouldn't grow more such exceptions.

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

Android tablet (not iOS — see prior analysis: egui's iOS path has unresolved touch-perf and keyboard issues). Sideloaded APK; no Play Store. The port landed 2026-06 (plan: `.claude/plans/android-port.md`):

- Entry points: `src/main.rs` (desktop) and `src/android.rs` (cfg'd `android_main`) share `ber_app::bootstrap(&Paths)`; eframe is wired in `src/ui/mod.rs` only (`run` / `run_android` → one `launch`).
- Touch: two-finger navigation + stroke retraction in `src/ui/pages/sketch/touch.rs`; on-screen numeric/qwerty pads in `src/ui/widgets/pads.rs` (Android always; desktop via `BER_TOUCH=1`); tooltips via `HintExt::hint` (no-op on Android). `egui_virtual_keyboard` was evaluated and rejected (stale, pre-0.33).
- Packaging: `scripts/build-android.sh` (cargo-ndk + the Gradle project in `android/`, arm64 only) — see `android/README.md` for one-time toolchain setup and signing.
