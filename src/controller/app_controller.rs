use chrono::NaiveDate;
use std::sync::mpsc::Sender;

use crate::engine::ber_repository::{BerRecord, BerRepository};
use crate::engine::dataloader::DataLoader;
use crate::engine::Db;
use datatypes::ber::ber_struct::Ber;
use datatypes::ber::common::ItemId;
use datatypes::ber::structural::{FloorRegions, Storey};
use domain::points::{Point2f32, Point2f64};
use domain::sketch::{DisplayMode, EditTools, SketchMode};
use manip_2d_plan::{
    Axis, CollinearitySuggestion, FloorPlan, ParameterAxis, ParameterId, VertexId,
};
use sketch::model::Stroke;
use sketch::SketchEngine;
use std::collections::HashSet;

/// Default storey ceiling height in meters, used as a placeholder until the UI
/// collects it from the assessor.
const DEFAULT_CEILING_HEIGHT_M: f64 = 2.4;

/// Range (in pixels) for what counts as "near-collinear, ask the user."
/// `MIN_SEPARATION` is just above the parametric layer's dedup tolerance —
/// pairs closer than this already share a coordinate expression (and are
/// skipped by the detector). Anything from there up to `MAX_SEPARATION` is
/// fair game to ask the user about; pairs farther than `MAX_SEPARATION` are
/// treated as distinct walls.
const COLLINEARITY_MIN_SEPARATION: f32 = 1.0;
const COLLINEARITY_MAX_SEPARATION: f32 = 60.0;

/// In-session state for the "is this pair of walls the same line?" review.
/// Populated by `sketch_end_stroke` and consumed by `accept_current_review` /
/// `reject_current_review`.
#[derive(Debug, Default)]
pub struct CollinearityReview {
    /// All candidate pairs the detector returned for the current floor plan.
    pub suggestions: Vec<CollinearitySuggestion>,
    /// Index of the next suggestion to consider. We walk forward, skipping
    /// any pair that's become "already determined" (yes-side via the live
    /// parametric state, no-side via the `rejections` list).
    pub cursor: usize,
    /// Pairs the user explicitly said No to. Stored as full suggestions so
    /// we have the ring indices available for transitive-rejection checking.
    /// Reused for: if (A,B) was rejected and (A,C) gets approved later
    /// (C joins A's equivalence class), then asking (B,C) is redundant.
    pub rejections: Vec<CollinearitySuggestion>,
}

/// A completed stroke that's waiting for the user to confirm what to do with it.
/// Populated when `DrawPerpWalls` finishes a stroke while a floor plan already
/// exists — the controller doesn't apply the stroke until the user picks:
/// Replace / Add-new-storey / Ignore.
#[derive(Debug, Clone)]
pub struct PendingRedraw {
    pub stroke_points: Vec<Point2f32>,
    pub closed: bool,
}

/// User-controlled view transform on the sketch canvas, on top of the
/// auto-fit. `zoom = 1.0` and zero offset = pure auto-fit. The assessor uses
/// this to zoom out (giving room to draw a disjoint section) or pan around.
#[derive(Debug, Clone, Copy)]
pub struct ViewTransform {
    pub zoom: f32,
    pub offset_x: f32,
    pub offset_y: f32,
}

impl Default for ViewTransform {
    fn default() -> Self {
        Self {
            zoom: 1.0,
            offset_x: 0.0,
            offset_y: 0.0,
        }
    }
}

/// A single "what's the length of this wall?" question to put to the assessor.
#[derive(Debug, Clone)]
pub struct DimensionPrompt {
    /// Stable identifier of the wall's starting vertex. Survives reflows so
    /// the prompt can be carried across other operations without going stale.
    pub wall_start_vertex: VertexId,
    /// The currently-unknown parameter this wall constrains. Setting it from
    /// the user's entered length is what "applying the dimension" does.
    pub controlled_param: ParameterId,
}

/// World-coord endpoints of one wall.
#[derive(Debug, Clone, Copy)]
pub struct WallPositions {
    pub wall_a: (f64, f64),
    pub wall_b: (f64, f64),
}

/// UI-friendly snapshot of the current dimension prompt. `walls[0]` is the
/// wall the prompt was generated for ("primary"); subsequent entries are
/// other walls whose length expression is identical — the user can measure
/// any of them and enter the same value. World coordinates here are the same
/// coord system as the rendered BER polygon; the renderer transforms via
/// its `Transform2d`.
#[derive(Debug, Clone)]
pub struct DimensionPromptUiData {
    pub walls: Vec<WallPositions>,
    /// Current length of the primary wall in the plan's current units.
    pub current_length: f32,
}

/// Tracks the user-confirmed dimensions on the floor plan. Before any
/// dimension is entered the polygon is a pixel-scale notepad sketch; once the
/// first dimension lands, the whole plan converts to real units (meters) and
/// each subsequent dimension pins another parameter.
#[derive(Debug, Default)]
pub struct DimensionState {
    /// Parameter IDs the user has explicitly entered a length for.
    pub known_params: HashSet<ParameterId>,
    /// Current "which wall do I ask about next?" prompt, or `None` when all
    /// dimensions have been supplied (or there's nothing to dimension yet).
    pub current_prompt: Option<DimensionPrompt>,
    /// True once the first dimension is entered — from that point on the
    /// plan is in real units rather than pixels.
    pub scale_set: bool,
    /// Walls the user has explicitly dimensioned, identified by the stable
    /// VertexId of each wall's starting vertex. The UI uses this to decide
    /// which length labels to render — only user-set walls show lengths;
    /// derived/un-set walls show no label.
    pub dimensioned_walls: Vec<VertexId>,
    /// Walls the user explicitly skipped (by VertexId of the wall's start).
    /// Skip is per-wall, not per-param: skipping wall A doesn't prevent the
    /// user being asked about wall B even if both would set the same param.
    /// A param controlled by a skipped wall just isn't pinned by that wall;
    /// if no other wall can set it, the param stays un-dimensioned.
    pub skipped_walls: HashSet<VertexId>,
}

pub struct AppController {
    conn: Db,
    sketch_engine: SketchEngine,
    floor_plan: FloorPlan,
    loader_engine: DataLoader,
    /// The BER record being edited. `commit_to_ber` writes through this to SQLite.
    /// `None` means no BER is loaded yet — sketches stay in-memory until a BER exists.
    current_record: Option<BerRecord>,
    /// Pending "is this collinear?" questions for the current floor plan.
    /// `None` when there's nothing to ask.
    review: Option<CollinearityReview>,
    /// A drawn-but-not-applied stroke awaiting the user's redraw decision.
    /// Only set when DrawPerpWalls finishes a stroke and there's already a plan.
    pending_redraw: Option<PendingRedraw>,
    /// Tracks which parameters the user has dimensioned + the current prompt.
    dimensioning: DimensionState,
    /// View transform layered on top of the canvas auto-fit (pan + zoom).
    view: ViewTransform,
}

impl AppController {
    pub fn new(conn: Db) -> Self {
        Self {
            conn,
            loader_engine: DataLoader::new(),
            floor_plan: FloorPlan::default(),
            sketch_engine: SketchEngine::new(),
            current_record: None,
            review: None,
            pending_redraw: None,
            dimensioning: DimensionState::default(),
            view: ViewTransform::default(),
        }
    }

    /// Snapshot of the current dimension prompt for the UI to render, or
    /// `None` when nothing is being asked.
    pub fn current_dimension_prompt(&self) -> Option<DimensionPrompt> {
        self.dimensioning.current_prompt.clone()
    }

    /// True when there's an active dimension prompt waiting for the user.
    pub fn dimensioning_active(&self) -> bool {
        self.dimensioning.current_prompt.is_some()
    }

    /// True once the user has entered at least one dimension. Until then the
    /// polygon is in pixel space; after the first dimension lands, everything
    /// converts to real units in one scale_all() pass.
    pub fn dimensioning_scale_set(&self) -> bool {
        self.dimensioning.scale_set
    }

    /// Resolved positions (ring index, edge-start vertex index) of every wall
    /// the user has explicitly dimensioned. The UI uses this to decide which
    /// wall length labels to render — derived/un-set walls show no label.
    pub fn dimensioned_walls(&self) -> Vec<(usize, usize)> {
        self.dimensioning
            .dimensioned_walls
            .iter()
            .filter_map(|vid| self.floor_plan.find_vertex(*vid))
            .collect()
    }

    /// Every wall whose length is currently *known* — meaning all parameters
    /// in its length expression are in `known_params`. This is the superset
    /// of `dimensioned_walls`: it also catches walls whose length is implied
    /// by equality (e.g. the opposite side of a rectangle, after one side
    /// was dimensioned) or by sums/differences of already-set parameters.
    /// The UI uses this to label every wall whose length can be displayed.
    pub fn walls_with_known_length(&self) -> Vec<(usize, usize)> {
        let polygons = self.floor_plan.polygons();
        let known = &self.dimensioning.known_params;
        let mut result = Vec::new();
        for (ri, ring) in polygons.iter().enumerate() {
            for i in 0..ring.len() {
                let deps = self.floor_plan.wall_param_deps(ri, i);
                // Skip walls with no param deps — pre-first-dim, those are
                // pixel-space artifacts and labeling them in meters would be
                // misleading. Once at least one dim is set, scale_set is true.
                if deps.is_empty() {
                    continue;
                }
                if deps.iter().all(|p| known.contains(p)) {
                    result.push((ri, i));
                }
            }
        }
        result
    }

    /// Resolved view of the current prompt for the UI: world-coord endpoints
    /// of the primary wall + all length-equivalent walls + current length.
    /// None when there's no active prompt.
    pub fn dimension_prompt_for_ui(&self) -> Option<DimensionPromptUiData> {
        let prompt = self.dimensioning.current_prompt.as_ref()?;
        let (ring, edge_start) = self.floor_plan.find_vertex(prompt.wall_start_vertex)?;
        let polygons = self.floor_plan.polygons();
        let current_length = self.floor_plan.wall_length(ring, edge_start)?;

        // Equivalent walls = primary + any other walls with the same length
        // expression. Ensure the primary is first.
        let mut equiv = self.floor_plan.equivalent_length_walls(ring, edge_start);
        if let Some(pos) = equiv.iter().position(|w| *w == (ring, edge_start)) {
            if pos != 0 {
                equiv.swap(0, pos);
            }
        } else {
            equiv.insert(0, (ring, edge_start));
        }

        let walls: Vec<WallPositions> = equiv
            .into_iter()
            .filter_map(|(r, e)| {
                let pts = polygons.get(r)?;
                let nr = pts.len();
                if nr < 2 {
                    return None;
                }
                let pa = pts.get(e)?;
                let pb = pts.get((e + 1) % nr)?;
                Some(WallPositions {
                    wall_a: (pa.x as f64, pa.y as f64),
                    wall_b: (pb.x as f64, pb.y as f64),
                })
            })
            .collect();

        if walls.is_empty() {
            return None;
        }
        Some(DimensionPromptUiData {
            walls,
            current_length,
        })
    }

    /// Apply the user's entered length for the current dimension prompt.
    /// First entry: scale the whole plan so this wall has that length.
    /// Subsequent entries: set the prompt's controlled parameter directly.
    pub fn apply_dimension(&mut self, value: f32) -> Result<(), String> {
        if !value.is_finite() || value <= 0.0 {
            return Err(format!(
                "dimension must be a positive number (got {})",
                value
            ));
        }
        let prompt = self
            .dimensioning
            .current_prompt
            .clone()
            .ok_or_else(|| "no active dimension prompt".to_string())?;

        let (ring, edge_start) = self
            .floor_plan
            .find_vertex(prompt.wall_start_vertex)
            .ok_or_else(|| "prompt wall vertex no longer exists".to_string())?;

        let pre_length = self.floor_plan.wall_length(ring, edge_start);
        println!(
            "dimension #{}: ring {}/edge {} (vertex_id {}, param P{}) → {} m  [was {:.3}, scale_set={}]",
            self.dimensioning.dimensioned_walls.len() + 1,
            ring,
            edge_start,
            prompt.wall_start_vertex,
            prompt.controlled_param,
            value,
            pre_length.unwrap_or(f32::NAN),
            self.dimensioning.scale_set,
        );

        // Snapshot so we can roll back if the dimension produces an invalid
        // (self-intersecting / collapsed / direction-flipped) polygon. The
        // parametric model alone doesn't enforce "all right-side vertical
        // segments sum to the spine height" — that's an emergent property
        // of the polygon being simple. Likewise it doesn't enforce that
        // edges keep going in the same direction; a parameter change can
        // push a coord past a fixed boundary and flip an edge without
        // producing a self-intersection.
        let snapshot = self.floor_plan.clone();
        let scale_set_before = self.dimensioning.scale_set;
        let known_params_before = self.dimensioning.known_params.clone();
        let dimensioned_walls_before = self.dimensioning.dimensioned_walls.clone();
        let edge_signs_before = snapshot_edge_signs(&snapshot);

        // Optimistically mark the just-set param as known up front. The repair
        // logic only scales params NOT in `known_params`; if we delayed the
        // insert until after the validate+repair, the repair would treat the
        // user's param as un-set and scale it down — both clobbering the
        // user's input and preventing convergence (the just-set wall and a
        // sibling wall scale in lockstep, never resolving their overlap).
        // On full rejection we'll roll back to `known_params_before`.
        self.dimensioning
            .known_params
            .insert(prompt.controlled_param);
        self.dimensioning
            .dimensioned_walls
            .push(prompt.wall_start_vertex);

        if !self.dimensioning.scale_set {
            // First dimension — convert the whole plan from pixels to real units
            // in one stroke by scaling every parameter and constant uniformly.
            let pixel_length = self
                .floor_plan
                .wall_length(ring, edge_start)
                .ok_or_else(|| "couldn't read wall length".to_string())?;
            if pixel_length < 1e-6 {
                return Err("wall has zero length, can't compute scale".into());
            }
            let scale = value / pixel_length;
            println!("  first dimension — scaling whole plan by {:.6}", scale);
            self.floor_plan.scale_all(scale);
            self.dimensioning.scale_set = true;
        } else {
            // Subsequent dimensions — adjust just the controlled parameter so
            // the wall takes the new length, preserving its direction along axis.
            if let Err(e) =
                self.floor_plan
                    .set_wall_length(ring, edge_start, value, prompt.controlled_param)
            {
                self.floor_plan = snapshot;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                println!("  → REJECTED (set_wall_length error): {:?}", e);
                return Err(format!("set_wall_length: {:?}", e));
            }
            println!(
                "  set_wall_length: P{} = {:?}",
                prompt.controlled_param,
                self.floor_plan.parameter_value(prompt.controlled_param)
            );
        }

        // If the apply produced an invalid polygon, try to repair it by
        // scaling the un-set parameters on the same axis as the dimensioned
        // wall — the un-set walls are still "free" (their original pixel
        // proportions were placeholders), so we can move them out of the way
        // to accommodate the user's input. E.g. when the bottom-arm height
        // of an E grows past where the middle arm currently sits, shrink the
        // middle arm's un-set y-coords toward y=0 to make room.
        //
        // Only if no factor produces a valid polygon do we reject the input
        // outright.
        if let Err(initial_err) = self.floor_plan.validate_simple() {
            let dim_axis = self
                .floor_plan
                .param_axis(prompt.controlled_param)
                .unwrap_or(ParameterAxis::Y);
            println!(
                "  polygon invalid post-apply ({:?}) — attempting repair on axis {:?}",
                initial_err, dim_axis
            );
            if !self.try_repair_unset_params_on_axis(dim_axis, &snapshot) {
                self.floor_plan = snapshot;
                self.dimensioning.scale_set = scale_set_before;
                self.dimensioning.known_params = known_params_before;
                self.dimensioning.dimensioned_walls = dimensioned_walls_before;
                println!(
                    "  → REJECTED: no repair found ({:?}). Rolled back.",
                    initial_err
                );
                return Err(format!(
                    "{} is incompatible with the shape — even shrinking the \
                     un-set walls didn't yield a simple polygon ({:?}). Try \
                     a value that's consistent with the other walls already set.",
                    value, initial_err
                ));
            }
            println!("  repair succeeded.");
        }

        // Direction-flip guard: even after a "simple" polygon validation,
        // we still need to reject cases where a parameter change pushed an
        // un-set coord past a known coord and silently inverted an edge.
        // E.g. setting the bottom arm of a T to 10m can drop its top
        // endpoint above the polygon's top edge — no edges cross, but the
        // adjacent wall now goes "the wrong way" and visually the polygon
        // has changed shape.
        let edge_signs_after = snapshot_edge_signs(&self.floor_plan);
        if let Some((r, e)) = find_flipped_edge(&edge_signs_before, &edge_signs_after) {
            self.floor_plan = snapshot;
            self.dimensioning.scale_set = scale_set_before;
            self.dimensioning.known_params = known_params_before;
            self.dimensioning.dimensioned_walls = dimensioned_walls_before;
            println!(
                "  → REJECTED: ring {}/edge {} would flip direction. Rolled back.",
                r, e
            );
            return Err(format!(
                "{} would invert ring {}/edge {} (its direction would flip — \
                 that's a shape change, not a dimension adjustment). Try a \
                 value that's consistent with the other walls already set.",
                value, r, e
            ));
        }

        // known_params already updated optimistically above.
        // Apply an aesthetic nudge if the polygon's aspect got extreme from
        // this dimension. Only touches un-set parameters; user's set values
        // stay exact.
        self.aesthetic_rescale_if_needed();
        self.commit_to_ber()?;
        let post_length = self.floor_plan.wall_length(ring, edge_start);
        println!(
            "  ✓ accepted. wall now {:.3} m. known params: {:?}. dimensioned walls: {}/{}",
            post_length.unwrap_or(f32::NAN),
            {
                let mut k: Vec<_> = self.dimensioning.known_params.iter().copied().collect();
                k.sort();
                k
            },
            self.dimensioning.dimensioned_walls.len(),
            self.floor_plan.all_active_params().len(),
        );
        self.compute_next_dimension_prompt();
        Ok(())
    }

    /// Try to repair an invalid polygon by redistributing un-set parameters
    /// on the given axis. Three strategies tried in order:
    ///
    /// 1. **Distribute**: evenly between the bounding known coords. Works when
    ///    un-set params naturally sit between the bounds — e.g. for an E,
    ///    the top arms fit between the top constant and the user-set bottom
    ///    arm's top.
    ///
    /// 2. **Preserve offsets**: keep each un-set param's pre-apply distance
    ///    from the lower bound. Works when the un-set params extend
    ///    *outside* the bounds — e.g. a T whose shaft was dimensioned, the
    ///    un-set top of the polygon needs to move *above* V0's constant.
    ///
    /// 3. **Uniform scale**: scale all un-set params by a decreasing factor.
    ///    Last resort, handles a wide variety of cases at the cost of
    ///    visually distorting the polygon.
    ///
    /// Returns `true` if any strategy produced a valid simple polygon.
    /// Direction-flip rejection happens at the caller (apply_dimension).
    fn try_repair_unset_params_on_axis(
        &mut self,
        axis: ParameterAxis,
        snapshot: &FloorPlan,
    ) -> bool {
        let post_apply = self.floor_plan.clone();

        // Strategy 1: distribute between bounds.
        if self.try_linear_remap_ring0(axis) {
            return true;
        }
        self.floor_plan = post_apply.clone();

        // Strategy 2: preserve pre-apply offsets from the lower bound.
        if self.try_linear_remap_preserve_offsets(axis, snapshot) {
            return true;
        }
        self.floor_plan = post_apply.clone();

        // Strategy 3: uniform scaling.
        const FACTORS: &[f32] = &[0.9, 0.75, 0.55, 0.35, 0.2, 0.1];
        for &factor in FACTORS {
            self.floor_plan = post_apply.clone();
            let active = self.floor_plan.all_active_params();
            for p in active {
                if self.dimensioning.known_params.contains(&p) {
                    continue;
                }
                if self.floor_plan.param_axis(p) == Some(axis) {
                    self.floor_plan.scale_parameter(p, factor);
                }
            }
            if self.floor_plan.validate_simple().is_ok() {
                return true;
            }
        }

        self.floor_plan = post_apply;
        false
    }

    /// Repair strategy 2: for each un-set parameter, set its new value to
    /// `new_lower + (pre_value - pre_lower)` where `new_lower` is the
    /// post-apply coord of the vertex just before the run and `pre_lower` /
    /// `pre_value` are from the pre-apply snapshot. This preserves each
    /// un-set param's distance from the "anchor" vertex preceding it in the
    /// polygon's traversal — the polygon's shape relative to that anchor is
    /// unchanged, just translated to accommodate the new user-set value.
    fn try_linear_remap_preserve_offsets(
        &mut self,
        axis: ParameterAxis,
        snapshot: &FloorPlan,
    ) -> bool {
        let polygons = self.floor_plan.polygons();
        let snap_polys = snapshot.polygons();
        let Some(ring) = polygons.first() else {
            return false;
        };
        let Some(snap_ring) = snap_polys.first() else {
            return false;
        };
        let n = ring.len();
        if n < 4 || snap_ring.len() != n {
            return false;
        }

        // Classify each vertex once. We can't use closures that borrow
        // `self.floor_plan` immutably alongside `self.floor_plan.set_parameter`
        // mutably below, so resolve everything up front into owned data.
        let vertex_param: Vec<Option<ParameterId>> = (0..n)
            .map(|i| match axis {
                ParameterAxis::X => self.floor_plan.vertex_x_param(0, i),
                ParameterAxis::Y => self.floor_plan.vertex_y_param(0, i),
            })
            .collect();
        let is_unset = |i: usize| -> bool {
            matches!(vertex_param[i], Some(p) if !self.dimensioning.known_params.contains(&p))
        };
        let coord_of = |pts: &[Point2f32], i: usize| -> f32 {
            match axis {
                ParameterAxis::X => pts[i].x,
                ParameterAxis::Y => pts[i].y,
            }
        };

        if is_unset(0) {
            // V0 should be a constant (the polygon's anchor). Defensive bail.
            return false;
        }

        // First pass: collect (param, new_value) assignments without mutating.
        let mut assignments: Vec<(ParameterId, f32)> = Vec::new();
        let mut i = 0;
        while i < n {
            if !is_unset(i) {
                i += 1;
                continue;
            }
            let start = i;
            while i < n && is_unset(i) {
                i += 1;
            }
            let end = i; // exclusive

            let prev_idx = (start + n - 1) % n;
            if is_unset(prev_idx) {
                continue;
            }

            let new_lower = coord_of(ring, prev_idx);
            let pre_lower = coord_of(snap_ring, prev_idx);

            // Distinct un-set params in run order. For each, the pre-apply
            // value comes from the snapshot vertex at the same position.
            let mut applied: Vec<ParameterId> = Vec::new();
            for j in start..end {
                let Some(p) = vertex_param[j] else {
                    continue;
                };
                if applied.contains(&p) {
                    continue;
                }
                applied.push(p);
                let pre_value = coord_of(snap_ring, j);
                let offset = pre_value - pre_lower;
                assignments.push((p, new_lower + offset));
            }
        }

        // Second pass: apply.
        for (p, value) in assignments {
            self.floor_plan.set_parameter(p, value);
        }

        self.floor_plan.validate_simple().is_ok()
    }

    /// Linear-remap repair for ring 0. Walks vertices in polygon order, finds
    /// maximal runs of vertices whose `axis`-coord is an un-set parameter
    /// (bounded by vertices whose coord is known — either a constant or a
    /// set parameter). For each run, redistributes the distinct un-set params
    /// evenly within the bounding interval. Returns true if the resulting
    /// polygon validates simple.
    fn try_linear_remap_ring0(&mut self, axis: ParameterAxis) -> bool {
        let polygons = self.floor_plan.polygons();
        let Some(ring_positions) = polygons.first() else {
            return false;
        };
        let n = ring_positions.len();
        if n < 4 {
            return false;
        }

        // Source of each vertex's relevant-axis coord: Known(value) or Unset(param, value).
        #[derive(Clone, Copy)]
        enum Source {
            Known(f32),
            Unset(ParameterId, f32),
        }
        let coord_of = |i: usize| -> f32 {
            match axis {
                ParameterAxis::X => ring_positions[i].x,
                ParameterAxis::Y => ring_positions[i].y,
            }
        };
        let param_of = |i: usize| -> Option<ParameterId> {
            match axis {
                ParameterAxis::X => self.floor_plan.vertex_x_param(0, i),
                ParameterAxis::Y => self.floor_plan.vertex_y_param(0, i),
            }
        };
        let seq: Vec<Source> = (0..n)
            .map(|i| match param_of(i) {
                Some(p) if !self.dimensioning.known_params.contains(&p) => {
                    Source::Unset(p, coord_of(i))
                }
                _ => Source::Known(coord_of(i)),
            })
            .collect();

        // Bail if the polygon starts with an un-set vertex — would need
        // wrap-around handling. For ring 0 this shouldn't happen (V0 is
        // always Constant by construction), but be defensive.
        if matches!(seq[0], Source::Unset(_, _)) {
            return false;
        }

        // Find each maximal run of un-set entries; for each, redistribute.
        let mut i = 0;
        while i < n {
            if matches!(seq[i], Source::Known(_)) {
                i += 1;
                continue;
            }
            let start = i;
            while i < n && matches!(seq[i], Source::Unset(_, _)) {
                i += 1;
            }
            let end = i; // exclusive

            let Source::Known(lower) = seq[(start + n - 1) % n] else {
                // Two consecutive un-set runs separated by nothing — shouldn't
                // happen given the loop structure, but be safe.
                continue;
            };
            let Source::Known(upper) = seq[end % n] else {
                continue;
            };

            // Distinct un-set params in polygon-traversal order.
            let mut distinct: Vec<ParameterId> = Vec::new();
            for s in &seq[start..end] {
                if let Source::Unset(p, _) = s {
                    if !distinct.contains(p) {
                        distinct.push(*p);
                    }
                }
            }
            if distinct.is_empty() {
                continue;
            }

            // Evenly distribute between `lower` and `upper` (exclusive of
            // both bounds). `step` carries the sign — if upper < lower
            // (polygon traverses from larger to smaller along this axis),
            // step is negative and params decrease in order, which matches
            // the un-set params' position in the traversal.
            let step = (upper - lower) / (distinct.len() + 1) as f32;
            for (idx, p) in distinct.iter().enumerate() {
                let new_val = lower + step * (idx + 1) as f32;
                self.floor_plan.set_parameter(*p, new_val);
            }
        }

        self.floor_plan.validate_simple().is_ok()
    }

    /// If the polygon's aspect ratio is extreme (> ~5:1), scale the still-
    /// unset parameters along the short axis to bring aspect back to ~3:1
    /// (capped at 2× scaling). Reverts if the rescale would invalidate the
    /// polygon. Pure aesthetics — the user's confirmed dimensions never move.
    fn aesthetic_rescale_if_needed(&mut self) {
        const TRIGGER_ASPECT: f32 = 5.0;
        const TARGET_ASPECT: f32 = 3.0;
        const MAX_FACTOR: f32 = 2.0;

        let Some(((min_x, min_y), (max_x, max_y))) = self.floor_plan.bounding_box() else {
            return;
        };
        let bw = max_x - min_x;
        let bh = max_y - min_y;
        if bw < 1e-3 || bh < 1e-3 {
            return;
        }
        let aspect = bw.max(bh) / bw.min(bh);
        if aspect <= TRIGGER_ASPECT {
            return;
        }

        let factor = (aspect / TARGET_ASPECT).clamp(1.0, MAX_FACTOR);
        // Scale the deficient axis: if the polygon is wider than tall, the
        // short axis is Y — scale the un-set y-params.
        let scale_y = bw > bh;

        let snapshot = self.floor_plan.clone();
        let active = self.floor_plan.all_active_params();
        for p in active {
            if self.dimensioning.known_params.contains(&p) {
                continue;
            }
            let scale_this = match self.floor_plan.param_axis(p) {
                Some(ParameterAxis::Y) => scale_y,
                Some(ParameterAxis::X) => !scale_y,
                None => false,
            };
            if scale_this {
                self.floor_plan.scale_parameter(p, factor);
            }
        }

        // If the rescale broke the polygon (e.g. pushed an un-set coord past
        // a known one), revert. Better to keep the extreme aspect than to
        // produce a self-intersecting shape.
        if self.floor_plan.validate_simple().is_err() {
            self.floor_plan = snapshot;
        }
    }

    /// Skip the current dimension prompt without entering a value. The
    /// specific WALL is suppressed from future prompts; the param it would
    /// have set remains un-set — but other walls (if any) can still set it.
    pub fn skip_current_dimension(&mut self) {
        if let Some(prompt) = self.dimensioning.current_prompt.take() {
            self.dimensioning
                .skipped_walls
                .insert(prompt.wall_start_vertex);
            let (ring, edge) = self
                .floor_plan
                .find_vertex(prompt.wall_start_vertex)
                .unwrap_or((0, 0));
            println!(
                "wall skipped: ring {}/edge {} (would have set P{}). \
                 The heuristic will continue looking for other walls.",
                ring, edge, prompt.controlled_param
            );
            self.compute_next_dimension_prompt();
        }
    }

    /// Recompute `current_prompt` based on which params are still unknown.
    /// Picks the first wall whose length depends on exactly one unknown
    /// parameter; clears the prompt when every active param is known.
    fn compute_next_dimension_prompt(&mut self) {
        // A wall is promptable when:
        //   - It's not in `skipped_walls` (the user already passed on it).
        //   - Its length depends on exactly one parameter not yet in
        //     `known_params`, with the rest of its deps all known.
        //
        // Skip is per-WALL, not per-param. Skipping wall W (whose controlled
        // param would have been P) doesn't preclude prompting wall W' that
        // also sets P — the user might prefer to measure W' instead. If no
        // other wall can cleanly set P, P stays un-dimensioned but the
        // user is still asked about every other param via walls that don't
        // depend on P.
        let active = self.floor_plan.all_active_params();
        let known = self.dimensioning.known_params.clone();
        let to_ask: HashSet<ParameterId> =
            active.into_iter().filter(|p| !known.contains(p)).collect();
        if to_ask.is_empty() {
            self.dimensioning.current_prompt = None;
            return;
        }

        let polygons = self.floor_plan.polygons();
        for (ring_idx, ring_points) in polygons.iter().enumerate() {
            for edge_start in 0..ring_points.len() {
                // Skip walls the user has already passed on.
                let Some(vid) = self.floor_plan.vertex_id_at(ring_idx, edge_start) else {
                    continue;
                };
                if self.dimensioning.skipped_walls.contains(&vid) {
                    continue;
                }
                // A wall is promptable iff exactly one of its deps is in
                // `to_ask` and the rest are in `known`. Deps that aren't in
                // either set (e.g. params whose only wall was skipped and
                // never set) block this wall from being promptable.
                let deps = self.floor_plan.wall_param_deps(ring_idx, edge_start);
                let mut to_ask_count = 0usize;
                let mut to_ask_param: Option<ParameterId> = None;
                let mut blocked = false;
                for d in &deps {
                    if to_ask.contains(d) {
                        to_ask_count += 1;
                        to_ask_param = Some(*d);
                    } else if !known.contains(d) {
                        // dep is neither known nor in to_ask — shouldn't
                        // happen, but be defensive.
                        blocked = true;
                        break;
                    }
                }
                if blocked {
                    continue;
                }
                if to_ask_count == 1 {
                    self.dimensioning.current_prompt = Some(DimensionPrompt {
                        wall_start_vertex: vid,
                        controlled_param: to_ask_param.unwrap(),
                    });
                    return;
                }
            }
        }
        // No promptable wall at all — every to_ask param is reachable only
        // through walls the user has skipped, or via walls touching multiple
        // un-set params. The remaining params stay un-dimensioned.
        self.dimensioning.current_prompt = None;
    }

    /// Kick off dimensioning if nothing else is in the way (no pending redraw,
    /// no in-flight collinearity review). No-op otherwise.
    fn maybe_start_dimensioning(&mut self) {
        if self.review.is_some()
            || self.pending_redraw.is_some()
            || self.dimensioning.current_prompt.is_some()
        {
            return;
        }
        // Collapse any zero-length edges + collinear vertices that the
        // collinearity review may have introduced (e.g. when the user marked
        // a top-left nook's edges as collinear with the polygon's outer top
        // edge, the nook's vertices end up at the same point as their
        // neighbours). Done before dimensioning starts so we don't prompt
        // for dimensions on degenerate walls. Only safe while scale isn't
        // set — afterwards the parametric IDs are referenced by known_params
        // and a rebuild would dangle them.
        if !self.dimensioning.scale_set {
            self.floor_plan.simplify_degenerate();
            if let Err(e) = self.commit_to_ber() {
                eprintln!("commit_to_ber after simplify_degenerate: {}", e);
            }
        }
        self.compute_next_dimension_prompt();
    }

    /// Is there a pending DrawPerpWalls stroke awaiting the user's redraw decision?
    pub fn has_pending_redraw(&self) -> bool {
        self.pending_redraw.is_some()
    }

    /// User said: replace the current storey with the pending stroke.
    pub fn confirm_replace_storey(&mut self) -> Result<(), String> {
        let Some(pending) = self.pending_redraw.take() else {
            return Ok(());
        };
        self.apply_drawperp_stroke(&pending.stroke_points, pending.closed)
    }

    /// User said: add a new storey using the pending stroke as its outline.
    /// Stub — implementation deferred to the multi-storey feature.
    pub fn confirm_add_storey_from_current(&mut self) {
        if let Some(_pending) = self.pending_redraw.take() {
            println!("Add new storey from current — TODO (stub for upcoming feature)");
        }
    }

    /// User said: discard the pending stroke without doing anything.
    pub fn discard_pending_redraw(&mut self) {
        self.pending_redraw = None;
    }

    /// Current view transform (auto-fit + user pan/zoom).
    pub fn view(&self) -> ViewTransform {
        self.view
    }

    /// Multiply current zoom by `factor`, keeping `anchor` (canvas pixel pos)
    /// stationary. Clamped to a reasonable range.
    pub fn zoom_view(&mut self, factor: f32, anchor: (f32, f32), canvas_center: (f32, f32)) {
        let old = self.view.zoom;
        let new = (old * factor).clamp(0.1, 10.0);
        let actual_factor = new / old;
        // Anchor's pre-zoom position relative to center, scaled by the actual factor
        // we applied, then re-anchored. Offset shifts so the anchor stays put.
        let rel_x = anchor.0 - canvas_center.0 - self.view.offset_x;
        let rel_y = anchor.1 - canvas_center.1 - self.view.offset_y;
        self.view.offset_x += rel_x * (1.0 - actual_factor);
        self.view.offset_y += rel_y * (1.0 - actual_factor);
        self.view.zoom = new;
    }

    /// Translate the view by a canvas-pixel delta (positive = pan right/down).
    pub fn pan_view(&mut self, delta_x: f32, delta_y: f32) {
        self.view.offset_x += delta_x;
        self.view.offset_y += delta_y;
    }

    /// Reset the view to pure auto-fit.
    pub fn reset_view(&mut self) {
        self.view = ViewTransform::default();
    }

    pub fn load_data_async(&self, tx: Sender<(String, f32)>) {
        let loader = self.loader_engine.clone();

        std::thread::spawn(move || {
            let _ = loader.load_general_data(tx);
        });
    }

    pub fn create_ber(&mut self, name: String, date_assessed: NaiveDate) -> Ber {
        let ber = Ber::new();
        let record = BerRecord {
            id: ber.id,
            name,
            date_assessed,
            data: ber.clone(),
        };

        BerRepository::new(&self.conn)
            .upsert(&record)
            .expect("upsert failed");
        self.current_record = Some(record);
        ber
    }

    pub fn get_ber(&self, id: &ItemId) -> Option<Ber> {
        let repo = BerRepository::new(&self.conn);
        repo.load(id).ok().map(|record| record.data)
    }

    /// Snapshot of the BER currently being edited, for the UI to render.
    /// Returns None if no BER has been created/loaded yet.
    pub fn current_ber_clone(&self) -> Option<Ber> {
        self.current_record.as_ref().map(|r| r.data.clone())
    }

    /// True when there's a completed orthogonal shape to act on. The UI uses
    /// this to gate downstream edit tools (Select, Move, Extend, Trim, …)
    /// behind the prerequisite "draw the orthogonal shape first."
    pub fn has_floor_plan(&self) -> bool {
        self.floor_plan.get_points().len() >= 3
    }

    /// Project current in-memory state into `self.current_record` and persist.
    /// No-op when no BER is loaded. Called by handler methods that complete a
    /// logical user action (stroke end, wall drag end, etc.) — not from
    /// incremental handlers like `sketch_add_point`.
    fn commit_to_ber(&mut self) -> Result<(), String> {
        let Some(record) = self.current_record.as_mut() else {
            return Ok(());
        };

        if record.data.storeys.is_empty() {
            record.data.storeys.push(Storey::default());
        }

        // Convert every polygon (ring) of the floor plan into a f64-typed
        // polygon for the BER data model. One ring → one FloorRegion.
        let polygons_f64: Vec<Vec<Point2f64>> = self
            .floor_plan
            .polygons()
            .into_iter()
            .map(|ring| {
                ring.into_iter()
                    .map(|p| Point2f64::new(p.x as f64, p.y as f64))
                    .collect()
            })
            .collect();

        if polygons_f64.iter().any(|p| p.len() >= 3) {
            record.data.storeys[0].floors = FloorRegions::from_vertex_lists(
                polygons_f64,
                DEFAULT_CEILING_HEIGHT_M,
                None,
                None,
                None,
            );
        }

        BerRepository::new(&self.conn)
            .upsert(record)
            .map_err(|e| format!("upsert failed: {}", e))
    }

    /// COLLINEARITY REVIEW-------------------------------------------
    /// The next collinearity suggestion the UI should ask the user about,
    /// or None if there's nothing pending. Walks forward from the cursor,
    /// skipping suggestions that have become "already determined" since the
    /// list was generated (yes-side via the live parametric state, no-side
    /// via the rejections list).
    pub fn current_review_suggestion(&self) -> Option<&CollinearitySuggestion> {
        let review = self.review.as_ref()?;
        for sug in &review.suggestions[review.cursor..] {
            if !self.review_should_skip(sug) {
                return Some(sug);
            }
        }
        None
    }

    /// Clone of the current suggestion, for cheap UI handoff without borrow gymnastics.
    pub fn current_review_suggestion_clone(&self) -> Option<CollinearitySuggestion> {
        self.current_review_suggestion().cloned()
    }

    /// Number of suggestions still to be considered (including the current).
    pub fn review_remaining(&self) -> usize {
        let Some(review) = self.review.as_ref() else {
            return 0;
        };
        review.suggestions[review.cursor..]
            .iter()
            .filter(|s| !self.review_should_skip(s))
            .count()
    }

    /// User said Yes: apply the suggestion, advance past it, commit.
    pub fn accept_current_review(&mut self) -> Result<(), String> {
        let Some(sug) = self.next_pending_suggestion() else {
            return Ok(());
        };
        println!(
            "collinearity YES: ring {}/edge {} & ring {}/edge {} along {:?} (coords {:.2}, {:.2})",
            sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, sug.axis, sug.coord_a, sug.coord_b
        );
        self.floor_plan.apply_collinear_suggestion(&sug);
        println!(
            "  → active_parameter_count = {}",
            self.floor_plan.active_parameter_count()
        );
        self.advance_review_cursor();
        let result = self.commit_to_ber();
        self.after_review_step();
        result
    }

    /// User said No: record the rejection (for transitive skipping), advance past it.
    pub fn reject_current_review(&mut self) {
        let Some(sug) = self.next_pending_suggestion() else {
            return;
        };
        println!(
            "collinearity NO:  ring {}/edge {} & ring {}/edge {} along {:?} (coords {:.2}, {:.2})",
            sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, sug.axis, sug.coord_a, sug.coord_b
        );
        if let Some(review) = self.review.as_mut() {
            review.rejections.push(sug);
        }
        self.advance_review_cursor();
        self.after_review_step();
    }

    /// User said Skip-all: clear the entire review without applying anything.
    /// Once the collinearity review is gone the controller picks up the
    /// dimensioning flow, if there's anything to ask about.
    pub fn skip_all_review(&mut self) {
        self.review = None;
        self.maybe_start_dimensioning();
    }

    /// Internal: pop the next pending suggestion by cloning it (so callers
    /// don't have to juggle borrows against `self`).
    fn next_pending_suggestion(&self) -> Option<CollinearitySuggestion> {
        self.current_review_suggestion().cloned()
    }

    /// Internal: move the cursor past the just-handled suggestion and any
    /// that follow which are now resolved. Clears `self.review` when there's
    /// nothing left to ask.
    fn advance_review_cursor(&mut self) {
        if let Some(review) = self.review.as_mut() {
            review.cursor += 1;
        } else {
            return;
        }
        // Skip past any suggestions that are now resolved. We re-borrow each
        // iteration so `review_should_skip` can immutably borrow `self` while
        // we mutate `self.review`.
        loop {
            let next_sug = self
                .review
                .as_ref()
                .and_then(|r| r.suggestions.get(r.cursor).cloned());
            let Some(sug) = next_sug else {
                self.review = None;
                return;
            };
            if self.review_should_skip(&sug) {
                if let Some(r) = self.review.as_mut() {
                    r.cursor += 1;
                }
            } else {
                return;
            }
        }
    }

    /// Kick off dimensioning when the review naturally drains to None.
    /// Called from accept/reject paths after `advance_review_cursor`.
    fn after_review_step(&mut self) {
        if self.review.is_none() {
            self.maybe_start_dimensioning();
        }
    }

    /// Check the live state to decide if a suggestion is already-resolved
    /// (no need to ask). Either the two edges are already collinear, or the
    /// pair is transitively rejected via an earlier No.
    fn review_should_skip(&self, sug: &CollinearitySuggestion) -> bool {
        if self
            .floor_plan
            .edges_collinear(sug.ring_a, sug.edge_a, sug.ring_b, sug.edge_b, &sug.axis)
        {
            return true;
        }
        let Some(review) = self.review.as_ref() else {
            return false;
        };
        // Transitive rejection: if any rejected pair is "the same question"
        // as `sug` (i.e. each side matches a side of the rejection through
        // existing equivalences), skip.
        for rej in &review.rejections {
            if rej.axis != sug.axis {
                continue;
            }
            let same = |sa: (usize, usize), ra: (usize, usize)| -> bool {
                (sa.0 == ra.0 && sa.1 == ra.1)
                    || self
                        .floor_plan
                        .edges_collinear(sa.0, sa.1, ra.0, ra.1, &rej.axis)
            };
            let sug_a = (sug.ring_a, sug.edge_a);
            let sug_b = (sug.ring_b, sug.edge_b);
            let rej_a = (rej.ring_a, rej.edge_a);
            let rej_b = (rej.ring_b, rej.edge_b);
            let aa_bb = same(sug_a, rej_a) && same(sug_b, rej_b);
            let ab_ba = same(sug_a, rej_b) && same(sug_b, rej_a);
            if aa_bb || ab_ba {
                return true;
            }
        }
        false
    }

    /// SKETCHING STUFF--------------------------------------------
    pub fn sketch_stroke(&self) -> Option<Stroke> {
        self.sketch_engine.stroke.clone()
    }

    pub fn sketch_mode(&self) -> SketchMode {
        self.sketch_engine.mode
    }
    pub fn set_sketch_mode(&mut self, mode: SketchMode) {
        self.sketch_engine.set_mode(mode);
    }

    pub fn set_display_mode(&mut self, mode: DisplayMode) {
        self.sketch_engine.set_display(mode);
    }
    pub fn display_mode(&self) -> DisplayMode {
        self.sketch_engine.display
    }

    pub fn set_edit_mode(&mut self, mode: EditTools) {
        self.sketch_engine.set_edit_tools(mode);
    }
    pub fn edit_mode(&self) -> EditTools {
        self.sketch_engine.edit_tool
    }

    pub fn sketch_start_stroke(&mut self, start: Point2f32) -> Result<(), String> {
        // A new stroke invalidates any pending collinearity review — the
        // polygon is about to change.
        self.review = None;
        self.sketch_engine
            .start_stroke(start)
            .map_err(|e| format!("Failed to start stroke: {:?}", e))
    }

    pub fn sketch_add_point(&mut self, point: Point2f32) -> Result<(), String> {
        self.sketch_engine
            .add_point(point)
            .map_err(|e| format!("Failed to add point: {:?}", e))
    }

    pub fn sketch_end_stroke(&mut self, _point: Point2f32) -> Result<(), String> {
        let edit_tool = self.sketch_engine.edit_tool;
        if let Some(stroke) = self
            .sketch_engine
            .end_stroke()
            .map_err(|e| format!("Failed to end stroke: {:?}", e))?
        {
            println!("Stroke completed with {} points", stroke.points.len());
            match edit_tool {
                EditTools::DrawPerpWalls => {
                    // Drawing the primary outline.
                    if self.floor_plan.ring_count() > 0 {
                        // A plan already exists — don't silently overwrite.
                        // Stash the stroke and let the UI ask the assessor
                        // what to do (Replace / Add new storey / Ignore).
                        self.pending_redraw = Some(PendingRedraw {
                            stroke_points: stroke.points,
                            closed: stroke.is_closed,
                        });
                    } else {
                        // No existing plan — apply directly.
                        self.apply_drawperp_stroke(&stroke.points, stroke.is_closed)?;
                    }
                }
                EditTools::AddSection => {
                    self.apply_addsection_stroke(&stroke.points, stroke.is_closed)?;
                }
                _ => {
                    // Other tools don't consume the stroke at this layer yet.
                }
            }
        }
        Ok(())
    }

    /// Apply a primary-outline stroke: replace the floor plan, persist, and
    /// populate the collinearity review. Resets dimensioning state — the new
    /// plan is a fresh pixel-scale sketch that hasn't been dimensioned yet.
    fn apply_drawperp_stroke(&mut self, points: &[Point2f32], closed: bool) -> Result<(), String> {
        self.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
            .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
        println!("floor plan: {:?}", self.floor_plan);
        self.dimensioning = DimensionState::default();
        self.commit_to_ber()?;
        self.populate_collinearity_review();
        self.maybe_start_dimensioning();
        Ok(())
    }

    /// Apply an AddSection stroke: append a ring (or replace if there's no
    /// outline yet), persist, populate review.
    fn apply_addsection_stroke(
        &mut self,
        points: &[Point2f32],
        closed: bool,
    ) -> Result<(), String> {
        let had_outline = self.floor_plan.ring_count() > 0;
        if !had_outline {
            self.floor_plan = FloorPlan::from_points_to_orthogonal(points, 10.0, closed)
                .map_err(|e| format!("Failed to draw outline: {:?}", e))?;
            // Same as DrawPerpWalls when there was nothing: reset dimensioning.
            self.dimensioning = DimensionState::default();
        } else {
            self.floor_plan
                .add_polygon(points, 10.0, closed)
                .map_err(|e| format!("Failed to add section: {:?}", e))?;
            // Existing known_params remain valid — apply_orthogonal_constraints
            // reuses param IDs for coords that match (ring 0's coords didn't
            // change). The new ring contributes additional unknown params we'll
            // pick up in the next dimensioning round.
            // KNOWN LIMITATION: if dimensions were already set (scale_set),
            // the new ring's vertex coords are in pixels while existing rings
            // are in meters. Mixed units will look wrong on the canvas until
            // a per-stroke pixels→meters conversion is added.
        }
        println!("floor plan (after AddSection): {:?}", self.floor_plan);
        self.commit_to_ber()?;
        self.populate_collinearity_review();
        self.maybe_start_dimensioning();
        Ok(())
    }

    /// Detect near-collinear walls and populate the review if any.
    ///
    /// After AddSection appends a new ring, pairs entirely within older
    /// rings were either resolved when those rings were drawn or were
    /// intentionally left as distinct walls — don't re-ask. We only
    /// surface pairs that involve the newly-added ring. For DrawPerpWalls
    /// the floor_plan was just replaced with a single ring at index 0,
    /// so the filter is a no-op.
    fn populate_collinearity_review(&mut self) {
        let newest_ring = self.floor_plan.ring_count().saturating_sub(1);
        let suggestions: Vec<_> = self
            .floor_plan
            .suggest_collinear(COLLINEARITY_MIN_SEPARATION, COLLINEARITY_MAX_SEPARATION)
            .into_iter()
            .filter(|s| s.ring_a == newest_ring || s.ring_b == newest_ring)
            .collect();
        if !suggestions.is_empty() {
            self.review = Some(CollinearityReview {
                suggestions,
                cursor: 0,
                rejections: Vec::new(),
            });
        }
    }
}

/// For each edge in each ring, record `(sign_dx, sign_dy)` where dx, dy
/// are the deltas from the edge's start vertex to its end vertex. Used by
/// the direction-flip guard in `apply_dimension`.
fn snapshot_edge_signs(plan: &FloorPlan) -> Vec<Vec<(i8, i8)>> {
    plan.polygons()
        .iter()
        .map(|ring| {
            let n = ring.len();
            (0..n)
                .map(|i| {
                    let a = ring[i];
                    let b = ring[(i + 1) % n];
                    let sx = (b.x - a.x).signum() as i8;
                    let sy = (b.y - a.y).signum() as i8;
                    (sx, sy)
                })
                .collect()
        })
        .collect()
}

/// Returns the (ring_idx, edge_idx) of the first edge whose direction sign
/// flipped between `before` and `after` (positive → negative or vice versa
/// along either axis). Zero signs are ignored — they represent edges that
/// were already degenerate in one direction, so a sign change there isn't
/// a "flip", and `validate_simple` separately catches genuinely-collapsed
/// edges.
fn find_flipped_edge(before: &[Vec<(i8, i8)>], after: &[Vec<(i8, i8)>]) -> Option<(usize, usize)> {
    for (ring_idx, (b_ring, a_ring)) in before.iter().zip(after.iter()).enumerate() {
        if b_ring.len() != a_ring.len() {
            return Some((ring_idx, 0));
        }
        for (edge_idx, (b_sign, a_sign)) in b_ring.iter().zip(a_ring.iter()).enumerate() {
            if b_sign.0 != 0 && a_sign.0 != 0 && b_sign.0 != a_sign.0 {
                return Some((ring_idx, edge_idx));
            }
            if b_sign.1 != 0 && a_sign.1 != 0 && b_sign.1 != a_sign.1 {
                return Some((ring_idx, edge_idx));
            }
        }
    }
    None
}

/// Statistics about a floor plan
#[derive(Debug, Clone)]
pub struct FloorPlanStats {
    pub perimeter: f32,
    pub area: f32,
    pub num_vertices: usize,
}
