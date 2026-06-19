# Roof RIDGE (two-apex pitched roofs + ridge-line input) + gen_3d_vol dissolution
# (api-engineer Mode 1, 2026-06-11) — APPROVED with recommended defaults:
# >=3 apexes/group rejected for now; ridge-line endpoints = stroke extent as drawn (no endpoint
# prompts; only the cross coordinate gets snap + LinePositionPrompt); line input = one height
# (level ridge; sloping via two-point flow); axis-aligned v1; split_by_line MOVES to ber_model;
# group base-z disagreement = warn-exclude.

## D1 — Durable model: Storey.roof_apexes unchanged; ridge = two apexes in one GROUP.
- No new serde, no snapshot bump. Deleting one apex degrades ridge→fan. Differing end z = sloping
  ridge (allowed). The two-point flow IS the model; the line input just places both at once.
- THE REPAIR (load-bearing): grouping/merging moves into the RECONSTRUCTION, not resolve.
  ceiling_step_regions_from_roofs becomes GROUP-AWARE: all regions shaped by one apex group are
  union-merged into ONE step area at the group's shared base delta. Consequences: post-step
  regions at resolve are whole conceptual areas (per-region containment grouping in with_apexes
  suffices); the "step list fragments into triangles under a fan" wart DIES; resolve_storey
  signature + call sequence UNCHANGED.
- Grouping semantics: apex ↔ region by quantized plan-corner identity (1e-6, fan_base_z's
  predicate), closed TRANSITIVELY (transition triangles carry both endpoints, linking A to B).
  At resolve, with_apexes groups by boundary-inclusive containment per post-step region:
  0 keep / 1 fan / 2 ridge / >=3 warn + first two in apex-list order (deterministic, never panic).
- Reload fixpoint chain: persisted ridge = hub triangles (u,v,H) + transition triangles (w,A,B);
  fan_base_z RELAXED to >=1 remaining non-apex vertex sharing one z (transition triangles keep
  exactly one; the needle test at structural.rs:2270 flips to Some deliberately — needles still
  die at the reconstruction's ring_area2 filter); reconstruction unions the group → one area at
  base delta; re-derive recreates the flat piece → grouping finds {A,B} → deterministic
  triangulation reproduces the same triangles. Settled after the first canonicalize (assign does).

## D2 — Ridge triangulation (private, structural.rs, the 2-apex branch of with_apexes)
Inputs: region ring (per-vertex z), apexes A=(pA,zA), B=(pB,zB), both contained (inclusive).
0. PRE-LIFT (load-bearing for idempotence): any ring vertex plan-matching an apex (1e-6) gets
   that apex's z. Apply in the FAN branch too (consistency; only affects dropped slivers there).
1. d = pB−pA; |d|^2 < 1e-12 → warn + single fan to A. t(P) = (P−pA)·d/|d|^2 (f64, unclamped).
2. Per ring edge e=(vi,vi+1): hub(e) = A if t(midpoint) < 0.5 else B (tie→B). Emit hub triangle
   (vi, vi+1, H) — boundary keeps (pre-lifted) z, H=(pHub,zHub). Drop plan-area < SLIVER_AREA_M2.
3. Per ring vertex where hub(entering) != hub(leaving): emit transition (v, hub(leaving),
   hub(entering)) — preserves CCW winding. Same sliver drop.
4. Fresh ItemIds, default Opes, inherited construction_id (fan parity).
Properties (doc + test): exact tiling for convex / <=2 hub switches (areas sum to region);
every triangle planar; RE-APPLY-TO-SELF IDENTITY for hub AND transition triangles (all three
t(w) cases — pre-lift makes it hold); >2 hub switches = overlapping transitions → REJECT at
assign (stable message "too irregular for a single ridge"), WARN+proceed at resolve.

## D3 — Input UX
(a) Two-point flow = validation change ONLY (controller flow untouched): validate_roof_apex
becomes group-aware via apex_groups — strictly-containing region → its group → 0 apexes: first-
point path; 1 apex: ALLOW (ridge forms; base = group base_z; hub-switch check on merged ring);
>=2: reject, NEW stable message ("…already has a ridge — two high points per ceiling area for
now"). Sloped region in NO group: irregular-slope rejection. REGION_ALREADY_SLOPED_MSG replaced;
tests matching it change together.
(b) Ridge-line input: axis-aligned v1, STROKE-only in roof-point mode (stroke=ridge, tap=point —
the ceiling-step stroke/tap routing pattern); endpoints = the stroke's own projected extent;
cross coordinate gets snap_ceiling_step_line + LinePositionPrompt (kind Line, no new kind);
one typed height for both endpoints; both endpoints must land in the same zero-apex region
(reject up front, stable message). Assign = add_roof_apex ×2 (quantization included).

## D4 — Walls: ZERO changes (regenerate's plane sampler is generic over planar regions);
optional one wall-under-ridge confidence test.

## Slices (order: R-0 first, then R-1 ∥ R-2, R-3, R-4)

R-0 (data-model-engineer) — DISSOLVE gen_3d_vol:
- Move Polygon3D::split_by_line + SplitLine3D + private normal helper + tests into
  ber_model/src/ber/structural.rs as impl Polygon3D, with a seam comment naming the consumer
  (the deferred wall face/height diagonal split).
- DELETE gen_3d_vol/ entirely; remove from workspace members (Cargo.toml); update CLAUDE.md
  (workspace layout line, rule 5, responsibility table), .claude/rules/scope-discipline.md,
  .claude/rules/architecture-boundaries.md crate list, .claude/agents/*.md mentions.
  check-boundaries.sh has no reference (verified). Root earcutr stays (storey_2d uses it).
- Verdicts: from_ber_storey DELETE (viewers render Storey directly); CeilingMesh + insert_vertex
  + ridge machinery DELETE (superseded by the ber_model ridge — the no-duplicates directive);
  split_wall_region DELETE (shell-flavoured scaffolding; diagonal split designs fresh against
  the moved split_by_line); 2D point helpers DELETE (only ceiling.rs consumed them).

R-1 (geometry-engineer) — manip geom.rs:
  /// Fit an AXIS-ALIGNED segment ... returning the stroke's projected EXTENT along the fitted
  /// line (a ridge needs endpoints, not an infinite line). (axis, coord, (lo, hi)).
  pub fn fit_axis_segment(points: &[Point2f32]) -> Option<(ParameterAxis, f32, (f32, f32))>
  fit_axis_line reimplemented as fit_axis_segment(..).map(|(a,c,_)|(a,c)). Re-export.

R-2 (data-model-engineer) — ber_model structural.rs:
  pub struct RoofApexGroup { pub apex_idxs: Vec<usize>, pub region_idxs: Vec<usize>,
    pub base_z: f64, pub merged_rings: Vec<Vec<Point2f64>> }  // doc per spec
  impl CeilingRegions {
    pub fn apex_groups(&self, apexes: &[RoofApex]) -> Vec<RoofApexGroup>  // THE grouping authority
    pub fn with_apexes(self, apexes: &[RoofApex]) -> Self  // SEMANTICS CHANGE: one pass, grouped
      // per region by inclusive containment: 0 keep / 1 fan / 2 ridge / >=3 warn+first-two
  }
  impl CeilingRegion { pub fn fan_base_z(...) }  // RELAXED to >=1 remaining
  pub fn ridge_hub_switches(ring: &[Point2f64], a: Point2f64, b: Point2f64) -> usize
  private: ridge_triangles (D2), fan_triangles (extracted).
  Group base-z disagreement → warn-exclude region from merge.
  Tests: rectangle gable (4 hub + 2 transition, areas sum); transition re-apply identity (3 t(w)
  cases); hub re-fan identity; sloping ridge; sliver; apex_groups transitive + merged ring =
  footprint; fan_base_z relaxation (needle test → Some(2.4)); hub_switches convex=2 / notch>2.

R-3 (document-engineer) — after R-2 (+R-1 for the fit):
  validate_roof_apex: SIGNATURE UNCHANGED, group-aware internals per D3(a); three-way stable
  messages.
  pub fn validate_roof_ridge(storey, a, b, z_m) -> Result<(), String>  // both strictly inside
    // the SAME region; group empty; endpoints distinct after f32 quantization; |z−base| >= 1cm;
    // hub switches <= 2
  impl StoreyDoc { pub fn fit_roof_ridge_line(&self, stroke) ->
    Result<(ParameterAxis, f32, (f32, f32)), String> }  // fit_axis_segment + cross-coord
    // footprint check + both endpoints in bbox
  ceiling_step_regions_from_roofs: signature unchanged; group-aware merge per D1 (one region per
  merged ring at base delta; plain off-default flats as today; ungrouped slopes warn-skipped).
  Doc rewrite (fragmentation note dies).
  Tests: step + ridge over stepped area → reconstruct → re-derive → equal → again → equal;
  validation matrix; wall-under-ridge confidence test.

R-4 (shell-engineer) — controller + UI:
  pub(super) enum PendingRoofRidge { Position { axis, coord_m, extent, prompt },
    Height { axis, coord_m, extent, snap: Option<CeilingLineSnap> } }  // reuse CeilingLineSnap
  apply_roof_ridge_stroke_event (fit → cross-coord snap → prompt or Height);
  assign_roof_ridge_height (materialize → validate_roof_ridge → add_roof_apex ×2 → mark_dirty →
  materialize → reconstruct canonicalize; pending stays open on Err); unsnap_roof_ridge;
  roof_ridge_snap_tag; pending_roof_ridge_line (ENDPOINTS, not bbox-spanning);
  roof_ridge_position_pending/height_pending; cancel; roof_ridge_views ("⌃ 4.20 m" /
  "4.20→4.50 m" sloping). Cleared everywhere pending_roof_point is; position entry points gain
  the ridge arm (kind Line); stroke suppression gains the pending.
  UI: SketchEvents RoofRidgeStrokeWorld/AssignRoofRidgeHeight/UnsnapRoofRidge/
  CancelPendingRoofRidge; roof_ridge_draw = roof_point_mode && DrawPerpWalls stroke routing
  (stroke=ridge, tap=point); dashed SEGMENT render; committed ridge overlays; snap tag routed to
  UnsnapRoofRidge; height panel reuse.
  Controller tests: stroke→prompt→height→materialize = pitched roof (two hub fans + transitions);
  two-point flow produces the SAME roof; snap skips prompt; unsnap reopens; reload fixpoint;
  delete one apex → fan; third point in ridge area rejected.

## Behavior traps
1. fan_base_z relaxation (needle test flips deliberately). 2. Stable-string changes move with
their tests. 3. PRE-LIFT is what makes re-apply identity — don't skip. 4. Transition winding
(v, leaving, entering); tie t>=0.5→B; determinism is part of the fixpoint. 5. Union settles in
assign's canonicalize; end-to-end test asserts identity on canonicalized state + idempotence.
6. Shared sliver constant. 7. Apexes go through Ber::add_roof_apex (quantization) — ridge
endpoints included. 8. Step-through-apex double-fan = pre-existing, don't fix in passing.
9. Outside undo (parity). 10. No serde change; build_persist_record untouched.
11. resolve_storey unchanged; composition order law. 12. >=3 in group at resolve: warn+first-two.
