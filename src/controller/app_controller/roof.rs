//! Phase-1 ROOF step (Stage 1: the gated mode + the whole-house context). The
//! roof is only relevant at the TOP storey of each location, so — like the
//! floor/ceiling-exposure tools — it's gated behind a "have you added all the
//! storeys?" prompt, then shows the whole-house outline shaded by which storey
//! is the top one there (context + roof separation). The roof-area
//! proposal/drawing + geometry land in later stages.
use super::*;
use ber_model::ber::roof::RoofArea;
use ber_model::ber::structural::{CeilingExposure, SurfaceKind};
use domain::points::Point2f64;
use manip_plan::ParameterAxis;
use std::collections::BTreeSet;

// The persisted roof-shape enum; re-exported (and again from `mod.rs`) so the UI
// keeps reaching it via `crate::controller::app_controller::RoofShape`.
pub use ber_model::ber::roof::RoofShape;

/// The in-flight per-area roof drawing, after the assessor grouped some ceiling
/// areas (`roof_selected`, possibly across SEVERAL storeys — an attic
/// conversion plus the storey below) and picked a sloped shape. Session-only,
/// cleared on mode exit / cancel / commit. The committed roof area lands on
/// `Ber.roof` (SEPARATE from the ceiling) — it does NOT touch the ceiling
/// apexes. `storey_idx` is the BASE storey (the lowest covered eave,
/// `roof_base_and_eave`).
#[derive(Clone, Debug)]
pub enum PendingRoofArea {
    /// Awaiting ridge STROKE(s) for `shape` over the grouped area based on
    /// `storey_idx`. A GABLE takes one stroke and moves on; a HIP collects
    /// every ridge the assessor draws in `ridges` (an L-shaped hip has
    /// several) until they Finish. Each stroke is edge-snapped
    /// ([`snap_ridge_to_edge`], [`RIDGE_EDGE_SNAP_M`]) as it lands.
    DrawRidge {
        shape: RoofShape,
        storey_idx: usize,
        ridges: Vec<(Point2f64, Point2f64)>,
    },
    /// Ridge(s) placed; awaiting the PITCH in degrees. `shape` is Gable or
    /// Hip; `eave_z` is the covered storey's ceiling height (the eave), the
    /// ridges rise from there by the pitch (each over its own local run).
    PickPitch {
        storey_idx: usize,
        shape: RoofShape,
        ridges: Vec<(Point2f64, Point2f64)>,
        eave_z: f64,
    },
}

/// How close (metres, world) a drawn ridge must be to a footprint edge to be
/// treated as ON it (a mono-pitch eave-line drawn roughly along the wall).
const RIDGE_EDGE_SNAP_M: f64 = 0.5;

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
        // Leaving the outline draw tool: drop any un-decided re-draw stroke
        // (a stray second outline) so it doesn't linger on the canvas. (n3)
        self.pending_redraw = None;
        self.roof_mode = on;
        self.roof_selected.clear();
        self.pending_roof_area = None;
        if on {
            // The roof step is a PLAN task: its takeover replaces the canvas
            // with the whole-house plan, and its tap/ridge input only routes
            // in 2D. Entering from the 3D view would show the overlay with
            // DEAD input — drop back to the 2D plan first.
            self.set_sketch_mode(SketchMode::Storey2D);
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
            self.ceiling_point_mode = false;
            self.pending_roof_point = None;
            self.pending_roof_ridge = None;
            self.pending_roof_rect = None;
            self.dormer_mode = false;
            self.pending_rect_is_dormer = false;
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

    /// The selectable ceiling regions for the roof step — every ceiling across
    /// the stack with sky / attic above it (i.e. a roof): `Party` (another
    /// dwelling above) and derived `InterStorey` (our own storey above) regions
    /// get no roof and drop off. Grouped-in-progress flag from `roof_selected`.
    /// Empty while the gate is up.
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
                .ceilings
                .regions
                .iter()
                // Party ceilings have a dwelling above, InterStorey ones our
                // own storey above — neither has a roof, so neither is
                // selectable.
                .filter(|r| {
                    !matches!(
                        r.exposure,
                        CeilingExposure::Party | CeilingExposure::InterStorey
                    )
                })
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
            if let Some(id) = storey.surface_at(SurfaceKind::Ceilings, world_x, world_y, tol) {
                if let Some(region) = storey.ceilings.regions.iter().find(|r| {
                    r.id == id
                        && !matches!(
                            r.exposure,
                            CeilingExposure::Party | CeilingExposure::InterStorey
                        )
                }) {
                    let area = region.area();
                    if best.as_ref().is_none_or(|(a, _, _)| area < *a) {
                        best = Some((area, idx, document::centroid_key(region.centroid())));
                    }
                }
            }
        }
        if let Some((_, idx, key)) = best {
            // Regions from DIFFERENT storeys may share one roof area: an attic
            // conversion's sloped ceiling plus the surrounding flat ceilings of
            // the storey below form ONE roof (base storey + eave resolved by
            // `roof_base_and_eave`, apexes datum-shifted at commit).
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

    /// Every placed ridge's endpoints (world coords) for the dashed overlay —
    /// a hip's collected ridges while more are being drawn, and the full set
    /// while the pitch is being entered. Empty otherwise.
    pub fn pending_roof_area_ridge_lines(&self) -> Vec<((f64, f64), (f64, f64))> {
        let ridges = match self.pending_roof_area.as_ref() {
            Some(PendingRoofArea::DrawRidge { ridges, .. })
            | Some(PendingRoofArea::PickPitch { ridges, .. }) => ridges,
            None => return Vec::new(),
        };
        ridges
            .iter()
            .map(|(a, b)| ((a.x, a.y), (b.x, b.y)))
            .collect()
    }

    /// `Some(count of ridges drawn so far)` while a HIP awaits its ridges —
    /// drives the "draw them all, then Finish" toolbar. `None` for a gable's
    /// single-stroke ridge stage (and outside the ridge stage).
    pub fn roof_area_hip_ridge_count(&self) -> Option<usize> {
        match self.pending_roof_area.as_ref()? {
            PendingRoofArea::DrawRidge {
                shape: RoofShape::Hip,
                ridges,
                ..
            } => Some(ridges.len()),
            _ => None,
        }
    }

    /// True while a GABLE's placed ridge awaits its pitch — the stage that
    /// offers "Centre on roof" (re-centre the rough stroke between the eaves).
    pub fn roof_area_can_centre_ridge(&self) -> bool {
        matches!(
            self.pending_roof_area,
            Some(PendingRoofArea::PickPitch {
                shape: RoofShape::Gable,
                ..
            })
        )
    }

    /// Re-centre the pending GABLE ridge between the footprint boundaries it
    /// spans ([`centre_ridge`]) — the assessor drew it roughly and asked for
    /// the centred version. No-op outside the gable pitch stage.
    ///
    /// [`centre_ridge`]: ber_model::ber::roof::centre_ridge
    pub fn centre_pending_roof_ridge(&mut self) {
        if !matches!(
            self.pending_roof_area.as_ref(),
            Some(PendingRoofArea::PickPitch {
                shape: RoofShape::Gable,
                ..
            })
        ) {
            return;
        }
        // The grouping is still live during the pitch stage (cleared on commit).
        let footprint = self.roof_selected_rings();
        if let Some(PendingRoofArea::PickPitch { ridges, .. }) = self.pending_roof_area.as_mut() {
            for ridge in ridges.iter_mut() {
                *ridge = ber_model::ber::roof::centre_ridge(*ridge, &footprint);
            }
        }
    }

    /// The combined bbox of ALL grouped ceiling regions (world metres, across
    /// every selected storey). `None` when nothing is grouped.
    fn roof_selected_bbox(&self) -> Option<((f64, f64), (f64, f64))> {
        let record = self.current_record.as_ref()?;
        let mut bb: Option<(f64, f64, f64, f64)> = None;
        for (storey_idx, storey) in record.data.storeys.iter().enumerate() {
            for r in &storey.ceilings.regions {
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
        }
        bb.map(|(minx, miny, maxx, maxy)| ((minx, miny), (maxx, maxy)))
    }

    /// The sorted, de-duplicated storey indices of the grouped regions — the
    /// committed area's `covered_storeys`.
    fn roof_selected_storeys(&self) -> Vec<usize> {
        self.roof_selected
            .iter()
            .map(|(idx, _)| *idx)
            .collect::<BTreeSet<usize>>()
            .into_iter()
            .collect()
    }

    /// The grouped selection's BASE storey + the eave height LOCAL to its floor
    /// datum. Every grouped region contributes a world-z eave candidate: its
    /// resolved flat plane (`datum + flat_z`) or — for a sloped region (a
    /// converted attic's rafter ceiling) — the storey's default ceiling
    /// height. A SLOPED roof takes the LOWEST candidate (`highest = false`):
    /// the merged footprint's boundary sits on the lowest covered ceiling — an
    /// attic-spanning gable rises from the ceiling of the storey BELOW. A FLAT
    /// roof takes the HIGHEST: it lies on the highest ceiling piece it covers
    /// (a roof over a stepped ceiling sits on the raised side, not inside it).
    /// The base storey is the winning candidate's owner, so the stored
    /// storey-local `eave_z` renders at the right world height.
    fn roof_base_and_eave(&self, highest: bool) -> Option<(usize, f64)> {
        let record = self.current_record.as_ref()?;
        let datums = record.data.storey_floor_datums();
        let mut best: Option<(usize, f64, f64)> = None; // (storey, world_z, datum)
        for storey_idx in self.roof_selected_storeys() {
            let Some(storey) = record.data.storeys.get(storey_idx) else {
                continue;
            };
            let datum = datums.get(storey_idx).copied().unwrap_or(0.0);
            let local = storey
                .ceilings
                .regions
                .iter()
                .filter(|r| {
                    self.roof_selected
                        .contains(&(storey_idx, document::centroid_key(r.centroid())))
                })
                .filter_map(|r| r.flat_z())
                .fold(None, |acc: Option<f64>, z| {
                    Some(acc.map_or(z, |a| if highest { a.max(z) } else { a.min(z) }))
                });
            let Some(local) = local.or(storey.ceiling_height_m) else {
                continue;
            };
            let world = datum + local;
            let better = match best {
                None => true,
                Some((_, w, _)) => {
                    if highest {
                        world > w
                    } else {
                        world < w
                    }
                }
            };
            if better {
                best = Some((storey_idx, world, datum));
            }
        }
        best.map(|(storey_idx, world, datum)| (storey_idx, world - datum))
    }

    /// The grouped ceiling-area rings (plan, world metres, no closing
    /// duplicate) across EVERY selected storey, UNIONED into contiguous
    /// pieces — the roof area's covered footprint. Merged so the grouped
    /// selection is ONE roof: ridge snapping/centring measure against the
    /// COMBINED boundary (not the internal seams between grouped areas —
    /// including the seam where an attic's rings meet the storey-below's),
    /// corner-snap targets are the real outline corners, and the committed
    /// footprint fans as one surface.
    fn roof_selected_rings(&self) -> Vec<Vec<Point2f64>> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut rings: Vec<Vec<Point2f64>> = Vec::new();
        for (storey_idx, storey) in record.data.storeys.iter().enumerate() {
            for r in storey.ceilings.regions.iter().filter(|r| {
                self.roof_selected
                    .contains(&(storey_idx, document::centroid_key(r.centroid())))
            }) {
                let mut ring: Vec<Point2f64> = r
                    .polygon
                    .exterior
                    .iter()
                    .map(|v| Point2f64::new(v.x, v.y))
                    .collect();
                if ring.len() >= 2 && ring[0] == ring[ring.len() - 1] {
                    ring.pop();
                }
                rings.push(ring);
            }
        }
        ber_model::ber::roof::merge_footprint_rings(&rings)
    }

    /// True when ANY grouped storey has a SLOPED ceiling (it has ceiling
    /// apexes) — a converted attic / room-in-roof, where the roof should adopt
    /// the ceiling's slope ("Follow ceiling") rather than a freshly-drawn
    /// pitch. A multi-storey grouping (attic + the storey below) counts: the
    /// roof follows the attic's rafters and extends them over the rest.
    pub fn roof_area_ceiling_is_sloped(&self) -> bool {
        let Some(record) = self.current_record.as_ref() else {
            return false;
        };
        self.roof_selected_storeys().into_iter().any(|idx| {
            record
                .data
                .storeys
                .get(idx)
                .map(|s| !s.ceiling_apexes.is_empty())
                .unwrap_or(false)
        })
    }

    /// Commit a finished roof area onto `Ber.roof` (SEPARATE from the ceiling — it
    /// does NOT touch the ceiling apexes), then clear the grouping + pending state.
    /// `storey_idx` is the BASE storey (`roof_base_and_eave`); `covered_storeys`
    /// records every grouped storey. The geometry-driving apexes are computed by
    /// `shape`: `Gable`/`Hip` from the drawn ridge(s) + pitch (one apex pair per
    /// ridge, each over its own local run); `FollowCeiling` copies every covered
    /// storey's ceiling apexes DATUM-SHIFTED into base-storey-local z (so an
    /// attic's rafter planes extend over the storey below at the right world
    /// height); `Flat` has none. The descriptive `ridge` field records the FIRST
    /// drawn ridge (the apexes carry the full multi-ridge geometry).
    fn commit_roof_area(
        &mut self,
        storey_idx: usize,
        shape: RoofShape,
        pitch_deg: f32,
        eave_z: f64,
        ridges: Vec<(Point2f64, Point2f64)>,
    ) -> Result<(), String> {
        let footprint = self.roof_selected_rings();
        let covered_storeys = self.roof_selected_storeys();
        let apexes = match shape {
            RoofShape::Flat => Vec::new(),
            RoofShape::Gable | RoofShape::Hip => ridges
                .iter()
                .flat_map(|r| RoofArea::ridge_apexes(*r, pitch_deg, eave_z, &footprint))
                .collect(),
            // Adopt the covered storeys' ceiling slope (converted attic) —
            // apex z re-based onto the BASE storey's datum in ber_model.
            RoofShape::FollowCeiling => {
                let record = self
                    .current_record
                    .as_ref()
                    .ok_or("No record to add the roof to.")?;
                ber_model::ber::roof::adopted_ceiling_apexes(
                    &record.data,
                    storey_idx,
                    &covered_storeys,
                )
            }
        };
        let record = self
            .current_record
            .as_mut()
            .ok_or("No record to add the roof to.")?;
        record.data.roof.areas.push(RoofArea {
            id: ItemId::default(),
            storey_idx,
            covered_storeys,
            shape,
            pitch_deg,
            eave_z,
            ridge: ridges.first().copied(),
            footprint,
            apexes,
        });
        self.pending_roof_area = None;
        self.roof_selected.clear();
        self.mark_dirty()?;
        Ok(())
    }

    /// Pick the shape for the grouped roof area. `Flat` and `FollowCeiling` commit
    /// immediately (no ridge/pitch — FollowCeiling reuses the sloped ceiling);
    /// `Gable`/`Hip` start the ridge-draw flow. No-op without a grouped area.
    /// A FLAT roof lies on the HIGHEST grouped ceiling piece; a sloped one
    /// rises from the LOWEST (its eave).
    pub fn pick_roof_area_shape(&mut self, shape: RoofShape) {
        if !self.roof_area_can_pick_shape() {
            return;
        }
        let Some((storey_idx, eave_z)) = self.roof_base_and_eave(shape == RoofShape::Flat) else {
            return;
        };
        match shape {
            RoofShape::Flat | RoofShape::FollowCeiling => {
                let _ = self.commit_roof_area(storey_idx, shape, 0.0, eave_z, Vec::new());
            }
            RoofShape::Gable | RoofShape::Hip => {
                self.pending_roof_area = Some(PendingRoofArea::DrawRidge {
                    shape,
                    storey_idx,
                    ridges: Vec::new(),
                });
            }
        }
    }

    /// A ridge stroke (world/metre coords) for the grouped area.
    ///
    /// A GABLE ridge is orthogonal by construction: axis-fit, extend to the
    /// area's full span (vertical end walls), and SNAP onto a footprint edge
    /// it nearly sits on (a rough eave-line becomes a true mono-pitch); then
    /// the PITCH question opens.
    ///
    /// A HIP ridge is tidied by [`snap_hip_ridge`]: drawn near-orthogonal it
    /// straightens + edge-snaps like a gable's, but drawn clearly DIAGONAL (a
    /// hip's corner ridge) it keeps its direction as drawn and instead snaps
    /// its endpoints onto nearby footprint corners / already-drawn ridge ends.
    /// The hip COLLECTS the ridge and awaits more strokes (an L-shaped hip has
    /// several) until [`finish_roof_area_ridges`](Self::finish_roof_area_ridges).
    ///
    /// The apex height is computed from pitch at assign time. Returns a
    /// user-facing error if the stroke isn't line-like or the storey has no
    /// ceiling height.
    ///
    /// [`snap_hip_ridge`]: ber_model::ber::roof::snap_hip_ridge
    pub fn apply_roof_area_ridge(&mut self, world_points: &[(f32, f32)]) -> Result<(), String> {
        // Drain the raw stroke buffer (mirror the exposure-carve handler): the
        // engine EXTENDS an in-flight stroke when the next StartStroke lands near
        // a leftover endpoint, so a second ridge stroke would otherwise append
        // to this one's stale points.
        let _ = self.sketch_engine.take_stroke_raw();
        let Some(PendingRoofArea::DrawRidge {
            shape,
            storey_idx,
            ridges,
        }) = self.pending_roof_area.as_ref()
        else {
            return Ok(());
        };
        let (shape, storey_idx) = (*shape, *storey_idx);
        let prior_endpoints: Vec<Point2f64> = ridges.iter().flat_map(|(a, b)| [*a, *b]).collect();
        let pts: Vec<Point2f32> = world_points
            .iter()
            .map(|(x, y)| Point2f32::new(*x, *y))
            .collect();
        let not_a_line =
            || "That stroke isn't a straight line — draw one straight stroke along the ridge.";
        let eave_z = self
            .roof_base_and_eave(false)
            .map(|(_, z)| z)
            .ok_or("Set this storey's ceiling height before drawing its roof.")?;
        let footprint = self.roof_selected_rings();
        let ridge = match shape {
            RoofShape::Hip => {
                // Free-direction fit: a hip's corner ridges are legitimately
                // diagonal — snap_hip_ridge decides orthogonal vs corner-snap.
                let (a, b) = manip_plan::fit_segment(&pts).ok_or_else(not_a_line)?;
                ber_model::ber::roof::snap_hip_ridge(
                    (
                        Point2f64::new(a.x as f64, a.y as f64),
                        Point2f64::new(b.x as f64, b.y as f64),
                    ),
                    &footprint,
                    &prior_endpoints,
                    RIDGE_EDGE_SNAP_M,
                )
            }
            _ => {
                let (axis, coord, _) = manip_plan::fit_axis_segment(&pts).ok_or_else(not_a_line)?;
                let ((minx, miny), (maxx, maxy)) = self
                    .roof_selected_bbox()
                    .ok_or("No grouped roof area to place the ridge in.")?;
                // `axis == X` ⇒ ridge runs along Y at x = coord, slopes fall
                // in X; `axis == Y` ⇒ ridge runs along X at y = coord. A gable
                // spans the full area along the ridge (vertical end walls).
                let (a, b) = match axis {
                    ParameterAxis::X => (
                        Point2f64::new(coord as f64, miny),
                        Point2f64::new(coord as f64, maxy),
                    ),
                    ParameterAxis::Y => (
                        Point2f64::new(minx, coord as f64),
                        Point2f64::new(maxx, coord as f64),
                    ),
                };
                // Drawn nearly on an eave edge ⇒ ON it (mono-pitch).
                ber_model::ber::roof::snap_ridge_to_edge((a, b), &footprint, RIDGE_EDGE_SNAP_M)
            }
        };
        match shape {
            RoofShape::Hip => {
                if let Some(PendingRoofArea::DrawRidge { ridges, .. }) =
                    self.pending_roof_area.as_mut()
                {
                    ridges.push(ridge);
                }
            }
            _ => {
                self.pending_roof_area = Some(PendingRoofArea::PickPitch {
                    storey_idx,
                    shape,
                    ridges: vec![ridge],
                    eave_z,
                });
            }
        }
        Ok(())
    }

    /// HIP: done drawing ridges — move the collected set to the PITCH question.
    /// Errors if no ridge has been drawn yet; no-op outside the hip ridge stage.
    pub fn finish_roof_area_ridges(&mut self) -> Result<(), String> {
        let Some(PendingRoofArea::DrawRidge {
            shape: RoofShape::Hip,
            storey_idx,
            ridges,
        }) = self.pending_roof_area.as_ref()
        else {
            return Ok(());
        };
        if ridges.is_empty() {
            return Err("Draw at least one ridge before finishing.".to_string());
        }
        let (storey_idx, ridges) = (*storey_idx, ridges.clone());
        let eave_z = self
            .roof_base_and_eave(false)
            .map(|(_, z)| z)
            .ok_or("Set this storey's ceiling height before drawing its roof.")?;
        self.pending_roof_area = Some(PendingRoofArea::PickPitch {
            storey_idx,
            shape: RoofShape::Hip,
            ridges,
            eave_z,
        });
        Ok(())
    }

    /// Assign the PITCH (degrees) to the placed ridge(s) and commit the roof area
    /// to `Ber.roof` (SEPARATE from the ceiling — no apexes, the ceiling is
    /// untouched). The 3-D roof planes are derived from shape + pitch + ridges +
    /// footprint on resolve. Pitch must be in `(0, 90)`. No-op unless the pitch
    /// stage is up.
    pub fn assign_roof_area_pitch(&mut self, pitch_deg: f32) -> Result<(), String> {
        let Some(PendingRoofArea::PickPitch {
            storey_idx,
            shape,
            ridges,
            eave_z,
        }) = self.pending_roof_area.as_ref()
        else {
            return Ok(());
        };
        let (storey_idx, shape, ridges, eave_z) = (*storey_idx, *shape, ridges.clone(), *eave_z);
        if !(pitch_deg > 0.0 && pitch_deg < 90.0) {
            return Err("Enter a roof pitch between 0 and 90 degrees.".to_string());
        }
        self.commit_roof_area(storey_idx, shape, pitch_deg, eave_z, ridges)
    }

    /// Discard the in-flight per-area roof drawing (keeps the grouping).
    pub fn cancel_roof_area(&mut self) {
        self.pending_roof_area = None;
    }
}
