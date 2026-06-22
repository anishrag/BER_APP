# Ceiling regions as the source of truth

Status: proposed (scoping). Supersedes the half-step idea of "make ceiling steps a
durable declarative field". This plan goes further: stop deriving ceiling regions
from declarative inputs at all — store the regions themselves and derive only the
secondary geometry (risers, wall tops) from them.

## The decision and why

Today `resolve_storey` rebuilds the ceiling regions from scratch on **every**
resolve:

```rust
let mut roofs = CeilingRegions::default_for_footprint(polygons_f64, height_m, cid);
roofs = roofs.with_z_deltas(&areas);          // steps + dividers + roof_splits
roofs = roofs.with_apexes_hinted(&apexes, &hints);
```

Because the regions are thrown away and rebuilt each time, the app must store enough
**declarative intent** to reproduce them (`roof_apexes`, `ceiling_dividers`,
`roof_splits`), and it must **reconstruct** the editable step list back out of the
resolved geometry on reload (`ceiling_step_regions_from_roofs` + `apex_groups` +
`fan_base_z`). That reconstruction is inference over tolerances and is the root of a
class of bugs (the divider-vanishing bug fixed in `dad6b76`; the carve/ridge
interaction flagged while doing `roof_splits`).

The `ceiling_dividers` field is the clearest tell that this is incidental
complexity: a divider is *just two regions at the same height that haven't been
fused*. The field exists **only** because `default_for_footprint` re-fuses them
every resolve. Keep the regions and the field is unnecessary.

**Confirmed assumption (drives the whole plan):** the storey **outline is
effectively frozen before ceiling work**; a late outline change forcing some manual
ceiling touch-up is acceptable. This removes the one thing the declarative model
buys (auto-refit of steps/ridges to a changed footprint), so the trade lands in
favour of regions-as-truth.

## Target architecture

- **Source of truth:** `Storey::roofs: CeilingRegions` (polygons + per-vertex z),
  stored durably. (Already persisted today — see "Migration".)
- **Ceiling edits are one-shot mutations** of `storey.roofs`, applied when the user
  acts, then persisted. Not replayed on every resolve.
  - Step  → split the picked region by a line, shift one side's z by Δ.
  - Divider → split the picked region by a line, same z.
  - Apex/ridge → replace the picked flat region with its fan/ridge faces.
  - Structure carve → coplanar face split (`split_faces_coplanar`, already this shape).
- **`resolve_storey` for ceilings** no longer rebuilds. It only:
  - seeds `default_for_footprint` when a storey first gets a height and has no roofs;
  - re-fits to the outline **when the outline changed** (simple, lossy — see below);
  - derives risers (`riser_faces`) and wall tops from `storey.roofs` (unchanged —
    wall derivation already reads `storey.roofs`, so it doesn't care how roofs got
    there).

The geometry functions (`default_for_footprint`, `with_z_deltas`, `with_apexes`,
`split_faces_coplanar`) **survive** — they just get called **once at edit time** on
the stored regions instead of every resolve from a rebuilt base.

## The keystone change (atomic)

The pivot is one thing: **resolve stops calling `default_for_footprint` + replay on a
storey that already has roofs.** Everything else is consequence. This flips steps
*and* apexes together for a storey — you can't make steps one-shot while apexes still
replay, because the replay's `default_for_footprint` clobbers the stored regions.

So Stage 1 is the keystone; the rest is cleanup that can land incrementally with the
suite green.

## Editing model — the real cost (and the recommendation)

This is where the truth model is genuinely harder than replay. In the replay model,
editing is trivial: change the input, re-replay. In the truth model, editing an
existing ridge/step means **undoing its effect on the regions and applying the new
one** — and a fan/ridge isn't cleanly invertible from triangulated geometry (the very
reason apexes were declarative).

**Recommendation: edit-by-clear-and-redraw.** To change a ridge, the user clears it
(the region resets to flat at its current base height — a cheap "un-shape": replace
the sloped faces over a footprint with one flat region at their `fan_base_z`) and
redraws. To change a step, drag the boundary / re-enter Δ as a fresh split+shift.
This needs **no inversion machinery and no replay**, which is the whole point. Given
the workflow is largely set-once, this is acceptable UX and keeps the model simple.

**Apex re-edit metadata (optional, recommend deferring):** if "tweak this ridge's
height in place" is wanted later, keep the apex point(s) as lightweight per-region
metadata used *only* to re-invoke the ridge tool — explicitly NOT a source of truth
and NOT replayed. Adds a derived/source duplication risk; only add it if the
clear-and-redraw UX proves annoying.

## Migration — effectively free

Saved records **already persist `roofs`** (the storey serializes its resolved
geometry; `resolve_storey` "clones the existing storey and overrides only the derived
geometry"). So every existing save is already in the truth form. Loading under the
new model just **trusts the stored `roofs`** instead of recomputing it. The
declarative fields (`roof_apexes`, `ceiling_dividers`, `roof_splits`) become
vestigial on load; drop them after a deprecation window (keep `#[serde(default)]`
readers so old blobs still parse).

No reconstruction-seeder needed. (Contrast the abandoned half-step plan, which did.)

## Undo/redo — already works

`HistorySnapshot` already snapshots the whole active `Storey`
(`record_storey: Option<(usize, Storey)>`), which includes `roofs`. One-shot region
edits are captured and reverted by the existing snapshot — no change needed.

## Outline change — simple lossy re-fit

When the outline changes (rare, per the confirmed assumption):
- Clip every stored region to the new footprint (boolean intersect).
- Fill any newly-exposed area with default-height region(s) (`default_for_footprint`
  over `new_footprint − union(existing regions)`).
- A step/ridge near a moved boundary may end short or get clipped — user touches up.

Trigger off the existing outline-revision tracking (the controller already tracks the
revision the geometry was derived at — see `current_record` revision field).

## What gets deleted

- `Storey::ceiling_dividers` (and its sync `sync_ceiling_dividers`).
- `Storey::roof_splits` (the field added for the carve fix — becomes a one-shot
  `split_faces_coplanar` mutation; the function stays).
- `Storey::roof_apexes` / `roof_apex_hints` as **source of truth/replay** (optionally
  retained as re-edit metadata per the apex note above; otherwise deleted).
- `document::ceiling_step_regions_from_roofs` and its inference branches.
- `AppController::ceiling_step_regions` session map + all `reconstruct_*` calls
  (storeys.rs, roof_points.rs).
- `CeilingRegion::fan_base_z` (reconstruction-only) — verify no other caller.
- `RoofApexGroup::base_z` / `merged_rings` and the `apex_groups` work that computes
  them, **iff** the surviving validation callers don't use them.

## What stays

- `CeilingRegions::default_for_footprint`, `with_z_deltas`, `with_apexes_hinted`,
  `split_faces_coplanar`, `riser_faces`, `region_plane` — called once at edit time.
- `apex_groups` **region/apex grouping** for ridge/rect VALIDATION
  (`roof_ridges.rs:validate_roof_ridge_extent`, `roof_rects.rs:validate_roof_rect`):
  "does this area already have a ridge?" is independent of reconstruction. Likely only
  needs `apex_idxs`/`region_idxs`, so it can be slimmed (drop `base_z`/`merged_rings`).
- The step CREATION UX (fit line → snap → position prompt → build ring) and its snap
  helpers (`region_line_coords`, `ceiling_line_coords`). Only the *destination*
  changes: it mutates `storey.roofs` once instead of pushing into a replayed session
  list.
- A relaxation dividend: one-shot apply no longer needs the "re-apply-to-self is
  identity" guarantees, so the boundary-inclusive-vs-strict predicate dance, the
  pre-lift step, and sliver-drop-to-self in `with_apexes` can likely be simplified.

## Staging (suite green at each step)

1. **Keystone:** `resolve_storey` treats a non-empty `storey.roofs` as truth — seed
   default only when empty, otherwise don't rebuild; still derive walls/risers from
   it. Move step/apex/divider/split application out of the always-on resolve path into
   controller-invoked one-shot mutations on `storey.roofs`. Outline-change re-fit
   added here. (This is the big one; most test churn lands here.)
2. **Editing:** implement clear-and-redraw for ridge/step edits (the un-shape helper).
3. **Delete dividers:** remove `ceiling_dividers` + `sync_ceiling_dividers`; the
   boundary now lives in the stored regions.
4. **Delete reconstruction:** remove `ceiling_step_regions_from_roofs`, the session
   `ceiling_step_regions` map, and all `reconstruct_*` calls.
5. **Slim/retire apex fields:** decide re-edit-metadata vs full delete; slim
   `apex_groups`/`RoofApexGroup`; drop `fan_base_z` if unused.
6. **Retire `roof_splits` field:** fold into the one-shot carve mutation.
7. **Tests:** delete the reconstruction/fixpoint tests
   (`reconstruction_is_group_aware`, the off-default-flat + divider-fixpoint set in
   `ceiling_step.rs`; the apex/ridge reload-fixpoint tests in `resolve.rs`). Replace
   with: one-shot edit → persisted regions are correct; reload trusts stored regions
   (trivial round-trip); outline-shrink re-fit behaves; ridge/rect validation still
   fires. Controller round-trip tests in `app_controller/tests.rs` rewritten to the
   truth model.

## Risks / open items

- **Editing ergonomics** is the load-bearing risk. Clear-and-redraw must feel fine; if
  not, apex re-edit metadata (with the duplication risk) is the fallback.
- **Outline re-fit** is intentionally lossy. Confirm the rare cases (jut added after a
  ridge) degrade gracefully, not into a panic or a flattened roof.
- **`apex_groups` slimming** — verify the two validation callers' exact field use
  before removing `base_z`/`merged_rings`.
- **Cross-storey effects** — `inherited_floor_levels` / `lowest_flat_z_over` read
  `roofs` to seat the storey above. They read regions, so truth-vs-derived is
  transparent, but re-verify the storey-above floor datum after the keystone.
- **Size:** medium-to-large. Stage 1 is a focused but deep change to the most
  heavily-tested file (`resolve.rs`); Stages 3–7 are mechanical deletions. Net effect
  is a smaller, flatter codebase and the removal of a whole inference/fixpoint
  subsystem — at the cost of a harder in-place edit story (mitigated by
  clear-and-redraw).

## Recommended next step

Before committing to Stage 1, do the **un-shape spike**: confirm a sloped fan/ridge
region can be cleanly reset to one flat region at its base height (for both
clear-and-redraw and outline-refit-of-a-sloped-region). That primitive is load-bearing
for the whole editing model; if it's ugly, revisit the apex-metadata path first.
