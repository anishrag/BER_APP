# BER_APP — State layers, the resolve pipeline, and persistence

> **Purpose.** Background for an architecture audit of *where state lives, how it
> flows, and how it is persisted/reconstructed*. It was written after diagnosing a
> bug ("floor exposures go green→red when you add a window after loading the dev
> checkpoint"). That bug is a symptom of the asymmetry described in §6. Use this as
> orientation, then verify against the code — symbol names are given (with a few
> line numbers, which drift) so you can `grep` rather than trust line refs.

---

## 1. The three state layers

### Layer 1 — `Ber` (the resolved, durable record)
- **Type:** `ber_model::ber::ber_struct::Ber` (`ber_model/src/ber/ber_struct.rs`), containing `Vec<Storey>` (`ber_model/src/ber/structural/storey.rs`).
- **What it is:** the *finalised, rendered* data model — pure data, no I/O. Every floor/roof/wall **region** carries its current attributes baked in (`exposure`, `construction_id`, opening `opes`), plus the construction registry, `window_types`, windows/doors, and per-`Storey` durable INPUTS.
- **Durable inputs on `Storey`** (recoverable truth, survive any save): `floor_splits`, `roof_splits`, the canonical ceiling-step list, `roof_apexes` / `roof_apex_hints`, `ceiling_height_m`, `floor_thickness_m`, `living_areas` (resolved), `windows`/`doors` (with `OpeningPlacement` + `WallAnchor`).
- **`Ber`-level:** construction registry (`wall/floor/ceiling_constructions`), `window_types`, `photos`, `mapping_reviewed`, storeys.
- **Serialized to:** the DB `ber.data_json` column **and** the dev `checkpoint.json` (both are `serde_json` of `Ber`).

### Layer 2 — `Document` / `StoreyDoc` (the editing model)
- **Type:** `document::Document` = `Vec<StoreyDoc>` + `active_storey`; per-storey `document::StoreyDoc` (`document/src/lib.rs`). UI-free, DB-free; builds standalone.
- **What it is:** the **editable source of truth that re-resolves *into* Layer 1.** `resolve_storey` reads a `StoreyDoc` and produces a `Storey`.
- **`StoreyDoc` fields:**
  - *Parametric / in-flight editing buffers:* `outline` (`ShapeSlot` = parametric `FloorPlan` + `DimensionState`), `floor_plan` (live buffer), `dimensioning`, `living_areas` (slots), `phase_shape`, `step_shape`, `active` (which shape), `edit_state`, `pending_arc`, `subarea_containment_override`.
  - *Durable side-tables that `resolve` STAMPS from* — **these are the audit focus:**
    | Field | Key | Drives (resolve stamp) |
    |---|---|---|
    | `wall_sections` | `VertexId` | 1-D perimeter wall exposure (`sections_*`) |
    | `wall_face_partitions` | `VertexId` | 2-D wall-face splits + their exposures (`apply_face_partitions`) |
    | `riser_exposure` | `RiserKey` | step-riser wall exposure (`stamp_riser_exposure`) |
    | `floor_exposure` | `CentroidKey` | floor exposure (`stamp_floor_exposure`) |
    | `ceiling_exposure` | `CentroidKey` | roof/ceiling exposure (`stamp_ceiling_exposure`) |
    | `wall_constructions` | `SurfaceRef` | wall construction type/makeup/phase (`stamp_wall_constructions`) |
    | `floor_constructions` | `CentroidKey` | floor construction (`stamp_floor_constructions`) |
    | `ceiling_constructions` | `CentroidKey` | roof/ceiling construction (`stamp_ceiling_constructions`) |
  - `CentroidKey` = region centroid quantized to the mm (`document/src/phase.rs`, `centroid_key`).
- **Serialized to:** the DB `snapshot` table (`snapshot_json` = `serde_json` of the whole `Document`), gated by `PARAMETRIC_SNAPSHOT_VERSION`. **NOT serialized to `checkpoint.json`.**

### Layer 3 — `AppController` (the session / shell)
- **Type:** `crate::controller::app_controller::AppController` (`src/controller/app_controller/`).
- **What it is:** the thin shell driving Layer 2 → Layer 1: `UiEvent` dispatch, undo/redo (`HistorySnapshot` = `Document` clone + active record), `current_storey_idx`, tool modes, persistence orchestration, UI-data derivation.
- **Re-resolve INPUTS held here (not in `StoreyDoc`):** `phase_regions`, `step_regions`, `ceiling_step_regions` (per-storey `HashMap`s). These are **passed into `resolve_storey` on every resolve** (see §2) — they are live inputs, not just overlay-drawing data.
- **Persisted?** No. On load they are **reconstructed** from Layer 1 (`reconstruct_phase_regions_for`, `reconstruct_step_regions_for`, `load_ceiling_step_regions_for`); the rest (history, modes) is reset.

---

## 2. The resolve pipeline (runs on EVERY edit)

The central fact: **Layers 1 and 2 are not produced once and frozen.** Any edit marks the storey dirty and the next `materialize()` rebuilds it.

```
edit → mark_dirty()            (storeys.rs ~706: bumps `revision`)
     → materialize()           (storeys.rs ~714: if revision != derived_revision)
        → refresh_storey()     (ACTIVE storey only; storeys.rs ~347)
           → resolve_storey(&document.storeys[idx], &ber, idx,
                             phase_regions, step_regions, ceiling_step_regions)
                                (document/src/resolve.rs ~48)
```

`resolve_storey` rebuilds the storey from the outline footprint + phase/step inputs,
then **stamps attributes back on** from the `StoreyDoc` side-tables, in order
(`document/src/resolve.rs`, ~400–422):
`apply_face_partitions` → `stamp_riser_exposure` → `stamp_floor_exposure` →
`stamp_ceiling_exposure` → openings tail (`reanchor_openings`,
`derive_opening_vertices`, `prune_orphaned_openings`, `rebuild_opes`).
Construction stamps (`stamp_wall/floor/ceiling_constructions`, `stamp_*_phases`)
run from the controller around the resolve call.

**Implication:** every stamp reads a `StoreyDoc` side-table (or a Layer-3 input).
If that table is empty at resolve time, the rebuilt region falls back to its
default (`Unset` exposure / default construction / no partition). The resolved
`Ber` regions you loaded with are *overwritten* by the rebuild.

### Two resolve entry points (audit the difference)
- `refresh_storey()` — **active storey only.** Used by `materialize()` (every edit). Runs `resolve_storey` (which stamps from the durable `StoreyDoc` maps).
- `refresh_all_storeys()` (`storeys.rs ~609`) — **all storeys**, then a FINAL pass `derive_horizontal_exposure_all()` (`~671`) that runs `auto_derive_horizontal_exposure` (party floors/ceilings; `document/src/exposure.rs ~810`, writes the durable maps via `or_insert`) and re-stamps across non-active storeys. Called by the floor/ceiling tools, ceiling height, and load.

The completeness rollups (`checklist.rs`, `ber_storey_status`) read the **resolved
`Ber`** (Layer 1), e.g. `floor_exposure_complete` (`document/src/exposure.rs ~867`)
= "every floor region's `exposure != Unset`".

---

## 3. Persistence

### DB (production) — `src/engine/ber_repository.rs`
- `upsert` writes `ber.data_json` = `serde_json(&record.data)` → **Layer 1 only** in that column.
- `save_snapshot` writes a separate `snapshot` table: `(ber_id, version, storey_idx, snapshot_json)` where `snapshot_json` = `serde_json(&Document)` → **Layer 2** (whole multi-storey `Document`). Written on the flush path (`persistence.rs ~701`).
- `load_snapshot` returns `(version, storey_idx, json)`.

### Dev checkpoint — `src/engine/checkpoint.rs`
- `save_checkpoint` (`persistence.rs ~587`) → `checkpoint::save(&record.data)` = **Layer 1 only**. There is **no** `Document` snapshot.
- Gated behind `BER_CHECKPOINT=1` / a "Load checkpoint" affordance.

---

## 4. Reconstruction on load

### `open_ber` (DB) — `persistence.rs ~169`
1. Deserialize `Ber` from `data_json`.
2. Size `Document` to storey count; for each storey `load_storey_into_editor(storey)` → **`StoreyDoc::from_storey`** (the rebuild fallback).
3. Reconstruct Layer-3 inputs: `reconstruct_phase_regions_for`, `reconstruct_step_regions_for`, `load_ceiling_step_regions_for`.
4. **`try_restore_snapshot(id)`** (`persistence.rs ~545`): if a version-matching `Document` snapshot exists, deserialize it and **replace `self.document`** wholesale — restoring ALL Layer-2 side-tables. On missing/stale/version-mismatch/parse-fail, it **silently keeps the `from_storey` rebuild.**

### `load_checkpoint` (dev) — `persistence.rs ~598`
Same as `open_ber` steps 1–3, but **no `try_restore_snapshot`** (there's no snapshot). So Layer 2 is whatever `from_storey` produced.

### `StoreyDoc::from_storey` — `document/src/load.rs ~20` (THE GAP)
Reconstructs **only**: `outline` + `floor_plan` + `dimensioning` (from the resolved footprint), `living_areas` (from resolved polygons), `wall_sections` (`sections_from_wall_regions`).
Does **NOT** reconstruct: `floor_exposure`, `ceiling_exposure`, `riser_exposure`,
`wall_face_partitions`, `wall/floor/ceiling_constructions`.

---

## 5. The classification principle (the intended design)

Everything that drives re-resolve should be either:
- **(a) Reconstructible from Layer 1** — the resolved record encodes it — so rebuild it on load instead of persisting twice. *Examples that DO this:* phase regions (from phase-tagged floor regions), ceiling steps (from the durable step list), wall_sections (from wall regions).
- **(b) Genuinely NOT invertible from Layer 1** — must be persisted (this is the reason the DB keeps the Layer-2 snapshot at all). *Examples:* the parametric `FloorPlan` (which edges are linked/constrained, params, pending arc) — a finalised polygon can be re-traced but the parametric structure can't be cleanly inverted; face-partition 2-D geometry is similar.

---

## 6. The known gap / bug (the audit's starting thread)

`floor_exposure` (and `ceiling_exposure`, `riser_exposure`, the construction
maps) are **category (a)** — each resolved region carries its `exposure` /
`construction_id`, so they *could* be reconstructed from Layer 1 like phase
regions are. **But `from_storey` has no reconstruction pass for them.**

Consequence on a **Layer-1-only load** (the dev checkpoint; also any DB record
whose Layer-2 snapshot is missing or version-stale):
- On load the resolved `Ber` regions still carry their exposures → the checklist
  reads Layer 1 → **green**.
- The first edit (e.g. place a window) → `mark_dirty` → `materialize` →
  `refresh_storey` → `resolve_storey` rebuilds the floor and `stamp_floor_exposure`
  reads the **empty** `StoreyDoc.floor_exposure` map → regions revert to `Unset`
  → **red**.

Production (DB) hides this because `try_restore_snapshot` reloads the full
`Document`. So the bug surfaces on the checkpoint, and would also surface on a DB
record if the snapshot is absent or `PARAMETRIC_SNAPSHOT_VERSION` was bumped.

---

## 7. Audit checklist

For **each** `StoreyDoc` side-table and each Layer-3 input, answer:
1. Is it a **resolve input** (read by `resolve_storey` or a stamp)? → then losing it changes Layer 1 on the next edit.
2. Is it **reconstructed on load** from Layer 1 (`from_storey` / `reconstruct_*`)?
3. Is it **persisted** (Layer-2 snapshot)?
4. Is it **derivable from Layer 1** at all (category a vs b)?

Findings to confirm:
- `floor_exposure` / `ceiling_exposure` / `riser_exposure` / construction maps / `wall_face_partitions`: input = yes, reconstructed = **no**, persisted = via snapshot only, derivable = yes (except face-partition 2-D geometry). → **fix candidates** (reconstruct in `from_storey`, OR persist the `Document` in the checkpoint too).
- `wall_sections`: input = yes, reconstructed = **yes**. (The one that's done right — the template.)
- `phase_regions` / `step_regions` / `ceiling_step_regions`: input = yes, reconstructed = **yes** (Layer 3).

Broader questions worth raising:
- Should Layer-2 side-tables live on `Ber`/`Storey` (Layer 1) instead, since they're category (a)? That would make them survive any save with no reconstruction.
- Is the snapshot-version fallback (silently rebuild from `from_storey`) safe, given `from_storey` is incomplete? Any version bump silently drops exposures/constructions on old records.
- Single source of truth: the dual "resolved `Ber` regions carry attributes" + "`StoreyDoc` maps are the real source" is the root tension. Which is authoritative?

---

## 8. Key file map

| Concern | Location |
|---|---|
| Layer 1 model | `ber_model/src/ber/ber_struct.rs`, `ber_model/src/ber/structural/*` |
| Layer 2 model | `document/src/lib.rs` (`StoreyDoc`/`Document`) |
| Resolve + stamps | `document/src/resolve.rs` (`resolve_storey`, `stamp_*`, `apply_face_partitions`) |
| Auto-derive exposure | `document/src/exposure.rs` (`auto_derive_horizontal_exposure`, `*_exposure_complete`) |
| Load reconstruction | `document/src/load.rs` (`StoreyDoc::from_storey`) |
| Resolve orchestration | `src/controller/app_controller/storeys.rs` (`materialize`, `refresh_storey`, `refresh_all_storeys`, `derive_horizontal_exposure_all`, `reconstruct_*`) |
| Persistence orchestration | `src/controller/app_controller/persistence.rs` (`open_ber`, `try_restore_snapshot`, `save_checkpoint`, `load_checkpoint`, `PARAMETRIC_SNAPSHOT_VERSION`) |
| DB / snapshot tables | `src/engine/ber_repository.rs` (`upsert`, `save_snapshot`, `load_snapshot`) |
| Dev checkpoint | `src/engine/checkpoint.rs` |
| Completeness rollups | `src/controller/app_controller/checklist.rs` |
| Repro artifact | `checkpoint.json` (Layer-1-only; reproduces §6) |
