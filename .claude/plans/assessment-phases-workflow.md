# Plan: Assessment workflow phases — Mapping → Structural Details → Object Counts

## Goal

Frame the assessor's on-device job as three sequential-ish workflow phases, so the app
can guide a real BER survey end to end. The first phase (**Mapping**) is what we're
finalizing now; the latter two (**Structural Details**, **Object Counts**) are built
**in parallel** after Mapping lands.

```
Mapping  ──complete──▶  Structural Details  ┐
                                            ├─ worked on in parallel
                        Object Counts       ┘
```

The unifying idea: **Mapping decides *what & where* exists** (geometry + inferred
construction phase); **Structural Details fills in the fabric specifics and backs them
with proof**; **Object Counts enumerates the discrete plant/appliances and photographs
them**. Each later phase enriches what Mapping recorded rather than re-deriving it.

## Settled framing (from discussion)

### Design principle — phases are revisitable, non-destructive

The three phases are **not a one-way pipeline**. The assessor must be able to go back to a
previous phase — re-open Mapping after starting Structural Details, re-photograph an
element, fix a construction tag — **without losing or corrupting work done in a later
phase**. On site, surveys are non-linear: you notice a mis-tagged wall while detailing it.
Concretely:

- **Phase switching is soft, not gated.** Any phase can be entered at any time; the app
  shows completeness indicators rather than locking later phases until earlier ones finish.
- **Later-phase data is bound by stable identity, not position/order.** Structural Details
  fields/`Photo`s and counted items reference elements by their durable id (`ItemId` /
  `construction_id`), so re-editing the Mapping model doesn't orphan or mis-attach them.
- **Re-running a phase is additive/idempotent where possible** — re-entering Structural
  Details shows the specifics already entered and what's outstanding; it doesn't wipe prior
  details or photos. Object Counts preserves existing instances rather than resetting the
  tally.
- **Mapping edits degrade gracefully.** If a mapped element that later-phase details/photos
  were bound to is deleted or re-tagged, surface it (e.g. an "orphaned" flag) rather than
  silently dropping the data or crashing. Decide the exact reconciliation in Stage A.


### Phase 1 — Mapping (in progress, being finalized now)

Measurements + mapping. When complete:
- All storeys are drawn and dimensioned.
- Every element (walls, floors, ceilings/roofs, windows, doors) exists in the model and has
  its **construction phase inferred** from the drawn phase regions (year-of-construction →
  DEAP age band; see [[construction-phase-tagging]]).
- The element's **construction type and make-up are NOT yet specified** — they sit at
  defaults/placeholders. Mapping settles *what exists, where, and from which build phase*;
  it does not settle the fabric specifics.

This phase is the prerequisite gate: Structural Details and Object Counts both consume the
mapped model, so they only become useful once Mapping is "done enough" for a storey/house.

### Phase 2 — Structural Details (parallel build #1)

A second walk of the house that does two coupled things per structural element: **enter the
fabric specifics, and capture the proof that backs them**. Detailing and evidence happen
together — you record "filled cavity" *and* photograph the drill hole in the same step.

What's **inferred from Mapping** (pre-filled, editable here):
- Construction **phase** (from the drawn phase regions).
- Geometry-derived quantities already known (areas, the element's storey/position).

What's **entered in this phase** (the gap Mapping leaves):
- Construction **type** + **make-up** per wall/floor/ceiling/roof element (e.g. cavity vs.
  solid, filled vs. unfilled, insulation type/thickness, U-value inputs).
- Window/door specifics (glazing, frame, dimensions) on the fenestration items.
- The **evidence `Photo`s** for each: window glass thickness + spacer/stamp → the window;
  drill holes for a pumped cavity → the wall construction; visible insulation thickness →
  the relevant floor/ceiling/wall construction.

Net: this phase **sets construction type/make-up/details on mapped elements (via
`ensure_*_construction`) and appends evidence `Photo`s** — no new geometry. It's a guided
per-element checklist driven off the Mapping model: "these N elements still need details
and/or evidence; here's what's entered and what's outstanding."

### Phase 3 — Object Counts (parallel build #2)

Photograph (and in the process **enumerate/count**) the discrete plant + appliance
items that aren't part of the drawn fabric: fireplace(s), boiler, blower heaters,
vents, attic hatch, flue(s), solar PV panels, solar water heaters, hot-water
cylinder(s), etc.

Unlike Structural Details (which enriches elements Mapping already created), this phase
**creates the items as it goes** (you discover a fireplace, add it, photograph it) — so
it's count + capture. Most of these map to the systems/services side of DEAP, not the
fabric side.

## Current data-model anchors (what already exists)

- **`Photo { id, file_path, description }`** — `ber_model/src/ber/common.rs:36`. The one
  evidence primitive. `file_path` is a local image path (Android: app storage). Reused
  by every phase below; **no new photo type needed.**
- Photos already hang off:
  - **Windows / fenestration** — `fenestration.rs:36,54` (`photos: Vec<Photo>`).
  - **Structural constructions** (wall/floor/ceiling fabric) — `structural.rs:139`.
  - **Systems items** — `systems.rs:71` (`photo: Option<Photo>`).
  - **BER-level** — `ber_struct.rs:33` (`Ber.photos`) and `common.rs:223` (an item's
    `photos`).
- **Construction model** — `ber_model/src/ber/structural.rs`:
  `ensure_{wall,floor,ceiling}_construction(type, makeup, phase)` finds-or-creates a
  construction by the `(type, makeup, phase)` triple, and a surface's durable
  `construction_id` points at it (see [[construction-phase-tagging]]). Today Mapping feeds a
  real **phase** but defaults type/makeup; **Structural Details supplies the real type +
  makeup** via the same call (only those axes change; phase preserved). No new construction
  model needed — just real values where placeholders sit now.
- **Systems model** — `ber_model/src/ber/systems.rs`: `HeatingSystem`,
  `VentilationSystem`, `SolarElements`, etc. The Object Counts items land here (or as
  new sibling structs alongside these) — this is where boiler/vents/solar already live.

So both later phases are mostly **UI/workflow + filling existing constructions /
`Vec<Photo>` / system structs**, not a fabric/geometry change. Structural Details writes
through `ensure_*_construction` (type/makeup) and the elements' `photos`; Object Counts adds
system/count items. That's why they can be built in parallel and after Mapping.

## Architecture placement (respect the boundaries)

- **Data model** (`ber_model`): any *new* Object Counts item types (e.g. fireplace, attic
  hatch, flue) as plain structs/enums beside the existing `systems.rs` ones, each owning
  `photos`/`photo`. Pure data, no I/O. (`data-model-engineer`.)
- **Editing/model state** (`document`): storey-scoped model mutation goes here as
  `Document` methods — setting an element's construction type/make-up (calling
  `ensure_*_construction` in `ber_model`), and deriving "which mapped elements still lack
  details or evidence" over a storey's shapes. UI-free, DB-free.
- **Workflow** (`src/controller`): the **phase sequencing** — which phase the assessor is
  in, the per-phase checklist/cursor, driving the detail-entry + `Photo` capture for the
  chosen element, marking items counted. Thin: snapshot → call `document` → persist.
- **Persistence** (`src/engine`): storing `Photo.file_path` rows + the new system/count
  items via `BerRepository`. Image *bytes* live on the filesystem; only the path is in
  SQLite.
- **UI** (`src/ui`): the camera-capture flow, per-phase checklists, and a top-level phase
  switcher. Camera/file access is an Android concern — wire it behind the same
  `UiEvent`/owned-data boundary; no egui types escape `src/ui`.

## Stages (build order)

> Mapping is being finalized under its own plans (see [[construction-phase-tagging]] and
> the measurement/resolve plans). The stages below are the **new** Structural Details +
> Object Counts work, built in parallel once Mapping is stable.

### Stage A — Phase model + navigation (shared prerequisite for B & C)

- A notion of "current assessment phase" the app surfaces (Mapping / Structural Details /
  Object Counts), with the workspace able to switch between them. Likely a small controller
  + UI concern; no ber_model change beyond what's needed to know an element's
  detail/evidence status.
- A capture-`Photo` primitive in the UI (open camera → save image to app storage →
  return a `Photo { file_path }`) reused by both later phases. Android camera/file wiring
  per `android/` packaging.

### Stage B — Structural Details (enter fabric specifics + attach proof)

- Derive, from the Mapping model, the **per-element worklist**: each wall/floor/ceiling/roof
  and window/door, showing its inferred phase and what's still missing — type/make-up not
  yet set, and which evidence photos are outstanding (cavity drill-hole, insulation
  thickness, glass stamp).
- Detail entry: a per-element form to pick construction **type + make-up** (and window/door
  specifics), writing through `document` → `ensure_*_construction` so the element's
  `construction_id` lands on the right `(type, makeup, phase)` construction. Phase stays as
  inferred unless explicitly changed.
- Evidence capture in the same form: tap to photograph → append to the element's existing
  `photos: Vec<Photo>`.
- Completeness indicator per element (details set? required photos present?) feeding the
  soft-gate status from Stage A.
- Persist details + photos via `BerRepository`.

### Stage C — Object Counts (enumerate + photograph plant/appliances)

- Define the count categories: fireplace, boiler, blower heater, vent, attic hatch,
  flue, solar PV, solar water heater, hot-water cylinder, … (confirm the full list with
  the assessor before fixing the enum). Add any missing item structs in `ber_model`
  beside `systems.rs`, each with `photos`/`photo`.
- UI: a tally-style screen per category — add an instance, photograph it, count rises.
- Persist instances + photos.

## Open questions (resolve before coding B/C)

1. **Full Object Counts list + DEAP mapping.** Exact set of countable items and which DEAP
   field each feeds (some already exist under `systems.rs`; some are new). Drives the
   `ber_model` additions in Stage C.
2. **Structural Details — field set + completeness rules.** Which detail fields each
   element type needs (the construction type/make-up taxonomy + window/door specifics, and
   how they map to DEAP/U-value inputs), and what counts as "sufficiently detailed +
   evidenced" (required-photo kinds vs. just ≥1 photo). Drives both the Stage B form and its
   completeness indicator.
3. **Orphan reconciliation (Stage A).** Phase gating is settled as **soft + revisitable**
   (see Design principle). The remaining detail: exactly how to reconcile later-phase data
   when the Mapping element it was bound to is deleted or re-tagged — flag as orphaned,
   re-prompt to re-bind, or keep with a stale marker?
4. **Image storage + sync.** Where photos live on the Android device, and whether
   they're ever exported/backed-up off-device. Only `file_path` is in the DB today.
5. **Per-photo metadata.** Do evidence photos need a "kind" tag (drill-hole vs.
   insulation vs. stamp) beyond the free-text `Photo.description`? If so, a small enum on
   `Photo` or the binding.

## Verification (per stage, when built)

`cargo test --workspace`, `cargo fmt --all --check`, boundary scan (no `egui` outside
`src/ui`, no `rusqlite` outside `src/engine`, `document` builds standalone). Manual on
device: complete Mapping for a storey → enter Structural Details → set a wall's
type/make-up and photograph its cavity drill-hole → confirm the construction updates (phase
preserved) and both survive reload → enter Object Counts → add + photograph a boiler →
confirm it persists.

## Status

Planning only. Mapping is the active work; Structural Details + Object Counts are scoped
here for the parallel build that follows. No code written for B/C yet.
