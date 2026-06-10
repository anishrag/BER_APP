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
}

/// The last snapping action, kept for replay: the action's input, the document
/// state BEFORE it ran, and the elective snaps it made (each addressable by its
/// `from` point for the forced-choice policy).
pub(super) struct LastSnapAction {
    pub(super) action: SnapAction,
    pub(super) before: HistorySnapshot,
    pub(super) decisions: Vec<manip_2d_plan::SnapDecision>,
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
    pub current_kind: Option<manip_2d_plan::SnapKind>,
    /// Candidates to offer on click: `(kind, apply-index)`, most-aggressive first.
    pub options: Vec<(manip_2d_plan::SnapKind, usize)>,
    /// Whether "don't snap" is a valid choice (false for a mandatory extend/trim
    /// attach — the menu then omits it).
    pub skippable: bool,
}

impl AppController {
    /// After a snapping action, capture it for replay: drain the elective snaps it
    /// made (recorded in `manip`), and if any, remember the action + its
    /// pre-state (the undo entry it just pushed). No snaps → nothing to replay.
    pub(super) fn capture_snap_action(&mut self, action: SnapAction) {
        let decisions = manip_2d_plan::take_snap_decisions();
        if decisions.is_empty() {
            return;
        }
        let Some(before) = self.history.last().cloned() else {
            return;
        };
        self.last_snap_action = Some(LastSnapAction {
            action,
            before,
            decisions,
        });
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
        };
        if let Err(e) = r {
            eprintln!("[snap replay] re-run failed: {e}");
        }
    }

    /// Replay the last snapping action under a forced-choice snap policy: rewind to
    /// the pre-action state, re-run from the raw stroke with the overrides, and
    /// rebuild. Re-captures so it can be replayed again.
    pub(super) fn replay_last_snap_action(
        &mut self,
        policy: Vec<(Point2f32, manip_2d_plan::SnapChoice)>,
    ) {
        let Some(last) = self.last_snap_action.take() else {
            return;
        };
        // Drop the original action's undo entry (the re-run pushes its own), rewind
        // the document, then re-run under the policy.
        self.history.pop();
        self.restore_history_snapshot(last.before);
        manip_2d_plan::set_snap_policy(policy);
        let _ = manip_2d_plan::take_snap_decisions(); // clear stale
        self.dispatch_snap_action(&last.action);
        manip_2d_plan::clear_snap_policy();
        // Re-capture (the direct dispatch bypassed the normal capture point).
        let decisions = manip_2d_plan::take_snap_decisions();
        if let Some(before) = self.history.last().cloned() {
            self.last_snap_action = Some(LastSnapAction {
                action: last.action,
                before,
                decisions,
            });
        }
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
        let policy: Vec<(Point2f32, manip_2d_plan::SnapChoice)> = last
            .decisions
            .iter()
            .map(|d| {
                let c = if (d.from.x - from.x).abs() < 1e-3 && (d.from.y - from.y).abs() < 1e-3 {
                    choice
                } else {
                    d.chosen
                };
                let sc = match c {
                    Some(i) => manip_2d_plan::SnapChoice::Apply(i),
                    None => manip_2d_plan::SnapChoice::Skip,
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
        let policy: Vec<(Point2f32, manip_2d_plan::SnapChoice)> = last
            .decisions
            .iter()
            .map(|d| (d.from, manip_2d_plan::SnapChoice::Skip))
            .collect();
        self.replay_last_snap_action(policy);
    }
}
