# UI restructure — ballpark design (2026-06-11)

Status: BALLPARK — agreed in conversation, to be strengthened before implementation.
Decisions from Anish: BER record created IMMEDIATELY on "New BER" (Key Details edits the live
record, no Create button); Load BER = inline list panel on Home; Exposure lives in the Area
category; Sketch layout is RESPONSIVE — portrait → ribbon (top), landscape → side panel (left).

## 1. App-level navigation

States: Splash → Home ⇄ BER Workspace.

**Home (main menu)**
- "New BER" → controller.create_ber("Untitled BER", today) immediately → workspace at Key Details.
- "Load BER" → expands an INLINE list panel: one row per saved BER (name, date assessed, 🗑 delete
  with confirm) → tap a row → open_ber → workspace at Key Details.
  Wires up the currently-dead BerRepository::list/delete + BerSummary.
- Room below for future options (settings, export queue, …).

**BER Workspace** — persistent chrome around three sections:
- Top bar: BER name (read-only here; edited in Key Details) • section tabs
  [ Key Details | Sketch | More Details ] • "Exit BER" right-aligned.
- Tabs always clickable — no gating between sections (Continue is a convenience, not a lock).
- Continue button at the bottom of each section: Key Details → Sketch → More Details
  (More Details' button reserved for Finish/Export later).
- Exit BER (every section): flush_if_dirty → Home. No confirm dialog needed (autosave means
  nothing is lost); revisit if in-flight prompts should warn.

**Key Details** = the current New-BER form (name, date assessed, construction year, extensions),
now editing the LIVE record: field edits write through to the controller (needs new setters:
rename + set_date_assessed; set_construction_phases exists) and autosave like sketch edits.
Same page serves new and loaded BERs identically.

**More Details** = stub (placeholder + chrome). Future DEAP-adjacent forms (systems, ventilation,
hot water, lighting).

## 2. Sketch section — responsive category layout

Categories are defined ONCE as data (name + ordered tool list); two renderers:
- **Portrait** (h > w): RIBBON — category tab row on top, the active category's tools in a second
  row beneath, canvas below, prompt dock at the bottom.
- **Landscape** (w ≥ h): SIDE PANEL — collapsible labelled groups on the left (all categories
  visible at once), canvas right, prompt dock at the bottom.
Orientation = aspect ratio of the available rect, re-checked per frame (free with egui).

**Canvas-level navigation (OUTSIDE the categories):**
- Storey tabs ABOVE the canvas: [ Storey 1 | Storey 2 | + | All ] (existing behaviour).
- 2D/3D = a FLOATING button on the canvas that TOGGLES on click (plain text "2D"/"3D" for now;
  visual design later). **Disabled until the scope has its ceiling height(s) set** (hover hint:
  "Set a ceiling height (Heights) first"). This REPLACES the click-3D-opens-prompt flow: heights
  are entered via the Heights category whenever the user wants; 3D is just gated on "some notion
  of the 3D shape" (today: ceiling height set; may later include walls).
- Display mode (FloorMapDims / Roofs / Walls / Floors / AllSurfaces) = a second floating button
  next to it that CYCLES through the modes on click, label = current mode (plain text for now).

**Prompt dock** (bottom, all orientations): every contextual flow panel renders here regardless
of active category — dimension prompts, position prompts (area + line), cut picks, H/V picker,
step amounts, exposure type buttons, collinearity review, redraw decision. One consistent home.

## 3. Categories — full inventory

| Category | Tools |
| --- | --- |
| **Edit** | Draw walls (DrawPerpWalls) · Add section (AddDetachedSection) · **Move** (RENAME of the current "Edit" tool) · Extend Ortho · Extend Curve · Trim · Trim Ortho · Trim Curve |
| **Area** | Living area · Construction year (phase regions) · Exposure |
| **Heights** | Default ceiling height (always-available field + Set; shows the current value per storey) · Floor steps · Ceiling steps (still requires the default height — inline hint, no longer tied to 3D) |
| **Drawing** | Undo · Redo · Fit view · snap-tag visibility / Un-snap last |
| **Dimensions** | Wall-length label visibility toggle · resume/skip the current dimension prompt · collinearity review entry |
| **State** | Save checkpoint · Load checkpoint (dev) · autosave status ("saved 12s ago") |

Unmentioned items placed by judgement (flag if wrong): storey management → above-canvas
tabs; the dev BER_CHECKPOINT env path stays. (2D/3D + display mode = floating canvas buttons
per Anish, 2026-06-11.)

## 4. Implementation sketch (for the strengthening pass)

- Page model: `Page::{Splash, Home, Workspace(Section)}` with `Section::{KeyDetails, Sketch,
  MoreDetails}`; one `workspace_chrome()` wrapper renders tabs/exit/continue around the section.
- Controller: `rename_ber`, `set_date_assessed`, `list_bers` (wire BerSummary), `delete_ber`,
  `exit_ber` (= flush_if_dirty + drop current_record? or keep loaded — decide: drop, Home is
  record-free). Replace the ceiling_height_prompt gate flow with plain 3D-button disablement
  (keep `ceiling_heights_set`; the Heights category owns entry).
- `EditTools::Edit` variant rename → `Move` (check serde exposure — sketch engine state isn't
  persisted, so likely code-only).
- This restructure is the natural moment to SPLIT sketch.rs (~2.6k lines): categories.rs
  (the category data + both renderers), prompt_dock.rs, canvas.rs, events stay.
- Slices when implementing: (1) navigation shell + Home + workspace chrome + Key Details live
  editing; (2) sketch category layout (both orientations) + tool rename; (3) heights/3D
  decoupling; review after each.

## Deferred decisions

- More Details content; Finish/Export flow; Home future options.
- Whether Exit BER warns on in-flight prompts (currently: no).
- Whether "notion of 3D shape" should eventually require more than ceiling heights.
- BER name editing location (Key Details only, or tap-to-rename in the top bar).
