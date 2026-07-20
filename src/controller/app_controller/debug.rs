//! Debug dump of the current derivation state — the "🐞 Debug dump" button.
//! Materializes, then prefixes the session inputs the resolve projection ran
//! with (floor-step / ceiling-step areas, in-flight pending states, mode
//! flags) onto `document::debug_dump_storey`'s per-storey report, so a pasted
//! dump carries everything needed to replay a roof/wall derivation question.

use super::*;
use std::fmt::Write as _;

impl AppController {
    /// Multi-line, greppable report of the current storey's resolved
    /// geometry (or EVERY storey on the "All storeys" tab), prefixed with
    /// the controller's session state. Materializes first so the dump shows
    /// what the next render/persist would see. The UI prints it to stdout
    /// and copies it to the clipboard.
    pub fn debug_dump_current(&mut self) -> String {
        self.materialize();
        let mut s = String::new();
        let _ = writeln!(s, "=== SESSION STATE ===");
        let _ = writeln!(
            s,
            "modes: living_area={} porch={} exposure={} phase={} step={} ceiling_step={} \
             ceiling_point={}",
            self.living_area_mode,
            self.porch_mode,
            self.exposure_mode,
            self.phase_mode,
            self.step_mode,
            self.ceiling_step_mode,
            self.ceiling_point_mode
        );
        let _ = writeln!(s, "current_storey_idx: {:?}", self.current_storey_idx);
        for (idx, regions) in &self.step_regions {
            for (i, r) in regions.iter().enumerate() {
                let ring: Vec<String> = r
                    .polygon
                    .iter()
                    .map(|p| format!("({:.4},{:.4})", p.x, p.y))
                    .collect();
                let _ = writeln!(
                    s,
                    "step_region[storey {idx}][{i}] level_m={:.4} ring({}): {}",
                    r.level_m,
                    ring.len(),
                    ring.join(" ")
                );
            }
        }
        for (idx, regions) in &self.ceiling_step_regions {
            for (i, r) in regions.iter().enumerate() {
                let ring: Vec<String> = r
                    .polygon
                    .iter()
                    .map(|p| format!("({:.4},{:.4})", p.x, p.y))
                    .collect();
                let _ = writeln!(
                    s,
                    "ceiling_step_region[storey {idx}][{i}] delta_m={:.4} ring({}): {}",
                    r.delta_m,
                    ring.len(),
                    ring.join(" ")
                );
            }
        }
        let _ = writeln!(s, "pending_ceiling_step: {:?}", self.pending_ceiling_step);
        let _ = writeln!(s, "pending_roof_point: {:?}", self.pending_roof_point);
        let _ = writeln!(s, "pending_roof_ridge: {:?}", self.pending_roof_ridge);

        match &self.current_record {
            None => {
                let _ = writeln!(s, "no BER record loaded");
            }
            Some(record) => match self.current_storey_idx {
                Some(idx) => match record.data.storeys.get(idx) {
                    Some(storey) => {
                        s.push_str(&document::debug_dump_storey(storey, &format!("{idx}")));
                    }
                    None => {
                        let _ = writeln!(s, "storey {idx} not in the record");
                    }
                },
                // The All-storeys tab: dump every storey.
                None => {
                    for (idx, storey) in record.data.storeys.iter().enumerate() {
                        s.push_str(&document::debug_dump_storey(storey, &format!("{idx}")));
                    }
                }
            },
        }
        s
    }
}
