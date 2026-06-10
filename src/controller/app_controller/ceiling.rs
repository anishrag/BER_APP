//! The first-3D ceiling-height gate: 3D views need real storey heights, so the
//! first request for a 3D view while the current scope has none opens a typed
//! prompt instead of switching. Applying the height stamps the scope (via
//! [`Ber::set_default_ceiling_height`]) and completes the switch; cancelling
//! stays in 2D. The prompt flag is session-only and OUTSIDE undo — same
//! deliberate choice as phases/steps; overwriting the value is its own undo.
use super::*;

impl AppController {
    /// Ceiling heights for the CURRENT view scope: `Some(i)` → storey `i` set;
    /// `None` ("All storeys") → every storey set. False with no record/storeys.
    pub fn ceiling_heights_set(&self) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        let storeys = &record.data.storeys;
        if storeys.is_empty() {
            return false;
        }
        match self.current_storey_idx {
            Some(i) => storeys.get(i).is_some_and(|s| s.ceiling_height_m.is_some()),
            None => storeys.iter().all(|s| s.ceiling_height_m.is_some()),
        }
    }

    /// Is the first-3D "set default ceiling height" prompt open?
    pub fn ceiling_height_prompt_pending(&self) -> bool {
        self.ceiling_height_prompt
    }

    /// Apply the typed default height (m): rejects non-finite/≤0; stamps the
    /// current scope (`Ber::set_default_ceiling_height` — on the current
    /// storey, or filling every UNSET storey from the All-storeys tab), closes
    /// the prompt, switches to the 3D view the gate intercepted, marks dirty
    /// (the next materialize derives the roofs and re-extrudes the walls).
    pub fn set_default_ceiling_height(&mut self, height_m: f32) -> Result<(), String> {
        if !height_m.is_finite() || height_m <= 0.0 {
            return Err("Ceiling height must be a positive number of metres.".into());
        }
        let Some(record) = self.current_record.as_mut() else {
            return Err("no BER loaded".into());
        };
        record
            .data
            .set_default_ceiling_height(self.current_storey_idx, height_m as f64);
        // On the All-storeys tab the deferred per-storey derive never runs
        // (`refresh_storey` early-returns with no active storey), so re-derive
        // every storey EAGERLY — otherwise walls keep their provisional 2.4 m
        // extrusion and roofs stay missing until each storey is revisited.
        if self.current_storey_idx.is_none() {
            self.refresh_all_storeys();
        }
        self.ceiling_height_prompt = false;
        // Complete the switch the gate intercepted. `Storey3D` is the only 3D
        // sketch mode — per-storey vs full-house is decided by which storey tab
        // is active, so no separate target needs remembering.
        self.sketch_engine.set_mode(SketchMode::Storey3D);
        self.mark_dirty()?;
        Ok(())
    }

    /// Dismiss the prompt without setting a height (stays in 2D).
    pub fn cancel_ceiling_height_prompt(&mut self) {
        self.ceiling_height_prompt = false;
    }
}
