---
description: Guidance against speculative expansion in this codebase.
---

# Scope discipline

These aren't enforced by a script — they're easy to violate when "improving" a file Claude is touching. The diff is the last word; this rule file is the up-front context.

## `gen_3d_vol/` is currently dead weight

**Rule:** Don't add to `gen_3d_vol/` unless the change is required by a caller that already exists, or that you're adding in the same task.

**Why:** The crate is ~1,400 LOC (`lib.rs`, `ceiling.rs`, `walls.rs`, `geometry.rs`) and nothing in the main app currently calls it. It was slimmed from ~4,400 LOC when floor/roof/wall *region derivation* moved to its canonical home — `datatypes` (`with_levels`, `default_for_footprint`/`with_z_deltas`/`with_apexes`, `regenerate`) composed by `document::resolve_storey`. What's left is only what those ops don't do (volume assembly for a 3D viewer, ceiling mesh triangulation + ridge-vertex insertion, wall split-by-line). Expanding it speculatively inflates the unused surface — and re-growing derivation logic here would recreate the duplication that was just deleted.

**How to apply:** If a task is "show the sketch as a 3D volume," the work is the controller wiring up `Gen3D::from_ber_storey` and the mesh machinery. New *derivation* logic (region splitting, wall generation) goes in `datatypes`/`document`, never here. If `gen_3d_vol` needs a new method for an actual caller, add the minimum to satisfy that call.

## Don't uncomment large commented blocks

**Rule:** Don't restore the commented sections of `manip_2d_plan/src/lib.rs` (or other large `//`-block regions) without a concrete caller.

**Why:** Those sections (add area, trim, collinear constraints, room detection, measurements) were left commented intentionally — they were not stable, or didn't compile against the current types. Restoring them en masse will pull in obsolete dependencies and may regress the parts that do work.

**How to apply:** If you need one of those features, port the specific function the caller depends on, fix the type mismatches, and verify it compiles in isolation. Don't uncomment in bulk.

## Don't add abstractions you don't need

The repo already has a workspace, traits, several module layers, and parametric/constraint infrastructure. New features default to direct functions/structs in the most appropriate existing crate. Reach for a new trait or generic only when there are at least two concrete callers.

## Don't add error handling for impossible cases

Trust internal contracts. The `unwrap()`s in `src/engine/ber_repository.rs` tests, or `expect()` in `main.rs:16`, are fine — adding `Result` plumbing through layers that can't realistically fail just adds noise. Validate at boundaries (user input, DB, FS, network); don't validate inside trusted internal call chains.
