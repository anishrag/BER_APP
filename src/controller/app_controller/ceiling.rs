//! Default ceiling heights: entered via the Heights category whenever the
//! assessor wants (no prompt flow — the 2D/3D toggle is simply disabled until
//! the current scope has its heights). Setting a height stamps the scope (via
//! [`Ber::set_default_ceiling_height`]) and stays in the current view; the 3D
//! switch is a separate, now-ungated action. Heights are OUTSIDE undo — same
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

    /// The current scope's default ceiling height for display in the Heights
    /// field: per-storey → that storey's value; All-storeys → the shared value
    /// when every storey is set to the SAME height, else `None` (a mixed or
    /// incomplete house has no single value to show).
    pub fn default_ceiling_height(&self) -> Option<f64> {
        let record = self.current_record.as_ref()?;
        let storeys = &record.data.storeys;
        match self.current_storey_idx {
            Some(i) => storeys.get(i)?.ceiling_height_m,
            None => {
                let first = storeys.first()?.ceiling_height_m?;
                storeys
                    .iter()
                    .all(|s| s.ceiling_height_m.is_some_and(|h| (h - first).abs() < 1e-9))
                    .then_some(first)
            }
        }
    }

    /// Apply the typed default height (m) from the Heights category: rejects
    /// non-finite/≤0; stamps the current scope (`Ber::set_default_ceiling_height`
    /// — overwriting on the current storey, or filling every UNSET storey from
    /// the All-storeys tab), marks dirty (the next materialize derives the
    /// ceilings and re-extrudes the walls). Stays in the current view — switching
    /// to 3D is the floating canvas button's separate action.
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
        // extrusion and ceilings stay missing until each storey is revisited.
        if self.current_storey_idx.is_none() {
            self.refresh_all_storeys();
        }
        self.mark_dirty()?;
        Ok(())
    }
}
