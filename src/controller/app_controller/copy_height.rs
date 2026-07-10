//! "Copy height from a known surface" — the pick-mode shell for the three
//! height prompts (dormer top, ceiling-step amount, floor-step level).
//!
//! The assessor hits "Copy from" in one of those measurement prompts; the app
//! enters a pick mode where a tap on a KNOWN-height surface reads that height
//! and commits it immediately (then closes the prompt). "Known" is the
//! imprecise-thumbs rule — a mid-slope point has a real height but nobody could
//! read it — enforced by the `ber_model` `Storey::copy_height_*_at` helpers
//! (which own the geometry). This module is thin: it detects which prompt is
//! pending, calls the helper, and routes the found height through the prompt's
//! EXISTING commit (which already snapshots for undo + validates).

use super::AppController;

/// Which of the three height prompts the current copy-height pick feeds.
/// `Dormer`/`CeilingStep` read a CEILING-context height (flat area / level
/// ridge / flat-topped wall); `FloorStep` reads a floor level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyHeightTarget {
    /// The dormer flat-top height stage — commits `assign_dormer_height(H)`.
    Dormer,
    /// The ceiling-step amount stage — commits `assign_ceiling_step_delta(H −
    /// the storey's `ceiling_height_m`)` (the delta the amount prompt expects).
    CeilingStep,
    /// The floor-step level stage — commits `assign_step_level_to_pending(H)`.
    FloorStep,
}

impl AppController {
    /// Enter copy-height pick mode for whichever of the three height prompts is
    /// currently pending (dormer height / ceiling-step amount / floor-step
    /// level). No-op if none is pending. Clears any prior rejection error.
    pub fn start_copy_height(&mut self) {
        self.copy_height_error = None;
        self.copy_height_pick =
            if self.ceiling_rect_height_pending() && self.pending_rect_is_dormer() {
                Some(CopyHeightTarget::Dormer)
            } else if self.ceiling_step_amount_pending() {
                Some(CopyHeightTarget::CeilingStep)
            } else if self.step_pick_pending() {
                Some(CopyHeightTarget::FloorStep)
            } else {
                None // nothing awaiting a height — no-op
            };
    }

    /// A tap at plan `(x, y)` (world metres, `tol` the pick radius) while in
    /// copy-height pick mode. Materializes so the height query reads RESOLVED
    /// regions, asks the `ber_model` helper for the KNOWN height under the tap,
    /// then — on a hit — routes it through the target prompt's existing commit
    /// (which snapshots for undo + validates) and closes the pick. A tap on a
    /// sloped/unknown surface sets `copy_height_error` and STAYS in pick mode;
    /// a copied height the commit's own guard rejects (e.g. a ceiling step down
    /// to the floor) surfaces that normal error and also stays, so the assessor
    /// can pick a different surface.
    pub fn copy_height_tap(&mut self, x: f64, y: f64, tol: f64) {
        if !self.copy_target_pending() {
            // The underlying prompt was dismissed out-of-band (tool/storey
            // switch, undo) — the pick is stale. Heal it and swallow the tap.
            self.cancel_copy_height();
            return;
        }
        let Some(target) = self.copy_height_pick else {
            return; // not picking — ignore
        };
        self.materialize();
        let idx = self.current_storey_idx.unwrap_or(0);
        let Some(storey) = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(idx))
        else {
            return;
        };
        let known = match target {
            CopyHeightTarget::Dormer | CopyHeightTarget::CeilingStep => {
                storey.copy_height_ceiling_at(x, y, tol)
            }
            CopyHeightTarget::FloorStep => storey.copy_height_floor_at(x, y, tol),
        };
        let Some(h) = known else {
            // Rejected: the tapped surface's height isn't readable. Stay in
            // pick mode so the assessor can try a known surface.
            self.copy_height_error = Some(
                "Pick a flat area, ridge, or wall — that surface's height isn't known.".into(),
            );
            return;
        };
        // Route the found height through the target prompt's EXISTING commit.
        // For a ceiling step the commit takes a DELTA from the storey's default
        // ceiling height, so subtract it here.
        let result = match target {
            CopyHeightTarget::Dormer => self.assign_dormer_height(h as f32),
            CopyHeightTarget::CeilingStep => {
                match self
                    .current_record
                    .as_ref()
                    .and_then(|r| r.data.storeys.get(idx))
                    .and_then(|s| s.ceiling_height_m)
                {
                    Some(default) => self.assign_ceiling_step_delta((h - default) as f32),
                    None => Err("Set the storey's default ceiling height first.".into()),
                }
            }
            CopyHeightTarget::FloorStep => self.assign_step_level_to_pending(h as f32),
        };
        match result {
            Ok(()) => {
                self.copy_height_pick = None;
                self.copy_height_error = None;
            }
            Err(e) => {
                // The commit's own guard rejected the copied height — surface
                // its normal error, stay in pick mode.
                self.copy_height_error = Some(e);
            }
        }
    }

    /// Leave copy-height pick mode without copying (re-shows the keypad).
    pub fn cancel_copy_height(&mut self) {
        self.copy_height_pick = None;
        self.copy_height_error = None;
    }

    /// True while a copy-height pick is in progress — the UI shows the pick hint
    /// (not the keypad) and routes canvas taps to [`copy_height_tap`].
    ///
    /// AUTO-HEALING: also requires the targeted height prompt to STILL be
    /// pending. The number box is a non-modal foreground window, so the assessor
    /// can switch tools / storey (or undo) mid-pick and dismiss that prompt
    /// out-of-band; gating on [`copy_target_pending`] here means the pick simply
    /// reports done — the keypad re-shows and the canvas stops swallowing taps —
    /// without threading a `copy_height_pick` reset through every mode/storey/
    /// undo path that clears the prompts it rides on.
    ///
    /// [`copy_height_tap`]: Self::copy_height_tap
    /// [`copy_target_pending`]: Self::copy_target_pending
    pub fn copy_height_pick_pending(&self) -> bool {
        self.copy_target_pending()
    }

    /// Whether the height prompt the active pick TARGETS is still pending (the
    /// dormer height / ceiling-step amount / floor-step level stage). `false`
    /// when no pick is active. The auto-heal check behind
    /// [`copy_height_pick_pending`](Self::copy_height_pick_pending) and the
    /// stale-pick guard in [`copy_height_tap`](Self::copy_height_tap).
    fn copy_target_pending(&self) -> bool {
        match self.copy_height_pick {
            Some(CopyHeightTarget::Dormer) => {
                self.ceiling_rect_height_pending() && self.pending_rect_is_dormer()
            }
            Some(CopyHeightTarget::CeilingStep) => self.ceiling_step_amount_pending(),
            Some(CopyHeightTarget::FloorStep) => self.step_pick_pending(),
            None => false,
        }
    }

    /// The transient rejection message shown under the pick hint (a tap on an
    /// unknown surface, or a copied height the commit rejected). `None` when
    /// clean.
    pub fn copy_height_error(&self) -> Option<&str> {
        self.copy_height_error.as_deref()
    }
}
