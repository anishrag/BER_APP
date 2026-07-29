//! The NEW-STOREY flow: thickness-first prompt, the draw-vs-suggest method
//! choice, browsing/accepting suggested outlines, and seeding the inherited
//! floor levels.
//!
//! State machine (all controller-transient — outside undo, never persisted):
//! `add_storey` opens the flow at [`Thickness`] (the slab thickness is asked
//! FIRST, never on storey 0); entering it advances to [`ChooseMethod`]
//! (draw the outline, or browse suggestions derived from the storey below);
//! [`Browse`] cycles the ranked [`OutlineSuggestion`]s until one is accepted
//! — which adopts it as a FULLY-FINALISED outline (the `from_storey`
//! pattern via [`StoreyDoc::adopt_finalised_outline`] — no carve, no
//! collinearity review, no dimension prompts) and seeds the inherited floor
//! levels. Drawing instead (explicitly, or implicitly by stroking the
//! canvas) leaves the seed PENDING; the seed then fires at the outline's
//! FINALISATION transition ([`maybe_seed_inherited_levels`], called from the
//! dimensioning/carve tails) — restart-safe because `open_ber` re-derives
//! the pending set from which upper-storey outlines are still un-finalised.
//!
//! The thickness itself is durable (`Storey::floor_thickness_m`, OUTSIDE
//! undo); everything else here is session state, cleared on storey switch /
//! open / exit.
//!
//! [`Thickness`]: NewStoreyStage::Thickness
//! [`ChooseMethod`]: NewStoreyStage::ChooseMethod
//! [`Browse`]: NewStoreyStage::Browse
//! [`OutlineSuggestion`]: ber_model::ber::suggest::OutlineSuggestion
//! [`StoreyDoc::adopt_finalised_outline`]: document::StoreyDoc::adopt_finalised_outline
//! [`maybe_seed_inherited_levels`]: AppController::maybe_seed_inherited_levels
use super::*;
use ber_model::ber::suggest::{has_suggestion_inputs, suggest_storey_outlines, OutlineSuggestion};

/// At most this many ranked suggestions are offered to browse.
const MAX_OUTLINE_SUGGESTIONS: usize = 12;

/// Where the in-flight new-storey flow stands.
#[derive(Debug, Clone)]
pub(super) enum NewStoreyStage {
    /// Awaiting the floor-slab thickness (asked FIRST; outline strokes are
    /// ignored until it lands — the dock explains).
    Thickness,
    /// Thickness stamped; awaiting the method choice: draw the outline, or
    /// browse suggestions. A canvas stroke is an implicit "draw".
    ChooseMethod,
    /// Browsing the ranked outline suggestions (never empty). A canvas
    /// stroke is still an implicit "draw" (abandoning the browse).
    Browse {
        suggestions: Vec<OutlineSuggestion>,
        cursor: usize,
    },
}

/// The in-flight new-storey flow. Controller-transient: outside undo, never
/// persisted; cleared on storey switch / load / open / exit.
#[derive(Debug, Clone)]
pub(super) struct PendingNewStorey {
    pub(super) stage: NewStoreyStage,
}

/// One overhanging piece awaiting its level: which
/// `CeilingRegions::inherited_floor_pieces` piece it is, how big, the levels it
/// adjoins (longest shared boundary first) and the one currently chosen.
#[derive(Debug, Clone)]
pub(super) struct PendingOverhang {
    pub(super) index: usize,
    pub(super) area_m2: f64,
    pub(super) bordering: Vec<f64>,
    pub(super) chosen_z: f64,
}

/// The ONE question the floor-level rule raises: for each piece of the new
/// storey's floor with nothing under it, which neighbouring area's level it
/// copies (or a height given outright).
///
/// Everything else follows without asking — a piece with a ceiling under it
/// lands on that ceiling, and the storey's datum is simply the height of its
/// largest floor area. Raised only when a piece adjoins MORE than one level;
/// the seed is held until it is answered, so a floor is never stamped on an
/// assumption. Controller-transient like [`PendingNewStorey`]; fires on BOTH
/// paths into the seed — an accepted suggestion and a hand-drawn outline.
#[derive(Debug, Clone)]
pub(super) struct PendingFloorLevels {
    pub(super) storey_idx: usize,
    pub(super) footprint: Vec<Point2f32>,
    pub(super) overhangs: Vec<PendingOverhang>,
}

impl PendingFloorLevels {
    /// The current answers, positional by `inherited_floor_pieces` index — the
    /// shape [`CeilingRegions::resolve_floor_levels`] consumes.
    ///
    /// [`CeilingRegions::resolve_floor_levels`]:
    /// ber_model::ber::structural::resolve_floor_levels
    fn answers(&self) -> Vec<Option<f64>> {
        let n = self
            .overhangs
            .iter()
            .map(|o| o.index + 1)
            .max()
            .unwrap_or(0);
        let mut answers = vec![None; n];
        for o in &self.overhangs {
            answers[o.index] = Some(o.chosen_z);
        }
        answers
    }
}

/// UI-facing snapshot of the floor-level prompt. `None` when nothing is
/// waiting.
#[derive(Debug, Clone)]
pub struct FloorLevelPromptUiData {
    /// One entry per piece of floor that adjoins more than one level.
    pub overhangs: Vec<OverhangPromptUiData>,
}

/// One overhang's question: how big it is, and the neighbouring levels it
/// could copy — as below-ceiling heights, which is what the assessor measured.
#[derive(Debug, Clone)]
pub struct OverhangPromptUiData {
    pub index: usize,
    pub area_m2: f64,
    /// `(ceiling height m, chosen)`, longest shared boundary first.
    pub options: Vec<(f64, bool)>,
    /// The chosen height when it is NOT one of the offered neighbours — a
    /// height the assessor typed in.
    pub custom: Option<f64>,
}

/// UI-facing snapshot of the new-storey prompt (slice D renders it as the
/// top-priority dock toolbar). `None` when no flow is in progress.
#[derive(Debug, Clone)]
pub struct NewStoreyPromptUiData {
    /// True while the slab thickness is being asked (outline strokes are
    /// ignored; the dock shows the thickness input, prefilled 0.25).
    pub thickness_pending: bool,
    /// True while the draw-vs-suggest choice is being asked.
    pub method_pending: bool,
    /// Whether "Suggest outlines" can succeed — the storey below has its
    /// ceiling height and at least one flat ceiling region. Gates the button
    /// (the method itself also defends with a stable `Err`).
    pub can_suggest: bool,
    /// While browsing: the current suggestion as
    /// `(world-coord ring, label, index, count)` — the canvas tints the ring
    /// over the previous-storey watermark.
    pub suggestion: Option<(Vec<(f64, f64)>, String, usize, usize)>,
}

impl AppController {
    /// The current storey's floor-slab thickness (metres), for display
    /// (the Heights category, storey ≥ 1). `None` on storey 0 (no slab over
    /// another storey), on the All tab, or while unset.
    pub fn floor_thickness(&self) -> Option<f64> {
        let idx = self.current_storey_idx?;
        if idx == 0 {
            return None;
        }
        self.current_record
            .as_ref()?
            .data
            .storeys
            .get(idx)?
            .floor_thickness_m
    }

    /// Answer the new-storey flow's THICKNESS question: validate, stamp the
    /// durable `Storey::floor_thickness_m`, and advance the flow to the
    /// method choice. Also usable when no flow is pending (it just stamps).
    /// OUTSIDE undo — like the ceiling height, the thickness is a property
    /// entry, not a geometry mutation. Errors on storey 0 / the All tab /
    /// no BER / a non-finite or negative value (the prompt stays open).
    pub fn assign_floor_thickness(&mut self, thickness_m: f64) -> Result<(), String> {
        self.stamp_floor_thickness(thickness_m)?;
        if let Some(p) = self.pending_new_storey.as_mut() {
            if matches!(p.stage, NewStoreyStage::Thickness) {
                p.stage = NewStoreyStage::ChooseMethod;
            }
        }
        self.mark_dirty()
    }

    /// Re-edit the floor-slab thickness from the Heights category (storey
    /// ≥ 1). Same validation/stamp as [`assign_floor_thickness`], but never
    /// touches the new-storey flow's stage.
    ///
    /// [`assign_floor_thickness`]: AppController::assign_floor_thickness
    pub fn set_floor_thickness(&mut self, thickness_m: f64) -> Result<(), String> {
        self.stamp_floor_thickness(thickness_m)?;
        self.mark_dirty()
    }

    /// Shared validate + stamp of `Storey::floor_thickness_m` for the
    /// current storey.
    fn stamp_floor_thickness(&mut self, thickness_m: f64) -> Result<(), String> {
        let Some(idx) = self.current_storey_idx else {
            return Err("no storey selected".into());
        };
        if idx == 0 {
            return Err("the ground storey has no floor slab to set".into());
        }
        if !thickness_m.is_finite() || thickness_m < 0.0 {
            return Err("Floor thickness must be a non-negative number of metres.".into());
        }
        let record = self.current_record.as_mut().ok_or("no BER loaded")?;
        record.data.set_floor_thickness(idx, thickness_m);
        Ok(())
    }

    /// The user chose to DRAW the new storey's outline: close the flow and
    /// let the existing watermark/carve path take over. The inherited-level
    /// seed stays PENDING — it fires when the drawn outline finalises.
    pub fn choose_draw_outline(&mut self) {
        self.pending_new_storey = None;
        // They've committed to drawing the outline — drop straight into the
        // wall-drawing tool so a stroke draws it without a separate tool pick
        // (mirrors the draw-first area flows' `set_edit_mode`). Harmless on the
        // implicit-draw path (a stroke arrives already on this tool).
        self.set_edit_mode(EditTools::DrawPerpWalls);
    }

    /// The user chose to browse SUGGESTED outlines: derive the ranked
    /// candidates from the storey below and enter the Browse stage. Stable
    /// `Err` when there's nothing to suggest from (storey 0, no storey
    /// below, or the below storey has no ceiling height / flat ceiling area
    /// — the UI gates the button on [`new_storey_prompt`]'s `can_suggest`,
    /// but the method defends too), and a DISTINCT stable `Err` when the
    /// inputs are there but every candidate was filtered (the flat ceiling
    /// areas below are all under the suggestion engine's sliver floor).
    ///
    /// [`new_storey_prompt`]: AppController::new_storey_prompt
    pub fn choose_suggest_outlines(&mut self) -> Result<(), String> {
        const NO_SUGGESTIONS: &str = "No outline suggestions available — the storey below \
             needs its ceiling height set and a flat ceiling area. Draw the outline instead.";
        const ALL_FILTERED: &str = "No outline suggestions available — the flat ceiling \
             areas of the storey below are too small to build a storey on. Draw the outline \
             instead.";
        let Some(idx) = self.current_storey_idx else {
            return Err("no storey selected".into());
        };
        if idx == 0 {
            return Err(NO_SUGGESTIONS.into());
        }
        let record = self.current_record.as_ref().ok_or(NO_SUGGESTIONS)?;
        let below = record.data.storeys.get(idx - 1).ok_or(NO_SUGGESTIONS)?;
        // The below storey's construction-phase zoning feeds the phase
        // heuristics (the phase registry lives on `Ber`, not the `Storey`);
        // empty (nothing tagged yet) is fine — phases are a bonus input.
        let phase_zones = record.data.storey_phase_zones(idx - 1);
        let suggestions = suggest_storey_outlines(below, &phase_zones, MAX_OUTLINE_SUGGESTIONS);
        if suggestions.is_empty() {
            // Attribute the emptiness honestly: with the inputs present
            // (ceiling height + footprint + a flat ceiling area —
            // `has_suggestion_inputs`), the engine filtered every candidate
            // (too small / degenerate); without them, it never had anything
            // to work from.
            return Err(if has_suggestion_inputs(below) {
                ALL_FILTERED
            } else {
                NO_SUGGESTIONS
            }
            .into());
        }
        log::debug!(
            "[new-storey] {} outline suggestion(s) from storey {} ({} phase zone(s)): {:?}",
            suggestions.len(),
            idx - 1,
            phase_zones.len(),
            suggestions
                .iter()
                .map(|s| (s.source, s.area_m2, s.levels))
                .collect::<Vec<_>>()
        );
        self.pending_new_storey = Some(PendingNewStorey {
            stage: NewStoreyStage::Browse {
                suggestions,
                cursor: 0,
            },
        });
        Ok(())
    }

    /// Move the Browse cursor to the next (`forward`) / previous suggestion,
    /// wrapping. No-op outside the Browse stage.
    pub fn cycle_suggestion(&mut self, forward: bool) {
        if let Some(PendingNewStorey {
            stage:
                NewStoreyStage::Browse {
                    suggestions,
                    cursor,
                },
        }) = self.pending_new_storey.as_mut()
        {
            let n = suggestions.len();
            if n == 0 {
                return;
            }
            *cursor = if forward {
                (*cursor + 1) % n
            } else {
                (*cursor + n - 1) % n
            };
        }
    }

    /// Accept the suggestion under the Browse cursor: undo snapshot →
    /// [`StoreyDoc::adopt_finalised_outline`] (a FULLY-FINALISED outline —
    /// trap 4: NOT the carve path, NO collinearity review, no prompts) →
    /// seed the inherited floor levels → derive + CANONICALIZE the session
    /// step list to the exact derived pieces (the ceiling-step assign
    /// pattern) → close the flow. The undo snapshot precedes the adoption
    /// (trap 6): undo restores the empty outline; the Browse stage is not
    /// restored and the seeded steps remain — harmless, consistent on
    /// redraw.
    ///
    /// [`StoreyDoc::adopt_finalised_outline`]: document::StoreyDoc::adopt_finalised_outline
    pub fn accept_suggestion(&mut self) -> Result<(), String> {
        let Some(idx) = self.current_storey_idx else {
            return Err("no storey selected".into());
        };
        let ring32: Vec<Point2f32> = {
            let Some(PendingNewStorey {
                stage:
                    NewStoreyStage::Browse {
                        suggestions,
                        cursor,
                    },
            }) = self.pending_new_storey.as_ref()
            else {
                return Err("no suggestion is being browsed".into());
            };
            let s = suggestions
                .get(*cursor)
                .ok_or("no suggestion under the cursor")?;
            s.ring.iter().copied().map(to_f32).collect()
        };
        let undo_snap = self.current_history_snapshot();
        self.document.adopt_finalised_outline(&ring32)?;
        self.push_history_snapshot(undo_snap);
        log::debug!(
            "[new-storey] accepted suggested outline ({} verts) for storey {idx} — \
             finalised, no review, no prompts",
            ring32.len()
        );
        // Seed + derive + canonicalize (consumes the pending seed).
        self.seed_inherited_levels_now(idx, &ring32);
        self.pending_new_storey = None;
        // Nothing to review or dimension on a finalised adoption; recompute
        // so no stale prompt survives the buffer swap.
        self.review = None;
        self.compute_next_dimension_prompt();
        self.mark_dirty()
    }

    /// UI snapshot of the new-storey prompt — see [`NewStoreyPromptUiData`].
    /// `None` when no flow is in progress.
    pub fn new_storey_prompt(&self) -> Option<NewStoreyPromptUiData> {
        let p = self.pending_new_storey.as_ref()?;
        let (thickness_pending, method_pending, suggestion) = match &p.stage {
            NewStoreyStage::Thickness => (true, false, None),
            NewStoreyStage::ChooseMethod => (false, true, None),
            NewStoreyStage::Browse {
                suggestions,
                cursor,
            } => {
                let s = suggestions.get(*cursor)?;
                let ring: Vec<(f64, f64)> = s.ring.iter().map(|q| (q.x, q.y)).collect();
                let levels = if s.levels > 1 {
                    format!(", {} floor levels", s.levels)
                } else {
                    String::new()
                };
                let label = format!("{} — {:.1} m²{}", s.source, s.area_m2, levels);
                (
                    false,
                    false,
                    Some((ring, label, *cursor, suggestions.len())),
                )
            }
        };
        Some(NewStoreyPromptUiData {
            thickness_pending,
            method_pending,
            can_suggest: self.can_suggest_outlines(),
            suggestion,
        })
    }

    /// Whether [`choose_suggest_outlines`] has anything to work from: a
    /// storey below whose inputs satisfy the suggestion engine's cheap
    /// empty-guard mirror ([`has_suggestion_inputs`]) — the engine can still
    /// come up empty on validation, which the method's stable `Err` covers.
    ///
    /// [`choose_suggest_outlines`]: AppController::choose_suggest_outlines
    fn can_suggest_outlines(&self) -> bool {
        let Some(idx) = self.current_storey_idx else {
            return false;
        };
        if idx == 0 {
            return false;
        }
        self.current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx - 1))
            .is_some_and(has_suggestion_inputs)
    }

    /// Outline-stroke gating for the new-storey flow. Called by the outline
    /// stroke entry points BEFORE applying a `DrawPerpWalls` stroke:
    /// - [`Thickness`] pending → `true`: IGNORE the stroke (the dock
    ///   explains that the slab thickness comes first).
    /// - [`ChooseMethod`] / [`Browse`] → drawing IS the choice: implicit
    ///   [`choose_draw_outline`], then `false` (apply the stroke normally).
    /// - no flow → `false`.
    ///
    /// [`Thickness`]: NewStoreyStage::Thickness
    /// [`ChooseMethod`]: NewStoreyStage::ChooseMethod
    /// [`Browse`]: NewStoreyStage::Browse
    /// [`choose_draw_outline`]: AppController::choose_draw_outline
    pub(super) fn new_storey_blocks_outline_stroke(&mut self) -> bool {
        match self.pending_new_storey.as_ref().map(|p| &p.stage) {
            Some(NewStoreyStage::Thickness) => {
                log::debug!(
                    "[new-storey] outline stroke ignored — the floor-slab thickness \
                     is asked first (the dock explains)"
                );
                true
            }
            Some(_) => {
                log::debug!("[new-storey] stroke = implicit choose_draw_outline");
                self.choose_draw_outline();
                false
            }
            None => false,
        }
    }

    /// DRAWN-path seeding, called from the tails that can FINALISE an
    /// outline (dimension apply/skip, the world-outline carve): when the
    /// current upper storey's seed is pending and its outline just became
    /// finalised, seed the inherited floor levels from the storey below
    /// (trap 3: the seed fires exactly once, at the finalisation
    /// transition; `open_ber` re-derives the pending set, so the transition
    /// survives a restart; finalised/old records never retro-seed).
    /// Give a storey whose level questions were never answered its chance to
    /// ask again — on open, and on every storey switch.
    ///
    /// [`maybe_seed_inherited_levels`] alone fires only from the dimensioning /
    /// carve tails, i.e. at an outline's FINALISATION transition, and reads the
    /// footprint out of the EDITING BUFFER. Neither serves a storey that is
    /// already built: it has no transition left to fire, and its buffer can be
    /// EMPTY — a document snapshot older than the storey restores over the
    /// rebuild-from-record, leaving `scale_set` false and no outline points
    /// even though the record holds the real geometry. Such a storey would sit
    /// flat over a stepped ceiling forever, its floor floating above the lower
    /// part (field: 56 Elmwood Road).
    ///
    /// So this path takes the footprint from the RESOLVED RECORD, and gates on
    /// the same condition [`rederive_inherited_seed_pending`] uses to pend it:
    /// never seeded (`floor_datum_z_m` is `None`) AND a piece of its floor
    /// genuinely has a choice to make. That gate is what keeps it off a storey
    /// merely mid-draw, whose record floors are still stale.
    ///
    /// [`maybe_seed_inherited_levels`]: AppController::maybe_seed_inherited_levels
    /// [`rederive_inherited_seed_pending`]:
    /// AppController::rederive_inherited_seed_pending
    pub(super) fn maybe_ask_unanswered_levels(&mut self) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        if idx == 0 || !self.inherited_seed_pending.contains(&idx) {
            return;
        }
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let (Some(storey), Some(below)) = (
            record.data.storeys.get(idx),
            record.data.storeys.get(idx - 1),
        ) else {
            return;
        };
        if storey.floor_datum_z_m.is_some() {
            return; // already settled
        }
        let Some(fp) = storey.floors.footprint_ring() else {
            return; // nothing resolved yet — the drawn path still owns this
        };
        if !below
            .ceilings
            .inherited_floor_pieces(&fp)
            .iter()
            .any(|p| p.needs_choice())
        {
            return; // no question to re-ask
        }
        let fp32: Vec<Point2f32> = fp.iter().copied().map(domain::points::to_f32).collect();
        self.seed_inherited_levels_now(idx, &fp32);
    }

    pub(super) fn maybe_seed_inherited_levels(&mut self) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        if !self.inherited_seed_pending.contains(&idx) {
            return;
        }
        // The live buffer may hold the just-applied dimension — sync so the
        // outline slot (what `outline_finalised` reads) is current.
        self.document.sync_active();
        if !self.outline_finalised() {
            return;
        }
        let footprint = self.document.outline.floor_plan.get_points();
        if footprint.len() < 3 {
            return; // nothing drawn yet — not the finalisation transition
        }
        self.seed_inherited_levels_now(idx, &footprint);
    }

    /// Seed storey `idx`'s session step regions with the inherited floor
    /// levels from the storey below `footprint`, then derive and CANONICALIZE
    /// the session list to the exact derived pieces (the ceiling-step assign
    /// pattern — in-session equals post-reload by construction). CONSUMES the
    /// pending seed whether or not anything was inherited (the finalisation
    /// transition happened either way). The below storey's ceiling
    /// classification needs no seeding here: the commit's `derive_inter_storey`
    /// tail splits and stamps the covered piece `InterStorey` on every
    /// re-resolve (derived-only — nothing is pushed onto `ceiling_splits`).
    fn seed_inherited_levels_now(&mut self, idx: usize, footprint: &[Point2f32]) {
        if idx == 0 {
            self.inherited_seed_pending.remove(&idx);
            return;
        }
        // A prompt already up for this storey owns the seed — re-raising would
        // discard answers the assessor has already given.
        if self
            .pending_floor_levels
            .as_ref()
            .is_some_and(|p| p.storey_idx == idx)
        {
            return;
        }
        let Some(below) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx - 1))
        else {
            return;
        };
        // Rules 1, 2 and 4 need nothing from the assessor, and rule 3 has a
        // sensible default (copy the neighbour it mostly runs along). So STAMP
        // THE DEFAULTS NOW and raise the question as a REVISION — holding the
        // whole seed on an answer left the floor flat over a stepped ceiling,
        // warning about a mismatch the pending question was there to resolve.
        let asks: Vec<PendingOverhang> = document::overhang_choices(below, footprint)
            .into_iter()
            .map(|(index, area_m2, bordering)| PendingOverhang {
                index,
                area_m2,
                chosen_z: bordering[0],
                bordering,
            })
            .collect();
        self.stamp_inherited_levels(idx, footprint, &[]);
        if !asks.is_empty() {
            self.pending_floor_levels = Some(PendingFloorLevels {
                storey_idx: idx,
                footprint: footprint.to_vec(),
                overhangs: asks,
            });
            log::debug!("[new-storey] storey {idx} seeded; offering the overhang choice");
        }
    }

    /// Answer one overhang's question — which level that piece takes. `index`
    /// is the overhang's [`OverhangPromptUiData::index`]. Re-stamps the storey's
    /// levels immediately so the plan shows the answer as it is picked; the
    /// prompt stays open, so the choice can be toggled and compared. No-op when
    /// nothing is pending or the index is unknown.
    pub fn set_pending_overhang_level(&mut self, index: usize, level_z: f64) {
        let Some(p) = self.pending_floor_levels.as_mut() else {
            return;
        };
        let Some(o) = p.overhangs.iter_mut().find(|o| o.index == index) else {
            return;
        };
        o.chosen_z = level_z;
        let (idx, footprint) = (p.storey_idx, p.footprint.clone());
        let answers = p.answers();
        self.stamp_inherited_levels(idx, &footprint, &answers);
    }

    /// Dismiss the overhang question. The levels are already stamped — the
    /// defaults at seed time, then re-stamped on every pick — so this only
    /// closes the prompt. No-op when nothing is pending.
    pub fn confirm_pending_floor_levels(&mut self) {
        let Some(p) = self.pending_floor_levels.take() else {
            return;
        };
        let answers = p.answers();
        self.stamp_inherited_levels(p.storey_idx, &p.footprint, &answers);
    }

    /// The floor-level prompt for the dock, or `None` when nothing is waiting.
    pub fn floor_level_prompt(&self) -> Option<FloorLevelPromptUiData> {
        let p = self.pending_floor_levels.as_ref()?;
        let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
        Some(FloorLevelPromptUiData {
            overhangs: p
                .overhangs
                .iter()
                .map(|o| OverhangPromptUiData {
                    index: o.index,
                    area_m2: o.area_m2,
                    options: o
                        .bordering
                        .iter()
                        .map(|z| (*z, near(*z, o.chosen_z)))
                        .collect(),
                    custom: (!o.bordering.iter().any(|z| near(*z, o.chosen_z)))
                        .then_some(o.chosen_z),
                })
                .collect(),
        })
    }

    /// The seed proper, once the choices are settled (or there were none).
    fn stamp_inherited_levels(
        &mut self,
        idx: usize,
        footprint: &[Point2f32],
        answers: &[Option<f64>],
    ) {
        self.inherited_seed_pending.remove(&idx);
        // REPLACE, never append: answering the overhang question re-stamps a
        // storey that was already seeded with the defaults.
        self.step_regions.remove(&idx);
        let Some(below) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx - 1))
        else {
            return;
        };
        let (datum, seeded) = document::inherited_step_regions_with(below, footprint, answers);
        if let Some(storey) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx))
        {
            storey.floor_datum_z_m = datum;
        }
        // Same transition seeds the inherited construction-PHASE zones from the
        // storey below: a two-storey extension carries its construction date up
        // without re-drawing. The resolve partition clips them to this outline
        // and the post-commit reconstruction canonicalizes — a zone the new
        // outline doesn't reach drops out. (Re-borrow the ber here; `below`'s
        // borrow has ended.)
        let phases = self
            .current_record
            .as_ref()
            .map(|r| document::inherited_phase_regions_over(&r.data, idx - 1, footprint))
            .unwrap_or_default();
        log::debug!(
            "[new-storey] seeding {} inherited floor level(s) and {} phase zone(s) onto storey {idx}",
            seeded.len(),
            phases.len()
        );
        if !seeded.is_empty() {
            self.step_regions.entry(idx).or_default().extend(seeded);
        }
        if !phases.is_empty() {
            self.phase_regions.entry(idx).or_default().extend(phases);
        }
        // Derive now (with_levels splits the floors, the phase partition splits
        // them by construction), then canonicalize BOTH session lists from the
        // derived regions — trap 1: the session set is the exact ber_model
        // pieces, so reload is a fixpoint.
        if let Err(e) = self.commit_to_ber() {
            log::error!("commit_to_ber after inherited-level seed: {e}");
        }
        self.reconstruct_step_regions_for(idx);
        self.reconstruct_phase_regions_for(idx);
    }

    /// A WHOLESALE outline replacement on an UPPER storey (the redraw
    /// Replace path, or a world-carve over the storey) invalidates
    /// everything seeded against the old footprint: re-PEND the
    /// inherited-level seed and DROP the storey's session step regions —
    /// the seeded pieces were canonicalized against the OLD footprint and
    /// die with it, so the finalisation tail re-seeds against the new
    /// footprint exactly like a restart's
    /// [`rederive_inherited_seed_pending`] would. NOTE: dropping the entry
    /// also drops the user's OWN steps on the storey — the honest semantic
    /// for a wholesale redraw (they too were drawn against the old
    /// footprint). No-op on storey 0 / the All tab, and harmless on a fresh
    /// (never-seeded) upper storey.
    ///
    /// Also RETRACTS the LEGACY party-carve rings a pre-InterStorey record's
    /// finalisation pushed onto the storey BELOW's `ceiling_splits` under the
    /// old footprint ([`document::retract_legacy_party_rings`]), recomposing
    /// the below ceilings so the stale split merges back. Classification
    /// retraction is automatic — `derive_inter_storey`'s clear pass un-stamps
    /// `InterStorey` wherever the new footprint no longer covers. The callers
    /// run BEFORE the redraw commits, so `current_record` still holds the
    /// PRE-redraw resolve — its footprint is exactly the one the old carve
    /// used; a fresh (never-resolved) storey has none and skips cleanly.
    ///
    /// [`rederive_inherited_seed_pending`]:
    /// AppController::rederive_inherited_seed_pending
    pub(super) fn reset_inherited_seed_for_redraw(&mut self) {
        let Some(idx) = self.current_storey_idx else {
            return;
        };
        if idx == 0 {
            return;
        }
        self.inherited_seed_pending.insert(idx);
        self.step_regions.remove(&idx);
        // The inherited phase zones were canonicalized against the OLD footprint
        // too — drop them so the finalisation tail re-seeds against the new one.
        self.phase_regions.remove(&idx);
        // Retract the OLD footprint's legacy party-carve rings from the storey
        // below (see the doc comment above).
        let old_fp = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|s| s.floors.footprint_ring());
        let Some(old_fp) = old_fp else {
            return; // fresh storey — nothing was ever carved against it
        };
        let Some(below_ber) = self
            .current_record
            .as_mut()
            .and_then(|r| r.data.storeys.get_mut(idx - 1))
        else {
            return;
        };
        if document::retract_legacy_party_rings(below_ber, &old_fp) {
            self.recompose_ceilings(idx - 1);
        }
    }

    /// Re-derive which storeys still await their inherited-level seed —
    /// `{idx > 0 with an un-finalised outline}` — making the drawn-path
    /// seed RESTART-SAFE: called on open/restore after the editing model is
    /// rebuilt. Finalised (and old, pre-flow) records are never in the set,
    /// so they never retro-seed.
    pub(super) fn rederive_inherited_seed_pending(&mut self) {
        self.inherited_seed_pending = self
            .document
            .storeys
            .iter()
            .enumerate()
            .skip(1)
            .filter(|(_, s)| !(s.outline.dimensioning.scale_set && s.outline.finalised()))
            .map(|(i, _)| i)
            .collect();
        // A FINALISED storey can still owe its answer: the seed holds on an
        // overhang that adjoins more than one level, and a dismissed prompt (or
        // a reload while it was up) would otherwise strand the storey flat over
        // a stepped ceiling — its floor floating above the lower part forever,
        // since the finalised rule above never re-pends it. Never seeded at all
        // is exactly `floor_datum_z_m == None`; once seeded it is `Some` and
        // this stops firing.
        let unanswered: Vec<usize> = self
            .current_record
            .as_ref()
            .map(|r| {
                r.data
                    .storeys
                    .iter()
                    .enumerate()
                    .skip(1)
                    .filter(|(i, s)| {
                        s.floor_datum_z_m.is_none()
                            && s.floors.footprint_ring().is_some_and(|fp| {
                                r.data.storeys[i - 1]
                                    .ceilings
                                    .inherited_floor_pieces(&fp)
                                    .iter()
                                    .any(|p| p.needs_choice())
                            })
                    })
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
        self.inherited_seed_pending.extend(unanswered);
        if !self.inherited_seed_pending.is_empty() {
            log::debug!(
                "[new-storey] inherited-level seed pending (re-derived) for storeys {:?}",
                self.inherited_seed_pending
            );
        }
    }
}
