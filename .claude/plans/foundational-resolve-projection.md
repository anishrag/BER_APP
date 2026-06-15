# Plan: Geometry as a projection — multi-storey `Document`, kill `commit_to_ber`

## The target architecture (the "good solution")

Today two structures hold storey geometry and must be kept in sync: the single-storey
`Document` (editing source) and `current_record: Ber` (which double-duties as the durable
assessment record **and** a per-edit-synced render cache, via `commit_to_ber` at ~30 sites).
Both "eager" and "lazy" syncing are lateral — the real fix removes the stored synced copy.

End state:

```
Document  ── the whole building's editing model (the ONLY geometry truth)
├── storeys: Vec<StoreyDoc>     one per storey (StoreyDoc = today's `Document`, renamed)
├── active: usize               which storey is being edited
└── (one live editing cursor — the active storey's active shape)

record: Ber  ── assessment ONLY (constructions, phases, openings, systems, address)
               NO geometry held in memory

resolve_storey(&StoreyDoc, &constructions, phases) -> Storey   ── PURE projection
   ├── render:  document.storeys.map(resolve_storey)   (all storeys, one source)
   ├── persist: resolve every storey + record's assessment -> full Ber -> upsert
   └── export:  same
```

- No `commit_to_ber`, no `mark_dirty`-everywhere, no cache to invalidate. Geometry is a
  **view computed from `Document`**, memoized only as a local pure-fn optimization if profiling
  demands.
- Multi-storey `Document` makes "render all storeys" trivial (resolve each) and "switch storey"
  lossless (`document.active = n`, no rebuild-from-points).
- The snapshot work already serializes a `Document`; when it's the multi-storey container, reopen
  restores the whole building exactly.

## Prerequisite already in place

- The single-storey `Document` is already extracted (`document` crate; controller holds
  `self.document`) and is `serde`-able (the snapshot increment).
- A/B/E persistence done: idle/boundary saves, background writer, editing snapshot.
- Overlay getters (`living_area_polygons`, `wall_exposure_views`) already derive from `document`.

## Increment sequence (each compiles + `cargo test --workspace` green)

> Progress: **#1 partly done** (read-only `floor_construction_id`/`wall_construction_id`
> lookups added; pre-registration deferred into #3 where it's needed).
> **#2 done** — `resolve_storey(&mut self, idx) -> Storey` extracted from `commit_to_ber`
> (behavior-preserving; `commit_to_ber` is now a thin wrapper that calls it, assigns the
> result, stamps phases, bumps revision). Still `&mut` (registers constructions).
> **#3 done (pending live test)** — per-edit derive removed. `commit_to_ber` split into
> `refresh_storey` (derive) + `mark_dirty` (revision bump only) + `materialize` (lazy derive,
> gated on `revision != derived_revision`). The ~20 per-edit call sites now `mark_dirty`;
> render (Sketch page) and persist (`flush_if_dirty`/`autosave`) call `materialize` first.
> `commit_to_ber` (eager) KEPT for the 3 boundary callers (`save_checkpoint`,
> `set_current_storey`, `add_storey`) that must save/read `current_record` immediately, and
> for the tests. UI geometry-reader audit: only the Sketch page reads `current_ber_clone`, and
> it materializes. Tests that read `current_record` after an edit now call `materialize()`.
> **#4 done (pending live test)** — multi-storey `Document`. Today's `Document` renamed
> `StoreyDoc`; new `Document { storeys: Vec<StoreyDoc>, active_storey }` with `Deref`/`DerefMut`
> to the active storey (so the ~150 `self.document.<field>` accesses are unchanged — only one
> test needed a disjoint-borrow split). `add_storey` `push_storey`s a matching `StoreyDoc`;
> `set_current_storey(Some(i))` is now a LOSSLESS switch (`set_active_storey` — no
> rebuild-from-points); `set_current_storey(None)` empties only the live buffer for the All-tab;
> `open_ber` rebuilds all N storeys (fallback) then `try_restore_snapshot` restores the exact
> multi-storey Document; snapshot version 1→2; `refresh_storey` skips when no active storey.
> `HistorySnapshot` clones the whole `Document` (undo is per-storey; other storeys unchanged).
> **#5 — cornerstone done; full shed declined.** `resolve_storey` is now a PURE `&self`
> per-storey projection: construction ids are LOOKED UP (`floor/wall_construction_id`), and the
> two `&mut` prerequisites (lazy-create the storey slot + register constructions via
> `ensure_storey_constructions`) moved to the caller `refresh_storey`. Behavior-preserving (19
> suites green, incl. the construction-preserve test). The remaining step — `current_record`
> holding ZERO geometry — was DECLINED: it would require gutting/reworking the (tested,
> forward-looking) wall-construction-preserve mechanism (`rebuild_preserving` reads the existing
> resolved walls), for zero functional benefit — #5 is neutral for DEAP, the DB, and durability.
> End state: `Document` is the multi-storey geometry truth; `resolve_storey` is the pure
> projection; `current_record` is a lazily-refreshed cache fed by that projection.

**1 — Side-effect-free construction lookup (enabler).**
`commit_to_ber`'s derivation calls `ensure_floor/wall_construction` (mutates `Ber`'s registry).
Make the registry *pre-populated* so the projection can *look up* instead of insert:
- Add read-only `floor_construction_id(type, makeup, phase) -> Option<ItemId>` /
  `wall_construction_id(...)` to `Ber` (datatypes).
- Pre-register the default floor + default wall on `create_ber`/`open_ber`, and one floor per
  phase on `set_construction_phases`.
- No behaviour change; tests green. *Risk: low.*

**2 — Extract `resolve_storey(&self, StoreyDoc, &Ber, idx) -> Storey` (pure).**
Lift the floors/walls/living-areas derivation out of `commit_to_ber` into a pure function using
the read-only lookups from #1 (passing in the prior storey's `walls` for `rebuild_preserving`).
`commit_to_ber` becomes `let s = resolve_storey(...); write s into current_record; bump`. No
behaviour change. *Risk: medium — guard with the round-trip tests; this is the cornerstone.*

**3 — Drive render + persist from `resolve_storey`; delete `commit_to_ber`.**
- Render (`current_ber_clone` / the path feeding `storey_2d`) resolves the active storey from
  `document` on demand (memoize on `revision`); other storeys from `record`.
- `build_persist_record` resolves instead of cloning `current_record`'s synced geometry.
- Remove the ~30 `commit_to_ber` calls. Edits just bump `revision` (centralize the bump in the
  `UiEvent` dispatch so there's no per-site discipline).
- **Needs a live smoke run** (staleness audit of every `current_record`-geometry reader).
  *Risk: high — do with the app running.*

**4 — Promote `Document` to multi-storey.**
Rename today's `Document` → `StoreyDoc`; new `Document { storeys: Vec<StoreyDoc>, active }`.
`load_storey_into_editor` populates all storeys' `StoreyDoc`s; switching storey is
`document.active = n` (lossless — delete the rebuild-from-points path for switching). Rewire the
controller's `self.document.*` refs. The snapshot now captures every storey. *Risk: high —
touches the whole controller; live-test undo/redo + switch + dimension.*

**5 — `Ber` sheds geometry.**
With all storeys in `Document`, `record` holds assessment only; geometry is resolved on demand
(render) and materialized into a full `Ber` only at persist/export. Multi-storey overview/
watermark render straight from `document.storeys`. *Risk: medium, mostly mechanical after #4.*

## Critical files

- `datatypes/src/ber/structural.rs` — `Ber` construction lookups (#1), `Storey` shape (#5).
- `document/src/lib.rs` — `StoreyDoc` rename + new `Document` container (#4).
- `src/controller/app_controller/mod.rs` — `resolve_storey` (#2), render/persist rewire +
  delete `commit_to_ber` (#3), multi-storey rewire (#4), `load_storey_into_editor` (#4).
- `src/ui/app.rs` / `src/ui/render/storey_2d.rs` — render from `document.storeys` (#3, #5).

## Guardrails

- Each increment: `cargo build`, `cargo test --workspace`, `cargo fmt --all --check`, the
  boundary scan, `cargo build -p document`.
- Increments 3 and 4 are the ones that **need a live run** (draw → dimension → switch storey →
  undo/redo → idle-save → crash → reopen) — the test suite alone won't cover render staleness.
- The eager-vs-lazy `commit_to_ber` question never returns: by #3 there's no stored geometry to
  sync, and by #5 `Ber` holds no geometry at all.
