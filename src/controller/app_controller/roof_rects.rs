//! Roof RECT-ridge input: in roof-point mode a CLOSED stroke (instead of a
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
//! apexes (`Ber::add_roof_apex` ×4 — quantization included) as the DURABLE
//! record: one apex group, no new serde. Commit shape is byte-for-byte the
//! ridge's: mark dirty → materialize → canonicalize the session
//! ceiling-step list. Per-side snapping is deferred (Centre + typed
//! positions cover the pavilion case); the shell owns this in-flight state
//! machine, outside undo — roof-ridge parity throughout.
use super::*;
use datatypes::ber::structural::{apexes_form_rect, RoofApex};
use document::LinePositionPrompt;

/// The in-flight roof-RECT input, advanced stage by stage: closed stroke →
/// [`LenX`] → [`LenY`] → [`PosX`] → [`PosY`] → finalize → [`Height`] →
/// assign. Session-only, outside undo; cleared everywhere the pending ridge
/// is. Every stage carries the accumulating rect as its LO corner + side
/// lengths (`lo + len` is the HI corner); a typed length keeps `lo` fixed,
/// a typed/centred position moves `lo` on its axis.
///
/// [`LenX`]: PendingRoofRect::LenX
/// [`LenY`]: PendingRoofRect::LenY
/// [`PosX`]: PendingRoofRect::PosX
/// [`PosY`]: PendingRoofRect::PosY
/// [`Height`]: PendingRoofRect::Height
#[derive(Debug, Clone)]
pub(super) enum PendingRoofRect {
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

impl PendingRoofRect {
    /// The accumulating `(lo, len)`, whatever the stage.
    fn geometry(&self) -> ((f32, f32), (f32, f32)) {
        match self {
            PendingRoofRect::LenX { lo, len }
            | PendingRoofRect::LenY { lo, len }
            | PendingRoofRect::PosX { lo, len, .. }
            | PendingRoofRect::PosY { lo, len, .. }
            | PendingRoofRect::Height { lo, len } => (*lo, *len),
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

    /// A CLOSED stroke in roof-point mode (routed here by
    /// [`apply_roof_ridge_stroke_event`](Self::apply_roof_ridge_stroke_event)'s
    /// closure test): fit it to an axis-aligned flat-top rectangle
    /// ([`StoreyDoc::fit_roof_rect`] — every point near the bbox boundary,
    /// all four sides within the footprint + snap tolerance) and open the
    /// LenX question, prefilled with the drawn width. A blob or an
    /// off-footprint rect is rejected (logged), like a ridge fit failure.
    pub(super) fn apply_roof_rect_stroke(&mut self, pts: &[Point2f32]) {
        dump_stroke("roof-rect", pts);
        let (lo, hi) = match self.document.fit_roof_rect(pts) {
            Ok(fit) => fit,
            Err(e) => {
                log::error!("roof rect: {}", e);
                return;
            }
        };
        self.pending_roof_rect = Some(PendingRoofRect::LenX {
            lo: (lo.x, lo.y),
            len: (hi.x - lo.x, hi.y - lo.y),
        });
    }

    /// Advance the pending rect past its length stages: open the first
    /// POSITION question that has an outline reference to measure from
    /// (`remaining` lists the axes still to ask, in flow order — X's LO
    /// side, then Y's); one with no reference auto-resolves (logged by the
    /// document — the drawn value stands). No questions left → the geometry
    /// is fully determined: run the FINALIZE placement check
    /// ([`document::validate_roof_rect_placement`] against the RESOLVED
    /// roofs) and move to the Height stage. On a finalize rejection the
    /// pending state is left UNTOUCHED — the rejection lands on whichever
    /// stage the caller was on (the ridge's no-late-rejection shape).
    fn advance_roof_rect(
        &mut self,
        lo: (f32, f32),
        len: (f32, f32),
        remaining: &[ParameterAxis],
    ) -> Result<(), String> {
        for &axis in remaining {
            let coord = match axis {
                ParameterAxis::X => lo.0,
                ParameterAxis::Y => lo.1,
            };
            if let Some(prompt) = self.document.ceiling_line_position_prompt(axis, coord) {
                self.pending_roof_rect = Some(match axis {
                    ParameterAxis::X => PendingRoofRect::PosX { lo, len, prompt },
                    ParameterAxis::Y => PendingRoofRect::PosY { lo, len, prompt },
                });
                return Ok(());
            }
            // None → logged auto-resolve: the drawn coordinate is accepted.
        }
        let idx = self.current_storey_idx.unwrap_or(0);
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a roof rect to")?;
        let (plo, phi) = Self::rect_corners_f64(lo, len);
        document::validate_roof_rect_placement(storey, plo, phi)?;
        self.pending_roof_rect = Some(PendingRoofRect::Height { lo, len });
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
        let Some(PendingRoofRect::LenX { lo, len } | PendingRoofRect::LenY { lo, len }) =
            self.pending_roof_rect.as_ref()
        else {
            return Ok(()); // nothing awaiting a length — no-op
        };
        let (lo, len) = (*lo, *len);
        let on_x = matches!(self.pending_roof_rect, Some(PendingRoofRect::LenX { .. }));
        let axis = if on_x {
            ParameterAxis::X
        } else {
            ParameterAxis::Y
        };
        let proposed = if on_x { (len_m, len.1) } else { (len.0, len_m) };
        let idx = self.current_storey_idx.unwrap_or(0);
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to assign a roof rect to")?;
        document::validate_roof_rect_length(storey, axis, len_m)?;
        if on_x {
            self.pending_roof_rect = Some(PendingRoofRect::LenY { lo, len: proposed });
            Ok(())
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
            Some(PendingRoofRect::LenX { lo, len }) => {
                let (lo, len) = (*lo, *len);
                self.pending_roof_rect = Some(PendingRoofRect::LenY { lo, len });
                Ok(())
            }
            Some(PendingRoofRect::LenY { lo, len }) => {
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
            Some(PendingRoofRect::PosX { lo, len, .. }) => {
                let (lo, len) = ((coord, lo.1), *len);
                self.advance_roof_rect(lo, len, &[ParameterAxis::Y])
            }
            Some(PendingRoofRect::PosY { lo, len, .. }) => {
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
            Some(PendingRoofRect::PosX { .. }) => ParameterAxis::X,
            Some(PendingRoofRect::PosY { .. }) => ParameterAxis::Y,
            _ => return Ok(()), // no location stage up — no-op
        };
        let (lo, len) = self.pending_roof_rect.as_ref().unwrap().geometry();
        let idx = self.current_storey_idx.unwrap_or(0);
        self.materialize();
        let storey = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
            .ok_or("no storey to centre a roof rect in")?;
        let (plo, phi) = Self::rect_corners_f64(lo, len);
        let centred = document::centred_rect_lo(storey, axis, plo, phi)?;
        self.resolve_roof_rect_position(centred)
    }

    /// The 'Centre' button, dispatched to whichever roof flow is pending —
    /// the one `CentreRoofPosition` event's handler (a ridge and a rect are
    /// never pending at once; both no-op without an open location stage).
    pub fn centre_roof_position(&mut self) -> Result<(), String> {
        if self.pending_roof_rect.is_some() {
            self.centre_roof_rect_position()
        } else {
            self.centre_roof_ridge_position()
        }
    }

    /// Assign the flat top's HEIGHT: materialize, validate
    /// ([`document::validate_roof_rect_ridge`] — placement re-checked + the
    /// height itself), then stamp FOUR corner apexes as DURABLE input
    /// (`Ber::add_roof_apex` ×4 — quantization included), re-derive, and
    /// canonicalize the session ceiling-step list — byte-for-byte the
    /// ridge's commit shape. The pending rect stays open on rejection.
    /// Outside undo.
    pub fn assign_roof_rect_height(&mut self, z_m: f32) -> Result<(), String> {
        let Some(PendingRoofRect::Height { lo, len }) = self.pending_roof_rect.as_ref() else {
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
            .ok_or("no storey to assign a roof rect to")?;
        document::validate_roof_rect_ridge(storey, plo, phi, z_m as f64)?;
        let record = self.current_record.as_mut().expect("checked above");
        for p in [
            plo,
            Point2f64::new(phi.x, plo.y),
            phi,
            Point2f64::new(plo.x, phi.y),
        ] {
            record.data.add_roof_apex(
                idx,
                RoofApex {
                    point: p,
                    z_m: z_m as f64,
                },
            );
        }
        self.pending_roof_rect = None;
        self.mark_dirty()?;
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(idx);
        Ok(())
    }

    /// Discard the in-flight roof rect at any stage.
    pub fn cancel_pending_roof_rect(&mut self) {
        self.pending_roof_rect = None;
    }

    /// True while ANY roof-rect input is in flight.
    pub fn roof_rect_pending(&self) -> bool {
        self.pending_roof_rect.is_some()
    }

    /// Which side-length question is up — `Some(X)` at LenX, `Some(Y)` at
    /// LenY, `None` otherwise. The UI keys its prefill on the axis (LenX →
    /// LenY must refill).
    pub fn roof_rect_length_pending(&self) -> Option<ParameterAxis> {
        match self.pending_roof_rect.as_ref()? {
            PendingRoofRect::LenX { .. } => Some(ParameterAxis::X),
            PendingRoofRect::LenY { .. } => Some(ParameterAxis::Y),
            _ => None,
        }
    }

    /// The open length question's PREFILL: the rect's current (drawn) side
    /// length on the open stage's axis. `None` unless a length stage is up.
    pub fn pending_roof_rect_length(&self) -> Option<f32> {
        match self.pending_roof_rect.as_ref()? {
            PendingRoofRect::LenX { len, .. } => Some(len.0),
            PendingRoofRect::LenY { len, .. } => Some(len.1),
            _ => None,
        }
    }

    /// True while either rect position question is up (both show as the
    /// shared position panel, kind `Line`).
    pub fn roof_rect_position_pending(&self) -> bool {
        matches!(
            self.pending_roof_rect,
            Some(PendingRoofRect::PosX { .. } | PendingRoofRect::PosY { .. })
        )
    }

    /// True while the resolved rect awaits its height.
    pub fn roof_rect_height_pending(&self) -> bool {
        matches!(self.pending_roof_rect, Some(PendingRoofRect::Height { .. }))
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
    /// the UI's outline + label overlay — `roof_ridge_views`' sibling: every
    /// apex group with exactly FOUR apexes forming a rect (the same
    /// predicate resolve applies, [`apexes_form_rect`]), labelled
    /// "⌃ 4.20 m". The apex dots themselves still come from
    /// `roof_apex_views`.
    pub fn roof_rect_views(&self) -> Vec<([(f64, f64); 4], String)> {
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
            .filter(|g| g.apex_idxs.len() == 4)
            .filter_map(|g| {
                let quad: Vec<&RoofApex> = g
                    .apex_idxs
                    .iter()
                    .map(|&i| &storey.roof_apexes[i])
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
