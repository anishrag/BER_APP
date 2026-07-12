//! Roof RECT-ridge input: in ceiling-point mode a CLOSED stroke (instead of a
//! line stroke or a tap) draws the FLAT TOP of a flat-topped hip roof — a
//! pavilion. The stroke is APPROXIMATE, so each degree of freedom gets its
//! own question in the ridge's assessor order (dimensions first, then where
//! it sits): the two side LENGTHS (typed, prefilled drawn, skippable), the
//! two LO-side POSITIONS (the ceiling-line prompt machinery reused, each
//! offering the 'Centre' suggestion), then one typed HEIGHT.
//!
//! The FINALIZE placement check ([`document::validate_roof_rect_placement`])
//! runs at the last open location stage; the height stage re-checks it
//! cheaply ([`document::validate_roof_rect_ridge`]) and stamps FOUR corner
//! apexes (`Ber::add_ceiling_apex` ×4 — quantization included) as the DURABLE
//! record: one apex group, no new serde. Commit shape is byte-for-byte the
//! ridge's: mark dirty → materialize → canonicalize the session
//! ceiling-step list. Per-side snapping is deferred (Centre + typed
//! positions cover the pavilion case); the shell owns this in-flight state
//! machine, outside undo — ceiling-ridge parity throughout.
use super::*;
use ber_model::ber::structural::{apexes_form_rect, CeilingApex};
use document::LinePositionPrompt;

/// The in-flight roof-RECT input, advanced stage by stage: closed stroke →
/// [`LenX`] → [`LenY`] → [`PosX`] → [`PosY`] → finalize → [`Height`] →
/// assign. Session-only, outside undo; cleared everywhere the pending ridge
/// is. Every stage carries the accumulating rect as its LO corner + side
/// lengths (`lo + len` is the HI corner); a typed length keeps `lo` fixed,
/// a typed/centred position moves `lo` on its axis.
///
/// [`LenX`]: PendingCeilingRect::LenX
/// [`LenY`]: PendingCeilingRect::LenY
/// [`PosX`]: PendingCeilingRect::PosX
/// [`PosY`]: PendingCeilingRect::PosY
/// [`Height`]: PendingCeilingRect::Height
#[derive(Debug, Clone)]
pub(super) enum PendingCeilingRect {
    /// Question 1: the flat top's side length along X (typed, prefilled
    /// with the drawn width, skippable).
    LenX { lo: (f32, f32), len: (f32, f32) },
    /// Question 2: the side length along Y.
    LenY { lo: (f32, f32), len: (f32, f32) },
    /// Question 3: the LO side's X position — the ceiling-line position
    /// prompt on the rect's LEFT side, with the Centre suggestion.
    PosX {
        lo: (f32, f32),
        len: (f32, f32),
        prompt: LinePositionPrompt,
    },
    /// Question 4: the LO side's Y position (the BOTTOM side). The LAST
    /// location stage: the finalize placement check runs on the way out.
    PosY {
        lo: (f32, f32),
        len: (f32, f32),
        prompt: LinePositionPrompt,
    },
    /// All four drawn values resolved and the placement finalized; awaiting
    /// the flat top's height (metres, ABSOLUTE above the storey floor datum
    /// — all four corners get it).
    Height { lo: (f32, f32), len: (f32, f32) },
}

impl PendingCeilingRect {
    /// The accumulating `(lo, len)`, whatever the stage.
    fn geometry(&self) -> ((f32, f32), (f32, f32)) {
        match self {
            PendingCeilingRect::LenX { lo, len }
            | PendingCeilingRect::LenY { lo, len }
            | PendingCeilingRect::PosX { lo, len, .. }
            | PendingCeilingRect::PosY { lo, len, .. }
            | PendingCeilingRect::Height { lo, len } => (*lo, *len),
        }
    }
}

/// Which SIDES of an in-flight DORMER rect snapped flush onto a reference edge
/// at draw time — `[x_lo, x_hi, y_lo, y_hi]` (LEFT, RIGHT, BOTTOM, TOP). Only
/// the dormer path sets these (from [`manip_plan::snap_axis_rect_to_refs`]); the
/// PAVILION leaves the default all-false, so its FSM asks every prompt exactly
/// as before. A pinned axis's POSITION prompt is skipped (the fitted coordinate
/// stands); a FULLY-pinned axis (both sides) also skips its LENGTH prompt (the
/// length is the gap between the two refs).
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct RectPins {
    x_lo: bool,
    x_hi: bool,
    y_lo: bool,
    y_hi: bool,
}

impl RectPins {
    /// From [`manip_plan::snap_axis_rect_to_refs`]' `[x_lo, x_hi, y_lo, y_hi]`.
    pub(super) fn from_sides(s: [bool; 4]) -> Self {
        Self {
            x_lo: s[0],
            x_hi: s[1],
            y_lo: s[2],
            y_hi: s[3],
        }
    }

    /// BOTH of `axis`'s sides pinned → its length is fixed (skip the length
    /// prompt).
    fn both(&self, axis: ParameterAxis) -> bool {
        match axis {
            ParameterAxis::X => self.x_lo && self.x_hi,
            ParameterAxis::Y => self.y_lo && self.y_hi,
        }
    }

    /// EITHER of `axis`'s sides pinned → its position is fixed (skip the
    /// position prompt).
    fn any(&self, axis: ParameterAxis) -> bool {
        match axis {
            ParameterAxis::X => self.x_lo || self.x_hi,
            ParameterAxis::Y => self.y_lo || self.y_hi,
        }
    }

    /// When a length on `axis` changes, keep the HI side fixed (shifting `lo`)
    /// rather than anchoring `lo`? True only when the HI side alone is pinned.
    fn keep_hi(&self, axis: ParameterAxis) -> bool {
        match axis {
            ParameterAxis::X => self.x_hi && !self.x_lo,
            ParameterAxis::Y => self.y_hi && !self.y_lo,
        }
    }
}

impl AppController {
    /// The pending rect's `(lo, hi)` corners as `Point2f64` — the form the
    /// document validators take.
    fn rect_corners_f64(lo: (f32, f32), len: (f32, f32)) -> (Point2f64, Point2f64) {
        (
            Point2f64::new(lo.0 as f64, lo.1 as f64),
            Point2f64::new((lo.0 + len.0) as f64, (lo.1 + len.1) as f64),
        )
    }

    /// A CLOSED stroke in ceiling-point mode (routed here by
    /// [`apply_roof_ridge_stroke_event`](Self::apply_roof_ridge_stroke_event)'s
    /// closure test): fit it to an axis-aligned flat-top rectangle
    /// ([`StoreyDoc::fit_roof_rect`] — every point near the bbox boundary,
    /// all four sides within the footprint + snap tolerance) and open the
    /// LenX question, prefilled with the drawn width. A blob or an
    /// off-footprint rect is rejected (logged), like a ridge fit failure.
    pub(super) fn apply_roof_rect_stroke(&mut self, pts: &[Point2f32]) {
        dump_stroke("ceiling-rect", pts);
        let (lo, hi) = match self.document.fit_roof_rect(pts) {
            Ok(fit) => fit,
            Err(e) => {
                log::error!("ceiling rect: {}", e);
                return;
            }
        };
        self.pending_roof_rect = Some(PendingCeilingRect::LenX {
            lo: (lo.x, lo.y),
            len: (hi.x - lo.x, hi.y - lo.y),
        });
    }

    /// DORMER-only: open the LENGTH stage for `axis` unless BOTH its sides are
    /// pinned — then its length is fixed (the gap between the two snapped refs),
    /// so skip it: X skips to Y's length, Y skips to the position/finalize
    /// advance. The pavilion never calls this (pins are always unset there), so
    /// its LenX→LenY flow is unchanged.
    pub(super) fn open_dormer_len_stage(
        &mut self,
        axis: ParameterAxis,
        lo: (f32, f32),
        len: (f32, f32),
    ) -> Result<(), String> {
        // Every pin read is gated on `is_dormer` so a stale pin (pins are reset
        // at fewer sites than the flag) is provably inert on any non-dormer
        // path — the invariant holds by construction, not by luck.
        match axis {
            ParameterAxis::X => {
                if self.pending_rect_is_dormer && self.pending_rect_pins.both(ParameterAxis::X) {
                    self.open_dormer_len_stage(ParameterAxis::Y, lo, len)
                } else {
                    self.pending_roof_rect = Some(PendingCeilingRect::LenX { lo, len });
                    Ok(())
                }
            }
            ParameterAxis::Y => {
                if self.pending_rect_is_dormer && self.pending_rect_pins.both(ParameterAxis::Y) {
                    self.advance_roof_rect(lo, len, &[ParameterAxis::X, ParameterAxis::Y])
                } else {
                    self.pending_roof_rect = Some(PendingCeilingRect::LenY { lo, len });
                    Ok(())
                }
            }
        }
    }

    /// Advance the pending rect past its length stages: open the first
    /// POSITION question that has an outline reference to measure from
    /// (`remaining` lists the axes still to ask, in flow order — X's LO
    /// side, then Y's); one with no reference auto-resolves (logged by the
    /// document — the drawn value stands). No questions left → the geometry
    /// is fully determined: run the FINALIZE placement check
    /// ([`document::validate_roof_rect_placement`] against the RESOLVED
    /// ceilings) and move to the Height stage. On a finalize rejection the
    /// pending state is left UNTOUCHED — the rejection lands on whichever
    /// stage the caller was on (the ridge's no-late-rejection shape).
    fn advance_roof_rect(
        &mut self,
        lo: (f32, f32),
        len: (f32, f32),
        remaining: &[ParameterAxis],
    ) -> Result<(), String> {
        for &axis in remaining {
            // DORMER pin: a side snapped onto a reference fixes that axis's
            // POSITION exactly — the fitted `lo` coordinate stands, so skip the
            // position question (un-overridable by a later typed value). The
            // pavilion never sets pins, so this branch never fires there.
            if self.pending_rect_is_dormer && self.pending_rect_pins.any(axis) {
                continue;
            }
            let coord = match axis {
                ParameterAxis::X => lo.0,
                ParameterAxis::Y => lo.1,
            };
            if let Some(prompt) = self
                .document
                .ceiling_line_position_prompt(axis, coord, None)
            {
                self.pending_roof_rect = Some(match axis {
                    ParameterAxis::X => PendingCeilingRect::PosX { lo, len, prompt },
                    ParameterAxis::Y => PendingCeilingRect::PosY { lo, len, prompt },
                });
                return Ok(());
            }
            // None → logged auto-resolve: the drawn coordinate is accepted.
        }
        // The PAVILION placement check requires a FLAT, unclaimed ceiling region.
        // A DORMER is meant to sit on the roof SLOPE (a sloped, apex-claimed
        // region), so this check would wrongly reject it — and the rejection,
        // swallowed by the dormer caller, silently strands the FSM on the width
        // prompt. The dormer's own `validate_dormer` runs at the height commit,
        // so skip this pavilion-only check for the dormer path.
        if !self.pending_rect_is_dormer {
            let idx = self.current_storey_idx.unwrap_or(0);
            self.materialize();
            let storey = self
                .current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(idx))
                .ok_or("no storey to assign a ceiling rect to")?;
            let (plo, phi) = Self::rect_corners_f64(lo, len);
            document::validate_roof_rect_placement(storey, plo, phi)?;
        }
        self.pending_roof_rect = Some(PendingCeilingRect::Height { lo, len });
        Ok(())
    }

    /// Set the pending rect's open SIDE LENGTH (LenX or LenY): materialize,
    /// validate the TYPED value on the open stage's axis only
    /// ([`document::validate_roof_rect_length`] — the optimistic per-axis
    /// bbox bound; the OTHER axis's drawn length is its own stage's to
    /// reject, or finalize's when skipped, so a bad drawn depth never
    /// rejects a good typed width; the binding check is finalize's), stamp
    /// the typed value (the LO corner stays anchored), and advance — LenX →
    /// LenY, LenY → the position questions. The pending rect stays open on
    /// rejection. No-op unless a length stage is up.
    pub fn assign_roof_rect_length(&mut self, len_m: f32) -> Result<(), String> {
        let Some(PendingCeilingRect::LenX { lo, len } | PendingCeilingRect::LenY { lo, len }) =
            self.pending_roof_rect.as_ref()
        else {
            return Ok(()); // nothing awaiting a length — no-op
        };
        let (lo, len) = (*lo, *len);
        let on_x = matches!(
            self.pending_roof_rect,
            Some(PendingCeilingRect::LenX { .. })
        );
        let axis = if on_x {
            ParameterAxis::X
        } else {
            ParameterAxis::Y
        };
        let proposed = if on_x { (len_m, len.1) } else { (len.0, len_m) };
        // The PAVILION length check validates the typed side against the ceiling
        // AREA's span through the rect (a ridge-length check). That's meaningless
        // for a DORMER — its footprint may span multiple sloped regions and is
        // validated by `validate_dormer` at the height commit; on a sloped/ridged
        // roof that span comes out ~0 and wrongly rejects EVERY width ("no ceiling
        // area is that long — under 0.00 m"). Skip it for the dormer path
        // (pavilion unchanged; a degenerate dormer is caught at the commit).
        if !self.pending_rect_is_dormer {
            let idx = self.current_storey_idx.unwrap_or(0);
            self.materialize();
            let storey = self
                .current_record
                .as_ref()
                .and_then(|r| r.data.storeys.get(idx))
                .ok_or("no storey to assign a ceiling rect to")?;
            document::validate_roof_rect_length(storey, axis, len_m)?;
        }
        // DORMER pin: when the HI side of this axis is pinned, keep IT fixed as
        // the length changes (shift `lo`); the pavilion always anchors `lo`.
        let lo = if self.pending_rect_is_dormer && self.pending_rect_pins.keep_hi(axis) {
            match axis {
                ParameterAxis::X => (lo.0 + len.0 - len_m, lo.1),
                ParameterAxis::Y => (lo.0, lo.1 + len.1 - len_m),
            }
        } else {
            lo
        };
        if on_x {
            // DORMER: skip LenY when Y is fully pinned; the pavilion always asks.
            if self.pending_rect_is_dormer {
                self.open_dormer_len_stage(ParameterAxis::Y, lo, proposed)
            } else {
                self.pending_roof_rect = Some(PendingCeilingRect::LenY { lo, len: proposed });
                Ok(())
            }
        } else {
            self.advance_roof_rect(lo, proposed, &[ParameterAxis::X, ParameterAxis::Y])
        }
    }

    /// Skip the pending rect's open SIDE LENGTH question — accept the drawn
    /// length (already validated inside the footprint at fit time) and
    /// advance. A finalize rejection (no position question opened) keeps
    /// the stage open. No-op unless a length stage is up.
    pub fn skip_roof_rect_length(&mut self) -> Result<(), String> {
        match self.pending_roof_rect.as_ref() {
            Some(PendingCeilingRect::LenX { lo, len }) => {
                let (lo, len) = (*lo, *len);
                // DORMER: skip LenY when Y is fully pinned; the pavilion asks.
                if self.pending_rect_is_dormer {
                    self.open_dormer_len_stage(ParameterAxis::Y, lo, len)
                } else {
                    self.pending_roof_rect = Some(PendingCeilingRect::LenY { lo, len });
                    Ok(())
                }
            }
            Some(PendingCeilingRect::LenY { lo, len }) => {
                let (lo, len) = (*lo, *len);
                self.advance_roof_rect(lo, len, &[ParameterAxis::X, ParameterAxis::Y])
            }
            _ => Ok(()), // nothing awaiting a length — no-op
        }
    }

    /// Resolve the open rect POSITION stage with coordinate `coord` (a
    /// typed distance already mapped through
    /// [`StoreyDoc::resolve_ceiling_line_position`], or the Centre
    /// suggestion) and advance: PosX moves `lo.x` and opens PosY; PosY (the
    /// last location stage) moves `lo.y` and runs finalize → Height. The
    /// shared body of `apply_position_dimension`'s rect arms and
    /// [`centre_roof_rect_position`](Self::centre_roof_rect_position).
    pub(super) fn resolve_roof_rect_position(&mut self, coord: f32) -> Result<(), String> {
        match self.pending_roof_rect.as_ref() {
            Some(PendingCeilingRect::PosX { lo, len, .. }) => {
                let (lo, len) = ((coord, lo.1), *len);
                self.advance_roof_rect(lo, len, &[ParameterAxis::Y])
            }
            Some(PendingCeilingRect::PosY { lo, len, .. }) => {
                let (lo, len) = ((lo.0, coord), *len);
                self.advance_roof_rect(lo, len, &[])
            }
            _ => Ok(()), // no position stage up — no-op
        }
    }

    /// The 'Centre' button on the pending RECT's open location stage:
    /// materialize, compute the centred LO for that stage's axis
    /// ([`document::centred_rect_lo`] — the span through the rect centre),
    /// and advance exactly as if it had been typed. On `Err` the stage is
    /// untouched, so the prompt stays open. No-op when no rect location
    /// stage is up.
    pub fn centre_roof_rect_position(&mut self) -> Result<(), String> {
        let axis = match self.pending_roof_rect.as_ref() {
            Some(PendingCeilingRect::PosX { .. }) => ParameterAxis::X,
            Some(PendingCeilingRect::PosY { .. }) => ParameterAxis::Y,
            _ => return Ok(()), // no location stage up — no-op
        };
        let (lo, len) = self.pending_roof_rect.as_ref().unwrap().geometry();
        let idx = self.current_storey_idx.unwrap_or(0);
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to centre a ceiling rect in")?;
        let (plo, phi) = Self::rect_corners_f64(lo, len);
        let centred = document::centred_rect_lo(storey, axis, plo, phi)?;
        self.resolve_roof_rect_position(centred)
    }

    /// The 'Centre' button, dispatched to whichever flow is pending — the one
    /// `CentreRoofPosition` event's handler. A roof ridge and a roof rect are
    /// never pending at once; with neither, the open prompt is a drawn
    /// upper-storey OUTLINE's whole-shape position, centred within the storey
    /// below ([`center_current_position`](Self::center_current_position)).
    pub fn centre_roof_position(&mut self) -> Result<(), String> {
        if self.pending_roof_rect.is_some() {
            self.centre_roof_rect_position()
        } else if self.pending_roof_ridge.is_some() {
            self.centre_roof_ridge_position()
        } else {
            self.center_current_position()
        }
    }

    /// Assign the flat top's HEIGHT: materialize, validate
    /// ([`document::validate_roof_rect_ridge`] — placement re-checked + the
    /// height itself), then stamp FOUR corner apexes as DURABLE input
    /// (`Ber::add_ceiling_apex` ×4 — quantization included), re-derive, and
    /// canonicalize the session ceiling-step list — byte-for-byte the
    /// ridge's commit shape. The pending rect stays open on rejection.
    /// Outside undo.
    pub fn assign_roof_rect_height(&mut self, z_m: f32) -> Result<(), String> {
        let Some(PendingCeilingRect::Height { lo, len }) = self.pending_roof_rect.as_ref() else {
            return Ok(()); // nothing awaiting a height — no-op
        };
        let (lo, len) = (*lo, *len);
        let (plo, phi) = Self::rect_corners_f64(lo, len);
        let idx = self.current_storey_idx.unwrap_or(0);
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a ceiling rect to")?;
        document::validate_roof_rect_ridge(storey, plo, phi, z_m as f64)?;
        let record = self.current_record.as_mut().expect("checked above");
        for p in [
            plo,
            Point2f64::new(phi.x, plo.y),
            phi,
            Point2f64::new(plo.x, phi.y),
        ] {
            record.data.add_ceiling_apex(
                idx,
                CeilingApex {
                    point: p,
                    z_m: z_m as f64,
                },
            );
        }
        self.pending_roof_rect = None;
        self.dormer_mode = false;
        self.pending_rect_is_dormer = false;
        self.pending_rect_pins = RectPins::default();
        self.mark_dirty()?;
        // Regions-as-truth: recompose the ceilings from the inputs once.
        self.recompose_ceilings(idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// Discard the in-flight ceiling rect at any stage.
    pub fn cancel_pending_roof_rect(&mut self) {
        self.pending_roof_rect = None;
        self.dormer_mode = false;
        self.pending_rect_is_dormer = false;
        self.pending_rect_pins = RectPins::default();
    }

    /// True while ANY ceiling-rect input is in flight.
    pub fn ceiling_rect_pending(&self) -> bool {
        self.pending_roof_rect.is_some()
    }

    /// Which side-length question is up — `Some(X)` at LenX, `Some(Y)` at
    /// LenY, `None` otherwise. The UI keys its prefill on the axis (LenX →
    /// LenY must refill).
    pub fn ceiling_rect_length_pending(&self) -> Option<ParameterAxis> {
        match self.pending_roof_rect.as_ref()? {
            PendingCeilingRect::LenX { .. } => Some(ParameterAxis::X),
            PendingCeilingRect::LenY { .. } => Some(ParameterAxis::Y),
            _ => None,
        }
    }

    /// The open length question's PREFILL: the rect's current (drawn) side
    /// length on the open stage's axis. `None` unless a length stage is up.
    pub fn pending_roof_rect_length(&self) -> Option<f32> {
        match self.pending_roof_rect.as_ref()? {
            PendingCeilingRect::LenX { len, .. } => Some(len.0),
            PendingCeilingRect::LenY { len, .. } => Some(len.1),
            _ => None,
        }
    }

    /// True while either rect position question is up (both show as the
    /// shared position panel, kind `Line`).
    pub fn ceiling_rect_position_pending(&self) -> bool {
        matches!(
            self.pending_roof_rect,
            Some(PendingCeilingRect::PosX { .. } | PendingCeilingRect::PosY { .. })
        )
    }

    /// True while the resolved rect awaits its height.
    pub fn ceiling_rect_height_pending(&self) -> bool {
        matches!(
            self.pending_roof_rect,
            Some(PendingCeilingRect::Height { .. })
        )
    }

    /// The pending rect's four corners (world coords, CCW) for the UI's
    /// dashed overlay. `None` when nothing is pending.
    pub fn pending_roof_rect_outline(&self) -> Option<[(f64, f64); 4]> {
        let (lo, len) = self.pending_roof_rect.as_ref()?.geometry();
        let (x0, y0) = (lo.0 as f64, lo.1 as f64);
        let (x1, y1) = ((lo.0 + len.0) as f64, (lo.1 + len.1) as f64);
        Some([(x0, y0), (x1, y0), (x1, y1), (x0, y1)])
    }

    /// The current storey's COMMITTED flat tops as `(corners, label)` for
    /// the UI's outline + label overlay — `ceiling_ridge_views`' sibling: every
    /// apex group with exactly FOUR apexes forming a rect (the same
    /// predicate resolve applies, [`apexes_form_rect`]), labelled
    /// "⌃ 4.20 m". The apex dots themselves still come from
    /// `ceiling_apex_views`.
    pub fn ceiling_rect_views(&self) -> Vec<([(f64, f64); 4], String)> {
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return Vec::new();
        };
        storey
            .ceiling_apex_groups()
            .into_iter()
            .filter(|g| g.apex_idxs.len() == 4)
            .filter_map(|g| {
                let quad: Vec<&CeilingApex> = g
                    .apex_idxs
                    .iter()
                    .map(|&i| &storey.ceiling_apexes[i])
                    .collect();
                let (lo, hi) = apexes_form_rect(&quad)?;
                let label = format!("⌃ {:.2} m", quad[0].z_m);
                Some((
                    [(lo.x, lo.y), (hi.x, lo.y), (hi.x, hi.y), (lo.x, hi.y)],
                    label,
                ))
            })
            .collect()
    }
}
