//! Phase-1 ROOF step (Stage 1: the gated mode + the whole-house context). The
//! roof is only relevant at the TOP storey of each location, so — like the
//! floor/ceiling-exposure tools — it's gated behind a "have you added all the
//! storeys?" prompt, then shows the whole-house outline shaded by which storey
//! is the top one there (context + roof separation). The roof-area
//! proposal/drawing + geometry land in later stages.
use super::*;
use ber_model::ber::structural::{CeilingExposure, RoofApex, SurfaceKind};
use domain::points::Point2f64;
use manip_plan::ParameterAxis;

/// The roof shape the assessor picks for a grouped roof area. `Flat` keeps the
/// ceiling level (a flat roof, no apex); `Gable`/`Hip` slope up to a ridge — the
/// difference is whether the ridge reaches the end walls (gable: vertical end;
/// hip: the end slopes down too), which we realise by extending the ridge to the
/// area extent for a gable and keeping the drawn length for a hip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoofShape {
    Flat,
    Gable,
    Hip,
}

/// The in-flight per-area roof drawing, after the assessor grouped some ceiling
/// areas (`roof_selected`) and picked a sloped shape. Session-only, outside undo
/// (roof apexes are durable inputs re-applied on every resolve), cleared on mode
/// exit / cancel / commit.
#[derive(Clone, Debug)]
pub enum PendingRoofArea {
    /// Awaiting the ridge STROKE for `shape` over the grouped area on `storey_idx`.
    DrawRidge { shape: RoofShape, storey_idx: usize },
    /// Ridge placed (endpoints `a`/`b`); awaiting the PITCH in degrees. `perp_half`
    /// is half the area's span perpendicular to the ridge and `eave_z` the eave
    /// height (the storey ceiling height) — together they turn pitch into the apex
    /// height: `z = eave_z + perp_half · tan(pitch)`.
    PickPitch {
        storey_idx: usize,
        a: Point2f64,
        b: Point2f64,
        perp_half: f64,
        eave_z: f64,
    },
}

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
        self.pending_roof_area = None;
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

    /// True once at least one ceiling area is grouped and no per-area drawing is
    /// in flight — the shape buttons (Flat / Gable / Hip) are offered.
    pub fn roof_area_can_pick_shape(&self) -> bool {
        self.roof_mode
            && !self.roof_gate_pending
            && self.pending_roof_area.is_none()
            && !self.roof_selected.is_empty()
    }

    /// True while the grouped area awaits its ridge STROKE.
    pub fn roof_area_ridge_pending(&self) -> bool {
        matches!(
            self.pending_roof_area,
            Some(PendingRoofArea::DrawRidge { .. })
        )
    }

    /// True while the placed ridge awaits its PITCH (degrees).
    pub fn roof_area_pitch_pending(&self) -> bool {
        matches!(
            self.pending_roof_area,
            Some(PendingRoofArea::PickPitch { .. })
        )
    }

    /// The placed ridge's endpoints (world coords) for the dashed overlay while
    /// the pitch is being entered. `None` otherwise.
    pub fn pending_roof_area_ridge_line(&self) -> Option<((f64, f64), (f64, f64))> {
        match self.pending_roof_area.as_ref()? {
            PendingRoofArea::PickPitch { a, b, .. } => Some(((a.x, a.y), (b.x, b.y))),
            _ => None,
        }
    }

    /// The combined bbox of the grouped ceiling regions on `storey_idx` (world
    /// metres). `None` when none are grouped there.
    fn roof_selected_bbox(&self, storey_idx: usize) -> Option<((f64, f64), (f64, f64))> {
        let storey = self.current_record.as_ref()?.data.storeys.get(storey_idx)?;
        let mut bb: Option<(f64, f64, f64, f64)> = None;
        for r in &storey.roofs.regions {
            if !self
                .roof_selected
                .contains(&(storey_idx, document::centroid_key(r.centroid())))
            {
                continue;
            }
            for v in &r.polygon.exterior {
                bb = Some(match bb {
                    None => (v.x, v.y, v.x, v.y),
                    Some((minx, miny, maxx, maxy)) => {
                        (minx.min(v.x), miny.min(v.y), maxx.max(v.x), maxy.max(v.y))
                    }
                });
            }
        }
        bb.map(|(minx, miny, maxx, maxy)| ((minx, miny), (maxx, maxy)))
    }

    /// The storey the grouped area belongs to — the storey index of the grouped
    /// regions (first one). First cut: a roof area is assumed to sit on a single
    /// storey (apexes are per-storey).
    fn roof_target_storey(&self) -> Option<usize> {
        self.roof_selected.iter().map(|(idx, _)| *idx).min()
    }

    /// Pick the shape for the grouped roof area. `Flat` commits immediately (a
    /// flat roof keeps the ceiling level — no apex) and clears the grouping;
    /// `Gable`/`Hip` start the ridge-draw flow. No-op without a grouped area.
    pub fn pick_roof_area_shape(&mut self, shape: RoofShape) {
        if !self.roof_area_can_pick_shape() {
            return;
        }
        let Some(storey_idx) = self.roof_target_storey() else {
            return;
        };
        match shape {
            RoofShape::Flat => {
                // A flat roof is the default ceiling — nothing to apex. Clear the
                // grouping so the assessor can move to the next area.
                self.roof_selected.clear();
            }
            RoofShape::Gable | RoofShape::Hip => {
                self.pending_roof_area = Some(PendingRoofArea::DrawRidge { shape, storey_idx });
            }
        }
    }

    /// A ridge stroke (world/metre coords) for the grouped area: fit it to an
    /// axis-aligned segment, place the two ridge endpoints (a GABLE extends them
    /// to the area's full span so the end walls rise vertically; a HIP keeps the
    /// drawn length so the ends slope), then open the PITCH question. The apex
    /// height is computed from pitch at assign time. Returns a user-facing error
    /// if the stroke isn't line-like or the storey has no ceiling height.
    pub fn apply_roof_area_ridge(&mut self, world_points: &[(f32, f32)]) -> Result<(), String> {
        let Some(PendingRoofArea::DrawRidge { shape, storey_idx }) =
            self.pending_roof_area.as_ref()
        else {
            return Ok(());
        };
        let (shape, storey_idx) = (*shape, *storey_idx);
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        let (axis, coord, (mut lo, mut hi)) =
            manip_plan::fit_axis_segment(&pts).ok_or_else(|| {
                "That stroke isn't a straight line — draw one straight stroke along the ridge."
                    .to_string()
            })?;
        let ((minx, miny), (maxx, maxy)) = self
            .roof_selected_bbox(storey_idx)
            .ok_or("No grouped roof area to place the ridge in.")?;
        let eave_z = self
            .current_record
            .as_ref()
            .and_then(|r| r.data.storeys.get(storey_idx))
            .and_then(|s| s.ceiling_height_m)
            .ok_or("Set this storey's ceiling height before drawing its roof.")?;
        // `axis == X` ⇒ ridge runs along Y at x = coord, slopes fall in X;
        // `axis == Y` ⇒ ridge runs along X at y = coord, slopes fall in Y.
        let (along_lo, along_hi, perp_half) = match axis {
            ParameterAxis::X => (miny, maxy, (maxx - minx) / 2.0),
            ParameterAxis::Y => (minx, maxx, (maxy - miny) / 2.0),
        };
        // A gable spans the full area along the ridge (vertical end walls); a hip
        // keeps the drawn length (the ends slope in).
        if shape == RoofShape::Gable {
            lo = along_lo as f32;
            hi = along_hi as f32;
        }
        let (a, b) = match axis {
            ParameterAxis::X => (
                Point2f64::new(coord as f64, lo as f64),
                Point2f64::new(coord as f64, hi as f64),
            ),
            ParameterAxis::Y => (
                Point2f64::new(lo as f64, coord as f64),
                Point2f64::new(hi as f64, coord as f64),
            ),
        };
        self.pending_roof_area = Some(PendingRoofArea::PickPitch {
            storey_idx,
            a,
            b,
            perp_half,
            eave_z,
        });
        Ok(())
    }

    /// Assign the PITCH (degrees) to the placed ridge: derive the apex height
    /// (`eave_z + perp_half · tan(pitch)`), stamp BOTH ridge endpoints as durable
    /// apexes on the storey, recompose the roofs, and clear the grouping. Pitch
    /// must be in `(0, 90)`. No-op unless the pitch stage is up.
    pub fn assign_roof_area_pitch(&mut self, pitch_deg: f32) -> Result<(), String> {
        let Some(PendingRoofArea::PickPitch {
            storey_idx,
            a,
            b,
            perp_half,
            eave_z,
        }) = self.pending_roof_area.as_ref()
        else {
            return Ok(());
        };
        let (storey_idx, a, b, perp_half, eave_z) = (*storey_idx, *a, *b, *perp_half, *eave_z);
        if !(pitch_deg > 0.0 && pitch_deg < 90.0) {
            return Err("Enter a roof pitch between 0 and 90 degrees.".to_string());
        }
        let z_m = eave_z + perp_half * (pitch_deg as f64).to_radians().tan();
        let record = self
            .current_record
            .as_mut()
            .ok_or("No storey to assign the roof to.")?;
        for p in [a, b] {
            record
                .data
                .add_roof_apex(storey_idx, RoofApex { point: p, z_m });
        }
        self.pending_roof_area = None;
        self.roof_selected.clear();
        self.mark_dirty()?;
        // Regions-as-truth: recompose the roofs from the durable inputs, then
        // re-derive, and keep the ceiling-step display cache in step.
        self.recompose_roofs(storey_idx);
        self.materialize();
        self.reconstruct_ceiling_step_regions_for(storey_idx);
        Ok(())
    }

    /// Discard the in-flight per-area roof drawing (keeps the grouping).
    pub fn cancel_roof_area(&mut self) {
        self.pending_roof_area = None;
    }
}
