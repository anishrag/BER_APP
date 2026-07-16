---
description: Crate-level dependency boundaries enforced by .claude/scripts/check-boundaries.sh.
---

# Architecture boundaries

These are hard rules. The boundary script at `.claude/scripts/check-boundaries.sh` runs on every Edit/Write and on Stop, and exits 2 (blocking) when a rule is violated. Treat the rule file as the up-front context so you don't trip the linter in the first place.

## UI confined to `src/ui/**` and `render/**`

**Rule:** `egui`, `eframe`, `egui_extras`, `egui_datepicker`, `egui_field_editor` may not be imported outside `src/ui/` and `render/`. Within `render/` only `egui` itself is legitimate (its Cargo.toml declares no eframe) — it must stay window-stack-free.

**Why:** Earlier in this project's life the UI toolkit changed (slint → egui), and we want the option open. By keeping every other crate UI-agnostic, swapping the UI layer later (e.g. to Slint for iOS, or to a native Android shell calling Rust as a library) is a contained rewrite of `src/ui/` + `render/` rather than a full project rewrite. `render/` (the `ber_render` crate) is the shared drawing core — the 3D house / elevation renderers and their projection + fit math — packaged as a library so BER_deap's report generator drives the SAME tested drawing code headlessly and serializes the epaint shapes to SVG. It is UI code by nature; it lives outside `src/ui/` only so another repo can link it.

**How to apply:** Render code, egui types, and widget handling stay in `src/ui/` (interactive) or `render/` (pure painting shared with BER_deap — takes `&egui::Painter`, never `&mut egui::Ui`/`Response`, no interaction, no textures). The controller (`src/controller/`) and engine (`src/engine/`) communicate with UI only via owned data and the `UiEvent` enum; they may not import `ber_render` either — shared values both need (e.g. the default orbit angles) live in `domain`. Workspace data crates (`ber_model`, `domain`, `sketch`, `manip_plan`, `document`) never see an egui type.

## `document/` is the editing model — UI-free and DB-free

**Rule:** `document/` (the editable storey model the controller drives) imports neither UI deps (`egui`/`eframe`/`egui_*`, covered by the rule above) nor `rusqlite` (covered by the DB rule below). It must keep building standalone: `cargo build -p document`.

**Why:** `document` is the part you'd reuse to swap the UI or call Rust as a library from a native shell — it only works if it stays free of the app's UI and DB. Editing *logic* (model mutation over a storey's shapes) lives here as `Document` methods; the controller is the thin shell that wraps them with undo, persistence, and UI plumbing.

**How to apply:** New editing logic that only reads/mutates the storey model goes in `document` (in the matching submodule — `dimensioning.rs`, `exposure.rs`, `edit.rs`, or core `lib.rs`). The controller calls it and owns the shell concerns. The scan already blocks egui/rusqlite here via the two rules above.

## `nalgebra` confined to `domain/**`

**Rule:** `nalgebra` may only be imported from `domain/`.

**Why:** A recent refactor (commit `a4d174c`) removed `nalgebra` from every crate except `domain/` so the rest of the codebase doesn't have to know the matrix-library lineage. The typedefs `Point2f32`, `Point2f64`, `Point3f64` in `domain::points` are the public surface.

**How to apply:** Use the typedefs in function signatures and struct fields. If you need a vector operation the typedef doesn't expose, add a method to `domain::points` rather than reaching into nalgebra from elsewhere.

## No `slint` imports

**Rule:** No source file may `use slint` or any `slint_*` crate.

**Why:** `slint` was an earlier abandoned toolkit choice (its leftover `Cargo.toml` dependency was removed in June 2026). Importing it in source signals confusion about which toolkit the project is on.

**How to apply:** Use egui. If you find yourself wanting Slint-only behaviour, that's a project-level decision to revisit — not something to import ad-hoc.

## Database access through `src/engine/` only

**Rule:** `rusqlite` may only be imported from `src/engine/` (and from `tests/`).

**Why:** All BER persistence funnels through `BerRepository` (`src/engine/ber_repository.rs`). Spreading raw SQL through controller/UI fragments the schema and makes migrations harder.

**How to apply:** UI and controller code talks to the repository, not the connection. New persistence functionality (lists, queries, migrations) goes into `src/engine/`.
