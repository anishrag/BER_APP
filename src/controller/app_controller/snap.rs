//! Elective-snap capture and replay: the clickable snap-tag overlay,
//! forced-choice policies, and re-running the captured stroke under them.
use super::*;

/// A replayable snapping action: the raw stroke (plus enough context to re-run
/// it from scratch) under a forced snap policy.
#[derive(Debug, Clone)]
pub(super) enum SnapAction {
    Freehand {
        tool: EditTools,
        points: Vec<Point2f32>,
        closed: bool,
    },
    /// A new-storey outline traced in world (metre) space over the previous
    /// storey's watermark — replayed through the carve path
    /// (`apply_outline_world_inner`).
    OutlineWorld {
        points: Vec<(f32, f32)>,
        closed: bool,
    },
    /// A sub-area carve/cut stroke in world coords, replayed through the flow's
    /// own UI entry point; `pick` re-applies the user's cut-region choice.
    Subarea {
        flow: SubareaFlow,
        points: Vec<(f32, f32)>,
        pick: Option<(f32, f32)>,
    },
}

/// Which sub-area carve flow a [`SnapAction::Subarea`] replays through — each
/// maps one-to-one onto an `apply_*_stroke` entry point and its
/// `choose_*_cut_region` pick handler.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum SubareaFlow {
    LivingArea,
    PhaseRegion,
    StepRegion,
    /// A ceiling-step AREA stroke (closed loop / multi-side cut) under
    /// ceiling-step mode — routed by stroke shape inside
    /// `apply_ceiling_step_stroke_event` (a straight stroke becomes a step
    /// LINE instead, with its own bespoke tag). Shares the floor step's
    /// pending-cut pick (`choose_step_cut_region`).
    CeilingStep,
    Structure,
    FloorExposure,
    CeilingExposure,
}

/// The last snapping action, kept for replay: the action's input, the document
/// state BEFORE it ran, and the elective snaps it made (each addressable by its
/// `from` point for the forced-choice policy).
pub(super) struct LastSnapAction {
    pub(super) action: SnapAction,
    pub(super) before: HistorySnapshot,
    /// The history-stack length BEFORE the action ran. Replay truncates back to
    /// it, so entries pushed after the action (e.g. dimension answers given
    /// before the tag was clicked) are dropped along with the action's own.
    pub(super) hlen: usize,
    pub(super) decisions: Vec<manip_plan::SnapDecision>,
}

/// One clickable elective-snap tag for the canvas overlay (a projection of a
/// recorded `SnapDecision`). The UI draws a marker at `pos`, labels it with
/// `current_kind`, and on click offers `options`; picking one sends `from` back
/// to [`AppController::choose_snap`].
#[derive(Debug, Clone)]
pub struct SnapTagView {
    /// World position to draw the tag (the snapped point, or the drawn point if
    /// currently un-snapped).
    pub pos: (f32, f32),
    /// The decision's drawn point — its stable identity for `choose_snap`.
    pub from: (f32, f32),
    /// The applied kind, or `None` when this snap is currently disabled.
    pub current_kind: Option<manip_plan::SnapKind>,
    /// Candidates to offer on click: `(kind, apply-index)`, most-aggressive first.
    pub options: Vec<(manip_plan::SnapKind, usize)>,
    /// Whether "don't snap" is a valid choice (false for a mandatory extend/trim
    /// attach — the menu then omits it).
    pub skippable: bool,
}

impl AppController {
    /// Start a new snapping action: drop the previous action's capture (its
    /// tags must not survive into this action's history — a stale replay's
    /// truncate/restore would wipe work the old capture knows nothing about)
    /// and drain stale thread-local decisions so only THIS action's snaps are
    /// tagged. Every capturing entry point calls this before dispatching.
    pub(super) fn begin_snap_capture(&mut self) {
        self.last_snap_action = None;
        let _ = manip_plan::take_snap_decisions();
    }

    /// After a snapping action, capture it for replay: drain the elective snaps it
    /// made (recorded in `manip`), and if any, remember the action + its
    /// pre-state (the undo entry it just pushed). No snaps → nothing to replay.
    pub(super) fn capture_snap_action(&mut self, action: SnapAction) {
        let decisions = manip_plan::take_snap_decisions();
        if decisions.is_empty() {
            return;
        }
        let Some(before) = self.history.last().cloned() else {
            return;
        };
        self.last_snap_action = Some(LastSnapAction {
            action,
            before,
            // history.last() is the action's own undo entry, so the pre-action
            // stack length is one less than now.
            hlen: self.history.len() - 1,
            decisions,
        });
    }

    /// Capture variant for an action that did NOT push an undo entry (a stroke
    /// that only staged a PENDING cut): the caller supplies the pre-action
    /// snapshot it took itself, plus the decisions it drained. No snaps →
    /// nothing to replay.
    pub(super) fn capture_snap_action_pending(
        &mut self,
        action: SnapAction,
        before: HistorySnapshot,
        decisions: Vec<manip_plan::SnapDecision>,
    ) {
        if decisions.is_empty() {
            return;
        }
        self.last_snap_action = Some(LastSnapAction {
            action,
            before,
            // Nothing was pushed — the current stack length IS the pre-action one.
            hlen: self.history.len(),
            decisions,
        });
    }

    /// Drop the capture if it is a `Subarea` of `flow` — for mode exits that
    /// abandon the in-flight shape the tags describe (e.g. leaving ceiling-step
    /// mode mid-carve). Leaves any other action's capture alone.
    pub(super) fn clear_subarea_capture(&mut self, flow: SubareaFlow) {
        if matches!(
            self.last_snap_action.as_ref(),
            Some(LastSnapAction {
                action: SnapAction::Subarea { flow: f, .. },
                ..
            }) if *f == flow
        ) {
            self.last_snap_action = None;
        }
    }

    /// A cut-region pick committed (or was cancelled) for a sub-area flow:
    /// update the captured `Subarea` action so a later replay re-runs stroke +
    /// pick, or drop the capture on cancel (the cut — and with it the snapped
    /// geometry the tags describe — is gone). No-op unless the capture is a
    /// `Subarea` of this `flow`.
    pub(super) fn record_subarea_pick(&mut self, flow: SubareaFlow, pick: Option<(f32, f32)>) {
        let clear = match self.last_snap_action.as_mut() {
            Some(LastSnapAction {
                action:
                    SnapAction::Subarea {
                        flow: f, pick: p, ..
                    },
                ..
            }) if *f == flow => match pick {
                Some(pt) => {
                    // The stroke's decisions still describe the snaps; only the
                    // pick is new.
                    *p = Some(pt);
                    false
                }
                None => true,
            },
            _ => return,
        };
        if clear {
            self.last_snap_action = None;
        }
    }

    /// Re-run a captured snapping action from its raw input (used by replay).
    pub(super) fn dispatch_snap_action(&mut self, action: &SnapAction) {
        let r = match action {
            SnapAction::Freehand {
                tool,
                points,
                closed,
            } => {
                // Restore the tool so the inner apply_* dispatches the same way it did.
                self.sketch_engine.edit_tool = *tool;
                match tool {
                    EditTools::DrawPerpWalls => self.apply_drawperp_stroke(points, *closed),
                    EditTools::AddDetachedSection => self.apply_addsection_stroke(points, *closed),
                    EditTools::ExtendOrtho | EditTools::ExtendCurve => {
                        self.apply_extend_stroke(points)
                    }
                    EditTools::Trim | EditTools::TrimOrtho | EditTools::TrimCurve => {
                        self.apply_trim_stroke(points)
                    }
                    _ => Ok(()),
                }
            }
            SnapAction::OutlineWorld { points, closed } => {
                self.apply_outline_world_inner(points, *closed)
            }
            SnapAction::Subarea { flow, points, pick } => {
                match flow {
                    SubareaFlow::LivingArea => self.apply_living_area_stroke_event(points),
                    SubareaFlow::PhaseRegion => self.apply_phase_region_stroke_event(points),
                    SubareaFlow::StepRegion => self.apply_step_region_stroke_event(points),
                    SubareaFlow::CeilingStep => {
                        // The entry routes by stroke shape AND interprets the
                        // area as a CEILING step only under the mode; leaving
                        // the mode abandons the carve (and clears the capture,
                        // so this is a defensive guard, not a live path).
                        if self.ceiling_step_mode {
                            self.apply_ceiling_step_stroke_event(points);
                        }
                    }
                    SubareaFlow::Structure => self.apply_structure_carve_stroke(points),
                    SubareaFlow::FloorExposure => {
                        // The original run may still be mid-dimensioning; the
                        // restore rewound the document but the in-flight guard
                        // is controller-transient — clear it (and the storey
                        // ref that travels with it) so the re-run isn't
                        // swallowed by the "already carving" early-out.
                        self.floor_exposure_carve_in_flight = false;
                        self.floor_exposure_carve_ref = None;
                        self.apply_floor_exposure_carve_stroke(points);
                    }
                    SubareaFlow::CeilingExposure => {
                        self.ceiling_exposure_carve_in_flight = false;
                        self.ceiling_exposure_carve_ref = None;
                        self.apply_ceiling_exposure_carve_stroke(points);
                    }
                }
                // Re-apply the user's cut-region pick AFTER the stroke re-ran
                // (the pick handler updates the fresh capture the entry point
                // just made, via `record_subarea_pick`).
                if let Some((x, y)) = pick {
                    match flow {
                        SubareaFlow::LivingArea => self.choose_cut_region(*x, *y),
                        SubareaFlow::PhaseRegion => self.choose_phase_cut_region(*x, *y),
                        SubareaFlow::StepRegion | SubareaFlow::CeilingStep => {
                            self.choose_step_cut_region(*x, *y)
                        }
                        SubareaFlow::Structure => self.choose_structure_cut_region(*x, *y),
                        SubareaFlow::FloorExposure => self.choose_floor_exposure_cut_region(*x, *y),
                        SubareaFlow::CeilingExposure => {
                            self.choose_ceiling_exposure_cut_region(*x, *y)
                        }
                    }
                }
                Ok(())
            }
        };
        if let Err(e) = r {
            log::error!("[snap replay] re-run failed: {e}");
        }
    }

    /// Replay the last snapping action under a forced-choice snap policy: rewind to
    /// the pre-action state, re-run from the raw stroke with the overrides, and
    /// rebuild. Re-captures so it can be replayed again.
    pub(super) fn replay_last_snap_action(
        &mut self,
        policy: Vec<(Point2f32, manip_plan::SnapChoice)>,
    ) {
        let Some(last) = self.last_snap_action.take() else {
            return;
        };
        // Drop EVERY entry pushed since the action ran — its own undo entry plus
        // any later ones (dimension answers given before the tag was clicked) —
        // rewind the document, then re-run under the policy.
        self.history.truncate(last.hlen);
        self.restore_history_snapshot(last.before.clone());
        manip_plan::set_snap_policy(policy);
        let _ = manip_plan::take_snap_decisions(); // clear stale
        self.dispatch_snap_action(&last.action);
        manip_plan::clear_snap_policy();
        // Re-capture so the action can be replayed again. Two shapes:
        //  - A `Subarea` re-run captures ITSELF (its entry point drains the
        //    decisions into a fresh `LastSnapAction`, and a pick re-applied by
        //    the dispatch arm updates it via `record_subarea_pick`) — the
        //    thread-local is empty here, and the fresh capture is left as-is.
        //  - The tier-1 actions (`Freehand` / `OutlineWorld`) re-run through
        //    their INNER apply fns, which don't capture — their decisions are
        //    still in the thread-local, so rebuild the capture here with the
        //    original `before`/`hlen` (unchanged by construction: the truncate +
        //    restore above re-established exactly that state).
        let decisions = manip_plan::take_snap_decisions();
        let subarea = matches!(last.action, SnapAction::Subarea { .. });
        if !decisions.is_empty() && !subarea {
            self.last_snap_action = Some(LastSnapAction {
                action: last.action,
                before: last.before,
                hlen: last.hlen,
                decisions,
            });
        }
        // else: either the Subarea entry point captured internally
        // (`last_snap_action` is already the fresh capture — and any decisions
        // a later step recorded must NOT overwrite it with the old action), or
        // the re-run made no elective snaps and there's nothing left to tag.
    }

    /// One clickable snap tag for the canvas overlay: where to draw it, its
    /// current kind (`None` = un-snapped), the alternatives to offer on click, and
    /// the `from` that identifies it back to [`choose_snap`](Self::choose_snap).
    pub fn snap_overlay(&self) -> Vec<SnapTagView> {
        self.last_snap_action.as_ref().map_or(Vec::new(), |a| {
            a.decisions
                .iter()
                .map(|d| {
                    // Tag sits at the snapped point, or the drawn point if skipped.
                    let pos = d.chosen.map_or(d.from, |i| d.candidates[i].to);
                    SnapTagView {
                        pos: (pos.x, pos.y),
                        from: (d.from.x, d.from.y),
                        current_kind: d.chosen.map(|i| d.candidates[i].kind),
                        options: d
                            .candidates
                            .iter()
                            .enumerate()
                            .map(|(i, c)| (c.kind, i))
                            .collect(),
                        skippable: d.skippable,
                    }
                })
                .collect()
        })
    }

    /// Apply a per-snap choice to the snap at `from` and replay: `Some(i)` forces
    /// candidate `i`, `None` un-snaps it. Other snaps of the action keep their
    /// current choice. Drives the tag UI's pick.
    pub fn choose_snap(&mut self, from: Point2f32, choice: Option<usize>) {
        let Some(last) = &self.last_snap_action else {
            return;
        };
        // Build a policy that preserves every other snap's current choice and
        // overrides just this one.
        let policy: Vec<(Point2f32, manip_plan::SnapChoice)> = last
            .decisions
            .iter()
            .map(|d| {
                let c = if (d.from.x - from.x).abs() < 1e-3 && (d.from.y - from.y).abs() < 1e-3 {
                    choice
                } else {
                    d.chosen
                };
                let sc = match c {
                    Some(i) => manip_plan::SnapChoice::Apply(i),
                    None => manip_plan::SnapChoice::Skip,
                };
                (d.from, sc)
            })
            .collect();
        self.replay_last_snap_action(policy);
    }

    /// Replay the last snapping action with EVERY elective snap disabled — revert
    /// to the freehand points. (Test hook for the replay; the tag UI will later
    /// drive `replay_last_snap_action` with per-snap choices.)
    pub fn unsnap_last(&mut self) {
        let Some(last) = &self.last_snap_action else {
            return;
        };
        let policy: Vec<(Point2f32, manip_plan::SnapChoice)> = last
            .decisions
            .iter()
            .map(|d| (d.from, manip_plan::SnapChoice::Skip))
            .collect();
        self.replay_last_snap_action(policy);
    }
}
