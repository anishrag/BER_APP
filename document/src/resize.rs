//! Side-aware dimension EDIT on a finalised outline — the "the E-W width was
//! measured wrong" repair. The assessor picks a wall, types the correct
//! length, and says which END of the wall absorbs the change; that end's
//! whole coordinate PLANE (the building face, plus anything coplanar with it)
//! translates by the difference and everything anchored to the other side
//! stays put. The satellites follow the same plane move so the record stays
//! coherent instead of forking:
//!
//! - living areas / porches / in-flight carves: a shape TOUCHING the moving
//!   plane stretches with it; a shape poking OUTSIDE past it (a porch on the
//!   moving face) rides along rigidly; anything else stays.
//! - per-wall 1-D data on a wall that changed length: exposure sections
//!   shift/extend/clip so painted exposure carries into the grown length
//!   (or crops with the shrink); face-partition cuts stay anchored to the
//!   wall's fixed end.
//! - riser-keyed overrides (exposure + construction): re-keyed through the
//!   move instead of silently dropping.
//! - the durable record geometry (splits, ceiling steps/apexes, dormers,
//!   object pins) and each opening's along-fraction anchor: see
//!   [`resize_record_storey`].
//!
//! Pure model mutation — the shell wraps it with undo, cross-storey
//! propagation, session-zone transforms, and the re-resolve.

use crate::{ExposureSection, StoreyDoc};
use ber_model::ber::fenestration::WallAnchor;
use ber_model::ber::structural::Storey;
use domain::points::{Point2f32, Point2f64};
use manip_plan::{find_flipped_edge, FloorPlan, ParameterAxis, VertexId};

/// Which end of the edited wall moves, in ring order (the wall runs from its
/// start vertex to the next vertex in the ring).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WallEnd {
    Start,
    End,
}

/// Coordinate slack (metres) when matching geometry to the moving plane.
/// Coplanar geometry is written by the same solver so it agrees to well under
/// a millimetre; 15 mm also absorbs cross-storey faces carved from a
/// float-drifted parent without capturing genuinely distinct planes.
pub const PLANE_MATCH_TOL_M: f32 = 0.015;

/// Endpoint-displacement threshold (metres) below which a wall endpoint
/// counts as NOT moved by the plane move.
const ENDPOINT_MOVE_TOL_M: f32 = 1e-3;

/// One resize's moving plane: everything downstream (sub-shapes, record
/// geometry, session zones, storeys above) transforms against this.
#[derive(Debug, Clone, Copy)]
pub struct PlaneMove {
    /// The axis the moved coordinate lives on (X = an E-W move).
    pub axis: ParameterAxis,
    /// The plane's coordinate BEFORE the move.
    pub plane: f32,
    /// Signed displacement applied to the plane.
    pub delta: f32,
    /// +1.0 / -1.0 — which side of `plane` is OUTSIDE the edited storey
    /// (away from the edited wall's fixed end). A shape found on this side
    /// of the plane is attached outside it (a porch) and rides rigidly.
    pub outward: f32,
}

impl PlaneMove {
    /// The plane-relative coordinate of `c` on the moving axis.
    fn coord(&self, p_x: f32, p_y: f32) -> f32 {
        match self.axis {
            ParameterAxis::X => p_x,
            ParameterAxis::Y => p_y,
        }
    }

    /// Whether a coordinate lies ON the moving plane.
    fn on_plane(&self, c: f32) -> bool {
        (c - self.plane).abs() <= PLANE_MATCH_TOL_M
    }

    /// Move a point iff its axis-coordinate lies on the moving plane — the
    /// pointwise transform for durable ring/segment/pin geometry: a phase
    /// boundary, split line, ceiling step or ridge point ENDING on the moved
    /// face follows it; interior geometry stays.
    pub fn move_point_f64(&self, p: &mut Point2f64) {
        let c = self.coord(p.x as f32, p.y as f32);
        if self.on_plane(c) {
            match self.axis {
                ParameterAxis::X => p.x += self.delta as f64,
                ParameterAxis::Y => p.y += self.delta as f64,
            }
        }
    }

    /// f32 twin of [`move_point_f64`](Self::move_point_f64).
    pub fn move_point_f32(&self, p: &mut Point2f32) {
        let c = self.coord(p.x, p.y);
        if self.on_plane(c) {
            match self.axis {
                ParameterAxis::X => p.x += self.delta,
                ParameterAxis::Y => p.y += self.delta,
            }
        }
    }

    /// Pointwise transform over an f64 ring/segment in place.
    pub fn move_ring_f64(&self, ring: &mut [Point2f64]) {
        for p in ring {
            self.move_point_f64(p);
        }
    }

    /// Pointwise transform over an `(f32, f32)` plan point in place.
    pub fn move_pair_f32(&self, p: &mut (f32, f32)) {
        let c = self.coord(p.0, p.1);
        if self.on_plane(c) {
            match self.axis {
                ParameterAxis::X => p.0 += self.delta,
                ParameterAxis::Y => p.1 += self.delta,
            }
        }
    }
}

/// How one perimeter wall's base edge changed across the plane move (in the
/// outline's ring order: `a` = start vertex, `b` = end vertex).
#[derive(Debug, Clone, Copy)]
pub struct WallLengthChange {
    pub vid: VertexId,
    pub old: (Point2f32, Point2f32),
    pub new: (Point2f32, Point2f32),
}

impl WallLengthChange {
    pub fn old_len(&self) -> f32 {
        dist(self.old.0, self.old.1)
    }
    pub fn new_len(&self) -> f32 {
        dist(self.new.0, self.new.1)
    }
    /// The wall's START vertex was displaced by the move.
    pub fn start_moved(&self) -> bool {
        dist(self.old.0, self.new.0) > ENDPOINT_MOVE_TOL_M
    }
    /// The wall's END vertex was displaced by the move.
    pub fn end_moved(&self) -> bool {
        dist(self.old.1, self.new.1) > ENDPOINT_MOVE_TOL_M
    }
    /// The wall changed LENGTH (not a rigid translate).
    pub fn length_changed(&self) -> bool {
        (self.new_len() - self.old_len()).abs() > ENDPOINT_MOVE_TOL_M
    }
}

fn dist(a: Point2f32, b: Point2f32) -> f32 {
    ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt()
}

/// What a storey-level plane move did — the shell feeds this into the record
/// transform ([`resize_record_storey`]) and the session-zone transform.
#[derive(Debug)]
pub struct PlaneMoveOutcome {
    pub plane_move: PlaneMove,
    /// Every perimeter wall whose base edge changed (moved endpoint and/or
    /// changed length), old vs new.
    pub wall_changes: Vec<WallLengthChange>,
}

impl StoreyDoc {
    /// Resize the outline wall starting at `wall_start` to `new_len` metres,
    /// moving the plane through the wall's `moving_end` and keeping the other
    /// end fixed. Returns the plane move for the shell to propagate. A
    /// `new_len` equal to the current length is a no-op (`Ok(None)`).
    ///
    /// Only valid on a finalised (metre-scaled) outline — this is an EDIT of
    /// a completed dimension, not part of the dimensioning loop.
    pub fn resize_outline_wall(
        &mut self,
        wall_start: VertexId,
        new_len: f32,
        moving_end: WallEnd,
    ) -> Result<Option<PlaneMoveOutcome>, String> {
        if !new_len.is_finite() || new_len <= 0.0 {
            return Err(format!(
                "wall length must be a positive number (got {new_len})"
            ));
        }
        if !self.outline.dimensioning.scale_set {
            return Err("the outline isn't measured yet — finish dimensioning first".into());
        }
        // Slots are the source the move mutates; make them current first.
        self.sync_active();
        let (ring, edge_start) = self
            .outline
            .floor_plan
            .find_vertex(wall_start)
            .ok_or_else(|| "that wall no longer exists".to_string())?;
        if ring != 0 {
            return Err("only outline perimeter walls can be re-dimensioned".into());
        }
        let (axis, (_, start_c), (_, end_c)) = self
            .outline
            .floor_plan
            .wall_along_ends(ring, edge_start)
            .ok_or_else(|| "that wall isn't axis-aligned — re-draw it instead".to_string())?;
        let old_len = (end_c - start_c).abs();
        if (new_len - old_len).abs() <= ENDPOINT_MOVE_TOL_M {
            return Ok(None);
        }
        let (moving_c, fixed_c) = match moving_end {
            WallEnd::Start => (start_c, end_c),
            WallEnd::End => (end_c, start_c),
        };
        let outward = (moving_c - fixed_c).signum();
        let mv = PlaneMove {
            axis,
            plane: moving_c,
            delta: (new_len - old_len) * outward,
            outward,
        };
        match self.apply_plane_move(mv)? {
            Some(outcome) => Ok(Some(outcome)),
            // The edited wall's own end lies on the plane, so a zero match
            // means the plan and the prompt disagree — surface it.
            None => Err("couldn't find the wall's face to move".into()),
        }
    }

    /// Apply a plane move to THIS storey: the outline face on the plane, the
    /// sub-shapes, and the wall-keyed side tables. `Ok(None)` when nothing in
    /// this storey lies on the plane (a storey the move doesn't reach —
    /// callers use this to propagate a ground-floor edit only to the storeys
    /// stacked on the same face). Rolls the storey back on any failure.
    pub fn apply_plane_move(&mut self, mv: PlaneMove) -> Result<Option<PlaneMoveOutcome>, String> {
        self.sync_active();
        let backup = self.clone();
        let result = self.apply_plane_move_inner(mv);
        match result {
            Ok(v) => Ok(v),
            Err(e) => {
                *self = backup;
                Err(e)
            }
        }
    }

    fn apply_plane_move_inner(
        &mut self,
        mv: PlaneMove,
    ) -> Result<Option<PlaneMoveOutcome>, String> {
        let walls_before = self.outline.floor_plan.perimeter_walls();
        let signs_before = self.outline.floor_plan.edge_signs();

        let moved = self.outline.floor_plan.move_coordinate_planes(
            mv.axis,
            mv.plane,
            PLANE_MATCH_TOL_M,
            mv.delta,
        );
        if moved == 0 {
            return Ok(None);
        }
        self.outline
            .floor_plan
            .validate_simple()
            .map_err(|_| reject_message(mv))?;
        if find_flipped_edge(&signs_before, &self.outline.floor_plan.edge_signs()).is_some() {
            return Err(reject_message(mv));
        }

        // Sub-shapes: porches on the moving face ride rigidly; shapes touching
        // the plane stretch; everything else stays. Living areas (and any
        // in-flight carve) must remain simple and inside the outline.
        let outline = self.outline.floor_plan.clone();
        let containment_tol = (outline.bounding_box_diagonal() * 0.01).max(1e-3);
        let move_slot = |plan: &mut FloorPlan, keep_inside: bool| -> Result<(), String> {
            let pts: Vec<Point2f32> = plan
                .polygons()
                .iter()
                .flat_map(|ring| ring.iter().copied())
                .collect();
            if pts.is_empty() {
                return Ok(());
            }
            let touching = pts.iter().any(|p| mv.on_plane(mv.coord(p.x, p.y)));
            if !touching {
                return Ok(());
            }
            let beyond = pts
                .iter()
                .any(|p| mv.outward * (mv.coord(p.x, p.y) - mv.plane) > PLANE_MATCH_TOL_M);
            if beyond {
                // Attached outside the moving face (a porch): rigid ride.
                plan.translate_axis(mv.axis, mv.delta);
            } else {
                plan.move_coordinate_planes(mv.axis, mv.plane, PLANE_MATCH_TOL_M, mv.delta);
                plan.validate_simple().map_err(|_| {
                    "that change would collapse an area drawn against the moved wall".to_string()
                })?;
                if keep_inside && outline.any_point_outside(plan, containment_tol) {
                    return Err(
                        "that change would push an area outside the storey outline".to_string()
                    );
                }
            }
            Ok(())
        };
        for slot in &mut self.living_areas {
            move_slot(&mut slot.floor_plan, true)?;
        }
        for slot in &mut self.porches {
            move_slot(&mut slot.floor_plan, false)?;
        }
        if let Some(slot) = self.phase_shape.as_mut() {
            move_slot(&mut slot.floor_plan, true)?;
        }
        if let Some(slot) = self.step_shape.as_mut() {
            move_slot(&mut slot.floor_plan, true)?;
        }

        // Wall diff: which base edges changed, old vs new.
        let walls_after = self.outline.floor_plan.perimeter_walls();
        let mut wall_changes: Vec<WallLengthChange> = Vec::new();
        for (vid, a, b) in &walls_before {
            let Some((_, a2, b2)) = walls_after.iter().find(|(v2, _, _)| v2 == vid) else {
                continue;
            };
            let change = WallLengthChange {
                vid: *vid,
                old: (*a, *b),
                new: (*a2, *b2),
            };
            if change.start_moved() || change.end_moved() {
                wall_changes.push(change);
            }
        }

        // 1-D side tables on walls that changed LENGTH: sections carry the
        // painted exposure into the grown length (or crop with the shrink);
        // partition cuts stay anchored to the wall's fixed end.
        for change in wall_changes.iter().filter(|c| c.length_changed()) {
            if let Some(sections) = self.wall_sections.get_mut(&change.vid) {
                adjust_sections_for_length_change(sections, change);
            }
            if let Some(partition) = self.wall_face_partitions.get_mut(&change.vid) {
                let shift = if change.start_moved() {
                    change.new_len() - change.old_len()
                } else {
                    0.0
                };
                if shift.abs() > ENDPOINT_MOVE_TOL_M {
                    for cut in &mut partition.cuts {
                        cut.a.x += shift;
                        cut.b.x += shift;
                    }
                    for (sample, _) in &mut partition.regions {
                        sample.x += shift;
                    }
                }
            }
        }

        // Riser-keyed overrides: re-key edges ending on the moved plane so the
        // override survives instead of silently dropping.
        self.riser_exposure = self
            .riser_exposure
            .drain()
            .map(|(k, v)| (rekey_riser(k, &mv), v))
            .collect();
        self.riser_constructions = self
            .riser_constructions
            .drain()
            .map(|(k, v)| (rekey_riser(k, &mv), v))
            .collect();

        // The live buffer mirrors whichever slot is active — reload it so the
        // canvas shows the moved geometry immediately.
        self.load_active_into_buffer();

        Ok(Some(PlaneMoveOutcome {
            plane_move: mv,
            wall_changes,
        }))
    }
}

fn reject_message(mv: PlaneMove) -> String {
    let dir = match (mv.axis, mv.delta * mv.outward > 0.0) {
        (_, true) => "grow",
        (_, false) => "shrink",
    };
    format!(
        "that {dir} would make the outline self-intersect — another wall is in the way. \
         Pick a value that fits, or move the other side instead."
    )
}

/// Re-key one riser key through the plane move (endpoints on the plane
/// follow it; the key stays orientation-normalized).
fn rekey_riser(key: (i64, i64, i64, i64), mv: &PlaneMove) -> (i64, i64, i64, i64) {
    let mut a = Point2f64::new(key.0 as f64 / 1000.0, key.1 as f64 / 1000.0);
    let mut b = Point2f64::new(key.2 as f64 / 1000.0, key.3 as f64 / 1000.0);
    mv.move_point_f64(&mut a);
    mv.move_point_f64(&mut b);
    crate::riser_key(a, b)
}

/// Shift / extend / clip one wall's exposure sections for a length change.
/// Sections are absolute `[start_m, end_m)` from the wall's start vertex:
/// - start end moved → all sections shift by the length delta (their world
///   position is unchanged; the origin moved), and a section that sat at the
///   old start boundary re-extends to 0 so painted exposure carries into the
///   grown length.
/// - end end moved → a section that abutted the old end extends to the new
///   length (grow); everything clips to the new length (shrink).
fn adjust_sections_for_length_change(
    sections: &mut Vec<ExposureSection>,
    change: &WallLengthChange,
) {
    const ABUT_TOL_M: f32 = 0.02;
    let old_len = change.old_len();
    let new_len = change.new_len();
    let shift = if change.start_moved() {
        new_len - old_len
    } else {
        0.0
    };
    for sec in sections.iter_mut() {
        sec.start_m += shift;
        sec.end_m += shift;
    }
    if change.start_moved() {
        // A section that started at the old boundary (now at `shift`) carries
        // its exposure back to the new boundary.
        if let Some(first) = sections
            .iter_mut()
            .filter(|s| (s.start_m - shift).abs() <= ABUT_TOL_M)
            .min_by(|a, b| a.start_m.total_cmp(&b.start_m))
        {
            first.start_m = first.start_m.min(0.0);
        }
    }
    if change.end_moved() {
        if let Some(last) = sections
            .iter_mut()
            .filter(|s| (s.end_m - old_len).abs() <= ABUT_TOL_M)
            .max_by(|a, b| a.end_m.total_cmp(&b.end_m))
        {
            last.end_m = last.end_m.max(new_len);
        }
    }
    for sec in sections.iter_mut() {
        sec.start_m = sec.start_m.clamp(0.0, new_len);
        sec.end_m = sec.end_m.clamp(0.0, new_len);
    }
    sections.retain(|s| s.end_m - s.start_m > 1e-4);
}

/// Transform the DURABLE record geometry of one storey through a plane move:
/// structure splits, ceiling steps/apexes/anchors/continuations, dormer
/// footprints, object-room pins (pointwise: points on the moved plane follow
/// it), and every wall-anchored opening's along-fraction so the opening keeps
/// its distance from the wall's FIXED end rather than sliding proportionally.
/// `Err` (record untouched in a meaningful way only by fraction clamps —
/// callers restore from the undo snapshot on failure) when a shrunk wall can
/// no longer hold one of its openings.
pub fn resize_record_storey(storey: &mut Storey, outcome: &PlaneMoveOutcome) -> Result<(), String> {
    let mv = &outcome.plane_move;

    for ring in &mut storey.floor_splits {
        mv.move_ring_f64(ring);
    }
    for ring in &mut storey.ceiling_splits {
        mv.move_ring_f64(ring);
    }
    for seg in &mut storey.wall_splits {
        mv.move_ring_f64(seg.as_mut_slice());
    }
    for (ring, _) in &mut storey.ceiling_steps {
        mv.move_ring_f64(ring);
    }
    for apex in &mut storey.ceiling_apexes {
        mv.move_point_f64(&mut apex.point);
    }
    for hint in &mut storey.ceiling_apex_hints {
        mv.move_pair_f32(&mut hint.apex);
        mv.move_pair_f32(&mut hint.region);
    }
    for p in &mut storey.ceiling_unfanned {
        mv.move_pair_f32(p);
    }
    for p in &mut storey.ceiling_plane_anchors {
        mv.move_pair_f32(p);
    }
    for cont in &mut storey.ceiling_continuations {
        mv.move_pair_f32(&mut cont.target);
        mv.move_pair_f32(&mut cont.source);
    }
    for dormer in &mut storey.dormers {
        mv.move_ring_f64(&mut dormer.footprint);
    }
    for room in &mut storey.object_rooms {
        mv.move_point_f32(&mut room.plan_position);
    }

    // The stored ceilings are REGIONS-AS-TRUTH — unlike floors/walls they
    // don't re-derive from the outline, they get refit (clip + flat fill).
    // Stretch them through the plane move FIRST (coplanar: a flat ceiling
    // stays flat, a slope continues its pitch), so the refit finds the new
    // footprint already covered and mints no extra fill region in the grown
    // strip (which read as a phantom second ceiling in the field).
    storey.ceilings.stretch_boundary_plane(
        matches!(mv.axis, ParameterAxis::X),
        mv.plane as f64,
        PLANE_MATCH_TOL_M as f64,
        mv.delta as f64,
    );

    // Openings: keep each one's distance from its wall's FIXED end. The
    // resolve tail re-derives placements from these anchors.
    let adjust =
        |anchor: &mut Option<WallAnchor>, width_m: f32, label: &str| -> Result<(), String> {
            let Some(a) = anchor.as_mut() else {
                return Ok(());
            };
            if a.porch.is_some() {
                return Ok(()); // porch walls ride rigidly — nothing changes
            }
            let Some(start_vid) = a.wall.wall_start() else {
                return Ok(());
            };
            let Some(change) = outcome
                .wall_changes
                .iter()
                .find(|c| c.vid == start_vid && c.length_changed())
            else {
                return Ok(());
            };
            let (old_len, new_len) = (change.old_len(), change.new_len());
            let mut frac = if change.end_moved() {
                a.along_fraction * old_len / new_len
            } else {
                (a.along_fraction * old_len + (new_len - old_len)) / new_len
            };
            let half = (width_m * 0.5).max(0.0);
            if new_len < width_m - 1e-3 {
                return Err(format!(
                    "the wall would become too short ({new_len:.2} m) for the {label} on it \
                 ({width_m:.2} m) — remove or shrink the {label} first"
                ));
            }
            let (lo, hi) = (half / new_len, 1.0 - half / new_len);
            if lo <= hi {
                frac = frac.clamp(lo, hi);
            }
            a.along_fraction = frac;
            Ok(())
        };
    for w in &mut storey.windows {
        adjust(&mut w.wall_anchor, w.width_m, "window")?;
    }
    for d in &mut storey.doors {
        adjust(&mut d.wall_anchor, d.width_m, "door")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DimensionState, ShapeSlot, WallExposure};
    use ber_model::ber::common::ItemId;
    use ber_model::ber::fenestration::{OpeningPlacement, Window};
    use manip_plan::SurfaceRef;

    fn p(x: f32, y: f32) -> Point2f32 {
        Point2f32::new(x, y)
    }

    /// A finalised slot from exact ring points (the load-path shape).
    fn slot(points: &[Point2f32]) -> ShapeSlot {
        let plan = FloorPlan::from_clean_ring(points).unwrap();
        let known_params = plan.all_active_params();
        ShapeSlot {
            floor_plan: plan,
            dimensioning: DimensionState {
                known_params,
                scale_set: true,
                ..Default::default()
            },
        }
    }

    /// A finalised single-storey doc with a `w × h` rectangular outline at the
    /// origin: south wall y=0, north y=h, west x=0, east x=w.
    fn rect_doc(w: f32, h: f32) -> StoreyDoc {
        let mut doc = StoreyDoc::default();
        doc.outline = slot(&[p(0.0, 0.0), p(w, 0.0), p(w, h), p(0.0, h)]);
        doc.load_active_into_buffer();
        doc
    }

    /// The vid of the outline wall whose endpoints are (approximately) `a`→`b`.
    fn wall_vid(doc: &StoreyDoc, a: Point2f32, b: Point2f32) -> VertexId {
        doc.outline
            .floor_plan
            .perimeter_walls()
            .into_iter()
            .find(|(_, wa, wb)| dist(*wa, a) < 1e-3 && dist(*wb, b) < 1e-3)
            .map(|(vid, _, _)| vid)
            .unwrap_or_else(|| panic!("no wall {a:?} → {b:?}"))
    }

    fn ring_points(plan: &FloorPlan) -> Vec<Point2f32> {
        plan.get_points()
    }

    fn has_point(pts: &[Point2f32], x: f32, y: f32) -> bool {
        pts.iter()
            .any(|q| (q.x - x).abs() < 1e-3 && (q.y - y).abs() < 1e-3)
    }

    #[test]
    fn grow_east_moves_only_the_east_plane() {
        let mut doc = rect_doc(10.0, 6.0);
        // South wall runs (0,0) → (10,0); its End sits on the east plane.
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        let ids_before: Vec<VertexId> = doc
            .outline
            .floor_plan
            .perimeter_walls()
            .iter()
            .map(|(v, _, _)| *v)
            .collect();
        let outcome = doc
            .resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let pts = ring_points(&doc.outline.floor_plan);
        assert!(has_point(&pts, 12.0, 0.0), "east plane moved: {pts:?}");
        assert!(has_point(&pts, 12.0, 6.0));
        assert!(has_point(&pts, 0.0, 0.0), "west anchored");
        assert!(has_point(&pts, 0.0, 6.0));
        // Vertex identity survives — the durable side-table keys stay valid.
        let ids_after: Vec<VertexId> = doc
            .outline
            .floor_plan
            .perimeter_walls()
            .iter()
            .map(|(v, _, _)| *v)
            .collect();
        assert_eq!(ids_before, ids_after);
        // North + south walls stretched; east wall rode rigidly.
        let stretched: Vec<_> = outcome
            .wall_changes
            .iter()
            .filter(|c| c.length_changed())
            .collect();
        assert_eq!(stretched.len(), 2);
        let mv = outcome.plane_move;
        assert_eq!(mv.axis, ParameterAxis::X);
        assert!((mv.plane - 10.0).abs() < 1e-4);
        assert!((mv.delta - 2.0).abs() < 1e-4);
        assert!(mv.outward > 0.0);
        // The live buffer mirrors the moved outline.
        assert!(has_point(&ring_points(&doc.floor_plan), 12.0, 0.0));
    }

    #[test]
    fn grow_west_moves_the_constant_anchored_plane() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        // Start of the south wall sits on the west plane — V0's Constant.
        doc.resize_outline_wall(south, 11.0, WallEnd::Start)
            .unwrap()
            .unwrap();
        let pts = ring_points(&doc.outline.floor_plan);
        assert!(has_point(&pts, -1.0, 0.0), "west plane moved out: {pts:?}");
        assert!(has_point(&pts, -1.0, 6.0));
        assert!(has_point(&pts, 10.0, 0.0), "east anchored");
    }

    #[test]
    fn notch_on_an_l_plan_stays_put_when_the_far_face_moves() {
        // L-plan: 10 × 6 with a 4 × 2 bite out of the north-east corner.
        // The notch return at x=6 must NOT follow the east face at x=10.
        let mut doc = StoreyDoc::default();
        doc.outline = slot(&[
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 4.0),
            p(6.0, 4.0),
            p(6.0, 6.0),
            p(0.0, 6.0),
        ]);
        doc.load_active_into_buffer();
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        doc.resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let pts = ring_points(&doc.outline.floor_plan);
        assert!(has_point(&pts, 12.0, 0.0));
        assert!(has_point(&pts, 12.0, 4.0));
        assert!(has_point(&pts, 6.0, 4.0), "notch stayed: {pts:?}");
        assert!(has_point(&pts, 6.0, 6.0));
    }

    #[test]
    fn living_area_on_the_moving_face_stretches_others_stay() {
        let mut doc = rect_doc(10.0, 6.0);
        // One living area hugging the east face, one interior.
        doc.living_areas.push(slot(&[
            p(7.0, 1.0),
            p(10.0, 1.0),
            p(10.0, 5.0),
            p(7.0, 5.0),
        ]));
        doc.living_areas
            .push(slot(&[p(1.0, 1.0), p(3.0, 1.0), p(3.0, 3.0), p(1.0, 3.0)]));
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        doc.resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let touching = ring_points(&doc.living_areas[0].floor_plan);
        assert!(has_point(&touching, 12.0, 1.0), "stretched: {touching:?}");
        assert!(has_point(&touching, 7.0, 1.0), "inner edge anchored");
        let interior = ring_points(&doc.living_areas[1].floor_plan);
        assert!(has_point(&interior, 1.0, 1.0));
        assert!(has_point(&interior, 3.0, 3.0));
    }

    #[test]
    fn porch_outside_the_moving_face_rides_rigidly() {
        let mut doc = rect_doc(10.0, 6.0);
        // Porch attached OUTSIDE the east face: x 10 → 11.5.
        doc.porches.push(slot(&[
            p(10.0, 2.0),
            p(11.5, 2.0),
            p(11.5, 4.0),
            p(10.0, 4.0),
        ]));
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        doc.resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let porch = ring_points(&doc.porches[0].floor_plan);
        assert!(has_point(&porch, 12.0, 2.0), "porch moved: {porch:?}");
        assert!(has_point(&porch, 13.5, 4.0), "depth preserved");
    }

    #[test]
    fn shrink_that_would_invert_the_notch_rolls_back() {
        let mut doc = StoreyDoc::default();
        doc.outline = slot(&[
            p(0.0, 0.0),
            p(10.0, 0.0),
            p(10.0, 4.0),
            p(6.0, 4.0),
            p(6.0, 6.0),
            p(0.0, 6.0),
        ]);
        doc.load_active_into_buffer();
        let before = ring_points(&doc.outline.floor_plan);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        // East face at 10 → 5 would cross the notch return at 6.
        let err = doc.resize_outline_wall(south, 5.0, WallEnd::End);
        assert!(err.is_err());
        assert_eq!(ring_points(&doc.outline.floor_plan), before, "rolled back");
    }

    #[test]
    fn same_length_is_a_no_op() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        assert!(doc
            .resize_outline_wall(south, 10.0, WallEnd::End)
            .unwrap()
            .is_none());
    }

    #[test]
    fn painted_exposure_carries_into_the_grown_length() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        // Whole south wall painted Exposed.
        doc.wall_sections.insert(
            south,
            vec![ExposureSection {
                start_m: 0.0,
                end_m: 10.0,
                exposure: WallExposure::Exposed,
            }],
        );
        doc.resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let secs = &doc.wall_sections[&south];
        assert_eq!(secs.len(), 1);
        assert!((secs[0].start_m - 0.0).abs() < 1e-4);
        assert!((secs[0].end_m - 12.0).abs() < 1e-4, "extended: {secs:?}");
    }

    #[test]
    fn exposure_sections_shift_when_the_start_end_grows() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        // Two sections: [0,4) Exposed (abuts the start), [4,10) Semi.
        doc.wall_sections.insert(
            south,
            vec![
                ExposureSection {
                    start_m: 0.0,
                    end_m: 4.0,
                    exposure: WallExposure::Exposed,
                },
                ExposureSection {
                    start_m: 4.0,
                    end_m: 10.0,
                    exposure: WallExposure::SemiExposed,
                },
            ],
        );
        // Grow at the START end (west): the boundary between the two painted
        // sections keeps its world position — it moves to 6.0 in the new
        // wall-local frame — and the first section re-extends to 0.
        doc.resize_outline_wall(south, 12.0, WallEnd::Start)
            .unwrap()
            .unwrap();
        let secs = &doc.wall_sections[&south];
        assert_eq!(secs.len(), 2, "{secs:?}");
        assert!((secs[0].start_m - 0.0).abs() < 1e-4);
        assert!((secs[0].end_m - 6.0).abs() < 1e-4);
        assert!((secs[1].start_m - 6.0).abs() < 1e-4);
        assert!((secs[1].end_m - 12.0).abs() < 1e-4);
    }

    #[test]
    fn shrink_crops_exposure_sections() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        doc.wall_sections.insert(
            south,
            vec![ExposureSection {
                start_m: 0.0,
                end_m: 10.0,
                exposure: WallExposure::Exposed,
            }],
        );
        doc.resize_outline_wall(south, 8.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let secs = &doc.wall_sections[&south];
        assert_eq!(secs.len(), 1);
        assert!((secs[0].end_m - 8.0).abs() < 1e-4, "cropped: {secs:?}");
    }

    #[test]
    fn riser_overrides_rekey_through_the_move() {
        let mut doc = rect_doc(10.0, 6.0);
        // A step edge running east from (4,3) to the east face at (10,3):
        // its far endpoint sits ON the moving plane.
        let key = crate::riser_key(Point2f64::new(4.0, 3.0), Point2f64::new(10.0, 3.0));
        doc.riser_exposure.insert(key, WallExposure::Exposed);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        doc.resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let expect = crate::riser_key(Point2f64::new(4.0, 3.0), Point2f64::new(12.0, 3.0));
        assert_eq!(
            doc.riser_exposure.get(&expect),
            Some(&WallExposure::Exposed),
            "re-keyed: {:?}",
            doc.riser_exposure
        );
    }

    // ── record-side transform ───────────────────────────────────────────────

    fn window_on(vid: VertexId, frac: f32, width: f32) -> Window {
        Window {
            id: ItemId::default(),
            vertices: vec![],
            width_m: width,
            height_m: 1.2,
            window_type_id: ItemId::default(),
            placement: Some(OpeningPlacement::Wall {
                wall_id: ItemId::default(),
                along_m: 0.0,
                sill_m: 1.0,
            }),
            wall_anchor: Some(ber_model::ber::fenestration::WallAnchor {
                wall: SurfaceRef::wall(vid),
                along_fraction: frac,
                porch: None,
            }),
            assembly_id: None,
            multi_storey: false,
            shape: ber_model::ber::fenestration::WindowShape::Rectangular,
            overshading: Default::default(),
            num_opes: None,
        }
    }

    #[test]
    fn window_keeps_distance_from_the_fixed_end() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        let mut storey = Storey::default();
        // Window centred 2 m from the wall start (west, the fixed end).
        storey.windows.push(window_on(south, 0.2, 1.0));
        let outcome = doc
            .resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        resize_record_storey(&mut storey, &outcome).unwrap();
        let frac = storey.windows[0].wall_anchor.unwrap().along_fraction;
        // Still 2 m from the start: 2/12.
        assert!((frac - 2.0 / 12.0).abs() < 1e-4, "frac = {frac}");
    }

    #[test]
    fn window_keeps_distance_from_the_end_when_the_start_grows() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        let mut storey = Storey::default();
        // 2 m from the wall END (frac 0.8 of 10).
        storey.windows.push(window_on(south, 0.8, 1.0));
        let outcome = doc
            .resize_outline_wall(south, 12.0, WallEnd::Start)
            .unwrap()
            .unwrap();
        resize_record_storey(&mut storey, &outcome).unwrap();
        let frac = storey.windows[0].wall_anchor.unwrap().along_fraction;
        // Still 2 m from the end: 10/12.
        assert!((frac - 10.0 / 12.0).abs() < 1e-4, "frac = {frac}");
    }

    #[test]
    fn shrinking_below_a_window_width_is_rejected() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        let mut storey = Storey::default();
        storey.windows.push(window_on(south, 0.5, 3.0));
        let outcome = doc
            .resize_outline_wall(south, 2.0, WallEnd::End)
            .unwrap()
            .unwrap();
        let err = resize_record_storey(&mut storey, &outcome);
        assert!(err.is_err(), "2 m wall can't hold a 3 m window");
    }

    #[test]
    fn ceiling_regions_stretch_coplanar_with_the_move() {
        use ber_model::ber::structural::{CeilingRegion, CeilingRegions, Opes, Polygon3D};
        use domain::points::Point3f64;

        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        let mut storey = Storey::default();
        // One FLAT region over the west half, one SLOPED (z = 0.1·x) over the
        // east half ending on the moving face at x = 10.
        let flat = CeilingRegion {
            id: ItemId::default(),
            polygon: Polygon3D::new(vec![
                Point3f64::new(0.0, 0.0, 2.4),
                Point3f64::new(5.0, 0.0, 2.4),
                Point3f64::new(5.0, 6.0, 2.4),
                Point3f64::new(0.0, 6.0, 2.4),
            ]),
            opes: Opes::default(),
            construction_id: ItemId::default(),
            exposure: Default::default(),
        };
        let sloped = CeilingRegion {
            polygon: Polygon3D::new(vec![
                Point3f64::new(5.0, 0.0, 0.5),
                Point3f64::new(10.0, 0.0, 1.0),
                Point3f64::new(10.0, 6.0, 1.0),
                Point3f64::new(5.0, 6.0, 0.5),
            ]),
            ..flat.clone()
        };
        storey.ceilings = CeilingRegions {
            regions: vec![flat, sloped],
        };
        let outcome = doc
            .resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        resize_record_storey(&mut storey, &outcome).unwrap();

        assert_eq!(storey.ceilings.regions.len(), 2, "no region minted");
        // The flat west region is untouched.
        let flat_xs: Vec<f64> = storey.ceilings.regions[0]
            .polygon
            .exterior
            .iter()
            .map(|p| p.x)
            .collect();
        assert!(flat_xs.iter().all(|&x| x <= 5.0 + 1e-9));
        // The sloped region's face edge followed to x = 12 and CONTINUED its
        // pitch: z = 0.1 · 12 = 1.2 at the moved edge.
        let moved: Vec<_> = storey.ceilings.regions[1]
            .polygon
            .exterior
            .iter()
            .filter(|p| (p.x - 12.0).abs() < 1e-9)
            .collect();
        assert_eq!(moved.len(), 2, "both face vertices moved");
        for p in moved {
            assert!((p.z - 1.2).abs() < 1e-9, "pitch continued (z = {})", p.z);
        }
    }

    #[test]
    fn split_rings_and_pins_follow_the_plane() {
        let mut doc = rect_doc(10.0, 6.0);
        let south = wall_vid(&doc, p(0.0, 0.0), p(10.0, 0.0));
        let mut storey = Storey::default();
        // A phase-split ring covering the east zone x∈[7,10]: its two points
        // on the east face follow; the interior boundary at x=7 stays.
        storey.floor_splits.push(vec![
            Point2f64::new(7.0, 0.0),
            Point2f64::new(10.0, 0.0),
            Point2f64::new(10.0, 6.0),
            Point2f64::new(7.0, 6.0),
        ]);
        let outcome = doc
            .resize_outline_wall(south, 12.0, WallEnd::End)
            .unwrap()
            .unwrap();
        resize_record_storey(&mut storey, &outcome).unwrap();
        let ring = &storey.floor_splits[0];
        assert!((ring[1].x - 12.0).abs() < 1e-6, "{ring:?}");
        assert!((ring[2].x - 12.0).abs() < 1e-6);
        assert!((ring[0].x - 7.0).abs() < 1e-6, "interior boundary stays");
    }
}
