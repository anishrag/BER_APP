# Subtract-don't-split wall derivation (stepped bottoms)

User ruling (2026-07-25, Elmwood field report): the inter-storey junction
derivation must SUBTRACT the buried band from a wall face (like the trim/ortho
carve) instead of SPLITTING the wall into per-bottom-height strips. One
physical wall = ONE exposed `WallRegion` whose bottom edge may step, plus the
`InterStorey` band region(s) as today. Split only where the exposure CLASS
changes (roof voids, porches keep their splits); never emit adjacent
same-class pieces. Zero-length/sub-sliver pieces must be impossible.

Confirmed user rulings:
- Sill datum is the FLOOR: a typed sill means "above the wall's true base",
  everywhere along the wall, including over a junction notch. (Today a window
  hosted on a notch piece has its sill measured from the piece bottom — that
  is the bug; unification fixes it. Existing windows keep their world
  positions; stored sill values rebase once.)
- "Full height" / default opening height over a notch offers the truly
  available face (underside→top) — but the sill the assessor types stays
  floor-referenced; the min-sill clamp (can't sink into the buried band)
  stays.
- A band fully crossing a wall leaves two disconnected remainders → those are
  legitimately TWO regions (connected components).
- Multi-storey window runs healing (wall_run_top_z full-edge matching) is a
  welcome behavior change; note it in the commit.

Fixture of record: "56 Elmwood Road" storey idx1, y=1.42 run — currently
three Exposed pieces (bottoms 0.34 / 0.79 / 0.34, the 0.79 span over the
below-storey's raised ceiling z 2.998) + sliver pieces + 4 windows spread
0+2+2. Target: ONE region, base_profile 0.34→0.79→0.34, all four windows
linked, world rects unchanged.

## Full design (API steward, verbatim)

### 0. Headline

The stepped-bottom wall is not a new geometry representation — it already
exists (sloped-profile clip `clip_wall_by_band_profile` emits non-flat-bottom
remainders today) and most consumers are polygon-generic:
`WallRegions::regenerate` builds base profile a→b ++ top profile b→a
(wall_regions.rs:371-397); `top_start_index()` (:54) finds the seam;
`base_z_at_along()` (:283) is the underside query; `min_sill_for()` (:190)
raises openings off a raised underside; `opening_vertices` clamps to the
underside (openings.rs:2345-2359); the sill-rebase pivot in
`reanchor_openings` (document/src/resolve.rs:1042-1060) makes an opening's
WORLD rectangle the invariant; render/ draws `polygon.exterior` verbatim.

### 1a. derive_inter_storey (ber_model/src/ber/structural/inter_storey.rs) — THE change

Replace `process_wall`'s (658-882) vertical `split_wall_by_segment` at run
ends + per-run `split_wall_by_line`/`clip_wall_by_band_profile` with a
single-pass all-runs boolean clip in unrolled wall-plane space:

```rust
/// Clip one wall against EVERY active neighbour-band run at once — the
/// covered piece(s) under each run's band-top profile become InterStorey
/// band regions (one per run, as today), and the single remainder
/// (wall face MINUS the union of all covers) becomes the exposed wall,
/// its BOTTOM tracing the band tops, never split vertically at run ends.
/// Same intact-identity guarantees as the old sloped path: no cover
/// overlap → wall pushed unchanged (same id, same ring); no remainder →
/// wall stamped InterStorey whole.
fn clip_wall_by_bands(wall: WallRegion, runs: &[Active], frame: ..., out: &mut Vec<WallRegion>)
```

- Per run, a cover polygon: run profile polyline on top (flat runs = 2-point
  profile — unifies the two old paths), sides at t0·len / t1·len (extended
  1 m past a wall end when the run end is within MIN_SUBWALL_M/len of it —
  the old guard_t logic as cover extension), bottom at band_lo when it
  strictly bisects, else extended below zmin.min(profile_lo) − 1.0 (verbatim
  from clip_wall_by_band_profile:951-961).
- covered_i = face ∩ cover_i per run (bands stay per-run);
  remainder = face − ⋃cover_i; both through split_to_simple +
  band_piece_is_degenerate filters (:1010).
- Rebuild rings with wall_piece_from_plane_poly (:1037) unchanged.
- DELETE: the cut_t/recursion vertical-split block (794-820), the flat-path
  split_wall_by_line loop (842-874), MAX_WALL_SPLIT_DEPTH.
- Keep the dmin/dmax > len bail (943-948) (coplanar-flush merged hosts).
- Disconnected remainder → one exposed region per connected component.
- Fixpoint: previously-notched wall re-derives covered=hairline→intact; band
  re-covers fully → stamped whole, same ring. Two-neighbour composition
  (j = i±1, two passes) = successive subtraction.

### 1b/1c. derive_roof_voids + derive_porch_exclusions KEEP splitting
(exposure-class partitions — legitimate). Only §2.2 rebase_piece hardening.

### 2.1 Must change — ber_model
- `WallRegion::base_edge` floor band (wall_regions.rs:99-126,
  MAX_FLOOR_STEP_M:696): anchor to the base run's MINIMUM z
  (lo = min_z − ε, hi = min_z + MAX_FLOOR_STEP_M), accept any max-along base
  vertex within band. Re-pin steep_profile_truncates_… +
  merge_coplanar_flush suite.
- `rebase_ring_start` (662-685): require valid frame with
  base_z ≤ ring z_min + MAX_FLOOR_STEP_M, then LONGEST base edge, tie-break
  minimal base_z.
- `wall_face_height` (openings.rs:247-252): top_z_at_along −
  base_z_at_along(along) (doc'd behavior change).
- `ElevPanel` + `Ber::side_elevation` (elevation.rs:82-87, 200-223, 247-249,
  317-325): add `pub bottom: Vec<(f64,f64)>` mirroring `top`, build edges
  from it, fold into bounds; KEEP scalar base_z (z at panel's `a`) for sill
  math; occluder box keeps flat bottom (band panels fill the notch —
  documented approximation like tops :299-305).
- NEW `WallRegion::base_profile() -> Vec<(f32,f32)>`: underside mirror of
  top_profile() (:201-222) over exterior[..top_start_index()]. Canonical
  bottom-profile query. base_z_at_along → pub ONLY for the try_host caller.

### 2.2 Hardening against stepped inputs
- `stamp_wall_phases` (document/src/phase.rs:152-166): use base_edge(), not
  raw exterior[0]/[1] (already wrong today for floor-stepped bases).
- `src/ui/render/structure_fan.rs:401-412`: use base_edge().
- `rebase_piece` (roof_voids.rs:188-213): accept base_z ≤ z_min +
  MAX_FLOOR_STEP_M; keep refusing top-edge frames.
- nudge `try_host` (document/src/resolve.rs:1552-1575): check against
  base_z_at_along over the opening's span.
- `ElevationView` (src/controller/app_controller/openings.rs:257-263,
  1681-1707 + canvas.rs:4901-4972): add bottom_profile (from base_profile()),
  draw like the top profile; keep base_z for sill math.

### 2.3 Proven unaffected (cite in review)
WallAnchor/reanchor_openings/host_wall_for_rect (plan-parametric, world-rect
invariant, sill rebase :1042-1060); 1-D exposure recovery
(recover_wall_region_span uses base_frame plan-only; pass-2
stacked_exposure_above degrades harmlessly — test obligation);
riser_key/wall_face_key/SurfaceRef (plan-only); wall_run_top_z (improved);
wall_net_area (Newell polygon area); render/ + BER_deap (ring-generic);
wall-face partition tool (pre-derivation; polygon-generic split);
porch apply_splits (whole-ring walk).

### 2.4 Flag
SurfaceKey::Wall(riser_key, wall_base_z_mm) (storeys.rs:26-46, 84-92) —
one-time Structure-review selection reset on migrating records. Acceptable.

### 3. Geometry rep
Storage unchanged (Polygon3D.exterior). Canonical ordering: existing wall
vertex contract stated symmetrically (doc update on regenerate +
top_start_index). Profile queries live on WallRegion in
ber_model::ber::structural (like top_profile).

### 4. Serialization/compat
No serde change. Old split records unify on next open (refresh_all_storeys →
regenerate rebuilds perimeter wholesale, derive notches; nothing durable
rides the split). Window on piece 2-of-3 re-homes via previous_walls world
rect + anchor parent-relative fraction + host_wall_for_rect; sill rebases
once (quantize + near-identity guard :1051-1057) — test world-rect equality
then save→load→save byte equality, NOT raw sill equality across migration.

### 5. Sliver invariant
Every emitted wall region is either (a) an input ring passed through
byte-identically or (b) a split_to_simple component that passed
band_piece_is_degenerate (area ≥ MIN_SUBWALL_M², minor dim ≥ MIN_SUBWALL_M).
Structurally enforced (no split_wall_by_segment/split_wall_by_line calls
remain in the pass). Test helper asserts it over every wall after derive and
double-derive; debug_assert! in clip_wall_by_bands.

### 6. Implementation order (each step gate-green: cargo build -p document &&
cargo build -p manip_plan && cargo build -p ber_model && cargo test
--workspace && fmt (skip pre-existing offenders: ber_model/examples/
porch_check.rs, manip_plan/examples/ring_check.rs, manip_plan/src/query.rs,
src/engine/sync.rs, src/ui/render/mod.rs) && boundary scan)

- **Step 1 — contract hardening** (behavior-preserving on current geometry):
  base_edge band, rebase_ring_start, rebase_piece, stamp_wall_phases,
  structure_fan, try_host. Tests: re-pin steep_profile_…,
  merge_coplanar_flush suite, stepped-base stamp_wall_phases unit,
  >0.6 m-step base_edge unit.
- **Step 2 — the derive rewrite**: clip_wall_by_bands; delete splits; Elmwood
  fixture (one region, base_profile 0.34→0.79→0.34, 4 windows world-rect
  stable, bands InterStorey no fabric, sliver invariant, double-derive
  idempotent, L-shaped end case, two-crossing → two components; re-pin
  sloped-profile tests).
- **Step 3 — migration + persistence proof**: pre-change-shaped record
  fixture (three pieces + slivers + piece-relative sills) → unifies on load,
  sills rebase once, second save byte-stable, sections one span,
  constructions/phases/riser stamps land.
- **Step 4 — elevation bottom profile**: base_profile(); ElevPanel.bottom +
  side_elevation; ElevationView.bottom_profile + canvas draw;
  wall_face_height underside fix. Stepped-base side_elevation pins.

### 7. Notes
Per-run InterStorey bands stay unmerged (nudge rectangles +
host_wall_for_rect tiers untouched); cheap to revisit.
