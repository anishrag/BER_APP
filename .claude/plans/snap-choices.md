# Plan: elective-snap choices — tag, alternatives, pick-and-replay

Goal: when an elective snap happens, tag it in the canvas with the chosen `SnapKind`
(the most-aggressive candidate that passed its threshold). Click the tag → see the
other candidates that passed → pick one → the action **replays** with that choice
forced, rebuilding the shape.

Architecture rests on the pieces already built: the `elective_snap` choke point,
the `OnRing` find/apply split, and the `record_phase` thread-local pattern.

## Increments (each compiles + `cargo test --workspace` green)

**1 — Structured snap recording (foundation).** [manip_2d_plan, unit-testable]
- `SnapKind` → `pub`; add `pub SnapCandidate { kind, to, score }` and
  `pub SnapDecision { from, candidates, chosen }`.
- A thread-local `SNAP_DECISIONS` (like `SPLICE_PHASES`); `record_choice(from,
  candidates, chosen) -> Point2f32` records + returns the chosen point.
- Reframe `elective_snap` as a **single-candidate** `record_choice` (existing
  sites unchanged, now recorded).
- `pub fn take_snap_decisions() -> Vec<SnapDecision>` (drain), re-exported.

**2 — Candidate enumeration.** [manip_2d_plan]
- `find_candidates(ring, p, tols) -> Vec<SnapCandidate>` (top-K corners, nearest
  edge, no-snap), ranked most-aggressive first.
- `elective_choice(decision_id, candidates) -> Point2f32` — consults the forced
  policy (#3) else picks the top passing candidate; records.
- Convert the elective sites (`snap_to_corner`, the `find_nearest`-based corner
  snaps in carve/path_edits, `ToParallelWall`) to enumerate + `elective_choice`.
- Define the aggressiveness total order (corner ≻ edge ≻ parallel ≻ none).

**3 — Forced-choice policy + per-action replay.** [controller; needs live test]
- `SnapSession { policy: map<decision_id, choice>, decisions }` thread-local,
  set by the controller before an action, drained after.
- Each snapping action keeps its raw stroke; replay = restore the pre-action
  snapshot + set policy + re-invoke the action with the forced choice.
- Stable `decision_id` (keyed on input feature, not index).

**4 — Tag UI.** [src/ui; needs live test]
- Render a tag at each recorded snap (chosen `SnapKind`); click → candidate list
  → pick → `UiEvent::ForceSnap{decision_id, candidate}` → controller replays.

## Subtleties to respect
- **Path-dependence:** forcing a choice can change which OTHER snaps fire → tags
  are a LIVE projection of the latest replay, never a static checklist.
- **Decision identity** must survive replay; degrades if an upstream change moves
  the input — design the key on the input feature.
- Only the elective sites are in scope (mandatory projection / axis-squaring /
  param-pin / drag-constraint are not "could-have-stayed").
