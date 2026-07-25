//! Phase-1 ROOF step — the classify-then-generate flow. The roof is only
//! relevant at the TOP storey of each location, so — like the
//! floor/ceiling-exposure tools — it's gated behind a "have you added all the
//! storeys?" prompt, then runs two stages over the whole-house context:
//!
//! * **Classify** — pick every ceiling surface that IS the roof skin (flat
//!   roofs + the sloped ceilings of converted attics). Sloped roofable
//!   regions arrive preselected; confirming commits one `FollowCeiling` area
//!   over the sloped picks and a `Flat` area per storey for the flat picks.
//! * **Section** — group the remaining uncovered areas one roof at a time,
//!   pick a style (gable/hip ends), and confirm a GENERATED candidate — the
//!   ridge/apex geometry comes from `ber_model::ber::roof::generate_roof`,
//!   never hand-drawn.
use super::*;
use ber_model::ber::roof::{
    adopted_ceiling_apexes, default_ridge_axis, generate_roof, inherited_pitch_deg, GeneratedRoof,
    RidgeAxis, RoofArea, RoofEnd,
};
use ber_model::ber::structural::{CeilingApex, CeilingExposure, SurfaceKind};
use domain::points::Point2f64;
use std::collections::{BTreeMap, BTreeSet};

// The persisted roof-shape enum; re-exported (and again from `mod.rs`) so the UI
// keeps reaching it via `crate::controller::app_controller::RoofShape`.
pub use ber_model::ber::roof::RoofShape;

/// A selection of roofable ceiling regions, keyed by storey + centroid.
type RoofSelection = HashSet<(usize, document::CentroidKey)>;

/// The roof step's stage machine (session-only).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoofStage {
    /// "Have you added all storeys?" — the entry gate.
    Gate,
    /// Step A: pick every ceiling surface the roof FOLLOWS (flat roofs +
    /// sloped ceilings under the skin). Sloped roofable regions PRESELECTED.
    Classify,
    /// Step B: select uncovered areas, pick a style, preview generated
    /// candidates, confirm.
    Section,
}

/// Which end combination(s) the assessor asked for over the grouped section.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoofSectionStyle {
    /// Gable at both ends — one candidate.
    GableBoth,
    /// Hip at both ends — one candidate.
    HipBoth,
    /// Hip at ONE end (gable at the other) — two candidates, one per end.
    HipOne,
    /// Every end combination — four candidates to browse.
    Suggest,
    /// Flat roof — commits immediately, no candidates.
    Flat,
}

/// The in-flight Stage-B section: the grouped selection's base storey + eave,
/// the picked style, and the generated candidates the assessor is browsing.
/// Session-only, cleared on cancel / confirm / mode exit.
#[derive(Clone, Debug)]
pub struct PendingRoofSection {
    /// Base storey (`roof_base_and_eave` over the selection).
    pub storey_idx: usize,
    pub eave_z: f64,
    pub style: RoofSectionStyle,
    pub axis: RidgeAxis,
    pub pitch_deg: f32,
    /// The pitch was inherited from an adjacent sloped ceiling (not typed).
    pub pitch_inherited: bool,
    /// True → the pitch prompt is up, no candidates yet.
    pub needs_pitch: bool,
    pub candidates: Vec<GeneratedRoof>,
    pub selected: usize,
}

/// UI-facing snapshot of the pending Stage-B section (owned data only).
pub struct PendingRoofSectionView {
    /// The selected candidate's ridge for the dashed overlay (empty while the
    /// pitch prompt is up).
    pub ridge_lines: Vec<((f64, f64), (f64, f64))>,
    /// e.g. "Gable both ends", "Hip west end", "Pyramid".
    pub candidate_label: String,
    pub candidate_idx: usize,
    pub candidate_count: usize,
    pub pitch_deg: f32,
    pub pitch_inherited: bool,
    pub needs_pitch: bool,
    /// "ridge east–west" / "ridge north–south" for the Flip button.
    pub axis_label: &'static str,
}

/// One storey's footprint for the roof-context overlay: its outline ring (world
/// metres) + storey index. The UI draws these bottom→top so the TOP storey shows
/// at each location.
pub struct RoofContextRegion {
    pub storey_idx: usize,
    pub ring: Vec<(f64, f64)>,
}

/// One selectable ceiling region in the roof step: its plan ring (world metres,
/// no closing duplicate), the owning storey, whether it's currently selected,
/// and whether a committed roof area already covers it (dimmed, and not
/// selectable in the Section stage).
pub struct RoofRegionView {
    pub storey_idx: usize,
    pub ring: Vec<(f64, f64)>,
    pub selected: bool,
    pub covered: bool,
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

    /// The roof step's current stage — `None` when the step is off, `Gate`
    /// while the storey-count prompt is up.
    pub fn roof_stage(&self) -> Option<RoofStage> {
        if !self.roof_mode {
            return None;
        }
        if self.roof_gate_pending {
            return Some(RoofStage::Gate);
        }
        Some(self.roof_stage)
    }

    /// Enter / leave the roof step. On enter the storey-count GATE is raised (the
    /// roof depends on the full stack to know the top storey everywhere).
    /// Mutually exclusive with the other Mapping modes.
    pub fn set_roof_mode(&mut self, on: bool) {
        // Leaving the outline draw tool: drop any un-decided re-draw stroke
        // (a stray second outline) so it doesn't linger on the canvas. (n3)
        self.pending_redraw = None;
        self.roof_mode = on;
        self.roof_clear_confirm = false;
        self.roof_selected.clear();
        self.pending_roof_section = None;
        self.roof_stage = RoofStage::Gate;
        if on {
            // The roof step is a PLAN task: its takeover replaces the canvas
            // with the whole-house plan, and its tap input only routes in 2D.
            // Entering from the 3D view would show the overlay with DEAD
            // input — drop back to the 2D plan first.
            self.set_sketch_mode(SketchMode::Storey2D);
            self.roof_gate_pending = true;
            self.set_floor_exposure_mode(false);
            self.set_ceiling_exposure_mode(false);
            self.set_exposure_mode(false);
            self.set_openings_mode(false);
            self.set_living_area_mode(false);
            self.set_porch_mode(false);
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
            self.continuation_tool = None;
            self.pending_ceiling_continuation = None;
            self.reset_ceiling_confirm = false;
        } else {
            self.roof_gate_pending = false;
        }
    }

    /// Gate answer YES — every storey is added. Lower the gate, bring the
    /// resolved record current so the context reads live footprints, then
    /// open the right stage: CLASSIFY (with the sloped roofable regions
    /// preselected) when no roof exists yet, SECTION when a previous visit
    /// already classified/committed areas.
    pub fn confirm_all_storeys_added_roof(&mut self) {
        self.roof_gate_pending = false;
        self.materialize();
        let empty = self
            .current_record
            .as_ref()
            .is_none_or(|r| r.data.roof.is_empty());
        if empty {
            self.roof_stage = RoofStage::Classify;
            self.preselect_sloped_roof_regions();
        } else {
            self.roof_stage = RoofStage::Section;
        }
    }

    /// Gate answer NOT-YET — leave the step so the assessor can add the rest.
    pub fn defer_storey_count_roof(&mut self) {
        self.set_roof_mode(false);
    }

    /// Seed the Classify selection: every roofable ceiling region that is
    /// SLOPED (a converted attic's rafter ceiling IS the roof skin, so the
    /// overwhelmingly common answer is "yes, it follows").
    fn preselect_sloped_roof_regions(&mut self) {
        self.roof_selected.clear();
        let Some(record) = self.current_record.as_ref() else {
            return;
        };
        let mut sel: Vec<(usize, document::CentroidKey)> = Vec::new();
        for (storey_idx, storey) in record.data.storeys.iter().enumerate() {
            for r in storey.ceilings.regions.iter().filter(|r| {
                !matches!(
                    r.exposure,
                    CeilingExposure::Party | CeilingExposure::InterStorey
                ) && r.flat_z().is_none()
            }) {
                sel.push((storey_idx, document::centroid_key(r.centroid())));
            }
        }
        self.roof_selected.extend(sel);
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
    /// get no roof and drop off. `covered` marks regions already under a
    /// committed roof area (dimmed; not selectable in the Section stage).
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
                let covered = record.data.roof.covers_on(storey_idx, r.interior_point());
                out.push(RoofRegionView {
                    storey_idx,
                    ring,
                    selected,
                    covered,
                });
            }
        }
        out
    }

    /// Toggle the ceiling region under a tapped point (world metres) in/out of
    /// the current selection — hit-tested across every storey, smallest-area
    /// region wins. In the SECTION stage regions already covered by a committed
    /// roof area are not selectable. No-op while the gate is up or while a
    /// pending section's candidates are being browsed.
    pub fn roof_region_tap(&mut self, world_x: f64, world_y: f64, tol: f64) {
        if self.roof_gate_pending || self.pending_roof_section.is_some() {
            return;
        }
        let in_section = self.roof_stage == RoofStage::Section;
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
                        && !(in_section && record.data.roof.covers_on(idx, r.interior_point()))
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
            // `roof_base_and_eave`).
            let sel = (idx, key);
            if !self.roof_selected.insert(sel) {
                self.roof_selected.remove(&sel);
            }
        }
    }

    // ── Selection-derived helpers (over an explicit subset, so Classify can
    //    commit its sloped and flat picks separately) ────────────────────────

    /// The sorted, de-duplicated storey indices of a subset — a committed
    /// area's `covered_storeys`.
    fn roof_subset_storeys(subset: &RoofSelection) -> Vec<usize> {
        subset
            .iter()
            .map(|(idx, _)| *idx)
            .collect::<BTreeSet<usize>>()
            .into_iter()
            .collect()
    }

    /// A subset's ceiling-area rings (plan, world metres, no closing
    /// duplicate) across EVERY storey, UNIONED into contiguous pieces — the
    /// roof area's covered footprint. Merged so the grouped selection is ONE
    /// roof: the generated ridge measures against the COMBINED boundary (not
    /// the internal seams between grouped areas), and the committed footprint
    /// fans as one surface.
    fn roof_subset_rings(&self, subset: &RoofSelection) -> Vec<Vec<Point2f64>> {
        let Some(record) = self.current_record.as_ref() else {
            return Vec::new();
        };
        let mut rings: Vec<Vec<Point2f64>> = Vec::new();
        for (storey_idx, storey) in record.data.storeys.iter().enumerate() {
            for r in
                storey.ceilings.regions.iter().filter(|r| {
                    subset.contains(&(storey_idx, document::centroid_key(r.centroid())))
                })
            {
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

    /// A subset's BASE storey + the eave height LOCAL to its floor datum.
    /// Every region contributes a world-z eave candidate: its resolved flat
    /// plane (`datum + flat_z`) or — for a sloped region (a converted attic's
    /// rafter ceiling) — the storey's default ceiling height. A SLOPED roof
    /// takes the LOWEST candidate (`highest = false`): the merged footprint's
    /// boundary sits on the lowest covered ceiling — an attic-spanning gable
    /// rises from the ceiling of the storey BELOW. A FLAT roof takes the
    /// HIGHEST: it lies on the highest ceiling piece it covers (a roof over a
    /// stepped ceiling sits on the raised side, not inside it). The base
    /// storey is the winning candidate's owner, so the stored storey-local
    /// `eave_z` renders at the right world height.
    fn roof_subset_base_and_eave(
        &self,
        subset: &RoofSelection,
        highest: bool,
    ) -> Option<(usize, f64)> {
        let record = self.current_record.as_ref()?;
        let datums = record.data.storey_floor_datums();
        let mut best: Option<(usize, f64, f64)> = None; // (storey, world_z, datum)
        for storey_idx in Self::roof_subset_storeys(subset) {
            let Some(storey) = record.data.storeys.get(storey_idx) else {
                continue;
            };
            let datum = datums.get(storey_idx).copied().unwrap_or(0.0);
            // Each covered region contributes its z EXTREME for the mode: a
            // flat region its level, a SLOPED one its ridge top (highest) or
            // its eave bottom (lowest). The old `flat_z()`-only read skipped
            // sloped regions entirely and fell back to the storey ceiling
            // height — which put a FLAT roof over a sloped ceiling BELOW the
            // ceiling it covers (field report 803471b7: the flat plane sliced
            // through the gable). Rule: the roof sits at least at the top of
            // every ceiling it covers.
            let local = storey
                .ceilings
                .regions
                .iter()
                .filter(|r| subset.contains(&(storey_idx, document::centroid_key(r.centroid()))))
                .filter_map(|r| r.z_range().map(|(lo, hi)| if highest { hi } else { lo }))
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

    /// [`roof_subset_storeys`](Self::roof_subset_storeys) over the current selection.
    fn roof_selected_storeys(&self) -> Vec<usize> {
        Self::roof_subset_storeys(&self.roof_selected)
    }

    /// [`roof_subset_rings`](Self::roof_subset_rings) over the current selection.
    fn roof_selected_rings(&self) -> Vec<Vec<Point2f64>> {
        self.roof_subset_rings(&self.roof_selected)
    }

    /// [`roof_subset_base_and_eave`](Self::roof_subset_base_and_eave) over the
    /// current selection.
    fn roof_base_and_eave(&self, highest: bool) -> Option<(usize, f64)> {
        self.roof_subset_base_and_eave(&self.roof_selected, highest)
    }

    // ── Stage A: Classify ───────────────────────────────────────────────────

    /// Commit the Classify answer: the selected SLOPED regions (across all
    /// storeys) become ONE `FollowCeiling` area (the roof IS those rafter
    /// ceilings), the selected FLAT regions one `Flat` area PER STOREY. An
    /// empty selection is a valid answer (nothing follows the ceiling).
    /// Advances to the SECTION stage.
    pub fn confirm_roof_follow(&mut self) -> Result<(), String> {
        if self.roof_stage() != Some(RoofStage::Classify) {
            return Ok(());
        }
        // Partition the selection into sloped / flat-per-storey subsets.
        let (sloped, flat_by_storey) = {
            let Some(record) = self.current_record.as_ref() else {
                return Err("No record to add the roof to.".to_string());
            };
            let mut sloped: RoofSelection = HashSet::new();
            let mut flat_by_storey: BTreeMap<usize, RoofSelection> = BTreeMap::new();
            for (storey_idx, storey) in record.data.storeys.iter().enumerate() {
                for r in &storey.ceilings.regions {
                    let key = (storey_idx, document::centroid_key(r.centroid()));
                    if !self.roof_selected.contains(&key) {
                        continue;
                    }
                    if r.flat_z().is_none() {
                        sloped.insert(key);
                    } else {
                        flat_by_storey.entry(storey_idx).or_default().insert(key);
                    }
                }
            }
            (sloped, flat_by_storey)
        };
        // Resolve EVERY commit's base/eave up front so a failure commits
        // nothing — after a partial commit, re-confirming would push duplicate
        // areas (the selection and stage would still be live).
        let sloped_base = if sloped.is_empty() {
            None
        } else {
            Some(
                self.roof_subset_base_and_eave(&sloped, false)
                    .ok_or("Set every storey's ceiling height before adding its roof.")?,
            )
        };
        let flat_commits: Vec<(usize, RoofSelection, (usize, f64))> = flat_by_storey
            .iter()
            .map(|(storey_idx, subset)| {
                self.roof_subset_base_and_eave(subset, true)
                    .map(|be| (*storey_idx, subset.clone(), be))
                    .ok_or("Set every storey's ceiling height before adding its roof.".to_string())
            })
            .collect::<Result<_, _>>()?;
        if let Some((storey_idx, eave_z)) = sloped_base {
            let covered_storeys = Self::roof_subset_storeys(&sloped);
            let footprint = self.roof_subset_rings(&sloped);
            // Adopt the covered storeys' ceiling slope (converted attic) —
            // apex z re-based onto the BASE storey's datum in ber_model.
            let apexes = {
                let record = self.current_record.as_ref().expect("record checked above");
                adopted_ceiling_apexes(&record.data, storey_idx, &covered_storeys)
            };
            self.commit_roof_area(
                storey_idx,
                covered_storeys,
                footprint,
                RoofShape::FollowCeiling,
                0.0,
                eave_z,
                None,
                apexes,
                None,
            )?;
        }
        for (storey_idx, subset, (base_idx, eave_z)) in flat_commits {
            let footprint = self.roof_subset_rings(&subset);
            self.commit_roof_area(
                base_idx,
                vec![storey_idx],
                footprint,
                RoofShape::Flat,
                0.0,
                eave_z,
                None,
                Vec::new(),
                None,
            )?;
        }
        self.roof_selected.clear();
        self.roof_stage = RoofStage::Section;
        Ok(())
    }

    // ── Stage B: Section ────────────────────────────────────────────────────

    /// The end combinations a style asks for, in axis order `[low, high]`.
    fn roof_section_end_sets(style: RoofSectionStyle) -> Vec<[RoofEnd; 2]> {
        use RoofEnd::{Gable, Hip};
        match style {
            RoofSectionStyle::GableBoth => vec![[Gable, Gable]],
            RoofSectionStyle::HipBoth => vec![[Hip, Hip]],
            RoofSectionStyle::HipOne => vec![[Hip, Gable], [Gable, Hip]],
            RoofSectionStyle::Suggest => {
                vec![[Gable, Gable], [Hip, Hip], [Hip, Gable], [Gable, Hip]]
            }
            RoofSectionStyle::Flat => Vec::new(),
        }
    }

    /// (Re)generate the pending section's candidates from the current
    /// selection footprint + pitch + axis. Errors (and leaves the candidate
    /// list empty) when NO end combination fits a ridge over the footprint.
    fn regenerate_roof_section(&mut self) -> Result<(), String> {
        let footprint = self.roof_selected_rings();
        let Some(p) = self.pending_roof_section.as_mut() else {
            return Ok(());
        };
        // SUGGEST browses the whole space — every end combination along BOTH
        // axes ("offer the possibilities, let the assessor pick the closest to
        // the truth"); a constrained style keeps the chosen axis. Geometric
        // duplicates (e.g. a square's two pyramids) are dropped.
        let axes = if p.style == RoofSectionStyle::Suggest {
            vec![p.axis, p.axis.flipped()]
        } else {
            vec![p.axis]
        };
        let mut candidates: Vec<GeneratedRoof> = Vec::new();
        for axis in axes {
            for ends in Self::roof_section_end_sets(p.style) {
                if let Some(g) = generate_roof(&footprint, axis, ends, p.pitch_deg, p.eave_z) {
                    if !candidates
                        .iter()
                        .any(|c| c.ridge == g.ridge && c.apexes == g.apexes)
                    {
                        candidates.push(g);
                    }
                }
            }
        }
        if candidates.is_empty() {
            p.candidates.clear();
            p.selected = 0;
            return Err("Couldn't fit a ridge over that area — check the selection.".to_string());
        }
        p.selected = p.selected.min(candidates.len() - 1);
        p.candidates = candidates;
        Ok(())
    }

    /// Pick the style for the grouped section. `Flat` commits immediately (it
    /// lies on the HIGHEST grouped ceiling piece, like Classify's flat areas).
    /// The sloped styles open the candidate preview: the ridge axis defaults
    /// to the footprint's longer way, and the pitch is INHERITED from the
    /// sloped ceiling sharing the longest boundary with the selection — when
    /// no sloped ceiling touches it, the pitch prompt opens instead.
    /// No-op without a selection.
    pub fn pick_roof_section_style(&mut self, style: RoofSectionStyle) -> Result<(), String> {
        if self.roof_stage() != Some(RoofStage::Section)
            || self.pending_roof_section.is_some()
            || self.roof_selected.is_empty()
        {
            return Ok(());
        }
        if style == RoofSectionStyle::Flat {
            let (storey_idx, eave_z) = self
                .roof_base_and_eave(true)
                .ok_or("Set every storey's ceiling height before adding its roof.")?;
            let covered_storeys = self.roof_selected_storeys();
            let footprint = self.roof_selected_rings();
            self.commit_roof_area(
                storey_idx,
                covered_storeys,
                footprint,
                RoofShape::Flat,
                0.0,
                eave_z,
                None,
                Vec::new(),
                None,
            )?;
            self.roof_selected.clear();
            return Ok(());
        }
        let (storey_idx, eave_z) = self
            .roof_base_and_eave(false)
            .ok_or("Set every storey's ceiling height before adding its roof.")?;
        let footprint = self.roof_selected_rings();
        let axis = default_ridge_axis(&footprint);
        let record = self
            .current_record
            .as_ref()
            .ok_or("No record to add the roof to.")?;
        // Inherit from ANY storey's adjacent sloped ceiling, not just the
        // selection's own storeys: the flagship case is an attic conversion —
        // the rafter ceiling lives on the ATTIC storey (already classified
        // follow-ceiling, so never selectable here) while the section is the
        // flat band on the storey BELOW. The model filters by plan adjacency,
        // so scanning the whole stack is safe.
        let all_storeys: Vec<usize> = (0..record.data.storeys.len()).collect();
        let inherited = inherited_pitch_deg(&record.data, &all_storeys, &footprint);
        self.pending_roof_section = Some(PendingRoofSection {
            storey_idx,
            eave_z,
            style,
            axis,
            pitch_deg: inherited.unwrap_or(0.0),
            pitch_inherited: inherited.is_some(),
            needs_pitch: inherited.is_none(),
            candidates: Vec::new(),
            selected: 0,
        });
        if inherited.is_some() {
            if let Err(e) = self.regenerate_roof_section() {
                self.pending_roof_section = None;
                return Err(e);
            }
        }
        Ok(())
    }

    /// Set the section's pitch (degrees) — the answer to the pitch prompt, and
    /// the OVERRIDE for an inherited pitch — then (re)generate the candidates.
    /// Pitch must be in `(0, 90)`. No-op without a pending section.
    pub fn assign_roof_section_pitch(&mut self, pitch_deg: f32) -> Result<(), String> {
        let Some(p) = self.pending_roof_section.as_mut() else {
            return Ok(());
        };
        if !(pitch_deg > 0.0 && pitch_deg < 90.0) {
            return Err("Enter a roof pitch between 0 and 90 degrees.".to_string());
        }
        p.pitch_deg = pitch_deg;
        p.pitch_inherited = false;
        self.regenerate_roof_section()?;
        if let Some(p) = self.pending_roof_section.as_mut() {
            p.needs_pitch = false;
        }
        Ok(())
    }

    /// Reopen the pitch prompt over a pending section ("Change pitch" — the
    /// override path for an inherited pitch). The candidates stay until the
    /// next [`assign_roof_section_pitch`](Self::assign_roof_section_pitch)
    /// regenerates them; the view hides them while the prompt is up.
    pub fn reopen_roof_section_pitch(&mut self) {
        if let Some(p) = self.pending_roof_section.as_mut() {
            p.needs_pitch = true;
        }
    }

    /// Run the ridge the other way and regenerate the candidates. No-op while
    /// the pitch prompt is up (no candidates to regenerate yet).
    pub fn flip_roof_section_axis(&mut self) {
        let Some(p) = self.pending_roof_section.as_mut() else {
            return;
        };
        if p.needs_pitch {
            return;
        }
        p.axis = p.axis.flipped();
        // Flipping a fittable footprint stays fittable (the generator only
        // fails on degenerate footprints, which the first generation caught).
        let _ = self.regenerate_roof_section();
    }

    /// Cycle to the next generated candidate.
    pub fn next_roof_section_candidate(&mut self) {
        let Some(p) = self.pending_roof_section.as_mut() else {
            return;
        };
        if !p.candidates.is_empty() {
            p.selected = (p.selected + 1) % p.candidates.len();
        }
    }

    /// Commit the selected candidate as a `RoofArea`, then clear the pending
    /// state + selection — the step stays in SECTION for the next section.
    pub fn confirm_roof_section(&mut self) -> Result<(), String> {
        let Some(p) = self.pending_roof_section.as_ref() else {
            return Ok(());
        };
        let Some(candidate) = p.candidates.get(p.selected).cloned() else {
            return Ok(());
        };
        let (storey_idx, eave_z, pitch_deg) = (p.storey_idx, p.eave_z, p.pitch_deg);
        let shape = if candidate.ends == [RoofEnd::Gable, RoofEnd::Gable] {
            RoofShape::Gable
        } else {
            RoofShape::Hip
        };
        let covered_storeys = self.roof_selected_storeys();
        let footprint = self.roof_selected_rings();
        self.commit_roof_area(
            storey_idx,
            covered_storeys,
            footprint,
            shape,
            pitch_deg,
            eave_z,
            Some(candidate.ridge),
            candidate.apexes,
            Some(candidate.ends),
        )?;
        self.pending_roof_section = None;
        self.roof_selected.clear();
        Ok(())
    }

    /// Drop the pending section (keeps the selection, so another style can be
    /// picked over the same grouping).
    pub fn cancel_roof_section(&mut self) {
        self.pending_roof_section = None;
    }

    /// Raise the "wipe the whole roof?" confirm — the Start-roof-again button.
    /// [`clear_roof`](Self::clear_roof) is un-undoable (the roof is not in the
    /// history snapshot), so a mis-tap must not wipe minutes of work.
    pub fn request_clear_roof(&mut self) {
        if self.roof_stage() == Some(RoofStage::Section) {
            self.roof_clear_confirm = true;
        }
    }

    /// Dismiss the clear-roof confirm without wiping anything.
    pub fn cancel_clear_roof(&mut self) {
        self.roof_clear_confirm = false;
    }

    /// Whether the clear-roof confirm prompt is up.
    pub fn roof_clear_confirm_pending(&self) -> bool {
        self.roof_clear_confirm
    }

    /// Wipe the whole roof and return to CLASSIFY (with the sloped
    /// preselection) — the confirmed "start the roof again" action.
    pub fn clear_roof(&mut self) -> Result<(), String> {
        self.roof_clear_confirm = false;
        self.pending_roof_section = None;
        self.roof_selected.clear();
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };
        record.data.roof.areas.clear();
        // Roof mutation contract (see `derive_roof_voids`): re-resolve the
        // whole stack so stale void splits/stamps from the deleted areas
        // cannot survive.
        self.refresh_all_storeys();
        self.mark_dirty()?;
        self.roof_stage = RoofStage::Classify;
        self.preselect_sloped_roof_regions();
        Ok(())
    }

    /// UI-facing snapshot of the pending Stage-B section — `None` when no
    /// section is in flight.
    pub fn pending_roof_section_view(&self) -> Option<PendingRoofSectionView> {
        let p = self.pending_roof_section.as_ref()?;
        let candidate = (!p.needs_pitch)
            .then(|| p.candidates.get(p.selected))
            .flatten();
        let ridge_lines = candidate
            .map(|c| {
                let (a, b) = c.ridge;
                vec![((a.x, a.y), (b.x, b.y))]
            })
            .unwrap_or_default();
        // End names read off the CANDIDATE's axis (Suggest browses both axes,
        // so a candidate may run the other way): X runs west→east, Y
        // south→north. Cross-axis candidates say so in the label.
        let candidate_label = match candidate {
            None => String::new(),
            // Hip insets met: the ridge collapsed to a single peak.
            Some(c) if c.apexes.len() == 1 => "Pyramid".to_string(),
            Some(c) => {
                let (low, high) = match c.axis {
                    RidgeAxis::X => ("west", "east"),
                    RidgeAxis::Y => ("south", "north"),
                };
                let mut label = match c.ends {
                    [RoofEnd::Gable, RoofEnd::Gable] => "Gable both ends".to_string(),
                    [RoofEnd::Hip, RoofEnd::Hip] => "Hip both ends".to_string(),
                    [RoofEnd::Hip, RoofEnd::Gable] => format!("Hip {low} end"),
                    [RoofEnd::Gable, RoofEnd::Hip] => format!("Hip {high} end"),
                };
                if c.axis != p.axis {
                    label.push_str(match c.axis {
                        RidgeAxis::X => ", ridge east–west",
                        RidgeAxis::Y => ", ridge north–south",
                    });
                }
                label
            }
        };
        Some(PendingRoofSectionView {
            ridge_lines,
            candidate_label,
            candidate_idx: p.selected,
            candidate_count: if p.needs_pitch { 0 } else { p.candidates.len() },
            pitch_deg: p.pitch_deg,
            pitch_inherited: p.pitch_inherited,
            needs_pitch: p.needs_pitch,
            axis_label: match p.axis {
                RidgeAxis::X => "ridge east–west",
                RidgeAxis::Y => "ridge north–south",
            },
        })
    }

    /// Push a finished roof area onto `Ber.roof` (SEPARATE from the ceiling —
    /// it does NOT touch the ceiling apexes). `storey_idx` is the BASE storey;
    /// `covered_storeys` records every grouped storey. Every roof MUTATION
    /// re-resolves the whole storey stack: `derive_roof_voids` stamps covered
    /// storeys' walls with the paintable `SemiExposed` and has NO clear pass,
    /// so only a full re-resolve from durable inputs guarantees stale
    /// splits/stamps from superseded roof geometry cannot survive.
    #[allow(clippy::too_many_arguments)]
    fn commit_roof_area(
        &mut self,
        storey_idx: usize,
        covered_storeys: Vec<usize>,
        footprint: Vec<Vec<Point2f64>>,
        shape: RoofShape,
        pitch_deg: f32,
        eave_z: f64,
        ridge: Option<(Point2f64, Point2f64)>,
        apexes: Vec<CeilingApex>,
        ends: Option<[RoofEnd; 2]>,
    ) -> Result<(), String> {
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
            ridge,
            footprint,
            apexes,
            ends,
        });
        self.refresh_all_storeys();
        self.mark_dirty()?;
        Ok(())
    }
}
