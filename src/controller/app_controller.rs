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
use manip_2d_plan::{Axis, CollinearitySuggestion, FloorPlan, ParameterId, VertexId};
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

/// UI-friendly snapshot of the current dimension prompt: the two endpoints of
/// the wall the user is being asked to dimension, in world coordinates (same
/// coord system as the rendered BER polygon). The renderer transforms these
/// to canvas pixels via its `Transform2d`.
#[derive(Debug, Clone, Copy)]
pub struct DimensionPromptUiData {
    pub wall_a: (f64, f64),
    pub wall_b: (f64, f64),
    /// Current length of the wall in the plan's current units. Before the
    /// first dimension lands this is in pixels; after, it's in meters.
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

    /// Resolved view of the current prompt for the UI: world-coord endpoints
    /// + current length. None when there's no active prompt.
    pub fn dimension_prompt_for_ui(&self) -> Option<DimensionPromptUiData> {
        let prompt = self.dimensioning.current_prompt.as_ref()?;
        let (ring, edge_start) = self.floor_plan.find_vertex(prompt.wall_start_vertex)?;
        let polygons = self.floor_plan.polygons();
        let ring_pts = polygons.get(ring)?;
        let n = ring_pts.len();
        if n < 2 {
            return None;
        }
        let pa = ring_pts.get(edge_start)?;
        let pb = ring_pts.get((edge_start + 1) % n)?;
        let current_length = self.floor_plan.wall_length(ring, edge_start)?;
        Some(DimensionPromptUiData {
            wall_a: (pa.x as f64, pa.y as f64),
            wall_b: (pb.x as f64, pb.y as f64),
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
            self.floor_plan.scale_all(scale);
            self.dimensioning.scale_set = true;
        } else {
            // Subsequent dimensions — adjust just the controlled parameter so
            // the wall takes the new length, preserving its direction along axis.
            self.floor_plan
                .set_wall_length(ring, edge_start, value, prompt.controlled_param)
                .map_err(|e| format!("set_wall_length: {:?}", e))?;
        }

        self.dimensioning
            .known_params
            .insert(prompt.controlled_param);
        self.commit_to_ber()?;
        self.compute_next_dimension_prompt();
        Ok(())
    }

    /// Skip the current dimension prompt without entering a value. The prompt
    /// is dropped; the next call to `maybe_start_dimensioning` will pick a
    /// different wall (or stop, if no more single-unknown walls exist).
    pub fn skip_current_dimension(&mut self) {
        if self.dimensioning.current_prompt.take().is_some() {
            self.compute_next_dimension_prompt();
        }
    }

    /// Recompute `current_prompt` based on which params are still unknown.
    /// Picks the first wall whose length depends on exactly one unknown
    /// parameter; clears the prompt when every active param is known.
    fn compute_next_dimension_prompt(&mut self) {
        let active = self.floor_plan.all_active_params();
        let unknown: HashSet<ParameterId> = active
            .difference(&self.dimensioning.known_params)
            .copied()
            .collect();
        if unknown.is_empty() {
            self.dimensioning.current_prompt = None;
            return;
        }

        let polygons = self.floor_plan.polygons();
        for (ring_idx, ring_points) in polygons.iter().enumerate() {
            for edge_start in 0..ring_points.len() {
                let deps = self.floor_plan.wall_param_deps(ring_idx, edge_start);
                let unknown_deps: Vec<_> =
                    deps.into_iter().filter(|d| unknown.contains(d)).collect();
                if unknown_deps.len() == 1 {
                    if let Some(vid) = self.floor_plan.vertex_id_at(ring_idx, edge_start) {
                        self.dimensioning.current_prompt = Some(DimensionPrompt {
                            wall_start_vertex: vid,
                            controlled_param: unknown_deps[0],
                        });
                        return;
                    }
                }
            }
        }
        // No single-unknown-dep wall found. For an axis-aligned polygon this
        // shouldn't happen; if it does we just bail out and the UI shows no
        // prompt. (Hook for future "multi-unknown" prompts goes here.)
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

/// Statistics about a floor plan
#[derive(Debug, Clone)]
pub struct FloorPlanStats {
    pub perimeter: f32,
    pub area: f32,
    pub num_vertices: usize,
}
