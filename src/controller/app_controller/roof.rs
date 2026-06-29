//! Phase-1 ROOF step (Stage 1: the gated mode + the whole-house context). The
//! roof is only relevant at the TOP storey of each location, so — like the
//! floor/ceiling-exposure tools — it's gated behind a "have you added all the
//! storeys?" prompt, then shows the whole-house outline shaded by which storey
//! is the top one there (context + roof separation). The roof-area
//! proposal/drawing + geometry land in later stages.
use super::*;
use ber_model::ber::structural::{CeilingExposure, SurfaceKind};

/// One storey's footprint for the roof-context overlay: its outline ring (world
/// metres) + storey index. The UI draws these bottom→top so the TOP storey shows
/// at each location.
pub struct RoofContextRegion {
    pub storey_idx: usize,
    pub ring: Vec<(f64, f64)>,
}

/// One selectable ceiling region in the roof step: its plan ring (world metres,
/// no closing duplicate), the owning storey, and whether it's currently grouped
/// into the roof-area-in-progress.
pub struct RoofRegionView {
    pub storey_idx: usize,
    pub ring: Vec<(f64, f64)>,
    pub selected: bool,
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
        self.roof_selected.clear();
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

    /// The selectable ceiling regions for the roof step — every non-party ceiling
    /// across the stack (those have sky / attic above, i.e. a roof), with the
    /// grouped-in-progress flag from `roof_selected`. Empty while the gate is up.
    pub fn roof_regions_view(&self) -> Vec<RoofRegionView> {
        if self.roof_gate_pending {
            return Vec::new();
        }
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (storey_idx, storey) in record.data.storeys.iter().enumerate() {
            for r in storey
                .roofs
                .regions
                .iter()
                // Party ceilings have a dwelling above, not a roof — not selectable.
                .filter(|r| r.exposure != CeilingExposure::Party)
            {
                let mut ring: Vec<(f64, f64)> =
                    r.polygon.exterior.iter().map(|v| (v.x, v.y)).collect();
                if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
                    ring.pop();
                }
                let selected = self
                    .roof_selected
                    .contains(&(storey_idx, document::centroid_key(r.centroid())));
                out.push(RoofRegionView {
                    storey_idx,
                    ring,
                    selected,
                });
            }
        }
        out
    }

    /// Toggle the ceiling region under a tapped point (world metres) in/out of the
    /// roof-area-in-progress — hit-tested across every storey, smallest-area
    /// region wins. No-op while the gate is up.
    pub fn roof_region_tap(&mut self, world_x: f64, world_y: f64, tol: f64) {
        if self.roof_gate_pending {
            return;
        }
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let mut best: Option<(f64, usize, document::CentroidKey)> = None;
        for (idx, storey) in record.data.storeys.iter().enumerate() {
            if let Some(id) = storey.surface_at(SurfaceKind::Roofs, world_x, world_y, tol) {
                if let Some(region) = storey
                    .roofs
                    .regions
                    .iter()
                    .find(|r| r.id == id && r.exposure != CeilingExposure::Party)
                {
                    let area = region.area();
                    if best.as_ref().is_none_or(|(a, _, _)| area < *a) {
                        best = Some((area, idx, document::centroid_key(region.centroid())));
                    }
                }
            }
        }
        if let Some((_, idx, key)) = best {
            let sel = (idx, key);
            if !self.roof_selected.insert(sel) {
                self.roof_selected.remove(&sel);
            }
        }
    }
}
