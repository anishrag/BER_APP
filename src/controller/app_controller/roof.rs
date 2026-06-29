//! Phase-1 ROOF step (Stage 1: the gated mode + the whole-house context). The
//! roof is only relevant at the TOP storey of each location, so — like the
//! floor/ceiling-exposure tools — it's gated behind a "have you added all the
//! storeys?" prompt, then shows the whole-house outline shaded by which storey
//! is the top one there (context + roof separation). The roof-area
//! proposal/drawing + geometry land in later stages.
use super::*;

/// One storey's footprint for the roof-context overlay: its outline ring (world
/// metres) + storey index. The UI draws these bottom→top so the TOP storey shows
/// at each location.
pub struct RoofContextRegion {
    pub storey_idx: usize,
    pub ring: Vec<(f64, f64)>,
}

impl AppController {
    /// True when the roof step is active (the canvas shows the whole-house
    /// context + roof controls, with the normal plan tools gated off).
    pub fn roof_mode(&self) -> bool {
        self.roof_mode
    }

    /// True while the storey-count gate prompt is up (entered the step but the
    /// assessor hasn't confirmed every storey is added).
    pub fn roof_gate_pending(&self) -> bool {
        self.roof_gate_pending
    }

    /// Enter / leave the roof step. On enter the storey-count GATE is raised (the
    /// roof depends on the full stack to know the top storey everywhere).
    /// Mutually exclusive with the other Mapping modes.
    pub fn set_roof_mode(&mut self, on: bool) {
        self.roof_mode = on;
        if on {
            self.roof_gate_pending = true;
            self.set_floor_exposure_mode(false);
            self.set_ceiling_exposure_mode(false);
            self.set_exposure_mode(false);
            self.set_openings_mode(false);
            self.set_living_area_mode(false);
            self.phase_mode = false;
            self.step_mode = false;
            self.ceiling_step_mode = false;
            self.pending_ceiling_step = None;
            self.roof_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
        } else {
            self.roof_gate_pending = false;
        }
    }

    /// Gate answer YES — every storey is added. Lower the gate and bring the
    /// resolved record current so the context reads live footprints.
    pub fn confirm_all_storeys_added_roof(&mut self) {
        self.roof_gate_pending = false;
        self.materialize();
    }

    /// Gate answer NOT-YET — leave the step so the assessor can add the rest.
    pub fn defer_storey_count_roof(&mut self) {
        self.set_roof_mode(false);
    }

    /// Each storey's resolved footprint ring (world metres) for the
    /// top-storey-shaded context overlay. Empty while the gate is up / no record.
    pub fn roof_context(&self) -> Vec<RoofContextRegion> {
        if self.roof_gate_pending {
            return Vec::new();
        }
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        record
            .data
            .storeys
            .iter()
            .enumerate()
            .filter_map(|(storey_idx, s)| {
                s.floors.footprint_ring().map(|ring| RoofContextRegion {
                    storey_idx,
                    ring: ring.iter().map(|p| (p.x, p.y)).collect(),
                })
            })
            .collect()
    }
}
