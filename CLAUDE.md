# BER_APP

Rust app for an Irish BER (Building Energy Rating) assessor, eventually targeting an Android tablet so the assessor can transfer their manual workflow onto a device.

## Workspace layout

- `domain/` — shared primitives (`Point2f32`/`Point2f64`/`Point3f64`, sketch enums). Depends on `serde`, `strum_macros`, `nalgebra`.
- `datatypes/` — BER + DEAP data model. Pure data, no I/O.
- `sketch/` — stroke capture engine with a `SketchModeBehaviour` trait. Depends on `domain` only.
- `manip_2d_plan/` — floor-plan post-processing (simplify → orthogonalize → parametric). Depends on `domain` only.
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

## Conventions

- Use the `Point2*` / `Point3*` typedefs from `domain::points`, not raw `nalgebra::Point*` or tuples.
- Page-specific state lives on `BerApp` (`src/ui/app.rs`); page render functions emit `UiEvent`s and stay thin.
- Persistence goes through `BerRepository` (`src/engine/ber_repository.rs`).
- `cargo check --workspace` runs on Stop — keep the tree compiling.

## Target platform

Android tablet (not iOS — see prior analysis: egui's iOS path has unresolved touch-perf and keyboard issues). Sideloaded APK; no Play Store. Touch polish via egui's built-in multi-touch (pinch/zoom on the sketch canvas) plus `egui_virtual_keyboard` for text entry.
