---
description: Guidance against speculative expansion in this codebase.
---

# Scope discipline

These aren't enforced by a script — they're easy to violate when "improving" a file Claude is touching. The diff is the last word; this rule file is the up-front context.

## Don't uncomment large commented blocks

**Rule:** Don't restore the commented sections of `manip_2d_plan/src/lib.rs` (or other large `//`-block regions) without a concrete caller.

**Why:** Those sections (add area, trim, collinear constraints, room detection, measurements) were left commented intentionally — they were not stable, or didn't compile against the current types. Restoring them en masse will pull in obsolete dependencies and may regress the parts that do work.

**How to apply:** If you need one of those features, port the specific function the caller depends on, fix the type mismatches, and verify it compiles in isolation. Don't uncomment in bulk.

## Don't add abstractions you don't need

The repo already has a workspace, traits, several module layers, and parametric/constraint infrastructure. New features default to direct functions/structs in the most appropriate existing crate. Reach for a new trait or generic only when there are at least two concrete callers.

## Don't add error handling for impossible cases

Trust internal contracts. The `unwrap()`s in `src/engine/ber_repository.rs` tests, or `expect()` in `main.rs:16`, are fine — adding `Result` plumbing through layers that can't realistically fail just adds noise. Validate at boundaries (user input, DB, FS, network); don't validate inside trusted internal call chains.
