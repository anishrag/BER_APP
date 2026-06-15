//! Roof-RIDGE line input: in roof-point mode a STROKE (instead of a tap)
//! draws an axis-aligned ridge line. The stroke is APPROXIMATE — like every
//! other stroke — so each of its three degrees of freedom gets its own
//! question, prefilled with the drawn value and individually skippable, in
//! the assessor's measuring order (dimension first, then where it sits):
//!
//! 1. LENGTH — the ridge's own dimension (no reference wall): a typed
//!    amount validated optimistically against the RESOLVED ceiling regions
//!    ([`document::validate_roof_ridge_length`] — fits SOME area's bbox;
//!    the binding placement check is finalize's, below).
//! 2. CROSS position — how far the line is from a PARALLEL outline wall
//!    ([`StoreyDoc::snap_ceiling_step_line`] /
//!    [`StoreyDoc::ceiling_line_position_prompt`], reused).
//! 3. START position — how far the ridge's nearer end (the extent's `lo`)
//!    is from a PERPENDICULAR outline wall (the same machinery on the
//!    ALONG axis, `axis.other()`).
//!
//! The FINALIZE placement check ([`document::validate_roof_ridge_extent`])
//! runs at the moment the geometry becomes fully determined — the last open
//! stage — so its rejection always attaches to a value the user just typed.
//! Then one typed height places BOTH apexes at once (`Ber::add_roof_apex`
//! ×2) — a level ridge. A sloping ridge is entered via the two-point tap
//! flow instead.
//!
//! Like the roof point, the ridge's durable record is `Storey::roof_apexes`
//! (a ridge is just two apexes grouped with the regions they shape —
//! `CeilingRegions::apex_groups` is the grouping authority); the shell owns
//! the in-flight stroke → length → cross → start → height state machine and
//! the canonicalize step after assign. Outside undo —
//! phases/steps/ceiling-steps/roof-points parity.
use super::ceiling_steps::CeilingLineSnap;
use super::*;
use datatypes::ber::structural::RoofApex;
use document::LinePositionPrompt;

/// Which of the pending ridge's two POSITION questions still need asking, in
/// flow order — the `remaining` argument of
/// [`AppController::advance_roof_ridge`], consumed AFTER the Length stage
/// (the roof point's `remaining` axes, ridge-flavoured: the two questions
/// live on different axes AND measure different things). The length question
/// is not listed: it has no auto-resolve and is always asked first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RidgeQuestion {
    /// The line's cross coordinate (`coord_m`, on `axis`).
    Cross,
    /// The extent's start (`lo`, on the ALONG axis `axis.other()`).
    Start,
}

/// The in-flight roof-ridge input, advanced stage by stage: stroke →
/// ([`Length`] unless BOTH extent ends snapped →) ([`CrossPosition`] when
/// un-snapped →) ([`StartPosition`] when NEITHER end snapped →) finalize →
/// [`Height`] → assign. Session-only, outside undo; cleared on storey
/// switch, mode exit, and cancel.
///
/// Every stage carries the accumulating `(axis, coord_m, lo, len_m)` — the
/// ridge's endpoints are `(coord, lo)`/`(coord, lo + len)` per the axis
/// convention of [`AppController::ridge_endpoints`] — plus the per-stage
/// snaps. A snapped degree of freedom skips its question (the coordinate is
/// a reference wall's, exact by construction): the CROSS snap skips the
/// cross question, the START (lo) snap the start question, and the END (hi)
/// snap fixes the extent's far end — with the start ALSO snapped the LENGTH
/// is exact by construction (its question is skipped); with the start
/// un-snapped the Length question still opens (prefilled `hi − lo`) and a
/// typed length RE-ANCHORS at the snapped hi (`lo = hi − len`), which fixes
/// the position DOF and skips the Start question too. Each snap rides along
/// so the tag stays clickable, ONE tag at a time — the stage owns which
/// (see [`AppController::roof_ridge_snap_tag`]).
///
/// [`CrossPosition`]: PendingRoofRidge::CrossPosition
/// [`StartPosition`]: PendingRoofRidge::StartPosition
/// [`Length`]: PendingRoofRidge::Length
/// [`Height`]: PendingRoofRidge::Height
#[derive(Debug, Clone)]
pub(super) enum PendingRoofRidge {
    /// Question 1: the ridge LENGTH (metres along the ridge axis — the
    /// extent becomes `[lo, lo + len]`). No reference wall: it's the
    /// ridge's own dimension. Prefilled with the drawn (possibly
    /// end-snapped) length, skippable; a typed length is bounded
    /// optimistically ([`document::validate_roof_ridge_length`]) and
    /// re-anchors at a snapped hi. Skipped iff BOTH extent ends snapped
    /// (the length is exact by construction). All three snaps ride through.
    Length {
        axis: ParameterAxis,
        coord_m: f32,
        lo: f32,
        len_m: f32,
        cross_snap: Option<CeilingLineSnap>,
        start_snap: Option<CeilingLineSnap>,
        end_snap: Option<CeilingLineSnap>,
    },
    /// Question 2: the line's cross coordinate is being measured from a
    /// PARALLEL outline reference wall — the ceiling-step line prompt
    /// reused. Shell-resolved (`StoreyDoc::resolve_ceiling_line_position`);
    /// never enters the document's position-prompt loop. Implies the cross
    /// is UN-snapped: a snapped line skips this stage entirely. The START
    /// and END snaps (whose questions come later / already passed) ride
    /// through.
    CrossPosition {
        axis: ParameterAxis,
        coord_m: f32,
        lo: f32,
        len_m: f32,
        start_snap: Option<CeilingLineSnap>,
        end_snap: Option<CeilingLineSnap>,
        prompt: LinePositionPrompt,
    },
    /// Question 3: the extent's START (`lo`, the ridge's nearer end on the
    /// ALONG axis `axis.other()`) is being measured from a PERPENDICULAR
    /// outline reference wall — the same prompt machinery on the other
    /// axis. Implies NEITHER end snapped (either snap fixes the position
    /// DOF and skips this stage — `end_snap` is structurally absent here).
    /// The LAST location stage: its answer fully determines the geometry,
    /// so the finalize check runs on the way out of it.
    StartPosition {
        axis: ParameterAxis,
        coord_m: f32,
        lo: f32,
        len_m: f32,
        cross_snap: Option<CeilingLineSnap>,
        prompt: LinePositionPrompt,
    },
    /// All three drawn values resolved and the placement finalized; awaiting
    /// the ridge height (metres, ABSOLUTE above the storey floor datum —
    /// both endpoints get it).
    Height {
        axis: ParameterAxis,
        coord_m: f32,
        lo: f32,
        len_m: f32,
        cross_snap: Option<CeilingLineSnap>,
        start_snap: Option<CeilingLineSnap>,
        end_snap: Option<CeilingLineSnap>,
    },
}

impl AppController {
    /// UI entry point: a ridge stroke (world/metre coords) drawn in
    /// roof-point mode. Fits it to an axis-aligned segment
    /// ([`StoreyDoc::fit_roof_ridge_line`] — axis + cross coordinate + the
    /// stroke's own extent), then tries the elective wall snap on all THREE
    /// drawn degrees of freedom — the CROSS coordinate against parallel
    /// walls ([`StoreyDoc::snap_ceiling_step_line`], bbox extremes
    /// excluded: a ridge along a wall makes no roof) and BOTH extent ends
    /// (`lo`/`hi`) against perpendicular ones
    /// ([`StoreyDoc::snap_roof_ridge_end`], bbox extremes INCLUDED: an end
    /// on the boundary is a gable). When `lo` and `hi` would snap to the
    /// SAME wall only the nearer end keeps its snap. Then the LENGTH
    /// question opens (question 1, prefilled with the — possibly
    /// end-snapped — drawn length) UNLESS both ends snapped, in which case
    /// the length is exact by construction and the flow advances straight
    /// to the position questions; a finalize rejection on that all-snapped
    /// path rejects the stroke itself (logged), like a fit failure. A
    /// stroke that isn't line-like or lies outside the footprint (+ the
    /// end-snap tolerance) is rejected (logged).
    pub fn apply_roof_ridge_stroke_event(&mut self, world_points: &[(f32, f32)]) {
        let _ = self.sketch_engine.take_stroke_raw();
        if !self.roof_point_mode
            || self.roof_point_pending()
            || self.roof_ridge_pending()
            || self.roof_rect_pending()
        {
            return;
        }
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        if pts.len() < 2 {
            return;
        }
        // Flush the live buffer to its slot: the fit + snap read
        // `document.outline`, which is only synced on shape switches.
        self.sync_active();
        // A CLOSED stroke is the RECT-ridge flow's flat top, not a ridge
        // line — classified by THE closure rule the living-area/phase carve
        // uses ([`manip_2d_plan::stroke_is_closed`], single-sourced): ends
        // within `close_gap_m` AND enclosing real area — a SHORT ridge
        // stroke whose ends merely fall within the gap stays a line.
        if manip_2d_plan::stroke_is_closed(&pts, self.tuning.subarea.close_gap_m) {
            self.apply_roof_rect_stroke(&pts);
            return;
        }
        // Paste-ready stroke dump (matches the ceiling-step / area dumps).
        dump_stroke("roof-ridge", &pts);
        let (axis, coord_m, (lo, hi)) = match self.document.fit_roof_ridge_line(&pts) {
            Ok(fit) => fit,
            Err(e) => {
                log::error!("roof ridge: {}", e);
                return;
            }
        };
        let (coord_m, cross_snap) = match self.document.snap_ceiling_step_line(axis, coord_m) {
            Some((snapped, outline_wall_vid)) => (
                snapped,
                Some(CeilingLineSnap {
                    drawn_coord_m: coord_m,
                    outline_wall_vid,
                }),
            ),
            None => (coord_m, None),
        };
        // Both extent ends try the end snap (extremes included). Same-wall
        // guard: a short stroke near one wall can put both ends in
        // tolerance of it — only the nearer end keeps its snap.
        let mut lo_hit = self.document.snap_roof_ridge_end(axis.other(), lo);
        let mut hi_hit = self.document.snap_roof_ridge_end(axis.other(), hi);
        if let (Some((lc, lv)), Some((hc, hv))) = (lo_hit, hi_hit) {
            if lv == hv {
                if (lc - lo).abs() <= (hc - hi).abs() {
                    hi_hit = None;
                } else {
                    lo_hit = None;
                }
            }
        }
        let (lo_snapped, start_snap) = match lo_hit {
            Some((snapped, outline_wall_vid)) => (
                snapped,
                Some(CeilingLineSnap {
                    drawn_coord_m: lo,
                    outline_wall_vid,
                }),
            ),
            None => (lo, None),
        };
        let (hi_snapped, end_snap) = match hi_hit {
            Some((snapped, outline_wall_vid)) => (
                snapped,
                Some(CeilingLineSnap {
                    drawn_coord_m: hi,
                    outline_wall_vid,
                }),
            ),
            None => (hi, None),
        };
        // The extent under the snaps: a lone START snap SLIDES the drawn
        // length (it anchors the position, it doesn't stretch); a lone END
        // snap fixes the far end, so the segment runs drawn-lo → snapped-hi
        // (a typed length later re-anchors at hi); BOTH snapped → the
        // length is wall-to-wall exact by construction.
        let (lo, len_m) = match (&start_snap, &end_snap) {
            (Some(_), Some(_)) => (lo_snapped, hi_snapped - lo_snapped),
            (Some(_), None) => (lo_snapped, hi - lo),
            (None, _) => (lo, hi_snapped - lo),
        };
        if start_snap.is_some() && end_snap.is_some() {
            // Both ends snapped: the Length question is skipped — advance
            // straight to the position questions. A finalize rejection here
            // has no open stage to land on: it rejects the STROKE (logged).
            if let Err(e) = self.advance_roof_ridge(
                axis,
                coord_m,
                lo,
                len_m,
                cross_snap,
                start_snap,
                end_snap,
                &[RidgeQuestion::Cross, RidgeQuestion::Start],
            ) {
                log::error!("roof ridge: {}", e);
                self.pending_roof_ridge = None;
            }
            return;
        }
        self.pending_roof_ridge = Some(PendingRoofRidge::Length {
            axis,
            coord_m,
            lo,
            len_m,
            cross_snap,
            start_snap,
            end_snap,
        });
    }

    /// Advance the pending ridge past the Length stage: the first question
    /// in `remaining` whose degree of freedom is UN-snapped and has an
    /// outline reference to measure from opens its position prompt; a
    /// snapped one needs no question (its coordinate is a reference wall's,
    /// exact by construction — the START question is skipped when EITHER
    /// extent end snapped, since either fixes the position DOF) and one
    /// with no reference auto-resolves (the document logs it — the drawn
    /// value stands). No questions left → the geometry is fully determined:
    /// run the FINALIZE placement check
    /// ([`document::validate_roof_ridge_extent`] against the RESOLVED
    /// roofs) and move to the Height stage.
    ///
    /// On a finalize rejection the pending state is left UNTOUCHED —
    /// whichever stage the caller was on stays open — so the `Err` lands on
    /// the prompt the user just answered (the no-late-rejection invariant's
    /// honest form: every stage validates everything knowable at it; the
    /// full placement check runs at the last open stage). The one caller
    /// with NO open stage — the all-snapped straight-from-stroke path —
    /// drops the stroke on `Err` instead.
    #[allow(clippy::too_many_arguments)] // the accumulating ridge state — one bundle
    pub(super) fn advance_roof_ridge(
        &mut self,
        axis: ParameterAxis,
        coord_m: f32,
        lo: f32,
        len_m: f32,
        cross_snap: Option<CeilingLineSnap>,
        start_snap: Option<CeilingLineSnap>,
        end_snap: Option<CeilingLineSnap>,
        remaining: &[RidgeQuestion],
    ) -> Result<(), String> {
        // The ridge's OWN resolved ceiling region (priority ring): both
        // position questions default their reference wall to one bounding this
        // region, so on an L-shape the default isn't the globally-nearest wall
        // in the other arm. Same ridge → same ring for Cross and Start (the
        // Start question measures on `axis.other()`, but the region is the
        // ridge's region either way). `materialize` brings the roofs current
        // so the lookup sees this ridge's area.
        let idx = self.current_storey_idx.unwrap_or(0);
        self.materialize();
        let prefer_region: Option<Vec<Point2f64>> = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .and_then(|storey| document::ridge_region_ring(storey, axis, coord_m, lo, len_m));
        for &question in remaining {
            let (snapped, prompt_axis, coord) = match question {
                RidgeQuestion::Cross => (cross_snap.is_some(), axis, coord_m),
                RidgeQuestion::Start => {
                    (start_snap.is_some() || end_snap.is_some(), axis.other(), lo)
                }
            };
            if snapped {
                continue;
            }
            if let Some(prompt) = self.document.ceiling_line_position_prompt(
                prompt_axis,
                coord,
                prefer_region.as_deref(),
            ) {
                self.pending_roof_ridge = Some(match question {
                    RidgeQuestion::Cross => PendingRoofRidge::CrossPosition {
                        axis,
                        coord_m,
                        lo,
                        len_m,
                        start_snap,
                        end_snap,
                        prompt,
                    },
                    // The Start question only opens when NEITHER end
                    // snapped — `StartPosition` carries no end snap.
                    RidgeQuestion::Start => PendingRoofRidge::StartPosition {
                        axis,
                        coord_m,
                        lo,
                        len_m,
                        cross_snap,
                        prompt,
                    },
                });
                return Ok(());
            }
            // None → logged auto-resolve: the drawn value is accepted.
        }
        // No location questions remain — the geometry is fully determined.
        // FINALIZE: bring the derived roofs current (explicitly, not riding
        // the render loop) and run the binding placement check — the
        // MIDPOINT strictly inside one flat un-apexed region, the extent
        // within that region's run through it (boundary touch allowed — a
        // wall-to-wall gable) — before opening the Height stage. (`idx` and a
        // materialize are already in hand from the prefer-region lookup above;
        // the loop only reads, so the roofs are still current.)
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a roof ridge to")?;
        document::validate_roof_ridge_extent(storey, axis, coord_m, lo, len_m)?;
        self.pending_roof_ridge = Some(PendingRoofRidge::Height {
            axis,
            coord_m,
            lo,
            len_m,
            cross_snap,
            start_snap,
            end_snap,
        });
        Ok(())
    }

    /// Set the pending ridge's LENGTH (question 1): materialize (the
    /// validation runs against the RESOLVED roofs — explicitly, not riding
    /// the render loop), validate the typed value optimistically
    /// ([`document::validate_roof_ridge_length`] — > 1 cm and fits SOME
    /// flat, un-apexed ceiling area's bbox; no location is known yet, so
    /// the binding check is finalize's), then advance to the position
    /// questions. With the extent's END snapped (and the start not), the
    /// typed length RE-ANCHORS at the wall — `lo = hi − len`, the far end
    /// stays exactly on the snapped coordinate — which fixes the position
    /// DOF and skips the Start question. When both remaining positions are
    /// snapped the finalize check runs right here, so its rejection
    /// attaches to the typed length. The pending ridge stays open on
    /// rejection so the user can retype. No-op unless the length stage is
    /// up.
    pub fn assign_roof_ridge_length(&mut self, len: f32) -> Result<(), String> {
        let Some(PendingRoofRidge::Length {
            axis,
            coord_m,
            lo,
            len_m,
            cross_snap,
            start_snap,
            end_snap,
        }) = self.pending_roof_ridge.as_ref()
        else {
            return Ok(()); // nothing awaiting a length — no-op
        };
        let (axis, coord_m, lo, len_m) = (*axis, *coord_m, *lo, *len_m);
        let (cross_snap, start_snap, end_snap) = (*cross_snap, *start_snap, *end_snap);
        let idx = self.current_storey_idx.unwrap_or(0);
        // Bring the derived roofs current — the length is validated against
        // the resolved ceiling regions, not the editing buffer.
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a roof ridge to")?;
        document::validate_roof_ridge_length(storey, axis, len)?;
        // A snapped hi (with the start un-snapped — both-snapped never
        // reaches this stage) re-anchors the typed length at the wall.
        let lo = if end_snap.is_some() && start_snap.is_none() {
            (lo + len_m) - len
        } else {
            lo
        };
        self.advance_roof_ridge(
            axis,
            coord_m,
            lo,
            len,
            cross_snap,
            start_snap,
            end_snap,
            &[RidgeQuestion::Cross, RidgeQuestion::Start],
        )
    }

    /// Skip the pending ridge's LENGTH question — accept the drawn length
    /// (already validated inside the footprint at fit time; with the END
    /// snapped the drawn length already runs to the snapped wall, so the
    /// anchor holds with no re-anchoring) and advance to the position
    /// questions. When the remaining positions are snapped the finalize
    /// check runs right here; its rejection keeps the Length stage open.
    /// No-op unless the length stage is up.
    pub fn skip_roof_ridge_length(&mut self) -> Result<(), String> {
        let Some(PendingRoofRidge::Length {
            axis,
            coord_m,
            lo,
            len_m,
            cross_snap,
            start_snap,
            end_snap,
        }) = self.pending_roof_ridge.as_ref()
        else {
            return Ok(()); // nothing awaiting a length — no-op
        };
        let (axis, coord_m, lo, len_m) = (*axis, *coord_m, *lo, *len_m);
        let (cross_snap, start_snap, end_snap) = (*cross_snap, *start_snap, *end_snap);
        self.advance_roof_ridge(
            axis,
            coord_m,
            lo,
            len_m,
            cross_snap,
            start_snap,
            end_snap,
            &[RidgeQuestion::Cross, RidgeQuestion::Start],
        )
    }

    /// The 'Centre' button on the pending ridge's OPEN location stage:
    /// materialize (the suggestion reads the RESOLVED roofs), compute the
    /// centred value for that stage — the cross coordinate
    /// ([`document::centred_ridge_cross`]) or the extent start
    /// ([`document::centred_ridge_start`]), both the midpoint of the
    /// region's span through the ridge midpoint — and advance EXACTLY as if
    /// it had been typed: the same `advance_roof_ridge` call as
    /// `apply_position_dimension`'s ridge arms, so validation / finalize /
    /// stage behaviour is identical by construction. NOT routed through
    /// `resolve_ceiling_line_position` — that maps a distance from a
    /// reference wall; Centre produces the coordinate directly. On `Err`
    /// (user-facing) the pending stage is untouched, so the prompt stays
    /// open. No-op when no location stage is up.
    pub fn centre_roof_ridge_position(&mut self) -> Result<(), String> {
        // Bring the derived roofs current — the centred value is computed
        // against the resolved ceiling regions, not the editing buffer.
        let idx = self.current_storey_idx.unwrap_or(0);
        match self.pending_roof_ridge.as_ref() {
            Some(PendingRoofRidge::CrossPosition {
                axis,
                coord_m,
                lo,
                len_m,
                start_snap,
                end_snap,
                ..
            }) => {
                let (axis, coord_m, lo, len_m) = (*axis, *coord_m, *lo, *len_m);
                let (start_snap, end_snap) = (*start_snap, *end_snap);
                self.materialize();
                let storey = self
                    .current_record
                    .as_ref()
                    .and_then(|r| r.data.storeys.get(idx))
                    .ok_or("no storey to centre a roof ridge in")?;
                let centred = document::centred_ridge_cross(storey, axis, coord_m, lo, len_m)?;
                // Cross stage implies the cross was un-snapped — none to
                // carry (apply_position_dimension's CrossPosition shape).
                self.advance_roof_ridge(
                    axis,
                    centred,
                    lo,
                    len_m,
                    None,
                    start_snap,
                    end_snap,
                    &[RidgeQuestion::Start],
                )
            }
            Some(PendingRoofRidge::StartPosition {
                axis,
                coord_m,
                lo,
                len_m,
                cross_snap,
                ..
            }) => {
                let (axis, coord_m, lo, len_m, cross_snap) =
                    (*axis, *coord_m, *lo, *len_m, *cross_snap);
                self.materialize();
                let storey = self
                    .current_record
                    .as_ref()
                    .and_then(|r| r.data.storeys.get(idx))
                    .ok_or("no storey to centre a roof ridge in")?;
                let centred = document::centred_ridge_start(storey, axis, coord_m, lo, len_m)?;
                // Start stage implies NEITHER end was snapped — none to
                // carry (apply_position_dimension's StartPosition shape).
                self.advance_roof_ridge(axis, coord_m, centred, len_m, cross_snap, None, None, &[])
            }
            _ => Ok(()), // no location stage up — no-op
        }
    }

    /// The snap tag's "Don't snap": revert the SHOWING snap's degree of
    /// freedom to its DRAWN value, drop that snap, and REOPEN its skipped
    /// question (or auto-resolve onward, logged). One tag at a time, owned
    /// by the stage ([`roof_ridge_snap_tag`](Self::roof_ridge_snap_tag)) —
    /// the normative un-snap transition table:
    ///
    /// - the END tag (at Cross or Height) reverts `hi` to its drawn value
    ///   and reopens the LENGTH question — ALWAYS, whether or not it was
    ///   skipped (re-asking is cheaper than remembering); the cross and
    ///   start snaps ride;
    /// - the START tag (at Height, when no end snap outranks it) un-snaps
    ///   `lo` and re-asks the start question (the resolved length + cross
    ///   stand);
    /// - the CROSS tag (at StartPosition, or at Height when neither end
    ///   snapped) un-snaps `coord_m` and restarts from the cross question —
    ///   re-asking an answered start question is cheaper than remembering
    ///   whether it was answered (the roof point's restart-from-X
    ///   reasoning); the resolved length stands either way (its question
    ///   came first).
    ///
    /// Deliberately NOT routed through the generic
    /// `choose_snap`/`unsnap_last` replay — those rewind history and re-run
    /// a stroke, but the pending ridge lives OUTSIDE history. No-op while
    /// un-snapped or nothing is pending.
    pub fn unsnap_roof_ridge(&mut self) {
        let Some(pending) = self.pending_roof_ridge.take() else {
            return;
        };
        let restore = pending.clone();
        let result = match pending {
            // END tag showing: any stage carrying an end snap (it outranks
            // the others everywhere it can appear — CrossPosition and
            // Height). Revert hi to the drawn value and reopen the Length
            // question with the surviving snaps riding; the Length stage
            // always opens (no auto-resolve), so this never fails.
            PendingRoofRidge::CrossPosition {
                axis,
                coord_m,
                lo,
                len_m: _,
                start_snap,
                end_snap: Some(snap),
                ..
            }
            | PendingRoofRidge::Height {
                axis,
                coord_m,
                lo,
                len_m: _,
                cross_snap: _,
                start_snap,
                end_snap: Some(snap),
            } => {
                log::debug!(
                    "roof ridge END un-snapped from outline wall {:?} back to {:.3}; \
                     reopening the length question",
                    snap.outline_wall_vid,
                    snap.drawn_coord_m
                );
                // The cross snap rides only where it exists (at the Cross
                // stage it is None by construction — its question is open).
                let cross_snap = match &restore {
                    PendingRoofRidge::Height { cross_snap, .. } => *cross_snap,
                    _ => None,
                };
                self.pending_roof_ridge = Some(PendingRoofRidge::Length {
                    axis,
                    coord_m,
                    lo,
                    len_m: snap.drawn_coord_m - lo,
                    cross_snap,
                    start_snap,
                    end_snap: None,
                });
                Ok(())
            }
            // START tag showing: the Height stage with a start snap and no
            // end snap (the only post-start-question stage under the
            // Length-first order; an end snap would outrank).
            PendingRoofRidge::Height {
                axis,
                coord_m,
                lo: _,
                len_m,
                cross_snap,
                start_snap: Some(snap),
                end_snap: None,
            } => {
                log::debug!(
                    "roof ridge START un-snapped from outline wall {:?} back to {:.3}; \
                     reopening the start question",
                    snap.outline_wall_vid,
                    snap.drawn_coord_m
                );
                self.advance_roof_ridge(
                    axis,
                    coord_m,
                    snap.drawn_coord_m,
                    len_m,
                    cross_snap,
                    None,
                    None,
                    &[RidgeQuestion::Start],
                )
            }
            // CROSS tag showing: a post-cross-question stage with a cross
            // snap (and no start/end snap outranking it).
            PendingRoofRidge::StartPosition {
                axis,
                coord_m: _,
                lo,
                len_m,
                cross_snap: Some(snap),
                ..
            }
            | PendingRoofRidge::Height {
                axis,
                coord_m: _,
                lo,
                len_m,
                cross_snap: Some(snap),
                start_snap: None,
                end_snap: None,
            } => {
                log::debug!(
                    "roof ridge CROSS un-snapped from outline wall {:?} back to {:.3}; \
                     restarting from the cross question",
                    snap.outline_wall_vid,
                    snap.drawn_coord_m
                );
                self.advance_roof_ridge(
                    axis,
                    snap.drawn_coord_m,
                    lo,
                    len_m,
                    None,
                    None,
                    None,
                    &[RidgeQuestion::Cross, RidgeQuestion::Start],
                )
            }
            other => {
                self.pending_roof_ridge = Some(other); // not snapped — no-op
                return;
            }
        };
        if let Err(e) = result {
            // Every reopened question auto-resolved (no reference walls)
            // and the finalize check rejected the drawn geometry: keep the
            // snapped stage rather than stranding the user with no stage.
            log::warn!("roof ridge un-snap rejected: {e} — keeping the snapped value");
            self.pending_roof_ridge = Some(restore);
        }
    }

    /// The pending ridge's wall snap as ONE standard canvas snap tag — one
    /// at a time, the STAGE owns which: the tag shown is the snap whose
    /// question was most recently PASSED, end outranking start outranking
    /// cross. At the Length stage no snap question has been passed yet →
    /// no tag; from the cross question onward an END snap's tag shows (at
    /// the HI endpoint — the Length question, which it shapes, has been
    /// passed); at the start question the cross snap's tag shows (at the
    /// segment's midpoint; an end snap never coexists with an open start
    /// question); at the Height stage end outranks start (LO endpoint)
    /// outranks cross — un-snapping the showing one drops back to its
    /// question, where the next tag becomes reachable again. `options` is
    /// EMPTY and `skippable` is true — the tag's menu offers exactly
    /// "Don't snap", which the UI routes to
    /// [`unsnap_roof_ridge`](Self::unsnap_roof_ridge) (NOT the generic
    /// choose-snap replay — the ridge is outside history). `None` while the
    /// showing stage has no snap or nothing is pending.
    pub fn roof_ridge_snap_tag(&self) -> Option<SnapTagView> {
        #[derive(Clone, Copy)]
        enum Tag {
            End,
            Start,
            Cross,
        }
        let (axis, snap, tag) = match self.pending_roof_ridge.as_ref()? {
            // Question 1 (length) is pre-snap-question: nothing snapped
            // that the user could reject yet.
            PendingRoofRidge::Length { .. } => return None,
            // At the cross question the Length question has been passed:
            // an END snap's tag shows (the cross stage itself implies an
            // un-snapped cross; a start snap's question hasn't arrived).
            PendingRoofRidge::CrossPosition {
                axis,
                end_snap: Some(snap),
                ..
            } => (*axis, *snap, Tag::End),
            PendingRoofRidge::CrossPosition { .. } => return None,
            PendingRoofRidge::StartPosition {
                axis,
                cross_snap: Some(snap),
                ..
            } => (*axis, *snap, Tag::Cross),
            PendingRoofRidge::Height {
                axis,
                end_snap: Some(snap),
                ..
            } => (*axis, *snap, Tag::End),
            PendingRoofRidge::Height {
                axis,
                start_snap: Some(snap),
                ..
            } => (*axis, *snap, Tag::Start),
            PendingRoofRidge::Height {
                axis,
                cross_snap: Some(snap),
                ..
            } => (*axis, *snap, Tag::Cross),
            _ => return None,
        };
        let ((ax, ay), (bx, by)) = self.pending_roof_ridge_line()?;
        let (pos, from) = match tag {
            // The START tag sits on the LO endpoint; its identity is the
            // endpoint at the drawn start coordinate (the un-snap target).
            Tag::Start => {
                let pos = (ax as f32, ay as f32);
                let from = match axis {
                    ParameterAxis::X => (pos.0, snap.drawn_coord_m),
                    ParameterAxis::Y => (snap.drawn_coord_m, pos.1),
                };
                (pos, from)
            }
            // The END tag mirrors it on the HI endpoint.
            Tag::End => {
                let pos = (bx as f32, by as f32);
                let from = match axis {
                    ParameterAxis::X => (pos.0, snap.drawn_coord_m),
                    ParameterAxis::Y => (snap.drawn_coord_m, pos.1),
                };
                (pos, from)
            }
            // The CROSS tag sits at the segment midpoint; its identity is
            // the same midpoint at the pre-snap cross coordinate.
            Tag::Cross => {
                let mid = (((ax + bx) / 2.0) as f32, ((ay + by) / 2.0) as f32);
                let from = match axis {
                    ParameterAxis::X => (snap.drawn_coord_m, mid.1),
                    ParameterAxis::Y => (mid.0, snap.drawn_coord_m),
                };
                (mid, from)
            }
        };
        Some(SnapTagView {
            pos,
            from,
            current_kind: Some(manip_2d_plan::SnapKind::ToParallelWall),
            options: Vec::new(),
            skippable: true,
        })
    }

    /// The two apex points the pending ridge would place, from its (axis,
    /// cross coordinate, start, length): a vertical line (`axis` = X) has
    /// its endpoints at `(coord, lo)`/`(coord, lo + len)`; a horizontal one
    /// (`axis` = Y) at `(lo, coord)`/`(lo + len, coord)`.
    fn ridge_endpoints(
        axis: ParameterAxis,
        coord_m: f32,
        lo: f32,
        len_m: f32,
    ) -> (Point2f64, Point2f64) {
        let hi = lo + len_m;
        match axis {
            ParameterAxis::X => (
                Point2f64::new(coord_m as f64, lo as f64),
                Point2f64::new(coord_m as f64, hi as f64),
            ),
            ParameterAxis::Y => (
                Point2f64::new(lo as f64, coord_m as f64),
                Point2f64::new(hi as f64, coord_m as f64),
            ),
        }
    }

    /// The pending ridge's accumulated `(axis, coord_m, lo, len_m)`,
    /// whatever the stage.
    fn pending_ridge_geometry(&self) -> Option<(ParameterAxis, f32, f32, f32)> {
        let (PendingRoofRidge::Length {
            axis,
            coord_m,
            lo,
            len_m,
            ..
        }
        | PendingRoofRidge::CrossPosition {
            axis,
            coord_m,
            lo,
            len_m,
            ..
        }
        | PendingRoofRidge::StartPosition {
            axis,
            coord_m,
            lo,
            len_m,
            ..
        }
        | PendingRoofRidge::Height {
            axis,
            coord_m,
            lo,
            len_m,
            ..
        }) = self.pending_roof_ridge.as_ref()?;
        Some((*axis, *coord_m, *lo, *len_m))
    }

    /// Assign the ridge height to the pending line: materialize (the
    /// validation runs against the RESOLVED roofs), validate
    /// ([`document::validate_roof_ridge`] — the midpoint strictly inside one
    /// un-apexed flat region, both endpoints claimed by exactly it (inside
    /// or on its OUTER boundary — a gable end), a height that actually
    /// slopes it, no boundary foldover; all re-checked cheaply even though
    /// finalize passed — `materialize` may have shifted derived rings), then stamp
    /// BOTH apexes as DURABLE input (`Ber::add_roof_apex` ×2 — quantization
    /// included), re-derive, and CANONICALIZE the session ceiling-step list
    /// from the derived roofs (the roof-point parity). The endpoints are
    /// the RESOLVED `(coord, lo)`/`(coord, lo + len)` — typed answers land
    /// exactly; skipped ones keep the drawn values. The pending ridge stays
    /// open on rejection so the user can retype. Outside undo.
    pub fn assign_roof_ridge_height(&mut self, z_m: f32) -> Result<(), String> {
        let Some(PendingRoofRidge::Height {
            axis,
            coord_m,
            lo,
            len_m,
            ..
        }) = self.pending_roof_ridge.as_ref()
        else {
            return Ok(()); // nothing awaiting a height — no-op
        };
        let (a, b) = Self::ridge_endpoints(*axis, *coord_m, *lo, *len_m);
        let idx = self.current_storey_idx.unwrap_or(0);
        // Bring the derived roofs current — the ridge is validated against
        // the resolved ceiling regions, not the editing buffer.
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a roof ridge to")?;
        document::validate_roof_ridge(storey, a, b, z_m as f64)?;
        let record = self.current_record.as_mut().expect("checked above");
        for p in [a, b] {
            record.data.add_roof_apex(
                idx,
                RoofApex {
                    point: p,
                    z_m: z_m as f64,
                },
            );
        }
        self.pending_roof_ridge = None;
        self.mark_dirty()?;
        // Canonicalize: derive now, then rebuild the session ceiling-step
        // list from the derived roofs (group-aware — a ridge over a stepped
        // piece re-emits ONE merged step area). In-session state thus matches
        // what a reload would reconstruct — idempotence by construction.
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// Discard the in-flight roof ridge at any stage.
    pub fn cancel_pending_roof_ridge(&mut self) {
        self.pending_roof_ridge = None;
    }

    /// True while either position question is up — the cross's (question 2)
    /// or the start's (question 3). Both show as the shared position panel,
    /// kind `Line`.
    pub fn roof_ridge_position_pending(&self) -> bool {
        matches!(
            self.pending_roof_ridge,
            Some(PendingRoofRidge::CrossPosition { .. } | PendingRoofRidge::StartPosition { .. })
        )
    }

    /// True while the ridge LENGTH question (question 1) is up.
    pub fn roof_ridge_length_pending(&self) -> bool {
        matches!(
            self.pending_roof_ridge,
            Some(PendingRoofRidge::Length { .. })
        )
    }

    /// The pending length question's PREFILL: the ridge's current (drawn)
    /// length in metres. `None` unless the length stage is up.
    pub fn pending_roof_ridge_length(&self) -> Option<f32> {
        match self.pending_roof_ridge.as_ref()? {
            PendingRoofRidge::Length { len_m, .. } => Some(*len_m),
            _ => None,
        }
    }

    /// True when the open LENGTH question's extent is ANCHORED — an end
    /// snapped onto a wall, so a typed length extends from that exact
    /// coordinate (from a snapped start, or BACK from a snapped hi — the
    /// re-anchor; the length panel's "from the highlighted end" hover shows
    /// only then). False on every other stage and while neither end is
    /// snapped (a typed length still anchors at the drawn `lo`, but nothing
    /// is exact to point at).
    pub fn roof_ridge_length_anchored(&self) -> bool {
        matches!(
            self.pending_roof_ridge,
            Some(PendingRoofRidge::Length {
                start_snap: Some(_),
                ..
            }) | Some(PendingRoofRidge::Length {
                end_snap: Some(_),
                ..
            })
        )
    }

    /// True while the resolved ridge awaits its height.
    pub fn roof_ridge_height_pending(&self) -> bool {
        matches!(
            self.pending_roof_ridge,
            Some(PendingRoofRidge::Height { .. })
        )
    }

    /// True while ANY roof-ridge input is in flight. The UI uses this to keep
    /// the toolbar reachable with the mode off and to suppress stroke/tap
    /// capture while a stage is pending.
    pub fn roof_ridge_pending(&self) -> bool {
        self.pending_roof_ridge.is_some()
    }

    /// The pending ridge's SEGMENT endpoints (world coords) for the UI's
    /// dashed overlay — the resolved-so-far extent, NOT a bbox-spanning
    /// line. `None` when nothing is pending.
    pub fn pending_roof_ridge_line(&self) -> Option<((f64, f64), (f64, f64))> {
        let (axis, coord_m, lo, len_m) = self.pending_ridge_geometry()?;
        let (a, b) = Self::ridge_endpoints(axis, coord_m, lo, len_m);
        Some(((a.x, a.y), (b.x, b.y)))
    }

    /// The current storey's COMMITTED ridges as `(end_a, end_b, label)` in
    /// world coords for the UI's segment + label overlay: every apex group
    /// with exactly TWO apexes (the grouping authority,
    /// `CeilingRegions::apex_groups`, over the materialized record). Label
    /// "⌃ 4.20 m" for a level ridge, "4.20→4.50 m" when the two end heights
    /// differ (> 1e-6 — a sloping ridge from the two-point flow). The apex
    /// dots/labels themselves still come from `roof_apex_views`.
    pub fn roof_ridge_views(&self) -> Vec<((f64, f64), (f64, f64), String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        storey
            .roofs
            .apex_groups(&storey.roof_apexes)
            .into_iter()
            .filter(|g| g.apex_idxs.len() == 2)
            .map(|g| {
                let a = &storey.roof_apexes[g.apex_idxs[0]];
                let b = &storey.roof_apexes[g.apex_idxs[1]];
                let label = if (a.z_m - b.z_m).abs() > 1e-6 {
                    format!("{:.2}→{:.2} m", a.z_m, b.z_m)
                } else {
                    format!("⌃ {:.2} m", a.z_m)
                };
                ((a.point.x, a.point.y), (b.point.x, b.point.y), label)
            })
            .collect()
    }
}
