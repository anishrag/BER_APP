# The Structure-phase stale-read seam

**Status:** diagnosed, not closed. Symptoms patched; the underlying seam is open.
**Written:** 2026-07-29, after a session that shipped `dfdf62c..427f59d` on `main`.

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
- **The two materializes at the top of `set_structure_group_attrs` /
  `set_structure_group_name` are defensive.** No test changes behaviour when they
  are removed, and no case was found that does. They state the invariant; they
  are not a demonstrated fix. If Option 1 lands, delete them.
- The exposure axis **is** a legitimate group splitter. A session attempt to
  remove it was wrong and was reverted — see `b17cdd5`.

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
