# New-storey flow rework — Mode-1 spec (api-engineer, 2026-06-12). APPROVED, defaults:
# thickness REQUIRED (prefill 0.30, "Use 0.30" = submit); H4 emits after EVERY merge; anchor not
# re-based on later footprint growth (v1, warn; redraw re-seeds); single-ring suggestions v1
# (largest union piece); datum display deferred; ranking constants in code not tuning.yaml.

[FULL SPEC AS DELIVERED — slices A–D, heuristics H1–H6, datum rule, traps 1–10.]

## Key decisions
1. Storey.floor_thickness_m: Option<f64> (serde default) — durable, asked FIRST on add-storey
   (never storey 0), prefill 0.30, re-editable in Heights (storey ≥ 1), OUTSIDE undo.
2. DATUM RULE: z_0 = 0; z_N = z_{N−1} + (thickness None → below ceiling height [LEGACY,
   byte-identical for old records]; Some(t) → anchor_z + t where anchor_z = MIN flat_z among
   below-roof regions intersecting U's footprint (> SLIVER_AREA_M2), fallback below ceiling
   height). Inherited levels: each flat below-region with z > anchor + RISER_Z_EPS_M
   intersecting the footprint → piece ring∩footprint at level = z − anchor (thickness cancels).
   Sloped covered regions warn, contribute nothing.
3. INHERITANCE = AT-CREATION seeding of session StepRegions (the existing step machinery +
   its tested reload fixpoint; risers derive free). NOT a resolve input (no cross-storey
   coupling). Seed timing: suggestion path at ACCEPT; drawn path at the FINALISATION transition
   (outline_finalised first true). Controller session set inherited_seed_pending: HashSet<usize>
   — inserted by add_storey (idx>0) AFTER load_storey_into_editor(None); re-derived on
   open/restore as {idx>0, outline un-finalised}; consumed at seed; cleared on exit_ber.
4. Suggestion engine in DATATYPES (ber/suggest.rs — pure data transform; document has no geo).
   Adoption in document; sequencing in controller.
5. Accepted suggestion = FULLY-FINALISED outline (from_storey pattern: from_clean_ring +
   all params known + scale_set). NOT the carve path. NO collinearity review, no prompts.
6. Serde: one optional Storey field; no snapshot bump (all flow state controller-transient).

## A — datatypes (data-model-engineer)
CeilingRegions::lowest_flat_z_over(&self, footprint: &[Point2f64]) -> Option<f64>
CeilingRegions::inherited_floor_levels(&self, footprint) -> Vec<(Vec<Point2f64>, f64)>
  (pieces region∩footprint via split_to_simple, slivers dropped, f32-quantized, z − anchor)
Storey.floor_thickness_m + Ber::set_floor_thickness(idx, t) +
Ber::storey_floor_datums() -> Vec<f64> (the datum rule; unset heights count 0 — callers gate).
NEW ber/suggest.rs: OutlineSuggestion { ring (closed, metres, f32-quantized,
collinear-stripped), area_m2, levels, source: &'static str, score }, SuggestionContext { flats,
flat_union, footprint, footprint_area, all_flat },
pub type OutlineHeuristic = (&'static str, fn(&SuggestionContext) -> Vec<Vec<Point2f64>>);
pub const HEURISTICS: &[OutlineHeuristic]  // ADD A HEURISTIC = one fn + one slice entry
pub fn suggest_storey_outlines(below: &Storey, max: usize) -> Vec<OutlineSuggestion>
HEURISTICS: H1 all-flat union (largest piece); H2 largest single flat region; H3 same-z groups
(per z union, connected components ≥ min area); H4 grown-from-largest (greedy adjacent merge,
EMIT AFTER EVERY MERGE); H5 largest inscribed axis-aligned rectangle in H1's piece (coordinate
grid O(n^4), containment via area(rect∩P) ≈ area(rect)); H6 full footprint iff all_flat.
Pipeline: run all → validate (≥3 verts, area ≥ MIN_SUGGESTION_AREA_M2 = 4.0, strip-collinear,
f32-quantize) → score = 1.0·area/footprint_area + 0.5·area/bbox_area − 0.15·(levels−1) −
0.02·max(verts−4,0) → sort desc → dedup (sym-diff area < 0.02·larger) → cap max.
Tests: L-shape + stepped ceiling (H1/H2/H3/H5, ranked/deduped/capped); all-flat → footprint
once; unset height → empty; datums legacy vs anchored; old-JSON Storey loads.

## B — document (document-engineer)
step.rs: pub fn inherited_step_regions(below: &Storey, footprint: &[Point2f32])
  -> Vec<StepRegion>   // thin wrapper over inherited_floor_levels; re-export
lib.rs StoreyDoc::adopt_finalised_outline(&mut self, ring: &[Point2f32]) -> Result<(), String>
  // outline + live buffer replaced, finalised like a loaded storey; Err on degenerate
Tests: adopt → finalised/scale_set/resolve round-trips ring exactly; END-TO-END fixpoint
(below ceiling step → suggest → adopt → seed → resolve → reconstruct → re-resolve identical,
riser id stable).

## C — controller (shell-engineer; new new_storey.rs + mod.rs fields)
NewStoreyStage { Thickness, ChooseMethod, Browse { suggestions, cursor } };
PendingNewStorey { stage }; fields pending_new_storey (transient, outside undo) +
inherited_seed_pending: HashSet<usize>.
Methods: floor_thickness() (display, None on storey 0/unset); assign_floor_thickness(v) (validate
finite ≥ 0, stamp, advance Thickness→ChooseMethod, mark dirty); set_floor_thickness(v) (Heights
re-edit, no stage change); choose_draw_outline(); choose_suggest_outlines() -> Result (stable Err
when below has no flat ceilings/height); cycle_suggestion(forward); accept_suggestion() ->
Result (undo snapshot → adopt_finalised_outline → seed via document::inherited_step_regions +
commit + reconstruct canonicalize → clear pending → mark dirty; NO review);
new_storey_prompt() -> Option<NewStoreyPromptUiData { thickness_pending, method_pending,
can_suggest, suggestion: Option<(ring, label, idx, count)> }>;
maybe_seed_inherited_levels() (drawn path: idx>0 ∧ seed-pending ∧ outline_finalised → extend
step_regions[idx] + commit + reconstruct + consume).
Touch-points: add_storey sets pending=Thickness + seed-pending AFTER load_storey_into_editor;
clears at set_current_storey/load_storey_into_editor/exit_ber/open_ber; open/restore re-derives
seed-pending; apply_dimension/skip_current_dimension/apply_outline_world_inner tails call
maybe_seed_inherited_levels. Stroke gating: Thickness ignores outline strokes (dock explains);
ChooseMethod/Browse → stroke = implicit choose_draw_outline.
Tests: thickness-first flow; suggest/cycle/accept (finalised, no prompts, no review); drawn path
seeds at finalisation incl. across restart (re-derived pending); legacy None stacking; fixpoint.

## D — UI (shell-engineer)
Events: AssignFloorThickness, SetFloorThickness, ChooseDrawOutline, ChooseSuggestOutlines,
NextSuggestion, PrevSuggestion, AcceptSuggestion. SketchView.new_storey_prompt + thickness input
buffer + error. new_storey_toolbar at TOP dock priority (can't coexist with pending_redraw).
Canvas: translucent fill of the current suggestion ring over the watermark. Heights: thickness
field (storey ≥ 1) mirroring the ceiling-height field. full_house_3d: replace the inline
z-accumulation with ber.storey_floor_datums() (heights gate stays).

## Traps
1. Seed = the exact datatypes pieces (canonical) — fixpoint test end-to-end. 2. with_levels is
earlier-claims-first — seeded areas precede later user steps (existing semantics, don't "fix").
3. Drawn path seeds at FINALISATION; seed-pending re-derived on open (restart-safe); finalised/
old records never retro-seed. 4. Accept SKIPS the review + does NOT use carve (two deliberately
different paths). 5. f32-quantize suggestion rings at the datatypes boundary. 6. Thickness
outside undo; ACCEPT inside undo (snapshot before adoption; undo → empty outline, Browse stage
not restored, seeded steps remain — harmless, consistent on redraw). 7. build_persist_record
unfinalised branch KEEPS floor_thickness_m. 8. Legacy None stacking byte-identical. 9.
choose_suggest disabled by gate AND stable Err defensively. 10. confirm_add_storey_from_current
stub untouched (flag).
