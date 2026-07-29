# The Structure-phase stale-read seam

**Status:** Option 1 implemented and green, uncommitted in the working tree.
Region identity is now stable across a resolve, and the materialize the
full-vs-subset decision needs lives inside that decision — the per-caller
discipline is gone. Redundant re-point machinery still stands (see
"What remains").
**Written:** 2026-07-29, after a session that shipped `dfdf62c..427f59d` on `main`.
**Updated:** 2026-07-29 — Option 1 built and verified; see "What Option 1 turned
out to be" below.

## The problem in one paragraph

The controller writes construction attributes to the **durable maps** on
`StoreyDoc` (Layer 2). A resolved region's `construction_id` (Layer 1, the
record) only catches up at the next `materialize()`. So **between a write and a
materialize, the record is behind** — and any decision made by reading the
record during an edit answers about the state *before* this session. Worse,
`materialize()` **re-mints every region id**, so the obvious remedy (materialize
before deciding) invalidates the very ids the caller is holding.

```
controller writes ──▶ Layer 2  (StoreyDoc durable maps, keyed by surface)
                          │
                    [ materialize ]   ← record catches up here, and region ids are RE-MINTED
                          ▼
                      Layer 1  (Ber record: region.construction_id → construction registry)
```

## Evidence: four field bugs, one cause

| Symptom (56 Elmwood Road) | What read the stale record |
| --- | --- |
| Photo attached to a wall the assessor never photographed | `add_group_photo` read the region's `construction_id` before the attrs write had landed, so the photo went to the record-wide unattributed bucket |
| Ceiling insulation spec didn't stick | `set_structure_group_insulation` — same shape (patched before this session) |
| Photograph-first evidence vanished on spec | the re-attach looked its target up by region id, which the intervening materialize had re-minted |
| **Group name reverted to "Group 1"** | `group_covers_all_members` saw the walls still on the shared bucket, called the edit a subset fork, and cleared the name |

Every one was patched with a `materialize()` at one more call site. That is a
per-caller discipline, and it keeps being forgotten — which is the actual
complaint.

## Two fixes that were tried and REJECTED

Do not re-attempt these without reading why they failed.

1. **Snapshot the full-vs-subset answer at session open**
   (`EditGroup::covers_origin`). Wrong semantics: if a selection has since
   merged into another group, "am I a subset?" is a question about the group it
   is in **now**, not the one it started from. A forked pair must count as a
   whole group on its second edit — see
   `a_forked_subset_is_a_whole_group_for_the_next_edit`.

2. **Move the `materialize()` inside `group_covers_all_members`** so callers
   cannot get it wrong. Broke six tests. The reason is the crux: materialize
   re-mints region ids, so it invalidates the `ids` the caller already captured.
   **The seam cannot be encapsulated** — the operation that makes the record
   current is the same one that invalidates the caller's handles. It can only go
   at the *top* of an operation, before ids are captured.

   **Superseded 2026-07-29.** This was a conclusion about id churn, not about
   the seam. Once Option 1 made ids stable, the same move landed with the whole
   suite green. Do not read this entry as "encapsulation is impossible" — it is
   the record of *why* it was impossible before.

## The two real options

### Option 1 — stable region identity across resolves
Make `materialize()` stop re-minting region ids: a resolved region keeps its
`ItemId` when it corresponds to the same surface as before. The re-point
machinery already computes this correspondence — `refresh_storey` and
`refresh_all_storeys_keeping_selection` (`src/controller/app_controller/storeys.rs`
~546 and ~745) capture `(storey, kind, SurfaceKey)` before the resolve and
re-map afterwards, dropping anything that fails to re-locate (`filter_map` —
silent). Pushing that identity *into* the resolve, instead of re-mapping around
it, would let anything hold a region id across a materialize.

Consequences: `materialize()` becomes safe to call anywhere, the two defensive
materializes below become unnecessary, the selection/edit-group re-map code
disappears, and the silent `filter_map` drops go with it.

### Option 2 — the editor decides from Layer 2, not the record
Group membership and the full-vs-subset test consult the durable maps (which the
edit is writing) plus geometry, never `region.construction_id`. The record stops
being consulted mid-edit at all.

Narrower than Option 1 and touches less, but leaves the re-mint trap in place for
every other caller.

**Recommendation:** Option 1. It fixes the seam for everyone rather than for the
editor, and it retires machinery rather than adding any. Option 2 is the fallback
if Option 1's identity mapping turns out not to be total.

## Key locations

- `document/src/lib.rs` — `StoreyDoc` durable maps (`wall_constructions` is now
  `HashMap<SurfaceRef, Vec<WallSpanEntry>>`; `riser_constructions`,
  `floor_constructions`, `ceiling_constructions`).
- `document/src/phase.rs:~273` — `stamp_wall_constructions`, the reconciler that
  moves Layer 2 → Layer 1 during a resolve.
- `src/controller/app_controller/storeys.rs:546` — `refresh_storey`, and `:745`
  `refresh_all_storeys_keeping_selection`: the capture/re-map around the resolve.
  **This is where Option 1 starts.**
- `src/controller/app_controller/structure.rs`
  - `group_covers_all_members` — documents the caller contract; requires a
    current record.
  - `set_structure_group_attrs`, `set_structure_group_name` — materialize at the
    top, before capturing ids.
  - `add_group_photo`, `set_structure_group_insulation`,
    `rehome_unspecified_group_for_evidence`, `take_provisional_evidence` — the
    earlier per-caller patches.

## Safety net already in place

Characterisation tests pin the contract the rework must preserve. Run these first
and keep them green:

- `characterise_identical_attributes_including_name_are_one_group`
- `characterise_group_boxes_split_on_name_phase_and_exposure`
- `characterise_full_rename_keeps_photos_subset_rename_forks`
- `subset_decision_is_stable_across_a_mid_session_materialize`
- `a_forked_subset_is_a_whole_group_for_the_next_edit`
- `field_setting_makeup_after_naming_keeps_the_name`
- `photograph_first_keeps_evidence_private_then_merges_on_spec`
- `span_key_survives_the_split_boundary_moving`

591 tests green at `427f59d`; `cargo fmt --check` clean; boundary scan exit 0.

## Honest caveats — do not treat these as done

- **`field_reset_does_not_smear_photos_onto_the_unclassified_bucket` passes with
  its own fix disabled.** The fix (`carry_evidence_from_orphans` refusing to
  carry onto an unclassified construction) is right and the smear is plainly
  visible in the field record, but it was never reproduced synthetically. That
  test currently pins nothing.
- ~~**The two materializes at the top of `set_structure_group_attrs` /
  `set_structure_group_name` are defensive.**~~ Deleted 2026-07-29 with Option 1
  — the materialize they stood in for now lives inside
  `group_covers_all_members`. Removing them WITHOUT that (and without the
  identity restore) fails 7 tests, so they were load-bearing after all; the
  earlier "no test changes behaviour" reading was wrong.
- The exposure axis **is** a legitimate group splitter. A session attempt to
  remove it was wrong and was reverted — see `b17cdd5`.

## What Option 1 turned out to be (built 2026-07-29, working tree)

Not an identity threaded *through* the resolve — the ids are minted in a dozen
scattered geometry ops (`regenerate`, `apply_splits`, `with_risers`,
`partition`, `with_levels`, `with_apexes`, `refit_to_footprint`, the InterStorey
and roof-void and porch-exclusion splits), and touching all of them is a large,
risky surface. Instead **one re-labelling pass over the finished geometry**:

- `document/src/identity.rs` — `region_identity_snapshot(&Ber)` captures every
  region's durable key → id BEFORE the resolve; `restore_region_identity`
  hands each re-derived region its previous id back. Keys are the ones the
  callers already re-pointed by (wall = quantized base edge + base z; area =
  quantized centroid). Walls and porch walls share one pool. An id is handed
  out at most once, and an ambiguous key (two regions quantizing the same) is
  dropped, so ids stay unique and a genuinely new surface keeps its fresh id.
- The snapshot is taken at the top of `refresh_storey` / `refresh_all_storeys`
  and restored **inside `derive_horizontal_exposure_all`**, after
  `derive_porch_exclusions` (the last region-minting stage) and **before
  `relink_openings_after_derive`**. That ordering is load-bearing: a placement
  records its host by `wall_id`, so re-homing openings onto ids the pass is
  about to rewrite orphans every opening — 19 window/door/elevation tests fail
  if the restore lands after the re-link.
- `group_covers_all_members` now materializes itself and the two defensive
  materializes at `set_structure_group_attrs` / `set_structure_group_name` are
  gone. **This is rejected-approach #2, and it now works** — the six-test
  breakage was entirely the id re-mint.

**Evidence.** Across 19 controller fixtures (porches, ceiling steps/apexes,
risers, wall splits, T/L/notched plans, two-storey with InterStorey), a no-op
materialize churned every region id before, and churns **none** after. The key
correspondence measured **total and injective** in all 19 — the risk the
original plan named ("Option 1's identity mapping turns out not to be total")
did not materialise on any fixture. `cargo test --workspace` green (594),
`cargo fmt --check` clean, boundary scan exit 0.

Two new tests pin it, and both were verified to FAIL with the restore disabled:

- `region_ids_survive_a_materialize` — every region keeps its id, and ids stay
  unique, across a no-op re-derive on 10 fixtures.
- `subset_decision_needs_no_caller_discipline` — ids captured before the
  decision are still live after it, and the group keeps its name.

Disabling the restore also reproduces the original six-test breakage exactly
(7 pre-existing tests fail once the materialize moves inside), which is the
direct confirmation that stable identity is what unblocks the encapsulation.

## The re-point machinery is gone (same session)

`SurfaceKey`, `wall_base_z_mm`, `structure_id_for_key`,
`structure_surface_key`, `structure_surface_key_any`, `locate_structure_key`,
the riser/elevation re-point and both capture/re-map blocks are deleted — about
250 lines. `refresh_all_storeys_keeping_selection` is now just
`refresh_all_storeys()` + the prune.

What replaced them is `prune_dead_surface_ids`: the re-point half is obsolete
(identity is restored), but the **drop** half is not — a carve or a split
removes surfaces, and a stale id left in `structure_selected` keeps counting
toward the length-sensitive readers (`partition_structure_selected_count`, the
`structure_selected.len() != 1` gate in `partition.rs`, the `is_empty` checks,
the N/M group headers). Liveness is `Storey::surface_kind_of` over the same
scope the selection was seeded in (`structure_storey_indices`: the active
storey, or the whole stack from the All-storeys tab).

Two things checked before deleting, both of which had looked like blockers:

- **The single-storey arm's cross-storey drop is unreachable.** With
  `current_storey_idx == Some(i)` every path that populates the selection is
  already scoped to storey `i` (`structure_storey_indices`,
  `select_surfaces_in_region` via `carve.storey`), and `set_current_storey`
  clears `structure_selected` through `reset_transient_editing_state`. There are
  no foreign ids to drop, so widening the prune to the seeding scope changes
  nothing observable.
- **The prune had no test.** Disabling it left all 594 green — the same
  "pins nothing" flaw called out in the caveats below. Now pinned by
  `a_removed_surface_drops_out_of_the_selection` (a partition cut splits a
  selected wall; the whole wall's id must leave both the selection and the open
  group while an untouched wall stays), verified to fail with the prune
  disabled.

595 tests green, `cargo fmt --check` clean, boundary scan exit 0.

## Reset bugs found on top (same session)

Three defects in the Structure-phase Reset, all reported from the field:

1. **Reset blanked the phase to `Unassigned`** in the session mirror. The editor
   passes `g.phase` through on every attribute pick, so the next tap wrote that
   onto the durable entry — the reset silently undid the phase mapping one pick
   later. Now returns to the surface's MAPPING phase, via new
   `document::wall_mapping_phase` / `area_mapping_phase` (factored out of
   `stamp_wall_phases` / `stamp_ceiling_phases`, which already derived exactly
   this — one implementation, not a copy). `detach_construction_for` writes the
   mapping phase into the durable default too, instead of reading back the phase
   of the attribution being discarded.
2. **Naming a group before setting its fabric branded the shared unclassified
   bucket.** The bucket reads as a "full group" for the subset decision (245f4b7
   — correct there), and the in-place rename took that as licence. Every other
   un-answered surface in the house inherited the name. Now skipped for
   unclassified constructions; the durable per-surface entries carry the name to
   the actual members, and the bucket holds no evidence to ride along.
3. **Reset could not clear a name-only group.** The `attributed` gate counted
   only type and build-up, so a named-but-unspecified group looked untouched and
   the detach returned early — the box cleared to "Group N" while the record kept
   the name, and re-selecting re-opened the old named groups. The gate now counts
   the name, on both sides of the mirror.

Pinned by `reset_returns_the_group_to_its_mapping_phase`,
`reset_returns_each_zone_group_to_its_own_phase`,
`reset_clears_a_name_applied_before_any_fabric` (verified to fail with EITHER of
fixes 2 and 3 disabled) and `reset_of_named_groups_reopens_as_one_unnamed_group`.

**A regression this session introduced and then fixed — read before moving a
materialize again.** Encapsulating the materialize inside
`group_covers_all_members` left `construction_ids_for` reading a stale record one
line earlier in `set_structure_group_name`, which re-created leak (2) by another
route. The first fix — making `construction_ids_for` materialize too — broke
`photograph_first_keeps_evidence_private_then_merges_on_spec`, because
`rehome_unspecified_group_for_evidence` re-points regions **in the record
itself** and a re-derive undoes that before the photo is written. So:

> `construction_ids_for` must stay a pure read. A materialize is safe at the TOP
> of an operation and inside a decision, but NOT between a Layer-1 re-point and
> the write that depends on it.

`set_structure_group_name` now computes `full` (which materializes) before
`cids`. Order matters there; the comment says so.

## The duplicate-key bug (found on real data, 56 Elmwood Road)

**Symptom:** select three walls, set type + build-up + name + photo, and only two
of them take it — the third silently leaves the group.

**Cause:** the durable wall key was `(base edge, base z)`, and that is NOT
unique. The cross-storey derivation stamps a buried junction BAND over the foot
of a wall; the band shares the wall's base edge AND its base z, differing only in
top z. Storey 1 of 56 Elmwood Road has exactly that pair:

```
storey 1 key ((943, 4514, 6093, 4514), -250) has 2 regions:
    exp=InterStorey  z -0.25..0.20     <- buried junction band
    exp=Exposed      z -0.25..2.41     <- the real wall
```

Both the old re-point and the new identity restore break on it, differently:
`structure_id_for_key` took the FIRST match, which is the `InterStorey` band —
and `attrs_of` rejects an out-of-scope wall, so the selection lost it. The
identity snapshot saw one ambiguous key, refused to answer for either, and gave
BOTH fresh ids — so the held id died and the prune dropped it. Same symptom.

**Fix:** `RegionKey::Wall` carries the quantized TOP z as well. Verified against
the record: with the discriminator the twins keep their ids across a materialize
and the full flow sets **3 of 3**; without it, `2 of 3` and the third region is
gone — the report exactly.

**No fixture reproduces this shape** — a scan over the two-storey / overhang /
inter-storey / Elmwood-migration fixtures found no duplicate base key, which is
why 19 fixtures' worth of "zero churn" evidence missed it. Pinned instead by a
`document` unit test that builds the pair directly,
`a_junction_band_and_its_wall_keep_separate_identities` (verified to fail without
the discriminator). Worth building an app-level fixture with a partially covering
upper storey so the integration path is covered too.

**Method note:** the synthetic fixtures agreed with each other and were all
wrong. The real record disagreed on the first try. Reach for it earlier.

## What remains

- `field_reset_does_not_smear_photos_onto_the_unclassified_bucket` still pins
  nothing (see caveats) — untouched by this work.
- `reset_of_named_groups_reopens_as_one_unnamed_group` passes on `HEAD` too: it
  characterises the reported end-to-end flow but is not what fixes it. The
  reported state is reachable through the name-only path (3); if the assessor
  hits it again from a sequence that does NOT involve naming before attributing,
  there is a route still unfound.
- None of this is committed.

## What already shipped (dfdf62c..427f59d)

Keying and identity, from the audit — these are done and are not what remains:

- `4b848b6` one span key per wall surface: `(SurfaceRef, along-range,
  height-range)` matched by greatest **overlap**, so it survives a split moving.
  Retired `face_constructions`, `FaceRegionKey`, `wall_piece_key_of`, the
  sibling-count gate and the dual seeding.
- `ab50dbe` photograph-first evidence gets a provisional group of its own,
  merging into a matching group when the fabric is set.
- `428cf4f` unclassified constructions carry no evidence, plus a load-time repair.
- `245f4b7` an unattributed selection is not a subset (the name-revert fix — the
  one verified to bite).
- `1d6325d` / `b17cdd5` a uniform face partition keeps its exposure and stays
  changeable; the bulk set addresses each piece, so the lock is gone.

The deeper item the audit named — construction groups having no identity of their
own — was resolved in favour of the assessor's own rule: **identical attributes,
including the name, means the same group.** Content addressing stays; the
provisional flag is the one exception, for unspecified selections holding
evidence.
