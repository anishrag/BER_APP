# Roof point heights (A) + roof-aware wall generation (B) — interface spec (api-engineer, 2026-06-11)

Status: DESIGN — awaiting Anish's approval + answers to the open questions before implementation.
(Full spec below is the api-engineer's Mode-1 output, lightly trimmed.)

## Key decisions

### A — Point heights (sloped roofs)
- **Durable, not session**: `Storey.roof_apexes: Vec<RoofApex { point: Point2f64, z_m: f64 }>`
  (#[serde(default)]). A fan apex is NOT cleanly invertible from triangulated geometry, so the
  declarative-input pattern (like ceiling_height_m) is the honest one. resolve_storey becomes a
  pure function of footprint + height + step areas + apexes; reload fixpoint by construction.
- **Composition order is law**: default_for_footprint → with_z_deltas (steps) → with_apexes
  (points). with_z_deltas NEVER sees a sloped region (its flat-input stamp would corrupt fan z).
- **Apex-over-step reload fixpoint**: ceiling_step_regions_from_roofs becomes APEX-AWARE — a new
  CeilingRegion::fan_base_z(apexes) drops apex-matching vertices and reads the shared remaining z,
  so a stepped piece under a fan recovers its step delta on reload (the load-bearing invariant;
  test end-to-end).
- **Height = ABSOLUTE z above the storey floor datum** ("ridge at 4.2 m"); finite, > 0,
  |z − base| ≥ 0.01; below-base (dip) allowed with the warn-not-reject philosophy; z ≤ 0.1 warns.
- **Fan op**: CeilingRegions::with_apexes — boundary-INCLUSIVE containment (required: a re-applied
  apex is a corner of its own fan pieces), one triangle per boundary edge, boundary keeps its z,
  sliver-drop makes re-fan-to-self identity. Assign-time validation uses STRICT containment
  (region_strictly_containing) — two deliberately different predicates.
- **Interaction**: gate = ceiling-step gate; TAP → per-axis snap (snap_ceiling_step_line reused
  per axis; both snapped = corner snap free; ONE tag, "Don't snap" reverts both axes) → position
  prompts for each UN-snapped axis, X then Y, reusing LinePositionPrompt VERBATIM (+
  PositionPromptKind::Point, never serialized, unreachable! arms) → height entry → assign
  (materialize → validate → Ber::add_roof_apex → mark_dirty → materialize → re-canonicalize the
  ceiling-step session list). v1 REJECTS an apex in an already-sloped region (one point per area).
- Rendering: 3D free (per-vertex z already); 2D gains apex markers ("▲ 4.20 m"), corner-label
  dedupe by rounded (x,y), apex list with delete in the toolbar.

### B — Wall generation with amend-vs-create
- **Wall existence stays EXPOSURE-DRIVEN** (generating all walls would wreck the marked/unset
  distinction the reload recovery depends on). Roof changes AMEND marked walls' geometry.
- **Identity**: WallRegions::regenerate REPLACES rebuild_preserving. Same-wall = base edge matches
  a previous region's (both endpoints within 1e-2 m, either orientation; injective — best match
  wins, never duplicate ids) → keeps id + construction_id + OPES (amend). Split/extension
  (covered, endpoints differ) → inherits construction_id only, fresh id. Fresh segment → default
  cid. (Verified: nothing currently depends on ids regenerating.)
- **Geometry**: wall top TRACES the roof profile along the base (piecewise-linear over planar roof
  regions, VERTICAL JUMP where a ceiling step crosses the wall — perimeter envelope gap-free).
  Vertex-order CONTRACT: ext[0]/ext[1] = the base edge always (exposure recovery + base_midpoint
  read positionally); top complexity appends after index 1. Fallback flat top when height unset.
- **Trigger**: none new — resolve_storey already derives roofs before walls every materialize.
- **Risers DEFERRED** (interior roof-step/fan faces + floor risers incl. exposed-to-ground);
  resolve.rs SEAM comment restated to cover both families.
- Serde: only Storey.roof_apexes new; build_persist_record's unfinalised branch zeroes roof_apexes
  (roofs parity); no snapshot bump.

## Slices + order
A-1 ber_model (RoofApex, with_apexes, region_strictly_containing, fan_base_z, add/remove_roof_apex)
∥ B-1 ber_model (WallRegions::regenerate + private profile sampler; replace rebuild_preserving) —
independent. Then A-2 document (roof_point.rs validation; apex-aware step reconstruction;
PositionPromptKind::Point; resolve applies with_apexes — NO resolve signature change, apexes ride
the cloned Storey) ∥ B-2 document (resolve calls regenerate with roofs; rewrite wall comment +
riser SEAM) — same file, disjoint edits, land sequentially. Then A-3 controller
(roof_points.rs: PendingRoofPoint {Position, Height} with per-axis snaps; no session region map —
durable storage; the three position entry points gain Point arms; clears at every
pending-clearing site; NO reconstruct sites) + A-4 UI (events, toolbar, markers, tap routing,
prefill via the Point kind). B is COMPLETE after B-2 (no shell work). A and B land independently;
fans flow into wall tops automatically once both are in.

## Behavior traps
1. Composition order (above). 2. Two containment predicates (above). 3. The apex-over-step
fixpoint chain — test end-to-end. 4. Canonicalize ceiling-step session list after apex
assign/delete (fan recovery emits triangle step areas; in-session must equal post-reload; known
UX cost: step list fragments into triangles under a fan — accepted for now). 5. Serde as above +
old-JSON test. 6. Apexes outside undo (parity). 7. Wall vertex-order contract. 8. Injective id
inheritance. 9. Gate double-enforced; stable error strings. 10. regenerate runs every
materialize — keep O(n·m), no caching.

## Open questions for Anish
1. Multiple apexes per ceiling region: v1 rejects ("one high point per ceiling area for now");
   with_apexes supports it structurally. Lift later?
2. Apex exactly ON a boundary/outline wall (shed/lean-to roofs): v1 rejects; a ridge LINE
   primitive is probably the right tool for shed/gable — want that as a follow-up feature?
3. Wall identity by provenance (tag WallRegion with source (VertexId, start_m), surviving outline
   edits that move the wall slightly) — deferred hardening; geometric match covers the stated
   scenario.
4. Risers stay deferred (per above).
5. 3D wall rendering: storey_3d still draws the volume-hint band, not WallRegions — sloped wall
   TOPS won't show in 3D until a follow-up renders real wall regions (fan roofs DO show).
6. Step-list fragmentation under a fan — accepted or merge-on-display later?
